#![no_std]

use core::fmt::{self, Write};
use embassy_executor::SpawnError;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::{Duration, Timer};
use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii::FONT_6X10},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};
use esp_hal::{
    Blocking,
    i2c::master::{ConfigError as I2cConfigError, I2c},
};
use heapless::String;
use ssd1306::{
    I2CDisplayInterface, Ssd1306,
    mode::{BufferedGraphicsMode, DisplayConfig},
    prelude::*,
};

pub static HEARTBEAT_SIGNAL: Signal<CriticalSectionRawMutex, u32> = Signal::new();

const MAX_MSG_LEN: usize = 32;

type Oled<'d> = Ssd1306<
    I2CInterface<I2c<'d, Blocking>>,
    DisplaySize128x64,
    BufferedGraphicsMode<DisplaySize128x64>,
>;

type OledError = <Oled<'static> as DisplayConfig>::Error;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug)]
pub enum AppError {
    I2cInit {
        bus: &'static str,
        source: I2cConfigError,
    },
    Display {
        operation: DisplayOperation,
        source: OledError,
    },
    TaskSpawn {
        task: TaskName,
        source: SpawnError,
    },
    MessageFormat {
        heartbeat: u32,
        buffer_capacity: usize,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum DisplayOperation {
    Initialize,
    ClearBuffer,
    DrawLine1,
    DrawLine2,
    DrawLine3,
    Flush,
}

#[derive(Debug, Clone, Copy)]
pub enum TaskName {
    Heartbeat,
    Display,
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::I2cInit { bus, source } => {
                write!(f, "failed to initialize {bus}: {source}")
            }
            AppError::Display { operation, source } => {
                write!(f, "OLED display {operation} failed: {source:?}")
            }
            AppError::TaskSpawn { task, source } => {
                write!(f, "failed to spawn {task} task: {source}")
            }
            AppError::MessageFormat {
                heartbeat,
                buffer_capacity,
            } => write!(
                f,
                "failed to format heartbeat message #{heartbeat} into heapless String<{buffer_capacity}>"
            ),
        }
    }
}

impl fmt::Display for DisplayOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DisplayOperation::Initialize => f.write_str("initialization"),
            DisplayOperation::ClearBuffer => f.write_str("buffer clear"),
            DisplayOperation::DrawLine1 => f.write_str("line 1 draw"),
            DisplayOperation::DrawLine2 => f.write_str("line 2 draw"),
            DisplayOperation::DrawLine3 => f.write_str("line 3 draw"),
            DisplayOperation::Flush => f.write_str("flush"),
        }
    }
}

impl fmt::Display for TaskName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskName::Heartbeat => f.write_str("heartbeat"),
            TaskName::Display => f.write_str("display"),
        }
    }
}

pub struct Display<'d> {
    inner: Oled<'d>,
}

impl<'d> Display<'d> {
    pub fn new(i2c: I2c<'d, Blocking>) -> AppResult<Self> {
        let interface = I2CDisplayInterface::new(i2c);
        let mut display =
            Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
                .into_buffered_graphics_mode();
        display.init().map_err(|source| AppError::Display {
            operation: DisplayOperation::Initialize,
            source,
        })?;
        Ok(Self { inner: display })
    }

    pub fn show_message(&mut self, line1: &str, line2: &str, line3: &str) -> AppResult<()> {
        self.inner
            .clear(BinaryColor::Off)
            .map_err(|source| AppError::Display {
                operation: DisplayOperation::ClearBuffer,
                source,
            })?;

        let style = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);

        Text::with_baseline(line1, Point::new(0, 0), style, Baseline::Top)
            .draw(&mut self.inner)
            .map_err(|source| AppError::Display {
                operation: DisplayOperation::DrawLine1,
                source,
            })?;
        Text::with_baseline(line2, Point::new(0, 16), style, Baseline::Top)
            .draw(&mut self.inner)
            .map_err(|source| AppError::Display {
                operation: DisplayOperation::DrawLine2,
                source,
            })?;
        Text::with_baseline(line3, Point::new(0, 32), style, Baseline::Top)
            .draw(&mut self.inner)
            .map_err(|source| AppError::Display {
                operation: DisplayOperation::DrawLine3,
                source,
            })?;

        self.inner.flush().map_err(|source| AppError::Display {
            operation: DisplayOperation::Flush,
            source,
        })
    }
}

#[embassy_executor::task]
pub async fn heartbeat_task() {
    let mut counter: u32 = 0;
    loop {
        Timer::after(Duration::from_secs(1)).await;
        counter = counter.wrapping_add(1);
        HEARTBEAT_SIGNAL.signal(counter);
    }
}

#[embassy_executor::task]
pub async fn display_task(mut display: Display<'static>) {
    loop {
        let seq = HEARTBEAT_SIGNAL.wait().await;

        let mut msg: String<MAX_MSG_LEN> = String::new();
        if write!(&mut msg, "Heartbeat #{}", seq).is_err() {
            log::error!(
                "{}",
                AppError::MessageFormat {
                    heartbeat: seq,
                    buffer_capacity: MAX_MSG_LEN,
                }
            );
            continue;
        }

        log::info!("{}", msg);

        if let Err(error) = display.show_message("ESP32-C3", "SSD1306 OLED", &msg) {
            log::error!("recoverable runtime error: {}", error);
        }
    }
}
