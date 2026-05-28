use crate::{
    AppResult,
    protocol::{self, EncodedFrame, Frame, FrameType},
    role::{BROADCAST_ID, DEMO_ZONE_ID, NET_ID, NodeRole},
    tdma::{TdmaSchedule, TimeSync},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkPhase {
    Searching,
    Joined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameAction {
    Ignore,
    Joined {
        parent_id: u8,
        hop: u8,
        slot_id: u8,
    },
    Synced {
        sync_seq: u16,
        offset_ms: i64,
    },
    Data {
        temp_centi_c: i16,
        humidity_centi_percent: u16,
        alarm: bool,
    },
    Ack {
        acked_seq: u16,
        acked_type: FrameType,
    },
    Heartbeat {
        slot_id: u8,
        hop: u8,
        sync_seq: u16,
    },
}

#[derive(Debug, Clone)]
pub struct DemoNode {
    pub role: NodeRole,
    pub parent_id: Option<u8>,
    pub hop: u8,
    pub slot_id: u8,
    pub seq: u16,
    pub phase: NetworkPhase,
    pub sync: TimeSync,
    pub schedule: TdmaSchedule,
    last_data_seq: Option<(u8, u16)>,
}

impl DemoNode {
    pub const fn new(role: NodeRole) -> Self {
        Self {
            role,
            parent_id: role.parent_id(),
            hop: role.default_hop(),
            slot_id: role.default_slot(),
            seq: 0,
            phase: NetworkPhase::Searching,
            sync: TimeSync::new(role.default_hop()),
            schedule: TdmaSchedule::DEMO,
            last_data_seq: None,
        }
    }

    pub fn mark_joined(&mut self) {
        self.phase = NetworkPhase::Joined;
    }

    pub fn next_seq(&mut self) -> u16 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    pub fn make_hello(&mut self, local_time_ms: u64) -> AppResult<Frame> {
        let seq = self.next_seq();
        Ok(Frame {
            net_id: NET_ID,
            src_id: self.role.node_id(),
            dst_id: BROADCAST_ID,
            node_role: self.role,
            zone_id: DEMO_ZONE_ID,
            frame_type: FrameType::Hello,
            seq,
            hop: self.hop,
            gateway_time_ms: self.sync.gateway_time_ms(local_time_ms),
            payload: protocol::hello_payload(self.parent_id, self.slot_id)?,
        })
    }

    pub fn make_sync(&mut self, local_time_ms: u64) -> AppResult<Frame> {
        let seq = self.next_seq();
        Ok(Frame {
            net_id: NET_ID,
            src_id: self.role.node_id(),
            dst_id: BROADCAST_ID,
            node_role: self.role,
            zone_id: DEMO_ZONE_ID,
            frame_type: FrameType::Sync,
            seq,
            hop: self.hop,
            gateway_time_ms: self.sync.gateway_time_ms(local_time_ms),
            payload: protocol::sync_payload(
                seq,
                self.schedule.superframe_ms,
                self.schedule.slot_ms,
            )?,
        })
    }

    pub fn make_join_ack(
        &mut self,
        dst_id: u8,
        child_role: NodeRole,
        local_time_ms: u64,
    ) -> AppResult<Frame> {
        let seq = self.next_seq();
        let child_hop = self.hop.saturating_add(1);
        let child_slot = child_role.default_slot();
        Ok(Frame {
            net_id: NET_ID,
            src_id: self.role.node_id(),
            dst_id,
            node_role: self.role,
            zone_id: DEMO_ZONE_ID,
            frame_type: FrameType::JoinAck,
            seq,
            hop: self.hop,
            gateway_time_ms: self.sync.gateway_time_ms(local_time_ms),
            payload: protocol::join_ack_payload(self.role.node_id(), child_hop, child_slot)?,
        })
    }

    pub fn make_data(
        &mut self,
        local_time_ms: u64,
        temp_centi_c: i16,
        humidity_centi_percent: u16,
    ) -> AppResult<Frame> {
        let seq = self.next_seq();
        Ok(Frame {
            net_id: NET_ID,
            src_id: self.role.node_id(),
            dst_id: self.parent_id.unwrap_or(BROADCAST_ID),
            node_role: self.role,
            zone_id: DEMO_ZONE_ID,
            frame_type: FrameType::Data,
            seq,
            hop: self.hop,
            gateway_time_ms: self.sync.gateway_time_ms(local_time_ms),
            payload: protocol::data_payload(temp_centi_c, humidity_centi_percent)?,
        })
    }

    pub fn make_alarm(
        &mut self,
        local_time_ms: u64,
        temp_centi_c: i16,
        humidity_centi_percent: u16,
    ) -> AppResult<Frame> {
        let seq = self.next_seq();
        Ok(Frame {
            net_id: NET_ID,
            src_id: self.role.node_id(),
            dst_id: self.parent_id.unwrap_or(BROADCAST_ID),
            node_role: self.role,
            zone_id: DEMO_ZONE_ID,
            frame_type: FrameType::Alarm,
            seq,
            hop: self.hop,
            gateway_time_ms: self.sync.gateway_time_ms(local_time_ms),
            payload: protocol::data_payload(temp_centi_c, humidity_centi_percent)?,
        })
    }

    pub fn make_ack(
        &mut self,
        dst_id: u8,
        acked_seq: u16,
        acked_type: FrameType,
        local_time_ms: u64,
    ) -> AppResult<Frame> {
        let seq = self.next_seq();
        Ok(Frame {
            net_id: NET_ID,
            src_id: self.role.node_id(),
            dst_id,
            node_role: self.role,
            zone_id: DEMO_ZONE_ID,
            frame_type: FrameType::Ack,
            seq,
            hop: self.hop,
            gateway_time_ms: self.sync.gateway_time_ms(local_time_ms),
            payload: protocol::ack_payload(acked_seq, acked_type)?,
        })
    }

    pub fn make_heartbeat(&mut self, local_time_ms: u64) -> AppResult<Frame> {
        let seq = self.next_seq();
        Ok(Frame {
            net_id: NET_ID,
            src_id: self.role.node_id(),
            dst_id: self.parent_id.unwrap_or(BROADCAST_ID),
            node_role: self.role,
            zone_id: DEMO_ZONE_ID,
            frame_type: FrameType::Heartbeat,
            seq,
            hop: self.hop,
            gateway_time_ms: self.sync.gateway_time_ms(local_time_ms),
            payload: protocol::heartbeat_payload(self.slot_id, self.hop, self.sync.last_sync_seq)?,
        })
    }

    pub fn make_forwarded(
        &mut self,
        received: &Frame,
        dst_id: u8,
        local_time_ms: u64,
    ) -> AppResult<Frame> {
        let seq = self.next_seq();
        Ok(Frame {
            net_id: NET_ID,
            src_id: self.role.node_id(),
            dst_id,
            node_role: self.role,
            zone_id: DEMO_ZONE_ID,
            frame_type: received.frame_type,
            seq,
            hop: self.hop,
            gateway_time_ms: self.sync.gateway_time_ms(local_time_ms),
            payload: received.payload.clone(),
        })
    }

    pub fn apply_frame(&mut self, frame: &Frame, local_time_ms: u64) -> FrameAction {
        if frame.net_id != NET_ID {
            return FrameAction::Ignore;
        }
        if frame.dst_id != self.role.node_id() && frame.dst_id != BROADCAST_ID {
            return FrameAction::Ignore;
        }

        match frame.frame_type {
            FrameType::JoinAck => {
                if let Some((parent_id, hop, slot_id)) =
                    protocol::decode_join_ack_payload(&frame.payload)
                {
                    self.parent_id = Some(parent_id);
                    self.hop = hop;
                    self.slot_id = slot_id;
                    self.sync.hop = hop;
                    self.mark_joined();
                    FrameAction::Joined {
                        parent_id,
                        hop,
                        slot_id,
                    }
                } else {
                    FrameAction::Ignore
                }
            }
            FrameType::Sync | FrameType::Schedule => {
                // Gateway is the time authority — never sync to external SYNC
                if self.role == NodeRole::Gateway {
                    return FrameAction::Ignore;
                }
                if let Some((sync_seq, superframe_ms, slot_ms)) =
                    protocol::decode_sync_payload(&frame.payload)
                {
                    self.schedule.superframe_ms = superframe_ms;
                    self.schedule.slot_ms = slot_ms;
                    self.sync
                        .apply_sync(sync_seq, frame.gateway_time_ms, local_time_ms);
                    FrameAction::Synced {
                        sync_seq,
                        offset_ms: self.sync.offset_ms,
                    }
                } else {
                    FrameAction::Ignore
                }
            }
            FrameType::Data | FrameType::Alarm => {
                // Skip duplicate DATA/ALARM (dual-send redundancy)
                let key = (frame.src_id, frame.seq);
                if self.last_data_seq == Some(key) {
                    return FrameAction::Ignore;
                }
                self.last_data_seq = Some(key);
                if let Some((temp_centi_c, humidity_centi_percent)) =
                    protocol::decode_data_payload(&frame.payload)
                {
                    FrameAction::Data {
                        temp_centi_c,
                        humidity_centi_percent,
                        alarm: frame.frame_type == FrameType::Alarm,
                    }
                } else {
                    FrameAction::Ignore
                }
            }
            FrameType::Ack => {
                if let Some((acked_seq, acked_type)) = protocol::decode_ack_payload(&frame.payload)
                {
                    FrameAction::Ack {
                        acked_seq,
                        acked_type,
                    }
                } else {
                    FrameAction::Ignore
                }
            }
            FrameType::Heartbeat => {
                if let Some((slot_id, hop, sync_seq)) =
                    protocol::decode_heartbeat_payload(&frame.payload)
                {
                    FrameAction::Heartbeat {
                        slot_id,
                        hop,
                        sync_seq,
                    }
                } else {
                    FrameAction::Ignore
                }
            }
            _ => FrameAction::Ignore,
        }
    }

    pub fn encode_for_lora(frame: &Frame) -> AppResult<EncodedFrame> {
        Ok(frame.encode()?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentSample {
    pub temp_centi_c: i16,
    pub humidity_centi_percent: u16,
}

impl EnvironmentSample {
    pub const TEMP_ALARM_CENTI_C: i16 = 3_000;
    pub const HUMIDITY_ALARM_CENTI_PERCENT: u16 = 8_000;

    pub const fn normal() -> Self {
        Self {
            temp_centi_c: 2_480,
            humidity_centi_percent: 6_250,
        }
    }

    pub const fn is_alarm(self) -> bool {
        self.temp_centi_c >= Self::TEMP_ALARM_CENTI_C
            || self.humidity_centi_percent >= Self::HUMIDITY_ALARM_CENTI_PERCENT
    }

    pub const fn is_alarm_with(
        self,
        temp_alarm_centi_c: i16,
        humidity_alarm_centi_percent: u16,
    ) -> bool {
        self.temp_centi_c >= temp_alarm_centi_c
            || self.humidity_centi_percent >= humidity_alarm_centi_percent
    }
}
