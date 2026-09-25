# Tuning stall sensitivity

One line, in `stall/src/lib.rs`:

```rust
stall_ratio_pct: 112,
```

Higher = less sensitive. Lower = more sensitive.

It's currently at 112, which is very aggressive. Try **160** first, rebuild,
reflash (see `README.md` / `FLASHING.md`), and test:

- Still reverses randomly mid-travel → go higher (try 200).
- Doesn't reverse at the stopper anymore → come back down a bit.

Adjust by ~20-30 at a time and retest until it feels right.

## Prebuilt binaries to try

Three ready-to-flash builds at `releases/`, so you don't need to build
anything — just pick one and follow `FLASHING.md`:

- `firmware-a9c4d75-ratio160.uf2`
- `firmware-a9c4d75-ratio200.uf2`
- `firmware-a9c4d75-ratio240.uf2`

Start with 160. These aren't committed as the project default (that's still
112 in `stall/src/lib.rs`, pending real tuning data) — they're just field
tests built off commit `a9c4d75` with `stall_ratio_pct` swapped for each one.
