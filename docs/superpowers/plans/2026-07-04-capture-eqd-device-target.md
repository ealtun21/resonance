# Capture EQ'd device as target — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an action that saves the current reference-mode Result (loaded measurement shaped by the live EQ) as a reusable target curve, so a different device can later be EQ'd to match that already-EQ'd sound.

**Architecture:** One new pure function in the shared `resonance-reference` crate builds the Result as a mean-removed `RefCurve`; it persists through the existing `write_target`/`save_target` library machinery. Both clients get a "capture" control that prompts for a name (default derived from the measurement) and calls it — GUI via a new `RefCtl` popup in the Measurements section, TUI via the existing settings text-input infra.

**Tech Stack:** Rust workspace; `resonance-reference` (pure), `resonance-gui` (egui/eframe), `resonance-tui` (ratatui). No new dependencies.

## Global Constraints

- Conventional Commits, all lowercase.
- `make check` (fmt --check + clippy -D warnings + `cargo test --all`) must pass before every commit.
- Clippy pedantic is enforced workspace-wide — no new warnings.
- No AI-related content anywhere (code, comments, commit messages, docs).
- No `Co-Authored-By` / AI-attribution trailer on commits in this repo.
- f64 throughout the DSP/curve math.
- Functional style preferred (iterators, closures).
- No IPC, daemon, preset, or CLI changes — this feature is entirely client-side.

---

### Task 1: Core — `result_curve` + default-name helper (shared crate)

**Files:**
- Modify: `crates/resonance-reference/src/reference.rs` (struct `ReferenceState` ~line 188, its `Default` impl ~line 250, `impl ReferenceState` block ~line 282, test module near line 929)

**Interfaces:**
- Consumes: `resonance_ipc::BandState`; `resonance_ipc::fr::{LOG_MIN, LOG_MAX, response_db}` (already imported at the top of the file); `resonance_ipc::curve::RefCurve` (`from_points`, `interp`, `points`); `self.measurement: Option<RefCurve>`, `self.measurement_name: String`.
- Produces:
  - `ReferenceState::result_curve(&self, bands: &[BandState], sample_rate: f64) -> Option<RefCurve>`
  - `ReferenceState::eqd_target_default_name(&self) -> String`
  - `ReferenceState.capture_name: String` (transient scratch for the GUI name field; not persisted — `ReferenceState` has no serde derive, so no attribute is needed)

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `crates/resonance-reference/src/reference.rs` (there is an existing `curve(offset)` helper in that module returning a small `RefCurve`; reuse it):

```rust
    fn peaking_band(freq: f64, gain_db: f64, q: f64) -> resonance_ipc::BandState {
        resonance_ipc::BandState {
            band_type: resonance_ipc::BandType::Peaking,
            freq,
            gain_db,
            q,
            enabled: true,
            channels: resonance_ipc::ChannelMask::ALL,
            slope_db_oct: 12,
            scope: resonance_ipc::BandScope::default(),
            dynamics: None,
        }
    }

    #[test]
    fn result_curve_none_without_measurement() {
        let s = ReferenceState::default();
        assert!(s.result_curve(&[], 48000.0).is_none());
    }

    #[test]
    fn result_curve_flat_eq_preserves_measurement_shape_mean_removed() {
        let mut s = ReferenceState::default();
        // Tilted synthetic measurement: -6 dB @20 Hz rising to +6 dB @20 kHz.
        s.set_measurement(
            "test".into(),
            false,
            RefCurve::from_points(vec![(20.0, -6.0), (1000.0, 0.0), (20000.0, 6.0)]),
            None,
        );
        let c = s.result_curve(&[], 48000.0).unwrap();
        // Shape-only: the grid mean is ~0.
        let mean = c.points.iter().map(|&(_, d)| d).sum::<f64>() / c.points.len() as f64;
        assert!(mean.abs() < 1e-9, "captured curve should be mean-removed, got {mean}");
        // With a flat EQ the result equals the measurement up to a constant, so
        // the span between two frequencies matches the measurement's span.
        let span_res = c.interp(20000.0) - c.interp(20.0);
        let span_meas = 6.0 - (-6.0);
        assert!((span_res - span_meas).abs() < 0.1, "span {span_res} vs {span_meas}");
    }

    #[test]
    fn result_curve_applies_eq_band() {
        let mut s = ReferenceState::default();
        // Flat measurement so the only shaping is the EQ band.
        s.set_measurement(
            "flat".into(),
            false,
            RefCurve::from_points(vec![(20.0, 0.0), (20000.0, 0.0)]),
            None,
        );
        let flat = s.result_curve(&[], 48000.0).unwrap();
        let boosted = s.result_curve(&[peaking_band(1000.0, 6.0, 1.0)], 48000.0).unwrap();
        let lift = boosted.interp(1000.0) - flat.interp(1000.0);
        assert!((lift - 6.0).abs() < 1.0, "expected ~+6 dB at Fc, got {lift}");
    }

    #[test]
    fn eqd_target_default_name_from_measurement_else_fallback() {
        let mut s = ReferenceState::default();
        assert_eq!(s.eqd_target_default_name(), "EQ'd target");
        s.set_measurement("HD650".into(), false, curve(0.0), None);
        assert_eq!(s.eqd_target_default_name(), "HD650 (EQ'd)");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p resonance-reference result_curve eqd_target 2>&1 | tail -20`
