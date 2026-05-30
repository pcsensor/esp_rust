//! Compact binary wire protocol shared by every demo node.
//!
//! Frames are intentionally fixed-layout up to the bounded payload. This keeps
//! encode/decode deterministic on `no_std` targets and lets the UART stream
//! decoder resynchronize after byte loss or noisy input.

use core::fmt;
use heapless::Vec;

use crate::role::{NodeRole, RELAY_ID, SENSOR_ID};

pub const MAGIC: u8 = 0xc3;
pub const VERSION: u8 = 1;
pub const MAX_PAYLOAD_LEN: usize = 32;
pub const HEADER_LEN: usize = 21;
pub const CRC_LEN: usize = 2;
pub const MAX_FRAME_LEN: usize =
    1 + 1 + 2 + 1 + 1 + 1 + 1 + 1 + 2 + 1 + 8 + 1 + MAX_PAYLOAD_LEN + CRC_LEN;
pub const RX_BUFFER_LEN: usize = MAX_FRAME_LEN * 6;

pub type Payload = Vec<u8, MAX_PAYLOAD_LEN>;
pub type EncodedFrame = Vec<u8, MAX_FRAME_LEN>;
pub type RxBuffer = Vec<u8, RX_BUFFER_LEN>;

pub const HEARTBEAT_PRESENCE_RELAY: u8 = 1u8 << RELAY_ID;
pub const HEARTBEAT_PRESENCE_SENSOR: u8 = 1u8 << SENSOR_ID;

/// Application frame kind carried in the wire header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    Hello = 1,
    JoinAck = 2,
    Sync = 3,
    Schedule = 4,
    Data = 5,
    Alarm = 6,
    Ack = 7,
    Heartbeat = 8,
}

pub const fn heartbeat_presence_for_role(role: NodeRole) -> u8 {
    1u8 << role.node_id()
}

pub const fn heartbeat_presence_contains(mask: u8, role: NodeRole) -> bool {
    (mask & heartbeat_presence_for_role(role)) != 0
}

impl FrameType {
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Hello),
            2 => Some(Self::JoinAck),
            3 => Some(Self::Sync),
            4 => Some(Self::Schedule),
            5 => Some(Self::Data),
            6 => Some(Self::Alarm),
            7 => Some(Self::Ack),
            8 => Some(Self::Heartbeat),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Hello => "HELLO",
            Self::JoinAck => "JOIN_ACK",
            Self::Sync => "SYNC",
            Self::Schedule => "SCHEDULE",
            Self::Data => "DATA",
            Self::Alarm => "ALARM",
            Self::Ack => "ACK",
            Self::Heartbeat => "HEARTBEAT",
        }
    }
}

impl fmt::Display for FrameType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Decoded protocol frame.
///
/// `payload` is interpreted according to `frame_type` by the payload helpers in
/// this module. The frame can be encoded directly for LoRa UART transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub net_id: u16,
    pub src_id: u8,
    pub dst_id: u8,
    pub node_role: NodeRole,
    pub zone_id: u8,
    pub frame_type: FrameType,
    pub seq: u16,
    pub hop: u8,
    pub gateway_time_ms: u64,
    pub payload: Payload,
}

