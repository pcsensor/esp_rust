#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TdmaSchedule {
    pub superframe_ms: u32,
    pub slot_ms: u32,
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
        superframe_ms: 8_000,
        slot_ms: 1_000,
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

    pub const fn next_slot_delay_ms(self, gateway_time_ms: u64) -> u32 {
        let elapsed = (gateway_time_ms % self.superframe_ms as u64) % self.slot_ms as u64;
        (self.slot_ms as u64 - elapsed) as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeSync {
    pub last_sync_seq: u16,
    pub offset_ms: i64,
    pub hop: u8,
}

impl TimeSync {
    pub const fn new(hop: u8) -> Self {
        Self {
            last_sync_seq: 0,
            offset_ms: 0,
            hop,
        }
    }

    pub fn apply_sync(&mut self, sync_seq: u16, gateway_time_ms: u64, local_time_ms: u64) {
        let measured_offset = gateway_time_ms as i64 - local_time_ms as i64;
        self.offset_ms = if self.last_sync_seq == 0 {
            measured_offset
        } else {
            ((self.offset_ms * 7) + measured_offset) / 8
        };
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
