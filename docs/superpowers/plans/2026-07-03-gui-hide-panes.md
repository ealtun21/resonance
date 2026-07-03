# GUI Hide Panes + Settings Reorder Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user hide individual GUI panels (5 control cards + EQ bands + reference bar) from Settings so the FR graph can become the full UI, and reorder the Settings dialog to Theme → Panes → Channels → Advanced features → EQ phase.

**Architecture:** A new pure-logic `panes.rs` module defines a `PaneId` enum and a persisted `HashSet<PaneId>` of *hidden* panes (empty = all visible) lives on `GuiApp`. The Settings dialog gains a "Panes" section of visibility checkboxes; the layout render code guards each pane on its visibility and collapses empty regions so a fully-hidden lower area leaves the graph filling the window.

**Tech Stack:** Rust, egui/eframe, serde/serde_json (already dependencies of `resonance-gui`).

## Global Constraints

- Conventional Commits, all lowercase.
- **No AI-related content anywhere** — no `Co-Authored-By` / AI-attribution trailer on commits (repo convention overrides the harness default).
- `make check` (= `cargo fmt --all -- --check` + `cargo clippy --all-targets --all-features -- -D warnings` + `cargo test --all`) must pass before every commit. Clippy pedantic is enforced workspace-wide via a `[lints]` table.
- Functional style preferred (iterators, closures); match surrounding code's comment density and idiom.
- No DSP / IPC / daemon / audio surface is touched, so no `resonance verify` run is required.
- Work happens on branch `feat/gui-hide-panes` (already created; the design doc is committed there).

---

### Task 1: `panes.rs` pure-logic module

**Files:**
- Create: `crates/resonance-gui/src/panes.rs`
- Modify: `crates/resonance-gui/src/main.rs:9` (add `mod panes;`)

**Interfaces:**
- Consumes: `crate::card_layout::CardId` (variants `Effects`, `Applications`, `Outputs`, `DeviceMap`, `Profiles`).
- Produces:
  - `enum PaneId { Effects, Applications, Outputs, DeviceMap, Profiles, Bands, ReferenceBar }` — `Debug + Clone + Copy + PartialEq + Eq + Hash + Serialize + Deserialize`.
  - `PaneId::ALL: [PaneId; 7]` — in Settings display order.
  - `PaneId::title(self) -> &'static str`.
  - `PaneId::card(self) -> Option<CardId>` — `Some` for the five card panes, `None` for `Bands`/`ReferenceBar`.
  - `PaneId::from_card(card: CardId) -> PaneId`.
  - `pub(crate) fn hidden_from_json_or_default(s: &str) -> std::collections::HashSet<PaneId>`.

- [ ] **Step 1: Write `panes.rs` with the type, helpers, and failing tests**

Create `crates/resonance-gui/src/panes.rs`:

