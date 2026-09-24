//! Toolchain sanity check: flash this, watch it log over USB serial, confirm
//! the BOOTSEL/elf2uf2-rs/USB-CDC pipeline works before touching `main.rs`.
//!
//! Uses GPIO2, which isn't wired to anything else in this project -- it
//! never touches STBY, AIN1, AIN2 or PWMA, so it can't affect the TB6612FNG
//! wiring no matter what this program does.
//!
//! cargo run --release --bin blink              # with USB serial logging
//! screen /dev/ttyACM0 115200   # Linux; /dev/tty.usbmodemXXXX on macOS

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::{Level, Output};
use embassy_time::{Duration, Timer};

#[cfg(feature = "usb-log")]
use embassy_rp::peripherals::USB;
#[cfg(feature = "usb-log")]
use embassy_rp::usb::{Driver as UsbDriver, InterruptHandler as UsbIrq};

// A panic resets the board rather than halting it, same reasoning as main.rs.
use panic_reset as _;

// Compiled out entirely without the `usb-log` feature, same as main.rs.
#[cfg(feature = "usb-log")]
macro_rules! telemetry {
    ($($arg:tt)*) => { log::info!($($arg)*) };
}
#[cfg(not(feature = "usb-log"))]
macro_rules! telemetry {
    ($($arg:tt)*) => { let _ = ($($arg)*); };
}

bind_interrupts!(struct Irqs {
    #[cfg(feature = "usb-log")]
    USBCTRL_IRQ => UsbIrq<USB>;
});

#[cfg(feature = "usb-log")]
#[embassy_executor::task]
async fn logger_task(driver: UsbDriver<'static, USB>) {
    embassy_usb_logger::run!(1024, log::LevelFilter::Info, driver);
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    // Non-fatal if the spawn slot isn't free, same as main.rs: this program
    // has nothing else to protect by failing loudly.
    #[cfg(feature = "usb-log")]
    if let Ok(token) = logger_task(UsbDriver::new(p.USB, Irqs)) {
        spawner.spawn(token);
    }
    #[cfg(not(feature = "usb-log"))]
    let _ = spawner;

    let mut led = Output::new(p.PIN_2, Level::Low);

    // Let the host finish enumerating the USB CDC port before the first log
    // line, or it's dropped and the first couple seconds look dead.
    Timer::after(Duration::from_secs(2)).await;

    let mut on = false;
    loop {
        on = !on;
        led.set_level(if on { Level::High } else { Level::Low });
        telemetry!("blink: {}", if on { "on" } else { "off" });
        Timer::after(Duration::from_millis(500)).await;
    }
}
