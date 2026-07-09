# GUI Basic/Advanced Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a beginner "Basic" UI mode to `resonance-gui` — power, preset carousel, vertical gain sliders, five friendly-named effect sliders — alongside the untouched current UI ("Advanced"), with a one-time first-run chooser.

**Architecture:** New `UiMode { Basic, Advanced }` pref on `GuiApp`, persisted through eframe storage like the existing `show_*` bools. `render_panels` dispatches: Basic draws a self-contained screen in a new `ui/basic.rs`; Advanced runs the existing toolbar/shell/statusbar path unchanged. Basic reuses the existing IPC command path (`queue_edit`/`queue`), undo system, and painter-drawn kit widgets, so all nine themes apply automatically.

**Tech Stack:** Rust, egui/eframe, bespoke widget kit (`ui/kit.rs`), `resonance-ipc` types.

**Spec:** `docs/superpowers/specs/2026-07-09-gui-basic-mode-design.md`

## Global Constraints

- Run `make check` (fmt --check + clippy `-D warnings` pedantic + test --all) before **every** commit.
- Conventional Commits, all lowercase. **No AI-related content anywhere** (no Co-Authored-By trailers, no AI mentions in code/comments/commits).
- Workspace clippy pedantic is enforced; `float_cmp` is active (compare via `.abs() > f64::EPSILON`), cast lints are blanket-allowed.
- Panels never use `egui::Slider`/`Button`/`Checkbox` directly — only `ui/kit.rs` widgets (kit module doc rule).
- All DSP/sample values `f64`. Functional style preferred.
- No IPC, daemon, or DSP changes. `resonance-tui`/CLI untouched.
- Tests: `cargo test -p resonance-gui` (crate has no `[dev-dependencies]`; plain `#[cfg(test)] mod tests` with `use super::*;` is the house style, see `app.rs:1765`).

## Reference: exact names used throughout (verified against source)

- Commands (`resonance-ipc`): `Command::SetBand { index, freq, gain_db, q }`, `Command::SetEffectIntensity { effect, value }`, `Command::SetEffectEnabled { effect, enabled }`, `Command::SetPower { enabled }`, `Command::LoadProfile { name }`.
- `FxEffectId` (NOT `FxEffect`): variants `Fidelity, Ambience, Surround, DynamicBoost, Bass, Loudness, Crossfeed`; helpers `id.label()`, `id.min()` (−1.0 bipolar / 0.0), `id.is_bipolar()`.
- `BandState { band_type, freq, gain_db, q, enabled, channels: ChannelMask, slope_db_oct: u8, scope: BandScope, dynamics: Option<BandDynamics> }`; `BandType::{Peaking, LowShelf, HighShelf, LowPass, HighPass, BandPass, Notch, AllPass}`.
- `state.effects.get(id) -> (f64, bool)` (intensity, enabled). `state.current_preset: Option<String>`. `state.enabled: bool` (master power).
- `GuiApp` helpers: `self.queue_edit(cmd)` (undo snapshot + `dirty = true` + send), `self.queue(cmd)` (plain send), `self.profiles: Vec<String>`, `self.dirty: bool`, `self.dialog: Dialog`.
- Gain range: `crate::state::GAIN_LIMIT` = 40.0 (bands table uses `-GAIN_LIMIT..=GAIN_LIMIT`).
- Kit: `kit::tokens(ui)` → `t.text/t.dim/t.faint/t.well/t.accent`; spacing `SP_XS/S/M/L`, `CTRL_H`, `T_BODY`, `T_CAPTION`; widgets `button_tip`, `button_sized`, `icon_btn`, `slider_h`.
- Profile load pattern (devices.rs:406-410): `self.queue(Command::LoadProfile { name }); self.dirty = false;`

---

### Task 1: `UiMode` enum + persistence + first-run detection

**Files:**
- Modify: `crates/resonance-gui/src/state.rs` (add `UiMode`, add `Dialog::ModeChoice`, add test mod)
- Modify: `crates/resonance-gui/src/app.rs` (field, load in `new()`, save in `save()`)

**Interfaces:**
- Consumes: eframe storage pattern (`app.rs:767` load / `app.rs:1336` save).
- Produces: `crate::state::UiMode` with `UiMode::from_storage(&str) -> UiMode` and `UiMode::as_storage(self) -> &'static str`; `GuiApp.ui_mode: UiMode` field; `Dialog::ModeChoice` variant. Later tasks match on `self.ui_mode == UiMode::Basic`.

- [ ] **Step 1: Write the failing test**

