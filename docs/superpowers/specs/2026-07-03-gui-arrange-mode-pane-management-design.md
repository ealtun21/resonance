# Arrange-mode pane add/remove + dual-column layout

**Date:** 2026-07-03
**Crate:** `resonance-gui`
**Status:** approved (all decisions confirmed with the requester; see "Decisions")

## Motivation

Two problems with the just-shipped hide-panes feature, plus a redesign request:

1. **No dual side-by-side when EQ bands is hidden.** With EQ bands hidden, the
   remaining control cards collapse into a single full-width stack instead of
   sitting side-by-side in two columns.
2. **Hiding lives in Settings, arranging lives in edit mode — two disjoint
   places.** The user wants add/remove (show/hide) to be part of edit mode, in a
   clean, understandable way, with the ability to add panes back.
3. Advanced items must keep working but stay gated in Settings.

## Goals

- Live: when EQ bands is hidden, the two card columns sit **side-by-side filling
  the width** (dual), not one full-width stack.
- Arrange (edit-layout) mode gains **add/remove** via a **Hidden tray**: move a
  pane into the tray to remove it, move it back out to add it. Reversible and
  discoverable.
- **EQ bands stays the flexible center** when shown; removing it to the tray
  collapses the lower area to dual columns.
- The **Reference bar** is a removable pane too.
- **Advanced items** (Output/dither section, Convolution section, and the
  Slope/Scope/Dyn bands-table columns) remain controlled **only** by
  Settings → Advanced features. They are not listed in the tray.
- **Remove the Settings → Panes checkboxes** — arrange mode is the single place
  to add/remove the main panes.

## Non-goals

- No change to the DSP chain, IPC, daemon, or other clients.
- No change to Settings → Advanced features (its five toggles stay as-is).
- No change to the graph/spectrum plot itself.

## Panes

Two kinds, both already modeled by `PaneId`:

- **Column panes** (the five `CardId` cards): Effects, Applications, Outputs,
  Device → Profile, Profiles. They live in the Left or Right column.
- **Fixed-home panes**: `PaneId::Bands` (home = flexible center) and
  `PaneId::ReferenceBar` (home = a strip under the graph). They have no column.

## Data model (reuse existing — no new persisted state)

- `GuiApp.hidden_panes: HashSet<PaneId>` already records which panes are hidden.
- `CardLayout { left, right }` already records card positions. A **hidden card
  stays in its column list** so its slot is remembered; `hidden_panes` marks it
  hidden. This is the key simplification: nothing new to persist.
  - Remove a card → insert into `hidden_panes` (its `CardLayout` slot is kept).
  - Add a card back → remove from `hidden_panes` → it re-renders at its slot.
  - Add a card back **at a chosen position** (dropped into a column) → remove
    from `hidden_panes` **and** `move_card` it to the drop index.
- Bands / Reference bar: `hidden_panes` membership only (no position).
- **New persisted preference** `bands_off_layout: BandsOffLayout` — an enum
  `{ Columns, Stacked }`, default `Columns`, saved as a string storage key. Only
  affects the bands-hidden live layout (see below).

## Interaction — arrange mode

Drag-primary (matches the selected mock) with × / ＋ button shortcuts.

Drop zones in arrange mode, payload = `PaneId`, each validating pane kind:

| Zone | Accepts |
|------|---------|
| Left column | any column pane |
| Right column | any column pane |
| Center | `Bands` only |
| Reference row (edit-mode strip under the graph) | `ReferenceBar` only |
| Hidden tray | any pane |

- Drop a pane on the **Hidden tray** → hide it (add to `hidden_panes`; a card
  keeps its `CardLayout` slot).
- Drop a card into a **column** gap → show it there (remove from `hidden_panes`
  + `move_card` to that absolute index).
- Drop `Bands` on the **Center** / `ReferenceBar` on the **Reference row** →
  show it in its home (remove from `hidden_panes`).
- Reordering / moving cards **between** columns: the existing column drop-gaps,
  re-keyed from `CardId` to `PaneId`.
- **Shortcut buttons:** each placed tile gets a small **×** (→ hide); each tray
  tile gets **＋** (→ show, at its remembered home/slot). Cheap, and makes the
  feature discoverable without knowing to drag.

Payloads change from `CardId` to `PaneId` so bands and the reference bar can be
dragged. Drop zones reject a payload of the wrong kind (a card dropped on the
center is a no-op).

## Live layout (non-edit)

`lower_has_content()` unchanged (any card visible OR bands visible). Column count
is driven by EQ-bands visibility and which card columns hold visible cards:

**EQ bands shown** (already how `lower_columns_live` behaves — side panels drop
when their column has no visible card, and the bands center fills the slack):

