# DSP additions + parser hardening — design & plan

Date: 2026-07-01
Status: approved for autonomous execution (scope locked via three decisions below)

## Context

The CLAUDE.md "Work backlog" is almost entirely shipped (items 4, 5, 6, 7, 9,
10, 11-partial, 14, 15). The remaining open work lives in backlog item 8 and the
`docs/ROADMAP.md` DSP section. This spec covers four well-bounded items chosen so
each is **deterministically testable offline** (FFT / correlation / impulse) and
cross-platform in `resonance-dsp`, wired through IPC + all three front-ends
(GUI/TUI/CLI) so the feature is exercised "every way".

Deliberately **excluded** (need design sign-off + real-audio by-ear that cannot be
done headless — the macOS TCC wall blocks live audio over ssh): per-app EQ,
convolution/IR loader, linear-phase EQ, dynamic EQ.

### Locked decisions

1. **Scope:** all four items below.
2. **Execution:** sequential, single-driver (me), TDD per item, subagents used only
   for independent parallel sub-work (front-end wiring, fuzz run).
3. **Merge policy:** auto-merge each PR to `master` once **fully green** =
   `make check` (fmt + clippy `-D warnings` + `test --all`) on Linux + Linux live
   daemon smoke test + Windows-VM `cargo test --all` + macOS `cargo test`/clippy.
   The macOS *by-ear* audio check is explicitly out of scope (headless wall); DSP
   correctness rests on the offline suites + Linux live.

> The brainstorming user-review gate is **waived** for this run per the explicit
> instruction to execute autonomously overnight. Each item lands as its own
> squash-merge commit so any single feature is trivially revertable in the morning.

## Test surfaces (verified reachable at start)

| Surface | Capability this run |
| --- | --- |
| Linux (dev box) | FULL — no daemon/socket running, user asleep → live PipeWire test + `make check` + free daemon start/stop |
| Windows VM (`ssh -p 2222 Docker@127.0.0.1`, key `~/.ssh/resonance_winvm`) | build + `cargo test --all` (APO staticlib path) |
| macOS (`ssh nyverino@100.67.78.90`, Tailscale) | build + `cargo test --all` + clippy. **No live audio** (TCC) |