```rust
//! Which GUI panels the user has chosen to hide (Settings → Panes). Pure logic
//! (no egui) so the enum mapping and persistence parsing are unit-tested. The
//! FR graph itself is never hideable — it fills the window when everything else
//! is hidden.

use crate::card_layout::CardId;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// A hideable GUI panel. The five card panes map 1:1 to a `CardId`; `Bands` and
/// `ReferenceBar` are the two fixed anchors that are not movable cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum PaneId {
    Effects,
    Applications,
    Outputs,
    DeviceMap,
    Profiles,
    Bands,
    ReferenceBar,
}

impl PaneId {
    /// Every hideable pane, in the order the Settings checkboxes list them.
    pub(crate) const ALL: [PaneId; 7] = [
        PaneId::Effects,
        PaneId::Applications,
        PaneId::Outputs,
        PaneId::DeviceMap,
        PaneId::Profiles,
        PaneId::Bands,
        PaneId::ReferenceBar,
    ];

    /// Label for the Settings checkbox.
    pub(crate) fn title(self) -> &'static str {
        match self {
            PaneId::Effects => "Effects",
            PaneId::Applications => "Applications",
            PaneId::Outputs => "Outputs",
            PaneId::DeviceMap => "Device → Profile",
            PaneId::Profiles => "Profiles",
            PaneId::Bands => "EQ bands",
            PaneId::ReferenceBar => "Reference bar",
        }
    }

    /// The movable card this pane corresponds to, or `None` for the two fixed
    /// anchors (`Bands`, `ReferenceBar`).
    pub(crate) fn card(self) -> Option<CardId> {
        Some(match self {
            PaneId::Effects => CardId::Effects,
            PaneId::Applications => CardId::Applications,
            PaneId::Outputs => CardId::Outputs,
            PaneId::DeviceMap => CardId::DeviceMap,
            PaneId::Profiles => CardId::Profiles,
            PaneId::Bands | PaneId::ReferenceBar => return None,
        })
    }

    /// The pane for a movable card (the inverse of the card-carrying arm of
    /// [`card`](Self::card)).
    pub(crate) fn from_card(card: CardId) -> PaneId {
        match card {
            CardId::Effects => PaneId::Effects,
            CardId::Applications => PaneId::Applications,
            CardId::Outputs => PaneId::Outputs,
            CardId::DeviceMap => PaneId::DeviceMap,
            CardId::Profiles => PaneId::Profiles,
        }
    }
}

/// Parse the persisted hidden-panes set (a JSON array of `PaneId`). Any parse
/// error or unknown variant yields an empty set (all panes visible), so corrupt
/// or version-skewed storage never hides content unexpectedly.
pub(crate) fn hidden_from_json_or_default(s: &str) -> HashSet<PaneId> {
    serde_json::from_str::<HashSet<PaneId>>(s).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_has_seven_distinct_panes() {
        assert_eq!(PaneId::ALL.len(), 7);
        let set: HashSet<PaneId> = PaneId::ALL.iter().copied().collect();
        assert_eq!(set.len(), 7);
    }

    #[test]
    fn card_mapping_round_trips_and_anchors_have_none() {
        for card in CardId::ALL {
            assert_eq!(PaneId::from_card(card).card(), Some(card));
        }
        assert_eq!(PaneId::Bands.card(), None);
        assert_eq!(PaneId::ReferenceBar.card(), None);
    }

    #[test]
    fn hidden_set_json_round_trips() {
        let hidden: HashSet<PaneId> =
            [PaneId::Bands, PaneId::Outputs].into_iter().collect();
        let json = serde_json::to_string(&hidden).unwrap();
        assert_eq!(hidden_from_json_or_default(&json), hidden);
    }

    #[test]
    fn invalid_or_unknown_json_falls_back_to_all_visible() {
        assert!(hidden_from_json_or_default("garbage").is_empty());
        // One unknown variant fails the whole parse → empty (all visible).
        assert!(hidden_from_json_or_default(r#"["Effects","Nope"]"#).is_empty());
    }

    #[test]
    fn titles_are_nonempty() {
        assert!(PaneId::ALL.iter().all(|p| !p.title().is_empty()));
    }
}
```

Add the module declaration in `crates/resonance-gui/src/main.rs` after line 9 (`mod card_layout;`):

```rust
mod card_layout;
mod curve;
```

becomes:

```rust
mod card_layout;
mod curve;
mod panes;
```

(Insert `mod panes;` in alphabetical position — after `mod ipc;` / before `mod state;` also works; the exact slot only needs to keep the list sorted. Place it after `mod ipc;`.)

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo test -p resonance-gui panes::`
Expected: PASS — `test result: ok. 5 passed`.

(If `cargo test` reports `dead_code` for any helper, that's expected only under a plain `cargo build`; `--all-targets` clippy in Step 3 sees the test usage.)

- [ ] **Step 3: Run the full gate**

Run: `make check`
Expected: fmt clean, clippy clean (`-D warnings`), all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/resonance-gui/src/panes.rs crates/resonance-gui/src/main.rs
git commit -m "feat(gui): pane-visibility model (paneid enum + persistence parse)"
```

---

### Task 2: App state, persistence, Settings reorder + Panes section

