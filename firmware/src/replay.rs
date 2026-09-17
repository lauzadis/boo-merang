//! A compiled-in sample trace, substituted for the ADC under `--features
//! replay`.
//!
//! The point is to run the real firmware -- real `Ticker`, real detector, real
//! GPIO -- against a known input, so on-target timing can be compared against
//! what the host tests predict. With two LEDs on AIN1/AIN2 the reversals are
//! visible without any of the motor hardware present.
//!
//! The trace is one traverse: inrush, cruise, then into a stopper. It restarts
//! on each reversal, the way the real current does when the carriage backs off
//! the stopper.

/// 10ms per entry, so 400 entries is four seconds -- a deliberately short
/// traverse, to get several reversals a minute out of a bench run.
const LEN: usize = 400;

const TRACE: [u16; LEN] = build();

const fn build() -> [u16; LEN] {
    let mut t = [0u16; LEN];
    let mut i = 0;
    while i < LEN {
        t[i] = if i < 10 {
            // Inrush: 900 counts decaying to ~250 over the first 100ms. Should
            // be swallowed whole by the blanking window.
            900 - (65 * i) as u16
        } else if i < 300 {
            // Cruise, with a slow sag so the baseline has something to track.
            250 - (i as u16 - 10) / 20
        } else {
            // Into the stopper.
            900
        };
        i += 1;
    }
    t
}

pub struct Replay {
    idx: usize,
}

impl Replay {
    pub const fn new() -> Self {
        Self { idx: 0 }
    }

    /// Next sample. Holds the last value if the trace runs out, which only
    /// happens if a reversal never fires -- in which case the timeout will.
    pub fn next(&mut self) -> u16 {
        let v = TRACE[if self.idx < LEN { self.idx } else { LEN - 1 }];
        self.idx = self.idx.saturating_add(1);
        v
    }

    pub fn restart(&mut self) {
        self.idx = 0;
    }
}
