use crate::role::NodeRole;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinConfig {
    pub lora_uart_tx: u8,
    pub lora_uart_rx: u8,
    pub lora_m0: Option<u8>,
    pub lora_m1: Option<u8>,
    pub lora_aux: Option<u8>,
    pub lora_reset: Option<u8>,
    pub sht40_sda: Option<u8>,
    pub sht40_scl: Option<u8>,
    pub buzzer: Option<u8>,
}

impl PinConfig {
    pub const DEMO_DEFAULT: Self = Self {
        lora_uart_tx: 21,
        lora_uart_rx: 20,
        lora_m0: None,
        lora_m1: None,
        lora_aux: None,
        lora_reset: None,
        sht40_sda: Some(5),
        sht40_scl: Some(4),
        buzzer: Some(10),
    };

    pub const fn for_role(role: NodeRole) -> Self {
        match role {
            NodeRole::Gateway => Self {
                sht40_sda: None,
                sht40_scl: None,
                ..Self::DEMO_DEFAULT
            },
            NodeRole::Relay => Self {
                sht40_sda: None,
                sht40_scl: None,
                buzzer: None,
                ..Self::DEMO_DEFAULT
            },
            NodeRole::Sensor => Self {
                buzzer: None,
                ..Self::DEMO_DEFAULT
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoraUartConfig {
    pub baudrate: u32,
    pub channel: u8,
    pub air_rate_bps: u32,
    pub tx_power_dbm: u8,
    pub frequency_mhz: u16,
    pub net_id: u16,
}

impl LoraUartConfig {
    /// Defaults aligned with DX-LR32-433T22D factory settings:
    /// LEVEL=2 → 2148 bps, CHANNEL=00 → 433.15 MHz,
    /// baud=9600 8N1, power=22 dBm.
    pub const DEMO_DEFAULT: Self = Self {
        baudrate: 9_600,
        channel: 0,
        air_rate_bps: 2_148,
        tx_power_dbm: 22,
        frequency_mhz: 433,
        net_id: crate::role::NET_ID,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoraModuleConfigMode {
    /// Module is configured before boot via external tool; firmware only logs the plan.
    ManualBeforeBoot,
    /// Firmware sends AT commands to the module at boot to ensure correct settings.
    RuntimeAtCommands,
}

/// AT command sequence needed to prepare a DX-LR32 module for demo use.
///
/// All three modules share the same factory defaults, but the encryption key may
/// differ.  We disable key verification so any module can talk to any other.
/// AT command sequence — each entry is sent verbatim over UART.
/// `+++` is sent bare; AT commands include the required `\r\n` terminator
/// per the DX-LR32 manual §4.1.
pub const DX_LR32_DEMO_AT_SEQUENCE: &[&str] = &["+++", "AT+OPENKEY0\r\n", "+++"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoraModuleConfigPlan {
    pub module: &'static str,
    pub mode: LoraModuleConfigMode,
    pub uart: LoraUartConfig,
}

impl LoraModuleConfigPlan {
    pub const DX_LR32_DEMO: Self = Self {
        module: "DX-LR32-433T22D",
        mode: LoraModuleConfigMode::RuntimeAtCommands,
        uart: LoraUartConfig::DEMO_DEFAULT,
    };

    pub const fn mode_label(self) -> &'static str {
        match self.mode {
            LoraModuleConfigMode::ManualBeforeBoot => "manual-before-boot",
            LoraModuleConfigMode::RuntimeAtCommands => "runtime-at-commands",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sht40Config {
    pub i2c_address: u8,
    pub temp_alarm_centi_c: i16,
    pub humidity_alarm_centi_percent: u16,
    pub temp_clear_centi_c: i16,
    pub humidity_clear_centi_percent: u16,
}

impl Sht40Config {
    pub const DEFAULT: Self = Self {
        i2c_address: 0x44,
        temp_alarm_centi_c: 3_000,
        humidity_alarm_centi_percent: 8_000,
        temp_clear_centi_c: 2_900,
        humidity_clear_centi_percent: 7_500,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuzzerConfig {
    pub gpio: u8,
    pub active_low: bool,
}

impl BuzzerConfig {
    pub const DEFAULT: Self = Self {
        gpio: 10,
        active_low: true,
    };
}
