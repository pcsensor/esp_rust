#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::{Duration, Timer};
use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii::FONT_6X10},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    i2c::master::{Config as I2cConfig, I2c},
    interrupt::software::SoftwareInterruptControl,
    time::Rate,
    timer::timg::TimerGroup,
};
use esp_println::{logger::init_logger_from_env, println};
use ssd1306::{I2CDisplayInterface, Ssd1306, prelude::*};

esp_bootloader_esp_idf::esp_app_desc!();

const OLED_SDA_PIN: u8 = 5;
const OLED_SCL_PIN: u8 = 4;

static HEARTBEAT_SIGNAL: Signal<CriticalSectionRawMutex, Heartbeat> = Signal::new();

#[derive(Clone, Copy)]
struct Heartbeat {
    sequence: u32,
}

#[embassy_executor::task]
async fn heartbeat_task() {
    let mut sequence = 0;

    loop {
        sequence += 1;
        HEARTBEAT_SIGNAL.signal(Heartbeat { sequence });
        Timer::after(Duration::from_secs(1)).await;
    }
}

#[embassy_executor::task]
async fn serial_log_task() {
    loop {
        let heartbeat = HEARTBEAT_SIGNAL.wait().await;
        log::info!("serial task received heartbeat #{}\n", heartbeat.sequence);
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
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
    .expect("failed to initialize I2C0")
    .with_sda(peripherals.GPIO5)
    .with_scl(peripherals.GPIO4);

    let interface = I2CDisplayInterface::new(i2c);
    let mut display = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
        .into_buffered_graphics_mode();

    display.init().expect("failed to initialize SSD1306");
    display
        .clear(BinaryColor::Off)
        .expect("failed to clear OLED");

    let text_style = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    Text::with_baseline("ESP32-S3", Point::new(0, 0), text_style, Baseline::Top)
        .draw(&mut display)
        .expect("failed to draw title");
    Text::with_baseline("SSD1306 OLED", Point::new(0, 16), text_style, Baseline::Top)
        .draw(&mut display)
        .expect("failed to draw OLED line");
    Text::with_baseline(
        "Rust + esp-hal",
        Point::new(0, 32),
        text_style,
        Baseline::Top,
    )
    .draw(&mut display)
    .expect("failed to draw HAL line");
    display.flush().expect("failed to flush OLED");

    spawner.spawn(heartbeat_task().expect("failed to create heartbeat task"));
    spawner.spawn(serial_log_task().expect("failed to create serial log task"));

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
