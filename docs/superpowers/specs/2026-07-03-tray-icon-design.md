# System-tray icon + modular component graph — design

**Date:** 2026-07-03
**Status:** Approved (design), pending implementation plan

## Summary

Add a cross-platform, low-resource system-tray icon for Resonance as a **standalone
optional process** (`resonance-tray`). The tray controls the daemon (power/preset
toggles, start/stop/restart) and launches/quits a user interface, but is never embedded
in the daemon, and the daemon runs fine without it.

The work also formalizes a **modular component graph**: the daemon is always required, at
least one interface (`cli`, `tui`, or `gui`) is required, and the tray is an optional
add-on that cannot run standalone. All three interfaces — and the CLI in particular —
reach **full feature parity** for tray control, backed by one shared control module so no
interface is privileged.

## Goals

- One tray icon binary that works on Linux, Windows, and macOS.
- Low resource use: no GTK on Linux, no resident UI held alive just for the tray, embedded
  icon assets (no runtime SVG rasterizer), an idle/event-driven loop, and a slow status
  poll.
- Tray is a **separate process** from both the daemon and the UI. Daemon can run with or
  without it. The tray can start/stop the daemon but is never hosted inside it.
- Tray can start and stop a UI (GUI or, best-effort, TUI).
- Full parity: **cli, tui, and gui** can each do everything to the tray (start/stop,
  autostart, all settings, status). The CLI can do everything.
- Optional, modular install: a user can ship/install only the parts they want. Tray is
  opt-in and requires at least one interface — it is **not standalone**.
- Nice looking, using the existing project icon, with an active-vs-bypassed visual state.

## Non-goals

- No audio DSP changes. The tray touches no audio path; `resonance verify` is not involved.
- No new IPC protocol for audio; the tray reuses existing `Command`/`Response` types.
- No GTK/libappindicator runtime dependency on Linux.
- No always-resident GUI/TUI process just to host the tray.

## Component / requirement graph

```
resonanced (daemon)          REQUIRED  — core, headless, no UI
─── at least one interface (>=1 required) ──
resonance    (cli)   ─┐
resonance-tui         ├─ each is a full-parity interface
resonance-gui        ─┘
─── optional add-on ──
resonance-tray   OPTIONAL — requires daemon + >=1 of {cli, tui, gui}; NOT standalone
```

Enforced two ways:

1. **Packaging** — the tray package declares `Depends: resonanced AND (resonance-gui |
   resonance-tui | resonance)`. The exact expression per format (deb `|` alternatives,
   rpm boolean `Requires`, pacman `depends`/`optdepends`) is decided in the implementation
   plan; the intent is "daemon + at least one interface."
2. **Runtime guard** — on startup `resonance-tray` verifies at least one UI binary is
   reachable (sibling-of-exe lookup, then `$PATH`). If none is found it exits with a clear
   error rather than running as an orphan.

## Architecture

### Shared control module — `resonance-ipc::tray`

All tray *control* logic lives in a new module in `resonance-ipc`, the existing dep-light
crate that already hosts `service` and `paths`. It is std-only (no `ksni`, no `tray-icon`,
no heavy deps), so every interface can depend on it cheaply. This module is the mechanism
that guarantees parity.

It exposes:

- `TrayConfig` — serde/toml struct persisted at `$XDG_CONFIG_HOME/resonance/tray.toml`
  (platform config dir elsewhere), with:
  - `left_click`: `ToggleUi | Menu` — what a left click does.
  - `poll_secs`: status poll cadence (default 3).
  - `close_gui_to_tray`: bool — read by the UI; when set, closing the UI window exits the
    UI process but leaves the tray resident.
  - `recent_count`: how many presets to list in the Presets submenu.
  - `load()/save()` helpers.
- Process control: `spawn()`, `stop()`, `status()` for the tray process, using a flock
  pidfile at `$XDG_RUNTIME_DIR/resonance-tray.pid` (reusing the daemon's `shutdown.rs`
  singleton/cleanup pattern). `stop()` sends SIGTERM on Unix / taskkill on Windows.
- Autostart: `autostart_install()`, `autostart_remove()`, `autostart_status()` — the
  tray's **own** login entry (xdg-autostart `.desktop`, launchd plist / Login Item,
  Windows Run key), independent of the daemon's autostart. Generalized from the existing
  `service` module.
