# Reorderable Control Cards Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user rearrange the GUI's control cards (Effects, Applications, Outputs, Device→Profile, Profiles) between the left/right columns via an "Edit layout" mode, persisted across restarts. The graph and EQ Bands stay fixed anchors.

**Architecture:** A persisted `CardLayout { left, right }` of `CardId`s drives the wide-layout columns and the narrow accordion. An "Edit layout" mode (session-only) swaps the live cards for compact draggable tiles and shows drop zones between them; drops call a pure, unit-tested `move_card`. Drag-and-drop uses egui 0.34's built-in `dnd_drag_source` / `dnd_drop_zone` payload API — no new dependency.

**Tech Stack:** Rust, egui/eframe 0.34.3, serde/serde_json (persistence), the project's bespoke `kit` UI helpers.

**Spec:** `docs/specs/2026-07-01-reorderable-cards-design.md`

## Implementation refinement vs the spec

The spec described edit mode as "live cards with a grip handle". This plan instead renders **compact draggable tiles** (grip icon + card name) in edit mode, and live cards in normal mode. Rationale: it avoids drag-vs-widget-interaction conflicts, keeps drop-target indices simple, and lets absent cards (Applications/Outputs when the daemon reports no streams/sinks) still be positioned. The user-facing behaviour (an explicit edit mode with a grab handle, tiling not floating, persisted arrangement) is unchanged.

## Global Constraints

- Conventional Commits, all lowercase (e.g. `feat(gui): ...`).
- **No AI-related content anywhere** (code, comments, commit messages, docs). No `Co-Authored-By`/AI-attribution trailers.
- `make check` (fmt --check + clippy `-D warnings` + `test --all`) MUST pass before every commit. Clippy pedantic is enforced workspace-wide; if a new binding pair trips `clippy::similar_names`, add a scoped `#[allow(clippy::similar_names)]` with a one-line reason (matches existing code).
- Functional style preferred; match surrounding comment density and idiom.
- The graph (hero) and EQ Bands (elastic center) are fixed anchors and never move.
- Movable set = the 5 cards: Effects, Applications, Outputs, Device→Profile, Profiles. The Output (dither) section stays part of the Effects card (rendered beneath it when `show_dither`).
- Default arrangement = today's layout: `left: [Effects]`, `right: [Applications, Outputs, DeviceMap, Profiles]`.
- Edit-mode flag is session-only (never persisted); the `CardLayout` is persisted via egui storage.
- Drag-reordering is wide-only; the narrow accordion renders `left → EQ Bands → right` but has no drag.

---

### Task 1: `CardLayout` model (pure, tested)

**Files:**
- Create: `crates/resonance-gui/src/card_layout.rs`
- Modify: `crates/resonance-gui/src/main.rs` (add `mod card_layout;`)

**Interfaces:**
- Produces:
  - `pub(crate) enum CardId { Effects, Applications, Outputs, DeviceMap, Profiles }` — `Copy + Eq + Hash + Serialize + Deserialize`. `CardId::ALL: [CardId; 5]`, `CardId::title(self) -> &'static str`.
  - `pub(crate) enum CardCol { Left, Right }` — `Copy + Eq`.
  - `pub(crate) struct CardLayout { pub(crate) left: Vec<CardId>, pub(crate) right: Vec<CardId> }` with `Default`, `column(&self, CardCol) -> &[CardId]`, `move_card(&mut self, CardId, CardCol, usize)`, `from_json_or_default(&str) -> CardLayout`.

- [ ] **Step 1: Create the module with types + logic**

Create `crates/resonance-gui/src/card_layout.rs`:

```rust
//! Persisted arrangement of the GUI's movable control cards across the two side
//! columns. Pure logic (no egui) so the reorder maths is unit-tested.

use serde::{Deserialize, Serialize};

/// The movable control cards. The graph and EQ Bands are fixed anchors and are
/// deliberately absent here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum CardId {
    Effects,
    Applications,
    Outputs,
    DeviceMap,
    Profiles,
}

impl CardId {
    /// Every card, in canonical order. Also used to validate a loaded layout.
    pub(crate) const ALL: [CardId; 5] = [
        CardId::Effects,
        CardId::Applications,
        CardId::Outputs,
        CardId::DeviceMap,
        CardId::Profiles,
    ];

    /// Display name for the edit-mode tile.
    pub(crate) fn title(self) -> &'static str {
        match self {
            CardId::Effects => "Effects",
            CardId::Applications => "Applications",
            CardId::Outputs => "Outputs",
            CardId::DeviceMap => "Device → Profile",
            CardId::Profiles => "Profiles",
        }
    }
}

/// Which side column a card lives in. The center column is always EQ Bands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardCol {
    Left,
    Right,
}

/// The user's card arrangement. Every `CardId` appears exactly once across the
/// two columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CardLayout {
    pub(crate) left: Vec<CardId>,
    pub(crate) right: Vec<CardId>,
}

impl Default for CardLayout {
    fn default() -> Self {
        Self {
            left: vec![CardId::Effects],
            right: vec![
                CardId::Applications,
                CardId::Outputs,
                CardId::DeviceMap,
                CardId::Profiles,
            ],
        }
    }
}

impl CardLayout {
    pub(crate) fn column(&self, col: CardCol) -> &[CardId] {
        match col {
            CardCol::Left => &self.left,
            CardCol::Right => &self.right,
        }
    }

    fn column_mut(&mut self, col: CardCol) -> &mut Vec<CardId> {
        match col {
            CardCol::Left => &mut self.left,
            CardCol::Right => &mut self.right,
        }
    }

    fn locate(&self, id: CardId) -> Option<(CardCol, usize)> {
        if let Some(i) = self.left.iter().position(|&c| c == id) {
            return Some((CardCol::Left, i));
        }
        self.right
            .iter()
            .position(|&c| c == id)
            .map(|i| (CardCol::Right, i))
    }

    /// Move `id` to slot `to_idx` in `to_col`. Removes it from its current
    /// position first; `to_idx` is clamped to the target length and decremented
    /// when moving forward within the same column (the removal shifts later
    /// indices). No-op if `id` is not present.
    pub(crate) fn move_card(&mut self, id: CardId, to_col: CardCol, to_idx: usize) {
        let Some((from_col, from_idx)) = self.locate(id) else {
            return;
        };
        self.column_mut(from_col).remove(from_idx);
        let mut idx = to_idx;
        if from_col == to_col && from_idx < to_idx {
            idx -= 1;
        }
        let v = self.column_mut(to_col);
        let idx = idx.min(v.len());
        v.insert(idx, id);
    }

    /// True when the layout holds exactly the 5 known cards, once each.
    fn is_valid(&self) -> bool {
        let all: Vec<CardId> = self.left.iter().chain(&self.right).copied().collect();
        all.len() == CardId::ALL.len() && CardId::ALL.iter().all(|c| all.contains(c))
    }

    /// Parse persisted JSON, falling back to the default on any parse error or if
    /// the parsed layout doesn't contain exactly the known cards (guards corrupt
    /// or version-skewed storage).
    pub(crate) fn from_json_or_default(s: &str) -> Self {
        serde_json::from_str::<CardLayout>(s)
            .ok()
            .filter(CardLayout::is_valid)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_todays_arrangement() {
        let l = CardLayout::default();
        assert_eq!(l.left, vec![CardId::Effects]);
        assert_eq!(
            l.right,
            vec![
                CardId::Applications,
                CardId::Outputs,
                CardId::DeviceMap,
                CardId::Profiles
            ]
        );
        assert!(l.is_valid());
    }

    #[test]
    fn move_across_columns() {
        let mut l = CardLayout::default();
        l.move_card(CardId::Effects, CardCol::Right, 0);
        assert!(l.left.is_empty());
        assert_eq!(l.right[0], CardId::Effects);
        assert!(l.is_valid());
    }

    #[test]
    fn reorder_within_column_forward() {
        let mut l = CardLayout::default();
        l.move_card(CardId::Applications, CardCol::Right, 4);
        assert_eq!(
            l.right,
            vec![
                CardId::Outputs,
                CardId::DeviceMap,
                CardId::Profiles,
                CardId::Applications
            ]
        );
        assert!(l.is_valid());
    }

    #[test]
    fn reorder_within_column_backward() {
        let mut l = CardLayout::default();
        l.move_card(CardId::Profiles, CardCol::Right, 0);
        assert_eq!(
            l.right,
            vec![
                CardId::Profiles,
                CardId::Applications,
                CardId::Outputs,
                CardId::DeviceMap
            ]
        );
        assert!(l.is_valid());
    }

    #[test]
    fn move_last_card_out_leaves_empty_column() {
        let mut l = CardLayout::default();
        l.move_card(CardId::Effects, CardCol::Right, 4);
        assert!(l.left.is_empty());
        assert!(l.is_valid());
    }

    #[test]
    fn clamps_index_past_end() {
        let mut l = CardLayout::default();
        l.move_card(CardId::Effects, CardCol::Right, 999);
        assert_eq!(l.right.last(), Some(&CardId::Effects));
        assert!(l.is_valid());
    }

    #[test]
    fn always_five_cards_after_moves() {
        let mut l = CardLayout::default();
        l.move_card(CardId::Effects, CardCol::Right, 2);
        l.move_card(CardId::Profiles, CardCol::Left, 0);
        l.move_card(CardId::Outputs, CardCol::Left, 5);
        assert_eq!(l.left.len() + l.right.len(), 5);
        assert!(l.is_valid());
    }

    #[test]
    fn json_round_trip() {
        let mut l = CardLayout::default();
        l.move_card(CardId::Profiles, CardCol::Left, 0);
        let s = serde_json::to_string(&l).unwrap();
        assert_eq!(CardLayout::from_json_or_default(&s), l);
    }

    #[test]
    fn invalid_json_falls_back_to_default() {
        assert_eq!(
            CardLayout::from_json_or_default("garbage"),
            CardLayout::default()
        );
        let partial = r#"{"left":["Effects"],"right":["Profiles"]}"#;
        assert_eq!(
            CardLayout::from_json_or_default(partial),
            CardLayout::default()
        );
    }
}
```

