//! Flying ghost prop carriage controller.
//!
//! A brushed DC motor drags a carriage along a rope and reverses when it runs
//! into a stopper. All of the interesting logic lives in the `stall` crate and
//! is tested on the host; this binary is the thin layer that reads the ADC,
//! hands numbers to [`stall::Detector`], and applies what comes back to a
//! TB6612FNG.
//!
//! Build and flash (hold BOOTSEL while plugging in the Pico):
//!
//! ```text
//! cargo run --release                        # with USB CSV telemetry
//! cargo run --release --no-default-features  # deployed build, silent
//! cargo run --release --features replay      # replay a compiled-in trace
//! ```

#![no_std]
#![no_main]

use embassy_executor::Spawner;
#[cfg(not(feature = "replay"))]
use embassy_rp::adc::{Adc, Channel, Config as AdcConfig, InterruptHandler as AdcIrq};
use embassy_rp::bind_interrupts;
#[cfg(not(feature = "replay"))]
use embassy_rp::gpio::Pull;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::pwm::{Config as PwmConfig, Pwm, SetDutyCycle};
use embassy_time::{Duration, Instant, Ticker, Timer};
use stall::{Config, Detector, Direction, Tick};

// A panic reboots the board. See the note in Cargo.toml.
use panic_reset as _;

#[cfg(feature = "usb-log")]
use embassy_rp::peripherals::USB;
#[cfg(feature = "usb-log")]
use embassy_rp::usb::{Driver as UsbDriver, InterruptHandler as UsbIrq};

#[cfg(feature = "replay")]
mod replay;

// ---------------------------------------------------------------------------
// Telemetry. Compiled out entirely without the `usb-log` feature.
// ---------------------------------------------------------------------------