**Files:**
- Modify: `crates/resonance-gui/src/app.rs` (field near `:349`, init near `:716`, save near `:1256`, `reset_layout` at `:1472`, add `pane_visible` helper)
- Modify: `crates/resonance-gui/src/ui/dialogs.rs:129-226` (Settings body: reorder + Panes section)

**Interfaces:**
- Consumes: `crate::panes::{PaneId, hidden_from_json_or_default}` (Task 1).
- Produces:
  - `GuiApp.hidden_panes: std::collections::HashSet<PaneId>`.
  - `GuiApp::pane_visible(&self, pane: PaneId) -> bool` — `!self.hidden_panes.contains(&pane)`.
  - `reset_layout` additionally clears `hidden_panes`.

- [ ] **Step 1: Add the `use` and the struct field**

In `crates/resonance-gui/src/app.rs`, add to the imports near the top (next to `use crate::card_layout::{CardCol, CardId, CardLayout};` at line 8):

```rust
use crate::card_layout::{CardCol, CardId, CardLayout};
use crate::panes::{hidden_from_json_or_default, PaneId};
```

Add the field to the `GuiApp` struct, immediately after the `layout_edit` field (line 352):

```rust
    /// Session-only "arrange the layout" mode: shows draggable card tiles + drop
    /// zones instead of the live cards. Never persisted.
    pub(crate) layout_edit: bool,
    /// Panes the user has hidden via Settings → Panes (persisted). Empty ⇒ every
    /// pane is shown. The FR graph itself is never in this set.
    pub(crate) hidden_panes: std::collections::HashSet<PaneId>,
```

- [ ] **Step 2: Initialise the field in `new()`**

In the struct literal in `GuiApp::new()`, immediately after the `layout_edit:` line (line 717), add:

```rust
            layout_edit: std::env::var("RESONANCE_EDIT_LAYOUT").is_ok(),
            hidden_panes: cc
                .storage
                .and_then(|s| s.get_string("hidden_panes"))
                .map(|s| hidden_from_json_or_default(&s))
                .unwrap_or_default(),
```

- [ ] **Step 3: Persist the field in `save()`**

In `GuiApp::save()`, after the `card_layout` block (line 1257-1259), add:

```rust
        if let Ok(j) = serde_json::to_string(&self.layout) {
            storage.set_string("card_layout", j);
        }
        if let Ok(j) = serde_json::to_string(&self.hidden_panes) {
            storage.set_string("hidden_panes", j);
        }
```

- [ ] **Step 4: Add `pane_visible` and clear-on-reset**

Add the helper method inside the `impl GuiApp` block that contains `reset_layout` (just above `reset_layout`, near line 1470):

```rust
    /// Whether a pane is currently shown (not in the hidden set).
    pub(crate) fn pane_visible(&self, pane: PaneId) -> bool {
        !self.hidden_panes.contains(&pane)
    }
```

In `reset_layout` (line 1481), add the clear right after resetting the card layout:

```rust
        self.layout = CardLayout::default();
        self.hidden_panes.clear();
        self.set_status("layout reset");
```

- [ ] **Step 5: Reorder the Settings body and add the Panes section**

In `crates/resonance-gui/src/ui/dialogs.rs`, replace the entire `egui::ScrollArea::vertical().show(ui, |ui| { … })` body of `settings_dialog` (the block spanning lines 129-226) with this reordered version. Theme first, then Panes, then Channels, then Advanced features, then EQ phase:

```rust
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // ── Theme ─────────────────────────────────────────────────
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Theme").strong());
                    let cctx = ui.ctx().clone();
                    for t in Theme::ALL {
                        if ui.selectable_label(self.theme == t, t.label()).clicked() {
                            self.set_theme(&cctx, t);
                        }
                    }

                    // ── Panes ─────────────────────────────────────────────────
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Panes").strong());
                    ui.weak(
                        "Show or hide panels to declutter the view. Hidden panels \
                         stop rendering entirely; the graph fills whatever's left.",
                    );
                    ui.add_space(4.0);
                    for pane in crate::panes::PaneId::ALL {
                        let mut visible = self.pane_visible(pane);
                        if ui.checkbox(&mut visible, pane.title()).changed() {
                            if visible {
                                self.hidden_panes.remove(&pane);
                            } else {
                                self.hidden_panes.insert(pane);
                            }
                        }
                    }
                    ui.add_space(4.0);
                    if ui
                        .add_enabled(
                            !self.hidden_panes.is_empty(),
                            egui::Button::new("Show all panes"),
                        )
                        .clicked()
                    {
                        self.hidden_panes.clear();
                    }

                    // ── Channels ──────────────────────────────────────────────
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Channels").strong());
                    if let Some(s) = &state {
                        if s.channels >= 2 {
                            self.channels_section(ui, s);
                        } else {
                            ui.weak("Stereo or multichannel output required.");
                        }
                    } else {
                        ui.weak("Connect the daemon to configure channels.");
                    }

                    // ── Advanced features ─────────────────────────────────────
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Advanced features").strong());
                    ui.weak("Hidden by default to keep the main view clean.");
                    ui.add_space(4.0);
                    ui.checkbox(
                        &mut self.show_slope,
                        "Filter slope column (12/24/48 dB/oct)",
                    );
                    ui.checkbox(&mut self.show_scope, "Stereo scope column (Mid/Side)");
                    ui.checkbox(
                        &mut self.show_dynamics,
                        "Dynamic EQ column (level-driven bands)",
                    );
                    ui.checkbox(&mut self.show_dither, "Output dither section");
                    ui.checkbox(
                        &mut self.show_ir,
                        "Convolution section (WAV impulse response)",
                    );

                    // ── EQ phase ──────────────────────────────────────────────
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("EQ phase").strong());
                    if let Some(s) = &state {
                        let rate = s.sample_rate.max(1.0);
                        let frames = if s.phase_mode_linear && s.eq_fir_latency_frames > 0 {
                            s.eq_fir_latency_frames
                        } else {
                            resonance_dsp::convolution::BLOCK
                                + resonance_dsp::linphase::grid_len(rate) / 2
                        };
                        let ms = frames as f64 / rate * 1000.0;
                        ui.horizontal(|ui| {
                            let mut linear = s.phase_mode_linear;
                            if ui
                                .checkbox(&mut linear, "Linear phase")
                                .on_hover_text(format!(
                                    "Off — minimum phase: reacts like a real \
                                     headphone would, with no delay. \
                                     Recommended.\n\nOn — linear phase: exactly \
                                     the same tone, but attacks soften slightly \
                                     (filters ring before each hit, which no \
                                     physical speaker does) and playback lags by \
                                     {ms:.0} ms. Mid/Side and dynamic bands stay \
                                     minimum-phase either way.",
                                ))
                                .changed()
                            {
                                self.queue(Command::SetPhaseMode { linear });
                            }
                            if s.phase_mode_linear {
                                ui.weak(format!("(+{ms:.1} ms)"));
                            }
                        });
                        ui.weak(format!(
                            "Both modes produce the same tone. Off is truest to a \
                             physically tuned headphone; On trades {ms:.0} ms of \
                             delay and softer attacks for zero phase rotation.",
                        ));
                    } else {
                        ui.weak("Connect the daemon to change the EQ phase mode.");
                    }
                });
```

(The EQ-phase and Channels bodies are copied verbatim from the current file — only their order relative to Theme/Panes/Advanced changed. `state` is the `self.state.clone()` bound just above the `dialog_window` call; it stays.)

- [ ] **Step 6: Build and run the gate**

Run: `cargo build -p resonance-gui`
Expected: builds clean (no `dead_code` — `pane_visible` is used by the Panes checkboxes, `hidden_panes` is read/written by save/load/Settings/reset).

Run: `make check`
Expected: fmt clean, clippy clean, tests pass.

- [ ] **Step 7: Visual check (Settings order + persistence)**

