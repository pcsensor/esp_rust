//! Bounded relay buffering for scheduled store-and-forward DATA traffic.

use heapless::Deque;

use crate::protocol::Frame;

/// Depth of the relay store-and-forward queue for periodic DATA.
pub const RELAY_FORWARD_CAPACITY: usize = 4;

/// Bounded FIFO of periodic DATA frames awaiting forwarding in the relay slot.
/// When full the oldest frame is dropped so the freshest samples survive,
/// instead of the previous single-slot buffer silently overwriting everything.
#[derive(Debug)]
pub struct RelayForwardBuffer {
    frames: Deque<Frame, RELAY_FORWARD_CAPACITY>,
}

impl Default for RelayForwardBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl RelayForwardBuffer {
    pub const fn new() -> Self {
        Self {
            frames: Deque::new(),
        }
    }

    pub fn has_pending(&self) -> bool {
        !self.frames.is_empty()
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Buffer a periodic DATA frame for the relay slot. When the queue is full
    /// the oldest frame is dropped so the freshest samples survive.
    pub fn remember(&mut self, frame: &Frame) {
        if self.frames.is_full() {
            let _ = self.frames.pop_front();
        }
        let _ = self.frames.push_back(frame.clone());
    }

    pub fn take(&mut self) -> Option<Frame> {
        self.frames.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Frame, FrameType, Payload};
    use crate::role::{DEMO_ZONE_ID, GATEWAY_ID, NET_ID, NodeRole, SENSOR_ID};

    fn data_frame(seq: u16) -> Frame {
        Frame {
            net_id: NET_ID,
            src_id: SENSOR_ID,
            dst_id: GATEWAY_ID,
            node_role: NodeRole::Sensor,
            zone_id: DEMO_ZONE_ID,
            frame_type: FrameType::Data,
            seq,
            hop: 0,
            gateway_time_ms: 0,
            payload: Payload::new(),
        }
    }

    #[test]
    fn new_buffer_is_empty() {
        let buf = RelayForwardBuffer::new();
        assert!(!buf.has_pending());
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn take_returns_frames_in_fifo_order() {
        let mut buf = RelayForwardBuffer::new();
        buf.remember(&data_frame(1));
        buf.remember(&data_frame(2));
        buf.remember(&data_frame(3));

        assert_eq!(buf.len(), 3);
        assert_eq!(buf.take().map(|f| f.seq), Some(1));
        assert_eq!(buf.take().map(|f| f.seq), Some(2));
        assert_eq!(buf.take().map(|f| f.seq), Some(3));
        assert_eq!(buf.take(), None);
        assert!(!buf.has_pending());
    }

    #[test]
    fn overflow_drops_oldest_keeps_newest() {
        let mut buf = RelayForwardBuffer::new();
        // Push one more than capacity; the very first frame must be evicted.
        for seq in 1..=(RELAY_FORWARD_CAPACITY as u16 + 1) {
            buf.remember(&data_frame(seq));
        }

        assert_eq!(buf.len(), RELAY_FORWARD_CAPACITY);
        // Oldest (seq=1) was dropped; queue holds seq=2..=CAP+1 in order.
        assert_eq!(buf.take().map(|f| f.seq), Some(2));
        let mut last = 2;
        while let Some(frame) = buf.take() {
            assert_eq!(frame.seq, last + 1);
            last = frame.seq;
        }
        assert_eq!(last, RELAY_FORWARD_CAPACITY as u16 + 1);
    }

    #[test]
    fn sustained_overflow_keeps_only_newest_capacity_frames() {
        let mut buf = RelayForwardBuffer::new();
        for seq in 1..=20u16 {
            buf.remember(&data_frame(seq));
        }
        assert_eq!(buf.len(), RELAY_FORWARD_CAPACITY);
        let first = buf.take().map(|f| f.seq).unwrap();
        assert_eq!(first, 20 - RELAY_FORWARD_CAPACITY as u16 + 1);
    }
}
