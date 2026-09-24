//! Stall detection and direction sequencing for a rope-traversing prop carriage.
//!
//! The carriage runs a brushed DC motor along a rope and reverses when it hits a
//! physical stopper. A stopper shows up electrically as a stall: the motor stops
//! turning, back-EMF collapses, and current through the sense resistor jumps.
//!
//! This crate is pure logic. It takes a timestamp and an ADC reading and returns
//! the motor state the caller should apply. It has no hardware dependencies, does
//! not allocate, does not panic, and is exercised entirely by host-side tests.
//!
//! ```
//! use stall::{Config, Detector};
//!
//! let mut det = Detector::new(Config::default(), 0);
//! let tick = det.update(10, 250);
//! assert!(tick.reversed.is_none());
//! ```

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

/// Which way the carriage is travelling.
///
/// `Forward` is the direction the carriage starts in at power-on; which end of
/// the rope that corresponds to is a wiring detail, not something this crate
/// knows or cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Reverse,
}

impl Direction {
    /// The opposite direction.
    #[must_use]
    pub const fn flipped(self) -> Self {
        match self {
            Direction::Forward => Direction::Reverse,
            Direction::Reverse => Direction::Forward,
        }
    }
}

/// Why a reversal happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Sense current stayed above threshold for `confirm_samples` consecutive
    /// samples: the carriage hit a stopper.
    Stall,
    /// `max_traverse_ms` elapsed without a stall. The safety net — see the field
    /// docs. Seeing these in a log means something is wrong.
    Timeout,
}

/// Tuning parameters.
///
/// Every value here is provisional until somebody measures the motor's actual
/// running and stall current — see [`Config::default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Soft-start ramp length. Duty rises linearly from 0 to
    /// `cruise_duty_permille` across this window at the start of every traverse.
    ///
    /// A stationary motor presents only its winding resistance and draws a large
    /// inrush spike that looks exactly like a stall. Ramping mostly prevents the
    /// spike from forming in the first place.
    pub ramp_ms: u32,
    /// Detection is suppressed and the baseline is not updated for this long
    /// after a reversal, so whatever inrush survives the ramp is ignored.
    ///
    /// Must be `>= ramp_ms`; [`Detector::new`] raises it if it is not.
    pub blank_ms: u32,
    /// Trigger threshold as a percentage of the baseline, e.g. 250 for 2.5x.
    pub stall_ratio_pct: u32,
    /// Consecutive over-threshold samples required to call it a stall. Brushed
    /// motors are electrically noisy; a single spike must not trigger. Treated
    /// as 1 if set to 0.
    pub confirm_samples: u16,
    /// Safety net: reverse anyway after this long in one direction.
    ///
    /// Nobody is on site. If the carriage slips past a stopper, this keeps it
    /// moving instead of grinding into the rope end all night.
    pub max_traverse_ms: u32,
    /// EMA smoothing factor for the baseline, as a right-shift: the time
    /// constant is `2^baseline_shift` samples. At the suggested 10ms tick,
    /// shift 12 is roughly 41 seconds.
    ///
    /// This is not a free parameter. See [`Config::default`].
    pub baseline_shift: u8,
    /// Floor under the baseline so the threshold cannot collapse toward zero if
    /// the sense line reads near-nothing (motor disconnected, sense resistor
    /// shorted) and turn every sample into a stall.
    pub min_baseline_counts: u32,
    /// Normal running speed, 0..=1000. Clamped to 1000.
    pub cruise_duty_permille: u16,
}

