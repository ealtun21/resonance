# GUI pane visibility ("Hide panes") + Settings reorder

**Date:** 2026-07-03
**Crate:** `resonance-gui`
**Status:** approved design, pre-implementation

## Goal

Let the user hide individual panels from the GUI to declutter the view. Unlike
the existing "Edit layout" arrange mode (which only rearranges cards and, in
edit mode, shows placeholder tiles), hiding a pane makes it stop rendering
entirely. When every hideable pane is hidden, the FR graph + spectrum becomes
the full UI. This is the same "opt-in visibility" idea as the existing advanced-
feature toggles, but applied to whole panels.

Also reorder the Settings dialog so Theme comes first, followed by the new
Panes section, then Channels, Advanced features, and EQ phase.

## Scope of hideable panes

Seven panes, each independently toggleable:

- The five movable control cards: Effects, Applications, Outputs,
  Device → Profile, Profiles.
- The EQ bands pane.
- The Reference bar (the target/measurement strip that lives inside the graph).

The FR graph + live spectrum (the "hero") is **not** hideable — it is the
element that fills the window when everything else is hidden.

## Non-goals

- No change to the DSP chain, IPC protocol, daemon, or any other client.
- No change to the "Edit layout" arrange mode's behaviour (it continues to show
  the 3-column arranger with all card tiles regardless of visibility).
- Hidden panes are purely visual; they do not stop any DSP from running, so
  they are **not** surfaced in the "adv:" status-bar hint.

## Data model & persistence

New pure-logic module `crates/resonance-gui/src/panes.rs` (mirrors
`card_layout.rs` — no egui, unit-tested):

```rust
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
    pub(crate) const ALL: [PaneId; 7] = [ /* in Settings display order */ ];
    pub(crate) fn title(self) -> &'static str { /* "Effects", …, "EQ bands", "Reference bar" */ }
    /// The matching movable card, for the five card panes; None for Bands/ReferenceBar.
    pub(crate) fn card(self) -> Option<CardId> { /* Effects→Effects, … */ }
}
```

Visibility lives on `GuiApp` as a set of the *hidden* panes (default empty ⇒
everything visible — a fresh install shows the full UI):

```rust
/// Panes the user has hidden via Settings → Panes (persisted). Empty ⇒ all shown.
pub(crate) hidden_panes: std::collections::HashSet<PaneId>,
```

Helper on `GuiApp`:

```rust
pub(crate) fn pane_visible(&self, pane: PaneId) -> bool {
    !self.hidden_panes.contains(&pane)
}
```

Persistence follows the existing string-key pattern in `app.rs`:

- **Load** in `GuiApp::new()`: read storage key `hidden_panes` (a JSON array),
  parse with a `from_json_or_default`-style helper that returns an empty set on
  any parse error or unknown variant (so corrupt/version-skewed storage falls
  back to all-visible, never panics).
- **Save** in `GuiApp::save()`: `storage.set_string("hidden_panes", serde_json::to_string(&self.hidden_panes))`.

## Settings dialog

`dialogs.rs::settings_dialog` is reordered and gains a Panes section. New
top-to-bottom order (each section separated by the existing `ui.separator()`):

1. **Theme** — the existing theme picker (moved to the top).
2. **Panes** *(new)*.
3. **Channels** — unchanged content.
4. **Advanced features** — the existing five `show_*` checkboxes.
5. **EQ phase** — unchanged content.

Panes section:

- Header "Panes" + hint: "Show or hide panels to declutter the view. Hidden
  panels stop rendering entirely; the graph fills whatever's left."
- One checkbox per `PaneId` in `PaneId::ALL` order. Checkbox **sense is
  "visible"** (checked = shown). Toggling a box off inserts the pane into
  `hidden_panes`; on removes it. Label = `PaneId::title()`.
- A "Show all panes" button that clears `hidden_panes`.

## Rendering changes

### `render_card` (layout.rs)

