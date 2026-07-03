# Arrange-mode pane add/remove + dual-column layout — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the GUI a WYSIWYG arrange mode that adds/removes panes via a Hidden tray (drag + × / ＋ buttons), make EQ bands a removable flexible center, and give the bands-hidden live layout a Columns/Stack preference — all reusing the existing `hidden_panes` + `CardLayout` state.

**Architecture:** Reuse `hidden_panes: HashSet<PaneId>` (which panes are hidden) and `CardLayout.left/right` (card positions; hidden cards keep their slot). A single `PaneAction` enum, applied after each arrange frame, expresses hide/show/place. Live layout picks 3/2/1 columns from bands visibility + which card columns are populated; bands-hidden picks Columns vs Stacked from a persisted `BandsOffLayout` preference. Arrange mode renders compact draggable tiles + a Hidden tray with typed `PaneId` drag payloads and per-zone validation.

**Tech Stack:** Rust, egui/eframe (`dnd_drag_source` / `dnd_drop_zone` / `ui.columns`), serde/serde_json.

## Global Constraints

- Conventional Commits, all lowercase.
- **No AI-related content anywhere** — no `Co-Authored-By` / AI-attribution trailer on commits.
- `make check` (= `cargo fmt --all -- --check` + `cargo clippy --all-targets --all-features -- -D warnings` + `cargo test --all`) passes before every commit. Clippy pedantic is enforced workspace-wide.
- Binary crate: a new `pub(crate)` item unused by non-test code fails `-D dead_code`; land each item together with a consumer (bundle logic + first use in the same commit if needed).
- Functional style; match surrounding comment density and idiom.
- No DSP/IPC/daemon/audio surface → no `resonance verify`.
- Branch: `feat/gui-arrange-pane-management` (already created; spec committed there).
- Screenshot verification uses an **isolated** `XDG_DATA_HOME` (never `UISHOT_REAL=1`) so the user's real `~/.local/share/resonance` is untouched. Storage file: `$XDG_DATA_HOME/resonance/app.ron`, a flat RON map of string values. Dev hooks: `RESONANCE_DEMO=1` (full UI, no daemon), `RESONANCE_EDIT_LAYOUT=1` (start in arrange mode), `RESONANCE_OPEN=settings` (open Settings dialog), `RESONANCE_WINDOW_SIZE=WxH`.

---

### Task 1: `BandsOffLayout` preference

**Files:**
- Modify: `crates/resonance-gui/src/panes.rs` (add enum + tests)
- Modify: `crates/resonance-gui/src/app.rs` (field, load, save, reset)

**Interfaces:**
- Produces: `enum BandsOffLayout { Columns, Stacked }` with `Default = Columns`, `from_storage(&str) -> Self`, `as_storage(self) -> &'static str`; `GuiApp.bands_off_layout: BandsOffLayout`.

- [ ] **Step 1: Add the enum + failing tests to `panes.rs`**

Append to `crates/resonance-gui/src/panes.rs` (after `hidden_from_json_or_default`):

```rust
/// How the live lower area lays out when EQ bands is hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum BandsOffLayout {
    /// Cards render in their Left/Right columns side by side (two equal columns
    /// when both are populated; one fills the width when only one is).
    #[default]
    Columns,
    /// All visible cards stack in a single full-width column.
    Stacked,
}

impl BandsOffLayout {
    /// Parse the persisted value; anything unrecognised falls back to the
    /// default (`Columns`).
    pub(crate) fn from_storage(s: &str) -> Self {
        match s {
            "stacked" => Self::Stacked,
            _ => Self::Columns,
        }
    }

    /// The stable string written to storage.
    pub(crate) fn as_storage(self) -> &'static str {
        match self {
            Self::Columns => "columns",
            Self::Stacked => "stacked",
        }
    }
}
```

Add to the `#[cfg(test)] mod tests` in the same file:

```rust
    #[test]
    fn bands_off_layout_storage_round_trip() {
        for v in [BandsOffLayout::Columns, BandsOffLayout::Stacked] {
            assert_eq!(BandsOffLayout::from_storage(v.as_storage()), v);
        }
    }

    #[test]
    fn bands_off_layout_defaults_to_columns() {
        assert_eq!(BandsOffLayout::default(), BandsOffLayout::Columns);
        assert_eq!(BandsOffLayout::from_storage("garbage"), BandsOffLayout::Columns);
    }
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo test -p resonance-gui panes::`
Expected: PASS (7 tests — the 5 existing + 2 new).

- [ ] **Step 3: Add the field to `GuiApp` + load/save/reset**

In `crates/resonance-gui/src/app.rs`, extend the `panes` import:

```rust
use crate::panes::{hidden_from_json_or_default, BandsOffLayout, PaneId};
```

Add the field right after `hidden_panes` (line ~356):

