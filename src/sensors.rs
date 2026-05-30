//! SHT40 sensor driver used by the sensor-node firmware.

use core::fmt;

use embassy_time::{Duration, Timer};
use esp_hal::{Blocking, i2c::master::I2c};

use crate::{demo::EnvironmentSample, hardware::Sht40Config};

const MEASURE_HIGH_PRECISION: u8 = 0xfd;
const READ_DELAY: Duration = Duration::from_millis(10);

pub struct Sht40<'d> {
    i2c: I2c<'d, Blocking>,
    config: Sht40Config,
}

impl<'d> Sht40<'d> {
    pub const fn new(i2c: I2c<'d, Blocking>, config: Sht40Config) -> Self {
        Self { i2c, config }
    }

    pub async fn read_sample(&mut self) -> Result<EnvironmentSample, Sht40Error> {
        self.i2c
            .write(self.config.i2c_address, &[MEASURE_HIGH_PRECISION])
            .map_err(Sht40Error::I2c)?;
        Timer::after(READ_DELAY).await;

        let mut raw = [0u8; 6];
        self.i2c
            .read(self.config.i2c_address, &mut raw)
            .map_err(Sht40Error::I2c)?;

        if crc8(&raw[0..2]) != raw[2] {
            return Err(Sht40Error::BadTemperatureCrc);
        }
        if crc8(&raw[3..5]) != raw[5] {
            return Err(Sht40Error::BadHumidityCrc);
        }

        let raw_temp = u16::from_be_bytes([raw[0], raw[1]]) as i32;
        let raw_humidity = u16::from_be_bytes([raw[3], raw[4]]) as i32;

        let temp_centi_c = ((-45_00i32) + ((175_00i32 * raw_temp) / 65_535)) as i16;
        let humidity = (-6_00i32) + ((125_00i32 * raw_humidity) / 65_535);
        let humidity_centi_percent = humidity.clamp(0, 10_000) as u16;

        Ok(EnvironmentSample {
            temp_centi_c,
            humidity_centi_percent,
        })
    }
}

#[derive(Debug)]
pub enum Sht40Error {
    I2c(esp_hal::i2c::master::Error),
    BadTemperatureCrc,
    BadHumidityCrc,
}

impl fmt::Display for Sht40Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::I2c(source) => write!(f, "I2C transfer failed: {source}"),
            Self::BadTemperatureCrc => f.write_str("temperature CRC check failed"),
            Self::BadHumidityCrc => f.write_str("humidity CRC check failed"),
        }
    }
}

pub fn crc8(bytes: &[u8]) -> u8 {
    let mut crc = 0xff;
    for byte in bytes {
        crc ^= *byte;
        for _ in 0..8 {
            if (crc & 0x80) != 0 {
                crc = (crc << 1) ^ 0x31;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}
