// Stay `no_std` on the embedded target; on the host (unit tests) keep `std` so
// the default test harness links.
#![cfg_attr(target_os = "none", no_std)]

pub mod demo;
pub mod demo_log;
pub mod protocol;
pub mod relay;
pub mod role;
pub mod tdma;

// HAL-bound modules only exist on the embedded target.
#[cfg(target_os = "none")]
pub mod hardware;
#[cfg(target_os = "none")]
pub mod sensors;
#[cfg(target_os = "none")]
pub mod transport;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug)]
pub enum AppError {
    InvalidRoleFeatureSet,
    FrameEncode(protocol::EncodeError),
    #[cfg(target_os = "none")]
    UartConfig(esp_hal::uart::ConfigError),
    #[cfg(target_os = "none")]
    UartTx(esp_hal::uart::TxError),
    #[cfg(target_os = "none")]
    UartRx(esp_hal::uart::RxError),
    #[cfg(target_os = "none")]
    I2cConfig(esp_hal::i2c::master::ConfigError),
    #[cfg(target_os = "none")]
    I2c(esp_hal::i2c::master::Error),
    #[cfg(target_os = "none")]
    Sht40(sensors::Sht40Error),
    #[cfg(target_os = "none")]
    LoraTransport(transport::LoraUartError),
}

impl core::fmt::Display for AppError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AppError::InvalidRoleFeatureSet => f.write_str(
                "enable exactly one role feature: sensor-node, relay-node, or gateway-node",
            ),
            AppError::FrameEncode(source) => write!(f, "frame encode failed: {source}"),
            #[cfg(target_os = "none")]
            AppError::UartConfig(source) => write!(f, "LoRa UART config failed: {source}"),
            #[cfg(target_os = "none")]
            AppError::UartTx(source) => write!(f, "LoRa UART transmit failed: {source}"),
            #[cfg(target_os = "none")]
            AppError::UartRx(source) => write!(f, "LoRa UART receive failed: {source}"),
            #[cfg(target_os = "none")]
            AppError::I2cConfig(source) => write!(f, "I2C config failed: {source}"),
            #[cfg(target_os = "none")]
            AppError::I2c(source) => write!(f, "I2C transfer failed: {source}"),
            #[cfg(target_os = "none")]
            AppError::Sht40(source) => write!(f, "SHT40 read failed: {source}"),
            #[cfg(target_os = "none")]
            AppError::LoraTransport(source) => write!(f, "LoRa transport failed: {source}"),
        }
    }
}

impl From<protocol::EncodeError> for AppError {
    fn from(source: protocol::EncodeError) -> Self {
        Self::FrameEncode(source)
    }
}

#[cfg(target_os = "none")]
impl From<esp_hal::uart::ConfigError> for AppError {
    fn from(source: esp_hal::uart::ConfigError) -> Self {
        Self::UartConfig(source)
    }
}

#[cfg(target_os = "none")]
impl From<esp_hal::uart::TxError> for AppError {
    fn from(source: esp_hal::uart::TxError) -> Self {
        Self::UartTx(source)
    }
}

#[cfg(target_os = "none")]
impl From<esp_hal::uart::RxError> for AppError {
    fn from(source: esp_hal::uart::RxError) -> Self {
        Self::UartRx(source)
    }
}

#[cfg(target_os = "none")]
impl From<esp_hal::i2c::master::ConfigError> for AppError {
    fn from(source: esp_hal::i2c::master::ConfigError) -> Self {
        Self::I2cConfig(source)
    }
}

#[cfg(target_os = "none")]
impl From<esp_hal::i2c::master::Error> for AppError {
    fn from(source: esp_hal::i2c::master::Error) -> Self {
        Self::I2c(source)
    }
}

#[cfg(target_os = "none")]
impl From<sensors::Sht40Error> for AppError {
    fn from(source: sensors::Sht40Error) -> Self {
        Self::Sht40(source)
    }
}

#[cfg(target_os = "none")]
impl From<transport::LoraUartError> for AppError {
    fn from(source: transport::LoraUartError) -> Self {
        Self::LoraTransport(source)
    }
}
