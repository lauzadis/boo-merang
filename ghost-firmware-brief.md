# Flying ghost prop controller — implementation brief

## What this is

A Halloween decoration (Spirit Halloween "Small Flying Ghost") rides a rope back
and forth, driven by a small brushed DC motor in a carriage that grips the rope.
The stock control board reverses direction by detecting motor stall current when
the carriage hits a physical stopper on the rope.

The stock board has a watchdog: if it doesn't see a direction change within 50
seconds, it shuts the motor off and latches until power-cycled. On the stock
157-inch rope a traverse takes ~15s, so this never trips. The prop is being
strung across ~75 feet, where a traverse takes over a minute, so it faults out
mid-span every time. Power-cycling clears the fault but always resumes in the
forward direction regardless of which way it was going, so a timer-based
power interrupter won't work.

Decision: discard the stock board entirely (the audio it provided is not wanted)
and drive the motor from a Raspberry Pi Pico via a TB6612FNG motor driver, with
our own stall detection for direction reversal.

## Hardware

- Raspberry Pi Pico (RP2040). **Not** a Pico W. No debug probe available.
- TB6612FNG dual motor driver breakout (only channel A used).
- Power: 3x AA alkaline, nominally 4.5V, sagging toward ~3.3V over an evening.
  TB6612FNG motor supply minimum is 2.7V.
- Current sense resistor in the motor's ground return, between the driver's
  GND pin and battery negative. Value TBD pending measurement of the motor's
  running and stall current — assume 0.47Ω 1W for now, make it a named constant.
- RC low-pass filter on the sense line into the ADC: 1kΩ series into the analog
  pin, 10µF from the pin to ground. (~10ms smoothing.)
- Pico ground must connect to **battery negative**, i.e. the far side of the
  sense resistor from the driver, so the Pico's ground reference does not move.

### Pin assignment

Pick GPIOs and put them in one clearly-marked constants block at the top of
main so they can be changed without hunting. Needed:

- `PWMA` — PWM output to driver, motor speed
- `AIN1`, `AIN2` — direction pins (digital out)
- `STBY` — driver standby, must be driven high to enable the chip
- `SENSE` — analog input, must be GPIO26/27/28 (RP2040 ADC channels 0/1/2)

Driver truth table for channel A: `AIN1=1, AIN2=0` = forward; `AIN1=0, AIN2=1` =
reverse; both low = coast; both high = brake. Use coast, not brake, during the
pause between directions.

## Project structure

Cargo workspace, two crates:

```
Cargo.toml          workspace
stall/              pure logic, no_std, no HAL deps, tested on host
firmware/           embedded binary, thin wrapper
```

The `stall` crate must be `#![cfg_attr(not(test), no_std)]` and have **zero**
hardware dependencies — no embassy, no HAL, no embedded-hal traits. It takes
numbers in and returns a desired output state. This is the whole point of the
split: the interesting logic is testable with `cargo test` on the host, and the
embedded layer stays small enough that HAL API churn is cheap to absorb.

## The `stall` crate

### Interface

Something along these lines; adjust naming to taste:

```rust
pub enum Direction { Forward, Reverse }
pub enum Reason { Stall, Timeout }

pub struct Config {
    pub ramp_ms: u32,            // soft-start ramp length
    pub blank_ms: u32,           // detection suppressed this long after a reversal
    pub stall_ratio_pct: u32,    // trigger threshold as % of baseline, e.g. 250
    pub confirm_samples: u16,    // consecutive over-threshold samples required
    pub max_traverse_ms: u32,    // safety net: reverse anyway after this
    pub baseline_shift: u8,      // EMA smoothing factor (shift, not divide)
    pub min_baseline_counts: u32,// floor so threshold can't collapse near zero
    pub cruise_duty_permille: u16, // 0..=1000, normal running speed
}

pub struct Tick {
    pub dir: Direction,
    pub duty_permille: u16,
    pub reversed: Option<Reason>, // Some(..) on the tick where a reversal fired
}

impl Detector {
    pub fn new(cfg: Config, now_ms: u32) -> Self;
    pub fn update(&mut self, now_ms: u32, sample_counts: u16) -> Tick;
}
```

