# Start tray with GUI — design

## Problem

The tray (`resonance-tray`) can be started manually or at login (autostart), but a
user who just opens the GUI has no way to also bring the tray up automatically. We
want the GUI to start the tray on launch, on by default, without ever spawning a
duplicate when the tray is already running (e.g. it came up from autostart).

## Setting

New field on the shared tray config:

```rust
// crates/resonance-ipc/src/tray/config.rs — TrayConfig
/// When set, launching the GUI also starts the tray (unless it is already
/// running). On by default. GUI-scoped: the tray-with-GUI launch behaviour only
/// makes sense for the GUI process.
pub start_tray_with_gui: bool,   // Default: true
```

Stored in the shared `TrayConfig` TOML alongside the existing tray prefs
(`left_click`, `poll_secs`, `quit_stops_daemon`, `recent_count`). Chosen over
GUI-local eframe storage because `TrayConfig::load()` is synchronously available in
the GUI's pre-window startup thread (eframe storage is only readable once `GuiApp`
is constructed), and it keeps all tray settings in one place.

## Behaviour

In the GUI's startup supervisor thread (`run_native_app` in
`crates/resonance-gui/src/main.rs`, the same background thread that runs
`ensure_daemon_running`): after kicking off the daemon check, if
`start_tray_with_gui` is set, call `resonance_ipc::tray::control::start()`.

`control::start()` is already idempotent:

```rust
pub fn start() -> io::Result<()> {
    if is_running() { return Ok(()); }   // no duplicate when already up
    spawn_detached(&mut Command::new(tray::tray_bin()))
}
```

so a tray already running from autostart (or a previous launch) is left untouched —
this satisfies the "only start if not already started" requirement for free. The
call is best-effort: a spawn failure (e.g. no tray binary in a minimal build) is
logged/ignored and never blocks the GUI. It runs off the main thread, so it never
delays the window.

## UI

Add a checkbox **"Start the tray when the GUI launches"** to the Tray section of the
GUI settings dialog (`crates/resonance-gui/src/ui/dialogs.rs`), grouped with the
other tray toggles, bound to `cfg.start_tray_with_gui` and persisted on change like
its neighbours.

## Scope (out of scope by decision)

- No TUI settings row and no CLI `tray config` key for this field — it is GUI-only.
  The field still serializes/deserializes cleanly in the shared config regardless.
- No "start tray with TUI" analog (YAGNI).

## Tests

- Update the `TrayConfig` default test in `config.rs` to assert
  `start_tray_with_gui == true`.
- Existing round-trip serialization test covers the new field (extend its literal to
  include it).

## Verification

`make check` (fmt + clippy -D warnings + test --all) green. The GUI-startup spawn
path is not headlessly unit-testable (it launches a detached subprocess from a
pre-window thread); correctness rests on the idempotent `control::start()` (already
covered by its own module) plus the config default/round-trip tests.