Expected: FAIL — `no method named result_curve` / `no method named eqd_target_default_name` / `no field capture_name` is not referenced yet so only the two methods error. Compilation error is an acceptable "fail".

- [ ] **Step 3: Add the `capture_name` field**

In the `pub struct ReferenceState { … }` definition (near line 188), add after the customizer adjustment fields (e.g. after `pub adj_treble: f64,`):

```rust
    /// Transient scratch for the GUI "capture EQ'd result" name field. Not
    /// persisted (this struct has no serde derive; persistence goes through
    /// `PersistedReference`).
    pub capture_name: String,
```

In `impl Default for ReferenceState` (near line 250), add to the struct literal (e.g. after `adj_treble: 0.0,` / before `profile_meas: HashMap::new(),`):

```rust
            capture_name: String::new(),
```

- [ ] **Step 4: Implement `result_curve` and `eqd_target_default_name`**

Add to the `impl ReferenceState { … }` block (e.g. just before `pub fn series(`):

```rust
    /// Build the current EQ'd-device response — the loaded measurement shaped by
    /// the EQ `bands` — as a standalone target curve, mean-removed so only the
    /// shape is stored (targets are compared by shape; broadband loudness is the
    /// daemon preamp's job). Returns `None` when no measurement is loaded, since
    /// an EQ'd-device target is only meaningful relative to a real measurement.
    #[must_use]
    pub fn result_curve(&self, bands: &[BandState], sample_rate: f64) -> Option<RefCurve> {
        let meas = self.measurement.as_ref()?;
        const N: usize = 240;
        let mut pts: Vec<(f64, f64)> = (0..N)
            .map(|i| {
                let lf = LOG_MIN + (i as f64 / (N - 1) as f64) * (LOG_MAX - LOG_MIN);
                let f = 10f64.powf(lf);
                (f, meas.interp(f) + response_db(bands, f, sample_rate))
            })
            .collect();
        let mean = pts.iter().map(|&(_, db)| db).sum::<f64>() / N as f64;
        for p in &mut pts {
            p.1 -= mean;
        }
        Some(RefCurve::from_points(pts))
    }

    /// Suggested name for a captured EQ'd target: `"<measurement> (EQ'd)"`, or a
    /// generic fallback when the measurement is unnamed.
    #[must_use]
    pub fn eqd_target_default_name(&self) -> String {
        let n = self.measurement_name.trim();
        if n.is_empty() {
            "EQ'd target".to_string()
        } else {
            format!("{n} (EQ'd)")
        }
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p resonance-reference 2>&1 | tail -20`
Expected: PASS — all four new tests green, existing tests unaffected.

- [ ] **Step 6: Lint + commit**

Run: `cargo clippy -p resonance-reference --all-targets -- -D warnings 2>&1 | tail -5`
Expected: no warnings.

```bash
git add crates/resonance-reference/src/reference.rs
git commit -m "feat(reference): capture EQ'd result as a target curve"
```

---

### Task 2: TUI — capture row + name prompt in the Reference settings tab

