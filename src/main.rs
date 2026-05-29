#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use esp_backtrace as _;
#[cfg(feature = "gateway-node")]
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::{
    clock::CpuClock,
    interrupt::software::SoftwareInterruptControl,
    timer::timg::TimerGroup,
    uart::{Config as UartConfig, Uart},
};
#[cfg(feature = "sensor-node")]
use esp_hal::{
    i2c::master::{Config as I2cConfig, I2c},
    time::Rate,
};
use esp_println::{logger::init_logger_from_env, println};
#[cfg(feature = "sensor-node")]
use esp32c3_rust::sensors::Sht40;
use esp32c3_rust::{
    AppError, AppResult,
    demo::{AlarmLatch, AlarmTransition, DemoNode, EnvironmentSample, FrameAction, NetworkPhase},
    demo_log::{self, BuzzerAction, GatewayRxDataLog, GatewayStats},
    hardware::{LoraModuleConfigPlan, LoraUartConfig, PinConfig, Sht40Config},
    protocol::{self, Frame, FrameType},
    relay::RelayForwardBuffer,
    role::{ACTIVE_ROLE, GATEWAY_ID, NodeRole},
    transport::LoraUartTransport,
};

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    if let Err(error) = run().await {
        panic_on_fatal_error(error);
    }

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}