- [ ] **Step 2: Register the module**

In `crates/resonance-gui/src/main.rs`, add the module declaration next to the others (keep alphabetical grouping — after `mod browser;`):

```rust
mod card_layout;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p resonance-gui card_layout`
Expected: 9 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/resonance-gui/src/card_layout.rs crates/resonance-gui/src/main.rs
git commit -m "feat(gui): card-layout model for reorderable control cards"
```

---

### Task 2: App state — persist the layout + edit flag

**Files:**
- Modify: `crates/resonance-gui/src/app.rs` — `GuiApp` struct, `new()` loader, `save()`, `reset_layout()`

**Interfaces:**
- Consumes: `CardLayout`, `CardId`, `CardCol` (Task 1).
- Produces: `GuiApp.layout: CardLayout`, `GuiApp.layout_edit: bool`, `GuiApp.pending_card_move: Option<(CardId, CardCol, usize)>`.

- [ ] **Step 1: Import the types**

In `crates/resonance-gui/src/app.rs`, add near the other `use crate::...` imports:

```rust
use crate::card_layout::{CardCol, CardId, CardLayout};
```

- [ ] **Step 2: Add the fields**

In `struct GuiApp`, immediately after the `show_dither: bool,` field (added by the prior feature), add:

```rust
    /// User's arrangement of the movable control cards (persisted).
    pub(crate) layout: CardLayout,
    /// Session-only "arrange the layout" mode: shows draggable card tiles + drop
    /// zones instead of the live cards. Never persisted.
    pub(crate) layout_edit: bool,
    /// A card move requested this frame by a drop, applied after the columns
    /// finish rendering (so the lists aren't mutated mid-iteration).
    pub(crate) pending_card_move: Option<(CardId, CardCol, usize)>,
```

- [ ] **Step 3: Initialise them in the constructor**

In `new()`, immediately after the `show_dither: cc.storage...` initializer, add:

```rust
            layout: cc
                .storage
                .and_then(|s| s.get_string("card_layout"))
                .map(|s| CardLayout::from_json_or_default(&s))
                .unwrap_or_default(),
            layout_edit: false,
            pending_card_move: None,
```

- [ ] **Step 4: Persist on save**

In `fn save`, after the `storage.set_string("show_dither", ...)` line, add:

```rust
        if let Ok(j) = serde_json::to_string(&self.layout) {
            storage.set_string("card_layout", j);
        }
```

- [ ] **Step 5: Reset the arrangement in `reset_layout`**

In `reset_layout` (the fn that clears the panel splitters), add before `self.set_status("layout reset");`:

```rust
        self.layout = CardLayout::default();
