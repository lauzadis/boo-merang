# Tuning stall sensitivity

`stall_ratio_pct` in `stall/src/lib.rs`'s `Config::default()` is the whole
knob. It's how far above the running-current baseline (as a percentage) the
sense current has to rise, and stay, before the detector calls it a stall and
reverses. Too high and it never fires — grinds against the rope stopper until
`max_traverse_ms` times out. Too low and normal current noise trips it —
early, spurious reversals mid-traverse.

This is a one-line change:

```rust
stall_ratio_pct: 112,   // <- this one
```

Everything else in the test suite (`stall/tests/detector.rs`) derives its
expected numbers from whatever this is set to, so changing it and rerunning
`cargo test -p stall` shouldn't require editing anything else. If a test
starts failing after you change only this line, that's a real signal, not a
stale hardcoded assumption — read on for what it means.

Needs the dev toolchain (`rustup`, the `thumbv6m-none-eabi` target,
`elf2uf2-rs`) — see the **Building** section of `README.md` if that's not
set up yet.

## The loop

**1. Before touching hardware, sanity-check the number for free.**

Change the line, then:

```
cargo test -p stall
```

If `steady_state_does_not_reverse` fails, your number is inside the noise
floor — normal current variation alone is enough to trip it. Don't bother
flashing it; pick something higher and retest. This test is deliberately
*not* derived from `stall_ratio_pct` (see the comment above it) — it's
checking a property (no false positives on realistic noise), not tracking
the config, so it's supposed to fail here.

**2. Capture a real stall on hardware.**

Flash the default build (`cargo run --release` from `firmware/`, USB logging
on) and watch the CSV over serial:

```
screen -L -Logfile stall_capture.log /dev/ttyACM0 115200
```

(`/dev/tty.usbmodemXXXX` on macOS.) Let it run a few seconds normally, then
do a firm, complete stall by hand — make sure nothing is slipping, hold for
a couple seconds, let go. `Ctrl-A` then `K` to stop and save.

One thing worth knowing: `VM` (the driver's motor supply) shares a node with
the Pico's `VSYS`, and USB backfeeds that node through the Pico's own
VBUS->VSYS diode to ~4.6-5V — higher than the ~3.3-4.5V the battery pack
alone provides. A stalled motor's current scales directly with supply
voltage (no back-EMF at zero RPM); running current doesn't scale nearly as
much. So a USB-connected capture tends to read a *better* (higher) stall
ratio than the prop will actually see running on battery alone. Treat
whatever ratio you measure this way as optimistic, not exact.

**3. Read the log.**

Find `baseline` in the rows just before you stalled it, and the peak
`sample` during the sustained stall plateau. The achieved ratio is:

```
peak_sample / baseline * 100
```

That's your ceiling — `stall_ratio_pct` has to sit below this number to
catch that particular stall, with the caveat from step 2 that this ceiling
is probably a bit optimistic versus real battery-only behavior.

**4. Pick a number.**

You want daylight on both sides: above where step 1's noise test starts
failing, below what you measured in step 3. If that gap is wide, anywhere in
the middle is fine. If it's narrow (it was, last time this was tuned — about
3 percentage points), land in the middle of it rather than hugging either
edge, and expect to revisit it if field conditions vary from the bench test.

**5. Update the line, rerun `cargo test -p stall`, rebuild, reflash.**

Same drag-and-drop flow as `FLASHING.md` once you've got a new `.uf2`
(`cargo build --release`, then convert with `elf2uf2-rs`; see `README.md`'s
Flashing section). Test on battery alone, no USB attached, since that's the
condition that actually matters.

**6. If it's still wrong, that's data too.**

Doesn't reverse on a real stall → still too high, go lower. Reverses on its
own during ordinary running → too low, go higher. Either result narrows the
window for next time.

## After landing on a number

Update the doc comment above `Config::default()` in `stall/src/lib.rs` with
what you measured and why you picked what you picked — the existing comment
there is a good example of the level of detail worth leaving (what was
measured, under what conditions, what the resulting safe window was). Future
tuning starts from that context instead of from zero.