impl Default for Config {
    /// MOSTLY PROVISIONAL. `min_baseline_counts` is still a guess.
    /// `stall_ratio_pct` is measured, but sitting in a genuinely narrow
    /// window -- read on before touching it.
    ///
    /// This motor/battery/sense-resistor combination does not produce much
    /// daylight between running and stalled current. `stall_ratio_pct` went
    /// 250 (placeholder) -> 140 -> 115 -> 108, chasing real stall captures
    /// that kept coming in lower than expected (`VM` shares a node with the
    /// Pico's `VSYS`, and USB backfeeds that node to ~4.6-5V through the
    /// onboard VBUS->VSYS diode when plugged in for logging, so USB-powered
    /// captures run hotter than true battery-only current -- but even
    /// accounting for that, a firm no-slip stall in `stall_test5.log` peaked
    /// at only ~113.6% of baseline). 108 turned out to be inside the noise
    /// floor: `steady_state_does_not_reverse` (60s of realistic ±6% noise)
    /// started firing false reversals at 108-109%, and only cleared at 110%.
    /// So the entire usable window, measured tonight, is roughly
    /// **110-113%** -- about 3 points wide. 112 sits in the middle of it on
    /// purpose, rather than hugging either edge.
    ///
    /// A window this narrow is a real property of this hardware, not a
    /// tuning mistake, and it deserves attention beyond just picking a
    /// number inside it:
    /// - It was only measured from a handful of hand-stall captures under
    ///   USB power. More real battery-only measurements (a data-only USB
    ///   cable would let logging happen without contaminating `VM`) would
    ///   firm this up considerably.
    /// - More ADC oversampling (`OVERSAMPLE` in `firmware/src/main.rs`,
    ///   currently 16) is worth trying -- real captured noise (~±2%) was
    ///   already tighter than this test's ±6% synthetic model, so there may
    ///   be more margin available for free.
    /// - The motor has two extra leads (see teardown notes) that look like a
    ///   shaft-rotation sensor, unused by this design. If this margin proves
    ///   too thin in practice, that's a plausible path to a second, current-
    ///   independent stall signal rather than squeezing this one further.
    /// - The consequence of a false trigger (an early reversal) is much
    ///   milder than a missed stall (grinding against the rope stopper for
    ///   up to `max_traverse_ms`), which is part of why erring toward the
    ///   lower end of the window is reasonable for this application.
    ///
    /// STALE INPUT WARNING: 112 was measured with `cruise_duty_permille` at
    /// 700. It has since been raised to 1000 (the stock board ran noticeably
    /// faster, per field feedback), which raises motor speed and therefore
    /// back-EMF at stall-adjacent RPM -- the measured ratio and the 110-113%
    /// window above are not re-validated against the new duty and may have
    /// shifted. Needs a fresh CSV capture before this is trusted again.
    ///
    /// `baseline_shift` is the one value here that is *not* a free guess. The
    /// baseline chases the sample, so a stall only fires if current rises faster
    /// than the baseline can follow. For the "slow ramp to 900 counts over 10s
    /// must still fire" case, the baseline's time constant has to be long
    /// relative to that ramp: at a 10ms tick, shift 12 (~41s) fires around 7.5s
    /// into the ramp, while shift 9 (~5s) never fires at all — the baseline
    /// simply follows the current up and the threshold runs away ahead of it.
    /// Battery sag happens over an evening and the baseline re-seeds every
    /// traverse anyway, so there is no cost to the long time constant.
    /// Do not lower this without re-running the slow-ramp test.
    fn default() -> Self {
        Self {
            ramp_ms: 500,
            blank_ms: 800,
            stall_ratio_pct: 112,
            confirm_samples: 5,
            max_traverse_ms: 300_000,
            baseline_shift: 12,
            min_baseline_counts: 40,
            cruise_duty_permille: 1000,
        }
    }
}

impl Config {
    /// Returns the config with out-of-range values pulled into range.
    ///
    /// Clamping rather than rejecting is deliberate: this thing runs unattended
    /// for hours and a config mistake should degrade, not panic.
    #[must_use]
    pub fn sanitized(mut self) -> Self {
        // Detection must not start before the soft-start ramp has finished.
        if self.blank_ms < self.ramp_ms {
            self.blank_ms = self.ramp_ms;
        }
        if self.cruise_duty_permille > 1000 {
            self.cruise_duty_permille = 1000;
        }
        if self.confirm_samples == 0 {
            self.confirm_samples = 1;
        }
        // Beyond 24 the Q8 baseline shifts off the top of a u32.
        if self.baseline_shift > 16 {
            self.baseline_shift = 16;
        }
        self
    }
}

/// What the caller should do with the motor this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tick {
    /// Direction to drive. On a reversal tick this is already the *new*
    /// direction.
    pub dir: Direction,
    /// Duty cycle to apply, 0..=1000.
    pub duty_permille: u16,
    /// `Some(..)` only on the tick where a reversal fired.
    pub reversed: Option<Reason>,
}

/// Baseline and threshold are carried in Q8 fixed point: 8 fractional bits.
const Q8: u32 = 8;

/// Stall detector and direction sequencer.
///
/// Drive it by calling [`Detector::update`] on a fixed cadence with a
/// millisecond timestamp and an ADC reading.
#[derive(Debug, Clone)]
pub struct Detector {
    cfg: Config,
    dir: Direction,
    /// Start of the current traverse. Reset on every reversal.
    phase_start_ms: u32,
    /// EMA of the sense reading, Q8.
    baseline_q8: u32,
    /// Cleared on reversal; the first post-blanking sample is taken as the
    /// baseline outright rather than eased into from whatever was there before.
    baseline_seeded: bool,
    /// Consecutive over-threshold samples so far.
    confirm: u16,
}

impl Detector {
    /// Creates a detector travelling [`Direction::Forward`], with its first
    /// traverse starting at `now_ms`.
    #[must_use]
    pub fn new(cfg: Config, now_ms: u32) -> Self {
        Self {
            cfg: cfg.sanitized(),
            dir: Direction::Forward,
            phase_start_ms: now_ms,
            baseline_q8: 0,
            baseline_seeded: false,
            confirm: 0,
        }
    }

