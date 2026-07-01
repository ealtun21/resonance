# Advanced-features visibility settings

**Date:** 2026-07-01
**Clients:** resonance-gui + resonance-tui
**Status:** design approved, ready for implementation plan

## Problem

The main UI has accumulated advanced controls that clutter the default experience.
Four features in particular are "too advanced" for a first-run/simple view:

- per-band filter **slope** (12/24/48 dB/oct)
- per-band **scope** (Stereo/Mid/Side)
- output **dither** (off / 16 / 20 / 24-bit)
- **channels** (per-channel EQ, L/R swap, per-band channel targeting)

We want a settings menu that lets users enable/disable these advanced features so the
default UI is clean, while power users can opt in.

## Model & scope

A set of **client-local, per-feature visibility toggles** for exactly the four features
above, plus a small extensible framework so future advanced features slot in.

- Toggles are pure **UI preferences**, stored per client. They are **not** daemon state
  and do **not** appear in `.fac`/APO presets or profiles.
- **No changes to the DSP engine or the IPC protocol.** This is UI-only.
- Toggle granularity is **per-feature** (four independent switches), not a single master
  "advanced mode." Grouped under an "Advanced features" heading in each client's settings.

## Per-feature gating

When a feature's toggle is OFF:

| Feature | GUI (OFF) | TUI (OFF) |
|---|---|---|
| **Slope** | Hide the `Slope` column in the bands table | Hide the slope suffix in the `Type` cell; `Shift+S` disabled |
| **Scope** | Hide the `Scope` column in the bands table | Hide the scope suffix in the `Type` cell; `Shift+M` disabled |
| **Dither** | Hide the entire `Output`/dither section | Hide the dither status-bar indicator; `Shift+D` disabled |
| **Channels** | Remove the `Channels` section from the main view and **relocate** its controls (layout summary, L/R swap, per-channel-EQ toggle) into the Settings dialog. The per-band `Ch` column is hidden. | Hide the `Ch` column; `c` (channel targeting) and `w` (swap L/R) keys disabled. The swap toggle + layout summary are mirrored into the settings popup for parity; per-band targeting remains the on-band `c` picker, reachable only when the toggle is on. |

### Intentional GUI/TUI asymmetry on channels

- The **GUI** has a standalone `Channels` section (`devices.rs:104-175`), so its controls
  are physically **relocated** into the new Settings dialog when hidden.
- The **TUI** has no standalone channels *section* — only the `c` picker (modal), the `w`
  swap key, and the `Ch` band-table column. So the TUI toggle **gates** those keys + the
  column, and mirrors the swap/layout info into the existing settings popup. Per-band
  targeting stays as the on-band `c` picker.

## GUI: new modal Settings dialog

- Add a `Dialog::Settings` variant, following the existing modal pattern
  (`app.rs` `render_dialogs`, `dialogs.rs` for Help/Load Preset/Export).
- Opened from a **gear button** in the toolbar (`toolbar.rs`). The overflow (☰) menu's
  Theme entry is removed (Theme moves into the dialog); no other opener is added.
- Contents:
  1. **"Advanced features"** group — the four checkboxes (slope, scope, dither, channels).
  2. The **relocated Channels controls** (channel-count/layout summary, L/R swap toggle,
     per-channel-EQ toggle) — moved out of the left-column `Channels` section.
  3. The **existing Theme switcher moved out of the ☰ overflow menu** (`toolbar.rs:439-445`)
     into this dialog, so Settings is the single home for preferences.
- The left-column `Channels` section (`devices.rs` `channels_section`) is removed from the
  main layout (`layout.rs` wide + narrow paths); its rendering logic moves into the dialog.

## TUI: extend the existing Settings popup

- The TUI already has a Settings popup (`s` key) with tabs incl. **Preferences**
  (`settings.rs`, rendered at `ui.rs:1861-1911`, Preferences body `ui.rs:2263-2335`).
- Add an **"Advanced"** subsection to the **Preferences tab** (next to the existing
  "show spectrum" visibility toggle) containing the four toggles.
- Mirror the channel **swap toggle + layout summary** into that tab for parity with the
  GUI (informational + swap control); per-band channel targeting stays as the on-band `c`
  picker.

## Persistence & DSP-safety

- **GUI:** persist via egui storage, matching how `theme` (`app.rs:1193`) and
  `per_channel_eq` (`app.rs:335`) are already stored.
- **TUI:** add fields to the `Prefs` struct (`prefs.rs`), which is already
  serialized to the config directory on load/save.

**Toggles are UI-only and never mutate DSP state.** Hiding "dither" while it is set to
24-bit leaves dither running at 24-bit — it is only hidden from view. Rationale: resetting
a feature to neutral on hide would silently destroy a loaded preset's per-band slopes,
mid/side scopes, or per-channel routing, which is unacceptable.

To avoid a "hidden but active" footgun, each client keeps a compact **"advanced active"**
hint (e.g. in the status bar) when a hidden feature currently holds a **non-default**
value (dither ≠ off, any band slope ≠ default, any band scope ≠ Stereo, or any per-channel
routing in effect). When every hidden feature is at its default, no hint shows.

## Defaults & multichannel

- All four toggles default **OFF** (clean UI) on a fresh install.
- **Exception:** on a device with **`>2` channels**, the channels controls auto-show
  (mirrors today's "per-channel EQ auto-on for `>2ch`" behaviour, GUI `app.rs:335` /
  `devices.rs`, TUI `show_ch()` `app.rs:1205-1207`). Stereo users get the simple view.
- Discoverability: help popups / footer hints do **not** advertise disabled keybindings;
  the settings menu is the documented place to enable them. (When a key is disabled and
  pressed, it is a no-op — optionally a brief "enable in settings" status message.)

## Testing

- **TUI:** `Prefs` round-trip test (serialize/deserialize the new fields; correct
  defaults). Unit tests for the visibility predicates: given (toggle state, channel
  count) → is the column shown / is the key active.
- **GUI:** extract column/section visibility into **pure predicate functions** so they are
  unit-testable without a live egui context; test them against toggle + channel-count
  combinations. Test the `>2ch` auto-show exception.
- **Both:** verify the "advanced active" hint predicate — shows only when a hidden
  feature holds a non-default value.
- `make check` (fmt --check + clippy -D warnings + test --all) must pass before commit.

## Out of scope

- No master "advanced mode" switch (per-feature only).
- No new DSP effects, no IPC changes, no preset/profile format changes.
- Only the four named features are gated; the framework is extensible but we do not gate
  other controls (reference overlay, spectrum, effects rack, etc.) in this work.
