//! Host-side tests: synthetic sample traces pushed through the detector.
//!
//! Counts below are 12-bit ADC counts. The numbers (250 steady, 900 stalled) are
//! the brief's placeholders, not measurements — the tests care about the shape of
//! the trace, not its absolute scale.

use stall::{Config, Detector, Direction, Reason, Tick};

const TICK_MS: u32 = 10;

/// A reversal, as observed by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Event {
    /// Milliseconds since the start of the trace.
    at_ms: u32,
    reason: Reason,
    /// Direction the detector switched *to*.
    new_dir: Direction,
}

struct Trace {
    events: Vec<Event>,
    ticks: Vec<Tick>,
    /// The detector as it stood at the end of the trace, so tests can assert on
    /// the baseline and threshold it actually arrived at.
    det: Detector,
}

/// Runs an open-loop trace: `sample(elapsed_ms)` supplies each reading.
fn run<F>(cfg: Config, start_ms: u32, duration_ms: u32, mut sample: F) -> Trace
where
    F: FnMut(u32) -> u16,
{
    let mut det = Detector::new(cfg, start_ms);
    let mut events = Vec::new();
    let mut ticks = Vec::new();
    let mut elapsed = 0u32;
    while elapsed <= duration_ms {
        let now = start_ms.wrapping_add(elapsed);
        let t = det.update(now, sample(elapsed));
        if let Some(reason) = t.reversed {
            events.push(Event {
                at_ms: elapsed,
                reason,
                new_dir: t.dir,
            });
        }
        ticks.push(t);
        elapsed += TICK_MS;
    }
    Trace { events, ticks, det }
}

/// Runs a closed-loop trace: `sample(ms_since_last_reversal)` supplies each
/// reading, so the simulated carriage leaves the stopper when the detector
/// reverses, the way the real one does.
fn run_reactive<F>(cfg: Config, duration_ms: u32, mut sample: F) -> Trace
where
    F: FnMut(u32) -> u16,
{
    let mut det = Detector::new(cfg, 0);
    let mut events = Vec::new();
    let mut ticks = Vec::new();
    let mut elapsed = 0u32;
    let mut last_reversal = 0u32;
    while elapsed <= duration_ms {
        let t = det.update(elapsed, sample(elapsed - last_reversal));
        if let Some(reason) = t.reversed {
            events.push(Event {
                at_ms: elapsed,
                reason,
                new_dir: t.dir,
            });
            last_reversal = elapsed;
        }
        ticks.push(t);
        elapsed += TICK_MS;
    }
    Trace { events, ticks, det }
}

/// Deterministic stand-in for brush noise, in `[-amp, amp]`.
fn noise(seed: &mut u32, amp: i32) -> i32 {
    *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    let span = amp * 2 + 1;
    ((*seed >> 16) as i32) % span - amp
}

fn steady(seed: &mut u32, level: i32, amp: i32) -> u16 {
    (level + noise(seed, amp)).clamp(0, 4095) as u16
}

/// A config whose traverse limit is far enough out not to interfere with tests
/// that are about something other than the timeout.
fn no_timeout() -> Config {
    Config {
        max_traverse_ms: u32::MAX / 2,
        ..Config::default()
    }
}

/// 1. Inrush is ignored: it lands inside the blanking window.
#[test]
fn inrush_does_not_reverse() {
    let t = run(no_timeout(), 0, 5_000, |ms| {
        if ms < 100 {
            // 900 decaying to 250 across the first 100ms.
            (900 - (650 * ms / 100)) as u16
        } else {
            250
        }
    });
    assert_eq!(t.events, vec![], "inrush spike must not reverse");
}

/// 1b. Inrush is ignored *and* does not poison the baseline.
///
/// Suppressing the reversal is only half the job. If the inrush spike is folded
/// into the baseline — or seeds it — the threshold parks at 2.5x the spike and
/// the detector is blind for the rest of the traverse without ever looking
/// broken.
#[test]
fn inrush_does_not_poison_the_baseline() {
    let stall_at = 3_000;
    let t = run(no_timeout(), 0, 6_000, |ms| {
        if ms < 100 {
            (900 - (650 * ms / 100)) as u16
        } else if ms < stall_at {
            250
        } else {
            900
        }
    });
    let stalls: Vec<_> = t.events.iter().filter(|e| e.at_ms >= stall_at).collect();
    assert_eq!(
        stalls.len(),
        1,
        "a real stall after the inrush must still fire: {:?}",
        t.events
    );
    assert!(
        t.events.iter().all(|e| e.at_ms >= stall_at),
        "nothing should fire before the real stall: {:?}",
        t.events
    );
}

