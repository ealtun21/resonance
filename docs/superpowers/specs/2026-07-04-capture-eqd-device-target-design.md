# Capture EQ'd device as a target curve

**Date:** 2026-07-04
**Status:** approved (design)

## Problem

Reference mode overlays a device **measurement** and a **target** on the FR
graph, and shows a bold **Result** line = measurement shaped by the current EQ.
That Result is the actual response the user has dialed in on the current device.

Today there is no way to keep that Result. A user who tunes device A to a sound
they like cannot reuse that tuning as a *goal* for a different device — the only
saveable target is the customizer's edited *target curve*, not the *achieved
result*.

## Goal

Add an action that captures the current Result (`measurement + EQ`) and saves it
as a target curve in the user's target library. Later, with a different device's
measurement loaded, the user selects that saved target and EQs the new device
(manually or via Auto-EQ) so it matches the tonal balance of the first,
already-EQ'd device.

## Non-goals

- CLI support. Reference/measurement state is client-side only (GUI + TUI); the
  daemon and CLI have no measurement. Out of scope.
- Provenance metadata (source measurement name / date / customizer settings
  baked into the file). YAGNI for the first cut.
- Baking effects (Fidelity, Ambience, Surround, …) into the captured curve.
  Target curves are tonal frequency-response only; reference mode already ignores
  the effects for the Result line. The captured curve is EQ-band FR only.
- Separate left/right target files. Capture uses the already-resolved active
  measurement curve (`ReferenceState.measurement`) — the same curve the Result
  line is drawn from and the same one the user sees.

## Approach

Reuse the existing target-save machinery (`write_target` / `save_target`,
`user_curve_dir`, the "Your targets" library, the target dropdown). The only new
core logic is computing the Result as a standalone `RefCurve`.

### Core — `resonance-reference` crate (pure, unit-tested)

New method on `ReferenceState` (in `crates/resonance-reference/src/reference.rs`):

```rust
/// Build the current EQ'd-device response (measurement shaped by the EQ) as a
/// standalone target curve. Returns None when no measurement is loaded — a
/// device target is only meaningful relative to a real measurement.
pub fn result_curve(&self, bands: &[BandState], sample_rate: f64) -> Option<RefCurve>;
```

Behavior:

1. Return `None` if `self.measurement.is_none()` (matches the `active()`
   requirement that a measurement exist).
2. Sample a log-frequency grid from `LOG_MIN..=LOG_MAX` (reuse the crate
   constants), ~240 points to match the FR-graph sampling density.
3. For each grid frequency `f`:
   `db(f) = self.measurement.interp(f) + response_db(bands, f, sample_rate)`.
   This is the same expression `series()` uses for the Result line, minus the
   view-relative `na`/`base` normalization and the `off_m`/`off_eq` display
   offsets.
4. Mean-remove: subtract the arithmetic mean of `db(f)` over the grid so the
   stored curve is **shape only**. Rationale: targets are compared by shape
   (comparison paths already mean-remove), and broadband loudness is owned by the
   daemon preamp, not the target. This keeps the captured target at a neutral
   0-mean level regardless of the EQ's DC/headroom term.
5. Return `RefCurve::from_points(pts)`.

Persist through the existing `save_target(name, &curve)` (writes via
`write_target`, resets the customizer, selects the new target) or
`write_target(name, &curve)` directly. No new file format — the standard
`frequency,raw` `.txt` in `user_curve_dir()`. The saved target appears in "Your
targets", the target dropdown, and is usable as an Auto-EQ / manual-EQ goal for
any other measurement.

Default suggested name: `"<measurement name> (EQ'd)"`, falling back to
`"EQ'd target"` when the measurement has no name.

### GUI — `crates/resonance-gui/src/ui/reference_bar.rs`

Add a "Capture EQ'd result" action in the **Measurements** section of the
reference bar (near the measurement chip — the capture is about the loaded
measurement, not the customizer).

- Enabled only when a measurement is loaded (`result_curve` would return `Some`).
- Opens a small inline name field (mirror the customizer's `kit::text_field` +
  Save pattern at `reference_bar.rs:805`), pre-filled with the default name.
- Save → call `result_curve(bands, sample_rate)`; on `Some(curve)` →
  `save_target(name, &curve)` and a status toast (`set_status`). The `bands` /
  `sample_rate` are already available where `series()` is invoked.