async fn run() -> AppResult<()> {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let software_interrupt = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, software_interrupt.software_interrupt0);

    init_logger_from_env();

    println!("pipe-net demo boot");
    println!(
        "role={} node_id={} default_slot={} hop={}",
        ACTIVE_ROLE,
        ACTIVE_ROLE.node_id(),
        ACTIVE_ROLE.default_slot(),
        ACTIVE_ROLE.default_hop()
    );
    let pins = PinConfig::for_role(ACTIVE_ROLE);
    let lora = LoraUartConfig::DEMO_DEFAULT;
    let lora_plan = LoraModuleConfigPlan::DX_LR32_DEMO;
    println!(
        "lora uart: tx=GPIO{} rx=GPIO{} baud={} channel={} freq={}MHz air_rate={}bps tx_power={}dBm net_id=0x{:04x}",
        pins.lora_uart_tx,
        pins.lora_uart_rx,
        lora.baudrate,
        lora.channel,
        lora.frequency_mhz,
        lora.air_rate_bps,
        lora.tx_power_dbm,
        lora.net_id
    );
    println!(
        "lora module config: module={} mode={}",
        lora_plan.module,
        lora_plan.mode_label()
    );

    let mut uart = Uart::new(
        peripherals.UART1,
        UartConfig::default().with_baudrate(lora.baudrate),
    )?
    .with_rx(peripherals.GPIO20)
    .with_tx(peripherals.GPIO21);

    // Configure DX-LR32 module via AT commands at boot
    esp32c3_rust::transport::configure_dx_lr32_module(&mut uart).await?;
    esp32c3_rust::transport::drain_uart(&mut uart);

    let mut lora_transport = LoraUartTransport::new(uart);

    #[cfg(feature = "gateway-node")]
    let mut buzzer = Output::new(peripherals.GPIO10, Level::High, OutputConfig::default());

    #[cfg(not(feature = "gateway-node"))]
    let _ = peripherals.GPIO10;

    #[cfg(feature = "sensor-node")]
    let mut sht40 = {
        let i2c = I2c::new(
            peripherals.I2C0,
            I2cConfig::default().with_frequency(Rate::from_khz(400)),
        )?
        .with_sda(peripherals.GPIO5)
        .with_scl(peripherals.GPIO4);
        Sht40::new(i2c, Sht40Config::DEFAULT)
    };

    let mut node = DemoNode::new(ACTIVE_ROLE);
    let mut gateway_alarm_active = false;
    let mut sensor_alarm_latch = AlarmLatch::new();
    let mut pending_ack = PendingAck::new();
    let mut relay_forward = RelayForwardBuffer::new();
    // SYNC re-broadcast is deferred to the relay control slot instead of being
    // sent inline, so it stays inside the TDMA schedule.
    let mut pending_sync_forward: Option<Frame> = None;
    // A received ALARM is forwarded by the relay in its own slot (slot 3) rather
    // than inline on receipt, so the second hop stays inside the TDMA schedule.
    let mut pending_alarm_forward: Option<Frame> = None;
    let mut gateway_stats = GatewayStats::new();
    let boot = Instant::now();
    // Tracks the last HELLO send while searching so retries run on a local timer
    // (decoupled from the unsynchronized clock's arbitrary slot boundaries).
    let mut last_hello_ms = elapsed_ms(boot);

    match ACTIVE_ROLE {
        NodeRole::Gateway => {
            node.mark_joined();
            println!("gateway online: broadcasting SYNC/SCHEDULE every TDMA superframe");
        }
        NodeRole::Relay | NodeRole::Sensor => {
            let frame = node.make_hello(elapsed_ms(boot))?;
            let bytes = lora_transport.send_frame(&frame)?;
            println!(
                "{} searching: send {} seq={} bytes={} parent={:?}",
                ACTIVE_ROLE, frame.frame_type, frame.seq, bytes, node.parent_id
            );
            println!(
                "{} searching: preferred_parent={:?} hop={} slot={}",
                ACTIVE_ROLE, node.parent_id, node.hop, node.slot_id
            );
        }
    }

    loop {
        let local_time_ms = elapsed_ms(boot);
        let slot = node
            .schedule
            .slot_at(node.sync.gateway_time_ms(local_time_ms));

        service_rx(
            &mut node,
            &mut lora_transport,
            boot,
            #[cfg(feature = "gateway-node")]
            &mut buzzer,
            &mut gateway_alarm_active,
            &mut pending_ack,
            &mut relay_forward,
            &mut pending_sync_forward,
            &mut pending_alarm_forward,
            &mut gateway_stats,
        )?;

        #[cfg(feature = "gateway-node")]
        {
            let gateway_time_ms = node.sync.gateway_time_ms(elapsed_ms(boot));
            gateway_stats.update_sync(
                node.sync.last_sync_seq,
                node.sync.offset_ms,
                node.sync.offset_delta_ms,
            );
            if gateway_stats.should_report(gateway_time_ms) {
                demo_log::print_gateway_stats(&gateway_stats, gateway_time_ms);
                gateway_stats.mark_reported(gateway_time_ms);
            }
        }

        // Only follow the TDMA schedule once the clock is trustworthy (synced)
        // and, for relay/sensor, the node has joined. Until then stay in RX and
        // retry HELLO on a local timer so an unsynchronized clock never drives
        // slot-based transmission.
        let follow_schedule = node.is_synced()
            && (node.role == NodeRole::Gateway || node.phase == NetworkPhase::Joined);

        if !follow_schedule {
            if node.phase == NetworkPhase::Searching
                && matches!(ACTIVE_ROLE, NodeRole::Relay | NodeRole::Sensor)
                && local_time_ms.saturating_sub(last_hello_ms) >= HELLO_RETRY_INTERVAL_MS
            {
                let frame = node.make_hello(local_time_ms)?;
                let bytes = lora_transport.send_frame(&frame)?;
                last_hello_ms = local_time_ms;
                println!(
                    "{} searching: retry {} seq={} bytes={}",
                    ACTIVE_ROLE, frame.frame_type, frame.seq, bytes
                );
            }
        } else if let Some(local_time_ms) = enter_tx_window(&node, boot).await {
            match ACTIVE_ROLE {
                NodeRole::Gateway if slot == node.schedule.sync_slot => {
                    let frame = node.make_sync(local_time_ms)?;
                    let bytes = lora_transport.send_frame(&frame)?;
                    println!(
                        "tx {} frame_seq={} sync_seq={} schedule_v={} active={}ms guard_before={}ms gateway_time={} slot={} bytes={}",
                        frame.frame_type,
                        frame.seq,
                        node.sync_seq,
                        node.schedule.schedule_version,
                        node.schedule.active_ms,
                        node.schedule.guard_before_ms,
                        frame.gateway_time_ms,
                        slot,
                        bytes
                    );

                    #[cfg(feature = "gateway-node")]
                    {
                        if !gateway_alarm_active {
                            buzzer.set_high();
                        }
                    }
                }
                NodeRole::Relay | NodeRole::Sensor
                    if node.phase == NetworkPhase::Joined
                        && slot == node.schedule.alarm_retry_slot =>
                {
                    if let Some((frame, attempt)) = pending_ack.next_retry(local_time_ms) {
                        let bytes = lora_transport.send_frame(&frame)?;
                        println!(
                            "{} retry {} seq={} attempt={} bytes={}",
                            ACTIVE_ROLE, frame.frame_type, frame.seq, attempt, bytes
                        );
                    } else if pending_ack.is_exhausted(local_time_ms) {
                        if let Some((seq, frame_type)) = pending_ack.drop_exhausted() {
                            println!(
                                "{} drop {} seq={} after {} attempts",
                                ACTIVE_ROLE,
                                frame_type,
                                seq,
                                PendingAck::MAX_ATTEMPTS
                            );
                        }
                    }
                }
                NodeRole::Relay
                    if node.phase == NetworkPhase::Joined
                        && slot == node.schedule.relay_control_slot =>
                {
                    // Re-broadcast the latest SYNC inside the reserved control
                    // slot instead of inline on receipt, keeping it in-schedule.
                    if let Some(sync_frame) = pending_sync_forward.take() {
                        let forwarded = node.make_forwarded(
                            &sync_frame,
                            esp32c3_rust::role::BROADCAST_ID,
                            local_time_ms,
                        )?;
                        let bytes = lora_transport.send_frame(&forwarded)?;
                        println!(
                            "relay control slot: forward SYNC sync_seq={} offset_ms={} drift={}ms bytes={}",
                            node.sync.last_sync_seq,
                            node.sync.offset_ms,
                            node.sync.offset_delta_ms,
                            bytes
                        );
                    }
                }
                NodeRole::Relay
                    if node.phase == NetworkPhase::Joined
                        && slot == node.schedule.relay_heartbeat_slot =>
                {
                    let frame = node.make_heartbeat(local_time_ms)?;
                    let bytes = lora_transport.send_frame(&frame)?;
                    println!(
                        "{} tx HEARTBEAT seq={} parent={:?} data_slot={} heartbeat_slot={} sync_seq={} offset_ms={} drift={}ms bytes={}",
                        ACTIVE_ROLE,
                        frame.seq,
                        node.parent_id,
                        node.slot_id,
                        node.schedule.relay_heartbeat_slot,
                        node.sync.last_sync_seq,
                        node.sync.offset_ms,
                        node.sync.offset_delta_ms,
                        bytes
                    );
                }
                NodeRole::Sensor
                    if node.phase == NetworkPhase::Joined
                        && slot == node.schedule.sensor_heartbeat_slot =>
                {
                    let frame = node.make_heartbeat(local_time_ms)?;
                    let bytes = lora_transport.send_frame(&frame)?;
                    println!(
                        "{} tx HEARTBEAT seq={} parent={:?} data_slot={} heartbeat_slot={} sync_seq={} offset_ms={} drift={}ms bytes={}",
                        ACTIVE_ROLE,
                        frame.seq,
                        node.parent_id,
                        node.slot_id,
                        node.schedule.sensor_heartbeat_slot,
                        node.sync.last_sync_seq,
                        node.sync.offset_ms,
                        node.sync.offset_delta_ms,
                        bytes
                    );
                }
                NodeRole::Sensor
                    if node.phase == NetworkPhase::Joined && slot == node.schedule.sensor_slot =>
                {
                    #[cfg(feature = "sensor-node")]
                    let sample = read_environment_sample(&mut sht40).await;

                    #[cfg(not(feature = "sensor-node"))]
                    let sample = read_environment_sample().await;

                    let alarm_transition = sensor_alarm_latch.update(
                        sample,
                        Sht40Config::DEFAULT.temp_alarm_centi_c,
                        Sht40Config::DEFAULT.humidity_alarm_centi_percent,
                        Sht40Config::DEFAULT.temp_clear_centi_c,
                        Sht40Config::DEFAULT.humidity_clear_centi_percent,
                    );
                    let alarm = sensor_alarm_latch.is_active();
                    match alarm_transition {
                        AlarmTransition::Raised => println!(
                            "sensor alarm raised: temp={}.{:02}C humidity={}.{:02}% thresholds={}cC/{}c%",
                            sample.temp_centi_c / 100,
                            sample.temp_centi_c.unsigned_abs() % 100,
                            sample.humidity_centi_percent / 100,
                            sample.humidity_centi_percent % 100,
                            Sht40Config::DEFAULT.temp_alarm_centi_c,
                            Sht40Config::DEFAULT.humidity_alarm_centi_percent
                        ),
                        AlarmTransition::Cleared => {
                            if pending_ack.cancel_if_matches(FrameType::Alarm) {
                                println!(
                                    "sensor alarm cleared locally: cancel pending ALARM retry"
                                );
                            }
                            println!(
                                "sensor alarm cleared: temp={}.{:02}C humidity={}.{:02}% clear_thresholds={}cC/{}c%",
                                sample.temp_centi_c / 100,
                                sample.temp_centi_c.unsigned_abs() % 100,
                                sample.humidity_centi_percent / 100,
                                sample.humidity_centi_percent % 100,
                                Sht40Config::DEFAULT.temp_clear_centi_c,
                                Sht40Config::DEFAULT.humidity_clear_centi_percent
                            );
                        }
                        AlarmTransition::Unchanged => {}
                    }
                    let frame = if alarm {
                        node.make_alarm(
                            local_time_ms,
                            sample.temp_centi_c,
                            sample.humidity_centi_percent,
                        )?
                    } else {
                        node.make_data(
                            local_time_ms,
                            sample.temp_centi_c,
                            sample.humidity_centi_percent,
                        )?
                    };
                    // ALARM is covered by hop-by-hop ACK/retry, so send it once;
                    // best-effort DATA is repeated for redundancy (receivers
                    // de-duplicate on (src_id, seq)).
                    let bytes = if alarm {
                        lora_transport.send_frame(&frame)?
                    } else {
                        send_best_effort(&mut lora_transport, &frame, BEST_EFFORT_TX_REPEATS)
                            .await?
                    };
                    pending_ack.remember(&frame, local_time_ms);
                    println!(
                        "tx {} seq={} parent={:?} temp={}.{:02}C humidity={}.{:02}% gateway_time={} bytes={}",
                        frame.frame_type,
                        frame.seq,
                        node.parent_id,
                        sample.temp_centi_c / 100,
                        sample.temp_centi_c.unsigned_abs() % 100,
                        sample.humidity_centi_percent / 100,
                        sample.humidity_centi_percent % 100,
                        frame.gateway_time_ms,
                        bytes
                    );
                }
                NodeRole::Relay
                    if node.phase == NetworkPhase::Joined && slot == node.schedule.relay_slot =>
                {
                    // Second ALARM hop (relay -> gateway) runs here, inside the
                    // relay slot, instead of inline on receipt — keeping it in the
                    // TDMA schedule. It carries an ACK and is retried in slot 4.
                    if let Some(alarm_frame) = pending_alarm_forward.take() {
                        let forwarded =
                            node.make_forwarded(&alarm_frame, GATEWAY_ID, local_time_ms)?;
                        let origin_seq = origin_seq(&alarm_frame);
                        let bytes = lora_transport.send_frame(&forwarded)?;
                        pending_ack.remember(&forwarded, local_time_ms);
                        println!(
                            "relay slot tx ALARM origin_seq={} relay_seq={} ack_required=true bytes={}",
                            origin_seq, forwarded.seq, bytes
                        );
                    }
                    // Forward up to MAX_FORWARD_PER_SLOT buffered DATA frames so a
                    // backlog drains instead of being overwritten; each is sent
                    // best-effort (repeated) since only ALARM carries an ACK.
                    let mut forwarded_count = 0;
                    while forwarded_count < MAX_FORWARD_PER_SLOT {
                        let Some(frame) = relay_forward.take() else {
                            break;
                        };
                        let forwarded = node.make_forwarded(&frame, GATEWAY_ID, local_time_ms)?;
                        let bytes = send_best_effort(
                            &mut lora_transport,
                            &forwarded,
                            BEST_EFFORT_TX_REPEATS,
                        )
                        .await?;
                        let origin_seq = origin_seq(&frame);
                        pending_ack.remember(&forwarded, local_time_ms);
                        forwarded_count += 1;
                        println!(
                            "relay slot tx buffered {} origin_seq={} relay_seq={} ack_required={} bytes={}",
                            frame.frame_type,
                            origin_seq,
                            forwarded.seq,
                            is_ack_required(forwarded.frame_type),
                            bytes
                        );
                    }
                    println!(
                        "relay slot active: buffered_data={} pending_ack={} parent={:?} hop={} sync_seq={} offset_ms={} offset_delta={}ms",
                        relay_forward.has_pending(),
                        pending_ack.has_pending(),
                        node.parent_id,
                        node.hop,
                        node.sync.last_sync_seq,
                        node.sync.offset_ms,
                        node.sync.offset_delta_ms
                    );
                }
                NodeRole::Gateway if slot == node.schedule.alarm_retry_slot => {}
                NodeRole::Relay if slot == node.schedule.alarm_retry_slot => {}
                NodeRole::Sensor if slot == node.schedule.alarm_retry_slot => {}
                NodeRole::Gateway if slot == node.schedule.quiet_slot => {}
                NodeRole::Relay if slot == node.schedule.quiet_slot => {}
                NodeRole::Sensor if slot == node.schedule.quiet_slot => {}
                _ => {}
            }
        }

        // Service the radio continuously until the next slot boundary instead of
        // sleeping through the whole slot, so frames are pulled out of the UART
        // FIFO promptly (no aging/overflow) and handled with minimal latency.
        loop {
            let gateway_time_ms = node.sync.gateway_time_ms(elapsed_ms(boot));
            let remaining_ms = node.schedule.next_slot_delay_ms(gateway_time_ms);
            if remaining_ms <= RX_POLL_INTERVAL_MS {
                Timer::after(Duration::from_millis(remaining_ms.max(2) as u64)).await;
                break;
            }
            Timer::after(Duration::from_millis(RX_POLL_INTERVAL_MS as u64)).await;
            service_rx(
                &mut node,
                &mut lora_transport,
                boot,
                #[cfg(feature = "gateway-node")]
                &mut buzzer,
                &mut gateway_alarm_active,
                &mut pending_ack,
                &mut relay_forward,
                &mut pending_sync_forward,
                &mut pending_alarm_forward,
                &mut gateway_stats,
            )?;
        }
    }
}