Append to `crates/resonance-gui/src/state.rs` (file currently has no test module):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_mode_storage_round_trip() {
        assert_eq!(UiMode::from_storage(UiMode::Basic.as_storage()), UiMode::Basic);
        assert_eq!(
            UiMode::from_storage(UiMode::Advanced.as_storage()),
            UiMode::Advanced
        );
        // Unknown/corrupt stored values fall back to the full editor.
        assert_eq!(UiMode::from_storage("garbage"), UiMode::Advanced);
        assert_eq!(UiMode::from_storage(""), UiMode::Advanced);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p resonance-gui ui_mode_storage_round_trip`
Expected: compile error — `UiMode` not found.

- [ ] **Step 3: Implement `UiMode` + `Dialog::ModeChoice`**

In `crates/resonance-gui/src/state.rs`, above the `Dialog` enum:

```rust
/// Which UI the window shows: the beginner screen or the full editor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum UiMode {
    Basic,
    Advanced,
}

impl UiMode {
    pub(crate) fn from_storage(s: &str) -> Self {
        if s == "basic" {
            UiMode::Basic
        } else {
            UiMode::Advanced
        }
    }

    pub(crate) fn as_storage(self) -> &'static str {
        match self {
            UiMode::Basic => "basic",
            UiMode::Advanced => "advanced",
        }
    }
}
```

Add a variant to `Dialog` (state.rs:32, after `None`):

```rust
    /// First-run choice between Basic and Advanced mode (shown once, when no
    /// `ui_mode` pref is stored yet).
    ModeChoice,
```

- [ ] **Step 4: Wire the field into `GuiApp`**

In `crates/resonance-gui/src/app.rs`:

(a) Add the field after `show_ir: bool` (~line 348):

```rust
    /// Basic (beginner) vs Advanced (full editor) UI. Persisted; when the
    /// stored key is absent (first run) a one-time chooser dialog is shown.
    pub(crate) ui_mode: UiMode,
```

(b) Make sure `UiMode` is imported where `Dialog`/`Confirm` are imported from `crate::state` (extend the existing `use crate::state::{...}` line).

(c) In `GuiApp::new()`, in the struct literal near the other pref loads (~line 767):

```rust
            ui_mode: cc
                .storage
                .and_then(|s| s.get_string("ui_mode"))
                .map_or(UiMode::Advanced, |s| UiMode::from_storage(&s)),
```

(d) Still in `new()`, find the existing `dialog: Dialog::None,` initializer and replace it with:

```rust
            dialog: if cc.storage.and_then(|s| s.get_string("ui_mode")).is_some() {
                Dialog::None
            } else {
                Dialog::ModeChoice
            },
```

(e) In `fn save` (~app.rs:1336), next to the other `set_string` calls:

```rust
        storage.set_string("ui_mode", self.ui_mode.as_storage().to_string());
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p resonance-gui ui_mode_storage_round_trip`
Expected: PASS. (A `Dialog::ModeChoice`-is-never-constructed warning cannot occur — it is constructed in `new()`.)

- [ ] **Step 6: `make check`, commit**

```bash
make check
git add crates/resonance-gui/src/state.rs crates/resonance-gui/src/app.rs
git commit -m "feat(gui): add ui mode pref with first-run detection"
```

---

### Task 2: Mode dispatch, first-run chooser dialog, mode-switch buttons

**Files:**
- Create: `crates/resonance-gui/src/ui/basic.rs` (screen skeleton)
- Modify: `crates/resonance-gui/src/ui/mod.rs` (declare module)
- Modify: `crates/resonance-gui/src/app.rs` (`render_panels` dispatch, `render_dialogs` entry)
- Modify: `crates/resonance-gui/src/ui/dialogs.rs` (`mode_choice_dialog`)
- Modify: `crates/resonance-gui/src/ui/toolbar.rs` (`tb_power` visibility, `tb_basic` button)

**Interfaces:**
- Consumes: `UiMode`, `Dialog::ModeChoice` (Task 1); `self.disconnected(ui)` (layout.rs:61, already `pub(crate)`); `kit::button_tip`, `kit::button_sized`; `dialog_window` (widgets.rs:58).
- Produces: `GuiApp::basic_screen(&mut self, ui: &mut egui::Ui)` — the Basic root, later tasks fill it; `tb_power` becomes `pub(crate)`.

- [ ] **Step 1: Create the Basic screen skeleton**

Create `crates/resonance-gui/src/ui/basic.rs`:

```rust
//! Basic mode: the beginner screen — power, preset carousel, per-band gain
//! sliders and friendly-named effect sliders. No graph, no numbers beyond
//! frequency labels. Advanced mode (the full editor) is untouched by this
//! module; both share the same IPC command path and undo stack.

use crate::app::GuiApp;
use crate::state::UiMode;
use crate::ui::kit;
use eframe::egui;
use resonance_ipc::DaemonState;