`update` is called on a fixed cadence (see firmware section). It is a pure state
transition — no interior mutability tricks, no globals, no panics on any input.
Use saturating arithmetic throughout; `now_ms` wrapping should not misbehave
(use `wrapping_sub` for elapsed-time computation).

### Algorithm

Per call, compute `elapsed = now_ms.wrapping_sub(phase_start_ms)`.

**Duty cycle.** If `elapsed < ramp_ms`, duty ramps linearly from 0 to
`cruise_duty_permille` across the ramp. Otherwise duty is `cruise_duty_permille`.
Soft start matters: a stationary motor presents only its winding resistance and
draws a large inrush spike, which looks exactly like a stall. Ramping over ~500ms
mostly prevents that spike from forming.

**Blanking.** If `elapsed < blank_ms`, do no detection and do not update the
baseline. `blank_ms` must be >= `ramp_ms`. On the first tick after blanking ends,
seed the baseline directly from the sample rather than easing into it from zero.

**Baseline.** An exponential moving average of the sample, in Q8 fixed point
(`baseline_q8`), updated with a right shift rather than a division. Critically:
**only fold a sample into the baseline if it is below the threshold.** If stall
samples update the baseline, the baseline chases the stall and the detector
never fires. This is the single easiest bug to write here.

**Threshold.** `max(baseline, min_baseline_counts) * stall_ratio_pct / 100`.
Comparing against a moving baseline rather than a fixed number is deliberate —
battery voltage sags over the evening and friction changes with temperature, and
nobody will be on site to retune a constant. Watch for overflow: do the multiply
in u32 (or u64) before the divide.

**Debounce.** Increment a counter while over threshold, reset to zero the moment
a sample falls below. Fire only when the counter reaches `confirm_samples`.
Brushed motors are electrically noisy; a single spike must not trigger.

**Timeout.** If `elapsed > max_traverse_ms`, reverse anyway with
`Reason::Timeout`. Nobody will be present to intervene if the carriage slips past
a stopper, so this keeps it moving rather than grinding into the rope end all
night. Suggest 180_000.

**Reversal.** Flip direction, reset `phase_start_ms` to now, clear the debounce
counter, mark the baseline as needing re-seeding. The caller is responsible for
any dwell/coast time between directions — see below.

### Suggested starting config

These are guesses, to be replaced once real current measurements exist. Put them
in a `Default` impl and make it obvious they're provisional.

```
ramp_ms: 500, blank_ms: 800, stall_ratio_pct: 250, confirm_samples: 5,
max_traverse_ms: 180_000, baseline_shift: 5, cruise_duty_permille: 700
```

### Required tests

Write these as host-side unit tests driving synthetic sample traces. Build a
small helper that runs a trace through the detector and returns the ticks where
a reversal fired, then assert on that.

1. **Inrush is ignored.** A 900-count spike decaying to 250 over the first 100ms
   produces no reversal, because it lands inside the blanking window.
2. **Steady state is quiet.** 250 counts ±15 of noise for 60 seconds produces no
   reversal.
3. **A stall fires.** Steady state, then a ramp to 900 counts held — fires once,
   within roughly `confirm_samples` sample periods of the ramp crossing the
   threshold.
4. **Single spikes are rejected.** Steady state with isolated one-sample spikes
   to 900 produces no reversal.
5. **Baseline tracks battery sag.** Steady state that drifts from 250 down to 150
   counts over several minutes produces no reversal — this is the test that
   catches a hardcoded threshold.
6. **Baseline does not chase a stall.** Directly asserts the "only update
   baseline below threshold" rule: a slow ramp from 250 to 900 over 10 seconds
   must still fire.
