# Reorderable control cards (GUI)

**Date:** 2026-07-01
**Client:** resonance-gui
**Status:** design approved, ready for implementation plan

## Problem

The GUI's control cards sit in fixed columns. Users want to reorganize them —
tiling-window-manager style (cards snap into slots, nothing floats) — so they can
put the cards they use most where they want them.

## Scope & model

The 5 control cards become draggable:

- **Effects**
- **Applications**
- **Outputs**
- **Device→Profile** (device mapping)
- **Profiles**

The **graph** (hero, top) and the **EQ Bands** card (elastic center column) are fixed
anchors and do **not** move.

Each card belongs to a **column** (`Left` or `Right`) and has an **order** within it.

Data model (persisted):

```rust
enum CardId { Effects, Applications, Outputs, DeviceMap, Profiles }
enum CardCol { Left, Right }
struct CardLayout { left: Vec<CardId>, right: Vec<CardId> }
```

Default (today's arrangement): `left: [Effects]`,
`right: [Applications, Outputs, DeviceMap, Profiles]`.

All 5 ids always live in the layout. Cards that are conditionally absent
(Applications/Outputs only render when the daemon reports app streams / output sinks)
are skipped at render time but keep their slot in the list.

## Edit mode

Rearranging happens in an explicit **edit mode** (no grip clutter in normal use):

- Session-only flag `layout_edit: bool` on `GuiApp`, default `false`, **not persisted**
  (the app never launches in edit mode).
- Toggled from the overflow (☰) menu — a checkable **"Edit layout"** item in the
  existing "View" section, next to "Reset layout".
- While `layout_edit` is on:
  - Each movable card header shows a **grip handle** (a 6-dot drag icon) and becomes a
    drag source; the columns show **drop zones** (insertion lines).
  - A thin **banner** at the top of the controls strip reads *"Arranging layout — drag
    cards to rearrange"* with a **"Done"** button that exits edit mode (so the exit is
    always discoverable, not only via the menu).
  - Movable cards get a subtle affordance (e.g. a faint dashed outline) so it's obvious
    what can be dragged.
- While off: normal rendering — no grips, no drop zones, headers just toggle collapse.

## Interaction (tiling, not floating)

- The grip handle is a **drag source** carrying `CardId` (egui's `DragAndDrop<CardId>`
  payload).
- Each column exposes **drop zones** at every gap — above the first card, between cards,
  below the last card, and the whole area of an empty column. The hovered gap shows an
  **insertion line**; an empty column shows a faint "drop a card here" hint.
- On release, the card is removed from its current `(column, index)` and inserted at the
  target `(column, index)`; the columns re-tile. Nothing floats.
- Clicking a card header still toggles its collapse (the grip is a separate drag source,
  so click-to-collapse and drag-to-move never conflict).

## Rendering

- `lower_columns` renders the left column by iterating `layout.left` →
  `render_card(id, ui, state)` (a dispatch that calls the existing
  `effects_section` / `apps_section` / `sinks_section` / `device_mapping_section` /
  `profiles_panel`), the elastic center = EQ Bands (unchanged), and the right column by
  iterating `layout.right`.
- Column widths stay fixed (`EFFECTS_W` 320 / `DEVICES_W` 384). Column reordering does
  not resize columns (out of scope).
- An empty column still renders (as a drop target in edit mode; nothing in normal mode).
- **Narrow layout:** the accordion renders `left` cards, then EQ Bands, then `right`
  cards — so it reflects the arrangement — but drag-reordering is **wide-only** (edit
  mode has no effect in narrow; the grips/drop zones only appear in the wide columns).

## Implementation approach

**Manual egui drag-and-drop** using egui's built-in `DragAndDrop<CardId>` payload API +
per-gap drop zones. **No new dependency**; full control over the grip + insertion-line
look to match the bespoke `kit` styling. (Considered and rejected: `egui_dnd` — a new
dep and awkward across two separate column lists; `egui_tiles` — full tiling, out of
scope per the movement-model decision.)

A small **grip icon** (6 dots) is added to `ui/icons.rs` (`Icon::Grip`) if no suitable
existing glyph fits.

## Persistence & reset

- Persist `CardLayout` via egui storage as JSON (same mechanism as `theme` / `reference`
  — `storage.set_string("card_layout", serde_json::to_string(&layout))`). Load in `new()`
  (default on missing/unparseable), save in `save()`.
- The `layout_edit` flag is **not** persisted.
- Extend the existing overflow-menu **"Reset layout"** action to also restore the default
  `CardLayout` (alongside clearing the panel-size splitters it already resets).

## Testing

Pure, egui-free functions carry the logic and are unit-tested:

- `CardLayout::default()` returns the documented default arrangement.
- `CardLayout::move_card(&mut self, id: CardId, to_col: CardCol, to_idx: usize)` removes
  the id from wherever it currently is and inserts it at `(to_col, to_idx)`. Tested for:
  same-column reorder (up and down), cross-column move, moving the last card out of a
  column (leaving it empty), and clamping `to_idx` past the end.
- serde round-trip of `CardLayout`.
- `CardLayout` always contains exactly the 5 ids after any `move_card` (no loss/dup).

The drag rendering itself isn't unit-testable; verify via the demo/Xvfb screenshot
harness (`contrib/dev/uishot.sh` with `RESONANCE_DEMO=1`): default arrangement, edit-mode
banner + grips visible, and a rearranged layout.

`make check` (fmt + clippy `-D warnings` + `test --all`) must pass before every commit.

## Out of scope

- Resizing column widths by drag (columns stay fixed 320 / 384).
- Moving the graph or the EQ Bands card (fixed anchors).
- Full tiling / splits / tabs (egui_tiles).
- Drag-reordering in the narrow accordion (narrow only reflects the wide arrangement).
- Per-profile or per-device layouts (one global arrangement).