- Distinct label + tooltip so it is not confused with the customizer's existing
  "Save" (which bakes the edited *target* curve). Tooltip: "Save the current
  EQ'd result (measurement + EQ) as a reusable target."
- Icon: reuse the existing `Save` vector icon with the distinct label, or a
  capture-appropriate icon from `icons.rs` if one reads more clearly.

Wire it into the section-collapse layout (`REF_LAYOUT` / `RefCtl`) like the other
Measurements-section controls so it participates in overflow.

### TUI — settings Reference tab + existing text-input infra

The Settings screen already has a name-prompt mechanism —
`SettingsState.text_input: Option<TextInput>` with a `TextPurpose` enum, an
overlay renderer (`render_text_input_overlay`), and full key handling
(`settings_confirm_text` on Enter, Esc clears). Reuse it — no new `InputMode`.

- Add `TextPurpose::CaptureTarget` (in `settings.rs`).
- Add a new row to the Reference tab (tab 5) after Reset:
  **row 13 "Capture EQ'd result"** (append at the end so existing rows 0–12 keep
  their indices; bump `SettingsState::max_cursor` for tab 5 from `12` to `13`,
  and add the row label in `render_settings_content`).
- `settings_reference_activate` cursor `13`: if a measurement is loaded, seed
  `s.text_input = Some(TextInput::new(default_name, TextPurpose::CaptureTarget,
  "Target name"))`, where `default_name` = `"<measurement_name> (EQ'd)"`
  (fallback `"EQ'd target"`). No measurement → `set_status` a hint and do
  nothing.
- `settings_confirm_text` arm `TextPurpose::CaptureTarget`: compute
  `result_curve(&self.bands, sample_rate)` (bands from `self.bands`, sample rate
  from `self.state.as_ref().map(|s| s.sample_rate)`), `write_target(name, &curve)`
  on `Some`, `set_status`, clear `text_input`.

`self.bands` and `self.state.sample_rate` are already the fields feeding the
reference overlay in `render_eq_curve`.

## Data flow

```
[measurement loaded] + [EQ bands]
        │  result_curve(bands, sample_rate)
        ▼
  RefCurve (log grid, db = meas.interp(f) + response_db(bands,f,sr), mean-removed)
        │  save_target / write_target  (name: "<meas> (EQ'd)")
        ▼
  <user_curve_dir>/<name>.txt   (frequency,raw)  → "Your targets" library
        │  select as target on a DIFFERENT measurement
        ▼
  Auto-EQ or manual EQ device B to match  → device B ≈ EQ'd device A (tonal shape)
```

## Testing

Unit tests on `result_curve` in `resonance-reference`:

- **No measurement → `None`.**
- **Flat EQ (no bands / all-zero gain):** returned curve equals the measurement
  shape, mean-removed (mean ≈ 0; point-to-point deltas match the measurement's).
- **+6 dB peaking band at Fc:** curve at Fc is ~+6 dB above the flat-EQ curve at
  Fc (within filter tolerance), confirming the EQ is applied.
- **Mean is ~0** for any band set (shape-only invariant).

`make check` (fmt + clippy -D warnings + `cargo test --all`).

GUI/TUI rendering is not runnable headless here (consistent with prior reference
work); correctness rests on the shared-crate unit tests plus the existing Xvfb
screenshot harness for a visual sanity check where practical.

## Files touched

- `crates/resonance-reference/src/reference.rs` — new `result_curve` + tests;
  transient `capture_name: String` (`#[serde(skip)]`) for the GUI popup field.
- `crates/resonance-gui/src/ui/reference_bar.rs` — capture action (popup +
  name field + Save) in the Measurements section + layout wiring.
- `crates/resonance-tui/src/settings.rs` — `TextPurpose::CaptureTarget`; tab-5
  `max_cursor` 12 → 13.
- `crates/resonance-tui/src/app.rs` — capture row in `settings_reference_activate`
  + `TextPurpose::CaptureTarget` arm in `settings_confirm_text`.
- `crates/resonance-tui/src/ui.rs` — Reference-tab "Capture EQ'd result" row label.

No IPC, daemon, preset, or CLI changes.
