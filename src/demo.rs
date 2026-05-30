//! Hardware-independent demo protocol state machine.
//!
//! This module owns frame construction, inbound frame application, join state,
//! data sequence accounting, and alarm latching without depending on ESP HAL.

use crate::{
    AppResult,
    protocol::{self, EncodedFrame, Frame, FrameType},
    role::{BROADCAST_ID, DEMO_ZONE_ID, NET_ID, NodeRole},
    tdma::{TdmaSchedule, TimeSync},
};

/// Join state for a demo node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkPhase {
    /// Node is still bootstrapping and may send HELLO outside TDMA timing.
    Searching,
    /// Node has accepted a JOIN_ACK and can participate in scheduled traffic
    /// once its clock is synced.
    Joined,
}

/// Result of applying a received frame to a `DemoNode`.
///
/// The action is deliberately data-only so the hardware-bound task can decide
/// whether to transmit, log, buffer, or ignore without duplicating protocol
/// parsing logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameAction {
    /// The frame was irrelevant, malformed, duplicated, or not actionable for
    /// this node.
    Ignore,
    /// A JOIN_ACK was accepted and local topology state was updated.
    Joined { parent_id: u8, hop: u8, slot_id: u8 },
    /// A SYNC/SCHEDULE frame was accepted and the local clock was updated.
    Synced { sync_seq: u16, offset_ms: i64 },
    /// A DATA or ALARM payload was accepted.
    Data {
        origin_id: u8,
        origin_seq: u16,
        temp_centi_c: i16,
        humidity_centi_percent: u16,
        alarm: bool,
    },
    /// An ACK payload was accepted.
    Ack {
        acked_seq: u16,
        acked_type: FrameType,
    },
    /// A HEARTBEAT payload was accepted.
    Heartbeat { slot_id: u8, hop: u8, sync_seq: u16 },
}

/// Hardware-independent protocol state for one demo node.
///
/// `DemoNode` owns role, topology, sequence counters, TDMA schedule, and clock
/// sync state. Callers build outbound frames with the `make_*` methods and feed
/// inbound frames through `apply_frame`.
#[derive(Debug, Clone)]
pub struct DemoNode {
    role: NodeRole,
    parent_id: Option<u8>,
    hop: u8,
    slot_id: u8,
    seq: u16,
    data_seq: u16,
    heartbeat_seq: u16,
    sync_seq: u16,
    phase: NetworkPhase,
    sync: TimeSync,
    schedule: TdmaSchedule,
    last_data_seq: Option<(u8, u16)>,
    last_heartbeat_seq: Option<(u8, u16)>,
}

impl DemoNode {
    pub const fn new(role: NodeRole) -> Self {
        Self {
            role,
            parent_id: role.parent_id(),
            hop: role.default_hop(),
            slot_id: role.default_slot(),
            seq: 0,
            data_seq: 0,
            heartbeat_seq: 0,
            sync_seq: 0,
            phase: NetworkPhase::Searching,
            sync: TimeSync::new(role.default_hop()),
            schedule: TdmaSchedule::DEMO,
            last_data_seq: None,
            last_heartbeat_seq: None,
        }
    }

    /// Static role selected for this node.
    pub const fn role(&self) -> NodeRole {
        self.role
    }

    /// Current parent node, if the node has a preferred parent.
    pub const fn parent_id(&self) -> Option<u8> {
        self.parent_id
    }

    /// Current routing hop count.
    pub const fn hop(&self) -> u8 {
        self.hop
    }

    /// TDMA data slot assigned to this node.
    pub const fn slot_id(&self) -> u8 {
        self.slot_id
    }

    /// Current join phase.
    pub const fn phase(&self) -> NetworkPhase {
        self.phase
    }

    /// Last sync sequence emitted or accepted by this node.
    pub const fn sync_seq(&self) -> u16 {
        self.sync_seq
    }

    /// Current TDMA schedule.
    pub const fn schedule(&self) -> TdmaSchedule {
        self.schedule
    }

    /// Current clock-sync state.
    pub const fn sync(&self) -> TimeSync {
        self.sync
    }

    /// Convert local monotonic time into the node's gateway time view.
    pub const fn gateway_time_ms(&self, local_time_ms: u64) -> u64 {
        self.sync.gateway_time_ms(local_time_ms)
    }

