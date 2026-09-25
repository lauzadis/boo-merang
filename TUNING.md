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