/// 2. Steady state is quiet.
#[test]
fn steady_state_does_not_reverse() {
    let mut seed = 1;
    let t = run(no_timeout(), 0, 60_000, |_| steady(&mut seed, 250, 15));
    assert_eq!(t.events, vec![], "60s of steady running must not reverse");
}

/// 3. A stall fires, promptly.
#[test]
fn stall_fires_once() {
    let mut seed = 7;
    let stall_at = 5_000;
    let t = run(no_timeout(), 0, 8_000, |ms| {
        if ms < stall_at {
            steady(&mut seed, 250, 15)
        } else {
            // Hitting a stopper: current rises over ~50ms and stays up.
            let r = ((ms - stall_at) * 13).min(650);
            (250 + r) as u16
        }
    });
    assert_eq!(
        t.events.len(),
        1,
        "expected exactly one reversal: {:?}",
        t.events
    );
    let ev = t.events[0];
    assert_eq!(ev.reason, Reason::Stall);
    assert_eq!(ev.new_dir, Direction::Reverse);
    // Threshold sits at 2.5 x 250 = 625 counts, crossed ~30ms into the rise;
    // confirm_samples then costs 5 more ticks.
    let latest = stall_at + 50 + (u32::from(Config::default().confirm_samples) + 2) * TICK_MS;
    assert!(
        (stall_at..=latest).contains(&ev.at_ms),
        "reversal at {}ms, expected within {}..={}",
        ev.at_ms,
        stall_at,
        latest
    );
}

/// 4. Single spikes are rejected.
#[test]
fn isolated_spikes_are_rejected() {
    let mut seed = 42;
    let t = run(no_timeout(), 0, 60_000, |ms| {
        if ms >= 2_000 && ms % 500 == 0 {
            900
        } else {
            steady(&mut seed, 250, 15)
        }
    });
    assert_eq!(t.events, vec![], "one-sample spikes must not reverse");
}

/// 5. Baseline tracks battery sag. This is the test that catches a hardcoded
///    threshold.
#[test]
fn baseline_follows_battery_sag() {
    let mut seed = 11;
    let sag_ms = 300_000;
    let t = run(no_timeout(), 0, sag_ms, |ms| {
        let level = 250 - (100 * ms as i32 / sag_ms as i32);
        steady(&mut seed, level, 15)
    });
    assert_eq!(
        t.events,
        vec![],
        "a slow sag from 250 to 150 must not reverse"
    );
    // And the threshold followed it down rather than sitting at a fixed number:
    // 1.4 x 150 = 210 counts, give or take the EMA's lag and the noise.
    let thr = t.det.threshold_counts();
    assert!(
        (180..=240).contains(&thr),
        "threshold ended at {} counts, expected it to track the sag down to ~210",
        thr
    );
}

/// 6. Baseline does not chase a stall. A ramp slow enough that a self-updating
///    baseline would absorb it must still fire.
#[test]
fn baseline_does_not_chase_a_stall() {
    let mut seed = 3;
    let ramp_start = 2_000;
    let ramp_ms = 10_000;
    let t = run(no_timeout(), 0, 20_000, |ms| {
        if ms < ramp_start {
            steady(&mut seed, 250, 15)
        } else {
            let into = (ms - ramp_start).min(ramp_ms);
            (250 + 650 * into / ramp_ms) as u16
        }
    });
    assert!(
        t.events.iter().any(|e| e.reason == Reason::Stall),
        "slow ramp to 900 must still fire a stall, got {:?}",
        t.events
    );
    assert!(t.events[0].at_ms >= ramp_start);
}

/// 6b. The "only fold samples below the threshold" rule, pinned directly.
///
/// The rule matters most when the baseline is fast: a marginal stall — over the
/// threshold, but not by much — gives a self-updating baseline time to climb
/// past the stall current during the debounce window, resetting the counter
/// forever. Freezing the baseline while over threshold is what makes this fire.
/// Run deliberately fast (shift 5) so the effect is visible in milliseconds.
#[test]
fn baseline_freezes_while_over_threshold() {
    let cfg = Config {
        baseline_shift: 5,
        ..no_timeout()
    };
    let t = run(cfg, 0, 6_000, |ms| if ms < 2_000 { 250 } else { 750 });
    assert_eq!(
        t.events.len(),
        1,
        "a marginal stall must fire even with a fast baseline: {:?}",
        t.events
    );
    assert_eq!(t.events[0].reason, Reason::Stall);
}

