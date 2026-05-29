use crate::protocol::FrameType;
use crate::role::NodeRole;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TdmaSchedule {
    pub schedule_version: u8,
    pub superframe_ms: u32,
    pub slot_ms: u32,
    pub guard_before_ms: u32,
    pub active_ms: u32,
    pub slot_count: u8,
    pub sync_slot: u8,
    pub relay_control_slot: u8,
    pub sensor_slot: u8,
    pub relay_slot: u8,
    pub alarm_retry_slot: u8,
    pub relay_heartbeat_slot: u8,
    pub sensor_heartbeat_slot: u8,
    pub quiet_slot: u8,
}

impl TdmaSchedule {
    pub const DEMO: Self = Self {
        schedule_version: 1,
        superframe_ms: 8_000,
        slot_ms: 1_000,
        guard_before_ms: 100,
        active_ms: 700,
        slot_count: 8,
        sync_slot: 0,
        relay_control_slot: 1,
        sensor_slot: 2,
        relay_slot: 3,
        alarm_retry_slot: 4,
        relay_heartbeat_slot: 5,
        sensor_heartbeat_slot: 6,
        quiet_slot: 7,
    };

    pub const fn slot_at(self, gateway_time_ms: u64) -> u8 {
        ((gateway_time_ms % self.superframe_ms as u64) / self.slot_ms as u64) as u8
    }

    pub const fn slot_elapsed_ms(self, gateway_time_ms: u64) -> u32 {
        ((gateway_time_ms % self.superframe_ms as u64) % self.slot_ms as u64) as u32
    }

    pub const fn active_end_ms(self) -> u32 {
        self.guard_before_ms + self.active_ms
    }

    pub const fn is_active_window(self, gateway_time_ms: u64) -> bool {
        let elapsed = self.slot_elapsed_ms(gateway_time_ms);
        elapsed >= self.guard_before_ms && elapsed < self.active_end_ms()
    }

    pub const fn next_slot_delay_ms(self, gateway_time_ms: u64) -> u32 {
        self.slot_ms - self.slot_elapsed_ms(gateway_time_ms)
    }