impl Frame {
    pub fn encode(&self) -> Result<EncodedFrame, EncodeError> {
        let mut out = EncodedFrame::new();
        push(&mut out, MAGIC)?;
        push(&mut out, VERSION)?;
        extend(&mut out, &self.net_id.to_le_bytes())?;
        push(&mut out, self.src_id)?;
        push(&mut out, self.dst_id)?;
        push(&mut out, self.node_role as u8)?;
        push(&mut out, self.zone_id)?;
        push(&mut out, self.frame_type as u8)?;
        extend(&mut out, &self.seq.to_le_bytes())?;
        push(&mut out, self.hop)?;
        extend(&mut out, &self.gateway_time_ms.to_le_bytes())?;
        push(&mut out, self.payload.len() as u8)?;
        extend(&mut out, &self.payload)?;
        let crc = crc16_ccitt(&out);
        extend(&mut out, &crc.to_le_bytes())?;
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < HEADER_LEN + CRC_LEN {
            return Err(DecodeError::TooShort);
        }
        if bytes[0] != MAGIC {
            return Err(DecodeError::BadMagic);
        }
        if bytes[1] != VERSION {
            return Err(DecodeError::UnsupportedVersion);
        }

        let payload_len = bytes[20] as usize;
        let expected_len = HEADER_LEN + payload_len + CRC_LEN;
        if bytes.len() != expected_len {
            return Err(DecodeError::BadLength);
        }
        if payload_len > MAX_PAYLOAD_LEN {
            return Err(DecodeError::PayloadTooLong);
        }

        let expected_crc = u16::from_le_bytes([bytes[expected_len - 2], bytes[expected_len - 1]]);
        let actual_crc = crc16_ccitt(&bytes[..expected_len - 2]);
        if expected_crc != actual_crc {
            return Err(DecodeError::BadCrc);
        }

        let node_role = NodeRole::from_u8(bytes[6]).ok_or(DecodeError::BadRole)?;
        let frame_type = FrameType::from_u8(bytes[8]).ok_or(DecodeError::BadFrameType)?;
        let mut payload = Payload::new();
        payload
            .extend_from_slice(&bytes[HEADER_LEN..HEADER_LEN + payload_len])
            .map_err(|_| DecodeError::PayloadTooLong)?;

        Ok(Self {
            net_id: u16::from_le_bytes([bytes[2], bytes[3]]),
            src_id: bytes[4],
            dst_id: bytes[5],
            node_role,
            zone_id: bytes[7],
            frame_type,
            seq: u16::from_le_bytes([bytes[9], bytes[10]]),
            hop: bytes[11],
            gateway_time_ms: u64::from_le_bytes([
                bytes[12], bytes[13], bytes[14], bytes[15], bytes[16], bytes[17], bytes[18],
                bytes[19],
            ]),
            payload,
        })
    }
}

/// Failure while serializing a frame or payload into a bounded buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeError {
    FrameTooLong,
    PayloadTooLong,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooLong => f.write_str("frame buffer too small"),
            Self::PayloadTooLong => f.write_str("payload buffer too small"),
        }
    }
}

/// Failure while validating a complete frame buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    TooShort,
    BadMagic,
    UnsupportedVersion,
    BadLength,
    PayloadTooLong,
    BadCrc,
    BadRole,
    BadFrameType,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort => f.write_str("frame is too short"),
            Self::BadMagic => f.write_str("bad frame magic"),
            Self::UnsupportedVersion => f.write_str("unsupported frame version"),
            Self::BadLength => f.write_str("bad frame length"),
            Self::PayloadTooLong => f.write_str("payload is too long"),
            Self::BadCrc => f.write_str("bad crc"),
            Self::BadRole => f.write_str("bad node role"),
            Self::BadFrameType => f.write_str("bad frame type"),
        }
    }
}

/// Incremental decoder for a noisy byte stream.
///
/// Bytes can arrive in arbitrary chunks. `next_frame` returns complete frames
/// one at a time and discards invalid prefixes so the stream can resynchronize
/// after corrupted bytes, bad CRCs, or partial frames.
#[derive(Debug, Default)]
pub struct FrameStreamDecoder {
    buffer: RxBuffer,
}

impl FrameStreamDecoder {
    pub const fn new() -> Self {
        Self {
            buffer: RxBuffer::new(),
        }
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) -> Result<(), StreamDecodeError> {
        if self.buffer.len() + bytes.len() > RX_BUFFER_LEN {
            self.buffer.clear();
            return Err(StreamDecodeError::BufferOverflow);
        }
        self.buffer
            .extend_from_slice(bytes)
            .map_err(|_| StreamDecodeError::BufferOverflow)
    }