Remote verification recipe (from memory): `git fetch` then detached
`git checkout -f <sha>` of the exact commit (remotes' branches are divergent), then
`cargo test --all` / clippy.

## Conventions to honour

- Conventional Commits, all lowercase; **no AI/Co-Authored-By trailer** (repo rule).
- `make check` green before every commit; clippy pedantic is enforced workspace-wide
  via the `[lints]` table — mirror the existing `#[allow(clippy::…)]` + justification
  comment style (e.g. `float_cmp` is active).
- Builder pattern for DSP construction; functional/iterator style.
- Effects follow the `Effect` trait (`process`, `reset`, `set_intensity`/`intensity`,
  `set_enabled`/`enabled`); a new effect is a `FxEffect` variant + `ProcessorChain`
  field + entries in `FxEffect::ALL` and the mirror `FxEffectId::ALL` (the fixed-size
  arrays force every match arm to be updated).

---

## Item 1 — Crossfeed (Bauer/Meier) + Output dither

Bundled per the user's selection; one branch `feat/crossfeed-dither`, two commits.

### 1a. Crossfeed

New `FxEffect::Crossfeed`, `CrossfeedEffect` in `effects.rs`. Reduces hard L/R
isolation on headphones (listening-fatigue reducer). Meier-style: each ear also
hears an attenuated, low-passed copy of the opposite channel.

- Per stereo pair (ch0=L, ch1=R): `outL = L + level·lp(R)`, `outR = R + level·lp(L)`,
  where `lp` is a 1st/2nd-order low-pass on the crossfeed path (the LP supplies the
  frequency-dependent phase that stands in for the head-shadow ITD/ILD).
- `intensity` 0..1 (not bipolar): `level` and cutoff mapped so 0 = **bit-exact
  bypass**, rising to a moderate maximum (`level ≈ 0.5`, cutoff ≈ 700–900 Hz).
- **Level compensation** so a centred (mono, L==R) signal is not boosted:
  normalise by `1/(1+level)` (or fold into the mix) so mono stays ~unity gain.
- Stereo semantics only: operate on channels 0 and 1; pass channels ≥2 through
  unchanged (documented). Mono (1ch) → bypass.
- Rate-dependent LP coefficients rebuilt on `rebind_sample_rate` (via `new`).

Tests (`effects_tests.rs`):
- intensity 0 → bit-exact passthrough.
- crossfeed on: L/R cross-correlation strictly increases vs a decorrelated input.
- hard-left input (R=0) → non-zero R output (bleed present).
- LP character: high-freq bleed magnitude < low-freq bleed magnitude.
- mono input (L==R): output level within ±0.5 dB of input (no boost/cut).
- no NaN/inf; stable after `rebind_sample_rate` to 44.1 k and 96 k.

### 1b. Output dither (TPDF)

Not an intensity effect — a final-stage quantizer. New `DitherStage` + `dither`
field on `ProcessorChain`, applied at the tail of `route()` (final output width).
IPC `Command::SetDither { bits: Option<u32> }` (None = off; 16 | 20 | 24).

- TPDF = sum of two independent uniform PRNGs, amplitude ±1 LSB of the target grid.
  RT-safe xorshift PRNG (no alloc, no syscalls); per-channel state.
- `bits = None` (default) → **bit-exact passthrough** (route unchanged).
- Output snapped to the target quantisation grid plus the triangular dither.

Tests (`chain.rs` / a new `dither_tests` module):
- off → bit-exact passthrough (regression on the existing passthrough tests).
- 16-bit dither on a −90 dBFS sine: undithered truncation shows harmonic spikes;
  dithered output decorrelates them (harmonic peaks below a threshold, noise floor
  raised) — the canonical dither correctness test.
- dithered samples lie on `k/2^(bits-1)` grid ± one dither LSB.
- dither RMS ≈ expected TPDF level for the target depth.
- no NaN/inf.

> Caveat documented in code + ROADMAP: the OS sinks are f32 float, so dithering to
> 16/20/24-bit is only meaningful when the user targets an integer depth downstream.
> Off by default.

---

## Item 2 — Adjustable filter slopes (12/24/48 dB/oct)

Branch `feat/filter-slopes`. Extend HP/LP/shelf bands with a selectable slope; the
current fixed 2nd-order biquad becomes the 12 dB/oct case (regression-preserving).

- Model: add `slope_db_oct: u8` (12 | 24 | 48) to the band model in `resonance-ipc`
  (`Band`) and to `ApoFilter`. Default 12 (== today). Peaking/Notch/BandPass/AllPass
  are single-biquad; slope control is N/A for them (UI greys it out).
- DSP: `ApoFilter` holds a cascade of `N` biquad sections (each with its own
  per-channel state). **HP/LP:** proper Butterworth cascade with the exact
  per-section Q table — 12 dB = 1 section (Q 0.7071); 24 dB = 2 sections
  (Q 0.5412, 1.3066); 48 dB = 4 sections (Q 0.5098, 0.6013, 0.9000, 2.5629). This
  keeps the −3 dB point exactly at Fc for every order. **Shelves:** cascade `order/2`
  shelf sections with the gain split across sections + the Butterworth Q table.
- Rebuild the cascade on `rebind_sample_rate` (Nyquist clamp per section).

Tests (`filter.rs`):
- 12 dB order is bit-exact to the current single-biquad path (regression).
- LP slope rejection an octave above Fc ≈ −12 / −24 / −48 dB (± tolerance) per order.
- −3 dB point stays at Fc for all orders (Butterworth maximally-flat).
- shelf reaches its full gain in the pass region for all orders.
- APO round-trip: our native profile (toml) persists `slope_db_oct`; APO `.txt`
  writing keeps the base type and documents that APO has no portable slope token
  (round-trip of slope is native-profile only).

---

## Item 3 — Mid/side EQ mode

Branch `feat/mid-side-eq`. Per-band `BandScope { Stereo, Mid, Side }`.

- Model: add `scope: BandScope` to the band model (`resonance-ipc` + `ApoFilter`),
  default `Stereo` (== today).
- DSP: restructure `ProcessorChain::process` per band by scope. `Stereo` → the
  existing per-channel, channel-mask path (unchanged, bit-exact). `Mid`/`Side` →
  operate on the front L/R pair (ch0, ch1): per frame derive `M=(L+R)/2`,
  `S=(L−R)/2`, run the band's biquad on M (or S) with dedicated M/S state, write back
  `L=M+S`, `R=M−S`. Channels ≥2 (beyond the front pair) pass through; mono → treat
  Mid/Side as Stereo (no side information).
- Interop: a band can be both Mid/Side-scoped and slope-adjusted (item 2) — the
  attributes are orthogonal.

Tests (`channel_tests.rs` / `filter.rs`):
- Stereo scope → bit-exact to current output (regression).
- Mid-only band: a pure side signal (L = −R) is untouched; a pure mid signal
  (L = R) is fully affected.
- Side-only band: the mirror — mid untouched, side affected.
- M/S forward+inverse transform is lossless with no active band.
- stable across `rebind_sample_rate` / `set_channels`.

---

## Item 4 — Parser fuzz + hardening (existing code)

Branch `feat/parser-fuzz-hardening`. Hardens the untrusted-input parsers
(`resonance-preset`: `.fac`, APO `.txt`, `graphic`).

- Add `cargo-fuzz` targets for the `.fac` and APO parsers (`crates/resonance-preset/fuzz/`).
- Run each target for a bounded budget on Linux; collect any crash/panic repro inputs.
- For each finding: add a regression unit test with the minimised input, then fix the
  parser to return a `Result`/skip gracefully instead of panicking. No parser should
  panic on any input.
- Runs as a background sub-task (fuzzing is wall-clock heavy, independent crate). The
  driver reviews findings and applies fixes with TDD; fuzz corpus/targets are the
  permanent deliverable (item 11 "cargo-fuzz on parsers").

Tests: the crash-repro regression tests + a `cargo fuzz run … -runs=N` smoke in CI notes.

---

## Execution order & rationale

1. **Crossfeed + dither** — cleanest new work, fits existing patterns, highest value
   for lowest risk. First.
2. **Filter slopes** — extends the filter model; precedent already exists.
3. **Mid/side EQ** — most invasive (touches the process loop + whole stack). Last of
   the features.
4. **Parser fuzz** — independent crate; runs in the background from the start; fixes
   folded in as findings arrive.

Per item: invoke test-driven-development, branch off the latest `master`, write
failing tests, implement, wire front-ends (parallel subagents where independent),
`make check`, Linux live smoke, push, verify on Windows VM + macOS, request a
self/subagent code review, then auto-merge on green. Branch off updated `master` for
the next item to avoid shared-file conflicts (IPC enum, `chain.rs`, front-ends).

## Wrap-up

Update `docs/ROADMAP.md` (move shipped items out of the gap list), prune the
CLAUDE.md backlog, and leave a morning summary of what merged + test evidence + any
deferred findings.