Render the Settings dialog without a daemon via the demo hook and the Xvfb screenshot harness:

Run: `RESONANCE_DEMO=1 contrib/dev/uishot.sh` (see `contrib/dev/uishot.sh` for the exact flags; it launches the GUI under Xvfb and captures a PNG). Open the Settings dialog (gear icon) in the captured run.
Expected: sections read top-to-bottom **Theme → Panes → Channels → Advanced features → EQ phase**; the Panes section lists 7 "visible" checkboxes (all checked) + a "Show all panes" button (disabled while none hidden).

If the harness cannot open the dialog non-interactively, instead launch `RESONANCE_DEMO=1 cargo run -p resonance-gui`, open Settings, toggle a couple of Panes checkboxes off, close and relaunch, and confirm the toggles persisted (checkboxes still off). Nothing visually hides yet — that is Task 3.

- [ ] **Step 8: Commit**

```bash
git add crates/resonance-gui/src/app.rs crates/resonance-gui/src/ui/dialogs.rs
git commit -m "feat(gui): settings panes section + reorder (theme first) + persistence"
```

---

### Task 3: Render guards — actually hide the panes

**Files:**
- Modify: `crates/resonance-gui/src/ui/layout.rs` (`shell` `:110-182`, `render_card` `:187`, `lower_columns` `:344-402`, `accordion_stack` `:408-430`, `layout_edit_banner` `:329-336`)
- Modify: `crates/resonance-gui/src/ui/curve_view.rs` (`hero` `:93-118`)

**Interfaces:**
- Consumes: `GuiApp::pane_visible(PaneId)` and `hidden_panes` (Task 2); `crate::panes::PaneId`.
- Produces: no new public interface — this task wires visibility into rendering.

- [ ] **Step 1: Guard `render_card` on card-pane visibility**

In `crates/resonance-gui/src/ui/layout.rs`, add a `use` for `PaneId` at the top (next to the existing `use crate::card_layout::{CardCol, CardId, CardLayout};`):

```rust
use crate::card_layout::{CardCol, CardId, CardLayout};
use crate::panes::PaneId;
```

At the very top of `render_card` (line 187, before the `match id`), return early when the card's pane is hidden:

```rust
    fn render_card(&mut self, ui: &mut egui::Ui, s: &DaemonState, id: CardId) -> bool {
        if !self.pane_visible(PaneId::from_card(id)) {
            return false;
        }
        match id {
```

Returning `false` already makes both the wide columns and the narrow accordion skip the card's inter-card spacing (the same path empty Applications/Outputs use). This alone makes hidden cards vanish in **edit-off** wide columns and in the narrow accordion.

- [ ] **Step 2: Add visibility helpers used by the wide layout**

Add these small private helpers to the `impl GuiApp` block in `layout.rs` (place just above `lower_columns`, near line 340). They centralise the "does this column / lower area have any visible content" checks:

```rust
    /// Visible cards in a column, honouring the hidden-panes set. Applications /
    /// Outputs still also require live rows, but that emptiness is handled where
    /// the card renders; here we only apply the user's hide choices so the column
    /// panel can be dropped when the user has hidden everything in it.
    fn visible_cards(&self, col: CardCol) -> Vec<CardId> {
        self.layout
            .column(col)
            .iter()
            .copied()
            .filter(|&id| self.pane_visible(PaneId::from_card(id)))
            .collect()
    }

    /// True when at least one lower pane (any control card or the EQ bands) is
    /// visible. When false the wide layout drops the whole controls strip and the
    /// narrow layout lets the graph fill, so the FR graph is the entire UI.
    fn lower_has_content(&self) -> bool {
        self.pane_visible(PaneId::Bands)
            || CardId::ALL
                .iter()
                .any(|&id| self.pane_visible(PaneId::from_card(id)))
    }
```

- [ ] **Step 3: Wide `shell` — drop the controls strip when the lower area is empty**

In `shell`, `LayoutMode::Wide` branch (lines 120-154), wrap the `Panel::bottom("controls_panel")` block so it is only shown when there is lower content. Replace:

