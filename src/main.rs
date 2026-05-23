#![no_std]
#![no_main]

use embassy_executor::Spawner;
use esp32s3_n16r8_rust::{AppError, AppResult, Display, TaskName, display_task, heartbeat_task};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    i2c::master::{Config as I2cConfig, I2c},
    interrupt::software::SoftwareInterruptControl,
    time::Rate,
    timer::timg::TimerGroup,
};
use esp_println::{logger::init_logger_from_env, println};

esp_bootloader_esp_idf::esp_app_desc!();

const OLED_SDA_PIN: u8 = 5;
const OLED_SCL_PIN: u8 = 4;

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    if let Err(error) = run(spawner).await {
        panic_on_fatal_error(error);
    }

    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(60)).await;
    }
}

async fn run(spawner: Spawner) -> AppResult<()> {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let software_interrupt = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, software_interrupt.software_interrupt0);

    init_logger_from_env();
    println!("Hello world from ESP32-S3 N16R8!");
    println!(
        "Initializing SSD1306 OLED over I2C: SDA=GPIO{}, SCL=GPIO{}",
        OLED_SDA_PIN, OLED_SCL_PIN
    );

    let i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .map_err(|source| AppError::I2cInit {
        bus: "I2C0",
        source,
    })?
    .with_sda(peripherals.GPIO5)
    .with_scl(peripherals.GPIO4);

    let display = Display::new(i2c)?;

    let heartbeat = heartbeat_task().map_err(|source| AppError::TaskSpawn {
        task: TaskName::Heartbeat,
        source,
    })?;
    spawner.spawn(heartbeat);

    let display = display_task(display).map_err(|source| AppError::TaskSpawn {
        task: TaskName::Display,
        source,
    })?;
    spawner.spawn(display);

    log::info!("application tasks started");

    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(60)).await;
    }
}

fn panic_on_fatal_error(error: AppError) -> ! {
    log::error!("fatal application error: {}", error);
    println!("FATAL application error: {}", error);
    panic!("fatal application error: {}", error);
}