At the top of `render_card`, return `false` when the card's pane is hidden
(`!self.pane_visible(pane_for(id))`). Returning `false` already makes both the
wide columns and the narrow accordion skip the card's inter-card spacing (same
path Applications/Outputs use when empty).

### Wide layout — `shell` + `lower_columns` (layout.rs)

- Compute `lower_has_content` = any of the five cards visible **or** `Bands`
  visible. In `shell`'s wide branch, if `!lower_has_content`, skip the
  `controls_panel` bottom panel entirely so the hero `CentralPanel` fills the
  whole area (full-graph UI).
- In `lower_columns`, **non-edit mode**:
  - Render the left side-panel only if the left column has ≥1 visible card;
    render the right side-panel only if the right column has ≥1 visible card.
  - The flexible center `CentralPanel` is the bands card when `Bands` is
    visible; when `Bands` is hidden it instead renders the remaining visible
    cards **stacked full-width** (a scrollable vertical stack via `render_card`,
    the same card widget the narrow accordion uses).
  - Consequences: all cards hidden + bands visible ⇒ bands fills full width (no
    side panels reserved); bands hidden + cards visible ⇒ stacked cards fill.
- **Edit mode is unchanged**: `layout_edit` still renders the 3-column arranger
  (both side columns with drop gaps + the live bands center), ignoring
  visibility — arranging is about placement, not hiding.

### `hero()` (curve_view.rs)

Guard the `Panel::bottom("hero_refbar")` block on `pane_visible(ReferenceBar)`.

### Narrow layout — `shell` + `accordion_stack` (layout.rs)

- Skip the `reference_bar_narrow` top panel when `ReferenceBar` is hidden.
- In `accordion_stack`, skip the `section(ui, "EQ bands", …)` block when `Bands`
  is hidden; hidden cards are already skipped via `render_card`.
- When `!lower_has_content` (all five cards + bands hidden): render the graph as
  the filling `CentralPanel` instead of a fixed-height top panel, so it fills
  the window. The reference bar, if still visible, stays attached below the
  graph (bottom panel).

## Reset / restore

"Restore to default" must also unhide every pane:

- `reset_layout()` (the overflow menu "Reset layout" action) additionally clears
  `hidden_panes`.
- The edit-layout banner "Reset" button (in `layout_edit_banner`) additionally
  clears `hidden_panes`.
- The Settings "Show all panes" button clears `hidden_panes` locally.

There is always a way back to a hidden pane (the toolbar gear → Settings →
Panes, and the overflow "Reset layout"), so hiding everything can never trap the
user.

## Testing

- Unit tests in `panes.rs`:
  - default (empty set) reports every pane visible;
  - JSON round-trip of a non-empty hidden set;
  - invalid / unknown-variant JSON falls back to the empty (all-visible) set;
  - `PaneId::card()` maps the five card panes to their `CardId` and returns
    `None` for `Bands`/`ReferenceBar`;
  - `PaneId::ALL` contains all seven variants once.
- No DSP/audio/IPC surface is touched, so no `resonance verify` run is required.
- `make check` (fmt + clippy -D warnings + test --all) is the gate.

## Files touched

- `crates/resonance-gui/src/panes.rs` — **new** module (PaneId, load/save helpers, tests).
- `crates/resonance-gui/src/main.rs` — declare `mod panes;`.
- `crates/resonance-gui/src/app.rs` — `hidden_panes` field, load in `new()`,
  save in `save()`, `pane_visible()` helper, `reset_layout()` clears it.
- `crates/resonance-gui/src/ui/dialogs.rs` — Settings reorder + Panes section.
- `crates/resonance-gui/src/ui/layout.rs` — `render_card` guard, `shell`
  full-graph fallback, `lower_columns` conditional columns + stacked fallback,
  `accordion_stack` skips, edit-banner Reset clears hidden_panes.
- `crates/resonance-gui/src/ui/curve_view.rs` — `hero()` reference-bar guard.