```rust
                let controls_h = (ui.available_height() * 0.4).max(150.0);
                egui::Panel::bottom("controls_panel")
                    .resizable(true)
                    .default_size(controls_h)
                    .min_size(80.0)
                    .show_separator_line(false)
                    .show_inside(ui, |ui| self.lower_columns(ui, state.as_ref()));
```

with:

```rust
                // Drop the whole controls strip when every lower pane is hidden
                // (and we're not arranging), so the hero graph fills the window.
                if self.layout_edit || self.lower_has_content() {
                    let controls_h = (ui.available_height() * 0.4).max(150.0);
                    egui::Panel::bottom("controls_panel")
                        .resizable(true)
                        .default_size(controls_h)
                        .min_size(80.0)
                        .show_separator_line(false)
                        .show_inside(ui, |ui| self.lower_columns(ui, state.as_ref()));
                }
```

(The hero `CentralPanel` block that follows is unchanged; as the last panel it now fills the whole area when the bottom strip is absent.)

- [ ] **Step 4: `lower_columns` — conditional side panels + stacked fallback when bands hidden**

Replace the body of `lower_columns` (lines 344-402) with the version below. Edit mode is byte-for-byte the old behaviour; only the non-edit path changes.

```rust
    fn lower_columns(&mut self, ui: &mut egui::Ui, state: Option<&DaemonState>) {
        // Edit-mode banner spans the top of the controls strip.
        if self.layout_edit {
            egui::Panel::top("layout_edit_banner")
                .frame(egui::Frame::NONE)
                .show_separator_line(false)
                .show_inside(ui, |ui| self.layout_edit_banner(ui));
        }

        // Arrange mode always shows the full 3-column arranger (both side
        // columns + the live bands center), ignoring the hidden-panes set —
        // arranging is about placement, not visibility.
        let bands_visible = self.layout_edit || self.pane_visible(PaneId::Bands);
        let left_cards = self.visible_cards(CardCol::Left);
        let right_cards = self.visible_cards(CardCol::Right);

        // Left side panel: shown in edit mode, or when it has a visible card.
        if self.layout_edit || !left_cards.is_empty() {
            egui::Panel::left("effects_col")
                .resizable(false)
                .exact_size(EFFECTS_W)
                .frame(egui::Frame::NONE)
                .show_separator_line(false)
                .show_inside(ui, |ui| {
                    if let Some(s) = state {
                        padded_scroll(ui, "effects_scroll", |ui| {
                            self.render_lower_column(ui, s, CardCol::Left);
                        });
                    }
                });
        }
        // Right side panel: same rule.
        if self.layout_edit || !right_cards.is_empty() {
            egui::Panel::right("devices_col")
                .resizable(false)
                .exact_size(DEVICES_W)
                .frame(egui::Frame::NONE)
                .show_separator_line(false)
                .show_inside(ui, |ui| {
                    if let Some(s) = state {
                        padded_scroll(ui, "side", |ui| {
                            self.render_lower_column(ui, s, CardCol::Right);
                        });
                    }
                });
        }

        if bands_visible {
            // Centre column IS the bands card: a card-styled CentralPanel that
            // fills whatever the side panels leave (full width if both are gone).
            let t = kit::tokens(ui);
            let card_frame = egui::Frame::default()
                .fill(ui.visuals().faint_bg_color)
                .stroke(egui::Stroke::new(1.0, t.line))
                .corner_radius(egui::CornerRadius::same(kit::R_CARD as u8))
                .outer_margin(egui::Margin::symmetric(8, 10));
            egui::CentralPanel::default()
                .frame(card_frame)
                .show_inside(ui, |ui| {
                    if let Some(s) = state {
                        self.bands_card(ui, s);
                    }
                });
        } else {
            // Bands hidden: the remaining visible cards stack full-width in a
            // scroll area (the same card widget the narrow accordion uses).
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show_inside(ui, |ui| {
                    if let Some(s) = state {
                        padded_scroll(ui, "stacked_cards", |ui| {
                            for id in left_cards.iter().chain(&right_cards) {
                                if self.render_card(ui, s, *id) {
                                    ui.add_space(12.0);
                                }
                            }
                        });
                    }
                });
        }

        // Apply a card move requested by a drop this frame, after both columns
        // finished rendering (never mutate the lists mid-iteration).
        if let Some((id, col, idx)) = self.pending_card_move.take() {
            self.layout.move_card(id, col, idx);
        }
    }
```

