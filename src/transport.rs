use core::fmt;

use embassy_time::{Duration, Timer};
use esp_hal::{
    Blocking,
    uart::{RxError, TxError, Uart},
};

use crate::{
    hardware::DX_LR32_DEMO_AT_SEQUENCE,
    protocol::{
        DecodeError, EncodedFrame, Frame, FrameStreamDecoder, MAX_FRAME_LEN, StreamDecodeError,
    },
};

pub trait LoraTransport {
    type Error;

    fn send(&mut self, frame: &Frame) -> Result<usize, Self::Error>;
    fn receive(&mut self, buffer: &[u8]) -> Result<Option<Frame>, Self::Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryTransportError {
    Encode,
    Decode(DecodeError),
}

pub struct MemoryTransport {
    last_tx: Option<EncodedFrame>,
}

pub struct LoraUartTransport<'d> {
    uart: Uart<'d, Blocking>,
    decoder: FrameStreamDecoder,
}

impl<'d> LoraUartTransport<'d> {
    pub const fn new(uart: Uart<'d, Blocking>) -> Self {
        Self {
            uart,
            decoder: FrameStreamDecoder::new(),
        }
    }

    pub fn send_frame(&mut self, frame: &Frame) -> Result<usize, LoraUartError> {
        let encoded = frame.encode().map_err(|_| LoraUartError::Encode)?;
        self.write_all(&encoded)?;
        self.uart.flush().map_err(LoraUartError::Tx)?;
        Ok(encoded.len())
    }

    pub fn read_frame(&mut self) -> Result<Option<Frame>, LoraUartError> {
        let mut buffer = [0u8; MAX_FRAME_LEN];
        let read = self.uart.read(&mut buffer).map_err(LoraUartError::Rx)?;
        if read == 0 {
            return Ok(None);
        }
        self.decoder
            .push_bytes(&buffer[..read])
            .map_err(LoraUartError::Stream)?;
        self.decoder.next_frame().map_err(LoraUartError::Stream)
    }

    pub fn try_read_frame(&mut self) -> Result<Option<Frame>, LoraUartError> {
        if let Some(frame) = self.decoder.next_frame().map_err(LoraUartError::Stream)? {
            return Ok(Some(frame));
        }
        if !self.uart.read_ready() {
            return Ok(None);
        }
        self.read_frame()
    }

    fn write_all(&mut self, mut bytes: &[u8]) -> Result<(), LoraUartError> {
        while !bytes.is_empty() {
            let written = self.uart.write(bytes).map_err(LoraUartError::Tx)?;
            bytes = &bytes[written..];
        }
        Ok(())
    }
}

impl LoraTransport for LoraUartTransport<'_> {
    type Error = LoraUartError;

    fn send(&mut self, frame: &Frame) -> Result<usize, Self::Error> {
        self.send_frame(frame)
    }

    fn receive(&mut self, buffer: &[u8]) -> Result<Option<Frame>, Self::Error> {
        if buffer.is_empty() {
            self.try_read_frame()
        } else {
            Frame::decode(buffer)
                .map(Some)
                .map_err(LoraUartError::Decode)
        }
    }
}

