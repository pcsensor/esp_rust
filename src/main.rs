//! Firmware entry point and async task wiring for the LoRa demo nodes.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
#[cfg(not(feature = "sensor-node"))]
use embassy_futures::select::{Either, select};
#[cfg(feature = "sensor-node")]
use embassy_futures::select::{Either3, select3};
#[cfg(any(feature = "gateway-node", feature = "sensor-node"))]
use embassy_sync::signal::Signal;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
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
    demo::{DemoNode, FrameAction, GatewayHeartbeatState, NetworkPhase, RelayHeartbeatState},
    demo_log::{self, BuzzerAction, GatewayRxDataLog, GatewayStats},
    hardware::{LoraModuleConfigPlan, LoraUartConfig, PinConfig},
    protocol::{self, Frame, FrameType},
    relay::RelayForwardBuffer,
    role::{ACTIVE_ROLE, GATEWAY_ID, NodeRole},
    transport::{LoraRx, LoraTx},
};
#[cfg(feature = "sensor-node")]
use esp32c3_rust::{
    demo::{AlarmLatch, AlarmTransition, EnvironmentSample},
    hardware::Sht40Config,
    tdma::TdmaSchedule,
};

esp_bootloader_esp_idf::esp_app_desc!();

/// Depth of the RX frame channel feeding `core` from `lora_rx_task`. Sized with
/// headroom so a burst of decoded frames never blocks the RX task; `core` is the
/// single consumer and drains it promptly.
const RX_DEPTH: usize = 8;

/// Decoded frames flow from the interrupt-driven `lora_rx_task` to `core` here.
static RX_FRAMES: Channel<CriticalSectionRawMutex, Frame, RX_DEPTH> = Channel::new();

#[cfg(feature = "sensor-node")]
static SENSOR_LATEST: Signal<CriticalSectionRawMutex, EnvironmentSample> = Signal::new();

#[cfg(feature = "sensor-node")]
static ALARM_EVENTS: Channel<CriticalSectionRawMutex, AlarmTransition, 4> = Channel::new();

#[cfg(feature = "gateway-node")]
static BUZZER_SIG: Signal<CriticalSectionRawMutex, BuzzerAction> = Signal::new();

#[cfg(feature = "sensor-node")]
const SENSOR_SAMPLE_INTERVAL_MS: u64 = TdmaSchedule::DEMO.superframe_ms as u64;

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    if let Err(error) = run(spawner).await {
        panic_on_fatal_error(error);
    }

    // All spawned tasks (core, lora_rx, sensor, buzzer) are now running on the
    // executor. Yield forever so the executor continues to schedule them.
    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}

#[cfg(feature = "sensor-node")]
#[embassy_executor::task]
async fn sensor_task(mut sht40: Sht40<'static>) {
    let mut alarm_latch = AlarmLatch::new();

    loop {
        let sample = read_environment_sample(&mut sht40).await;
        let alarm_transition = alarm_latch.update(
            sample,
            Sht40Config::DEFAULT.temp_alarm_centi_c,
            Sht40Config::DEFAULT.humidity_alarm_centi_percent,
            Sht40Config::DEFAULT.temp_clear_centi_c,
            Sht40Config::DEFAULT.humidity_clear_centi_percent,
        );

        SENSOR_LATEST.signal(sample);
        if alarm_transition != AlarmTransition::Unchanged {
            ALARM_EVENTS.sender().send(alarm_transition).await;
        }

        Timer::after(Duration::from_millis(SENSOR_SAMPLE_INTERVAL_MS)).await;
    }
}

#[cfg(feature = "gateway-node")]
#[embassy_executor::task]
async fn buzzer_task(mut buzzer: Output<'static>) {
    loop {
        match BUZZER_SIG.wait().await {
            BuzzerAction::On => buzzer.set_low(),
            BuzzerAction::Off => buzzer.set_high(),
            BuzzerAction::Unchanged => {}
        }
    }
}

