# Resonance components

Resonance ships as a small set of independent binaries that cooperate over
the daemon's Unix-socket/TCP IPC protocol. None of them link against each
other — each is its own process, started and stopped independently.

- **`resonanced`** — the audio daemon. Required, headless. Captures system
  audio, runs the DSP chain, plays it back. Everything else is a client of
  its IPC socket.
- **One interface** (at least one is required to configure the daemon):
  `resonance` (CLI), `resonance-tui` (terminal UI), `resonance-gui`
  (desktop UI).
- **`resonance-tray`** — optional system-tray controller. It requires the
  daemon *and* at least one interface to be installed: it refuses to start
  with no interface present, and it is never embedded in the daemon (it is
  always its own process). A single-package build (the default for this
  project's PKGBUILD/RPM/tarball) installs all five binaries together, so
  the "requires" relationship above is a runtime contract, not something
  the package manager enforces — see "Modular install" below.

## What the tray does

The tray controls the daemon's power state, current preset, and lifecycle
(start/stop/restart), and it opens or quits whichever UI process the user
has configured. It does not run any DSP itself and it does not duplicate
daemon state — every action is a command sent over the same IPC protocol
the CLI/TUI/GUI already use.

Tray configuration and behaviour have full parity across all three
interfaces, backed by a shared `resonance-ipc::tray` module and a single
config file, `~/.config/resonance/tray.toml`:

- **CLI** — `resonance tray …` subcommands read/write the same config and
  can drive the tray non-interactively (scripting, headless setup).
- **GUI** — Settings → Tray section exposes the same options as a form.
- **TUI** — Settings → Tray tab exposes the same options as a form.

Whichever interface last changed the config, the others pick it up — there
is no interface-specific tray state.

## Modular install

Today the project builds and packages all binaries together (single
`cargo build --all --release`, one PKGBUILD/RPM/tarball). `resonance-tray`
is simply one more binary in that set: installing the package installs it,
but nothing requires the user to run it. Users who don't want a tray icon
just never launch `resonance-tray` (and can disable its autostart entry,
if one was created, from the tray's own settings).

Splitting `resonance-tray` into its own installable package (so it can be
pulled in or left out at the package-manager level, with a declared
`depends` on the daemon and an `optdepends`/"one of" relation on the three
interfaces) is deferred — it extends the already-open packaging backlog
(CLAUDE.md item 10: Flatpak/`.deb` still open). The functional guarantee —
"the tray refuses to run standalone" — is enforced at runtime by
`resonance-tray` itself regardless of how it's packaged, so leaving the
packages unsplit for now does not weaken that guarantee.

## Low resource

`resonance-tray` is intentionally light:

- **No GTK on Linux.** The Linux backend talks the StatusNotifierItem
  protocol directly over D-Bus (`ksni`), so there is no GTK (or Qt)
  dependency pulled in just to show a tray icon.
- **Backends are isolated.** The platform-specific tray toolkit code
  (`ksni` on Linux, `tray-icon`/`winit` on Windows and macOS) lives only in
  the `resonance-tray` crate. `resonanced` and the CLI/TUI/GUI interfaces
  never link any of it, so running without a tray costs those binaries
  nothing.
- **No resident UI for the tray itself.** The tray process only maintains
  a menu and polls daemon state to keep it current; it does not keep a
  hidden window or UI toolkit event loop alive beyond what's needed to
  host the tray icon.
