# Resonance GUI Overhaul Mockup — Polish Pass — Design

**Date:** 2026-07-26
**Status:** Approved
**Scope:** mockup files only, at `/home/nyverino/resonance-mockups/resonance-overhaul/`
(outside the git repo, matching where prior GUI mockups have lived). No changes
to `resonance-gui` Rust code, `theme.rs`, or any real IPC/DSP behaviour. Those
are the next, separate effort (porting this mockup into egui).

## Background

The user produced `Resonance GUI overhaul.zip` in a separate Claude design
session: a "Nocturne" design-system mockup (`Resonance Overhaul.dc.html`)
covering five pages behind a left icon-rail (Equalize / Effects / Mixer /
Presets / Setup), plus a persistent Simple/Advanced toolbar toggle where
Simple adds a "Tune for me" (measurement auto-match) and "Tune by ear"
(paired-comparison listening test + simplified 8-slider EQ) flow. The overall
direction is already decided by the user — this spec covers two concrete
polish issues found before moving to the real egui port.

## Problem

1. Rendered at the app's real window size, the Effects, Mixer, and Presets
   pages each leave several hundred px of bare background below their card
   content — the card grids don't use the available height.
2. The mockup only exists in its single default purple "Nocturne" palette.
   The real app ships 9 themes (`theme.rs`); the user hasn't seen this layout
   under any of them and wants to before signing off on moving to a real port.

## Decisions (from brainstorming)

| Question | Decision |
|---|---|
| Empty-space philosophy | Add real content — but per-page judgement, not uniform filling |
| Effects page | Cards become click-to-expand accordions (detail on demand); page stops forcing full viewport height when nothing is expanded |
| Mixer page | Add a per-channel FR overlay (multi-line chart + eye-toggle legend) below Applications/Outputs/Channels |
| Presets page | Add a "Recent activity" panel: diff of what changed on the loaded profile since its last save |
| Theme ramp fidelity | Keep the mockup's fuller 100–900 OKLCH ramp look. `theme.rs` currently derives only a handful of surface tiers via lighten/darken/blend, not full ramps — upgrading it to match is deferred to the porting plan, not done here |
| Ramp generation mechanism | Precomputed static CSS blocks (Approach B), not live in-browser OKLCH math — faster/simpler since this is throwaway mockup work; the real math only needs to exist once, in a throwaway offline script |
| Themes to preview | Breeze Dark, Gruvbox, Nord, Matrix, Light (real fixed palettes from `theme.rs`), plus one generic "Native (example)" using a plausible default accent, clearly illustrative |
| Switcher UI | No new control — wire the mockup's existing (currently inert) Setup → Appearance theme buttons to set `data-theme` on the root live |

## Design

### 1. Effects page — accordions, no forced full height

- Each of the 9 effect cards (Fidelity, Ambience, Surround, DynBoost, Bass,
  Loudness, Crossfeed, Output dither, Convolution) gets a click target that
  expands it in place to show real per-effect detail:
  - Convolution → the loaded IR's waveform (sample data)
  - Crossfeed → a simple before/after crossfeed diagram
  - Others → a small explanatory diagram or the effect's own mini curve,
    whichever is representative of what that effect actually does
- Only one card expands at a time (accordion, not independent toggles) to
  keep the page from becoming visually busy.
- The page container stops stretching to `100vh` — at rest (nothing
  expanded) the grid is top-aligned and only as tall as its content; expanding
  a card grows the page naturally instead of revealing pre-reserved dead space.

### 2. Mixer page — per-channel FR overlay

- New card below the existing Applications / Outputs / Channels row:
  per-channel frequency-response curves (one line per channel) with a
  legend of eye-icon toggles to show/hide individual channels, matching the
  pattern already shipped in the real GUI for >2-channel setups.
- Sample data only (consistent with the rest of the mockup's fake
  `DaemonState`) — 2-channel (FL/FR) by default since that's the mockup's
  assumed device.

### 3. Presets page — recent activity / diff panel

- New card in the empty space under Profiles/Import/Preview/A-B/Device
  mapping: a short list of what changed on the currently loaded profile
  since it was last saved (e.g. "Band 3 gain +2.1 dB → +4.7 dB", "Preamp
  −6.0 dB → −6.9 dB"), building on the dirty-state concept already in the
  Profiles pane.
- Empty state (no unsaved changes) shows a quiet "No changes since last
  save" line rather than disappearing, so the card doesn't itself become a
  new empty-space problem.

### 4. Theme preview

- A one-off script (Python, run locally, not shipped) computes OKLCH-based
  ramps for each of the 6 preview themes, seeded from the real colours in
  `theme.rs` (`accent`, `boost`, `cut`, `graph_bg` per `Theme::palette()`),
  reusing the lightness stops already implicit in the current default
  purple ramp (reverse-engineered from `styles.css`).
- Output is pasted into `styles.css` as `:root[data-theme="nord"] { ... }`
  style override blocks — one per theme, each redefining the ~15 base
  tokens (`--color-bg/-surface/-text/-accent/-accent-2/-divider` and the
  `neutral-*`/`accent-*`/`accent-2-*` ramps). Boost/cut map to whatever
  green/red (or, for Matrix, green/dim-green) equivalents the mockup uses
  for signed gain values, per theme.
- The Setup page's existing Appearance buttons (Native (auto), Native Dark,
  Native Light, Breeze Dark, Gruvbox, Nord, Matrix, Light, Matugen (auto))
  get a click handler that sets `data-theme` on `<html>` for the 6 previewable
  entries; the 3 host-dependent entries (Native (auto)/Matugen (auto), and
  Native Dark/Light without a fixed accent) stay visually inert or fall back
  to the generic Native example, with no claim of accuracy.
- Verification: screenshot each of the 6 themes on at least the Equalize
  page (most visually complex) via headless chromium, reviewed inline.

## Out of scope

- Any change to `theme.rs`, `resonance-gui`, or other Rust code — this pass
  only touches the mockup files.
- Live/in-browser OKLCH computation (Approach A) — deferred permanently in
  favour of the precomputed-block approach unless a future need for
  fully-dynamic theming in the mockup itself arises.
- `Current UI (Recreation).dc.html` — untouched.
- Any new real DSP/IPC capability. The per-channel FR overlay and recent-
  activity diff are UI-only additions over the mockup's existing fake data
  layer, not functioning features.
- Deciding *when* to port to egui, or scoping that port — this spec is
  mockup polish only; the port gets its own brainstorm/spec when the user is
  ready to move forward.
