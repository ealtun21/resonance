# Docs & GitHub landing overhaul (niri-style)

Date: 2026-07-04
Status: approved, in progress

## Problem

The README is textually current but is a wall of prose with **zero images**, and
the GitHub repo **About** blurb ("Terminal EQ daemon for Linux/PipeWire ...")
still reads as TUI-only + Linux-only. There are no screenshots, no topics, no
homepage, and the (enabled) wiki is empty. Goal: a polished, image-led landing
page modelled on <https://github.com/niri-wm/niri>, plus a real wiki.

## Deliverables

1. **Wordmark banner** — `docs/media/banner.svg` (+ rendered PNG), built in the
   app's visual language (purple→teal gradient, EQ bars, response curve from the
   existing icon) with a "Resonance" wordmark and tagline
   *"System-wide equalizer & audio effects — Linux · macOS · Windows"*.
2. **Screenshots** under `docs/media/`:
   - GUI hero (Breeze Dark, preset loaded, FR curve) — Xvfb + `uishot.sh`.
   - Theme gallery: Gruvbox, Nord, Matrix, Light (matugen excluded).
   - TUI (EQ curve + panels) — real terminal (`kitty`/`foot`) under Xvfb → PNG.
   - CLI (`resonance status`) — terminal capture.
   - Tray menu — best-effort (needs a status-notifier host in Xvfb).
   - Windows GUI — Windows VM, if a live desktop session is reachable.
   - macOS GUI — MacBook over Tailscale, if a GUI session is available.
   - ⚠️ items: attempt real capture; otherwise a labelled placeholder + a flag
     for the user to supply. Layout is identical either way. Clients only — the
     live daemon/audio is never restarted or re-routed.
3. **README rewrite** (niri structure, leaner):
   banner → badges (CI · License · Release · Platforms) → quick-link buttons
   (Install · Wiki · Usage · Roadmap · Releases) → hero screenshot → About →
   Features → screenshot gallery → short per-platform Install quickstart
   (details → wiki) → key Usage commands (full ref → wiki) → Status/roadmap →
   Contributing/License. Verbose install/usage prose moves to the wiki.
4. **Repo metadata**:
   - About → "System-wide parametric EQ + audio-effects engine for Linux, macOS
     & Windows — GUI, TUI & CLI, with FxSound & EqualizerAPO preset support."
   - Topics → equalizer, audio, dsp, pipewire, coreaudio, wasapi, apo, fxsound,
     equalizerapo, rust, egui, ratatui, parametric-eq, crossfeed, convolution,
     linux, macos, windows.
   - Homepage → wiki URL.
5. **Wiki pages** (`resonance.wiki` repo): Home, Installation, Usage & CLI
   reference, Presets & AutoEQ, Effects & DSP, Configuration & autostart,
   Troubleshooting, Architecture. Images referenced via `raw.githubusercontent`
   URLs so they render in both README and wiki.

## Execution order

Banner → Linux captures (GUI themes, TUI, CLI) → ⚠️ captures (tray, Windows,
macOS) → README → wiki → repo metadata → verify links render.

## Non-goals

- No GitHub Pages site (deferred; wiki chosen instead).
- No AI-related content anywhere (project convention).
- No changes to code behaviour; docs/assets/metadata only.