#[cfg(feature = "usb-log")]
macro_rules! telemetry {
    ($($arg:tt)*) => { log::info!($($arg)*) };
}
#[cfg(not(feature = "usb-log"))]
macro_rules! telemetry {
    ($($arg:tt)*) => { let _ = ($($arg)*); };
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------

/// Control loop period. `stall`'s time constants are in milliseconds, so this
/// only has to be fast enough to resolve `confirm_samples`; it is not itself a
/// tuning parameter.
const TICK: Duration = Duration::from_millis(10);

/// Coast time between directions. Long enough that the motor is no longer
/// turning before the H-bridge is asked to push it the other way -- slamming a
/// spinning brushed motor into reverse is how you get a current spike that
/// reads as a stall, or a dead driver.
const DWELL: Duration = Duration::from_millis(300);

/// Emit a telemetry row every N ticks. 10 gives 10 rows/second, which is
/// readable in a terminal and still resolves a stall edge.
const LOG_EVERY: u32 = 10;

/// ADC reads averaged per tick, on top of the hardware RC filter. Brushed
/// motors are electrically filthy; this is nearly free and costs no latency
/// worth caring about.
#[cfg(not(feature = "replay"))]
const OVERSAMPLE: u16 = 16;

/// Motor PWM frequency. Above audibility, below anything the TB6612FNG minds.
const PWM_HZ: u32 = 25_000;

/// Current sense resistor in the motor's ground return, between the driver's
/// GND pin and battery negative. PLACEHOLDER VALUE, pending measurement of
/// the motor's running and stall current. Everything downstream (the stall
/// ratio, the ADC headroom, whether 3xAA can even deliver the current)
/// depends on this number, so measure it before trusting any of it.
///
/// Not read anywhere in this file -- the detector's threshold is a ratio of
/// a self-tracking baseline, not an absolute current, so nothing here needs
/// to convert counts to amps for correctness. This constant exists purely so
/// there is one documented place to update when the physical resistor
/// changes, instead of a stale number scattered across comments.
#[allow(dead_code)]
const SENSE_RESISTOR_OHMS: f32 = 0.47;

// ---------------------------------------------------------------------------
// Hardware notes that are not expressible in code
// ---------------------------------------------------------------------------
//
//   At SENSE_RESISTOR_OHMS = 0.47: 1A -> 470mV -> ~583 counts of a 3.3V
//   12-bit ADC. Keep this comment's arithmetic in sync with the constant
//   above if the resistor changes.
//
// The sense line reaches the ADC through a 1k series resistor with 10uF to
// ground: ~10ms, which is one tick, so a step in current is ~63% visible on the
// next tick and settled within three or four.
//
// GROUND: the Pico's ground must connect to BATTERY NEGATIVE -- the far side of
// the sense resistor from the driver -- not to the driver's GND pin. Tie it to
// the driver side and the Pico's whole ground reference moves with motor
// current, and the ADC reading becomes meaningless.
//
// STBY wants a pulldown to ground. Between reset and the first line of main the
// GPIO is an input, and a floating STBY on a driver wired to a motor is not a
// state to leave to chance.

bind_interrupts!(struct Irqs {
    #[cfg(not(feature = "replay"))]
    ADC_IRQ_FIFO => AdcIrq;
    #[cfg(feature = "usb-log")]
    USBCTRL_IRQ => UsbIrq<USB>;
});

#[cfg(feature = "usb-log")]
#[embassy_executor::task]
async fn logger_task(driver: UsbDriver<'static, USB>) {
    embassy_usb_logger::run!(2048, log::LevelFilter::Info, driver);
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    // ===== PIN ASSIGNMENT ===================================================
    // The only place in this crate that names a GPIO. Change them here.
    //
    // The PWM slice must match the PWM pin: on RP2040, GPIO n is driven by
    // slice (n / 2) % 8, channel A when n is even and channel B when it is odd.
    // GPIO16 -> slice 0, channel A, hence `new_output_a` below.
    //
    // SENSE must be GPIO26, 27 or 28 (ADC channels 0, 1, 2).
    //
    //   PWMA  -> driver PWMA   (PWM_SLICE0, from PIN_16)
    //   AIN1  -> driver AIN1
    //   AIN2  -> driver AIN2
    //   STBY  -> driver STBY
    //   SENSE <- top of the sense resistor (ADC0)
    let pwm_slice = p.PWM_SLICE0;
    let pwm_pin = p.PIN_16;
    let ain1_pin = p.PIN_14;
    let ain2_pin = p.PIN_15;
    let stby_pin = p.PIN_13;
    let sense_pin = p.PIN_26;
    // =========================================================================

    #[cfg(feature = "usb-log")]
    // The task macro hands back a token only if a slot is free; without the
    // logger the prop still runs, so a failure here is logged nowhere and
    // deliberately not fatal.
    if let Ok(token) = logger_task(UsbDriver::new(p.USB, Irqs)) {
        spawner.spawn(token);
    }
    #[cfg(not(feature = "usb-log"))]
    let _ = spawner;

    let mut motor = Motor::new(pwm_slice, pwm_pin, ain1_pin, ain2_pin);
    let mut sense = SenseInput::new(p.ADC, sense_pin);

    // Direction pins are already low (coast) and duty is zero, so it is safe to
    // take the driver out of standby now.
    let mut stby = Output::new(stby_pin, Level::Low);
    stby.set_high();

    let cfg = Config::default();
    let mut detector = Detector::new(cfg, millis());

    telemetry!(
        "# ghost carriage: tick={}ms ratio={}% confirm={} ramp={}ms blank={}ms max_traverse={}ms shift={}",
        TICK.as_millis(),
        cfg.stall_ratio_pct,
        cfg.confirm_samples,
        cfg.ramp_ms,
        cfg.blank_ms,
        cfg.max_traverse_ms,
        cfg.baseline_shift
    );
    telemetry!("t_ms,sample,baseline,threshold,confirm,dir,duty");

    let mut ticker = Ticker::every(TICK);
    let mut n: u32 = 0;

    loop {
        ticker.next().await;
        n = n.wrapping_add(1);

        let sample = sense.read().await;
        let tick: Tick = detector.update(millis(), sample);

        if let Some(reason) = tick.reversed {
            telemetry!(
                "# reversal t={} reason={} new_dir={} sample={} baseline={} threshold={}",
                millis(),
                match reason {
                    stall::Reason::Stall => "stall",
                    stall::Reason::Timeout => "timeout",
                },
                dir_str(tick.dir),
                sample,
                detector.baseline_counts(),
                detector.threshold_counts()
            );

            // Coast, let the motor stop turning, and only then restart the
            // traverse clock -- otherwise the soft-start ramp burns its window
            // while the motor is still freewheeling and the motor jumps
            // straight to part throttle when the new direction is applied.
            motor.coast();
            sense.restart();
            Timer::after(DWELL).await;
            detector.resume(millis());
            ticker.reset();
        }

        motor.apply(tick.dir, tick.duty_permille);

        if n.is_multiple_of(LOG_EVERY) {
            telemetry!(
                "{},{},{},{},{},{},{}",
                millis(),
                sample,
                detector.baseline_counts(),
                detector.threshold_counts(),
                detector.confirm_count(),
                dir_str(tick.dir),
                tick.duty_permille
            );
        }
    }
}

/// Milliseconds since boot, wrapped into the `u32` the detector expects. The
/// detector handles the rollover (every 49.7 days); nothing here needs to.
fn millis() -> u32 {
    Instant::now().as_millis() as u32
}

fn dir_str(dir: Direction) -> &'static str {
    match dir {
        Direction::Forward => "fwd",
        Direction::Reverse => "rev",
    }
}