```

- [ ] **Step 6: Verify it compiles**

Run: `cargo build -p resonance-gui`
Expected: builds clean (the new fields are unused until Task 4 — no error, and `pub(crate)` fields don't warn as dead code the way private ones would; if clippy later flags them, Task 4 consumes them).

- [ ] **Step 7: Commit**

```bash
git add crates/resonance-gui/src/app.rs
git commit -m "feat(gui): persist card layout + edit-mode state"
```

---

### Task 3: Grip icon

**Files:**
- Modify: `crates/resonance-gui/src/ui/icons.rs` — `Icon` enum, `ALL`, `paths` dispatch, new `draw_grip`

**Interfaces:**
- Produces: `Icon::Grip` (a 6-dot drag handle).

- [ ] **Step 1: Add the enum variant**

In `crates/resonance-gui/src/ui/icons.rs`, in `enum Icon`, add after `Help,`:

```rust
    Grip,
```

- [ ] **Step 2: Add the gallery entry**

In the `ALL` array, add after `(Icon::Help, "Help"),`:

```rust
    (Icon::Grip, "Grip"),
```

- [ ] **Step 3: Add the dispatch arm**

In `fn paths`, add after `Icon::Help => draw_help(p),`:

```rust
        Icon::Grip => draw_grip(p),
```

- [ ] **Step 4: Add the draw routine**

Add a new `draw_grip` fn next to the other `draw_*` fns (e.g. after `draw_menu`):

```rust
/// Six dots in two columns — the "drag to reorder" grip handle.
fn draw_grip(p: &Pen) {
    for x in [0.38, 0.62] {
        for y in [0.30, 0.50, 0.70] {
            p.dot(x, y, 0.07);
        }
    }
}
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo build -p resonance-gui`
Expected: builds clean.

- [ ] **Step 6: Commit**

```bash
git add crates/resonance-gui/src/ui/icons.rs
git commit -m "feat(gui): add grip icon for card drag handles"
```

---

### Task 4: Data-driven card rendering (normal mode)

Render the wide columns and the narrow accordion from `self.layout` instead of the hardcoded order — with no visual change to the default arrangement and no drag yet.

**Files:**
- Modify: `crates/resonance-gui/src/ui/layout.rs` — imports, `lower_columns`, `accordion_stack`, new `render_card` + `render_lower_column`
- Modify: `crates/resonance-gui/src/ui/devices.rs` — remove `devices_profiles`, add `profiles_saved_hint`

**Interfaces:**
- Consumes: `CardLayout`, `CardId`, `CardCol` (Task 1); `layout` field (Task 2).
- Produces: `GuiApp::render_card(&mut self, &mut Ui, &DaemonState, CardId) -> bool`, `GuiApp::render_lower_column(&mut self, &mut Ui, &DaemonState, CardCol)`, `GuiApp::profiles_saved_hint(&self) -> String`.

- [ ] **Step 1: Add the imports to layout.rs**

In `crates/resonance-gui/src/ui/layout.rs`, add after `use crate::ui::kit;`:

```rust
use crate::card_layout::{CardCol, CardId};
```

(`CardLayout` itself isn't named until Task 5's edit banner, so it's added to this import there — importing it now would trip `clippy::unused_imports` under `-D warnings`.)

- [ ] **Step 2: Replace `devices_profiles` with `profiles_saved_hint`**

In `crates/resonance-gui/src/ui/devices.rs`, delete the entire `devices_profiles` fn:

```rust
    pub(crate) fn devices_profiles(&mut self, ui: &mut egui::Ui) {
        section_hint(ui, "Device → Profile", "auto-switch", |ui| {
            self.device_mapping_section(ui);
        });
        ui.add_space(12.0);
        let n = self.profiles.len();
        let filt = self.profile_filter.trim().to_lowercase();
        let saved = if filt.is_empty() {
            format!("{n} saved")
        } else {
            let shown = self
                .profiles
                .iter()
                .filter(|p| p.to_lowercase().contains(&filt))
                .count();
            format!("{shown}/{n} saved")
        };
        section_hint(ui, "Profiles", &saved, |ui| self.profiles_panel(ui));
        // Channels now lives under Effects (left column, matching the mock).
    }
```

and add in its place:

```rust
    /// Right-aligned hint for the Profiles card head: "N saved", or "shown/N
    /// saved" when the filter is active.
    pub(crate) fn profiles_saved_hint(&self) -> String {
        let n = self.profiles.len();
        let filt = self.profile_filter.trim().to_lowercase();
        if filt.is_empty() {
            format!("{n} saved")
        } else {
            let shown = self
                .profiles
                .iter()
                .filter(|p| p.to_lowercase().contains(&filt))
                .count();
            format!("{shown}/{n} saved")
        }
    }