Note: `render_card` re-checks `pane_visible`, but `left_cards`/`right_cards` are already filtered, so the stacked loop only iterates visible cards — the re-check is a cheap no-op that keeps `render_card` self-guarding.

- [ ] **Step 5: `accordion_stack` (narrow) — skip hidden bands section**

In `accordion_stack` (lines 408-430), guard the fixed EQ-bands section on visibility. Replace:

```rust
                    section(ui, "EQ bands", |ui| self.bands_section(ui, s));
                    ui.add_space(GAP);
```

with:

```rust
                    if self.pane_visible(PaneId::Bands) {
                        section(ui, "EQ bands", |ui| self.bands_section(ui, s));
                        ui.add_space(GAP);
                    }
```

(Hidden cards in the left/right loops are already skipped by `render_card`.)

- [ ] **Step 6: Narrow `shell` — reference-bar guard + full-graph fallback**

In `shell`, `LayoutMode::Narrow` branch (lines 158-180), replace the whole branch body with:

```rust
            LayoutMode::Narrow => {
                let ref_visible = self.pane_visible(PaneId::ReferenceBar);
                if self.lower_has_content() {
                    let gh = (ui.available_height() * 0.5).max(180.0);
                    egui::Panel::top("graph_narrow")
                        .resizable(true)
                        .default_size(gh)
                        .min_size(150.0)
                        .show_separator_line(false)
                        .show_inside(ui, |ui| {
                            if let Some(s) = &state {
                                self.eq_curve(ui, s);
                            }
                        });
                    if ref_visible {
                        egui::Panel::top("reference_bar_narrow")
                            .resizable(false)
                            .show_separator_line(false)
                            .show_inside(ui, |ui| self.reference_bar(ui));
                    }
                    egui::CentralPanel::default().show_inside(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("controls_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.accordion_stack(ui, state.as_ref()));
                    });
                } else {
                    // Nothing below the graph: let it fill, with the reference bar
                    // (if shown) pinned under it.
                    if ref_visible {
                        egui::Panel::bottom("reference_bar_narrow")
                            .resizable(false)
                            .show_separator_line(false)
                            .show_inside(ui, |ui| self.reference_bar(ui));
                    }
                    egui::CentralPanel::default().show_inside(ui, |ui| {
                        if let Some(s) = &state {
                            self.eq_curve(ui, s);
                        }
                    });
                }
            }
```

- [ ] **Step 7: `hero` (wide) — guard the reference bar**

In `crates/resonance-gui/src/ui/curve_view.rs`, add the `use` for `PaneId` at the top of the file (next to the other `use crate::...` lines):

```rust
use crate::panes::PaneId;
```

Wrap the reference-bar bottom panel in `hero` (lines 93-118) in a visibility check:

```rust
        // Reference bar pinned to the very bottom (its own top rule) — unless the
        // user has hidden it in Settings → Panes.
        if self.pane_visible(PaneId::ReferenceBar) {
            egui::Panel::bottom("hero_refbar")
                .frame(egui::Frame::NONE)
                .show_separator_line(false)
                .show_inside(ui, |ui| {
                    let line = kit::tokens(ui).line;
                    let (lr, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 1.0),
                        egui::Sense::hover(),
                    );
                    ui.painter()
                        .hline(lr.x_range(), lr.center().y, egui::Stroke::new(1.0, line));
                    egui::Frame::default()
                        .inner_margin(egui::Margin {
                            left: kit::CARD_PAD_X as i8,
                            right: kit::CARD_PAD_X as i8,
                            top: 0,
                            bottom: kit::SP_XS as i8,
                        })
                        .show(ui, |ui| self.reference_bar(ui));
                });
        }
```

