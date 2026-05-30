//! Structured serial logging helpers for the gateway demo output.
//!
//! The formatting is kept in one place so protocol handling can report compact
//! events without carrying presentation details.

use crate::protocol::{Frame, FrameType};
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
    pub last_sync_seq: u16,
    pub offset_ms: i64,
    pub offset_delta_ms: i64,
    pub last_report_ms: u64,
}

impl GatewayStats {
    pub const REPORT_INTERVAL_MS: u64 = 30_000;

    pub const fn new() -> Self {
        Self {
            rx_data: 0,
            rx_alarm: 0,
            tx_ack: 0,
            slot_violations: 0,
            origin_seq_gap_total: 0,
            last_origin_seq: None,
            last_sync_seq: 0,
            offset_ms: 0,
            offset_delta_ms: 0,
            last_report_ms: 0,
        }
    }

    /// Count a frame dropped for transmitting outside its TDMA slot.
    pub fn record_slot_violation(&mut self) {
        self.slot_violations = self.slot_violations.saturating_add(1);
    }

    pub fn update_sync(&mut self, last_sync_seq: u16, offset_ms: i64, offset_delta_ms: i64) {
        self.last_sync_seq = last_sync_seq;
        self.offset_ms = offset_ms;
        self.offset_delta_ms = offset_delta_ms;
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

    pub fn should_report(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.last_report_ms) >= Self::REPORT_INTERVAL_MS
    }

    pub fn mark_reported(&mut self, now_ms: u64) {
        self.last_report_ms = now_ms;
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

pub fn print_gateway_heartbeat(frame: &Frame, slot_id: u8, sync_seq: u16) {
    println!(
        "[RX HEARTBEAT] from=node {} ({}) reported_slot={} hop={} sync_seq={} gateway_time={}",
        frame.src_id,
        frame.node_role,
        slot_id,
        frame.hop,
        sync_seq,
        GatewayTime(frame.gateway_time_ms)
    );
}

pub fn print_gateway_stats(stats: &GatewayStats, now_ms: u64) {
    println!();
    println!("{}", LINE_THIN);
    println!(
        "[NET STATS] uptime={} rx_data={} rx_alarm={} tx_ack={} slot_violations={} origin_seq_gap_total={} last_sync={} offset={}ms drift={}ms",
        GatewayTime(now_ms),
        stats.rx_data,
        stats.rx_alarm,
        stats.tx_ack,
        stats.slot_violations,
        stats.origin_seq_gap_total,
        stats.last_sync_seq,
        stats.offset_ms,
        stats.offset_delta_ms
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