    pub fn next_frame(&mut self) -> Result<Option<Frame>, StreamDecodeError> {
        loop {
            self.discard_before_magic();

            if self.buffer.len() < HEADER_LEN + CRC_LEN {
                return Ok(None);
            }

            if self.buffer[1] != VERSION {
                self.discard(1);
                continue;
            }

            let payload_len = self.buffer[20] as usize;
            if payload_len > MAX_PAYLOAD_LEN {
                self.discard(1);
                continue;
            }

            let frame_len = HEADER_LEN + payload_len + CRC_LEN;
            if self.buffer.len() < frame_len {
                return Ok(None);
            }

            match Frame::decode(&self.buffer[..frame_len]) {
                Ok(frame) => {
                    self.discard(frame_len);
                    return Ok(Some(frame));
                }
                Err(error) => {
                    self.discard(1);
                    if matches!(error, DecodeError::TooShort) {
                        return Ok(None);
                    }
                    continue;
                }
            }
        }
    }

    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }

    fn discard_before_magic(&mut self) {
        let Some(index) = self.buffer.iter().position(|byte| *byte == MAGIC) else {
            self.buffer.clear();
            return;
        };
        self.discard(index);
    }

    fn discard(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        if count >= self.buffer.len() {
            self.buffer.clear();
            return;
        }

        let remaining = self.buffer.len() - count;
        for index in 0..remaining {
            self.buffer[index] = self.buffer[index + count];
        }
        self.buffer.truncate(remaining);
    }
}

/// Failure while feeding or draining the stream decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamDecodeError {
    BufferOverflow,
    Decode(DecodeError),
}

impl fmt::Display for StreamDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferOverflow => f.write_str("RX frame stream buffer overflow"),
            Self::Decode(source) => write!(f, "stream frame decode failed: {source}"),
        }
    }
}

pub fn hello_payload(parent_id: Option<u8>, slot_id: u8) -> Result<Payload, EncodeError> {
    let mut payload = Payload::new();
    push(&mut payload, parent_id.unwrap_or(0))?;
    push(&mut payload, slot_id)?;
    Ok(payload)
}

pub fn join_ack_payload(parent_id: u8, hop: u8, slot_id: u8) -> Result<Payload, EncodeError> {
    let mut payload = Payload::new();
    push(&mut payload, parent_id)?;
    push(&mut payload, hop)?;
    push(&mut payload, slot_id)?;
    Ok(payload)
}

pub fn sync_payload(
    sync_seq: u16,
    schedule_version: u8,
    superframe_ms: u32,
    slot_ms: u32,
    guard_before_ms: u32,
    active_ms: u32,
) -> Result<Payload, EncodeError> {
    let mut payload = Payload::new();
    extend_payload(&mut payload, &sync_seq.to_le_bytes())?;
    push(&mut payload, schedule_version)?;
    extend_payload(&mut payload, &superframe_ms.to_le_bytes())?;
    extend_payload(&mut payload, &slot_ms.to_le_bytes())?;
    extend_payload(&mut payload, &(guard_before_ms as u16).to_le_bytes())?;
    extend_payload(&mut payload, &(active_ms as u16).to_le_bytes())?;
    Ok(payload)
}

pub fn data_payload(
    origin_id: u8,
    origin_seq: u16,
    temp_centi_c: i16,
    humidity_centi_percent: u16,
) -> Result<Payload, EncodeError> {
    let mut payload = Payload::new();
    push(&mut payload, origin_id)?;
    extend_payload(&mut payload, &origin_seq.to_le_bytes())?;
    extend_payload(&mut payload, &temp_centi_c.to_le_bytes())?;
    extend_payload(&mut payload, &humidity_centi_percent.to_le_bytes())?;
    Ok(payload)
}

pub fn ack_payload(acked_seq: u16, acked_type: FrameType) -> Result<Payload, EncodeError> {
    let mut payload = Payload::new();
    extend_payload(&mut payload, &acked_seq.to_le_bytes())?;
    push(&mut payload, acked_type as u8)?;
    Ok(payload)
}