```

`section_hint` was only used by `devices_profiles`, so it's now unused in `devices.rs`. Change its import line from:

```rust
use crate::ui::widgets::{gain_color, section_hint};
```

to:

```rust
use crate::ui::widgets::gain_color;
```

(`device_mapping_section`, `profiles_panel`/`profiles_section`, and `channels_section` remain and are still used.)

- [ ] **Step 3: Add `render_card` + `render_lower_column` to layout.rs**

In `crates/resonance-gui/src/ui/layout.rs`, inside `impl GuiApp`, add these two methods (place them just above `lower_columns`):

```rust
    /// Render one control card by id, wrapped in its section frame. Returns true
    /// if it drew anything — absent Applications/Outputs cards draw nothing and
    /// return false, so the caller can skip their inter-card spacing.
    fn render_card(&mut self, ui: &mut egui::Ui, s: &DaemonState, id: CardId) -> bool {
        match id {
            CardId::Effects => {
                section_hint(ui, "Effects", "DSP sound effects", |ui| {
                    self.effects_section(ui, s);
                });
                // Output stage (dither) rides under Effects when enabled.
                if self.show_dither {
                    ui.add_space(12.0);
                    section_hint(ui, "Output", "dither", |ui| {
                        self.output_section(ui, s);
                    });
                }
                true
            }
            CardId::Applications => {
                if s.apps.is_empty() {
                    return false;
                }
                section_hint(ui, "Applications", "per-app volume", |ui| {
                    self.apps_section(ui, s);
                });
                true
            }
            CardId::Outputs => {
                if s.sinks.is_empty() {
                    return false;
                }
                section_hint(ui, "Outputs", "device volume", |ui| {
                    self.sinks_section(ui, s);
                });
                true
            }
            CardId::DeviceMap => {
                section_hint(ui, "Device → Profile", "auto-switch", |ui| {
                    self.device_mapping_section(ui);
                });
                true
            }
            CardId::Profiles => {
                let saved = self.profiles_saved_hint();
                section_hint(ui, "Profiles", &saved, |ui| self.profiles_panel(ui));
                true
            }
        }
    }

    /// Render one wide-layout side column from the persisted card order. Normal
    /// mode draws the live cards (skipping absent ones); edit mode (Task 5) draws
    /// compact draggable tiles with drop zones.
    fn render_lower_column(&mut self, ui: &mut egui::Ui, s: &DaemonState, col: CardCol) {
        let ids = self.layout.column(col).to_vec();
        for id in &ids {
            if self.render_card(ui, s, *id) {
                ui.add_space(12.0);
            }
        }
    }
```

- [ ] **Step 4: Rewrite the two side columns in `lower_columns`**

In `lower_columns`, replace the left-column `.show_inside` closure body:

```rust
            .show_inside(ui, |ui| {
                if let Some(s) = state {
                    padded_scroll(ui, "effects_scroll", |ui| {
                        section_hint(ui, "Effects", "DSP sound effects", |ui| {
                            self.effects_section(ui, s);
                        });
                        // Output stage (dither) — advanced, off by default. The
                        // Channels controls now live in the Settings dialog (gear
                        // icon) to keep the main view uncluttered.
                        if self.show_dither {
                            ui.add_space(12.0);
                            section_hint(ui, "Output", "dither", |ui| {
                                self.output_section(ui, s);
                            });
                        }
                    });
                }
            });
```

with:

```rust
            .show_inside(ui, |ui| {
                if let Some(s) = state {
                    padded_scroll(ui, "effects_scroll", |ui| {
                        self.render_lower_column(ui, s, CardCol::Left);
                    });
                }
            });
```

and replace the right-column `.show_inside` closure body:

```rust
            .show_inside(ui, |ui| {
                padded_scroll(ui, "side", |ui| {
                    // Applications (per-app volume) sits at the top of the right
                    // column when the backend reports app streams — adjusted more
                    // often than the device→profile mapping below it.
                    if let Some(s) = state {
                        if !s.apps.is_empty() {
                            section_hint(ui, "Applications", "per-app volume", |ui| {
                                self.apps_section(ui, s);
                            });
                            ui.add_space(12.0);
                        }
                        if !s.sinks.is_empty() {
                            section_hint(ui, "Outputs", "device volume", |ui| {
                                self.sinks_section(ui, s);
                            });
                            ui.add_space(12.0);
                        }
                    }
                    self.devices_profiles(ui);
                });
            });
