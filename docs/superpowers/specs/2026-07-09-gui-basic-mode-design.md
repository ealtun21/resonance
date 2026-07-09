# GUI Basic/Advanced Mode — Design

**Date:** 2026-07-09
**Status:** Approved
**Scope:** `resonance-gui` only. No IPC, daemon, or DSP changes. TUI/CLI untouched.

## Problem

The GUI today has one audience: people who can read a frequency-response
graph and know what Q means. The existing "advanced features" checkboxes
(slope, scope, dynamics, dither, convolution) already layer *extra*
complexity on top of that, but there is nothing below it. A user who just
wants "bass up, load a preset, done" — and who cannot read graphs — has no
mode to live in.

## Goal

Two UI modes:

- **Basic** — a beginner screen: power, preset carousel, vertical gain
  sliders, five friendly-named effect sliders. No graph, no band table, no
  numbers beyond frequency labels.
- **Advanced** — the current UI, byte-for-byte unchanged. The existing
  per-feature `show_*` checkboxes remain inside Advanced as the
  "extra advanced" layer.

## Decisions (from brainstorming)

| Question | Decision |
|----------|----------|
| Tier structure | 2 modes (Basic/Advanced); existing `show_*` checkboxes stay as-is inside Advanced |
| Basic content | Phone-style vertical EQ sliders + preset carousel + effect sliders |
| Slider ↔ band mapping | One slider per real gain-capable band (no fixed 10-band overlay, no profile rewriting) |
| Effects shown in Basic | The five preset-driven FxSound effects, friendly names |
| First run | Ask: modal choice dialog (Simple / Advanced), shown once |
| Preset carousel source | Saved profiles (same list as Advanced's Profiles panel) |
| Architecture | Mode enum + self-contained Basic screen (`ui/basic.rs`); Advanced code path untouched |

## Design

### 1. Mode model and persistence

- New `UiMode { Basic, Advanced }` enum, field on `GuiApp`.
- Persisted via eframe storage under key `ui_mode`, same mechanism as the
  existing `show_*` prefs (load in `GuiApp::new()`, save in `save()`).
- **First run:** if the `ui_mode` key is absent, show a modal
  `Dialog::ModeChoice` on the first frame. Two large buttons:
  - **Simple** — "Presets and sliders. No clutter."
  - **Advanced** — "Full parametric EQ, measurements, routing."
  The choice is saved immediately; the dialog never reappears. Existing
  installs (which have other prefs but no `ui_mode` key) see it exactly
  once — acceptable one-click cost, good discoverability.
- **Switching:** Basic screen has an `Advanced ⚙` button top-right; the
  Advanced toolbar gains a `Basic` button next to the Settings gear.
  Switching is instant and persisted.
- The Settings dialog (theme, advanced checkboxes, channels, phase) remains
  reachable from Advanced only. Basic inherits the active theme/palette.

### 2. Basic screen (`ui/basic.rs`, new file)

A single `CentralPanel` with a centered, max-width column. Built from the
existing painter-drawn kit widgets (`ui/kit.rs`) and the active `Palette`,
so all nine themes apply automatically.

```
⏻ Power                              [Advanced ⚙]

Preset:        ◀    Rock    ▶

  │   │   ●   │   │   │   │        vertical gain sliders,
  ●   │   │   │   ●   │   │        one per gain band
  │   ●   │   ●   │   ●   ●
 60  150  400  1k  2.4k  6k  15k   frequency labels

Bass ──●──  Clarity ──●──  Ambience ──●──
Wide ──●──  Boost ──●──

footer: connection state only
```

- No FR graph, no band table, no reference bar, no device map, no
  apps/outputs panels, no meters, no "adv:" hint.
- Slider count follows the profile. More than ~12 sliders → the slider row
  scrolls horizontally.
- Daemon not running → reuse the existing Start-daemon screen/button flow.

### 3. EQ slider behaviour

- Source of truth is the live `DaemonState` band table, identical to
  Advanced. Sliders render from state; dragging sends the same gain-only
  band edit the Advanced table sends (the SetBand path that preserves
  type/Q/slope/scope/dynamics — the shadow-side reset bug is already
  fixed).
- One slider per **gain-capable** band: PK, LS, HS. HP/LP/notch/AP bands
  get no slider; they keep running in the DSP, invisible in Basic, so
  preset semantics are fully preserved and profiles round-trip untouched.
- Slider range = the same gain edit limits Advanced uses. Center detent at
  0 dB. Double-click resets a slider to 0 dB. No visible dB numbers; the
  value appears in a tooltip while hovering/dragging.
- Label under each slider: the band's frequency, short-formatted
  (60, 150, 1k, 2.4k, 15k).
- Undo: the existing per-gesture undo system applies; Ctrl+Z works in
  Basic. No visible undo/reset button in v1.
- Dynamic bands: the slider edits base gain; dynamics settings are
  untouched. Per-channel profiles: Basic edits the band list exactly as
  Advanced's gain column would; channel routing is untouched.

### 4. Effect sliders

| Basic label | Effect | Notes |
|-------------|--------|-------|
| Bass | Bass | bipolar, centered at 0 |
| Clarity | Fidelity | 0–100 |
| Ambience | Ambience | 0–100 |
| Wide | Surround | bipolar, centered at 0 |
| Boost | DynBoost | 0–100 |

- Same value ranges and SetEffect IPC commands as the Advanced effects
  panel. State-driven, so loading a preset visibly moves the sliders.
- No numbers shown; tooltip only.
- Loudness, Crossfeed, preamp, dither, convolution, and linear phase are
  not shown in Basic. If the active profile enables them they keep
  running — that is the preset doing its job; no hint is shown.

### 5. Preset carousel

- Cycles the daemon's **saved profiles** — the same list, order, and
  switch action as clicking a profile in Advanced's Profiles panel (same
  unsaved-changes semantics; no new behaviour).
- Wraps at both ends. Shows the current profile name; a `•` dirty marker
  appears when the state has been edited since the profile was applied.
- No saved profiles → carousel shows "No presets yet" disabled, with a
  tooltip pointing at Advanced.

### 6. Edge cases

- Power off → sliders remain visible but dimmed, matching the Advanced
  bypass look.
- Basic never hides *state* — it hides *editors*. Anything a profile
  configures (IR, dither, M/S bands, HP/LP) continues to run; the power
  button remains the master bypass.

### 7. Testing

- Unit tests: mode persistence round-trip; first-run detection (`ui_mode`
  key absent); band→slider filter (PK/LS/HS in, HP/LP/notch/AP out);
  frequency label formatting; carousel ordering/wrap/dirty-marker.
- `make check` green (fmt, clippy pedantic, tests).
- Visual pass via the Xvfb screenshot harness (`contrib/dev/uishot.sh`)
  for: Basic screen, first-run dialog, mode switch both directions, all
  themes spot-check.
- No DSP or IPC changes → `resonance verify` not required.

## Out of scope

- TUI basic mode.
- New IPC commands or daemon changes.
- Window-geometry management per mode (rejected approach C).
- Fixed 10-band graphic EQ overlay / profile conversion (rejected —
  destructive).
- Visible undo/reset button in Basic (deferred; Ctrl+Z works).