const MAX_RX_FRAMES_PER_LOOP: usize = 4;
/// Poll the radio at least this often so received frames are pulled out of the
/// UART hardware FIFO promptly instead of aging/overflowing across a full slot.
const RX_POLL_INTERVAL_MS: u32 = 20;
/// Maximum buffered DATA frames the relay forwards within one relay slot,
/// bounded so the combined airtime stays inside the active window.
const MAX_FORWARD_PER_SLOT: usize = 2;
/// Best-effort DATA/forward frames are sent this many times for redundancy.
/// Receivers de-duplicate on (src_id, seq), so repeats are transparent.
const BEST_EFFORT_TX_REPEATS: u8 = 2;
/// Spacing between best-effort repeats to decorrelate bursty interference.
const BEST_EFFORT_REPEAT_GAP_MS: u64 = 40;
/// While searching (unsynced), retry HELLO on this local-time cadence rather
/// than on TDMA slot boundaries, which are meaningless before the clock is set.
const HELLO_RETRY_INTERVAL_MS: u64 = 2_000;

async fn enter_tx_window(node: &DemoNode, boot: Instant) -> Option<u64> {
    let local_time_ms = elapsed_ms(boot);
    let gateway_time_ms = node.sync.gateway_time_ms(local_time_ms);
    let elapsed = node.schedule.slot_elapsed_ms(gateway_time_ms);

    if elapsed < node.schedule.guard_before_ms {
        let wait_ms = node.schedule.guard_before_ms - elapsed;
        Timer::after(Duration::from_millis(wait_ms as u64)).await;
    }

    let local_time_ms = elapsed_ms(boot);
    let gateway_time_ms = node.sync.gateway_time_ms(local_time_ms);
    if node.schedule.is_active_window(gateway_time_ms) {
        Some(local_time_ms)
    } else {
        None
    }
}