impl GuiApp {
    pub(crate) fn basic_screen(&mut self, ui: &mut egui::Ui) {
        let Some(state) = self.state.clone() else {
            self.disconnected(ui);
            return;
        };
        egui::ScrollArea::vertical()
            .id_salt("basic_screen")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let max_w = 640.0_f32.min(ui.available_width());
                ui.vertical_centered(|ui| {
                    ui.set_max_width(max_w);
                    ui.add_space(kit::SP_L);
                    self.basic_header(ui, &state);
                });
            });
    }

    /// Top row: power pill left, mode switch right.
    fn basic_header(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        ui.horizontal(|ui| {
            self.tb_power(ui, Some(state));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if kit::button_tip(ui, "Advanced", false, true, "Switch to the full editor") {
                    self.ui_mode = UiMode::Advanced;
                }
            });
        });
    }
}
```

- [ ] **Step 2: Declare the module**

In `crates/resonance-gui/src/ui/mod.rs`, add `pub(crate) mod basic;` (or plain `mod basic;` — match the visibility style of the existing module lines exactly).

- [ ] **Step 3: Make `tb_power` reachable from basic.rs**

In `crates/resonance-gui/src/ui/toolbar.rs:204`, change:

```rust
    fn tb_power(&mut self, ui: &mut egui::Ui, state: Option<&DaemonState>) {
```

to:

```rust
    pub(crate) fn tb_power(&mut self, ui: &mut egui::Ui, state: Option<&DaemonState>) {
```

- [ ] **Step 4: Dispatch on mode in `render_panels`**

In `crates/resonance-gui/src/app.rs:1543`, at the top of `fn render_panels`:

```rust
    fn render_panels(&mut self, ui: &mut egui::Ui) {
        if self.ui_mode == UiMode::Basic {
            egui::CentralPanel::default().show_inside(ui, |ui| self.basic_screen(ui));
            return;
        }
        // ... existing body unchanged ...
```

- [ ] **Step 5: First-run chooser dialog**

In `crates/resonance-gui/src/ui/dialogs.rs`, add (near `settings_dialog`, same impl block; `Dialog`, `kit`, and `dialog_window` are already imported in this file — extend the `use crate::state::...` import with `UiMode`):

```rust
    /// One-time first-run chooser (no close button: a choice must be made).
    /// Shown when no `ui_mode` pref existed at startup; never again after.
    pub(crate) fn mode_choice_dialog(&mut self, ctx: &egui::Context) {
        if !matches!(self.dialog, Dialog::ModeChoice) {
            return;
        }
        dialog_window(ctx, "Welcome to Resonance")
            .resizable(false)
            .show(ctx, |ui| {
                ui.add_space(kit::SP_M);
                ui.label("How much control do you want? You can switch anytime.");
                ui.add_space(kit::SP_M);
                let size = egui::vec2(300.0, 44.0);
                if kit::button_sized(ui, "Simple — presets and sliders", true, true, size, 15.0) {
                    self.ui_mode = UiMode::Basic;
                    self.dialog = Dialog::None;
                }
                ui.add_space(kit::SP_S);
                if kit::button_sized(ui, "Advanced — full parametric EQ", false, true, size, 15.0)
                {
                    self.ui_mode = UiMode::Advanced;
                    self.dialog = Dialog::None;
                }
                ui.add_space(kit::SP_M);
            });
    }
```

Register it first in `render_dialogs` (`app.rs:1564`):

```rust
    fn render_dialogs(&mut self, ctx: &egui::Context) {
        self.mode_choice_dialog(ctx);
        self.preset_dialog(ctx);
        // ... rest unchanged ...
```

- [ ] **Step 6: "Basic" button in the Advanced toolbar**

In `crates/resonance-gui/src/ui/toolbar.rs`, add next to `tb_settings` (~line 383):

```rust
    fn tb_basic(&mut self, ui: &mut egui::Ui) {
        if kit::button_tip(ui, "Basic", false, true, "Switch to Basic mode") {
            self.ui_mode = UiMode::Basic;
        }
    }
```

(Import `UiMode` in toolbar.rs's `use crate::state::...` line.)

Call it in the right-aligned cluster (toolbar.rs:163-179), after `self.tb_settings(ui);` so it renders left of the gear:

```rust
                self.tb_settings(ui);
                Self::tb_sep(ui);
                self.tb_basic(ui);
```

Then find the toolbar collapse-width budget (constants `w_settings`/`w_help = 28.0` around toolbar.rs:92-94 and the `base` sum around line 105) and add a term for the new button, e.g. `let w_basic = 64.0;` added into `base`, so narrow-window collapse thresholds still account for every right-cluster control.

- [ ] **Step 7: Verify by compile + run**

```bash
cargo clippy -p resonance-gui --all-targets -- -D warnings
cargo run -p resonance-gui
```

Manual check: toolbar shows "Basic"; clicking it swaps to the Basic skeleton (power pill + "Advanced" button); "Advanced" swaps back; restart remembers the mode. To see the first-run dialog, temporarily clear eframe storage (`rm ~/.local/share/resonance-gui/app.ron` — verify the exact eframe storage path with `find ~/.local/share -name '*.ron' -path '*resonance*'`) and relaunch: chooser appears, pick one, relaunch again → no chooser.

- [ ] **Step 8: `make check`, commit**

```bash
make check
git add -A crates/resonance-gui
git commit -m "feat(gui): basic mode shell, first-run chooser and mode switch"
```

---

### Task 3: Pure helpers — band filter, freq labels, carousel step, fx labels

**Files:**
- Modify: `crates/resonance-gui/src/ui/basic.rs` (helpers + tests)

**Interfaces:**
- Consumes: `resonance_ipc::{BandScope, BandState, BandType, ChannelMask, FxEffectId}`.
- Produces (free functions in `ui/basic.rs`, used by Tasks 5-7):
  - `pub(crate) const BASIC_FX: [FxEffectId; 5]`
  - `pub(crate) fn basic_fx_label(id: FxEffectId) -> &'static str`
  - `pub(crate) fn gain_slider_bands(bands: &[BandState]) -> Vec<usize>`
  - `pub(crate) fn fmt_freq_short(freq: f64) -> String`
  - `pub(crate) fn carousel_step(profiles: &[String], current: Option<&str>, dir: i32) -> Option<String>`

- [ ] **Step 1: Write the failing tests**

Append to `crates/resonance-gui/src/ui/basic.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use resonance_ipc::{BandScope, ChannelMask};

    fn band(band_type: BandType) -> BandState {
        BandState {
            band_type,
            freq: 1000.0,
            gain_db: 0.0,
            q: 1.0,
            enabled: true,
            channels: ChannelMask::default(),
            slope_db_oct: 12,
            scope: BandScope::default(),
            dynamics: None,
        }
    }

    #[test]
    fn gain_slider_bands_keeps_only_gain_capable_types() {
        let bands = vec![
            band(BandType::Peaking),   // 0: yes
            band(BandType::HighPass),  // 1: no
            band(BandType::LowShelf),  // 2: yes
            band(BandType::Notch),     // 3: no
            band(BandType::HighShelf), // 4: yes
            band(BandType::AllPass),   // 5: no
            band(BandType::LowPass),   // 6: no
            band(BandType::BandPass),  // 7: no
        ];
        assert_eq!(gain_slider_bands(&bands), vec![0, 2, 4]);
        assert!(gain_slider_bands(&[]).is_empty());
    }

    #[test]
    fn fmt_freq_short_formats_hz_and_khz() {
        assert_eq!(fmt_freq_short(60.0), "60");
        assert_eq!(fmt_freq_short(150.0), "150");
        assert_eq!(fmt_freq_short(999.0), "999");
        assert_eq!(fmt_freq_short(1000.0), "1k");
        assert_eq!(fmt_freq_short(1500.0), "1.5k");
        assert_eq!(fmt_freq_short(2400.0), "2.4k");
        assert_eq!(fmt_freq_short(15000.0), "15k");
        assert_eq!(fmt_freq_short(20000.0), "20k");
        // Near-integer kHz snaps to the round label.
        assert_eq!(fmt_freq_short(1049.0), "1k");
    }

    #[test]
    fn carousel_step_wraps_and_handles_missing_current() {
        let p: Vec<String> = ["Rock", "Jazz", "Flat"].map(String::from).to_vec();
        assert_eq!(carousel_step(&p, Some("Rock"), 1).as_deref(), Some("Jazz"));
        assert_eq!(carousel_step(&p, Some("Flat"), 1).as_deref(), Some("Rock")); // wrap fwd
        assert_eq!(carousel_step(&p, Some("Rock"), -1).as_deref(), Some("Flat")); // wrap back
        assert_eq!(carousel_step(&p, None, 1).as_deref(), Some("Rock")); // none → first
        assert_eq!(carousel_step(&p, None, -1).as_deref(), Some("Flat")); // none → last
        assert_eq!(carousel_step(&p, Some("Gone"), 1).as_deref(), Some("Rock"));
        assert_eq!(carousel_step(&[], Some("Rock"), 1), None);
    }

    #[test]
    fn basic_fx_labels_are_beginner_friendly() {
        assert_eq!(basic_fx_label(FxEffectId::Bass), "Bass");
        assert_eq!(basic_fx_label(FxEffectId::Fidelity), "Clarity");
        assert_eq!(basic_fx_label(FxEffectId::Ambience), "Ambience");
        assert_eq!(basic_fx_label(FxEffectId::Surround), "Wide");
        assert_eq!(basic_fx_label(FxEffectId::DynamicBoost), "Boost");
        assert_eq!(BASIC_FX.len(), 5);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p resonance-gui basic`
Expected: compile errors — the helper functions do not exist yet.

- [ ] **Step 3: Implement the helpers**

Add to `crates/resonance-gui/src/ui/basic.rs` (module level, above the `impl GuiApp` block; extend the `use resonance_ipc::...` line to `{BandState, BandType, DaemonState, FxEffectId}`):

```rust
/// The five effects Basic mode exposes, in display order. Loudness and
/// Crossfeed stay Advanced-only (spec §4).
pub(crate) const BASIC_FX: [FxEffectId; 5] = [
    FxEffectId::Bass,
    FxEffectId::Fidelity,
    FxEffectId::Ambience,
    FxEffectId::Surround,
    FxEffectId::DynamicBoost,
];

/// Beginner-friendly display name for a Basic-mode effect slider.
pub(crate) fn basic_fx_label(id: FxEffectId) -> &'static str {
    match id {
        FxEffectId::Bass => "Bass",
        FxEffectId::Fidelity => "Clarity",
        FxEffectId::Surround => "Wide",
        FxEffectId::DynamicBoost => "Boost",
        _ => id.label(),
    }
}

/// Indices of bands that get a Basic-mode gain slider: only types where
/// `gain_db` shapes the response (peaking + shelves). HP/LP/band-pass/notch/
/// all-pass have no gain of their own; they keep running in the DSP but are
/// Advanced-only to edit.
pub(crate) fn gain_slider_bands(bands: &[BandState]) -> Vec<usize> {
    bands
        .iter()
        .enumerate()
        .filter(|(_, b)| {
            matches!(
                b.band_type,
                BandType::Peaking | BandType::LowShelf | BandType::HighShelf
            )
        })
        .map(|(i, _)| i)
        .collect()
}

/// Short frequency label for slider captions: "60", "150", "1k", "2.4k".
pub(crate) fn fmt_freq_short(freq: f64) -> String {
    if freq < 1000.0 {
        format!("{freq:.0}")
    } else {
        let k = freq / 1000.0;
        if (k - k.round()).abs() < 0.05 {
            format!("{:.0}k", k.round())
        } else {
            format!("{k:.1}k")
        }
    }
}

/// Next profile in carousel order. `dir` is +1/-1; wraps at both ends. A
/// `current` that is `None` or missing from the list lands on the first entry
/// going forward and the last going backward.
pub(crate) fn carousel_step(
    profiles: &[String],
    current: Option<&str>,
    dir: i32,
) -> Option<String> {
    if profiles.is_empty() {
        return None;
    }
    let n = profiles.len() as i32;
    let cur = current.and_then(|c| profiles.iter().position(|p| p == c));
    let next = match cur {
        Some(i) => (i as i32 + dir).rem_euclid(n),
        None if dir > 0 => 0,
        None => n - 1,
    };
    profiles.get(next as usize).cloned()
}
```

Note: `Ambience` falls through to `id.label()` (which is already "Ambience"), keeping the match small. If clippy pedantic objects to anything here, fix the code — do not `#[allow]`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p resonance-gui basic`
Expected: 4 tests PASS. (Helpers currently unused outside tests → possible `dead_code` warnings under `-D warnings`. If `make check` trips on them, add a temporary `#[allow(dead_code)]` on each helper with a `// used from task 5+` comment — and REMOVE those allows in Tasks 5-7 when the call sites land. Prefer no allow if the compiler is satisfied by the test usage.)

- [ ] **Step 5: `make check`, commit**

```bash
make check
git add crates/resonance-gui/src/ui/basic.rs
git commit -m "feat(gui): basic mode pure helpers for bands, labels and carousel"
```

---

### Task 4: Kit widgets — vertical slider + tooltip variant of the horizontal slider

**Files:**
- Modify: `crates/resonance-gui/src/ui/kit.rs`

**Interfaces:**
- Consumes: existing `slider_h` (kit.rs:300-368), `tokens(ui)`.
- Produces:
  - `pub(crate) fn slider_v(ui: &mut egui::Ui, size: egui::Vec2, value: &mut f64, range: RangeInclusive<f64>, active: bool, tip: &str) -> bool`
  - `pub(crate) fn slider_h_tip(ui: &mut egui::Ui, width: f32, height: f32, value: &mut f64, range: RangeInclusive<f64>, tip: &str) -> bool`

- [ ] **Step 1: Refactor `slider_h` to expose its response**

In `crates/resonance-gui/src/ui/kit.rs`, rename the existing `slider_h` body into a private `slider_h_inner` that returns `(bool, egui::Response)` (return `(changed, resp)` at the end; the drawing code is otherwise byte-identical), then:

```rust
pub(crate) fn slider_h(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
) -> bool {
    slider_h_inner(ui, width, height, value, range).0
}

/// `slider_h` with a hover tooltip (Basic mode shows values only on hover).
pub(crate) fn slider_h_tip(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    tip: &str,
) -> bool {
    let (changed, resp) = slider_h_inner(ui, width, height, value, range);
    let _ = resp.on_hover_text(tip);
    changed
}
```

- [ ] **Step 2: Implement `slider_v`**

Add to kit.rs (near `slider_h`):

```rust
/// Vertical slider: `slider_h` transposed. Bipolar ranges (lo < 0 < hi) snap
/// to exactly 0 in a small dead zone around the centre, fill from the zero
/// line, and reset to 0 on double-click. `active = false` dims the paint
/// (powered-off chain reads as inert) without locking interaction out,
/// matching how Advanced keeps controls editable while bypassed.
pub(crate) fn slider_v(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    active: bool,
    tip: &str,
) -> bool {
    let t = tokens(ui);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let (lo, hi) = (*range.start(), *range.end());
    let cx = rect.center().x;
    let pad = 8.0;
    let y0 = rect.bottom() - pad; // low end of the range
    let y1 = rect.top() + pad; // high end
    let th = (y0 - y1).max(1.0);
    let bipolar = lo < 0.0 && hi > 0.0;

    let mut changed = false;
    if bipolar && resp.double_clicked() {
        if value.abs() > f64::EPSILON {
            *value = 0.0;
            changed = true;
        }
    } else if resp.dragged() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let f = f64::from(((y0 - p.y) / th).clamp(0.0, 1.0));
            let mut nv = lo + f * (hi - lo);
            if bipolar {
                let zero_y = y0 - ((0.0 - lo) / (hi - lo)) as f32 * th;
                if (p.y - zero_y).abs() <= 3.0 {
                    nv = 0.0;
                }
            }
            if (nv - *value).abs() > f64::EPSILON {
                *value = nv;
                changed = true;
            }
        }
    }

    let frac = ((*value - lo) / (hi - lo)).clamp(0.0, 1.0) as f32;
    let hy = y0 - frac * th;
    let accent = if active { t.accent } else { t.faint };
    let p = ui.painter();
    let track = egui::Rect::from_min_max(egui::pos2(cx - 2.5, y1), egui::pos2(cx + 2.5, y0));
    p.rect_filled(track, 2.5, t.well);
    // Filled portion: from the zero line for bipolar sliders, else from the bottom.
    let zero_y = if bipolar {
        y0 - ((0.0 - lo) / (hi - lo)) as f32 * th
    } else {
        y0
    };
    let (fa, fb) = if hy <= zero_y { (hy, zero_y) } else { (zero_y, hy) };
    p.rect_filled(
        egui::Rect::from_min_max(egui::pos2(cx - 2.5, fa), egui::pos2(cx + 2.5, fb)),
        2.5,
        accent,
    );
    let r = if resp.hovered() || resp.dragged() { 7.0 } else { 6.0 };
    p.circle_filled(egui::pos2(cx, hy), r, accent);
    p.circle_filled(
        egui::pos2(cx, hy),
        r - 3.0,
        if active { Color32::WHITE } else { t.dim },
    );
    let _ = resp.on_hover_text(tip);
    changed
}
```

- [ ] **Step 3: Verify compile (painter widgets have no unit tests — house style)**

Run: `cargo clippy -p resonance-gui --all-targets -- -D warnings`
Expected: clean. (Same dead-code caveat as Task 3 step 4 until Task 5 uses them.)

- [ ] **Step 4: `make check`, commit**

```bash
make check
git add crates/resonance-gui/src/ui/kit.rs
git commit -m "feat(gui): vertical slider and tooltip slider kit widgets"
```

---

### Task 5: Basic EQ gain sliders

**Files:**
- Modify: `crates/resonance-gui/src/ui/basic.rs`

**Interfaces:**
- Consumes: `gain_slider_bands`, `fmt_freq_short` (Task 3); `kit::slider_v` (Task 4); `self.queue_edit(Command::SetBand {...})`; `crate::state::GAIN_LIMIT`.
- Produces: `GuiApp::basic_eq(&mut self, ui, state: &DaemonState)`, called from `basic_screen`.

- [ ] **Step 1: Implement the section**

Add to the `impl GuiApp` block in `crates/resonance-gui/src/ui/basic.rs` (extend imports: `use crate::state::{GAIN_LIMIT, UiMode};` and `use resonance_ipc::{BandState, BandType, Command, DaemonState, FxEffectId};`):

```rust
    /// One vertical gain slider per gain-capable band (spec §3). Other band
    /// types keep running in the DSP but are not editable here.
    fn basic_eq(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        let idxs = gain_slider_bands(&state.bands);
        if idxs.is_empty() {
            let t = kit::tokens(ui);
            ui.colored_label(t.faint, "No adjustable bands — pick a preset");
            return;
        }
        const SLIDER_W: f32 = 44.0;
        const SLIDER_H: f32 = 180.0;
        let on = state.enabled;
        egui::ScrollArea::horizontal()
            .id_salt("basic_eq")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = kit::SP_XS;
                    for &i in &idxs {
                        let Some(b) = state.bands.get(i) else { continue };
                        let mut gain = b.gain_db;
                        ui.vertical(|ui| {
                            ui.set_width(SLIDER_W);
                            let tip = format!("{gain:+.1} dB");
                            if kit::slider_v(
                                ui,
                                egui::vec2(SLIDER_W, SLIDER_H),
                                &mut gain,
                                -GAIN_LIMIT..=GAIN_LIMIT,
                                on,
                                &tip,
                            ) {
                                self.queue_edit(Command::SetBand {
                                    index: i,
                                    freq: b.freq,
                                    gain_db: gain,
                                    q: b.q,
                                });
                            }
                            let t = kit::tokens(ui);
                            ui.vertical_centered(|ui| {
                                ui.colored_label(t.dim, fmt_freq_short(b.freq));
                            });
                        });
                    }
                });
            });
    }
```

Call it from `basic_screen` after the header:

```rust
                    self.basic_header(ui, &state);
                    ui.add_space(kit::SP_L);
                    self.basic_eq(ui, &state);
```

Remove any temporary `#[allow(dead_code)]` from `gain_slider_bands`/`fmt_freq_short` (Task 3 step 4).

- [ ] **Step 2: Run + manual verify**

```bash
cargo run -p resonance-gui
```

In Basic mode with the daemon running: sliders match the current profile's gain bands; dragging a slider audibly changes EQ and the Advanced band table (switch modes) shows the new gain with type/Q/slope/scope untouched; double-click recenters to 0 dB; hover shows "+3.5 dB" tooltip; power off dims sliders but they stay draggable; a >12-band profile scrolls horizontally.

- [ ] **Step 3: `make check`, commit**

```bash
make check
git add crates/resonance-gui/src/ui/basic.rs
git commit -m "feat(gui): basic mode eq gain sliders"
```

---

### Task 6: Preset carousel

**Files:**
- Modify: `crates/resonance-gui/src/ui/basic.rs`

**Interfaces:**
- Consumes: `carousel_step` (Task 3); `self.profiles: Vec<String>`; `state.current_preset`; `self.dirty`; `self.queue(Command::LoadProfile { name })` + `self.dirty = false;` (the exact pattern from devices.rs:406-410); `kit::button_tip`.
- Produces: `GuiApp::basic_carousel(&mut self, ui, state: &DaemonState)`.

- [ ] **Step 1: Implement the carousel**

Add to `impl GuiApp` in basic.rs:

```rust
    /// ◀ name ▶ over the daemon's saved profiles — the same list and load
    /// action as Advanced's Profiles panel (spec §5). A `•` marks unsaved
    /// edits since the profile was applied.
    fn basic_carousel(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        let profiles = self.profiles.clone();
        let current = state.current_preset.clone();
        let have = !profiles.is_empty();
        ui.horizontal(|ui| {
            let t = kit::tokens(ui);
            ui.colored_label(t.dim, "Preset");
            ui.add_space(kit::SP_S);
            let mut load = None;
            if kit::button_tip(ui, "◀", false, have, "Previous preset") {
                load = carousel_step(&profiles, current.as_deref(), -1);
            }
            let label = match (&current, have) {
                (Some(name), _) if self.dirty => format!("{name} •"),
                (Some(name), _) => name.clone(),
                (None, true) => "—".to_string(),
                (None, false) => "No presets yet".to_string(),
            };
            let (r, resp) =
                ui.allocate_exact_size(egui::vec2(180.0, kit::CTRL_H), egui::Sense::hover());
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                &label,
                egui::FontId::proportional(kit::T_BODY),
                if have { t.text } else { t.faint },
            );
            if !have {
                let _ = resp.on_hover_text("Save presets in Advanced mode");
            }
            if kit::button_tip(ui, "▶", false, have, "Next preset") {
                load = carousel_step(&profiles, current.as_deref(), 1);
            }
            if let Some(name) = load {
                self.queue(Command::LoadProfile { name });
                self.dirty = false;
            }
        });
    }
```

Call it from `basic_screen` between header and EQ:

```rust
                    self.basic_header(ui, &state);
                    ui.add_space(kit::SP_L);
                    self.basic_carousel(ui, &state);
                    ui.add_space(kit::SP_L);
                    self.basic_eq(ui, &state);
```

- [ ] **Step 2: Run + manual verify**

`cargo run -p resonance-gui`: ▶/◀ cycle profiles with wrap; name matches Advanced's active profile; editing a slider adds `•`; loading clears it; with zero profiles both arrows disable and the label reads "No presets yet" with the tooltip.

- [ ] **Step 3: `make check`, commit**

```bash
make check
git add crates/resonance-gui/src/ui/basic.rs
git commit -m "feat(gui): basic mode preset carousel"
```

---

### Task 7: Effect sliders row

**Files:**
- Modify: `crates/resonance-gui/src/ui/basic.rs`

**Interfaces:**
- Consumes: `BASIC_FX`, `basic_fx_label` (Task 3); `kit::slider_h_tip` (Task 4); `state.effects.get(id)`; `self.queue_edit(Command::SetEffectIntensity/SetEffectEnabled)` — the exact auto-enable-on-drag pattern from effects.rs:55-66.
- Produces: `GuiApp::basic_fx(&mut self, ui, state: &DaemonState)`.

- [ ] **Step 1: Implement the row**

Add to `impl GuiApp` in basic.rs:

```rust
    /// Five friendly-named effect sliders (spec §4). Dragging auto-enables the
    /// effect, mirroring Advanced's effects rack; Loudness/Crossfeed/preamp
    /// stay Advanced-only.
    fn basic_fx(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        let on = state.enabled;
        let gap = kit::SP_M;
        let n = BASIC_FX.len() as f32;
        let col_w = ((ui.available_width() - gap * (n - 1.0)) / n).max(64.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for id in BASIC_FX {
                let (mut intensity, enabled) = state.effects.get(id);
                ui.vertical(|ui| {
                    ui.set_width(col_w);
                    let t = kit::tokens(ui);
                    ui.colored_label(
                        if on && enabled { t.text } else { t.faint },
                        basic_fx_label(id),
                    );
                    let tip = format!("{:+.0}%", intensity * 100.0);
                    if kit::slider_h_tip(ui, col_w, 12.0, &mut intensity, id.min()..=1.0, &tip) {
                        if !enabled {
                            self.queue_edit(Command::SetEffectEnabled {
                                effect: id,
                                enabled: true,
                            });
                        }
                        self.queue_edit(Command::SetEffectIntensity {
                            effect: id,
                            value: intensity,
                        });
                    }
                });
            }
        });
    }
```

Call it from `basic_screen` after `basic_eq`:

```rust
                    self.basic_eq(ui, &state);
                    ui.add_space(kit::SP_L);
                    self.basic_fx(ui, &state);
```

- [ ] **Step 2: Run + manual verify**

`cargo run -p resonance-gui`: five sliders labelled Bass/Clarity/Ambience/Wide/Boost; Bass and Wide are bipolar (fill from centre, snap to 0); dragging a disabled effect enables it (check the toggle in Advanced); loading a preset moves the sliders; hover shows "+42%".

- [ ] **Step 3: `make check`, commit**

```bash
make check
git add crates/resonance-gui/src/ui/basic.rs
git commit -m "feat(gui): basic mode effect sliders"
```

---

### Task 8: Footer, macOS padding, screenshots, final verification

**Files:**
- Modify: `crates/resonance-gui/src/ui/basic.rs`

**Interfaces:**
- Consumes: everything above; `contrib/dev/uishot.sh` (Xvfb screenshot harness).
- Produces: finished feature.

- [ ] **Step 1: Footer + macOS top padding**

Add to `impl GuiApp` in basic.rs and call as the last item in `basic_screen`:

```rust
    /// Minimal status line (spec §2): connection only — no meters, no "adv:".
    fn basic_footer(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        let t = kit::tokens(ui);
        let msg = if state.enabled {
            "Connected"
        } else {
            "Connected · audio is passing through unprocessed (power off)"
        };
        ui.colored_label(t.faint, msg);
    }
```

In `basic_screen`, directly after `ui.vertical_centered(|ui| {`'s `set_max_width` line, add macOS traffic-light clearance (Basic draws no top toolbar panel):

```rust
                    #[cfg(target_os = "macos")]
                    ui.add_space(28.0);
```

And at the end of the column:

```rust
                    self.basic_fx(ui, &state);
                    ui.add_space(kit::SP_L);
                    self.basic_footer(ui, &state);
                    ui.add_space(kit::SP_L);
```

- [ ] **Step 2: Keyboard sanity**

`handle_keyboard` runs before `render_panels` in `ui()` (app.rs:1361-1379), so Ctrl+Z / Ctrl+Shift+Z already work in Basic. Verify: drag a slider, Ctrl+Z → gain reverts (spec §3).

- [ ] **Step 3: Screenshot pass**

Read the header of `contrib/dev/uishot.sh` for its exact flags, then use it to capture and view: (1) Basic screen with a loaded preset, (2) first-run chooser (clear the eframe storage `.ron` first), (3) Advanced mode — confirm it is visually unchanged, (4) Basic in a couple of themes (switch theme in Advanced settings, return to Basic). Fix any layout breakage found.

- [ ] **Step 4: Full check + final commit**

```bash
make check
git add -A
git commit -m "feat(gui): basic mode footer and platform polish"
```

Expected: fmt clean, clippy clean, all tests pass (existing 68 + the new gui tests).

---

## Verification checklist (maps to spec §7)

- [ ] `cargo test -p resonance-gui` — mode round-trip, band filter, freq labels, carousel, fx labels all pass.
- [ ] First-run: no `ui_mode` stored → chooser appears once; choice persists.
- [ ] Mode switch both directions via buttons; persists across restart.
- [ ] Basic: sliders track profile; edits reach the daemon (gain-only, type/Q/slope/scope/dynamics preserved — verify in Advanced table); double-click → 0 dB; Ctrl+Z undo.
- [ ] Carousel = Advanced profile load semantics; dirty `•`; empty-state.
- [ ] Fx sliders: 5 friendly names, bipolar Bass/Wide, auto-enable on drag.
- [ ] Power off dims Basic controls but leaves them editable.
- [ ] Advanced mode pixel-identical (screenshot diff vs pre-branch capture if in doubt).
- [ ] No IPC/daemon/DSP diffs: `git diff master -- crates/resonance-daemon crates/resonance-ipc crates/resonance-dsp` is empty.