```rust
    pub(crate) hidden_panes: std::collections::HashSet<PaneId>,
    /// Live lower-area layout when EQ bands is hidden (persisted).
    pub(crate) bands_off_layout: BandsOffLayout,
```

Initialise it in `new()` right after the `hidden_panes:` initialiser (line ~727):

```rust
            bands_off_layout: cc
                .storage
                .and_then(|s| s.get_string("bands_off_layout"))
                .map(|s| BandsOffLayout::from_storage(&s))
                .unwrap_or_default(),
```

Persist it in `save()` after the `hidden_panes` block (line ~1271):

```rust
        storage.set_string("bands_off_layout", self.bands_off_layout.as_storage().to_string());
```

Leave `reset_layout` as-is for now (the arrange-banner Reset handles this pref in Task 2; `reset_layout` resets card order + unhides, and the pref is a separate user choice that need not reset). No behavioural consumer yet — the field is read in Task 2.

- [ ] **Step 4: Guard against dead_code and build**

Because nothing reads `bands_off_layout` until Task 2, add a temporary `#[allow(dead_code)]` on the field to keep `-D dead_code` happy for this commit:

```rust
    /// Live lower-area layout when EQ bands is hidden (persisted).
    #[allow(dead_code)] // consumed by the live layout in the next task
    pub(crate) bands_off_layout: BandsOffLayout,
```

Run: `make check`
Expected: EXIT 0, no warnings. (Remove the `#[allow(dead_code)]` in Task 2 when the field is first read.)

- [ ] **Step 5: Commit**

```bash
git add crates/resonance-gui/src/panes.rs crates/resonance-gui/src/app.rs
git commit -m "feat(gui): bands-off layout preference (columns/stacked)"
```

---

### Task 2: Live bands-hidden dual/stack + arrange-banner control

**Files:**
- Modify: `crates/resonance-gui/src/ui/layout.rs` (`lower_columns_live`, `layout_edit_banner`)
- Modify: `crates/resonance-gui/src/app.rs` (remove the Task-1 `#[allow(dead_code)]`)

**Interfaces:**
- Consumes: `GuiApp.bands_off_layout` (Task 1), `visible_cards` / `render_card` (existing).

- [ ] **Step 1: Implement bands-hidden dual vs stack in `lower_columns_live`**

In `crates/resonance-gui/src/ui/layout.rs`, replace the `else` branch of `lower_columns_live` (the "Bands hidden" block that currently stacks) with a Columns/Stacked switch. Find:

```rust
        } else {
            // Bands hidden: no side panels — the remaining visible cards stack
            // full-width in a scroll area (the same card widget the narrow
            // accordion uses).
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
```

Replace with:

```rust
        } else {
            // Bands hidden: layout per the user's preference.
            let Some(s) = state else { return };
            match self.bands_off_layout {
                // Stacked: all visible cards in one full-width scrolled column.
                crate::panes::BandsOffLayout::Stacked => {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show_inside(ui, |ui| {
                            padded_scroll(ui, "stacked_cards", |ui| {
                                for id in left_cards.iter().chain(&right_cards) {
                                    if self.render_card(ui, s, *id) {
                                        ui.add_space(12.0);
                                    }
                                }
                            });
                        });
                }
                // Columns: two equal side-by-side columns. If only one side has
                // visible cards it fills the width; both populated → 50/50.
                crate::panes::BandsOffLayout::Columns => {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show_inside(ui, |ui| {
                            let render_col = |app: &mut Self, ui: &mut egui::Ui, cards: &[CardId], salt: &str| {
                                padded_scroll(ui, salt, |ui| {
                                    for id in cards {
                                        if app.render_card(ui, s, *id) {
                                            ui.add_space(12.0);
                                        }
                                    }
                                });
                            };
                            match (left_cards.is_empty(), right_cards.is_empty()) {
                                (false, false) => {
                                    // Two equal columns filling the width.
                                    ui.columns(2, |cols| {
                                        render_col(self, &mut cols[0], &left_cards, "dual_left");
                                        render_col(self, &mut cols[1], &right_cards, "dual_right");
                                    });
                                }
                                (false, true) => render_col(self, ui, &left_cards, "dual_left"),
                                (true, false) => render_col(self, ui, &right_cards, "dual_right"),
                                (true, true) => {} // lower_has_content() would be false here
                            }
                        });
                }
            }
        }
```

Note: the closure borrows `s` (a `&DaemonState`) and takes `app: &mut Self` explicitly so the two `cols[..]` calls don't both need `&mut self` captured. `ui.columns` gives equal-width columns filling the available width.

- [ ] **Step 2: Remove the Task-1 dead_code allow**