pub fn heartbeat_payload(
    slot_id: u8,
    hop: u8,
    sync_seq: u16,
    presence_mask: u8,
) -> Result<Payload, EncodeError> {
    let mut payload = Payload::new();
    push(&mut payload, slot_id)?;
    push(&mut payload, hop)?;
    extend_payload(&mut payload, &sync_seq.to_le_bytes())?;
    push(&mut payload, presence_mask)?;
    Ok(payload)
}

pub fn decode_join_ack_payload(payload: &[u8]) -> Option<(u8, u8, u8)> {
    if payload.len() < 3 {
        return None;
    }
    Some((payload[0], payload[1], payload[2]))
}

pub fn decode_sync_payload(payload: &[u8]) -> Option<(u16, u8, u32, u32, u32, u32)> {
    if payload.len() < 15 {
        return None;
    }
    Some((
        u16::from_le_bytes([payload[0], payload[1]]),
        payload[2],
        u32::from_le_bytes([payload[3], payload[4], payload[5], payload[6]]),
        u32::from_le_bytes([payload[7], payload[8], payload[9], payload[10]]),
        u16::from_le_bytes([payload[11], payload[12]]) as u32,
        u16::from_le_bytes([payload[13], payload[14]]) as u32,
    ))
}

pub fn decode_data_payload(payload: &[u8]) -> Option<(u8, u16, i16, u16)> {
    if payload.len() < 7 {
        return None;
    }
    Some((
        payload[0],
        u16::from_le_bytes([payload[1], payload[2]]),
        i16::from_le_bytes([payload[3], payload[4]]),
        u16::from_le_bytes([payload[5], payload[6]]),
    ))
}

pub fn decode_ack_payload(payload: &[u8]) -> Option<(u16, FrameType)> {
    if payload.len() < 3 {
        return None;
    }
    let acked_seq = u16::from_le_bytes([payload[0], payload[1]]);
    let acked_type = FrameType::from_u8(payload[2])?;
    Some((acked_seq, acked_type))
}

pub fn decode_heartbeat_payload(payload: &[u8]) -> Option<(u8, u8, u16, u8)> {
    if payload.len() < 4 {
        return None;
    }
    Some((
        payload[0],
        payload[1],
        u16::from_le_bytes([payload[2], payload[3]]),
        payload.get(4).copied().unwrap_or(0),
    ))
}

