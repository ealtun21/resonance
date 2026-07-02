# Dynamic EQ Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Per-band level-driven gain (de-essing / resonance taming): a Peaking band morphs its gain toward `gain_db + range_db` when in-band level crosses a threshold.

**Architecture:** Feed-forward linked sidechain inside `ApoFilter` (band-pass detector at the band's Fc/Q → one-pole peak envelope → gain-offset law → cheap peaking-coefficient morph from cached trig). Wire shape mirrors shipped slope (PR #33) / scope (PR #34): IPC `BandState` field + appended `Command`, daemon handler, APO snapshot parity, CLI/GUI/TUI controls behind advanced prefs.

**Tech Stack:** Rust workspace; no new dependencies.

**Spec:** `docs/superpowers/specs/2026-07-02-dynamic-eq-design.md` (semantics + test list live there; this plan is the file-level execution map).

## Global Constraints

- Conventional Commits, all lowercase, **no AI/Co-Authored-By trailers**.
- `make check` green before every commit (fmt --check, clippy pedantic `-D warnings`, `cargo test --all`).
- Live Linux daemon must NOT be restarted/killed (user's parked audio-cut window).
- Postcard enum append-only: new `Command` variant goes LAST.
- Progressive disclosure: new UI hidden behind advanced prefs, off by default, surfaced in `adv:` hints.
- Dynamics apply to `Peaking` bands only (v1); invariant: dyn state only exists on Peaking bands.
- Defaults/clamps: threshold −30 [−80..0], range −6 [−24..24], attack 5 [0.1..500], release 150 [1..5000]; non-finite → that field's default.

---

### Task 1: DSP — `DynParams` + sidechain/morph inside `ApoFilter`

**Files:**
- Modify: `crates/resonance-dsp/src/filter.rs` (struct + builder + methods + unit tests in the existing `mod tests`)

**Interfaces (Produces):**
- `pub struct DynParams { pub threshold_db: f64, pub range_db: f64, pub attack_ms: f64, pub release_ms: f64 }` + `DynParams::DEFAULT` + `fn clamped(self) -> Self`
- `ApoFilterBuilder::dynamics(Option<DynParams>)`
- `ApoFilter::set_dynamics(&mut self, Option<DynParams>, sr: f64) -> Result<(), FilterError>`
- `ApoFilter::dynamics(&self) -> Option<DynParams>` (None on non-peaking / unset)
- `ApoFilter::dynamics_active(&self) -> bool` (set && enabled && realizable)
- `ApoFilter::dyn_detect(&mut self, det: f64)` — per-frame hook: sidechain → envelope → morph head coeffs

**Implementation core** (private `DynState` on `ApoFilter`):

```rust
#[derive(Debug, Clone)]
struct DynState {
    params: DynParams,        // clamped
    sc_coeffs: BiquadCoeffs,  // band-pass at the band's freq/q (unity peak)
    sc_state: BiquadState,    // linked detector — ONE state, not per-channel
    env: f64,                 // linear peak envelope
    att: f64,                 // 1 - exp(-1000/(attack_ms * sr))
    rel: f64,
    cos_w0: f64,              // cached trig for the gain-only morph
    alpha: f64,
    offset_db: f64,           // currently applied offset
}
```

`dyn_detect` (early-return unless enabled && realizable && state present):
rectified BP output → `env += coef * (sc - env)` (attack coef when rising) →
`env_db = 20·log10(env.max(1e-8))` → `over = env_db - threshold` →
`target = 0` if `over ≤ 0` else `sign(range)·min(over, |range|)` →
if `|target - offset_db| > 0.01` recompute the head peaking coeffs inline from
cached `cos_w0`/`alpha` with `A = 10^((gain_db + target)/40)` (mirror
`BiquadCoeffs::peaking` including the `1 + alpha/A` normalisation). Extract the
recompute as a private helper `dyn_apply_offset(&mut self, target: f64)` so
`reset`/`update` can restore the static response (`target = 0`).

Consistency rules:
- `update()` / `rebind()`: rebuild `sc_coeffs` + trig cache + att/rel at the new
  freq/q/sr, keep `env`, set `offset_db = 0` (head was just set static).
- `reset()`: reset `sc_state`, `env = 0`, and `dyn_apply_offset(0.0)` so stale
  morphed coefficients can't survive a reset.
- `set_dynamics(None, sr)`: drop state + restore static head via `update`.
- `set_dynamics(Some(p), sr)` on a non-Peaking type: store nothing, return Ok
  (front-ends gate; daemon no-ops) — documented on the method.
- builder: `.dynamics(Some(p))` attaches only when the resolved type is Peaking.

- [ ] **Step 1:** Write failing tests in `filter.rs mod tests` (names/assertions from spec test list 2–7, 10, 11): `dyn_below_threshold_matches_static_band`, `dyn_full_morph_reaches_range`, `dyn_partial_morph_tracks_overshoot`, `dyn_positive_range_boosts_when_loud`, `dyn_attack_release_timing`, `dyn_out_of_band_signal_does_not_trigger`, `dyn_params_clamped_and_nonfinite_rejected`, `dyn_survives_rebind`. Use a helper that feeds an in-band sine at a given dBFS amplitude frame-by-frame (`dyn_detect(x)` then `process_channel(x, 0)`) and measures settled RMS gain over the last quarter of the signal vs a static twin band.
- [ ] **Step 2:** `cargo test -p resonance-dsp dyn_` → all FAIL (missing symbols).
- [ ] **Step 3:** Implement per the core above.
- [ ] **Step 4:** `cargo test -p resonance-dsp` → PASS, including all pre-existing tests (regression: `dynamics: None` paths byte-identical — `slope_12db_is_bit_exact_to_single_biquad` etc. still green).
- [ ] **Step 5:** Commit `feat(dsp): per-band dynamic gain morph in apofilter`.

### Task 2: DSP — chain loop integration

**Files:**
- Modify: `crates/resonance-dsp/src/chain.rs` (`ProcessorChain::process` + tests)

**Interfaces:** Consumes Task 1's `dynamics_active`/`dyn_detect`.

In the `BandScope::Stereo` arm: when `filter.dynamics_active()`, take a
frame-major path — per frame compute the linked detector sample = mean of the
band's **masked** channels, call `filter.dyn_detect(det)`, then process the
frame's masked channels as today. The existing band-major loop stays untouched
for static bands (bit-exact). In the `Mid`/`Side` arm (and the mono-Mid branch):
call `filter.dyn_detect(m)` / `(s)` on the value already computed, before
`process_channel`.

- [ ] **Step 1:** Failing tests in `chain.rs mod tests`: `dyn_band_in_chain_cuts_when_loud` (stereo, full-morph vs static twin), `dyn_side_scope_triggers_on_side_only` (spec test 8), `dyn_masked_band_ignores_unmasked_channel` (spec test 9: band masked to ch0, loud tone on ch1 only → no morph), `static_bands_bit_exact_with_dynamics_feature_present` (reuse `multi_band_cascade_is_order_equivalent` shape).
- [ ] **Step 2:** Run → FAIL. **Step 3:** implement. **Step 4:** `cargo test -p resonance-dsp` → PASS.
- [ ] **Step 5:** Commit `feat(dsp): dynamic band detection in the chain process loop`.

### Task 3: IPC model + command

**Files:**
- Modify: `crates/resonance-ipc/src/lib.rs`

**Interfaces (Produces):**
- `pub struct BandDynamics { pub threshold_db: f64, pub range_db: f64, pub attack_ms: f64, pub release_ms: f64 }` (Copy, PartialEq, serde) + `BandDynamics::DEFAULT`
- `BandState.dynamics: Option<BandDynamics>` with `#[serde(default)]` (doc comment mirrors the `channels` postcard note)
- `Command::SetBandDynamics { index: usize, dynamics: Option<BandDynamics> }` — **appended last**
- `BandType::uses_dynamics(self) -> bool` (`matches!(self, BandType::Peaking)`)
- `impl From<BandDynamics> for resonance_dsp::filter::DynParams` + reverse (mirror the `BandScope`↔`DspScope` pair at lib.rs:536)

- [ ] **Step 1:** Failing tests: postcard round-trip of `BandState` with `Some(BandDynamics)`, toml round-trip, old-toml-without-field → `None` (mirror the existing serde-default tests near lib.rs:999).
- [ ] **Step 2:** FAIL → **Step 3:** implement → **Step 4:** `cargo test -p resonance-ipc` PASS.
- [ ] **Step 5:** Commit `feat(ipc): band dynamics model + setbanddynamics command`.

### Task 4: Daemon wiring

**Files:**
- Modify: `crates/resonance-daemon/src/state.rs` (AudioCommand variant after `SetBandScope` at :66; snapshot at :288 adds `dynamics: f.dynamics().map(Into::into)`)
- Modify: `crates/resonance-daemon/src/ipc_server.rs` (match arm at :169 area; handler next to `handle_set_band_scope` at :680; ApplyState band build at :893; `info!` chain-command log per #43 convention)
- Modify: `crates/resonance-daemon/src/config.rs` (`from_preset` :85 → `dynamics: None` with a comment "presets carry no dynamics"; `into_chain` :141 → `.dynamics(b.dynamics.map(Into::into))`)

Handler (mirror `handle_set_band_scope`, plus an index-range check like
`handle_set_band_channels` and a `uses_dynamics` gate):

```rust
fn handle_set_band_dynamics(
    state: &SharedState,
    index: usize,
    dynamics: Option<BandDynamics>,
) -> Response {
    // validate index + band type up front so the client gets a real error
    // (the RT closure can't reply)
    ...Response::Error for out-of-range or non-peaking with Some(..)...
    state.send(AudioCommand::SetBandDynamics { index, dynamics }, move |chain| {
        let sr = chain.sample_rate;
        if let Some(f) = chain.filters.get_mut(index) {
            let _ = f.set_dynamics(dynamics.map(Into::into), sr);
        }
    });
    Response::Ok
}
```

- [ ] **Step 1:** Failing daemon test: profile round-trip preserves dynamics (mirror an existing profile round-trip test in config.rs/tests) + snapshot reports it.
- [ ] **Steps 2–4:** FAIL → implement → `cargo test -p resonance-daemon` PASS.
- [ ] **Step 5:** Commit `feat(daemon): setbanddynamics handler + profile persistence`.

### Task 5: Windows APO parity

**Files:**
- Modify: `crates/resonance-apo/src/state.rs`

Changes:
- `FilterSnapshot` += `pub dyn_enabled: u32` + `pub dyn_threshold_db: f64, pub dyn_range_db: f64, pub dyn_attack_ms: f64, pub dyn_release_ms: f64` (append after `channels`).
- `STATE_VERSION` 5 → 6, doc line `/// v6: + per-band dynamics.`
- `from_chain` (:245 area): fill from `f.dynamics()`.
- `build_chain` (:301 area): `.dynamics(snapshot fields → Option<DynParams>)`.
- in-place `update` path (:370 area): `let _ = slot.set_dynamics(..., sample_rate);` after the scope assign.

- [ ] **Step 1:** Failing test: `ChainSnapshot::from_chain` → `build_chain` round-trips dynamics (mirror existing snapshot round-trip tests in the same file).
- [ ] **Steps 2–4:** FAIL → implement → `cargo test -p resonance-apo -- --test-threads=1` PASS (shared-state-file harness rule).
- [ ] **Step 5:** Commit `feat(apo): carry band dynamics through the chain snapshot`.

### Task 6: CLI

**Files:**
- Modify: `crates/resonance-cli/src/main.rs` (Sub variant after `BandScope` :141; parse handler after :507; status band line tail at :1023 area)
- Modify: `crates/resonance-cli/src/verify.rs` (`pick_mode` :148 — dynamics count as "prediction doesn't model")

Subcommand (1-based index like band-slope/band-scope):

```
resonance band-dyn <index> off
resonance band-dyn <index> <threshold_db> <range_db> [attack_ms] [release_ms]
```

Parse: `off` (case-insensitive) → `dynamics: None`; else two required f64 + two
optional (defaults 5 / 150). Status tail: `dyn −30/−6` (thr/range, dim) when
set, after the scope tail. `pick_mode`: extend `effects_active` with
`|| state.bands.iter().any(|b| b.enabled && b.dynamics.is_some())`.

- [ ] **Step 1:** Failing parse unit tests (mirror the band-slope/scope parse tests if present; else add: valid set, `off`, 0-index bail, non-finite bail).
- [ ] **Steps 2–4:** FAIL → implement → `cargo test -p resonance-cli` PASS.
- [ ] **Step 5:** Commit `feat(cli): band-dyn subcommand + verify mode pick`.

### Task 7: GUI

**Files:**
- Modify: `crates/resonance-gui/src/app.rs` (pref field `show_dynamics` near :343; storage load :695 / save :1244; adv hint :1585)
- Modify: `crates/resonance-gui/src/ui/bands.rs` (column flag :54 area; `band_dyn_cell` next to `band_scope_cell` :449)
- Modify: `crates/resonance-gui/src/ui/dialogs.rs` (settings checkbox after show_scope :138)

`band_dyn_cell`: menu button labelled `Dyn` (highlighted when set); popup =
enable toggle + 4 `DragValue` rows (threshold dBFS, range dB, attack ms,
release ms, spec clamps) + `off` button; sends
`Command::SetBandDynamics` on change. Greyed (disabled) unless
`band_type.uses_dynamics()`. Column rendered only when `show_dynamics`.
adv hint: report when any band has dynamics set while the column is hidden
(mirror the slope/scope hint lines).

- [ ] **Steps:** implement (GUI has thin test coverage — follow the compile +
  existing single GUI test), `cargo test -p resonance-gui` + `cargo clippy -p resonance-gui` PASS, screenshot via `contrib/dev/uishot.sh` for the PR.
- [ ] Commit `feat(gui): dynamic eq band controls behind advanced pref`.

### Task 8: TUI

**Files:**
- Modify: `crates/resonance-tui/src/prefs.rs` (`show_dynamics` after :34)
- Modify: `crates/resonance-tui/src/app.rs` (toggle + editor state next to `cycle_band_scope` :990; `advanced_hint_label` :45 gains the 6th bool)
- Modify: `crates/resonance-tui/src/ui.rs` (prefs row ~:2201; band row tail :1442 area; help overlay; editor modal render)

Keys: verify `y`/`Y` are unbound in the graph/table panels (grep the key
match in app.rs); if taken pick the nearest free pair and document in help.
`y` = toggle dynamics on selected band (DEFAULT params ↔ off; no-op +
status hint on non-peaking). `Y` = modal editor: 4 rows, ↑↓ select, ←→
adjust (threshold ±1, range ±0.5, attack ±1, release ±10, spec clamps),
Esc/Enter close; sends `SetBandDynamics` per adjust. Band row tail appends
`dyn` marker when set (mirror scope tail :1442). Prefs row gates the marker
+ keys; `adv:` hint covers hidden-but-set.

- [ ] **Steps:** failing unit test for the toggle/no-op-on-non-peaking if the
  existing app.rs tests cover cycle_band_scope (mirror); implement; `cargo test -p resonance-tui` PASS.
- [ ] Commit `feat(tui): dynamic eq band controls behind advanced pref`.

### Task 9: Docs + full verification + PR

- [ ] `docs/ROADMAP.md`: move Dynamic EQ out of "Medium value" into the
  "already ahead" list (one line, mirror the mid/side entry style).
- [ ] Linux: `make check` (root Makefile) → green.
- [ ] Cross-target clippy at stable 1.96 from Linux for msvc + darwin
  (CI-parity lesson; skip ring-blocked crates as documented in memory).
- [ ] Windows VM (`ssh -p 2222 Docker@127.0.0.1`, key `~/.ssh/resonance_winvm`):
  detached `git checkout -f <sha>`, `cargo test --all` + `cargo test -p resonance-apo -- --test-threads=1`.
- [ ] macOS (`ssh nyverino@100.67.78.90`): detached checkout, `cargo test --all` + clippy.
- [ ] Push branch, `gh pr create` (body notes: Linux live verify deferred to
  the user's audio-cut window), merge on green, then watch the **post-merge**
  `windows-installer` run via `gh run list` conclusion fields.