**Files:**
- Modify: `crates/resonance-tui/src/settings.rs` (`enum TextPurpose`; `SettingsState::max_cursor` tab-5 arm)
- Modify: `crates/resonance-tui/src/app.rs` (`settings_reference_activate` ~line 2243; `settings_confirm_text` ~line 2045)
- Modify: `crates/resonance-tui/src/ui.rs` (`render_settings_content` reference rows ~line 2185; test `reference_tab_renders_controls` ~line 3256)

**Interfaces:**
- Consumes (from Task 1): `ReferenceState::result_curve`, `ReferenceState::eqd_target_default_name`, `ReferenceState::write_target`. Also `self.bands: Vec<BandState>`, `self.state: Option<DaemonState>` (both already fields of `App`), `crate::settings::TextInput::new`, `App::set_status`.
- Produces: `TextPurpose::CaptureTarget`; a new Reference-tab row at cursor index `13`.

- [ ] **Step 1: Write the failing test**

In `crates/resonance-tui/src/ui.rs`, extend the existing `reference_tab_renders_controls` test (~line 3256) — add the new label to the set it asserts is present. Find the assertion listing reference-tab labels (it checks strings like `"Browse online"`, `"Auto-EQ"`, `"Reset customizer"`) and add a check for the capture row:

```rust
        assert!(text.contains("Capture EQ'd result"), "capture row should render");
```

