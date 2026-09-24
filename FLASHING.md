# Flashing the boo-merang controller

No dev tools needed — this is a drag-and-drop UF2 flash, same on any OS.

Current build: [`releases/firmware-ef615c1.uf2`](releases/firmware-ef615c1.uf2),
built from commit `ef615c1`. `cruise_duty_permille: 1000`, `stall_ratio_pct:
112` (measured before the duty change — see the `Config::default()` doc
comment in `stall/src/lib.rs` for the caveat). USB logging is enabled in this
build, so if you want to watch the CSV telemetry while testing, plug into a
laptop and `screen /dev/ttyACM0 115200` (or `/dev/tty.usbmodemXXXX` on
macOS) — it runs fine on battery alone either way, USB is only needed for
logging.

1. Disconnect the battery pack.
2. Hold **BOOTSEL** on the Pico W, plug in USB, release. A mass-storage drive
   named **RPI-RP2** mounts.
3. Drag the `.uf2` file onto it. It'll unmount itself in a couple seconds —
   that's the board rebooting into the new firmware. No prompts, nothing else
   to confirm.
4. Reconnect the battery pack and test.

If `RPI-RP2` doesn't show up: you're probably on a charge-only cable, or
released BOOTSEL before plugging in. Retry with a known data cable.