```

with:

```rust
            .show_inside(ui, |ui| {
                if let Some(s) = state {
                    padded_scroll(ui, "side", |ui| {
                        self.render_lower_column(ui, s, CardCol::Right);
                    });
                }
            });
```

- [ ] **Step 5: Rewrite `accordion_stack` (narrow) to follow the layout**

Replace the body of `accordion_stack`'s frame closure:

```rust
                if let Some(s) = state {
                    section_hint(ui, "Effects", "DSP sound effects", |ui| {
                        self.effects_section(ui, s);
                    });
                    if self.show_dither {
                        ui.add_space(GAP);
                        section_hint(ui, "Output", "dither", |ui| {
                            self.output_section(ui, s);
                        });
                    }
                    if !s.apps.is_empty() {
                        ui.add_space(GAP);
                        section_hint(ui, "Applications", "per-app volume", |ui| {
                            self.apps_section(ui, s);
                        });
                    }
                    if !s.sinks.is_empty() {
                        ui.add_space(GAP);
                        section_hint(ui, "Outputs", "device volume", |ui| {
                            self.sinks_section(ui, s);
                        });
                    }
                    ui.add_space(GAP);
                    section(ui, "EQ bands", |ui| self.bands_section(ui, s));
                    // Channels controls relocated to the Settings dialog.
                }
                ui.add_space(GAP);
                section_hint(ui, "Device → Profile", "auto-switch", |ui| {
                    self.device_mapping_section(ui);
                });
                ui.add_space(GAP);
                section(ui, "Profiles", |ui| self.profiles_panel(ui));
```

with:

```rust
                if let Some(s) = state {
                    // Left-column cards, then the fixed EQ Bands anchor, then the
                    // right-column cards — reflecting the wide-layout arrangement.
                    for id in self.layout.left.clone() {
                        if self.render_card(ui, s, id) {
                            ui.add_space(GAP);
                        }
                    }
                    section(ui, "EQ bands", |ui| self.bands_section(ui, s));
                    ui.add_space(GAP);
                    for id in self.layout.right.clone() {
                        if self.render_card(ui, s, id) {
                            ui.add_space(GAP);
                        }
                    }
                }
```

- [ ] **Step 6: Build + verify default layout unchanged**

Run: `cargo build -p resonance-gui && cargo test -p resonance-gui`
Expected: builds clean, all tests pass.

Then capture a demo screenshot and confirm the default arrangement matches the pre-change layout (Effects left; Applications/Outputs/Device→Profile/Profiles right):

Run:
```bash
cargo build -p resonance-gui && RESONANCE_DEMO=1 UISHOT_OUT="$PWD/../shots_cards" contrib/dev/uishot.sh --wait 5 1240x760
```
Expected: `wrote .../ui_1240x760.png`. Eyeball it — the left column shows Effects, the right column shows Applications, Outputs, Device→Profile, Profiles (same as before). Then `rm -rf ../shots_cards`.

- [ ] **Step 7: Commit**

```bash
git add crates/resonance-gui/src/ui/layout.rs crates/resonance-gui/src/ui/devices.rs
git commit -m "feat(gui): render control columns + accordion from card layout"
```

---

### Task 5: Edit mode — draggable tiles, drop zones, and the toggle

**Files:**
- Modify: `crates/resonance-gui/src/ui/toolbar.rs` — overflow menu "Edit layout" toggle
- Modify: `crates/resonance-gui/src/ui/layout.rs` — `render_lower_column` (edit branch), new `card_tile` / `drop_gap` / `layout_edit_banner`, `lower_columns` (banner + apply move)
- Modify: `crates/resonance-gui/src/app.rs` — dev hook to force edit mode for screenshots

**Interfaces:**
- Consumes: `layout`, `layout_edit`, `pending_card_move` (Task 2); `CardLayout::move_card` (Task 1); `Icon::Grip` (Task 3); `render_lower_column` (Task 4).

- [ ] **Step 1: Add the "Edit layout" toggle to the overflow menu**

In `crates/resonance-gui/src/ui/toolbar.rs`, in `overflow_menu`'s "View" section, after the "Reset layout" item, add:

```rust
                let editing = self.layout_edit;
                if kit::menu_item(ui, "Edit layout", editing) {
                    self.layout_edit = !editing;
                }