    /// Whether a periodic scheduled frame of `frame_type` from `role` is allowed
    /// in `slot`. ALARM spans the owner's data slot and the retry slot because it
    /// is first sent / forwarded in the data slot and retransmitted in the retry
    /// slot. Frame types not listed (control/bootstrap) are not slot-constrained.
    pub fn slot_owner_ok(self, role: NodeRole, frame_type: FrameType, slot: u8) -> bool {
        match (role, frame_type) {
            (NodeRole::Sensor, FrameType::Data | FrameType::Alarm) => {
                slot == self.sensor_slot || slot == self.alarm_retry_slot
            }
            (NodeRole::Relay, FrameType::Data | FrameType::Alarm) => {
                slot == self.relay_slot || slot == self.alarm_retry_slot
            }
            (NodeRole::Relay, FrameType::Heartbeat) => slot == self.relay_heartbeat_slot,
            (NodeRole::Sensor, FrameType::Heartbeat) => slot == self.sensor_heartbeat_slot,
            _ => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeSync {
    pub last_sync_seq: u16,
    pub offset_ms: i64,
    pub last_measured_offset_ms: i64,
    pub offset_delta_ms: i64,
    pub hop: u8,
}

impl TimeSync {
    pub const fn new(hop: u8) -> Self {
        Self {
            last_sync_seq: 0,
            offset_ms: 0,
            last_measured_offset_ms: 0,
            offset_delta_ms: 0,
            hop,
        }
    }

    pub fn apply_sync(&mut self, sync_seq: u16, gateway_time_ms: u64, local_time_ms: u64) {
        let measured_offset = gateway_time_ms as i64 - local_time_ms as i64;
        self.offset_delta_ms = if self.last_sync_seq == 0 {
            0
        } else {
            measured_offset - self.last_measured_offset_ms
        };
        self.offset_ms = if self.last_sync_seq == 0 {
            measured_offset
        } else {
            ((self.offset_ms * 7) + measured_offset) / 8
        };
        self.last_measured_offset_ms = measured_offset;
        self.last_sync_seq = sync_seq;
    }

    pub const fn gateway_time_ms(self, local_time_ms: u64) -> u64 {
        if self.offset_ms >= 0 {
            local_time_ms.saturating_add(self.offset_ms as u64)
        } else {
            local_time_ms.saturating_sub((-self.offset_ms) as u64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: TdmaSchedule = TdmaSchedule::DEMO;

    #[test]
    fn slot_at_maps_and_wraps_across_superframe() {
        assert_eq!(S.slot_at(0), 0);
        assert_eq!(S.slot_at(999), 0);
        assert_eq!(S.slot_at(1_000), 1);
        assert_eq!(S.slot_at(2_500), 2);
        assert_eq!(S.slot_at(7_999), 7);
        // Wraps at the 8 s superframe boundary.
        assert_eq!(S.slot_at(8_000), 0);
        assert_eq!(S.slot_at(8_000 + 3_400), 3);
    }

    #[test]
    fn slot_elapsed_is_offset_within_slot() {
        assert_eq!(S.slot_elapsed_ms(0), 0);
        assert_eq!(S.slot_elapsed_ms(1_500), 500);
        assert_eq!(S.slot_elapsed_ms(8_000 + 250), 250);
    }

    #[test]
    fn active_window_excludes_guard_bands() {
        // active_end = guard_before(100) + active(700) = 800.
        assert_eq!(S.active_end_ms(), 800);
        // Pre-guard: not active.
        assert!(!S.is_active_window(0));
        assert!(!S.is_active_window(99));
        // Active window [100, 800).
        assert!(S.is_active_window(100));
        assert!(S.is_active_window(799));
        // Post-guard: not active.
        assert!(!S.is_active_window(800));
        assert!(!S.is_active_window(999));
        // Boundaries repeat every slot.
        assert!(!S.is_active_window(1_099));
        assert!(S.is_active_window(1_100));
    }

    #[test]
    fn slot_owner_accepts_only_the_assigned_slot() {
        use crate::protocol::FrameType;
        use crate::role::NodeRole;

        // Sensor DATA/ALARM is valid in the sensor slot and the retry slot.
        assert!(S.slot_owner_ok(NodeRole::Sensor, FrameType::Data, S.sensor_slot));
        assert!(S.slot_owner_ok(NodeRole::Sensor, FrameType::Alarm, S.sensor_slot));
        assert!(S.slot_owner_ok(NodeRole::Sensor, FrameType::Alarm, S.alarm_retry_slot));
        assert!(!S.slot_owner_ok(NodeRole::Sensor, FrameType::Data, S.relay_slot));

        // Relay DATA/ALARM is valid in the relay slot and the retry slot.
        assert!(S.slot_owner_ok(NodeRole::Relay, FrameType::Data, S.relay_slot));
        assert!(S.slot_owner_ok(NodeRole::Relay, FrameType::Alarm, S.alarm_retry_slot));
        assert!(!S.slot_owner_ok(NodeRole::Relay, FrameType::Alarm, S.sensor_slot));

        // HEARTBEAT is bound to each role's own heartbeat slot.
        assert!(S.slot_owner_ok(
            NodeRole::Relay,
            FrameType::Heartbeat,
            S.relay_heartbeat_slot
        ));
        assert!(S.slot_owner_ok(
            NodeRole::Sensor,
            FrameType::Heartbeat,
            S.sensor_heartbeat_slot
        ));
        assert!(!S.slot_owner_ok(
            NodeRole::Relay,
            FrameType::Heartbeat,
            S.sensor_heartbeat_slot
        ));

        // Control/bootstrap frame types are not slot-constrained.
        assert!(S.slot_owner_ok(NodeRole::Gateway, FrameType::Sync, S.quiet_slot));
        assert!(S.slot_owner_ok(NodeRole::Sensor, FrameType::JoinAck, S.quiet_slot));
    }

    #[test]
    fn next_slot_delay_counts_down_to_boundary() {
        assert_eq!(S.next_slot_delay_ms(0), 1_000);
        assert_eq!(S.next_slot_delay_ms(1_500), 500);
        assert_eq!(S.next_slot_delay_ms(7_999), 1);
    }

    #[test]
    fn time_sync_first_sample_sets_offset_without_drift() {
        let mut sync = TimeSync::new(0);
        sync.apply_sync(1, 1_000, 900);
        assert_eq!(sync.offset_ms, 100);
        assert_eq!(sync.offset_delta_ms, 0);
        assert_eq!(sync.last_measured_offset_ms, 100);
        assert_eq!(sync.last_sync_seq, 1);
    }

    #[test]
    fn time_sync_smooths_offset_and_reports_drift() {
        let mut sync = TimeSync::new(0);
        sync.apply_sync(1, 1_000, 900); // measured offset = 100
        sync.apply_sync(2, 2_000, 1_820); // measured offset = 180
        // EWMA: (100 * 7 + 180) / 8 = 110; drift = 180 - 100 = 80.
        assert_eq!(sync.offset_ms, 110);
        assert_eq!(sync.offset_delta_ms, 80);
        assert_eq!(sync.last_measured_offset_ms, 180);
    }

    #[test]
    fn gateway_time_applies_signed_offset() {
        let mut pos = TimeSync::new(0);
        pos.apply_sync(1, 1_000, 900); // offset +100
        assert_eq!(pos.gateway_time_ms(2_000), 2_100);

        let mut neg = TimeSync::new(0);
        neg.apply_sync(1, 500, 900); // offset -400
        assert_eq!(neg.gateway_time_ms(1_000), 600);
        // Saturates instead of underflowing.
        assert_eq!(neg.gateway_time_ms(100), 0);
    }
}
