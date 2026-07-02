# Dynamic EQ — design

Date: 2026-07-02
Status: approved for autonomous execution (user delegated task choice; the
brainstorming/spec user-review gates are waived per the standing autonomous-run
precedent of 2026-07-01 — decisions below are locked by the driver and each
merge remains trivially revertable as a single squash commit)

## Context

Backlog item 8 / ROADMAP "Dynamic EQ": per-band gain driven by input level —
de-essing, resonance taming, compression-style mastering. The last remaining
item-8 feature besides linear-phase mode. Wiring shape mirrors the shipped
per-band slope (PR #33) and mid/side scope (PR #34) features.

Constraints honoured:

- The live Linux daemon must NOT be restarted (user's parked audio-cut window).
  Verification = offline DSP suites + `make check` + Windows VM (`cargo test
  --all`, optional in-guest audiodg live proof) + macOS suite/clippy over ssh.
  Linux live `resonance verify` happens in the user's next window.
- No by-ear verification (standing policy 2026-07-02).
- Progressive disclosure: all new UI is hidden behind an advanced-visibility
  preference, off by default, surfaced in the `adv:` status hints.

## Semantics (locked)

A band with dynamics enabled morphs its **gain** between the static `gain_db`
and `gain_db + range_db` as the level of the signal *in that band's region*
crosses a threshold.

Parameters (`BandDynamics`, all `f64`):

| param | meaning | clamp | default |
| --- | --- | --- | --- |
| `threshold_db` | detector level (dBFS) where the morph starts | −80 … 0 | −30 |
| `range_db` | signed max gain offset; negative = cut when loud (de-ess), positive = boost when loud | −24 … +24 | −6 |
| `attack_ms` | detector attack time constant | 0.1 … 500 | 5 |
| `release_ms` | detector release time constant | 1 … 5000 | 150 |

Gain law (feed-forward, zero lookahead → **zero added latency**):

```
env      = peak envelope of sidechain, one-pole attack/release smoothing
o        = env_db − threshold_db          (overshoot)
offset   = 0                              if o ≤ 0
         = sign(range) · min(o, |range|)  if o > 0
eff_gain = gain_db + offset
```

1:1 overshoot growth capped by `|range|` — the de-esser law. No ratio knob
(YAGNI; range cap already bounds the action).

**Sidechain** (linked — one detector per band, all its channels get the same
offset, so the stereo image never wobbles):

- source = the band's input signal at its position in the chain (feed-forward):
  - `Stereo` scope: mean of the band's **masked** channels per frame
  - `Mid` scope: the M sample; `Side` scope: the S sample (mono chain: ch 0)
- filter = band-pass biquad at the band's `freq`/`q` (cookbook BP, unity peak)
  so only in-band energy triggers the morph
- detector = rectify → one-pole peak follower: rising edges use
  `1 − exp(−1/(attack_s · sr))`, falling use the release coefficient;
  converted to dB per frame (`max(env, 1e-8)` floors the log)

**Applicability: `Peaking` bands only (v1).** The two real use cases (de-ess,
resonance taming) are peaking bands; peaking never cascades (no slope
interaction) and its gain-only coefficient morph is cheap from cached trig.
`BandType::uses_dynamics()` mirrors `uses_slope()`; front-ends grey the control
out elsewhere. Shelves can be added later without model changes.

**Coefficient morph:** the peaking biquad's `ω`-terms don't depend on gain —
`cos ω0` and `α` are cached; per-frame morph recomputes only
`A = 10^(eff/40)` and the five coefficients (~1 powf + 10 flops), applied when
the offset moved > 0.01 dB since the last rebuild. Filter state (`x/y`
history) is preserved across morphs — same click-free rationale as
`ApoFilter::update`.

Cost: ~2× a static band per dynamic band (1 extra biquad + envelope + rare
powf). Reference: the full 10-band+fx chain runs at 1 % DSP load today.

## DSP layer (`resonance-dsp`)

- `filter.rs`: `DynParams { threshold_db, range_db, attack_ms, release_ms }`
  (dsp-local mirror, like `BandScope`), plus a private `DynState` on
  `ApoFilter`:
  - sidechain `BiquadState` (single, linked) + BP coeffs
  - envelope value, attack/release coefficients (rate-derived)
  - cached `cos_w0`, `alpha`, last applied offset
- `ApoFilter` gains `dynamics: Option<…>`, builder `.dynamics(Option<DynParams>)`,
  `set_dynamics(Option<DynParams>, sr)`, and a per-frame hook
  `dyn_detect(det_sample)` that updates the envelope and morphs the head
  coefficients. `rebind`/`update`/`set_channels` rebuild/keep dyn state
  (envelope resets on rate change; params clamped on entry).
- `chain.rs` process loop: bands with active dynamics take a frame-major path —
  per frame: compute the detector sample (masked mean, or the M/S value already
  computed in the M/S branch), call `dyn_detect`, then process the frame's
  channels as today. Static bands keep the existing band-major loop untouched
  (bit-exact regression).
- `dynamics.rs` **not** created — the feature lives inside `filter.rs`
  (it is a property of a band, exactly like slope/scope).

## IPC (`resonance-ipc`)

- `BandDynamics` struct (Copy, serde, defaults fn) + `BandState.dynamics:
  Option<BandDynamics>` with `#[serde(default)]` → old profile `.toml`s load
  as `None`. Postcard wire stays version-locked (clients ship with the daemon —
  existing precedent, noted on `BandState.channels`).
- `Command::SetBandDynamics { index: usize, dynamics: Option<BandDynamics> }`
  **appended** to the enum (postcard append-only rule).
- `BandType::uses_dynamics()` helper; `BandDynamics::DEFAULT` for front-ends.
- `ApplyState`/undo-redo, profiles, A/B slots, `DaemonState.bands` all carry
  the field for free once `BandState` has it.

## Daemon

- `AudioCommand`/handler: `SetBandDynamics` → `filters[i].set_dynamics(...)`,
  mirrored into the state snapshot; `info!` chain-command log line (issue #43
  convention).
- Profile persist/restore via `BandState` (automatic). `apply_profile_chain`
  path already replays bands.

## Windows APO parity

- `FilterSnapshot` += `dyn_enabled: bool` + the four params (fixed-size, shmem
  seqlock friendly); `ChainSnapshot` version bump. The APO builds the same
  `ApoFilter` via the shared dsp crate → DSP parity is automatic.

## CLI

- `resonance band-dyn <index> off` — clear
- `resonance band-dyn <index> <threshold_db> <range_db> [attack_ms] [release_ms]`
  — set (defaults 5/150 for the optional pair)
- `status`/`format` band lines append a `dyn(thr/range)` marker when set.
- `resonance verify` mode pick: bands with dynamics **enabled** count as
  "effects active" → baseline A/B mode (the static FR prediction no longer
  holds once test tones can trigger the morph).

## GUI

- Settings dialog: new advanced checkbox `show_dynamics` (default off), joins
  slope/scope/dither/channels; `adv:` hint reports a hidden-but-set dynamic
  band.
- Band table: `Dyn` column (menu button) → popup with enable + four
  drag-values + "off"; greyed for non-peaking bands; button highlights when
  active.

## TUI

- Preferences row `show dynamics` gating a `Dy` column (abbrev `thr→range`).
- Key `y` on the selected band: toggles dynamics on (defaults) / off.
- Key `Y`: opens a small modal editor (4 rows, ←/→ adjust, Esc close) — same
  interaction pattern as the settings rows.
- Help overlay + `adv:` hint updated (hint label fns gain a 6th bool).

## Out of scope (v1)

- Shelf/HP-LP dynamics, per-channel (unlinked) detection, lookahead, ratio
  knob, upward compression below threshold, FR-curve visualisation of the
  dynamic region, APO `.txt` interop token (native profiles only — APO has no
  portable dynamic-EQ syntax).

## Tests (all offline, deterministic)

`resonance-dsp` (filter.rs / chain.rs / a new `dynamics_tests.rs` if sizeable):

1. `dynamics: None` → bit-exact to the pre-change band-major path (regression).
2. Below threshold: in-band sine at −40 dBFS, threshold −30 → output equals the
   static band's (settled tail within 1e-9).
3. Full morph: loud in-band sine (overshoot ≫ |range|), range −6 → measured
   band gain ≈ static −6 dB (±0.3 dB, settled).
4. Partial morph: overshoot ≈ 3 dB, range −6 → offset ≈ −3 dB (±0.5 dB).
5. Positive range boosts when loud.
6. Attack/release: level step up then down; offset crosses 63 % of target
   within ~attack (and decays within ~release) window (loose ×2 tolerance —
   one-pole timing).
7. Frequency selectivity: loud tone 3+ octaves away does not trigger
   (|offset| < 0.5 dB) — the BP sidechain works.
8. Scope: Side-scoped dynamic band triggers on a pure-side signal, not on mono
   (and vice versa for Mid).
9. Mask: a band masked to ch0 with a loud ch1-only signal does not trigger
   (linked detector reads masked channels only).
10. `rebind_sample_rate` 48k→96k→44.1k: no NaN/inf, envelope re-derived, morph
    still lands (repeat test 3 at 96k).
11. Params clamped (threshold −200 → −80 etc.); non-finite params rejected to
    defaults (hostile IPC hardening, same posture as `update`).

`resonance-ipc`: postcard + toml round-trip of `BandState` with and without
dynamics; old-toml-without-field loads `None`.

Front-end/daemon: profile save→load round-trip keeps dynamics; ApplyState
(undo) replays it; CLI arg parse unit tests.

## Verification gates (merge policy)

1. Linux `make check` (fmt, clippy pedantic, `test --all`) — live daemon
   untouched.
2. Cross-target clippy from Linux on stable 1.96 (msvc + darwin, CI parity).
3. Windows VM: `cargo test --all` (`resonance-apo` with `--test-threads=1`).
4. macOS: `cargo test --all` + clippy (detached checkout of the exact SHA).
5. PR → merge on green; watch the **post-merge** `windows-installer` run
   (push-triggered only) via `gh run list` conclusion fields, never piped exit
   codes.
6. Linux live `resonance verify` A/B on a dynamic band: deferred to the user's
   audio-cut window (daemon restart required); noted in the PR.