/// Channel A of the TB6612FNG.
struct Motor<'d> {
    pwm: Pwm<'d>,
    ain1: Output<'d>,
    ain2: Output<'d>,
}

impl<'d> Motor<'d> {
    fn new(
        slice: embassy_rp::Peri<'d, embassy_rp::peripherals::PWM_SLICE0>,
        pwm_pin: embassy_rp::Peri<'d, embassy_rp::peripherals::PIN_16>,
        ain1_pin: embassy_rp::Peri<'d, embassy_rp::peripherals::PIN_14>,
        ain2_pin: embassy_rp::Peri<'d, embassy_rp::peripherals::PIN_15>,
    ) -> Self {
        let mut cfg = PwmConfig::default();
        // top is the counter wrap, so period = (top + 1) / clk with divider 1.
        cfg.top = (embassy_rp::clocks::clk_sys_freq() / PWM_HZ).saturating_sub(1) as u16;
        let pwm = Pwm::new_output_a(slice, pwm_pin, cfg);

        let mut motor = Self {
            pwm,
            // Both low is coast, which is where we want to start.
            ain1: Output::new(ain1_pin, Level::Low),
            ain2: Output::new(ain2_pin, Level::Low),
        };
        motor.coast();
        motor
    }

    /// Drives `dir` at `duty_permille` (0..=1000).
    fn apply(&mut self, dir: Direction, duty_permille: u16) {
        match dir {
            // AIN1=1, AIN2=0 forward; AIN1=0, AIN2=1 reverse. Both high would
            // be brake, which this firmware never asks for.
            Direction::Forward => {
                self.ain2.set_low();
                self.ain1.set_high();
            }
            Direction::Reverse => {
                self.ain1.set_low();
                self.ain2.set_high();
            }
        }
        self.set_duty(duty_permille);
    }

    /// Both direction pins low: the H-bridge floats and the motor freewheels.
    fn coast(&mut self) {
        self.ain1.set_low();
        self.ain2.set_low();
        self.set_duty(0);
    }

    fn set_duty(&mut self, permille: u16) {
        // Infallible for num <= denom, which `stall` guarantees; ignored rather
        // than unwrapped because this runs unattended and a panic here would be
        // a worse outcome than a tick at the previous duty.
        let _ = self.pwm.set_duty_cycle_fraction(permille.min(1000), 1000);
    }
}

/// The sense resistor as seen by the ADC -- or, under `--features replay`, a
/// compiled-in trace, so the same binary can be timed on-target without the
/// motor, the driver or the sense resistor being present.
struct SenseInput<'d> {
    #[cfg(not(feature = "replay"))]
    adc: Adc<'d, embassy_rp::adc::Async>,
    #[cfg(not(feature = "replay"))]
    channel: Channel<'d>,
    #[cfg(feature = "replay")]
    trace: replay::Replay,
    #[cfg(feature = "replay")]
    _marker: core::marker::PhantomData<&'d ()>,
}

impl<'d> SenseInput<'d> {
    #[cfg(not(feature = "replay"))]
    fn new(
        adc: embassy_rp::Peri<'d, embassy_rp::peripherals::ADC>,
        pin: embassy_rp::Peri<'d, embassy_rp::peripherals::PIN_26>,
    ) -> Self {
        Self {
            adc: Adc::new(adc, Irqs, AdcConfig::default()),
            // No pull: the RC filter and the sense resistor set the level.
            channel: Channel::new_pin(pin, Pull::None),
        }
    }

    #[cfg(feature = "replay")]
    fn new(
        _adc: embassy_rp::Peri<'d, embassy_rp::peripherals::ADC>,
        _pin: embassy_rp::Peri<'d, embassy_rp::peripherals::PIN_26>,
    ) -> Self {
        Self {
            trace: replay::Replay::new(),
            _marker: core::marker::PhantomData,
        }
    }

    /// One averaged reading.
    #[cfg(not(feature = "replay"))]
    async fn read(&mut self) -> u16 {
        let mut sum: u32 = 0;
        let mut taken: u32 = 0;
        for _ in 0..OVERSAMPLE {
            // A failed conversion is dropped rather than unwrapped; if every
            // read in a tick fails we report 0, which reads as "no current"
            // and is handled by `min_baseline_counts`.
            if let Ok(v) = self.adc.read(&mut self.channel).await {
                sum += u32::from(v);
                taken += 1;
            }
        }
        if taken == 0 {
            0
        } else {
            (sum / taken) as u16
        }
    }

    #[cfg(feature = "replay")]
    async fn read(&mut self) -> u16 {
        self.trace.next()
    }

    /// Called on a reversal so a replayed traverse starts from the top.
    fn restart(&mut self) {
        #[cfg(feature = "replay")]
        self.trace.restart();
    }
}