| Left has cards | Right has cards | Columns |
|:---:|:---:|:---:|
| yes | yes | **3** — Left \| bands \| Right |
| yes | no  | **2** — Left \| bands |
| no  | yes | **2** — bands \| Right |
| no  | no  | **1** — bands fills the width |

**EQ bands hidden** — governed by a persisted preference `bands_off_layout`:

- **Columns** (default): visible cards render in their Left/Right columns
  side-by-side. Both sides populated → two equal columns (50/50, egui
  `ui.columns(2)`); only one side populated → it fills the width.
- **Stacked**: all visible cards stack in a single full-width column (the prior
  behaviour).
- No visible cards **and** bands hidden → `lower_has_content()` is false → the
  graph fills the window (existing full-graph fallback).

The preference is a small segmented control in the arrange banner ("When EQ bands
is hidden: Columns | Stack"), persisted like the other GUI prefs. It is
configured in arrange mode and applies to the live view.

## Arrange (edit) layout

- **Banner** (existing) — Done + Reset, plus a one-line hint: "Drag panes into
  Hidden to remove them; drag back (or ＋) to add." Reset unhides all + resets
  card order (existing).
- **Reference row** (edit-mode only, a strip directly under the graph in both
  wide and narrow): the live reference bar with a grip + × when shown, or a
  "drop Reference bar here" placeholder drop zone when hidden. Keeps the graph
  plot itself untouched. Live mode is unchanged (reference bar stays inside the
  hero card / its narrow panel).
- **Lower strip**: Left column zone | Center | Right column zone.
  - Columns show the existing compact draggable card tiles (visible cards only),
    each with an × shortcut; empty columns show "(empty — drag a card here)".
  - Center shows a compact "EQ bands" tile (grip + × ) when shown, or a "drop EQ
    bands here" placeholder drop zone when hidden.
- **Hidden tray**: a labelled area (e.g. below the columns) listing a tile per
  hidden pane, each a drag source and carrying ＋ to restore. Always shown in
  arrange mode (reads "nothing hidden" when empty) so the mechanism is teachable.

## Settings changes

- **Remove** the Settings → Panes section (the seven visibility checkboxes and
  "Show all panes") added previously.
- Settings order becomes: **Theme → Channels → Advanced features → EQ phase**
  (Panes removed).
- Settings → Advanced features unchanged.

## Files touched (anticipated)

- `crates/resonance-gui/src/card_layout.rs` — DnD payload becomes `PaneId`
  (or a small `DragPane` payload); helper to place/reorder.
- `crates/resonance-gui/src/panes.rs` — small helpers (pane kind, column vs
  fixed-home); already has `PaneId`/`from_card`.
- `crates/resonance-gui/src/ui/layout.rs` — dual-column live layout; arrange-mode
  tray + drop zones + × / ＋ ; center bands tile/placeholder; reference row.
- `crates/resonance-gui/src/ui/curve_view.rs` — hero: no reference bar in the
  hero during arrange mode (it moves to the reference row); unchanged live.
- `crates/resonance-gui/src/ui/dialogs.rs` — remove the Settings → Panes section.
- `crates/resonance-gui/src/app.rs` — drop the "Show all panes" plumbing if
  unused after the Settings removal; keep `hidden_panes` + `pane_visible` +
  reset-clears-hidden; add the `bands_off_layout` preference (field + load/save +
  reset default) and the arrange-banner segmented control that sets it.

## Testing

- Pure-logic unit tests in `card_layout` / `panes` for the place/hide/show
  transitions (e.g. hiding a card keeps its slot; showing at a drop index moves
  it; fixed-home panes toggle without a column).
- No DSP/audio/IPC surface → no `resonance verify`.
- `make check` gate; headless screenshot verification (isolated `XDG_DATA_HOME`,
  `RESONANCE_DEMO=1`, `RESONANCE_EDIT_LAYOUT=1`, seeded `hidden_panes`) for:
  live dual columns (bands hidden), arrange tray add/remove, center placeholder,
  reference row, and the all-hidden → full-graph fallback.

## Decisions (confirmed with the requester)

1. **Remove Settings → Panes** — arrange mode is the single place to add/remove
   main panes. ✅
2. **Advanced items stay Settings-only** — not listed in the tray. ✅
3. **Drag-primary + × / ＋ button shortcuts.** ✅
4. **Center during arrange = compact "EQ bands" tile** (not the full table). ✅
5. **Live column count** per the table above: bands shown → 3 / 2 / 1 by how many
   card columns are populated; bands hidden → `bands_off_layout` preference
   (Columns default, or Stacked). ✅