    pub fn mark_joined(&mut self) {
        self.phase = NetworkPhase::Joined;
    }

    /// A node may only trust its TDMA slot timing once it holds a valid clock.
    /// The gateway is the time authority, so it is always synced; every other
    /// role becomes synced after the first SYNC it applies (`last_sync_seq != 0`).
    pub fn is_synced(&self) -> bool {
        self.role == NodeRole::Gateway || self.sync.last_sync_seq != 0
    }

    /// TDMA discipline gate for reception: once this node has a trustworthy
    /// (synced) clock, periodic scheduled traffic (DATA/ALARM/HEARTBEAT) is only
    /// accepted from the role that owns the slot the sender claims to have
    /// transmitted in (derived from `frame.gateway_time_ms`). Control/bootstrap
    /// frames are reactive (not slot-bound) and are always accepted; while
    /// unsynced the local clock is untrustworthy, so everything is accepted.
    pub fn accepts_frame_slot(&self, frame: &Frame) -> bool {
        if !self.is_synced() {
            return true;
        }
        match frame.frame_type {
            FrameType::Hello
            | FrameType::JoinAck
            | FrameType::Ack
            | FrameType::Sync
            | FrameType::Schedule => return true,
            _ => {}
        }
        let claimed_slot = self.schedule.slot_at(frame.gateway_time_ms);
        self.schedule
            .slot_owner_ok(frame.node_role, frame.frame_type, claimed_slot)
    }

    pub fn next_seq(&mut self) -> u16 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    /// DATA/ALARM origin sequence uses its own sequence space so bootstrap and
    /// control frames do not make the first sensor sample appear as seq=2.
    pub fn next_data_seq(&mut self) -> u16 {
        self.data_seq = self.data_seq.wrapping_add(1);
        self.data_seq
    }