/// Drain the UART FIFO and handle any complete frames. Called frequently (every
/// `RX_POLL_INTERVAL_MS`) so received frames never age in the hardware FIFO.
fn service_rx(
    node: &mut DemoNode,
    lora_transport: &mut LoraUartTransport<'_>,
    boot: Instant,
    #[cfg(feature = "gateway-node")] buzzer: &mut Output<'_>,
    gateway_alarm_active: &mut bool,
    pending_ack: &mut PendingAck,
    relay_forward: &mut RelayForwardBuffer,
    pending_sync_forward: &mut Option<Frame>,
    pending_alarm_forward: &mut Option<Frame>,
    gateway_stats: &mut GatewayStats,
) -> AppResult<()> {
    let local_time_ms = elapsed_ms(boot);
    if let Err(error) = lora_transport.drain_to_decoder() {
        log::warn!("LoRa RX drain failed: {}", error);
    }
    for _ in 0..MAX_RX_FRAMES_PER_LOOP {
        match lora_transport.next_decoded_frame() {
            Ok(Some(frame)) => handle_received_frame(
                node,
                lora_transport,
                &frame,
                local_time_ms,
                #[cfg(feature = "gateway-node")]
                buzzer,
                gateway_alarm_active,
                pending_ack,
                relay_forward,
                pending_sync_forward,
                pending_alarm_forward,
                gateway_stats,
            )?,
            Ok(None) => break,
            Err(error) => {
                log::warn!("LoRa RX decode error: {}", error);
                break;
            }
        }
    }
    Ok(())
}

