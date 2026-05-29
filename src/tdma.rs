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