    /// HEARTBEAT uses its own sequence space so DATA/ALARM origin sequence
    /// numbers stay strictly consecutive, keeping gap accounting exact.
    pub fn next_heartbeat_seq(&mut self) -> u16 {
        self.heartbeat_seq = self.heartbeat_seq.wrapping_add(1);
        self.heartbeat_seq
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
        let frame_seq = self.next_seq();
        self.sync_seq = self.sync_seq.wrapping_add(1);
        Ok(Frame {
            net_id: NET_ID,
            src_id: self.role.node_id(),
            dst_id: BROADCAST_ID,
            node_role: self.role,
            zone_id: DEMO_ZONE_ID,
            frame_type: FrameType::Sync,
            seq: frame_seq,
            hop: self.hop,
            gateway_time_ms: self.sync.gateway_time_ms(local_time_ms),
            payload: protocol::sync_payload(
                self.sync_seq,
                self.schedule.schedule_version,
                self.schedule.superframe_ms,
                self.schedule.slot_ms,
                self.schedule.guard_before_ms,
                self.schedule.active_ms,
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
        let origin_seq = self.next_data_seq();
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
            payload: protocol::data_payload(
                self.role.node_id(),
                origin_seq,
                temp_centi_c,
                humidity_centi_percent,
            )?,
        })
    }

    pub fn make_alarm(
        &mut self,
        local_time_ms: u64,
        temp_centi_c: i16,
        humidity_centi_percent: u16,
    ) -> AppResult<Frame> {
        let seq = self.next_seq();
        let origin_seq = self.next_data_seq();
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
            payload: protocol::data_payload(
                self.role.node_id(),
                origin_seq,
                temp_centi_c,
                humidity_centi_percent,
            )?,
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
        let seq = self.next_heartbeat_seq();
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
                if let Some((
                    sync_seq,
                    schedule_version,
                    superframe_ms,
                    slot_ms,
                    guard_before_ms,
                    active_ms,
                )) = protocol::decode_sync_payload(&frame.payload)
                {
                    self.schedule.schedule_version = schedule_version;
                    self.schedule.superframe_ms = superframe_ms;
                    self.schedule.slot_ms = slot_ms;
                    self.schedule.guard_before_ms = guard_before_ms;
                    self.schedule.active_ms = active_ms;
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
                if let Some((origin_id, origin_seq, temp_centi_c, humidity_centi_percent)) =
                    protocol::decode_data_payload(&frame.payload)
                {
                    FrameAction::Data {
                        origin_id,
                        origin_seq,
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
                let key = (frame.src_id, frame.seq);
                if self.last_heartbeat_seq == Some(key) {
                    return FrameAction::Ignore;
                }
                self.last_heartbeat_seq = Some(key);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlarmLatch {
    active: bool,
}

impl AlarmLatch {
    pub const fn new() -> Self {
        Self { active: false }
    }

    pub const fn is_active(self) -> bool {
        self.active
    }

    pub fn update(
        &mut self,
        sample: EnvironmentSample,
        temp_alarm_centi_c: i16,
        humidity_alarm_centi_percent: u16,
        temp_clear_centi_c: i16,
        humidity_clear_centi_percent: u16,
    ) -> AlarmTransition {
        let previous = self.active;
        if self.active {
            self.active = sample.temp_centi_c >= temp_clear_centi_c
                || sample.humidity_centi_percent >= humidity_clear_centi_percent;
        } else {
            self.active = sample.temp_centi_c >= temp_alarm_centi_c
                || sample.humidity_centi_percent >= humidity_alarm_centi_percent;
        }

        match (previous, self.active) {
            (false, true) => AlarmTransition::Raised,
            (true, false) => AlarmTransition::Cleared,
            _ => AlarmTransition::Unchanged,
        }
    }
}

impl Default for AlarmLatch {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlarmTransition {
    Unchanged,
    Raised,
    Cleared,
}

#[cfg(test)]
mod tests {
    use super::{DemoNode, FrameAction, NetworkPhase};
    use crate::protocol::{self, Frame, FrameType, Payload};
    use crate::role::{BROADCAST_ID, DEMO_ZONE_ID, GATEWAY_ID, NET_ID, NodeRole, RELAY_ID};

    fn frame(role: NodeRole, frame_type: FrameType, gateway_time_ms: u64) -> Frame {
        Frame {
            net_id: NET_ID,
            src_id: role.node_id(),
            dst_id: BROADCAST_ID,
            node_role: role,
            zone_id: DEMO_ZONE_ID,
            frame_type,
            seq: 1,
            hop: role.default_hop(),
            gateway_time_ms,
            payload: Payload::new(),
        }
    }

    #[test]
    fn gateway_is_synced_immediately() {
        assert!(DemoNode::new(NodeRole::Gateway).is_synced());
    }

    #[test]
    fn relay_and_sensor_are_unsynced_until_first_sync() {
        let mut node = DemoNode::new(NodeRole::Relay);
        assert!(!node.is_synced());
        node.sync.apply_sync(1, 1_000, 900);
        assert!(node.is_synced());
    }

    #[test]
    fn unsynced_node_accepts_any_frame() {
        let node = DemoNode::new(NodeRole::Relay);
        // Claims the quiet slot (7); would be rejected once synced.
        assert!(node.accepts_frame_slot(&frame(NodeRole::Sensor, FrameType::Data, 7_200)));
    }

    #[test]
    fn synced_node_enforces_slot_for_periodic_traffic() {
        let mut node = DemoNode::new(NodeRole::Relay);
        node.sync.apply_sync(1, 1_000, 1_000);
        // Sensor DATA in the sensor slot (2) is accepted; in the quiet slot (7) dropped.
        assert!(node.accepts_frame_slot(&frame(NodeRole::Sensor, FrameType::Data, 2_300)));
        assert!(!node.accepts_frame_slot(&frame(NodeRole::Sensor, FrameType::Data, 7_300)));
    }

    #[test]
    fn synced_node_always_accepts_control_frames() {
        let mut node = DemoNode::new(NodeRole::Sensor);
        node.sync.apply_sync(1, 1_000, 1_000);
        // SYNC / JOIN_ACK accepted regardless of the slot they claim.
        assert!(node.accepts_frame_slot(&frame(NodeRole::Gateway, FrameType::Sync, 7_500)));
        assert!(node.accepts_frame_slot(&frame(NodeRole::Relay, FrameType::JoinAck, 7_500)));
    }

    #[test]
    fn alarm_accepted_in_both_data_and_retry_slots() {
        let mut node = DemoNode::new(NodeRole::Relay);
        node.sync.apply_sync(1, 1_000, 1_000);
        // Sensor ALARM: sensor slot (2) for first send, retry slot (4) for retransmit.
        assert!(node.accepts_frame_slot(&frame(NodeRole::Sensor, FrameType::Alarm, 2_300)));
        assert!(node.accepts_frame_slot(&frame(NodeRole::Sensor, FrameType::Alarm, 4_300)));
    }

    #[test]
    fn data_origin_seq_is_independent_from_control_frame_seq() {
        let mut node = DemoNode::new(NodeRole::Sensor);

        assert_eq!(node.make_hello(0).unwrap().seq, 1);
        let data = node.make_data(1_000, 2_500, 4_000).unwrap();
        let (_origin_id, origin_seq, _temp, _humidity) =
            protocol::decode_data_payload(&data.payload).unwrap();

        assert_eq!(data.seq, 2);
        assert_eq!(origin_seq, 1);
    }

    #[test]
    fn data_and_alarm_share_consecutive_origin_sequence() {
        let mut node = DemoNode::new(NodeRole::Sensor);
        let data = node.make_data(1_000, 2_500, 4_000).unwrap();
        let alarm = node.make_alarm(2_000, 3_100, 8_100).unwrap();

        let (_, data_origin_seq, _, _) = protocol::decode_data_payload(&data.payload).unwrap();
        let (_, alarm_origin_seq, _, _) = protocol::decode_data_payload(&alarm.payload).unwrap();

        assert_eq!(data_origin_seq, 1);
        assert_eq!(alarm_origin_seq, 2);
    }

    #[test]
    fn demo_nodes_join_sync_and_deliver_data_across_two_hops() {
        let mut gateway = DemoNode::new(NodeRole::Gateway);
        let mut relay = DemoNode::new(NodeRole::Relay);
        let mut sensor = DemoNode::new(NodeRole::Sensor);

        gateway.mark_joined();

        let relay_hello = relay.make_hello(100).unwrap();
        let relay_join = gateway
            .make_join_ack(relay_hello.src_id, relay_hello.node_role, 110)
            .unwrap();
        assert_eq!(
            relay.apply_frame(&relay_join, 120),
            FrameAction::Joined {
                parent_id: GATEWAY_ID,
                hop: 1,
                slot_id: NodeRole::Relay.default_slot()
            }
        );
        assert_eq!(relay.phase(), NetworkPhase::Joined);

        let gateway_sync = gateway.make_sync(1_000).unwrap();
        assert!(matches!(
            relay.apply_frame(&gateway_sync, 1_020),
            FrameAction::Synced { sync_seq: 1, .. }
        ));
        assert!(relay.is_synced());

        let sensor_hello = sensor.make_hello(1_100).unwrap();
        let sensor_join = relay
            .make_join_ack(sensor_hello.src_id, sensor_hello.node_role, 1_110)
            .unwrap();
        assert_eq!(
            sensor.apply_frame(&sensor_join, 1_120),
            FrameAction::Joined {
                parent_id: RELAY_ID,
                hop: 2,
                slot_id: NodeRole::Sensor.default_slot()
            }
        );

        let forwarded_sync = relay
            .make_forwarded(&gateway_sync, BROADCAST_ID, 1_200)
            .unwrap();
        assert!(matches!(
            sensor.apply_frame(&forwarded_sync, 1_220),
            FrameAction::Synced { sync_seq: 1, .. }
        ));
        assert!(sensor.is_synced());

        let sensor_data = sensor.make_data(2_300, 2_650, 6_700).unwrap();
        assert_eq!(
            relay.apply_frame(&sensor_data, 2_310),
            FrameAction::Data {
                origin_id: NodeRole::Sensor.node_id(),
                origin_seq: 1,
                temp_centi_c: 2_650,
                humidity_centi_percent: 6_700,
                alarm: false,
            }
        );

        let forwarded_data = relay
            .make_forwarded(&sensor_data, GATEWAY_ID, 3_300)
            .unwrap();
        assert_eq!(
            gateway.apply_frame(&forwarded_data, 3_310),
            FrameAction::Data {
                origin_id: NodeRole::Sensor.node_id(),
                origin_seq: 1,
                temp_centi_c: 2_650,
                humidity_centi_percent: 6_700,
                alarm: false,
            }
        );
    }
}