/// Configure a DX-LR32 module via AT commands at boot.
///
/// Sends an AT command sequence (enter AT mode → config → exit), waits for
/// expected responses, and lets the module reboot into transparent mode.
/// Returns `Ok(())` on success or `Err` on timeout / unexpected response.
///
/// **Important**: after this function returns the module reboots; the caller
/// must wait ~500 ms before sending data frames.
pub async fn configure_dx_lr32_module(uart: &mut Uart<'_, Blocking>) -> Result<(), LoraUartError> {
    for (i, cmd) in DX_LR32_DEMO_AT_SEQUENCE.iter().enumerate() {
        let is_enter = *cmd == "+++" && i == 0;
        let is_exit = *cmd == "+++" && i > 0;

        // Each entry in DX_LR32_DEMO_AT_SEQUENCE is sent verbatim
        // (AT commands already include \r\n; +++ is sent bare)
        write_all_raw(uart, cmd.as_bytes())?;

        if is_enter {
            // Wait for "Entry AT" response
            match read_until(uart, b"Entry AT", 3_000).await {
                Ok(..) => log::info!("DX-LR32: entered AT mode"),
                Err(LoraUartError::Timeout) => {
                    log::warn!("DX-LR32: no 'Entry AT' response (already in AT mode?)");
                }
                Err(e) => return Err(e),
            }
        } else if is_exit {
            // Module auto-resets after exit; give it time
            Timer::after(Duration::from_millis(1_500)).await;
            log::info!("DX-LR32: exited AT mode, module rebooting");
        } else {
            // Read until "OK" or "EEROR"
            match read_until(uart, b"OK", 2_000).await {
                Ok(..) => log::info!("DX-LR32: {} → OK", cmd),
                Err(LoraUartError::Timeout) => {
                    log::warn!("DX-LR32: {} timed out waiting for OK", cmd);
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

/// Drain any post-reboot garbage bytes from the module UART.
pub fn drain_uart(uart: &mut Uart<'_, Blocking>) {
    let mut sink = [0u8; 64];
    while uart.read_ready() {
        let _ = uart.read(&mut sink);
    }
}

// -- internal helpers --

fn write_all_raw(uart: &mut Uart<'_, Blocking>, mut bytes: &[u8]) -> Result<(), LoraUartError> {
    while !bytes.is_empty() {
        let written = uart.write(bytes).map_err(LoraUartError::Tx)?;
        bytes = &bytes[written..];
    }
    uart.flush().map_err(LoraUartError::Tx)?;
    Ok(())
}

async fn read_until(
    uart: &mut Uart<'_, Blocking>,
    marker: &[u8],
    timeout_ms: u64,
) -> Result<usize, LoraUartError> {
    let mut buf = [0u8; 128];
    let mut tail = 0usize;
    let poll_interval_ms = 20u64;
    let mut elapsed = 0u64;

    loop {
        if elapsed >= timeout_ms {
            return Err(LoraUartError::Timeout);
        }

        if uart.read_ready() {
            let mut byte = [0u8; 1];
            let n = uart.read(&mut byte).map_err(LoraUartError::Rx)?;
            if n == 0 {
                Timer::after(Duration::from_millis(10)).await;
                continue;
            }
            if tail < buf.len() {
                buf[tail] = byte[0];
                tail += 1;
            } else {
                // Shift buffer to make room
                buf.copy_within(1.., 0);
                buf[tail - 1] = byte[0];
            }
            if tail >= marker.len() && &buf[tail - marker.len()..tail] == marker {
                return Ok(tail);
            }
        } else {
            Timer::after(Duration::from_millis(poll_interval_ms)).await;
            elapsed += poll_interval_ms;
        }
    }
}

#[derive(Debug)]
pub enum LoraUartError {
    Encode,
    Decode(DecodeError),
    Stream(StreamDecodeError),
    Tx(TxError),
    Rx(RxError),
    Timeout,
}

impl fmt::Display for LoraUartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode => f.write_str("frame encode failed"),
            Self::Decode(source) => write!(f, "frame decode failed: {source}"),
            Self::Stream(source) => write!(f, "UART stream decode failed: {source}"),
            Self::Tx(source) => write!(f, "UART TX failed: {source}"),
            Self::Rx(source) => write!(f, "UART RX failed: {source}"),
            Self::Timeout => f.write_str("AT command timed out"),
        }
    }
}

impl MemoryTransport {
    pub const fn new() -> Self {
        Self { last_tx: None }
    }

    pub const fn last_tx(&self) -> Option<&EncodedFrame> {
        self.last_tx.as_ref()
    }
}

impl Default for MemoryTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl LoraTransport for MemoryTransport {
    type Error = MemoryTransportError;

    fn send(&mut self, frame: &Frame) -> Result<usize, Self::Error> {
        let encoded = frame.encode().map_err(|_| MemoryTransportError::Encode)?;
        let len = encoded.len();
        self.last_tx = Some(encoded);
        Ok(len)
    }

    fn receive(&mut self, buffer: &[u8]) -> Result<Option<Frame>, Self::Error> {
        if buffer.is_empty() {
            return Ok(None);
        }
        Frame::decode(buffer)
            .map(Some)
            .map_err(MemoryTransportError::Decode)
    }
}
