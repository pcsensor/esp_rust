#![no_std]

pub mod demo;
pub mod hardware;
pub mod protocol;
pub mod role;
pub mod sensors;
pub mod tdma;
pub mod transport;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug)]
pub enum AppError {
    InvalidRoleFeatureSet,
    FrameEncode(protocol::EncodeError),
    UartConfig(esp_hal::uart::ConfigError),
    UartTx(esp_hal::uart::TxError),
    UartRx(esp_hal::uart::RxError),
    I2cConfig(esp_hal::i2c::master::ConfigError),
    I2c(esp_hal::i2c::master::Error),
    Sht40(sensors::Sht40Error),
    LoraTransport(transport::LoraUartError),
}

impl core::fmt::Display for AppError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AppError::InvalidRoleFeatureSet => f.write_str(
                "enable exactly one role feature: sensor-node, relay-node, or gateway-node",
            ),
            AppError::FrameEncode(source) => write!(f, "frame encode failed: {source}"),
            AppError::UartConfig(source) => write!(f, "LoRa UART config failed: {source}"),
            AppError::UartTx(source) => write!(f, "LoRa UART transmit failed: {source}"),
            AppError::UartRx(source) => write!(f, "LoRa UART receive failed: {source}"),
            AppError::I2cConfig(source) => write!(f, "I2C config failed: {source}"),
            AppError::I2c(source) => write!(f, "I2C transfer failed: {source}"),
            AppError::Sht40(source) => write!(f, "SHT40 read failed: {source}"),
            AppError::LoraTransport(source) => write!(f, "LoRa transport failed: {source}"),
        }
    }
}

impl From<protocol::EncodeError> for AppError {
    fn from(source: protocol::EncodeError) -> Self {
        Self::FrameEncode(source)
    }
}

impl From<esp_hal::uart::ConfigError> for AppError {
    fn from(source: esp_hal::uart::ConfigError) -> Self {
        Self::UartConfig(source)
    }
}

impl From<esp_hal::uart::TxError> for AppError {
    fn from(source: esp_hal::uart::TxError) -> Self {
        Self::UartTx(source)
    }
}

impl From<esp_hal::uart::RxError> for AppError {
    fn from(source: esp_hal::uart::RxError) -> Self {
        Self::UartRx(source)
    }
}

impl From<esp_hal::i2c::master::ConfigError> for AppError {
    fn from(source: esp_hal::i2c::master::ConfigError) -> Self {
        Self::I2cConfig(source)
    }
}

impl From<esp_hal::i2c::master::Error> for AppError {
    fn from(source: esp_hal::i2c::master::Error) -> Self {
        Self::I2c(source)
    }
}

impl From<sensors::Sht40Error> for AppError {
    fn from(source: sensors::Sht40Error) -> Self {
        Self::Sht40(source)
    }
}

impl From<transport::LoraUartError> for AppError {
    fn from(source: transport::LoraUartError) -> Self {
        Self::LoraTransport(source)
    }
}