/// Interrupt-driven RX task: the sole owner of the UART RX half and frame
/// decoder. It awaits incoming bytes, decodes complete frames, and hands them to
/// `core` over `RX_FRAMES`. Because it runs independently of TX/TDMA timing, the
/// hardware FIFO is drained promptly with no aging/overflow and no polling.
#[embassy_executor::task]
async fn lora_rx_task(mut lora_rx: LoraRx<'static>) {
    loop {
        match lora_rx.read_into_decoder().await {
            Ok(_) => loop {
                match lora_rx.next_decoded_frame() {
                    Ok(Some(frame)) => RX_FRAMES.sender().send(frame).await,
                    Ok(None) => break,
                    Err(error) => {
                        log::warn!("LoRa RX decode error: {}", error);
                        break;
                    }
                }
            },
            Err(error) => {
                log::warn!("LoRa RX read failed: {}", error);
                // Back off briefly so a persistent error can't spin the task.
                Timer::after(Duration::from_millis(5)).await;
            }
        }
    }
}

/// Core protocol task: sole owner of the network node state and the UART TX
/// half. Wakes on incoming frames (from the RX channel) or TDMA slot timers,
/// handles protocol logic inline, and transmits in its assigned slot window.
/// All protocol state lives here — no locks needed. Panics on fatal error
/// (matching the original `run()` behaviour).
#[embassy_executor::task]
async fn core_task(
    mut lora_tx: LoraTx<'static>,
    mut node: DemoNode,
    mut gateway_alarm_active: bool,
    #[cfg(feature = "sensor-node")] mut latest_sensor_sample: EnvironmentSample,
    #[cfg(feature = "sensor-node")] mut sensor_alarm_active: bool,
    mut pending_ack: PendingAck,
    mut relay_forward: RelayForwardBuffer,
    mut gateway_stats: GatewayStats,
    mut relay_heartbeat: RelayHeartbeatState,
    mut gateway_heartbeat: GatewayHeartbeatState,
    boot: Instant,
) {
    // Wrap the body in an async block so `?` propagates errors. The loop is
    // infinite — only an error (via `?`) can exit the block.
    if let Err(error) = async {
        let mut pending_sync_forward: Option<Frame> = None;
        let mut pending_alarm_forward: Option<Frame> = None;
        let mut last_hello_ms = elapsed_ms(boot);

        match ACTIVE_ROLE {
            NodeRole::Gateway => {
                node.mark_joined();
                println!("gateway online: broadcasting SYNC/SCHEDULE every TDMA superframe");
            }
            NodeRole::Relay | NodeRole::Sensor => {
                let frame = node.make_hello(elapsed_ms(boot))?;
                let bytes = lora_tx.send_frame(&frame).await?;
                println!(
                    "{} searching: send {} seq={} bytes={} parent={:?}",
                    ACTIVE_ROLE, frame.frame_type, frame.seq, bytes, node.parent_id()
                );
                println!(
                    "{} searching: preferred_parent={:?} hop={} slot={}",
                    ACTIVE_ROLE, node.parent_id(), node.hop(), node.slot_id()
                );
            }
        }

        let mut last_tx_slot: Option<u8> = None;

        loop {
            #[cfg(feature = "sensor-node")]
            drain_sensor_alarm_events(
                &mut latest_sensor_sample,
                &mut sensor_alarm_active,
                &mut pending_ack,
            );

            let local_time_ms = elapsed_ms(boot);
            let gateway_time_ms = node.sync().gateway_time_ms(local_time_ms);
            let slot = node.schedule().slot_at(gateway_time_ms);

            let follow_schedule = node.is_synced()
                && (node.role() == NodeRole::Gateway || node.phase() == NetworkPhase::Joined);

            enum CoreWake {
                Frame(Frame),
                #[cfg(feature = "sensor-node")]
                SensorAlarm(AlarmTransition),
                Timer,
            }

            let wait_ms = if follow_schedule {
                tx_window_delay_ms(&node, gateway_time_ms, slot, last_tx_slot)
            } else if node.phase() == NetworkPhase::Searching
                && matches!(ACTIVE_ROLE, NodeRole::Relay | NodeRole::Sensor)
            {
                (last_hello_ms + HELLO_RETRY_INTERVAL_MS).saturating_sub(local_time_ms)
            } else {
                HELLO_RETRY_INTERVAL_MS
            };

            #[cfg(feature = "sensor-node")]
            let wake = if wait_ms == 0 {
                drain_sensor_alarm_events(
                    &mut latest_sensor_sample,
                    &mut sensor_alarm_active,
                    &mut pending_ack,
                );
                CoreWake::Timer
            } else {
                match select3(
                    RX_FRAMES.receive(),
                    ALARM_EVENTS.receive(),
                    Timer::after(Duration::from_millis(wait_ms)),
                )
                .await
                {
                    Either3::First(frame) => CoreWake::Frame(frame),
                    Either3::Second(transition) => CoreWake::SensorAlarm(transition),
                    Either3::Third(()) => CoreWake::Timer,
                }
            };

            #[cfg(not(feature = "sensor-node"))]
            let wake = if wait_ms == 0 {
                CoreWake::Timer
            } else {
                match select(
                    RX_FRAMES.receive(),
                    Timer::after(Duration::from_millis(wait_ms)),
                )
                .await
                {
                    Either::First(frame) => CoreWake::Frame(frame),
                    Either::Second(()) => CoreWake::Timer,
                }
            };

            match wake {
                CoreWake::Frame(frame) => {
                    handle_received_frame(
                        &mut node,
                        &mut lora_tx,
                        &frame,
                        elapsed_ms(boot),
                        &mut gateway_alarm_active,
                        &mut pending_ack,
                        &mut relay_forward,
                        &mut pending_sync_forward,
                        &mut pending_alarm_forward,
                        &mut gateway_stats,
                        &mut relay_heartbeat,
                        &mut gateway_heartbeat,
                    )
                    .await?;
                    continue;
                }
                #[cfg(feature = "sensor-node")]
                CoreWake::SensorAlarm(transition) => {
                    handle_sensor_alarm_transition(
                        transition,
                        &mut latest_sensor_sample,
                        &mut sensor_alarm_active,
                        &mut pending_ack,
                    );
                    continue;
                }
                CoreWake::Timer => {}
            }

            if !follow_schedule {
                let local_time_ms = elapsed_ms(boot);
                if node.phase() == NetworkPhase::Searching
                    && matches!(ACTIVE_ROLE, NodeRole::Relay | NodeRole::Sensor)
                    && local_time_ms.saturating_sub(last_hello_ms) >= HELLO_RETRY_INTERVAL_MS
                {
                    let frame = node.make_hello(local_time_ms)?;
                    let bytes = lora_tx.send_frame(&frame).await?;
                    last_hello_ms = local_time_ms;
                    println!(
                        "{} searching: retry {} seq={} bytes={}",
                        ACTIVE_ROLE, frame.frame_type, frame.seq, bytes
                    );
                }
                continue;
            }

            let local_time_ms = elapsed_ms(boot);
            let gateway_time_ms = node.sync().gateway_time_ms(local_time_ms);
            if !node.schedule().is_active_window(gateway_time_ms) {
                continue;
            }
            let slot = node.schedule().slot_at(gateway_time_ms);
            if last_tx_slot == Some(slot) {
                continue;
            }
            last_tx_slot = Some(slot);

            {
                match ACTIVE_ROLE {
                    NodeRole::Gateway if slot == node.schedule().sync_slot => {
                        let frame = node.make_sync(local_time_ms)?;
                        let bytes = lora_tx.send_frame(&frame).await?;
                        println!();
                        println!();
                        println!();
                        println!("====================superframe=======================");
                        println!(
                            "tx {} frame_seq={} sync_seq={} schedule_v={} active={}ms guard_before={}ms gateway_time={} slot={} bytes={}",
                            frame.frame_type,
                            frame.seq,
                            node.sync_seq(),
                            node.schedule().schedule_version,
                            node.schedule().active_ms,
                            node.schedule().guard_before_ms,
                            frame.gateway_time_ms,
                            slot,
                            bytes
                        );
                        gateway_stats.mark_sync_sent(node.sync_seq());
                        demo_log::print_gateway_stats(&gateway_stats, frame.gateway_time_ms);
                        let status = gateway_heartbeat.status(frame.gateway_time_ms, node.schedule());
                        demo_log::print_gateway_join_status(&status, frame.gateway_time_ms);

                        #[cfg(feature = "gateway-node")]
                        {
                            if !gateway_alarm_active {
                                BUZZER_SIG.signal(BuzzerAction::Off);
                            }
                        }
                    }
                    NodeRole::Relay | NodeRole::Sensor
                        if node.phase() == NetworkPhase::Joined
                            && slot == node.schedule().alarm_retry_slot =>
                    {
                        if let Some((frame, attempt)) = pending_ack.next_retry(local_time_ms) {
                            let bytes = lora_tx.send_frame(&frame).await?;
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
                        if node.phase() == NetworkPhase::Joined
                            && slot == node.schedule().relay_control_slot =>
                    {
                        if let Some(sync_frame) = pending_sync_forward.take() {
                            let forwarded = node.make_forwarded(
                                &sync_frame,
                                esp32c3_rust::role::BROADCAST_ID,
                                local_time_ms,
                            )?;
                            let bytes = lora_tx.send_frame(&forwarded).await?;
                            println!(
                                "relay control slot: forward SYNC sync_seq={} offset_ms={} drift={}ms bytes={}",
                                node.sync().last_sync_seq,
                                node.sync().offset_ms,
                                node.sync().offset_delta_ms,
                                bytes
                            );
                        }
                    }
                    NodeRole::Relay
                        if node.phase() == NetworkPhase::Joined
                            && slot == node.schedule().relay_heartbeat_slot =>
                    {
                        let heartbeat_slot = node.schedule().relay_heartbeat_slot;
                        let presence_mask =
                            relay_heartbeat.presence_mask(gateway_time_ms, node.schedule());
                        transmit_heartbeat(
                            &mut lora_tx,
                            &mut node,
                            local_time_ms,
                            heartbeat_slot,
                            presence_mask,
                        )
                        .await?;
                    }
                    NodeRole::Sensor
                        if node.phase() == NetworkPhase::Joined
                            && slot == node.schedule().sensor_heartbeat_slot =>
                    {
                        let heartbeat_slot = node.schedule().sensor_heartbeat_slot;
                        let presence_mask = protocol::heartbeat_presence_for_role(node.role());
                        transmit_heartbeat(
                            &mut lora_tx,
                            &mut node,
                            local_time_ms,
                            heartbeat_slot,
                            presence_mask,
                        )
                        .await?;
                    }
                    #[cfg(feature = "sensor-node")]
                    NodeRole::Sensor
                        if node.phase() == NetworkPhase::Joined && slot == node.schedule().sensor_slot =>
                    {
                        take_latest_sensor_sample(&mut latest_sensor_sample);

                        let sample = latest_sensor_sample;
                        let alarm = sensor_alarm_active;
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
                        let bytes = if alarm {
                            lora_tx.send_frame(&frame).await?
                        } else {
                            send_best_effort(&mut lora_tx, &frame, BEST_EFFORT_TX_REPEATS).await?
                        };
                        pending_ack.remember(&frame, local_time_ms);
                        println!(
                            "tx {} seq={} parent={:?} temp={}.{:02}C humidity={}.{:02}% gateway_time={} bytes={}",
                            frame.frame_type,
                            frame.seq,
                            node.parent_id(),
                            sample.temp_centi_c / 100,
                            sample.temp_centi_c.unsigned_abs() % 100,
                            sample.humidity_centi_percent / 100,
                            sample.humidity_centi_percent % 100,
                            frame.gateway_time_ms,
                            bytes
                        );
                    }
                    NodeRole::Relay
                        if node.phase() == NetworkPhase::Joined && slot == node.schedule().relay_slot =>
                    {
                        if let Some(alarm_frame) = pending_alarm_forward.take() {
                            let forwarded =
                                node.make_forwarded(&alarm_frame, GATEWAY_ID, local_time_ms)?;
                            let origin_seq = origin_seq(&alarm_frame);
                            let bytes = lora_tx.send_frame(&forwarded).await?;
                            pending_ack.remember(&forwarded, local_time_ms);
                            println!(
                                "relay slot tx ALARM origin_seq={} relay_seq={} ack_required=true bytes={}",
                                origin_seq, forwarded.seq, bytes
                            );
                        }
                        let mut forwarded_count = 0;
                        while forwarded_count < MAX_FORWARD_PER_SLOT {
                            let Some(frame) = relay_forward.take() else {
                                break;
                            };
                            let forwarded = node.make_forwarded(&frame, GATEWAY_ID, local_time_ms)?;
                            let bytes =
                                send_best_effort(&mut lora_tx, &forwarded, BEST_EFFORT_TX_REPEATS)
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
                            node.parent_id(),
                            node.hop(),
                            node.sync().last_sync_seq,
                            node.sync().offset_ms,
                            node.sync().offset_delta_ms
                        );
                    }
                    NodeRole::Gateway if slot == node.schedule().alarm_retry_slot => {}
                    NodeRole::Relay if slot == node.schedule().alarm_retry_slot => {}
                    NodeRole::Sensor if slot == node.schedule().alarm_retry_slot => {}
                    NodeRole::Gateway if slot == node.schedule().quiet_slot => {}
                    NodeRole::Relay if slot == node.schedule().quiet_slot => {}
                    NodeRole::Sensor if slot == node.schedule().quiet_slot => {}
                    _ => {}
                }
            }
        }

        #[allow(unreachable_code)]
        Ok::<(), AppError>(())
    }
    .await
    {
        panic_on_fatal_error(error);
    }
}

/// Initialise hardware, spawn every task, then return so the executor can
/// schedule them. After this function returns, `main()` enters its idle loop.
async fn run(spawner: Spawner) -> AppResult<()> {
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
        "lora uart: tx=GPIO{} rx=GPIO{} baud={} channel={} freq={}MHz air_rate={}bps tx_power={}dBm packet={}B net_id=0x{:04x}",
        pins.lora_uart_tx,
        pins.lora_uart_rx,
        lora.baudrate,
        lora.channel,
        lora.frequency_mhz,
        lora.air_rate_bps,
        lora.tx_power_dbm,
        lora.packet_bytes,
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

    // AT configuration ran in Blocking mode above; only now switch the UART to
    // interrupt-driven async and split it into independently owned halves.
    let (uart_rx, uart_tx) = uart.into_async().split();
    let lora_rx = LoraRx::new(uart_rx);
    let lora_tx = LoraTx::new(uart_tx);

    // The RX half lives entirely inside its own task from here on.
    let rx_token = lora_rx_task(lora_rx).expect("lora_rx_task pool exhausted");
    spawner.spawn(rx_token);

    #[cfg(feature = "gateway-node")]
    {
        let buzzer = Output::new(peripherals.GPIO10, Level::High, OutputConfig::default());
        let buzzer_token = buzzer_task(buzzer).expect("buzzer_task pool exhausted");
        spawner.spawn(buzzer_token);
    }

    #[cfg(not(feature = "gateway-node"))]
    let _ = peripherals.GPIO10;

    #[cfg(feature = "sensor-node")]
    {
        let i2c = I2c::new(
            peripherals.I2C0,
            I2cConfig::default().with_frequency(Rate::from_khz(400)),
        )?
        .with_sda(peripherals.GPIO5)
        .with_scl(peripherals.GPIO4);
        let sht40 = Sht40::new(i2c, Sht40Config::DEFAULT);
        let sensor_token = sensor_task(sht40).expect("sensor_task pool exhausted");
        spawner.spawn(sensor_token);
    }

    // Spawn the core protocol task — it owns all node state and runs forever.
    let core_token = core_task(
        lora_tx,
        DemoNode::new(ACTIVE_ROLE),
        false,
        #[cfg(feature = "sensor-node")]
        EnvironmentSample::normal(),
        #[cfg(feature = "sensor-node")]
        false,
        PendingAck::new(),
        RelayForwardBuffer::new(),
        GatewayStats::new(),
        RelayHeartbeatState::new(),
        GatewayHeartbeatState::new(),
        Instant::now(),
    )
    .expect("core_task pool exhausted");
    spawner.spawn(core_token);

    Ok(())
}

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

/// Milliseconds the core should wait before the current slot's TX action, given
/// the schedule, current gateway time, and the slot already serviced this cycle.
///
/// Returns 0 when the active window is open and this slot hasn't transmitted yet
/// (act now), the time-to-window-start while still in the leading guard band, and
/// the time-to-next-slot once this slot is done or its window has passed. This
/// replaces the old blocking `enter_tx_window` wait so the core can instead
/// `select` against incoming frames.
fn tx_window_delay_ms(
    node: &DemoNode,
    gateway_time_ms: u64,
    slot: u8,
    last_tx_slot: Option<u8>,
) -> u64 {
    if last_tx_slot == Some(slot) {
        return node.schedule().next_slot_delay_ms(gateway_time_ms) as u64;
    }
    let elapsed = node.schedule().slot_elapsed_ms(gateway_time_ms);
    if elapsed < node.schedule().guard_before_ms {
        (node.schedule().guard_before_ms - elapsed) as u64
    } else if node.schedule().is_active_window(gateway_time_ms) {
        0
    } else {
        node.schedule().next_slot_delay_ms(gateway_time_ms) as u64
    }
}

/// Send a best-effort frame `repeats` times with a small inter-repeat gap.
/// Receivers de-duplicate on (src_id, seq), so the repeats add redundancy
/// without delivering duplicates to the application.
async fn send_best_effort(
    lora_tx: &mut LoraTx<'_>,
    frame: &Frame,
    repeats: u8,
) -> AppResult<usize> {
    let mut bytes = 0;
    for attempt in 0..repeats.max(1) {
        if attempt > 0 {
            Timer::after(Duration::from_millis(BEST_EFFORT_REPEAT_GAP_MS)).await;
        }
        bytes = lora_tx.send_frame(frame).await?;
    }
    Ok(bytes)
}

async fn transmit_heartbeat(
    lora_tx: &mut LoraTx<'_>,
    node: &mut DemoNode,
    local_time_ms: u64,
    heartbeat_slot: u8,
    presence_mask: u8,
) -> AppResult<()> {
    let frame = node.make_heartbeat_with_presence(local_time_ms, presence_mask)?;
    let bytes = lora_tx.send_frame(&frame).await?;
    println!(
        "{} tx HEARTBEAT seq={} parent={:?} data_slot={} heartbeat_slot={} sync_seq={} presence=0b{:08b} offset_ms={} drift={}ms bytes={}",
        ACTIVE_ROLE,
        frame.seq,
        node.parent_id(),
        node.slot_id(),
        heartbeat_slot,
        node.sync().last_sync_seq,
        presence_mask,
        node.sync().offset_ms,
        node.sync().offset_delta_ms,
        bytes
    );
    Ok(())
}

#[cfg(feature = "sensor-node")]
fn take_latest_sensor_sample(latest_sample: &mut EnvironmentSample) {
    if let Some(sample) = SENSOR_LATEST.try_take() {
        *latest_sample = sample;
    }
}

#[cfg(feature = "sensor-node")]
fn drain_sensor_alarm_events(
    latest_sample: &mut EnvironmentSample,
    alarm_active: &mut bool,
    pending_ack: &mut PendingAck,
) {
    take_latest_sensor_sample(latest_sample);
    while let Ok(transition) = ALARM_EVENTS.try_receive() {
        handle_sensor_alarm_transition(transition, latest_sample, alarm_active, pending_ack);
    }
}

#[cfg(feature = "sensor-node")]
fn handle_sensor_alarm_transition(
    transition: AlarmTransition,
    latest_sample: &mut EnvironmentSample,
    alarm_active: &mut bool,
    pending_ack: &mut PendingAck,
) {
    take_latest_sensor_sample(latest_sample);
    let sample = *latest_sample;

    match transition {
        AlarmTransition::Raised => {
            *alarm_active = true;
            println!(
                "sensor alarm raised: temp={}.{:02}C humidity={}.{:02}% thresholds={}cC/{}c%",
                sample.temp_centi_c / 100,
                sample.temp_centi_c.unsigned_abs() % 100,
                sample.humidity_centi_percent / 100,
                sample.humidity_centi_percent % 100,
                Sht40Config::DEFAULT.temp_alarm_centi_c,
                Sht40Config::DEFAULT.humidity_alarm_centi_percent
            );
        }
        AlarmTransition::Cleared => {
            *alarm_active = false;
            if pending_ack.cancel_if_matches(FrameType::Alarm) {
                println!("sensor alarm cleared locally: cancel pending ALARM retry");
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
}

async fn handle_received_frame(
    node: &mut DemoNode,
    lora_tx: &mut LoraTx<'_>,
    frame: &Frame,
    local_time_ms: u64,
    gateway_alarm_active: &mut bool,
    pending_ack: &mut PendingAck,
    relay_forward: &mut RelayForwardBuffer,
    pending_sync_forward: &mut Option<Frame>,
    pending_alarm_forward: &mut Option<Frame>,
    gateway_stats: &mut GatewayStats,
    relay_heartbeat: &mut RelayHeartbeatState,
    gateway_heartbeat: &mut GatewayHeartbeatState,
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
            node.schedule().slot_at(frame.gateway_time_ms)
        );
        return Ok(());
    }

    let action = node.apply_frame(frame, local_time_ms);

    match (node.role(), frame.frame_type, action) {
        (NodeRole::Gateway, FrameType::Hello, _) if frame.node_role == NodeRole::Relay => {
            let ack = node.make_join_ack(frame.src_id, frame.node_role, local_time_ms)?;
            let bytes = lora_tx.send_frame(&ack).await?;
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
                let _bytes = lora_tx.send_frame(&ack).await?;
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
                    BUZZER_SIG.signal(BuzzerAction::On);
                    buzzer_action = BuzzerAction::On;
                } else if frame.frame_type == FrameType::Data && *gateway_alarm_active {
                    *gateway_alarm_active = false;
                    BUZZER_SIG.signal(BuzzerAction::Off);
                    buzzer_action = BuzzerAction::Off;
                }
            }

            let origin_seq_gap =
                gateway_stats.record_rx_data(frame.frame_type, origin_seq, ack_seq.is_some());
            if frame.node_role == NodeRole::Relay && origin_id == NodeRole::Sensor.node_id() {
                let now_ms = node.gateway_time_ms(local_time_ms);
                let _ = gateway_heartbeat.record_relayed_sensor_frame(now_ms, node.schedule());
            }
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
                presence_mask,
            },
        ) => {
            demo_log::print_gateway_heartbeat(frame, slot_id, sync_seq, presence_mask);
            if frame.node_role == NodeRole::Relay {
                let now_ms = node.gateway_time_ms(local_time_ms);
                let _ = gateway_heartbeat.record_relay_heartbeat(
                    presence_mask,
                    now_ms,
                    node.schedule(),
                );
            }
            let _ = hop;
        }
        (NodeRole::Relay, FrameType::Hello, _) if frame.node_role == NodeRole::Sensor => {
            let ack = node.make_join_ack(frame.src_id, frame.node_role, local_time_ms)?;
            let ack_bytes = lora_tx.send_frame(&ack).await?;
            println!(
                "rx sensor HELLO from={} -> tx JOIN_ACK seq={} bytes={}",
                frame.src_id, ack.seq, ack_bytes
            );

            // Forward the HELLO to the gateway so it can log the topology
            let mut notify = frame.clone();
            notify.dst_id = GATEWAY_ID;
            notify.hop = node.hop();
            let notify_bytes = lora_tx.send_frame(&notify).await?;
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
                presence_mask,
            },
        ) => {
            if frame.node_role == NodeRole::Sensor
                && protocol::heartbeat_presence_contains(presence_mask, NodeRole::Sensor)
            {
                relay_heartbeat.record_sensor_seen(node.gateway_time_ms(local_time_ms));
            }
            println!(
                "rx HEARTBEAT from={} reported_data_slot={} hop={} sync_seq={} presence=0b{:08b}",
                frame.src_id, slot_id, hop, sync_seq, presence_mask
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
                frame.frame_type,
                sync_seq,
                offset_ms,
                node.sync().offset_delta_ms
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
            if frame.node_role == NodeRole::Sensor && origin_id == NodeRole::Sensor.node_id() {
                relay_heartbeat.record_sensor_seen(node.gateway_time_ms(local_time_ms));
            }
            if alarm {
                // First ALARM hop (sensor -> relay): ACK the sensor immediately
                // so it stops retrying this hop, then buffer the ALARM for the
                // relay slot rather than forwarding inline. The second hop
                // (relay -> gateway) and its retries are scheduled in slot 3/4.
                let ack =
                    node.make_ack(frame.src_id, frame.seq, frame.frame_type, local_time_ms)?;
                let ack_bytes = lora_tx.send_frame(&ack).await?;
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
                frame.frame_type,
                sync_seq,
                offset_ms,
                node.sync().offset_delta_ms
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

    /// Next retry frame and attempt number, if we've waited long enough
    /// and haven't exceeded the maximum attempts.
    fn next_retry(&mut self, now_ms: u64) -> Option<(Frame, u8)> {
        let frame = self.frame.as_ref()?;
        if self.attempts >= Self::MAX_ATTEMPTS {
            return None;
        }
        if now_ms.saturating_sub(self.last_sent_ms) < Self::RETRY_DELAY_MS {
            return None;
        }
        self.attempts += 1;
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
        Some((frame.seq, frame.frame_type))
    }

    fn clear_if_matches(&mut self, seq: u16, frame_type: FrameType) -> bool {
        if self
            .frame
            .as_ref()
            .is_some_and(|f| f.seq == seq && f.frame_type == frame_type)
        {
            self.frame = None;
            true
        } else {
            false
        }
    }

    #[cfg(feature = "sensor-node")]
    fn cancel_if_matches(&mut self, frame_type: FrameType) -> bool {
        if self
            .frame
            .as_ref()
            .is_some_and(|f| f.frame_type == frame_type)
        {
            self.frame = None;
            true
        } else {
            false
        }
    }
}

fn is_ack_required(frame_type: FrameType) -> bool {
    // Only ALARM uses hop-by-hop ACK with bounded (up to MAX_ATTEMPTS) retry.
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

fn elapsed_ms(boot: Instant) -> u64 {
    boot.elapsed().as_millis()
}

fn panic_on_fatal_error(error: AppError) -> ! {
    log::error!("fatal application error: {}", error);
    println!("FATAL application error: {}", error);
    panic!("fatal application error: {}", error);
}