- UI discovery: `installed_uis()` → which of gui/tui/cli are present (used by both the
  runtime guard and the tray's adaptive menu).

`cli`, `tui`, and `gui` all call this module. No interface reimplements tray control.

### Tray binary — `resonance-tray` crate

The only crate that pulls tray backends, so the interfaces stay lean.

- Deps: `resonance-ipc`, `image` (PNG decode), and per-platform: `ksni` (Linux `cfg`),
  `tray-icon` (Windows + macOS `cfg`), `anyhow`.
- A `TrayBackend` trait with three `cfg`-gated implementations:
  - **Linux** → `ksni`: StatusNotifierItem over zbus D-Bus, no GTK/libappindicator, ~few-MB
    RSS. Runs its own D-Bus service task.
  - **Windows** → `tray-icon` (Shell_NotifyIcon) driven by a minimal message loop.
  - **macOS** → `tray-icon` (NSStatusItem) on the main-thread run loop.
- A shared **menu model** describing items + state; each backend renders it and reports
  click events back through a common event enum.

### Menu

```
● Resonance — <status>            tooltip: rate / channels / power
──────────────
[✓] Power (bypass toggle)         → IPC SetPower
Presets ▸  (recent/saved)         → IPC ListPresets + LoadPreset
──────────────
Open UI  /  Quit UI               → spawn / signal the UI (see below)
Daemon ▸ Start / Stop / Restart
        [✓] Autostart             → resonance-ipc::service
──────────────
[✓] Tray autostart at login       → resonance-ipc::tray autostart
Quit tray
```

The **Open/Quit UI** item adapts to installed interfaces:

- GUI present → "Open GUI" / "Quit GUI".
- Only TUI present → "Open TUI" — best-effort launch in the user's `$TERMINAL`.
- Only CLI present → the Open/Quit item is hidden (nothing windowed to open); daemon and
  power/preset control remain.

### Daemon communication

- **Power + presets** → `resonance-ipc` client: `GetState`, `SetPower`, `ListPresets`,
  `LoadPreset` (existing commands; no protocol change).
- **Daemon start/stop/restart/autostart** → `resonance-ipc::service` (the same code path
  the CLI `daemon` subcommand already uses).
- **Daemon down** → graceful: power/preset items are disabled and a "Start daemon" action
  is offered; the tray never errors out or spins.

### State refresh

A slow poll of `GetState` (default 3 s, `poll_secs`) updates:

- the icon variant (active vs. bypassed/grey),
- the tooltip (rate / channels / power),
- menu checkmarks (power, autostart states).

An immediate refresh runs after any tray-initiated action. Between polls the process is
idle (blocks on OS/D-Bus events), so CPU use is near zero.

### UI singleton + close-to-tray

Both the **GUI and the TUI** gain:

- a pidfile + "raise on second launch": a second launch signals the running instance to
  raise/focus, then exits. This lets the tray's "Open UI" focus an existing instance
  instead of spawning duplicates.
- `close_gui_to_tray` behavior: when enabled and the tray is running, closing the UI
  window exits the UI process while the tray stays resident. This keeps RAM low — there is
  no hidden UI kept alive; "Open UI" later spawns a fresh instance.

Focus-existing-window is best-effort per platform; spawn-if-absent is guaranteed.

### Icons

Pre-rendered PNGs derived from the existing `contrib/io.github.ealtun21.Resonance.svg`,
two variants (active + bypassed/grey), at sizes 16/22/24/32/48, embedded via
`include_bytes!` and decoded with the `image` crate. No runtime SVG rasterizer. macOS uses
a template image so it tints correctly for light/dark menu bars. A small build/dev helper
(re)generates the PNGs from the SVG so they stay in sync.

## Full-parity control surface

Every interface performs every tray action, all via `resonance-ipc::tray` + the shared
`tray.toml`:

| action | cli | tui | gui |
|---|---|---|---|
| start / stop / restart tray | `tray start` / `stop` / `restart` | key + menu | button |
| tray autostart at login | `tray enable` / `disable` (+ `install`/`uninstall`) | toggle | toggle |
| left-click / poll / close-to-tray / recent-count | `tray config <key> <value>` | settings rows | settings rows |
| tray status | `tray status` | status line | status dot |

The CLI `tray` subcommand mirrors the shape of the existing `daemon` subcommand
(`start/stop/restart/enable/disable/install/uninstall/status`) plus a `config` action for
the `tray.toml` fields.

## Testing

Headless unit tests (no real tray surface, CI-friendly):

- `TrayConfig` toml round-trip + defaults.
- Menu-model state transitions (power on/off, daemon up/down, UI present/absent).
- IPC command mapping (menu action → correct `Command`).
- UI-detection guard (`installed_uis()` and the no-UI refusal).
- Daemon-down fallback (disabled items + "Start daemon" offered).

Tray rendering itself is verified by a build/CI smoke build per platform and manual check;
no audio path is touched, so `resonance verify` is not used.

## Low-resource guarantees

- No GTK/libappindicator on Linux (ksni is pure-Rust D-Bus).
- Tray backends (`ksni`/`tray-icon`) are isolated to the `resonance-tray` crate; the
  interfaces never pull them.
- No resident UI kept alive for the tray (close-to-tray exits the UI process).
- Embedded icon assets; no runtime SVG rasterizer.
- Event-driven idle loop + slow status poll.

## Open items for the implementation plan

- Exact per-format packaging expressions for "daemon + >=1 interface."
- Exact `ksni` version/async-runtime choice and the Win/mac event-loop integration for
  `tray-icon` (bare message loop vs. a minimal winit loop) — pick the lowest-overhead
  option per platform.
- Cross-process "raise existing UI window" mechanism per platform (signal vs. IPC ping).
- `$TERMINAL` resolution for best-effort "Open TUI."