/// Send a best-effort frame `repeats` times with a small inter-repeat gap.
/// Receivers de-duplicate on (src_id, seq), so the repeats add redundancy
/// without delivering duplicates to the application.
async fn send_best_effort(
    lora_transport: &mut LoraUartTransport<'_>,
    frame: &Frame,
    repeats: u8,
) -> AppResult<usize> {
    let mut bytes = 0;
    for attempt in 0..repeats.max(1) {
        if attempt > 0 {
            Timer::after(Duration::from_millis(BEST_EFFORT_REPEAT_GAP_MS)).await;
        }
        bytes = lora_transport.send_frame(frame)?;
    }
    Ok(bytes)
}

fn handle_received_frame(
    node: &mut DemoNode,
    lora_transport: &mut LoraUartTransport<'_>,
    frame: &Frame,
    local_time_ms: u64,
    #[cfg(feature = "gateway-node")] buzzer: &mut Output<'_>,
    gateway_alarm_active: &mut bool,
    pending_ack: &mut PendingAck,
    relay_forward: &mut RelayForwardBuffer,
    pending_sync_forward: &mut Option<Frame>,
    pending_alarm_forward: &mut Option<Frame>,
    gateway_stats: &mut GatewayStats,
) -> AppResult<()> {
    #[cfg(not(feature = "gateway-node"))]
    let _ = gateway_alarm_active;

    // TDMA slot-strict reception: once our clock is trusted, drop periodic
    // scheduled traffic that arrives from a sender outside its assigned slot
    // instead of acting on out-of-slot (likely unsynchronized) frames.
    if !node.accepts_frame_slot(frame) {
        gateway_stats.record_slot_violation();
        log::warn!(
            "drop out-of-slot {} from {} claimed_slot={}",
            frame.frame_type,
            frame.src_id,
            node.schedule.slot_at(frame.gateway_time_ms)
        );
        return Ok(());
    }

    let action = node.apply_frame(frame, local_time_ms);

    match (node.role, frame.frame_type, action) {
        (NodeRole::Gateway, FrameType::Hello, _) if frame.node_role == NodeRole::Relay => {
            let ack = node.make_join_ack(frame.src_id, frame.node_role, local_time_ms)?;
            let bytes = lora_transport.send_frame(&ack)?;
            println!(
                "rx HELLO from={} role={} -> tx JOIN_ACK seq={} bytes={}",
                frame.src_id, frame.node_role, ack.seq, bytes
            );
        }
        (NodeRole::Gateway, FrameType::Hello, _)
            if frame.node_role == NodeRole::Sensor
                && frame.hop > 0
                && frame.dst_id == GATEWAY_ID =>
        {
            println!(
                "topology: sensor({}) -> relay({}) -> gateway({})  hop={}",
                frame.src_id,
                esp32c3_rust::role::RELAY_ID,
                GATEWAY_ID,
                frame.hop
            );
        }
        (NodeRole::Gateway, FrameType::Hello, _) => {
            println!(
                "rx HELLO from={} role={} ignored by gateway: demo topology requires sensor -> relay -> gateway",
                frame.src_id, frame.node_role
            );
        }
        (
            NodeRole::Gateway,
            FrameType::Data | FrameType::Alarm,
            FrameAction::Data {
                origin_id,
                origin_seq,
                temp_centi_c,
                humidity_centi_percent,
                alarm,
            },
        ) => {
            let mut ack_seq = None;
            // Only ALARM gets ACK for guaranteed delivery
            if alarm {
                let ack =
                    node.make_ack(frame.src_id, frame.seq, frame.frame_type, local_time_ms)?;
                let _bytes = lora_transport.send_frame(&ack)?;
                ack_seq = Some(ack.seq);
            }

            #[cfg(feature = "gateway-node")]
            let mut buzzer_action = BuzzerAction::Unchanged;
            #[cfg(not(feature = "gateway-node"))]
            let buzzer_action = BuzzerAction::Unchanged;
            #[cfg(feature = "gateway-node")]
            {
                if alarm {
                    *gateway_alarm_active = true;
                    buzzer.set_low();
                    buzzer_action = BuzzerAction::On;
                } else if frame.frame_type == FrameType::Data && *gateway_alarm_active {
                    *gateway_alarm_active = false;
                    buzzer.set_high();
                    buzzer_action = BuzzerAction::Off;
                }
            }

            let origin_seq_gap =
                gateway_stats.record_rx_data(frame.frame_type, origin_seq, ack_seq.is_some());
            demo_log::print_gateway_rx_data(&GatewayRxDataLog {
                frame_type: frame.frame_type,
                gateway_time_ms: frame.gateway_time_ms,
                origin_id,
                origin_seq,
                via_id: frame.src_id,
                relay_seq: frame.seq,
                hop: frame.hop.saturating_add(1),
                temp_centi_c,
                humidity_centi_percent,
                alarm,
                ack_seq,
                buzzer_action,
                origin_seq_gap,
            });
        }
        (
            NodeRole::Gateway,
            FrameType::Heartbeat,
            FrameAction::Heartbeat {
                slot_id,
                hop,
                sync_seq,
            },
        ) => {
            demo_log::print_gateway_heartbeat(frame, slot_id, sync_seq);
            let _ = hop;
        }
        (NodeRole::Relay, FrameType::Hello, _) if frame.node_role == NodeRole::Sensor => {
            let ack = node.make_join_ack(frame.src_id, frame.node_role, local_time_ms)?;
            let ack_bytes = lora_transport.send_frame(&ack)?;
            println!(
                "rx sensor HELLO from={} -> tx JOIN_ACK seq={} bytes={}",
                frame.src_id, ack.seq, ack_bytes
            );

            // Forward the HELLO to the gateway so it can log the topology
            let mut notify = frame.clone();
            notify.dst_id = GATEWAY_ID;
            notify.hop = node.hop;
            let notify_bytes = lora_transport.send_frame(&notify)?;
            println!(
                "forward sensor HELLO to gateway: src={} hop={} bytes={}",
                notify.src_id, notify.hop, notify_bytes
            );
        }
        (
            NodeRole::Relay,
            FrameType::Heartbeat,
            FrameAction::Heartbeat {
                slot_id,
                hop,
                sync_seq,
            },
        ) => {
            println!(
                "rx HEARTBEAT from={} reported_data_slot={} hop={} sync_seq={}",
                frame.src_id, slot_id, hop, sync_seq
            );
        }
        (
            NodeRole::Relay,
            FrameType::Sync | FrameType::Schedule,
            FrameAction::Synced {
                sync_seq,
                offset_ms,
            },
        ) => {
            // Defer the re-broadcast to the relay control slot (handled in the
            // main loop) so it stays inside the TDMA schedule instead of firing
            // inline during the gateway's SYNC slot.
            *pending_sync_forward = Some(frame.clone());
            println!(
                "rx {} sync_seq={} offset_ms={} drift={}ms -> queued for control slot",
                frame.frame_type, sync_seq, offset_ms, node.sync.offset_delta_ms
            );
        }
        (
            NodeRole::Relay,
            FrameType::Data | FrameType::Alarm,
            FrameAction::Data {
                origin_id,
                origin_seq,
                temp_centi_c,
                humidity_centi_percent,
                alarm,
            },
        ) => {
            if alarm {
                // First ALARM hop (sensor -> relay): ACK the sensor immediately
                // so it stops retrying this hop, then buffer the ALARM for the
                // relay slot rather than forwarding inline. The second hop
                // (relay -> gateway) and its retries are scheduled in slot 3/4.
                let ack =
                    node.make_ack(frame.src_id, frame.seq, frame.frame_type, local_time_ms)?;
                let ack_bytes = lora_transport.send_frame(&ack)?;
                *pending_alarm_forward = Some(frame.clone());
                println!(
                    "rx {} origin={} origin_seq={} from={} temp={}.{:02}C humidity={}.{:02}% alarm=true -> ack_bytes={} queued_for_relay_slot",
                    frame.frame_type,
                    origin_id,
                    origin_seq,
                    frame.src_id,
                    temp_centi_c / 100,
                    temp_centi_c.unsigned_abs() % 100,
                    humidity_centi_percent / 100,
                    humidity_centi_percent % 100,
                    ack_bytes
                );
            } else {
                relay_forward.remember(frame);
                println!(
                    "rx DATA origin={} origin_seq={} from={} temp={}.{:02}C humidity={}.{:02}% -> buffered_for_relay_slot",
                    origin_id,
                    origin_seq,
                    frame.src_id,
                    temp_centi_c / 100,
                    temp_centi_c.unsigned_abs() % 100,
                    humidity_centi_percent / 100,
                    humidity_centi_percent % 100
                );
            }
        }
        (
            NodeRole::Sensor | NodeRole::Relay,
            FrameType::JoinAck,
            FrameAction::Joined {
                parent_id,
                hop,
                slot_id,
            },
        ) => {
            println!(
                "rx JOIN_ACK: parent={} hop={} slot={}",
                parent_id, hop, slot_id
            );
        }
        (
            NodeRole::Sensor,
            FrameType::Sync | FrameType::Schedule,
            FrameAction::Synced {
                sync_seq,
                offset_ms,
            },
        ) => {
            println!(
                "rx {}: sync_seq={} offset_ms={} drift={}ms",
                frame.frame_type, sync_seq, offset_ms, node.sync.offset_delta_ms
            );
        }
        (
            NodeRole::Sensor | NodeRole::Relay,
            FrameType::Ack,
            FrameAction::Ack {
                acked_seq,
                acked_type,
            },
        ) => {
            let cleared = pending_ack.clear_if_matches(acked_seq, acked_type);
            println!(
                "rx ACK from={} acked_seq={} acked_type={} cleared_pending={}",
                frame.src_id, acked_seq, acked_type, cleared
            );
        }
        _ => {}
    }

    Ok(())
}