```

- [ ] **Step 2: Add the edit-mode render helpers to layout.rs**

First extend the card-layout import (the edit banner names `CardLayout::default()`): change

```rust
use crate::card_layout::{CardCol, CardId};
```

to

```rust
use crate::card_layout::{CardCol, CardId, CardLayout};
```

Then, in `crates/resonance-gui/src/ui/layout.rs`, inside `impl GuiApp`, add these three methods (place them just below `render_lower_column`):

```rust
    /// A compact draggable tile (grip + card name) shown in edit mode. The whole
    /// tile is the drag source carrying the card's `CardId`.
    fn card_tile(&mut self, ui: &mut egui::Ui, id: CardId) {
        ui.dnd_drag_source(egui::Id::new(("card_tile", id)), id, |ui| {
            let t = kit::tokens(ui);
            egui::Frame::default()
                .fill(ui.visuals().faint_bg_color)
                .stroke(egui::Stroke::new(1.0, t.line))
                .corner_radius(egui::CornerRadius::same(kit::R_CARD as u8))
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.set_width(ui.available_width());
                        let (r, _) = ui
                            .allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                        crate::ui::icons::draw(
                            ui.painter(),
                            crate::ui::icons::Icon::Grip,
                            r,
                            t.dim,
                        );
                        ui.add_space(6.0);
                        ui.label(id.title());
                    });
                });
        });
    }

    /// A thin full-width drop target between/around card tiles in edit mode. When
    /// a card is released over it, records the pending move to `(col, idx)`.
    fn drop_gap(&mut self, ui: &mut egui::Ui, col: CardCol, idx: usize) {
        let frame = egui::Frame::default().inner_margin(egui::Margin::symmetric(0, 3));
        let (_, payload) = ui.dnd_drop_zone::<CardId, _>(frame, |ui| {
            ui.allocate_exact_size(
                egui::vec2(ui.available_width().max(1.0), 8.0),
                egui::Sense::hover(),
            );
        });
        if let Some(p) = payload {
            self.pending_card_move = Some((*p, col, idx));
        }
    }

    /// The banner shown across the top of the controls strip while arranging.
    fn layout_edit_banner(&mut self, ui: &mut egui::Ui) {
        let t = kit::tokens(ui);
        egui::Frame::default()
            .fill(t.accent.gamma_multiply(0.18))
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Arranging layout — drag cards between the side columns.");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Done").clicked() {
                            self.layout_edit = false;
                        }
                        if ui.button("Reset").clicked() {
                            self.layout = CardLayout::default();
                        }
                    });
                });
            });
    }
```

- [ ] **Step 3: Add the edit branch to `render_lower_column`**

In `render_lower_column` (from Task 4), replace the whole body:

```rust
    fn render_lower_column(&mut self, ui: &mut egui::Ui, s: &DaemonState, col: CardCol) {
        let ids = self.layout.column(col).to_vec();
        for id in &ids {
            if self.render_card(ui, s, *id) {
                ui.add_space(12.0);
            }
        }
    }
```

with:

```rust
    fn render_lower_column(&mut self, ui: &mut egui::Ui, s: &DaemonState, col: CardCol) {
        let ids = self.layout.column(col).to_vec();
        if self.layout_edit {
            // Only show the drop gaps once a drag is in flight, so an idle edit
            // mode stays uncluttered.
            let dragging = egui::DragAndDrop::has_payload_of_type::<CardId>(ui.ctx());
            for (idx, id) in ids.iter().enumerate() {
                if dragging {
                    self.drop_gap(ui, col, idx);
                }
                self.card_tile(ui, *id);
                ui.add_space(6.0);
            }
            if dragging {
                self.drop_gap(ui, col, ids.len());
            } else if ids.is_empty() {
                ui.weak("(empty — drag a card here)");
            }
        } else {
            for id in &ids {
                if self.render_card(ui, s, *id) {
                    ui.add_space(12.0);
                }
            }
        }
    }
```

- [ ] **Step 4: Add the banner + apply the pending move in `lower_columns`**

In `lower_columns`, add the banner panel as the very first thing inside the fn (before the `egui::Panel::left(...)`):

```rust
        // Edit-mode banner spans the top of the controls strip.
        if self.layout_edit {
            egui::Panel::top("layout_edit_banner")
                .frame(egui::Frame::NONE)
                .show_separator_line(false)
                .show_inside(ui, |ui| self.layout_edit_banner(ui));
        }
```

Then, at the very end of `lower_columns` (after the central `CentralPanel` that renders `bands_card`), add:

```rust
        // Apply a card move requested by a drop this frame, now that both columns
        // have finished rendering (never mutate the lists mid-iteration).
        if let Some((id, col, idx)) = self.pending_card_move.take() {
            self.layout.move_card(id, col, idx);
        }