(If the test renders by setting `s.tab = 5` and rasterizing the content into a buffer `text`, this matches the existing pattern. Keep the assertion alongside the others.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p resonance-tui reference_tab_renders_controls 2>&1 | tail -15`
Expected: FAIL — `capture row should render` (the label is not drawn yet).

- [ ] **Step 3: Add the `TextPurpose` variant**

In `crates/resonance-tui/src/settings.rs`, add to `pub enum TextPurpose`:

```rust
    /// Capture the current EQ'd result (measurement + EQ) into the target
    /// library under the entered name.
    CaptureTarget,
```

- [ ] **Step 4: Bump the reference tab row count**

In `crates/resonance-tui/src/settings.rs`, `SettingsState::max_cursor`, change the tab-5 arm from `5 => 12,` to `5 => 13,` and update the adjacent comment to append `/ capture`:

```rust
            // Reference: on / target / measurement / browse-online / autoeq /
            // show-meas / normalize / bounds / tilt / bass / ear / treble / reset
            // / capture.
            5 => 13,
```

- [ ] **Step 5: Render the new row**

In `crates/resonance-tui/src/ui.rs`, `render_settings_content`, the tab-5 branch builds an ordered list of rows (Reset customizer is currently the last, index 12). Append one row after it so it becomes index 13. Match the surrounding row style (a label + hint tuple, same as `("Reset customizer", …)`):

```rust
            ("Capture EQ'd result", false, "(Enter: save measurement + EQ as a target)"),
```

Use the same three-field shape the neighboring rows use (label, an active/checked bool — `false` here since it's an action, and a hint string). If the reference-row list uses a different tuple arity, match it exactly (mirror the "Reset customizer" row).

- [ ] **Step 6: Seed the name prompt when the row is activated**

In `crates/resonance-tui/src/app.rs`, `settings_reference_activate`, add a `13` arm before the `_ => {}` catch-all:

```rust
            13 => {
                if self.reference.measurement.is_some() {
                    let default = self.reference.eqd_target_default_name();
                    if let InputMode::Settings(s) = &mut self.mode {
                        s.text_input = Some(crate::settings::TextInput::new(
                            default,
                            crate::settings::TextPurpose::CaptureTarget,
                            "Target name",
                        ));
                    }
                } else {
                    self.set_status("load a measurement first");
                }
            }
```

- [ ] **Step 7: Handle the prompt commit**

In `crates/resonance-tui/src/app.rs`, `settings_confirm_text`, add a `TextPurpose::CaptureTarget` arm to the `match purpose { … }` (alongside `SaveProfile`, `ExportProfile`, …):

```rust
            TextPurpose::CaptureTarget => {
                let name = buf.trim().to_string();
                let sr = self.state.as_ref().map_or(48000.0, |s| s.sample_rate);
                if !name.is_empty() {
                    if let Some(curve) = self.reference.result_curve(&self.bands, sr) {
                        self.reference.write_target(&name, &curve);
                        self.set_status(format!("captured EQ'd target: {name}"));
                    } else {
                        self.set_status("load a measurement first");
                    }
                }
                if let InputMode::Settings(s) = &mut self.mode {
                    s.text_input = None;
                }
            }
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test -p resonance-tui 2>&1 | tail -20`
Expected: PASS — `reference_tab_renders_controls` now green, all other TUI tests unaffected.

- [ ] **Step 9: Lint + commit**

Run: `cargo clippy -p resonance-tui --all-targets -- -D warnings 2>&1 | tail -5`
Expected: no warnings.

```bash
git add crates/resonance-tui/src/settings.rs crates/resonance-tui/src/app.rs crates/resonance-tui/src/ui.rs
git commit -m "feat(tui): capture EQ'd result as a target from the reference tab"
```

---

### Task 3: GUI — capture control in the reference bar's Measurements section

**Files:**
- Modify: `crates/resonance-gui/src/ui/reference_bar.rs` (`enum RefCtl` ~line 40; `REF_LAYOUT` table ~line 57; the `ref_ctl_width` / `ref_ctl_inline` / `ref_ctl_menu` / label `match RefCtl` arms; add two methods near `meas_to_target` ~line 607)

**Interfaces:**
- Consumes (from Task 1): `ReferenceState::result_curve`, `ReferenceState::eqd_target_default_name`, `ReferenceState::save_target`, `ReferenceState.capture_name`. Also `self.state: Option<DaemonState>` (has `.bands: Vec<BandState>` and `.sample_rate: f64`), `self.set_status`, `kit::pill_popup`, `kit::text_field`, `kit::button_tip`, `Icon::Save`.
- Produces: `RefCtl::CaptureResult` variant + its rendering; `GuiApp::capture_result_to_target`.

Note: `RefCtl` matches are exhaustive, so adding the variant makes `cargo check` fail until every match handles it — use that to find each site. Model each new arm on the existing `RefCtl::ToTarget` (the raw-measurement sibling) or `RefCtl::Customize` (the popup sibling).

- [ ] **Step 1: Add the enum variant**

In `crates/resonance-gui/src/ui/reference_bar.rs`, add to `enum RefCtl` (after `ToTarget,`):

```rust
    CaptureResult,
```

- [ ] **Step 2: Verify the build now fails on non-exhaustive matches**

Run: `cargo check -p resonance-gui 2>&1 | grep -E "non-exhaustive|RefCtl" | head`
Expected: errors listing each `match` over `RefCtl` that must handle `CaptureResult` (the layout table, `ref_ctl_width`, `ref_ctl_inline`, `ref_ctl_menu`, the label match, and any list at lines ~1579 / ~1795).

- [ ] **Step 3: Register it in the layout table**

In the `REF_LAYOUT` array (~line 57), add an entry in the Measurements section next to `ToTarget`. Priority `11` is unused; it collapses into the `…`/`☰` menu first among Measurements controls when space is tight (still reachable):

```rust
    (RefCtl::CaptureResult, Section::Measurements, false, 11),
```

- [ ] **Step 4: Add the label + inline + menu + width arms**

In the label `match c { … }` (~line 266, the one producing `icon_label`/`icon_only`/`check`/`label_only`), add:

```rust
            RefCtl::CaptureResult => icon_label("Capture EQ'd"),
```

In `ref_ctl_inline` (~line 294, the `match c` dispatching to render methods), add:

```rust
            RefCtl::CaptureResult => self.ref_capture_button(ui),
```

In `ref_ctl_menu` (~line 525), render the same popup-style control as the inline path (mirror how `RefCtl::Customize` is handled in that match — if `Customize` renders its body inline in the menu, do likewise; otherwise call `self.ref_capture_button(ui)`):

```rust
            RefCtl::CaptureResult => self.ref_capture_button(ui),
```

For `ref_ctl_width` (~line 260) and any remaining match (lines ~1579, ~1795 — these group popup-like controls; `Customize` appears there), add `RefCtl::CaptureResult` wherever `RefCtl::Customize` appears (it is also a `pill_popup`), or to the default arm if that fits the pattern at that site. Let the compiler errors from Step 2 drive exactly which sites need it.

- [ ] **Step 5: Implement the button + capture methods**

In `crates/resonance-gui/src/ui/reference_bar.rs`, add near `meas_to_target` (~line 607):

```rust
    /// "Capture EQ'd result" — a popup with a name field (seeded from the
    /// measurement) and a Save button that bakes the current measurement+EQ into
    /// the target library. Sibling of `meas_to_target`, which saves the raw
    /// (un-EQ'd) measurement.
    fn ref_capture_button(&mut self, ui: &mut egui::Ui) {
        let id = ui.make_persistent_id("ref_capture_pop");
        kit::pill_popup(
            ui,
            Some(Icon::Save),
            "Capture EQ'd",
            "Save the current EQ'd result (measurement + EQ) as a reusable target",
            id,
            300.0,
            |ui| self.ref_capture_body(ui),
        );
    }

    fn ref_capture_body(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new("Save the current EQ'd result (measurement + EQ) as a target.")
                .size(kit::T_CAPTION)
                .weak(),
        );
        ui.add_space(kit::SP_XS);
        // Seed the name field from the measurement the first time it's empty.
        if self.reference.capture_name.trim().is_empty() {
            self.reference.capture_name = self.reference.eqd_target_default_name();
        }
        let has_meas = self.reference.measurement.is_some();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = kit::SP_S;
            kit::text_field(
                ui,
                180.0,
                ui.make_persistent_id("ref_capture_name"),
                &mut self.reference.capture_name,
                "target name…",
                false,
            );
            if kit::button_tip(
                ui,
                "Save",
                true,
                has_meas,
                "Capture the EQ'd result into the target library",
            ) {
                self.capture_result_to_target();
            }
        });
    }

    fn capture_result_to_target(&mut self) {
        let Some((bands, sr)) = self
            .state
            .as_ref()
            .map(|st| (st.bands.clone(), st.sample_rate))
        else {
            self.set_status("no daemon connection");
            return;
        };
        let name = {
            let n = self.reference.capture_name.trim();
            if n.is_empty() {
                self.reference.eqd_target_default_name()
            } else {
                n.to_string()
            }
        };
        if let Some(curve) = self.reference.result_curve(&bands, sr) {
            self.reference.save_target(&name, &curve);
            self.reference.capture_name.clear();
            self.set_status(format!("captured EQ'd target: {name}"));
        } else {
            self.set_status("load a measurement first");
        }
    }