    /// Advances the state machine by one sample.
    ///
    /// Pure state transition: same inputs, same outputs, no globals, no panics
    /// on any input. `now_ms` is free to wrap.
    pub fn update(&mut self, now_ms: u32, sample_counts: u16) -> Tick {
        let elapsed = now_ms.wrapping_sub(self.phase_start_ms);

        // Safety net first, so a pathological config (blanking longer than the
        // traverse limit, say) can't disable it.
        if elapsed >= self.cfg.max_traverse_ms {
            return self.reverse(now_ms, Reason::Timeout);
        }

        if elapsed < self.cfg.blank_ms {
            // Blanked: no detection, and critically no baseline update either.
            // Folding inrush into the baseline would park the threshold far too
            // high for the rest of the traverse.
            return self.tick(elapsed, None);
        }

        let sample_q8 = u32::from(sample_counts) << Q8;

        if !self.baseline_seeded {
            self.baseline_q8 = sample_q8;
            self.baseline_seeded = true;
        }

        if sample_q8 > self.threshold_q8() {
            // Over threshold. Do NOT fold this sample into the baseline: if
            // stall samples move the baseline, the baseline chases the stall,
            // the threshold runs away ahead of the current, and the detector
            // never fires. This single `if` is the whole trick.
            self.confirm = self.confirm.saturating_add(1);
            if self.confirm >= self.cfg.confirm_samples {
                return self.reverse(now_ms, Reason::Stall);
            }
        } else {
            self.confirm = 0;
            self.fold_baseline(sample_q8);
        }

        self.tick(elapsed, None)
    }

    /// Restarts the traverse clock at `now_ms` without changing direction.
    ///
    /// Call this after the coast dwell that follows a reversal. The dwell sits
    /// between [`update`](Self::update) returning `reversed: Some(..)` and the
    /// motor actually starting to turn; without this the soft-start ramp and the
    /// blanking window would be measured from the start of the dwell and the
    /// motor would jump straight to part-throttle when it finally engages.
    pub fn resume(&mut self, now_ms: u32) {
        self.phase_start_ms = now_ms;
    }

    /// Current travel direction.
    #[must_use]
    pub fn direction(&self) -> Direction {
        self.dir
    }

    /// Current baseline, in ADC counts. For logging.
    #[must_use]
    pub fn baseline_counts(&self) -> u16 {
        (self.baseline_q8 >> Q8) as u16
    }

    /// Current trigger threshold, in ADC counts. For logging.
    #[must_use]
    pub fn threshold_counts(&self) -> u16 {
        // The threshold can legitimately exceed the ADC's 12-bit range, which
        // just means "cannot fire"; saturate rather than wrap so the log reads
        // sensibly.
        (self.threshold_q8() >> Q8).min(u32::from(u16::MAX)) as u16
    }

    /// Consecutive over-threshold samples so far. For logging.
    #[must_use]
    pub fn confirm_count(&self) -> u16 {
        self.confirm
    }

    /// The config in use, after sanitizing.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// `max(baseline, min_baseline_counts) * stall_ratio_pct / 100`, Q8.
    ///
    /// Comparing against a moving baseline rather than a fixed number is the
    /// point: battery voltage sags over the evening and friction changes with
    /// temperature, and nobody will be on site to retune a constant.
    fn threshold_q8(&self) -> u32 {
        let floor_q8 = self.cfg.min_baseline_counts.saturating_mul(1 << Q8);
        let base_q8 = self.baseline_q8.max(floor_q8);
        // Widened before the multiply: at 12-bit counts this fits u32, but the
        // config is not range-checked and overflow here would read as a stall.
        let scaled = u64::from(base_q8) * u64::from(self.cfg.stall_ratio_pct) / 100;
        scaled.min(u64::from(u32::MAX)) as u32
    }

    /// EMA step, by shift rather than divide.
    fn fold_baseline(&mut self, sample_q8: u32) {
        let shift = self.cfg.baseline_shift;
        self.baseline_q8 = self
            .baseline_q8
            .saturating_sub(self.baseline_q8 >> shift)
            .saturating_add(sample_q8 >> shift);
    }

    /// Duty for a given point in the traverse.
    fn duty_permille(&self, elapsed: u32) -> u16 {
        let cruise = self.cfg.cruise_duty_permille;
        if elapsed >= self.cfg.ramp_ms || self.cfg.ramp_ms == 0 {
            return cruise;
        }
        let ramped = u64::from(cruise) * u64::from(elapsed) / u64::from(self.cfg.ramp_ms);
        ramped as u16
    }

    fn tick(&self, elapsed: u32, reversed: Option<Reason>) -> Tick {
        Tick {
            dir: self.dir,
            duty_permille: self.duty_permille(elapsed),
            reversed,
        }
    }

    /// Flips direction and restarts the traverse.
    ///
    /// Any dwell/coast between directions is the caller's job — see
    /// [`resume`](Self::resume).
    fn reverse(&mut self, now_ms: u32, reason: Reason) -> Tick {
        self.dir = self.dir.flipped();
        self.phase_start_ms = now_ms;
        self.confirm = 0;
        self.baseline_seeded = false;
        self.tick(0, Some(reason))
    }
}
