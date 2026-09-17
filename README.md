# Boo-merang

Custom controller for a Spirit Halloween "Small Flying Ghost" prop, replacing
the stock board so it can run on a ~75 foot rope. See
[`ghost-firmware-brief.md`](ghost-firmware-brief.md) for the full design
rationale — this file is the practical how-to-build-and-run.

```
stall/     pure stall-detection logic, no_std, no hardware deps, `cargo test`
firmware/  embassy-based Pico (RP2040) binary, thin wrapper around `stall`
```

## Building

```
cargo test -p stall              # 14 tests, runs on the host, no board needed
```

The firmware crate targets `thumbv6m-none-eabi` and is intentionally excluded
from the workspace so the above doesn't need the ARM target installed. From
`firmware/`:

```
rustup target add thumbv6m-none-eabi
cargo install elf2uf2-rs --locked   # one-time; see Flashing below

cargo build --release                        # USB CSV telemetry on
cargo build --release --no-default-features  # deployed build, silent
cargo build --release --features replay      # bench-test without a real ADC
```

## Flashing

There's no debug probe, so flashing is BOOTSEL mass-storage. Hold the Pico's
BOOTSEL button while plugging it into USB, then:

```
cd firmware
cargo run --release
```

`.cargo/config.toml` wires `elf2uf2-rs -d` up as the runner, so this converts
the ELF to a UF2 and copies it onto the mounted `RPI-RP2` drive in one step;
the Pico reboots into the new firmware on its own.

## Wiring

| Pico pin | Signal | To |
|---|---|---|
| GPIO16 | `PWMA` | TB6612FNG `PWMA` |
| GPIO14 | `AIN1` | TB6612FNG `AIN1` |
| GPIO15 | `AIN2` | TB6612FNG `AIN2` |
| GPIO13 | `STBY` | TB6612FNG `STBY` |
| GPIO26 | `SENSE` (ADC0) | top of the current-sense resistor |
| GND | — | **battery negative**, not the driver's GND pin |

Pin numbers live in one block at the top of `firmware/src/main.rs::main` —
change them there, nowhere else.

Current sense: 0.47Ω 1W resistor in the motor's ground return (driver GND to
battery negative), with a 1kΩ series resistor + 10µF to ground feeding the ADC
pin. **The 0.47Ω value is a placeholder** — measure the motor's actual running
and stall current before trusting the stall-detection ratio; see the constant
in `firmware/src/main.rs`.

## Tuning `stall_ratio_pct` and friends

The plan is to have the prop owner plug the Pico into a laptop and capture the
CSV log while it runs:

```
screen /dev/tty.usbmodemXXXX 115200   # or any serial monitor
```

Every tick (or every 10th, by default) prints
`t_ms,sample,baseline,threshold,confirm,dir,duty`; every reversal also logs a
`# reversal ...` line with the reason. Paste the CSV rows into a spreadsheet to
see how cleanly a real stall clears the threshold and retune
`Config::default()` in `stall/src/lib.rs` accordingly. Build with
`--no-default-features` to silence this for the deployed unit.

## Bench-testing without the prop

Two options, cheapest first:

- **`--features replay`**: swaps the ADC read for a compiled-in synthetic
  trace (`firmware/src/replay.rs`) so the real firmware — real `Ticker`, real
  detector, real GPIO timing — runs against known input. Wire two LEDs to
  AIN1/AIN2 (through resistors) to watch it reverse every few seconds.
- **Before the driver board arrives**: a Pico alone, two LEDs standing in for
  AIN1/AIN2, and a potentiometer on GPIO26 in place of the sense resistor —
  twist the pot to fake a stall by hand, using the default (non-replay) build.

## Things that will bite you if skipped

- Pico ground must land on the battery side of the sense resistor, not the
  driver side, or the ADC reference moves with motor current.
- `blank_ms >= ramp_ms` — `Config::sanitized()` enforces this, but don't rely
  on it when picking new numbers.
- Coast (both AIN pins low) between directions, never brake-then-reverse.
- The sense resistor value is a guess until measured. Everything downstream
  depends on it.
