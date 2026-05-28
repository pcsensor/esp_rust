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
    demo::{DemoNode, EnvironmentSample, FrameAction, NetworkPhase},
    hardware::{LoraModuleConfigPlan, LoraUartConfig, PinConfig, Sht40Config},
    protocol::{Frame, FrameType},
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
    let mut pending_ack = PendingAck::new();
    let mut relay_forward = RelayForwardBuffer::new();
    let boot = Instant::now();

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

        // Drain all UART bytes to avoid hardware FIFO overflow
        if let Err(error) = lora_transport.drain_to_decoder() {
            log::warn!("LoRa RX drain failed: {}", error);
        }

        for _ in 0..MAX_RX_FRAMES_PER_LOOP {
            match lora_transport.next_decoded_frame() {
                Ok(Some(frame)) => {
                    handle_received_frame(
                        &mut node,
                        &mut lora_transport,
                        &frame,
                        local_time_ms,
                        #[cfg(feature = "gateway-node")]
                        &mut buzzer,
                        &mut gateway_alarm_active,
                        &mut pending_ack,
                        &mut relay_forward,
                    )?;
                }
                Ok(None) => break,
                Err(error) => {
                    log::warn!("LoRa RX decode error: {}", error);
                    break;
                }
            }
        }

        match ACTIVE_ROLE {
            NodeRole::Relay | NodeRole::Sensor
                if node.phase == NetworkPhase::Searching
                    && slot == node.schedule.maintenance_slot =>
            {
                let frame = node.make_hello(local_time_ms)?;
                let bytes = lora_transport.send_frame(&frame)?;
                println!(
                    "{} searching: retry {} seq={} bytes={}",
                    ACTIVE_ROLE, frame.frame_type, frame.seq, bytes
                );
            }
            NodeRole::Gateway if slot == node.schedule.sync_slot => {
                let frame = node.make_sync(local_time_ms)?;
                let bytes = lora_transport.send_frame(&frame)?;
                println!(
                    "tx {} seq={} gateway_time={} slot={} bytes={}",
                    frame.frame_type, frame.seq, frame.gateway_time_ms, slot, bytes
                );

                #[cfg(feature = "gateway-node")]
                {
                    if !gateway_alarm_active {
                        buzzer.set_high();
                    }
                }
            }
            NodeRole::Relay | NodeRole::Sensor
                if node.phase == NetworkPhase::Joined && slot == node.schedule.maintenance_slot =>
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
                } else if !pending_ack.has_pending() {
                    let frame = node.make_heartbeat(local_time_ms)?;
                    let bytes = lora_transport.send_frame(&frame)?;
                    pending_ack.remember(&frame, local_time_ms);
                    println!(
                        "{} tx HEARTBEAT seq={} parent={:?} slot={} bytes={}",
                        ACTIVE_ROLE, frame.seq, node.parent_id, node.slot_id, bytes
                    );
                }
            }
            NodeRole::Sensor
                if node.phase == NetworkPhase::Joined && slot == node.schedule.sensor_slot =>
            {
                #[cfg(feature = "sensor-node")]
                let sample = read_environment_sample(&mut sht40).await;

                #[cfg(not(feature = "sensor-node"))]
                let sample = read_environment_sample().await;

                let alarm = sample.is_alarm_with(
                    Sht40Config::DEFAULT.temp_alarm_centi_c,
                    Sht40Config::DEFAULT.humidity_alarm_centi_percent,
                );
                // If alarm has cleared, stop retrying the old ALARM
                if !alarm && pending_ack.has_pending() {
                    pending_ack.force_clear();
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
                let bytes = lora_transport.send_frame(&frame)?;
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
                if let Some(frame) = relay_forward.take() {
                    let forwarded = node.make_forwarded(&frame, GATEWAY_ID, local_time_ms)?;
                    // Dual-send for redundancy: send, pause, re-encode, send again.
                    // Same seq guarantees the receiver can deduplicate.
                    let bytes = lora_transport.send_frame(&forwarded)?;
                    Timer::after(Duration::from_millis(150)).await;
                    let bytes2 = lora_transport.send_frame(&forwarded)?;
                    pending_ack.remember(&forwarded, local_time_ms);
                    println!(
                        "relay slot tx buffered {} sensor_seq={} relay_seq={} bytes={}+{}",
                        frame.frame_type, frame.seq, forwarded.seq, bytes, bytes2
                    );
                }
                println!(
                    "relay slot active: buffered_data={} pending_ack={} parent={:?} hop={} sync_seq={} offset_ms={}",
                    relay_forward.has_pending(),
                    pending_ack.has_pending(),
                    node.parent_id,
                    node.hop,
                    node.sync.last_sync_seq,
                    node.sync.offset_ms
                );
            }
            // Slot 4 status lines temporarily disabled — uncomment for demo
            NodeRole::Gateway if slot == node.schedule.alarm_slot => {}
            NodeRole::Relay if slot == node.schedule.alarm_slot => {}
            NodeRole::Sensor if slot == node.schedule.alarm_slot => {}
            _ => {}
        }

        Timer::after(Duration::from_millis(node.schedule.slot_ms as u64)).await;
    }
}