#[derive(Debug, Default)]
struct PendingAck {
    frame: Option<Frame>,
    attempts: u8,
    last_sent_ms: u64,
}

impl PendingAck {
    const MAX_ATTEMPTS: u8 = 3;
    const RETRY_DELAY_MS: u64 = 2_000;

    const fn new() -> Self {
        Self {
            frame: None,
            attempts: 0,
            last_sent_ms: 0,
        }
    }

    fn has_pending(&self) -> bool {
        self.frame.is_some()
    }

    fn remember(&mut self, frame: &Frame, now_ms: u64) {
        if is_ack_required(frame.frame_type) {
            self.frame = Some(frame.clone());
            self.attempts = 1;
            self.last_sent_ms = now_ms;
        }
    }

    fn clear_if_matches(&mut self, acked_seq: u16, acked_type: FrameType) -> bool {
        let Some(frame) = &self.frame else {
            return false;
        };

        if frame.seq == acked_seq && frame.frame_type == acked_type {
            self.frame = None;
            self.attempts = 0;
            self.last_sent_ms = 0;
            true
        } else {
            false
        }
    }

    fn cancel_if_matches(&mut self, frame_type: FrameType) -> bool {
        let Some(frame) = &self.frame else {
            return false;
        };

        if frame.frame_type == frame_type {
            self.frame = None;
            self.attempts = 0;
            self.last_sent_ms = 0;
            true
        } else {
            false
        }
    }