/// 7. Timeout fires when no stall ever arrives.
#[test]
fn timeout_fires_at_the_limit() {
    let cfg = Config::default();
    let mut seed = 5;
    let t = run(cfg, 0, cfg.max_traverse_ms + 1_000, |_| {
        steady(&mut seed, 250, 15)
    });
    assert_eq!(t.events.len(), 1, "expected one timeout: {:?}", t.events);
    assert_eq!(t.events[0].reason, Reason::Timeout);
    assert_eq!(t.events[0].at_ms, cfg.max_traverse_ms);
    assert_eq!(t.events[0].new_dir, Direction::Reverse);
}

/// 8. Direction alternates across repeated stalls.
#[test]
fn direction_alternates() {
    let traverse_ms = 20_000;
    let t = run_reactive(no_timeout(), 120_000, |since_reversal| {
        if since_reversal < traverse_ms {
            250
        } else {
            900
        }
    });
    assert!(
        t.events.len() >= 4,
        "expected several reversals: {:?}",
        t.events
    );
    assert!(t.events.iter().all(|e| e.reason == Reason::Stall));
    for (i, ev) in t.events.iter().enumerate() {
        let expected = if i % 2 == 0 {
            Direction::Reverse
        } else {
            Direction::Forward
        };
        assert_eq!(ev.new_dir, expected, "reversal {} went the wrong way", i);
    }
}

/// 9. Clock wraparound is a non-event.
#[test]
fn clock_wraparound_matches_zero_start() {
    let cfg = Config::default();
    let duration = cfg.max_traverse_ms + 2_000;
    let trace = |ms: u32| -> u16 {
        // Two stalls and a stretch long enough to reach the traverse limit.
        if (30_000..30_400).contains(&ms) || (90_000..90_400).contains(&ms) {
            900
        } else {
            250
        }
    };

    let from_zero = run(cfg, 0, duration, trace);
    // Start 5s before the u32 rollover, so the wrap lands mid-traverse.
    let across_wrap = run(cfg, u32::MAX - 5_000, duration, trace);

    assert_eq!(
        from_zero.events, across_wrap.events,
        "events differ across wrap"
    );
    assert_eq!(
        from_zero.ticks, across_wrap.ticks,
        "tick output differs across wrap"
    );
    assert!(!from_zero.events.is_empty());
}

/// The soft-start ramp is what keeps inrush out of the detector; check its shape
/// rather than trusting the blanking window alone.
#[test]
fn duty_ramps_from_zero_to_cruise() {
    let cfg = Config::default();
    let t = run(no_timeout(), 0, 2_000, |_| 250);
    assert_eq!(t.ticks[0].duty_permille, 0, "must start from a standstill");
    let mid = t.ticks[(cfg.ramp_ms / TICK_MS / 2) as usize].duty_permille;
    assert!(
        (cfg.cruise_duty_permille / 2).abs_diff(mid) <= 20,
        "halfway through the ramp duty was {}",
        mid
    );
    let after = t.ticks[(cfg.ramp_ms / TICK_MS) as usize + 1].duty_permille;
    assert_eq!(after, cfg.cruise_duty_permille);
    // And the ramp restarts on the far side of a reversal.
    let mut det = Detector::new(no_timeout(), 0);
    let mut last = det.update(0, 250);
    for ms in (10..60_000).step_by(TICK_MS as usize) {
        last = det.update(ms, if ms > 5_000 { 900 } else { 250 });
        if last.reversed.is_some() {
            break;
        }
    }
    assert!(last.reversed.is_some());
    assert_eq!(
        last.duty_permille, 0,
        "reversal tick must command a standstill"
    );
}

/// `blank_ms < ramp_ms` is a configuration error the device must survive.
#[test]
fn blanking_is_raised_to_cover_the_ramp() {
    let cfg = Config {
        ramp_ms: 500,
        blank_ms: 100,
        ..Config::default()
    };
    let det = Detector::new(cfg, 0);
    assert_eq!(det.config().blank_ms, 500);
}

/// The threshold floor keeps a dead sense line from reading as a stall.
#[test]
fn threshold_floor_holds_when_current_is_near_zero() {
    // Floor threshold is min_baseline_counts(40) * stall_ratio_pct(112%) = 44.
    let cfg = no_timeout();
    let t = run(cfg, 0, 30_000, |ms| if ms < 20_000 { 0 } else { 40 });
    assert_eq!(
        t.events,
        vec![],
        "40 counts is under the floor-derived threshold"
    );
}