(Preserve the existing explanatory comment about centring the hline; it is elided above for brevity — keep those lines from the original when editing.)

- [ ] **Step 8: `layout_edit_banner` Reset — also unhide panes**

In `layout_edit_banner` (lines 329-335), extend the "Reset" click to clear the hidden set too:

```rust
                        if ui.button("Reset").clicked() {
                            self.layout = CardLayout::default();
                            self.hidden_panes.clear();
                        }
```

- [ ] **Step 9: Build and run the gate**

Run: `make check`
Expected: fmt clean, clippy clean (`-D warnings`), tests pass.

- [ ] **Step 10: Visual verification of hiding + fallbacks**

Launch the demo GUI (renders the full UI without a daemon): `RESONANCE_DEMO=1 cargo run -p resonance-gui`. Verify each case:

- Uncheck **Effects** in Settings → Panes: the Effects card disappears from the left column; the layout reflows.
- Uncheck **Reference bar**: the target/measurement strip under the graph disappears (wide) / the strip under the graph disappears (narrow).
- Uncheck **EQ bands** (keep some cards checked): in a wide window the center bands table is gone and the remaining cards stack full-width; in a narrow window the "EQ bands" accordion section is gone.
- Uncheck **every** pane (all 5 cards + EQ bands + reference bar): the FR graph + spectrum fills the whole window with no empty strips, in both a wide and a narrow window.
- Overflow menu (☰) → **Reset layout**: every pane returns; status toast reads "layout reset".
- Settings → **Show all panes**: every pane returns and the button greys out.

Optionally capture a before/after with `contrib/dev/uishot.sh` for the PR description.

- [ ] **Step 11: Commit**

```bash
git add crates/resonance-gui/src/ui/layout.rs crates/resonance-gui/src/ui/curve_view.rs
git commit -m "feat(gui): hide panes from settings; graph fills when all hidden"
```

---

## Self-Review

**Spec coverage:**
- Data model & persistence → Task 1 (`PaneId`, parse helper) + Task 2 (field, load, save). ✅
- `pane_visible` helper → Task 2 Step 4. ✅
- Settings reorder (Theme → Panes → Channels → Advanced → EQ phase) → Task 2 Step 5. ✅
- Panes section with per-pane "visible" checkboxes + "Show all panes" → Task 2 Step 5. ✅
- `render_card` hidden-guard → Task 3 Step 1. ✅
- Wide full-graph fallback (no controls strip when lower empty) → Task 3 Step 3. ✅
- Wide conditional side panels + stacked fallback when bands hidden → Task 3 Step 4. ✅
- Narrow reference-bar skip, bands-section skip, full-graph fallback → Task 3 Steps 5-6. ✅
- `hero()` reference-bar guard → Task 3 Step 7. ✅
- Edit mode unchanged → Task 3 Steps 3-4 keep the `layout_edit` path as the old behaviour. ✅
- Reset paths clear hidden set (reset_layout, edit banner, Show all) → Task 2 Step 4 + Task 2 Step 5 (Show all) + Task 3 Step 8. ✅
- Not surfaced in the "adv:" hint → no task touches `advanced_active_hint`; nothing to add. ✅
- Testing (panes.rs unit tests, make check) → Task 1 Steps 1-3. ✅

**Placeholder scan:** No TBD/TODO/"handle edge cases". Every code step shows full code. The one elision (Task 3 Step 7's hline comment) is explicitly flagged as "keep the original lines". ✅

**Type consistency:** `PaneId`, `PaneId::ALL`, `PaneId::title`, `PaneId::card`, `PaneId::from_card`, `hidden_from_json_or_default`, `GuiApp::pane_visible`, `GuiApp::hidden_panes`, `visible_cards`, `lower_has_content` — names used identically across Tasks 1-3. `CardId::ALL` exists (`card_layout.rs:19`). ✅
