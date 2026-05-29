use core::fmt;

use embassy_time::{Duration, Timer};
use esp_hal::{
    Async, Blocking,
    uart::{RxError, TxError, Uart, UartRx, UartTx},
};

use crate::{
    hardware::DX_LR32_DEMO_AT_SEQUENCE,
    protocol::{DecodeError, Frame, FrameStreamDecoder, MAX_FRAME_LEN, StreamDecodeError},
};

/// Transmit half of the LoRa UART link. Sole owner of `UartTx`, so only one
/// task ever drives the radio's TX path (no contention). Interrupt-driven async.
pub struct LoraTx<'d> {
    uart: UartTx<'d, Async>,
}

impl<'d> LoraTx<'d> {
    pub const fn new(uart: UartTx<'d, Async>) -> Self {
        Self { uart }
    }

    pub async fn send_frame(&mut self, frame: &Frame) -> Result<usize, LoraUartError> {
        let encoded = frame.encode().map_err(|_| LoraUartError::Encode)?;
        self.write_all(&encoded).await?;
        self.uart.flush_async().await.map_err(LoraUartError::Tx)?;
        Ok(encoded.len())
    }

    async fn write_all(&mut self, mut bytes: &[u8]) -> Result<(), LoraUartError> {
        while !bytes.is_empty() {
            let written = self
                .uart
                .write_async(bytes)
                .await
                .map_err(LoraUartError::Tx)?;
            bytes = &bytes[written..];
        }
        Ok(())
    }
}

/// Receive half of the LoRa UART link. Owns the `UartRx` plus the streaming
/// frame decoder, so RX can be serviced independently of TX timing.
/// Interrupt-driven async: `read_into_decoder` awaits the next bytes instead of
/// polling, so frames never age in the hardware FIFO.
pub struct LoraRx<'d> {
    uart: UartRx<'d, Async>,
    decoder: FrameStreamDecoder,
}

impl<'d> LoraRx<'d> {
    pub const fn new(uart: UartRx<'d, Async>) -> Self {
        Self {
            uart,
            decoder: FrameStreamDecoder::new(),
        }
    }

    /// Await the next chunk of UART bytes (interrupt-driven) and push it into the
    /// stream decoder. Returns the number of bytes read.
    pub async fn read_into_decoder(&mut self) -> Result<usize, LoraUartError> {
        let mut buffer = [0u8; MAX_FRAME_LEN];
        let read = self
            .uart
            .read_async(&mut buffer)
            .await
            .map_err(LoraUartError::Rx)?;
        if read > 0 {
            self.decoder
                .push_bytes(&buffer[..read])
                .map_err(LoraUartError::Stream)?;
        }
        Ok(read)
    }

    /// Pop one complete frame from the decoder buffer, if available.
    pub fn next_decoded_frame(&mut self) -> Result<Option<Frame>, LoraUartError> {
        self.decoder.next_frame().map_err(LoraUartError::Stream)
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
    // Module needs time after power-up before it can accept AT commands
    Timer::after(Duration::from_millis(1_500)).await;

    let Some((enter_cmd, config_cmds)) = DX_LR32_DEMO_AT_SEQUENCE.split_first() else {
        return Ok(());
    };

    Timer::after(Duration::from_millis(1_000)).await;
    write_all_raw(uart, enter_cmd.as_bytes())?;
    Timer::after(Duration::from_millis(1_000)).await;

    match read_until(uart, b"Entry AT", 3_000).await {
        Ok(..) => log::info!("DX-LR32: entered AT mode"),
        Err(LoraUartError::Timeout) => {
            log::warn!("DX-LR32: AT mode unavailable, keep existing transparent-mode config");
            return Ok(());
        }
        Err(e) => return Err(e),
    }

    for cmd in config_cmds.iter().copied().filter(|cmd| !is_at_escape(cmd)) {
        write_all_raw(uart, cmd.as_bytes())?;
        match read_until(uart, b"OK", 2_000).await {
            Ok(..) => log::info!("DX-LR32: {} -> OK", cmd),
            Err(LoraUartError::Timeout) => {
                log::warn!("DX-LR32: {} timed out waiting for OK", cmd);
            }
            Err(e) => return Err(e),
        }
    }

    if DX_LR32_DEMO_AT_SEQUENCE
        .last()
        .is_some_and(|cmd| is_at_escape(cmd))
    {
        Timer::after(Duration::from_millis(1_000)).await;
        write_all_raw(uart, DX_LR32_DEMO_AT_SEQUENCE.last().unwrap().as_bytes())?;
        Timer::after(Duration::from_millis(1_500)).await;
        log::info!("DX-LR32: exited AT mode, module rebooting");
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

fn is_at_escape(cmd: &str) -> bool {
    cmd.trim_end_matches(['\r', '\n']) == "+++"
}

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