    fn next_retry(&mut self, now_ms: u64) -> Option<(Frame, u8)> {
        let frame = self.frame.as_ref()?;
        if self.attempts >= Self::MAX_ATTEMPTS {
            return None;
        }
        if now_ms.saturating_sub(self.last_sent_ms) < Self::RETRY_DELAY_MS {
            return None;
        }

        self.attempts = self.attempts.saturating_add(1);
        self.last_sent_ms = now_ms;
        Some((frame.clone(), self.attempts))
    }

    fn is_exhausted(&self, now_ms: u64) -> bool {
        self.frame.is_some()
            && self.attempts >= Self::MAX_ATTEMPTS
            && now_ms.saturating_sub(self.last_sent_ms) >= Self::RETRY_DELAY_MS
    }

    fn drop_exhausted(&mut self) -> Option<(u16, FrameType)> {
        let frame = self.frame.take()?;
        self.attempts = 0;
        self.last_sent_ms = 0;
        Some((frame.seq, frame.frame_type))
    }
}

fn is_ack_required(frame_type: FrameType) -> bool {
    // Only ALARM needs guaranteed delivery with ACK/retry.
    // DATA and HEARTBEAT are periodic best-effort — one lost is acceptable.
    matches!(frame_type, FrameType::Alarm)
}

fn origin_seq(frame: &Frame) -> u16 {
    protocol::decode_data_payload(&frame.payload)
        .map(|(_, origin_seq, _, _)| origin_seq)
        .unwrap_or(frame.seq)
}

#[cfg(feature = "sensor-node")]
async fn read_environment_sample(sht40: &mut Sht40<'_>) -> EnvironmentSample {
    match sht40.read_sample().await {
        Ok(sample) => sample,
        Err(error) => {
            log::warn!("SHT40 read failed, using demo sample: {}", error);
            EnvironmentSample::normal()
        }
    }
}

#[cfg(not(feature = "sensor-node"))]
async fn read_environment_sample() -> EnvironmentSample {
    EnvironmentSample::normal()
}

fn elapsed_ms(boot: Instant) -> u64 {
    boot.elapsed().as_millis()
}

fn panic_on_fatal_error(error: AppError) -> ! {
    log::error!("fatal application error: {}", error);
    println!("FATAL application error: {}", error);
    panic!("fatal application error: {}", error);
}