```

If `kit::pill_popup`'s closure argument requires a non-capturing signature or a different arity than shown (check its definition near `kit.rs:624` used by `ref_customize_button`), match `ref_customize_button`'s exact call shape — it is the proven template.

- [ ] **Step 6: Build + run the full check**

Run: `cargo check -p resonance-gui 2>&1 | tail -5`
Expected: clean (no non-exhaustive-match errors remain).

Run: `make check 2>&1 | tail -20`
Expected: fmt clean, clippy `-D warnings` clean, `cargo test --all` all green.

- [ ] **Step 7: Commit**

```bash
git add crates/resonance-gui/src/ui/reference_bar.rs
git commit -m "feat(gui): capture EQ'd result as a target from the reference bar"
```

---

## Verification note

GUI/TUI rendering can't be exercised headless in this environment (consistent with prior reference work). Correctness rests on the Task 1 unit tests (`result_curve` shape/EQ/None behavior + default name) plus the TUI render test, and on `make check` compiling every match site. Where a display is available, sanity-check the GUI popup and the TUI Reference-tab row (`s` → tab `6` → last row) manually. No audio path is touched, so `resonance verify` is not applicable.
```

## Self-review

**1. Spec coverage:**
- result_curve (meas+EQ, mean-removed, None w/o measurement) → Task 1 ✓
- Default name "<meas> (EQ'd)" → Task 1 `eqd_target_default_name` ✓
- Persist via write_target/save_target → Tasks 2 (write_target) + 3 (save_target) ✓
- GUI Measurements-section control + name prompt → Task 3 ✓
- TUI reference-tab row + TextPurpose prompt → Task 2 ✓
- No CLI/IPC/daemon changes → constraints + no such tasks ✓
- Tests (flat, +6 dB, None, default-name) → Task 1 ✓

**2. Placeholder scan:** no TBD/TODO; every code step has full code.

**3. Type consistency:** `result_curve(&[BandState], f64) -> Option<RefCurve>` and `eqd_target_default_name(&self) -> String` used identically in Tasks 2 & 3; `capture_name` defined Task 1, consumed Task 3; `TextPurpose::CaptureTarget` defined & consumed Task 2; `RefCtl::CaptureResult` defined & consumed Task 3.