const MAX_RX_FRAMES_PER_LOOP: usize = 4;

fn handle_received_frame(
    node: &mut DemoNode,
    lora_transport: &mut LoraUartTransport<'_>,
    frame: &Frame,
    local_time_ms: u64,
    #[cfg(feature = "gateway-node")] buzzer: &mut Output<'_>,
    gateway_alarm_active: &mut bool,
    pending_ack: &mut PendingAck,
    relay_forward: &mut RelayForwardBuffer,
) -> AppResult<()> {
    #[cfg(not(feature = "gateway-node"))]
    let _ = gateway_alarm_active;

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
                temp_centi_c,
                humidity_centi_percent,
                alarm,
            },
        ) => {
            println!(
                "rx {} via={} temp={}.{:02}C humidity={}.{:02}% alarm={} gateway_time={}",
                frame.frame_type,
                frame.src_id,
                temp_centi_c / 100,
                temp_centi_c.unsigned_abs() % 100,
                humidity_centi_percent / 100,
                humidity_centi_percent % 100,
                alarm,
                frame.gateway_time_ms
            );
            // Only ALARM gets ACK for guaranteed delivery
            if alarm {
                let ack =
                    node.make_ack(frame.src_id, frame.seq, frame.frame_type, local_time_ms)?;
                let bytes = lora_transport.send_frame(&ack)?;
                println!(
                    "tx ACK seq={} acked_seq={} bytes={}",
                    ack.seq, frame.seq, bytes
                );
            }

            #[cfg(feature = "gateway-node")]
            {
                if alarm {
                    *gateway_alarm_active = true;
                    buzzer.set_low();
                    println!("alarm active: buzzer on");
                } else if frame.frame_type == FrameType::Data && *gateway_alarm_active {
                    *gateway_alarm_active = false;
                    buzzer.set_high();
                    println!("alarm cleared: buzzer off");
                }
            }
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
            println!(
                "rx HEARTBEAT from={} role={} slot={} hop={} sync_seq={}",
                frame.src_id, frame.node_role, slot_id, hop, sync_seq
            );
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
                "rx HEARTBEAT from={} slot={} hop={} sync_seq={}",
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
            let forwarded =
                node.make_forwarded(frame, esp32c3_rust::role::BROADCAST_ID, local_time_ms)?;
            let bytes = lora_transport.send_frame(&forwarded)?;
            println!(
                "rx {} sync_seq={} offset_ms={} -> forward bytes={}",
                frame.frame_type, sync_seq, offset_ms, bytes
            );
        }
        (
            NodeRole::Relay,
            FrameType::Data | FrameType::Alarm,
            FrameAction::Data {
                temp_centi_c,
                humidity_centi_percent,
                alarm,
            },
        ) => {
            if alarm {
                let ack =
                    node.make_ack(frame.src_id, frame.seq, frame.frame_type, local_time_ms)?;
                let ack_bytes = lora_transport.send_frame(&ack)?;
                let forwarded = node.make_forwarded(frame, GATEWAY_ID, local_time_ms)?;
                let forward_bytes = lora_transport.send_frame(&forwarded)?;
                pending_ack.remember(&forwarded, local_time_ms);
                println!(
                    "rx {} from sensor={} temp={}.{:02}C humidity={}.{:02}% alarm=true -> ack_bytes={} immediate_forward_bytes={}",
                    frame.frame_type,
                    frame.src_id,
                    temp_centi_c / 100,
                    temp_centi_c.unsigned_abs() % 100,
                    humidity_centi_percent / 100,
                    humidity_centi_percent % 100,
                    ack_bytes,
                    forward_bytes
                );
            } else {
                relay_forward.remember(frame);
                println!(
                    "rx DATA from sensor={} temp={}.{:02}C humidity={}.{:02}% -> buffered_for_relay_slot",
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
                "rx {}: sync_seq={} offset_ms={}",
                frame.frame_type, sync_seq, offset_ms
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
struct RelayForwardBuffer {
    frame: Option<Frame>,
}

impl RelayForwardBuffer {
    const fn new() -> Self {
        Self { frame: None }
    }

    fn has_pending(&self) -> bool {
        self.frame.is_some()
    }

    fn remember(&mut self, frame: &Frame) {
        self.frame = Some(frame.clone());
    }

    fn take(&mut self) -> Option<Frame> {
        self.frame.take()
    }
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

    fn force_clear(&mut self) {
        self.frame = None;
        self.attempts = 0;
        self.last_sent_ms = 0;
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