7. **Timeout fires.** Constant steady-state current with no stall ever produces
   exactly one reversal at `max_traverse_ms`, with `Reason::Timeout`.
8. **Direction alternates.** Repeated stalls produce Forward, Reverse, Forward…
9. **Clock wraparound.** Starting `now_ms` near `u32::MAX` and wrapping past zero
   behaves identically to starting at zero.

## The `firmware` crate

Keep this as thin as possible. Its entire job:

1. Init clocks, GPIO, PWM, ADC.
2. Drive `STBY` high.
3. Loop on a fixed cadence (suggest every 10ms): read the ADC, call
   `detector.update(now_ms, sample)`, apply `dir` to AIN1/AIN2 and
   `duty_permille` to the PWM.
4. On a tick where `reversed` is `Some(_)`, coast for a short dwell (~300ms,
   both direction pins low, duty zero) before applying the new direction. This
   avoids slamming a spinning motor into reverse. Do this in the firmware, not
   in the detector.

Use embassy (`embassy-rp`, `embassy-executor`, `embassy-time`) unless there's a
reason not to — `Ticker` gives a clean fixed-cadence loop. Do **not** pin
dependency versions from memory; resolve current versions and check the API
against the docs for whatever version actually resolves. The embassy-rp PWM and
ADC APIs have churned across releases (peripheral names like `PWM_CH0` vs
`PWM_SLICE0`, constructor signatures), so expect the examples in the embassy repo
for the resolved version to be the authority, not any snippet from memory.

Sample the ADC with oversampling — average 16 reads per tick — on top of the RC
filter already in hardware.

### Toolchain and flashing

- Target `thumbv6m-none-eabi`.
- No debug probe, so no `probe-rs`, no defmt-over-RTT. Flashing is via BOOTSEL
  mass-storage: build to ELF, convert with `elf2uf2-rs`, drag the UF2 onto the
  mounted drive. Set this up as the cargo runner in `.cargo/config.toml` so
  `cargo run --release` does it in one step.
- `memory.x` plus a `build.rs` that emits it, per the standard RP2040 setup.
- The RP2040 needs the second-stage bootloader; `embassy-rp` handles this when
  its `boot2` feature path is used, but verify the linker script and the
  `#[link_section = ".boot2"]` arrangement matches the crate version in use.

### Logging

There is no probe, so logs must go over USB CDC (`embassy-usb` +
`embassy-usb-logger`, or a hand-rolled CDC class). This matters more than usual:
the prop owner is remote, and the plan for tuning `stall_ratio_pct` is to have
him plug the Pico into a laptop, run the prop, and send back a capture.

Emit a line per tick, or per 10 ticks, containing at minimum: timestamp, raw
sample counts, current baseline, current threshold, debounce counter, direction,
duty. CSV, so it can be pasted into a spreadsheet and plotted. Also emit a
distinct line on every reversal with the `Reason`.

Make logging feature-gated so it can be compiled out for the deployed build.

### Bench testing without the real prop

Add a feature flag that replaces the ADC read with samples pulled from a
compiled-in table, so the same firmware binary can replay a synthetic trace
on-target and prove the timing behaves the same as it did in host tests.

Before the driver board arrives, the whole thing can be exercised on a Pico
alone with two LEDs standing in for AIN1/AIN2 and a potentiometer on the ADC pin
substituting for the sense resistor — twist the pot to fake a stall.

## Things to get right

- Pico ground on the battery side of the sense resistor, not the driver side.
- Baseline must not absorb stall samples.
- `blank_ms >= ramp_ms`.
- Coast between directions, never brake-then-reverse.
- No panics and no unwraps in the detector — it runs unattended for hours.
- Sense resistor value is a placeholder until the motor's actual running and
  stall current are measured. Everything downstream (threshold ratio, ADC
  headroom) depends on it, so make it a single well-commented constant.
