# Configuration

## Profiles & per-output mappings

State (EQ + effects + preamp, optionally a reference measurement) is saved as
named **profiles**. You can bind a profile to a specific output device so it
loads automatically when that device becomes active:

```sh
resonance devices                 # list outputs, mark the active one
resonance output "My DAC"         # pin an output (or `auto` to follow the default)
resonance map studio              # auto-load profile "studio" for the active output
resonance maps                    # list output → profile mappings
resonance unmap                   # remove the active output's mapping
```

## Autostart / service control

```sh
resonance daemon enable           # start at login  (disable to undo)
resonance daemon start|stop|restart|status
```

The mechanism is OS-native: a `systemd --user` unit on Linux (with a freedesktop
autostart fallback where `systemctl --user` is unavailable), a LaunchAgent on
macOS, and the Run registry key on Windows. See
[Installation → Run at login](Installation#run-at-login).

## System tray

The optional `resonance-tray` process shows a status-notifier icon with a menu:
power toggle, recent presets, Open UI, daemon control, autostart toggle, and
Quit. It's an add-on to a UI — it refuses to run without at least one client
installed and is never embedded in the daemon.

Config lives in `<config-dir>/tray.toml`:

```toml
poll-secs   = 3          # how often the menu refreshes from daemon state
left-click  = "toggle-ui" # or "menu" — what a left-click does
recent-count = 8         # how many recent presets to list
```

## Advanced-feature visibility

Advanced controls are **off by default** for a clean UI and can be enabled
per-feature in both clients:

- **GUI** — the gear-icon Settings dialog: per-band slope, scope, dynamics,
  dither, channels, plus theme.
- **TUI** — Settings → Preferences rows, or the `S` / `M` / `D` / `c` / `w`
  keys.

A status-bar `adv:` hint surfaces any hidden-but-non-default feature, so nothing
runs invisibly.

## File locations

| | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Config / profiles | `$XDG_CONFIG_HOME/resonance/` | `~/Library/Application Support/resonance/` | `%APPDATA%\resonance\` |
| Socket / port | `$XDG_RUNTIME_DIR/resonance.sock` | `$TMPDIR/resonance.sock` | `resonance.port` in `%TEMP%` |
| Logs | journald / stderr | `~/Library/Logs/resonance/` | daemon log path |

Override the socket/port path with `RESONANCE_SOCKET`.
