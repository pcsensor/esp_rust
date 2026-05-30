//! Structured serial logging helpers for the gateway demo output.
//!
//! The formatting is kept in one place so protocol handling can report compact
//! events without carrying presentation details.

use crate::{
    demo::GatewayJoinStatus,
    protocol::{Frame, FrameType},
};
// On the embedded target `println!` comes from esp-println; on the host it comes
// from the std prelude, so the logging helpers compile in both environments.
#[cfg(target_os = "none")]
use esp_println::println;

const LINE_BOLD: &str = "==============================================================";
const LINE_THIN: &str = "--------------------------------------------------------------";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuzzerAction {
    Unchanged,
    On,
    Off,
}

impl BuzzerAction {
    const fn label(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::On => "ON",
            Self::Off => "OFF",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewayRxDataLog {
    pub frame_type: FrameType,
    pub gateway_time_ms: u64,
    pub origin_id: u8,
    pub origin_seq: u16,
    pub via_id: u8,
    pub relay_seq: u16,
    pub hop: u8,
    pub temp_centi_c: i16,
    pub humidity_centi_percent: u16,
    pub alarm: bool,
    pub ack_seq: Option<u16>,
    pub buzzer_action: BuzzerAction,
    pub origin_seq_gap: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewayStats {
    pub rx_data: u32,
    pub rx_alarm: u32,
    pub tx_ack: u32,
    pub slot_violations: u32,
    pub origin_seq_gap_total: u32,
    pub last_origin_seq: Option<u16>,
    pub tx_sync: u32,
    pub last_tx_sync_seq: u16,
}

impl GatewayStats {
    pub const fn new() -> Self {
        Self {
            rx_data: 0,
            rx_alarm: 0,
            tx_ack: 0,
            slot_violations: 0,
            origin_seq_gap_total: 0,
            last_origin_seq: None,
            tx_sync: 0,
            last_tx_sync_seq: 0,
        }
    }

    /// Count a frame dropped for transmitting outside its TDMA slot.
    pub fn record_slot_violation(&mut self) {
        self.slot_violations = self.slot_violations.saturating_add(1);
    }

    pub fn mark_sync_sent(&mut self, sync_seq: u16) {
        self.tx_sync = self.tx_sync.saturating_add(1);
        self.last_tx_sync_seq = sync_seq;
    }

    pub fn record_rx_data(
        &mut self,
        frame_type: FrameType,
        origin_seq: u16,
        ack_sent: bool,
    ) -> Option<u16> {
        match frame_type {
            FrameType::Data => self.rx_data = self.rx_data.saturating_add(1),
            FrameType::Alarm => self.rx_alarm = self.rx_alarm.saturating_add(1),
            _ => {}
        }
        if ack_sent {
            self.tx_ack = self.tx_ack.saturating_add(1);
        }

        let origin_seq_gap = self.last_origin_seq.and_then(|last_seq| {
            let delta = origin_seq.wrapping_sub(last_seq);
            // HEARTBEAT now uses an independent sequence space, so consecutive
            // DATA/ALARM origin sequences differ by exactly 1. Any larger delta
            // means that many periodic frames were lost in between.
            (delta > 1).then_some(delta - 1)
        });
        if let Some(gap) = origin_seq_gap {
            self.origin_seq_gap_total = self.origin_seq_gap_total.saturating_add(gap as u32);
        }
        self.last_origin_seq = Some(origin_seq);
        origin_seq_gap
    }

    pub const fn rx_total(&self) -> u32 {
        self.rx_data + self.rx_alarm
    }
}

pub fn print_gateway_rx_data(event: &GatewayRxDataLog) {
    println!();
    println!("{}", LINE_BOLD);
    println!(
        "[RX {}] gateway_time={}",
        event.frame_type,
        GatewayTime(event.gateway_time_ms)
    );
    println!("{}", LINE_THIN);
    println!(
        "source      : node {} (sensor), seq={}",
        event.origin_id, event.origin_seq
    );
    println!(
        "via         : node {} (relay),  seq={}",
        event.via_id, event.relay_seq
    );
    println!("hop         : {}", event.hop);
    println!("temperature : {}", CentiTemp(event.temp_centi_c));
    println!(
        "humidity    : {}",
        CentiPercent(event.humidity_centi_percent)
    );
    println!("alarm       : {}", if event.alarm { "YES" } else { "NO" });
    if let Some(gap) = event.origin_seq_gap {
        println!("link        : via relay, crc ok, origin_seq_gap={gap}");
    } else {
        println!("link        : via relay, crc ok");
    }

    match event.ack_seq {
        Some(ack_seq) => println!(
            "action      : tx ACK seq={}, buzzer {}",
            ack_seq,
            event.buzzer_action.label()
        ),
        None => println!("action      : buzzer {}", event.buzzer_action.label()),
    }
    println!("{}", LINE_BOLD);
}

pub fn print_gateway_heartbeat(frame: &Frame, slot_id: u8, sync_seq: u16, presence_mask: u8) {
    println!(
        "[RX HEARTBEAT] from=node {} ({}) data_slot={} hop={} sync_seq={} presence=0b{:08b} gateway_time={}",
        frame.src_id,
        frame.node_role,
        slot_id,
        frame.hop,
        sync_seq,
        presence_mask,
        GatewayTime(frame.gateway_time_ms)
    );
}

pub fn print_gateway_join_status(status: &GatewayJoinStatus, now_ms: u64) {
    println!(
        "[JOIN STATUS] gateway_time={} relay={} last_seen={} sensor={} via=relay last_seen={}",
        GatewayTime(now_ms),
        status.relay.label(),
        LastSeen(status.relay_last_seen_ms),
        status.sensor.label(),
        LastSeen(status.sensor_last_seen_ms)
    );
}

pub fn print_gateway_stats(stats: &GatewayStats, now_ms: u64) {
    println!("{}", LINE_THIN);
    println!("[NET STATS] gateway_time={}", GatewayTime(now_ms));
    println!(
        "sync       : tx_count={} last_tx_sync_seq={}",
        stats.tx_sync, stats.last_tx_sync_seq
    );
    println!(
        "traffic    : rx_total={} data={} alarm={} tx_ack={}",
        stats.rx_total(),
        stats.rx_data,
        stats.rx_alarm,
        stats.tx_ack
    );
    println!(
        "link       : slot_violations={} origin_seq_gap_total={} last_origin_seq={}",
        stats.slot_violations,
        stats.origin_seq_gap_total,
        OptionalSeq(stats.last_origin_seq)
    );
    println!("{}", LINE_THIN);
}

struct GatewayTime(u64);

impl core::fmt::Display for GatewayTime {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let ms = self.0;
        let seconds = ms / 1_000;
        let hours = (seconds / 3_600) % 24;
        let minutes = (seconds / 60) % 60;
        let secs = seconds % 60;
        write!(f, "{hours:02}:{minutes:02}:{secs:02}.{:03}", ms % 1_000)
    }
}

struct LastSeen(Option<u64>);

impl core::fmt::Display for LastSeen {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(ms) => write!(f, "{}", GatewayTime(ms)),
            None => f.write_str("never"),
        }
    }
}

struct OptionalSeq(Option<u16>);

impl core::fmt::Display for OptionalSeq {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(seq) => write!(f, "{seq}"),
            None => f.write_str("-"),
        }
    }
}

struct CentiTemp(i16);

impl core::fmt::Display for CentiTemp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        write!(f, "{sign}{}.{:02} C", abs / 100, abs % 100)
    }
}

struct CentiPercent(u16);

impl core::fmt::Display for CentiPercent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{:02} %", self.0 / 100, self.0 % 100)
    }
}