In `crates/resonance-gui/src/app.rs`, delete the `#[allow(dead_code)]` line above `bands_off_layout` (it's now read by `lower_columns_live`).

- [ ] **Step 3: Add the arrange-banner segmented control**

In `layout_edit_banner` (layout.rs), add a "when EQ bands is hidden" toggle. Replace the banner body's `horizontal` with:

```rust
    fn layout_edit_banner(&mut self, ui: &mut egui::Ui) {
        let t = kit::tokens(ui);
        egui::Frame::default()
            .fill(t.accent.gamma_multiply(0.18))
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Arranging layout — drag panes to Hidden to remove; drag back (or ＋) to add.");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Done").clicked() {
                            self.layout_edit = false;
                        }
                        if ui.button("Reset").clicked() {
                            self.layout = CardLayout::default();
                            self.hidden_panes.clear();
                            self.bands_off_layout = crate::panes::BandsOffLayout::default();
                        }
                        ui.separator();
                        // Bands-off layout preference (applies to the live view).
                        ui.label("EQ bands off:");
                        let mut pref = self.bands_off_layout;
                        egui::ComboBox::from_id_salt("bands_off_layout")
                            .selected_text(match pref {
                                crate::panes::BandsOffLayout::Columns => "Columns",
                                crate::panes::BandsOffLayout::Stacked => "Stack",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut pref, crate::panes::BandsOffLayout::Columns, "Columns");
                                ui.selectable_value(&mut pref, crate::panes::BandsOffLayout::Stacked, "Stack");
                            });
                        self.bands_off_layout = pref;
                    });
                });
            });
    }
```

- [ ] **Step 4: Build + gate**

Run: `make check`
Expected: EXIT 0, no warnings.

- [ ] **Step 5: Screenshot-verify dual vs stack (bands hidden, live)**

Save this as `/tmp/user/1000/claude-1000/-home-nyverino-Documents-resonance/4efd6e91-6570-438d-b9cc-f1414d92d1c7/scratchpad/shot_one.sh` and run it twice (once per preference):

```bash
#!/usr/bin/env bash
set -euo pipefail
NAME="$1"; RON="$2"   # RON = extra app.ron body lines
OUT=/tmp/user/1000/claude-1000/-home-nyverino-Documents-resonance/4efd6e91-6570-438d-b9cc-f1414d92d1c7/scratchpad/shots
xdg="$(mktemp -d)"; mkdir -p "$xdg/data/resonance" "$xdg/config"
printf '{\n    "theme": "Dark",\n%s\n}\n' "$RON" > "$xdg/data/resonance/app.ron"
d=160; while [ -e "/tmp/.X11-unix/X$d" ]; do d=$((d+1)); done
Xvfb ":$d" -screen 0 1240x760x24 -nolisten tcp >/dev/null 2>&1 & xv=$!; sleep 0.6
env -u WAYLAND_DISPLAY XDG_DATA_HOME="$xdg/data" XDG_CONFIG_HOME="$xdg/config" DISPLAY=":$d" \
  WINIT_UNIX_BACKEND=x11 LIBGL_ALWAYS_SOFTWARE=1 RESONANCE_DEMO=1 RESONANCE_WINDOW_SIZE=1240x760 RUST_LOG=error \
  target/debug/resonance-gui >/dev/null 2>&1 & ap=$!; sleep 4
DISPLAY=":$d" import -window root "$OUT/$NAME.png" && echo "wrote $NAME.png"
kill "$ap" 2>/dev/null||true; wait "$ap" 2>/dev/null||true; kill "$xv" 2>/dev/null||true; wait "$xv" 2>/dev/null||true
rm -rf "$xdg"
```

Run:
```bash
cargo build -p resonance-gui
bash .../shot_one.sh live_dual  '    "hidden_panes": "[\"Bands\"]",\n    "bands_off_layout": "columns"'
bash .../shot_one.sh live_stack '    "hidden_panes": "[\"Bands\"]",\n    "bands_off_layout": "stacked"'
```
Expected: `live_dual.png` shows Effects (left) and the right-column cards side-by-side in two columns; `live_stack.png` shows one full-width stacked column. View both with the Read tool to confirm.

- [ ] **Step 6: Commit**

```bash
git add crates/resonance-gui/src/ui/layout.rs crates/resonance-gui/src/app.rs
git commit -m "feat(gui): dual/stack columns when eq bands is hidden (live), banner toggle"
```

---

### Task 3: Remove Settings → Panes + reorder Settings

**Files:**
- Modify: `crates/resonance-gui/src/ui/dialogs.rs` (`settings_dialog`)

**Interfaces:**
- Consumes: nothing new. `hidden_panes` / `pane_visible` stay (used by the layout + arrange mode). Only the Settings *UI* for panes is removed.

- [ ] **Step 1: Delete the Panes section from `settings_dialog`**

In `crates/resonance-gui/src/ui/dialogs.rs`, remove the entire `// ── Panes ──` block — from `ui.add_space(8.0); ui.separator(); ui.add_space(4.0); ui.label(egui::RichText::new("Panes").strong());` through the `if ui.add_enabled(!self.hidden_panes.is_empty(), egui::Button::new("Show all panes")) { … self.hidden_panes.clear(); }` block, inclusive. The remaining order is **Theme → Channels → Advanced features → EQ phase** (the Channels separator now directly follows the Theme list).

Concretely, the Theme block's closing `}` is immediately followed by the Channels block:

```rust
                    let cctx = ui.ctx().clone();
                    for t in Theme::ALL {
                        if ui.selectable_label(self.theme == t, t.label()).clicked() {
                            self.set_theme(&cctx, t);
                        }
                    }

                    // ── Channels ──────────────────────────────────────────────
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Channels").strong());
```

- [ ] **Step 2: Build + gate**

Run: `make check`
Expected: EXIT 0. (`hidden_panes` is still read by the layout, and `pane_visible` too, so no dead_code. `crate::panes::PaneId` import in dialogs.rs — if it becomes unused after removing the section, delete the `use`/qualified references; the Panes loop was the only dialogs.rs user, so remove any now-unused import to satisfy clippy.)

- [ ] **Step 3: Screenshot-verify Settings order**

```bash
cargo build -p resonance-gui
xdg="$(mktemp -d)"; mkdir -p "$xdg/data/resonance" "$xdg/config"
printf '{\n    "theme": "Dark"\n}\n' > "$xdg/data/resonance/app.ron"
d=170; Xvfb ":$d" -screen 0 760x820x24 -nolisten tcp >/dev/null 2>&1 & xv=$!; sleep 0.6
env -u WAYLAND_DISPLAY XDG_DATA_HOME="$xdg/data" XDG_CONFIG_HOME="$xdg/config" DISPLAY=":$d" \
  WINIT_UNIX_BACKEND=x11 LIBGL_ALWAYS_SOFTWARE=1 RESONANCE_DEMO=1 RESONANCE_OPEN=settings \
  RESONANCE_WINDOW_SIZE=760x820 RUST_LOG=error target/debug/resonance-gui >/dev/null 2>&1 & ap=$!; sleep 4
DISPLAY=":$d" import -window root "$OUT/settings_after.png" && echo wrote
kill "$ap" 2>/dev/null||true; kill "$xv" 2>/dev/null||true; rm -rf "$xdg"
```
Expected: sections read **Theme → Channels → Advanced features → EQ phase**; no "Panes" section.

- [ ] **Step 4: Commit**

```bash
git add crates/resonance-gui/src/ui/dialogs.rs
git commit -m "feat(gui): move pane add/remove to arrange mode; drop settings panes section"
```

---

### Task 4: `PaneAction` model + `PaneId` drag payload refactor

**Files:**
- Modify: `crates/resonance-gui/src/panes.rs` (re-add `card()`, add `PaneAction`)
- Modify: `crates/resonance-gui/src/app.rs` (replace `pending_card_move` with `pending_pane_action`, add `apply_pane_action`)
- Modify: `crates/resonance-gui/src/ui/layout.rs` (payloads `CardId`→`PaneId`, apply action)

**Interfaces:**
- Produces: `PaneId::card(self) -> Option<CardId>`; `enum PaneAction { PlaceCard { card: CardId, col: CardCol, idx: usize }, Show(PaneId), Hide(PaneId) }`; `GuiApp.pending_pane_action: Option<PaneAction>`; `GuiApp::apply_pane_action(&mut self, PaneAction)`.
- This task is a **behaviour-preserving refactor** for card reorder/move (no visible change yet); it wires the plumbing the next tasks use.

- [ ] **Step 1: Re-add `PaneId::card()` and add `PaneAction` to `panes.rs`**

Add `use crate::card_layout::CardCol;` alongside the existing `use crate::card_layout::CardId;` (combine: `use crate::card_layout::{CardCol, CardId};`).

Add to `impl PaneId` (mirror of `from_card`):

```rust
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
```

Add the action enum (below the `PaneId` impl):

```rust
/// A pending arrange-mode mutation, applied once after the frame renders (so the
/// column/tray lists are never mutated mid-iteration).
#[derive(Debug, Clone, Copy)]
pub(crate) enum PaneAction {
    /// Place a card into `col` at absolute index `idx` (and unhide it).
    PlaceCard { card: CardId, col: CardCol, idx: usize },
    /// Show a pane in its home (remove from the hidden set).
    Show(PaneId),
    /// Hide a pane (add to the hidden set; a card keeps its column slot).
    Hide(PaneId),
}
```

Add a test:

```rust
    #[test]
    fn card_round_trips_and_anchors_have_no_card() {
        for c in CardId::ALL {
            assert_eq!(PaneId::from_card(c).card(), Some(c));
        }
        assert_eq!(PaneId::Bands.card(), None);
        assert_eq!(PaneId::ReferenceBar.card(), None);
    }
```

- [ ] **Step 2: Run panes tests**

Run: `cargo test -p resonance-gui panes::`
Expected: PASS (8 tests).

- [ ] **Step 3: Swap the field + add `apply_pane_action` in `app.rs`**

Import `PaneAction`:

```rust
use crate::panes::{hidden_from_json_or_default, BandsOffLayout, PaneAction, PaneId};
```

Replace the field (line ~359):

```rust
    /// A pending arrange-mode action, applied after the arrange frame renders.
    pub(crate) pending_pane_action: Option<PaneAction>,
```

Replace its initialiser in `new()` (line ~727): `pending_card_move: None,` → `pending_pane_action: None,`.

Add the apply method next to `pane_visible` (line ~1484):

```rust
    /// Apply one arrange-mode action to the layout / hidden set.
    pub(crate) fn apply_pane_action(&mut self, action: PaneAction) {
        match action {
            PaneAction::PlaceCard { card, col, idx } => {
                self.hidden_panes.remove(&PaneId::from_card(card));
                self.layout.move_card(card, col, idx);
            }
            PaneAction::Show(pane) => {
                self.hidden_panes.remove(&pane);
            }
            PaneAction::Hide(pane) => {
                self.hidden_panes.insert(pane);
            }
        }
    }
```

- [ ] **Step 4: Update payloads + apply site in `layout.rs`**

The dragging-detection type (in `render_lower_column`):

```rust
            let dragging = egui::DragAndDrop::has_payload_of_type::<PaneId>(ui.ctx());
```

`card_tile` — payload becomes the pane:

```rust
    fn card_tile(&mut self, ui: &mut egui::Ui, id: CardId) {
        ui.dnd_drag_source(egui::Id::new(("card_tile", id)), PaneId::from_card(id), |ui| {
            // (unchanged frame/grip/label body)
        });
    }
```

`drop_gap` — payload becomes `PaneId`; place only if it's a card:

```rust
    fn drop_gap(&mut self, ui: &mut egui::Ui, col: CardCol, idx: usize) {
        let frame = egui::Frame::default().inner_margin(egui::Margin::symmetric(0, 3));
        let (_, payload) = ui.dnd_drop_zone::<PaneId, _>(frame, |ui| {
            ui.allocate_exact_size(
                egui::vec2(ui.available_width().max(1.0), 8.0),
                egui::Sense::hover(),
            );
        });
        if let Some(p) = payload {
            if let Some(card) = p.card() {
                self.pending_pane_action = Some(crate::panes::PaneAction::PlaceCard { card, col, idx });
            }
        }
    }
```

In `lower_columns_arrange`, replace the apply block:

```rust
        // Apply a pending arrange action after both columns render.
        if let Some(action) = self.pending_pane_action.take() {
            self.apply_pane_action(action);
        }
```

- [ ] **Step 5: Build + gate + regression screenshot**

Run: `make check`
Expected: EXIT 0.

Screenshot arrange mode with a card moved is hard to script (needs a drag), so just verify arrange mode still renders and card tiles still drag by launching interactively is skipped; instead confirm no regression in the static arrange view:
```bash
cargo build -p resonance-gui
xdg="$(mktemp -d)"; mkdir -p "$xdg/data/resonance" "$xdg/config"
printf '{\n    "theme": "Dark"\n}\n' > "$xdg/data/resonance/app.ron"
d=175; Xvfb ":$d" -screen 0 1240x760x24 -nolisten tcp >/dev/null 2>&1 & xv=$!; sleep 0.6
env -u WAYLAND_DISPLAY XDG_DATA_HOME="$xdg/data" XDG_CONFIG_HOME="$xdg/config" DISPLAY=":$d" \
  WINIT_UNIX_BACKEND=x11 LIBGL_ALWAYS_SOFTWARE=1 RESONANCE_DEMO=1 RESONANCE_EDIT_LAYOUT=1 \
  RESONANCE_WINDOW_SIZE=1240x760 RUST_LOG=error target/debug/resonance-gui >/dev/null 2>&1 & ap=$!; sleep 4
DISPLAY=":$d" import -window root "$OUT/arrange_refactor.png" && echo wrote
kill "$ap" 2>/dev/null||true; kill "$xv" 2>/dev/null||true; rm -rf "$xdg"
```
Expected: arrange banner + Effects tile (left), right-column tiles, EQ bands center — same as before the refactor.

- [ ] **Step 6: Commit**

```bash
git add crates/resonance-gui/src/panes.rs crates/resonance-gui/src/app.rs crates/resonance-gui/src/ui/layout.rs
git commit -m "refactor(gui): paneid drag payload + paneaction plumbing for arrange mode"
```

---

### Task 5: Hidden tray + card hide/show (buttons + drag)

**Files:**
- Modify: `crates/resonance-gui/src/ui/layout.rs` (`lower_columns_arrange`, `card_tile`, new `hidden_tray` + `tray_tile`)

**Interfaces:**
- Consumes: `PaneAction`, `apply_pane_action`, `PaneId`, `hidden_panes`, `visible_cards`.

- [ ] **Step 1: Give card tiles an × (hide) button**

Rework `card_tile` so the grip+label is the drag source and a trailing × queues a Hide. Replace the body:

```rust
    #[allow(clippy::unused_self)]
    fn card_tile(&mut self, ui: &mut egui::Ui, id: CardId) {
        let t = kit::tokens(ui);
        egui::Frame::default()
            .fill(ui.visuals().faint_bg_color)
            .stroke(egui::Stroke::new(1.0, t.line))
            .corner_radius(egui::CornerRadius::same(kit::R_CARD as u8))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_width(ui.available_width());
                    // Grip + label = the drag source.
                    ui.dnd_drag_source(egui::Id::new(("card_tile", id)), PaneId::from_card(id), |ui| {
                        let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                        crate::ui::icons::draw(ui.painter(), crate::ui::icons::Icon::Grip, r, t.dim);
                        ui.add_space(6.0);
                        ui.label(id.title());
                    });
                    // Trailing × removes the pane (to the Hidden tray).
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").on_hover_text("Hide this pane").clicked() {
                            self.pending_pane_action = Some(crate::panes::PaneAction::Hide(PaneId::from_card(id)));
                        }
                    });
                });
            });
    }
```

(`id.title()` is `CardId::title`.)

- [ ] **Step 2: Add `tray_tile` and `hidden_tray`**

Add two helpers to the `impl GuiApp` in layout.rs:

```rust
    /// A tile in the Hidden tray: grip+label is a drag source (drop into a
    /// column/center to restore), and a trailing ＋ restores to the pane's home.
    fn tray_tile(&mut self, ui: &mut egui::Ui, pane: PaneId) {
        let t = kit::tokens(ui);
        egui::Frame::default()
            .fill(ui.visuals().faint_bg_color)
            .stroke(egui::Stroke::new(1.0, t.line))
            .corner_radius(egui::CornerRadius::same(kit::R_CARD as u8))
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.dnd_drag_source(egui::Id::new(("tray_tile", pane)), pane, |ui| {
                        let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                        crate::ui::icons::draw(ui.painter(), crate::ui::icons::Icon::Grip, r, t.dim);
                        ui.add_space(6.0);
                        ui.label(pane.title());
                    });
                    if ui.small_button("＋").on_hover_text("Show this pane").clicked() {
                        self.pending_pane_action = Some(crate::panes::PaneAction::Show(pane));
                    }
                });
            });
    }

    /// The Hidden tray: a labelled strip listing every hidden pane, and itself a
    /// drop zone (drop a pane here to hide it).
    fn hidden_tray(&mut self, ui: &mut egui::Ui) {
        let hidden: Vec<PaneId> = PaneId::ALL
            .iter()
            .copied()
            .filter(|p| self.hidden_panes.contains(p))
            .collect();
        let frame = egui::Frame::default().inner_margin(egui::Margin::symmetric(8, 6));
        let (_, payload) = ui.dnd_drop_zone::<PaneId, _>(frame, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("Hidden:").strong());
                if hidden.is_empty() {
                    ui.weak("nothing hidden — drag a pane here to remove it");
                }
                for pane in hidden {
                    self.tray_tile(ui, pane);
                }
            });
        });
        if let Some(p) = payload {
            self.pending_pane_action = Some(crate::panes::PaneAction::Hide(*p));
        }
    }
```

`PaneId::title` already exists.

- [ ] **Step 3: Mount the tray in `lower_columns_arrange`**

In `lower_columns_arrange`, add a bottom panel for the tray, before the side panels (so egui lays it at the bottom of the controls strip, under the columns). Right after the `layout_edit_banner` top panel:

```rust
        egui::Panel::bottom("hidden_tray")
            .frame(egui::Frame::NONE)
            .show_separator_line(true)
            .show_inside(ui, |ui| self.hidden_tray(ui));
```

(The existing `pending_pane_action.take()` apply block at the end of `lower_columns_arrange` now also applies tray hides/shows.)

- [ ] **Step 4: Build + gate**

Run: `make check`
Expected: EXIT 0. (If egui lacks `small_button`, use `ui.button("×")`; both exist in egui 0.29+. Confirm the crate's egui version supports `small_button` — it does.)

- [ ] **Step 5: Screenshot-verify tray + hidden card**

```bash
cargo build -p resonance-gui
bash .../shot_one.sh arrange_tray '    "hidden_panes": "[\"Outputs\"]"'
```
But `shot_one.sh` doesn't set `RESONANCE_EDIT_LAYOUT`; make a variant or reuse the inline block from Task 4 Step 5 with `hidden_panes` seeded to `["Outputs"]`. Expected: arrange mode shows the Hidden tray at the bottom with an "Outputs" tile carrying ＋; the right column no longer shows Outputs; each visible tile shows a trailing ×. View with Read.

- [ ] **Step 6: Commit**

```bash
git add crates/resonance-gui/src/ui/layout.rs
git commit -m "feat(gui): hidden tray + hide/show cards in arrange mode (× / ＋ + drag)"
```

---

### Task 6: Bands + Reference bar in arrange mode

**Files:**
- Modify: `crates/resonance-gui/src/ui/layout.rs` (`lower_columns_arrange` center: bands tile / drop zone)
- Modify: `crates/resonance-gui/src/ui/curve_view.rs` (`hero`: reference row in arrange mode)

**Interfaces:**
- Consumes: `PaneAction`, `apply_pane_action`, `pane_visible`, `PaneId`.

- [ ] **Step 1: Bands as a compact tile / drop zone in the arrange center**

In `lower_columns_arrange`, replace the center `CentralPanel` body (currently the bands table or the "EQ bands hidden — show it in Settings → Panes." hint) with a compact tile when shown and a drop zone when hidden:

```rust
        egui::CentralPanel::default()
            .frame(bands_card_frame(ui))
            .show_inside(ui, |ui| {
                if self.pane_visible(PaneId::Bands) {
                    // Compact draggable "EQ bands" tile with an × to remove.
                    ui.horizontal(|ui| {
                        ui.set_width(ui.available_width());
                        ui.dnd_drag_source(egui::Id::new("bands_tile"), PaneId::Bands, |ui| {
                            let t = kit::tokens(ui);
                            let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                            crate::ui::icons::draw(ui.painter(), crate::ui::icons::Icon::Grip, r, t.dim);
                            ui.add_space(6.0);
                            ui.label(PaneId::Bands.title());
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("×").on_hover_text("Hide EQ bands").clicked() {
                                self.pending_pane_action = Some(crate::panes::PaneAction::Hide(PaneId::Bands));
                            }
                        });
                    });
                } else {
                    // Drop zone to restore the bands center.
                    let (_, payload) = ui.dnd_drop_zone::<PaneId, _>(egui::Frame::NONE, |ui| {
                        ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), ui.available_height().max(40.0)),
                            egui::Sense::hover(),
                        );
                        ui.weak("drop EQ bands here to show it");
                    });
                    if let Some(p) = payload {
                        if *p == PaneId::Bands {
                            self.pending_pane_action = Some(crate::panes::PaneAction::Show(PaneId::Bands));
                        }
                    }
                }
            });
```

- [ ] **Step 2: Reference row in the hero during arrange mode**

In `crates/resonance-gui/src/ui/curve_view.rs`, `hero()`, replace the reference-bar bottom panel so arrange mode shows a compact "Reference bar" tile / drop zone instead of the live bar. Change the existing:

```rust
        if self.pane_visible(PaneId::ReferenceBar) {
            egui::Panel::bottom("hero_refbar") …  // live reference bar
        }
```

to:

```rust
        if self.layout_edit {
            // Arrange mode: a reference "row" — a compact tile (drag/×) when
            // shown, or a drop zone when hidden — instead of the live bar.
            egui::Panel::bottom("hero_refbar")
                .frame(egui::Frame::NONE)
                .show_separator_line(true)
                .show_inside(ui, |ui| {
                    if self.pane_visible(PaneId::ReferenceBar) {
                        ui.horizontal(|ui| {
                            ui.dnd_drag_source(egui::Id::new("ref_tile"), PaneId::ReferenceBar, |ui| {
                                let t = kit::tokens(ui);
                                let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                                crate::ui::icons::draw(ui.painter(), crate::ui::icons::Icon::Grip, r, t.dim);
                                ui.add_space(6.0);
                                ui.label(PaneId::ReferenceBar.title());
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("×").on_hover_text("Hide reference bar").clicked() {
                                    self.pending_pane_action = Some(crate::panes::PaneAction::Hide(PaneId::ReferenceBar));
                                }
                            });
                        });
                    } else {
                        let (_, payload) = ui.dnd_drop_zone::<PaneId, _>(egui::Frame::NONE, |ui| {
                            ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::hover());
                            ui.weak("drop Reference bar here to show it");
                        });
                        if let Some(p) = payload {
                            if *p == PaneId::ReferenceBar {
                                self.pending_pane_action = Some(crate::panes::PaneAction::Show(PaneId::ReferenceBar));
                            }
                        }
                    }
                });
        } else if self.pane_visible(PaneId::ReferenceBar) {
            egui::Panel::bottom("hero_refbar")
                .frame(egui::Frame::NONE)
                .show_separator_line(false)
                .show_inside(ui, |ui| {
                    // (the existing live reference-bar body — hline + inset frame + self.reference_bar(ui))
                });
        }
```

Keep the existing live-bar body (the `hline` + inset `Frame` + `self.reference_bar(ui)`) verbatim inside the `else if` arm.

Note: `hero()` runs inside a `&mut self` method, so `self.pending_pane_action` is assignable here. The action is applied in `lower_columns_arrange` (same frame, controls panel renders after the central hero in the wide branch — verify ordering; if the hero renders after the controls panel, move the apply to the end of `shell`'s wide branch instead). **Ordering check:** in `shell` wide, the bottom `controls_panel` is created before the `CentralPanel` hero, but egui runs `show_inside` closures in call order, so `lower_columns_arrange` (and its `.take()` apply) runs before `hero`. A ref-bar action set in `hero` would then apply next frame — acceptable (one-frame latency), but to apply same-frame, move the `pending_pane_action.take()` apply to the very end of `shell`'s wide branch, after both panels. **Do that:** remove the apply from `lower_columns_arrange` and add it at the end of the wide branch in `shell`:

```rust
                egui::CentralPanel::default()
                    .frame(hero_frame)
                    .show_inside(ui, |ui| {
                        if let Some(s) = &state {
                            self.hero(ui, s);
                        }
                    });
                if let Some(action) = self.pending_pane_action.take() {
                    self.apply_pane_action(action);
                }
```

- [ ] **Step 3: Ensure `PaneId` + `PaneAction` reachable in `curve_view.rs`**

`curve_view.rs` already `use crate::panes::PaneId;`. No extra import needed (PaneAction is referenced as `crate::panes::PaneAction`).

- [ ] **Step 4: Build + gate**

Run: `make check`
Expected: EXIT 0.

- [ ] **Step 5: Screenshot-verify bands + reference in arrange**

Using the Task-4 inline block (with `RESONANCE_EDIT_LAYOUT=1`) and different seeds:
- seed `hidden_panes: []` → arrange: center shows a compact "EQ bands" tile with ×; under the graph a "Reference bar" tile with ×; tray empty.
- seed `hidden_panes: ["Bands","ReferenceBar"]` → arrange: center shows "drop EQ bands here"; under the graph "drop Reference bar here"; tray shows EQ bands + Reference bar tiles with ＋.

Capture both and view with Read.

- [ ] **Step 6: Full-matrix screenshot sweep + gate**

Re-verify the live matrix isn't regressed: bands shown both-sides (3-col), one-side (2-col), no cards (1); bands hidden columns/stack; all hidden (graph fills). Capture a representative few and view.

Run: `make check` (final).

- [ ] **Step 7: Commit**

```bash
git add crates/resonance-gui/src/ui/layout.rs crates/resonance-gui/src/ui/curve_view.rs
git commit -m "feat(gui): add/remove eq bands + reference bar in arrange mode via tray"
```

---

## Self-Review

**Spec coverage:**
- Dual/stack when bands hidden + preference → Task 1 (pref) + Task 2 (live + banner control). ✅
- Live column count 3/2/1 (bands shown) → already in `lower_columns_live`; unchanged, re-verified Task 6 Step 6. ✅
- Add/remove in arrange via Hidden tray → Task 5. ✅
- EQ bands as removable flexible center → Task 6 Step 1. ✅
- Reference bar removable pane → Task 6 Step 2. ✅
- Drag-primary + × / ＋ shortcuts → Task 4 (drag payloads), Task 5 (× on cards, ＋ in tray, tray drop), Task 6 (bands/ref drag + × + drop zones). ✅
- Advanced items stay Settings-only → untouched; not added to tray (`PaneId::ALL` has no dither/convolution). ✅
- Remove Settings → Panes; reorder Theme→Channels→Advanced→EQ phase → Task 3. ✅
- Reuse `hidden_panes` + `CardLayout`; no new persisted state except `bands_off_layout` → Tasks 1, 4. ✅
- Reset unhides all + resets order + resets pref → Task 2 Step 3 banner Reset. ✅

**Placeholder scan:** No TBD/TODO. Every code step shows full code. The two "keep the existing body verbatim" notes (Task 6 Step 2 live-bar arm) reference specific existing lines, not vague instructions.

**Type consistency:** `PaneId::from_card` / `PaneId::card` / `PaneId::title` / `PaneId::ALL`; `PaneAction::{PlaceCard{card,col,idx},Show,Hide}`; `GuiApp.pending_pane_action`; `GuiApp::apply_pane_action`; `BandsOffLayout::{Columns,Stacked}` / `from_storage` / `as_storage`; `GuiApp.bands_off_layout`; `visible_cards` / `lower_has_content` / `bands_card_frame` — used identically across tasks. `CardId::ALL` / `CardId::title` exist. `apply_pane_action` is applied once per frame (moved to end of `shell` wide branch in Task 6 so a ref-bar action set in `hero` applies same-frame).