pub fn crc16_ccitt(bytes: &[u8]) -> u16 {
    let mut crc = 0xffff;
    for byte in bytes {
        crc ^= (*byte as u16) << 8;
        for _ in 0..8 {
            if (crc & 0x8000) != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

fn push<const N: usize>(out: &mut Vec<u8, N>, byte: u8) -> Result<(), EncodeError> {
    out.push(byte).map_err(|_| EncodeError::FrameTooLong)
}

fn extend(out: &mut EncodedFrame, bytes: &[u8]) -> Result<(), EncodeError> {
    out.extend_from_slice(bytes)
        .map_err(|_| EncodeError::FrameTooLong)
}

fn extend_payload(out: &mut Payload, bytes: &[u8]) -> Result<(), EncodeError> {
    out.extend_from_slice(bytes)
        .map_err(|_| EncodeError::PayloadTooLong)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::role::{DEMO_ZONE_ID, NET_ID, NodeRole, SENSOR_ID};

    fn sample_frame(seq: u16) -> Frame {
        Frame {
            net_id: NET_ID,
            src_id: SENSOR_ID,
            dst_id: crate::role::RELAY_ID,
            node_role: NodeRole::Sensor,
            zone_id: DEMO_ZONE_ID,
            frame_type: FrameType::Data,
            seq,
            hop: 2,
            gateway_time_ms: 12_345,
            payload: data_payload(SENSOR_ID, seq, 2_513, 6_481).unwrap(),
        }
    }

    #[test]
    fn frame_encode_decode_roundtrip_preserves_fields() {
        let frame = sample_frame(7);
        let encoded = frame.encode().unwrap();

        assert_eq!(encoded[0], MAGIC);
        assert_eq!(encoded[1], VERSION);
        assert_eq!(encoded.len(), HEADER_LEN + frame.payload.len() + CRC_LEN);
        assert_eq!(Frame::decode(&encoded).unwrap(), frame);
    }

    #[test]
    fn decode_rejects_bad_crc() {
        let mut encoded = sample_frame(1).encode().unwrap();
        let payload_byte = HEADER_LEN;
        encoded[payload_byte] ^= 0x55;

        assert_eq!(Frame::decode(&encoded), Err(DecodeError::BadCrc));
    }

    #[test]
    fn stream_decoder_waits_for_split_frame() {
        let encoded = sample_frame(2).encode().unwrap();
        let split_at = 8;
        let mut decoder = FrameStreamDecoder::new();

        decoder.push_bytes(&encoded[..split_at]).unwrap();
        assert_eq!(decoder.next_frame().unwrap(), None);
        assert_eq!(decoder.buffered_len(), split_at);

        decoder.push_bytes(&encoded[split_at..]).unwrap();
        assert_eq!(decoder.next_frame().unwrap(), Some(sample_frame(2)));
        assert_eq!(decoder.buffered_len(), 0);
    }

    #[test]
    fn stream_decoder_discards_noise_before_magic() {
        let encoded = sample_frame(3).encode().unwrap();
        let mut decoder = FrameStreamDecoder::new();

        decoder.push_bytes(&[0, 1, 2, MAGIC - 1]).unwrap();
        decoder.push_bytes(&encoded).unwrap();

        assert_eq!(decoder.next_frame().unwrap(), Some(sample_frame(3)));
        assert_eq!(decoder.buffered_len(), 0);
    }

    #[test]
    fn stream_decoder_returns_sticky_frames_one_at_a_time() {
        let first = sample_frame(4).encode().unwrap();
        let second = sample_frame(5).encode().unwrap();
        let mut decoder = FrameStreamDecoder::new();

        decoder.push_bytes(&first).unwrap();
        decoder.push_bytes(&second).unwrap();

        assert_eq!(decoder.next_frame().unwrap(), Some(sample_frame(4)));
        assert_eq!(decoder.next_frame().unwrap(), Some(sample_frame(5)));
        assert_eq!(decoder.next_frame().unwrap(), None);
    }

    #[test]
    fn stream_decoder_resyncs_after_bad_crc() {
        let mut bad = sample_frame(6).encode().unwrap();
        let good = sample_frame(7).encode().unwrap();
        let crc_byte = bad.len() - 1;
        bad[crc_byte] ^= 0xaa;

        let mut decoder = FrameStreamDecoder::new();
        decoder.push_bytes(&bad).unwrap();
        decoder.push_bytes(&good).unwrap();

        assert_eq!(decoder.next_frame().unwrap(), Some(sample_frame(7)));
        assert_eq!(decoder.buffered_len(), 0);
    }

    #[test]
    fn stream_decoder_clears_buffer_on_overflow() {
        let mut decoder = FrameStreamDecoder::new();
        let bytes = [0x55; RX_BUFFER_LEN + 1];

        assert_eq!(
            decoder.push_bytes(&bytes),
            Err(StreamDecodeError::BufferOverflow)
        );
        assert_eq!(decoder.buffered_len(), 0);
    }

    #[test]
    fn heartbeat_payload_carries_presence_mask() {
        let payload = heartbeat_payload(5, 1, 42, HEARTBEAT_PRESENCE_RELAY).unwrap();

        assert_eq!(
            decode_heartbeat_payload(&payload),
            Some((5, 1, 42, HEARTBEAT_PRESENCE_RELAY))
        );
        assert!(heartbeat_presence_contains(
            HEARTBEAT_PRESENCE_RELAY,
            NodeRole::Relay
        ));
        assert!(!heartbeat_presence_contains(
            HEARTBEAT_PRESENCE_RELAY,
            NodeRole::Sensor
        ));
    }

    #[test]
    fn heartbeat_payload_decoder_accepts_legacy_payload_without_presence() {
        assert_eq!(decode_heartbeat_payload(&[6, 2, 7, 0]), Some((6, 2, 7, 0)));
    }
}