```

- [ ] **Step 5: Add a dev hook to force edit mode for screenshots**

In `crates/resonance-gui/src/app.rs`, find the demo/dev hooks (search for `RESONANCE_DEMO` in the constructor — the line `demo: std::env::var("RESONANCE_DEMO").is_ok(),`). In the same `Self { ... }` initializer, change the `layout_edit: false,` line (added in Task 2) to:

```rust
            layout_edit: std::env::var("RESONANCE_EDIT_LAYOUT").is_ok(),
```

(A dev/screenshot-only hook; unset in normal use, so the app always launches in normal mode.)

- [ ] **Step 6: Build + verify edit mode renders**

Run: `cargo build -p resonance-gui && cargo clippy -p resonance-gui --all-targets 2>&1 | grep -E "error|warning:" | head`
Expected: builds clean, no clippy errors/warnings. (If `clippy::similar_names` fires on `from_col`/`to_col` in `move_card`, add `#[allow(clippy::similar_names)]` above `move_card` with the comment `// from_col / to_col are deliberately parallel.`)

Capture a screenshot in forced edit mode:
```bash
cargo build -p resonance-gui && RESONANCE_DEMO=1 RESONANCE_EDIT_LAYOUT=1 UISHOT_OUT="$PWD/../shots_edit" contrib/dev/uishot.sh --wait 5 1240x760
```
Expected: the shot shows the "Arranging layout" banner with Done/Reset, and the side columns rendered as compact grip+name tiles (Effects on the left; Applications/Outputs/Device→Profile/Profiles on the right). Then `rm -rf ../shots_edit`.

- [ ] **Step 7: Commit**

```bash
git add crates/resonance-gui/src/ui/toolbar.rs crates/resonance-gui/src/ui/layout.rs crates/resonance-gui/src/app.rs
git commit -m "feat(gui): edit mode with draggable card tiles + drop zones"
```

---

### Task 6: Full gate + manual drag verification

**Files:** none (verification); minor clippy cleanup if flagged.

- [ ] **Step 1: Run the full gate**

Run: `make check`
Expected: fmt clean, clippy `-D warnings` clean, all tests pass. If `fmt` fails, run `make fmt-fix` and re-run; if clippy flags an unused import (e.g. `section_hint` in `devices.rs`, or `CardLayout` in `layout.rs` if unused), remove only genuinely-unused imports and re-run.

- [ ] **Step 2: Manual drag verification (live session, no audio impact needed)**

Launch the demo GUI headed (or ask the user): `RESONANCE_DEMO=1 cargo run -p resonance-gui`. Then:
- Open ☰ → "Edit layout": the banner appears and cards become tiles.
- Drag a tile (e.g. Profiles) by its grip from the right column to the left column — the drop gaps highlight; on release it moves. Drag it back; reorder within a column.
- Click "Done": the live cards render in the new arrangement.
- Reopen and confirm the arrangement persisted (close + relaunch, or ☰ → "Edit layout" again).
- ☰ → "Reset layout" restores the default arrangement.

(Dragging can't be scripted in the screenshot harness; this step is a real interactive check. If running headless-only, rely on the `move_card` unit tests + the edit-mode screenshot from Task 5 and note the interactive drag is unverified.)

- [ ] **Step 3: Commit any fmt/clippy fixes**

```bash
git add -A && git commit -m "style(gui): fmt + clippy cleanup" || echo "nothing to commit"
```

---

## Notes for the implementer

- **Line numbers drift** as you edit — match on the quoted code, not numbers.
- **egui 0.34 DnD:** `ui.dnd_drag_source(id, payload, add_contents)` paints the source at the cursor while dragging; `ui.dnd_drop_zone::<CardId, _>(frame, add_contents)` returns `(_, Option<Arc<CardId>>)` — `Some` on release over the zone, and it auto-highlights when a compatible payload hovers. `egui::DragAndDrop::has_payload_of_type::<CardId>(ctx)` tells you a card drag is active.
- **Persistence timing:** eframe calls `save()` on its auto-save timer and on exit — the same mechanism `theme`/`per_channel_eq` rely on. No explicit save call is needed after a move.
- **Borrow tip:** clone the column vec (`self.layout.column(col).to_vec()`) before iterating, since the loop body calls `&mut self` methods.
- **Do not** make the graph or EQ Bands draggable — they're fixed anchors.
