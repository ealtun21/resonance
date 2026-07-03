# Resonance

A terminal-driven equalizer daemon. Resonance routes audio through an `f64`
DSP chain (parametric EQ + FxSound-style effects) and plays the processed
result back. It loads FxSound `.fac` presets and EqualizerAPO `.txt` configs.

**Platforms:**
- **Linux / PipeWire** — creates a virtual "Resonance EQ" sink; apps that play
  to it are processed and forwarded to the real default device. See
  [PipeWire architecture](#architecture).
- **macOS / CoreAudio** — default input device → DSP → default output device
  (no kernel extension or virtual driver required). Acts as a real-time audio
  effect chain on whatever device you select as your input.
- **Windows / WASAPI** — an in-graph **Audio Processing Object (APO)** that the
  Windows audio engine loads on your playback device and runs the DSP in place.
  The daemon is the control plane (no audio); it pushes EQ/effect changes to the
  APO and reads back meters/spectrum. No virtual cable, no kernel driver — the
  installer attaches the APO and sets `DisableProtectedAudioDG` so the unsigned
  APO loads (DRM/protected-audio apps may mute while it's active).

[![CI](https://github.com/ealtun21/resonance/actions/workflows/ci.yml/badge.svg)](https://github.com/ealtun21/resonance/actions/workflows/ci.yml)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)

## Features

- **PipeWire two-stream node** — capture → process → playback, no extra routing.
- **Parametric EQ** — 14 biquad filter types (Audio EQ Cookbook formulas).
- **FxSound-matched effects** — Fidelity, Ambience, Surround, Dynamic Boost, Bass.
- **Preset interop** — FxSound `.fac` and EqualizerAPO `.txt`.
- **Three clients** — CLI (`resonance`), TUI (`resonance-tui`), GUI (`resonance-gui`).
- **Optional system tray** — `resonance-tray` controls daemon power/preset/
  lifecycle and opens/quits a UI; requires the daemon plus at least one
  client, never embedded in the daemon. See [contrib/tray/README.md](contrib/tray/README.md).
- **Lock-free RT audio path** — tokio IPC thread → `rtrb` SPSC → audio thread.

## Install

### One-liner (prebuilt, any distro)

Downloads the latest release tarball, verifies its checksum, and installs the
binaries into `/usr/local/bin` (or `~/.local/bin` if unprivileged):

```sh
curl -fsSL https://raw.githubusercontent.com/ealtun21/resonance/master/install.sh | bash
```

Pin a version with `RESONANCE_VERSION=v0.3.0` or change the target with
`PREFIX=~/.local`. The binaries are dynamic — see
[runtime requirements](#prebuilt-release-binaries).

**Steam Deck / immutable distros (SteamOS, Silverblue).** The installer
detects a read-only rootfs and installs into `~/.local` automatically — no
`steamos-readonly disable` (which is reverted on the next OS update). The
daemon, its socket, config, and the autostart entry all live under `$HOME`, so
nothing touches the immutable `/usr`. Run it from Desktop Mode.

### Arch Linux (AUR-style, from source)

The repo ships a `PKGBUILD`. Run the same script **inside a checkout** and it
builds a real pacman package instead of fetching prebuilts:

```sh
./install.sh        # makepkg -si — tracked by pacman, removable with: sudo pacman -R resonance-eq
```

Or invoke makepkg directly:

```sh
makepkg -si
```

On non-Arch checkouts `./install.sh` falls back to `cargo build --release` +
install into the prefix. Force a source build with `FROM_SOURCE=1`.

**Run at login (any init system).** The IPC clients install a per-user
autostart entry the same way on every distro — `resonance daemon enable` (and
the Start / Autostart toggles in the TUI/GUI). On systemd it writes a
`--user` unit; where `systemctl --user` is absent (OpenRC, runit, SysV, or a
bare session) it falls back to a freedesktop `autostart/resonanced.desktop`
plus direct process control, so the daemon still autostarts and the same
`enable / disable / start / stop / restart` commands work. No `systemctl` or
init script to edit by hand.

### Prebuilt release binaries

Each tagged release attaches `resonance-<ver>-x86_64-linux.tar.gz`. The binaries are
dynamically linked: they need PipeWire installed at runtime (`libpipewire-0.3.so.0`,
shipped by every PipeWire distro) and glibc ≥ 2.39 (built on Ubuntu 24.04 — the
build floor is set by PipeWire 1.x dev headers, not glibc). The pure clients
(`resonance`, `resonance-tui`, `resonance-gui`) run
anywhere that floor is met; `resonanced` additionally needs a running PipeWire.

### From source (any distro)

Prerequisites (Arch/CachyOS) — the `pipewire` package ships both libpipewire
and the SPA headers/pkg-config files; `pkgconf` provides `pkg-config`:

```sh
sudo pacman -S pipewire pkgconf
```

Build:

```sh
cargo build --release --all
```

Binaries land in `target/release/`: `resonanced`, `resonance`, `resonance-tui`,
`resonance-gui`.

### macOS

Requires **macOS 14.2+** (Core Audio Process Tap API). No third-party
software (BlackHole, Loopback, kexts) needed — the daemon taps system
audio natively via Apple's `CATapDescription`.

**Build the .app bundle (required for first-run permission prompt):**

```sh
git clone https://github.com/ealtun21/resonance && cd resonance
contrib/macos/build-app.sh
mv Resonance.app ~/Applications/
```

A plain `cargo build` binary works for compilation, but macOS will silently
deny audio capture from an unbundled CLI. The `build-app.sh` script
produces an ad-hoc-signed `.app` wrapper with the correct `Info.plist` +
entitlements so the TCC permission prompt fires.

**Grant Screen Recording permission (one-time):**

On macOS 14.2+ Apple gates the Process Tap API behind the **Screen
Recording** TCC service (`kTCCServiceScreenCapture`), even though we
don't actually capture the screen. The permission *cannot* be granted
via the usual first-launch prompt for ad-hoc signed apps — you have to
add it manually:

```sh
# Open the right Settings pane:
open "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
```

1. Click the **+** at the bottom of the list.
2. Add `~/Applications/Resonance.app`.
3. Toggle Resonance **ON**.
4. Launch the daemon via Launch Services (NOT from your terminal —
   TCC attributes child processes' permissions to the parent terminal,
   not the daemon):

```sh
open ~/Applications/Resonance.app
```

From then on every running app's audio flows through the daemon's DSP
chain:

```
all system apps → CATapDescription (private, stereo mixdown)
              → AudioAggregateDevice (wraps the tap)
              → DSP chain (EQ + Fidelity + Ambience + Surround + Bass + …)
              → default output device (speakers / headphones / interface)
```

If the permission was denied the daemon log says so explicitly:
```
WARN tap IOProc has fired N times but every block was silent — macOS is
     most likely refusing system audio capture. Open the bundled
     Resonance.app via Launch Services and grant the prompt: ...
```

**Re-signing invalidates the grant.** Every `build-app.sh` rebuild
changes the bundle's cdhash, and macOS TCC keys grants by code
requirement, not bundle ID alone. After each rebuild you must re-add
Resonance to the Screen Recording panel. For a stable grant across
builds, sign with a real Developer ID identity:

```sh
SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" contrib/macos/build-app.sh
```

**Drive the daemon from any client (same as Linux):**

```sh
~/Applications/Resonance.app/Contents/MacOS/resonance status
# Symlink to ~/.local/bin if you want it on PATH:
ln -s ~/Applications/Resonance.app/Contents/MacOS/resonance ~/.local/bin/resonance
```

**Run at login (launchd).** The IPC clients can install a per-user
LaunchAgent (`~/Library/LaunchAgents/com.ealtun21.resonanced.plist`) the
same way they install the systemd unit on Linux — the underlying
`resonance_ipc::service` module dispatches by OS. The CLI `resonance` exposes
the same `enable / disable / start / stop / restart` commands as on Linux.

**File locations on macOS:**
- Config (profiles, mappings): `~/Library/Application Support/resonance/`
- Logs (when running via launchd): `~/Library/Logs/resonance/`
- Socket: `$TMPDIR/resonance.sock` (e.g. `/var/folders/.../T/resonance.sock`)

### Windows

Download the installer (`resonance-setup-x.y.z.exe`) from the
[releases page](https://github.com/ealtun21/resonance/releases) and run it. It
installs the daemon + clients, attaches the in-graph **Audio Processing Object
(APO)** to your playback device, and sets `DisableProtectedAudioDG` so the
unsigned APO loads. No virtual cable or kernel driver is involved.

The daemon is the control plane only (it runs no audio on Windows — the APO does
the DSP inside the Windows audio engine). Drive it with the same CLI/TUI/GUI as
elsewhere; `resonance daemon enable` registers autostart via the Run registry
key. Clients reach the daemon over a localhost TCP port (written to a
`resonance.port` file under the per-user temp dir, `%TEMP%`) instead of a Unix
socket.

DRM/protected-audio apps may mute while the APO is active. Uninstall via
*Add/Remove Programs*, which detaches the APO and restores the audio settings.

### Architecture

- **Linux**: virtual sink "Resonance EQ" + pw_filter ↔ real device, set as
  default via WirePlumber metadata. Apps → virtual sink → DSP → real device.
- **macOS**: cpal duplex stream (input device → SPSC ring → output device,
  with the DSP chain in the output callback). Sample rate negotiated between
  the two devices (target 48 kHz). To process system audio rather than mic
  input, route system audio through an aggregate device or a loopback driver
  of your choice and select it as the default input.

## Usage

Start the daemon:

```sh
RUST_LOG=info resonanced
```

Drive it with the CLI:

```sh
# State & effects
resonance status                       # show power, preset, effects, EQ, meters
resonance power on|off                 # master bypass
resonance set fidelity 70              # effect intensity 0–100
                                       #   (fidelity|ambience|surround|dynamic_boost|bass)
resonance preamp -3.5                  # preamp gain in dB
resonance reset                        # flat EQ, effects off, 0 dB preamp

# Presets & profiles
resonance load /path/to/preset.fac     # load a .fac / EqualizerAPO .txt
resonance import preset.txt [name]     # parse + save as a profile (no load)
resonance export my-eq.txt             # write current EQ as EqualizerAPO .txt
resonance save <name>                  # save current state as a named profile
resonance profile <name>               # load a saved profile
resonance profiles                     # list saved profiles
resonance rm-profile <name>            # delete a profile
resonance rename <from> <to>           # rename a profile
resonance list [dir]                   # list preset files (default: XDG library)
resonance autoeq "HD 600"              # download an AutoEq correction + import it

# Output devices & per-output mappings
resonance devices                      # list output devices, mark the active one
resonance output <name>|auto           # pin an output (or follow the system default)
resonance map <profile>                # auto-load <profile> for the active output
resonance unmap                        # remove the active output's mapping
resonance maps                         # list output → profile mappings

# A/B compare
resonance store a|b                    # stash the current state into a slot
resonance recall a|b                   # restore a stashed slot

# Service control (systemd / launchd / Windows Run key)
resonance daemon start|stop|restart|enable|disable|install|uninstall|status

# Shell completions
resonance completions bash|zsh|fish|elvish|powershell
```

File paths (`load`/`import`/`export`/`list`) are resolved relative to your
current shell directory, not the daemon's.

Or use the interactive clients:

```sh
resonance-tui      # ratatui terminal UI
resonance-gui      # egui desktop UI
```

On Linux and macOS the daemon listens on a Unix socket; the default path is
platform-aware:
- Linux: `$XDG_RUNTIME_DIR/resonance.sock`
- macOS: `$TMPDIR/resonance.sock`

Override either with `RESONANCE_SOCKET=/some/path`. On Windows the clients use a
localhost TCP port instead (its number is stored in a `resonance.port` file under
`%TEMP%`; `RESONANCE_SOCKET` overrides that path too).

## Architecture

```
resonance/
└── crates/
    ├── resonance-dsp/      platform-agnostic DSP engine (f64)
    ├── resonance-preset/   .fac + APO .txt parsers → Preset model
    ├── resonance-ipc/      serde protocol + length-prefixed postcard transport
    │                       + platform-aware paths + service control
    │                       (systemd on Linux, launchd on macOS, Run key on
    │                       Windows)
    ├── resonance-daemon/   audio node + tokio IPC server (Unix socket on
    │                       Unix, loopback TCP on Windows).
    │                       Audio backend dispatches by target_os:
    │                         linux → src/audio/pipewire.rs (PipeWire)
    │                         macos → src/audio/coreaudio.rs (cpal/CoreAudio)
    │                         windows → control plane only; the APO runs the DSP
    ├── resonance-apo/      Windows APO: Rust DSP engine (C-ABI staticlib) +
    │                       C++ CBaseAudioProcessingObject shell (COM), plus the
    │                       memory-mapped daemon⇄APO control/telemetry bridge
    ├── resonance-cli/      CLI client  (resonance)
    ├── resonance-tui/      ratatui TUI client (resonance-tui)
    ├── resonance-gui/      egui/eframe desktop client (resonance-gui)
    └── resonance-tray/     optional system-tray controller (resonance-tray);
                            own process, requires the daemon + one client,
                            never embedded in resonance-daemon
```

Signal flow:
- **Linux**: PipeWire virtual-sink capture → ProcessorChain (APO filter bank
  → Fidelity → Ambience → Surround → Dynamic Boost → Bass) → PipeWire real
  device.
- **macOS**: `CATapDescription` (stereo mixdown of every process) →
  `AudioHardwareCreateProcessTap` → private `AudioAggregateDevice` →
  cpal input stream → ring buffer → output callback runs the same
  ProcessorChain → default output device. The tap is `MutedWhenTapped`
  so the original audio path is suppressed while we own routing; the
  user hears only the DSP-processed signal.
- **Windows**: apps → audio engine (`audiodg.exe`) → Resonance APO running the
  same ProcessorChain in-graph on the render endpoint → device. The daemon
  publishes EQ/effect changes to the APO over a memory-mapped file (seqlock)
  and reads meters + spectrum back the same way; the APO is COM-aggregated via
  a thin C++ `CBaseAudioProcessingObject` shell that links the Rust engine.

## Development

```sh
make check     # fmt --check + clippy -D warnings + test --all
make fmt-fix   # apply rustfmt
cargo test --all
```

Conventional Commits. Run `make check` before every commit.

## License

[GPL-3.0-or-later](LICENSE).
