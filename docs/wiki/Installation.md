# Installation

Resonance runs on **Linux (PipeWire)**, **macOS (Core Audio)**, and **Windows
(APO)**. Pick your platform below.

- [Linux](#linux)
- [macOS](#macos)
- [Windows](#windows)
- [From source](#from-source)
- [Run at login (autostart)](#run-at-login)

---

## Linux

### One-liner (prebuilt, any distro)

Downloads the latest release tarball, verifies its checksum, and installs the
binaries into `/usr/local/bin` (or `~/.local/bin` if unprivileged):

```sh
curl -fsSL https://raw.githubusercontent.com/ealtun21/resonance/master/install.sh | bash
```

Pin a version with `RESONANCE_VERSION=v0.3.0`, or change the target with
`PREFIX=~/.local`.

**Steam Deck / immutable distros (SteamOS, Silverblue).** The installer detects
a read-only rootfs and installs into `~/.local` automatically — no
`steamos-readonly disable` (which is reverted on the next OS update). The daemon,
its socket, config, and the autostart entry all live under `$HOME`. Run it from
Desktop Mode.

### Arch Linux (from source, pacman-tracked)

The repo ships a `PKGBUILD`. Run the same script **inside a checkout** and it
builds a real pacman package (`resonance-eq`) instead of fetching prebuilts:

```sh
./install.sh        # makepkg -si — removable with: sudo pacman -R resonance-eq
```

Or invoke `makepkg -si` directly. On non-Arch checkouts `./install.sh` falls
back to `cargo build --release` + install into the prefix (force with
`FROM_SOURCE=1`).

### Prebuilt release binaries

Each tagged release attaches `resonance-<ver>-x86_64-linux.tar.gz`. The binaries
are dynamically linked: they need PipeWire installed at runtime
(`libpipewire-0.3.so.0`, shipped by every PipeWire distro) and glibc ≥ 2.39
(built on Ubuntu 24.04). The pure clients (`resonance`, `resonance-tui`,
`resonance-gui`) run anywhere that floor is met; `resonanced` additionally needs
a running PipeWire.

### How it works (Linux)

The daemon creates a virtual **"Resonance EQ"** sink and a `pw_filter` bridged
to your real device, set as default via WirePlumber metadata. Apps play to the
virtual sink → DSP → real device.

---

## macOS

Requires **macOS 14.2+** (Core Audio process-tap API). No third-party software
(BlackHole, Loopback, kexts) needed — the daemon taps system audio natively via
Apple's `CATapDescription`.

### Build the `.app` bundle (required for the first-run permission prompt)

```sh
git clone https://github.com/ealtun21/resonance && cd resonance
contrib/macos/build-app.sh
mv Resonance.app ~/Applications/
```

A plain `cargo build` binary compiles fine, but macOS will silently deny audio
capture from an unbundled CLI. `build-app.sh` produces an ad-hoc-signed `.app`
with the correct `Info.plist` + entitlements so the TCC prompt fires.

### Grant Screen Recording (one-time)

On macOS 14.2+ Apple gates the process-tap API behind the **Screen Recording**
TCC service, even though Resonance never captures the screen. Add it manually:

```sh
open "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
```

1. Click **+** and add `~/Applications/Resonance.app`.
2. Toggle Resonance **ON**.
3. Launch via Launch Services (**not** from a terminal — TCC attributes a
   terminal-spawned child's permissions to the terminal, not the daemon):

```sh
open ~/Applications/Resonance.app
```

From then on every app's audio flows through the DSP chain to your default
output. If the permission was denied the daemon log says so explicitly.

**Re-signing invalidates the grant.** Each `build-app.sh` rebuild changes the
bundle's cdhash, and TCC keys grants by code requirement — so after every
rebuild you must re-add Resonance to the Screen Recording panel. For a stable
grant, sign with a real identity:

```sh
SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" contrib/macos/build-app.sh
```

### File locations (macOS)

- Config: `~/Library/Application Support/resonance/`
- Logs (via launchd): `~/Library/Logs/resonance/`
- Socket: `$TMPDIR/resonance.sock`

---

## Windows

Download `resonance-setup-x.y.z.exe` from the
[releases page](https://github.com/ealtun21/resonance/releases) and run it. It
installs the daemon + clients, attaches the in-graph **Audio Processing Object
(APO)** to your playback device, and sets `DisableProtectedAudioDG` so the
unsigned APO loads. **No virtual cable or kernel driver.**

The daemon is the control plane only (it runs no audio on Windows — the APO does
the DSP inside the Windows audio engine). Drive it with the same CLI/TUI/GUI;
`resonance daemon enable` registers autostart via the Run registry key. Clients
reach the daemon over a localhost TCP port (written to `resonance.port` under
`%TEMP%`) instead of a Unix socket.

> DRM/protected-audio apps may mute while the APO is active. Uninstall via
> *Add/Remove Programs*, which detaches the APO and restores the audio settings.

---

## From source

Prerequisites on Arch/CachyOS (the `pipewire` package ships libpipewire + SPA
headers/pkg-config; `pkgconf` provides `pkg-config`):

```sh
sudo pacman -S pipewire pkgconf
cargo build --release --all
```

Binaries land in `target/release/`: `resonanced`, `resonance`, `resonance-tui`,
`resonance-gui`.

---

## Run at login

Every client can install a per-user autostart entry the same way on every OS:

```sh
resonance daemon enable      # and: disable / start / stop / restart / status
```

- **Linux** — writes a `systemd --user` unit; where `systemctl --user` is absent
  (OpenRC, runit, SysV, bare session) it falls back to a freedesktop
  `autostart/resonanced.desktop` + direct process control.
- **macOS** — installs a LaunchAgent
  (`~/Library/LaunchAgents/com.ealtun21.resonanced.plist`).
- **Windows** — registers the Run registry key.

The GUI/TUI also expose Start / Autostart toggles.
