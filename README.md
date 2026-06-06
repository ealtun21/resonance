# Resonance

A terminal-driven equalizer daemon for Linux / PipeWire. Resonance captures system
audio, runs it through an `f64` DSP chain (parametric EQ + FxSound-style effects),
and plays the processed result back. It loads FxSound `.fac` presets and
EqualizerAPO `.txt` configs.

[![CI](https://github.com/ealtun21/resonance/actions/workflows/ci.yml/badge.svg)](https://github.com/ealtun21/resonance/actions/workflows/ci.yml)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)

## Features

- **PipeWire two-stream node** — capture → process → playback, no extra routing.
- **Parametric EQ** — 14 biquad filter types (Audio EQ Cookbook formulas).
- **FxSound-matched effects** — Fidelity, Ambience, Surround, Dynamic Boost, Bass.
- **Preset interop** — FxSound `.fac` and EqualizerAPO `.txt`.
- **Three clients** — CLI (`resonance`), TUI (`resonance-tui`), GUI (`resonance-gui`).
- **Lock-free RT audio path** — tokio IPC thread → `rtrb` SPSC → audio thread.

## Install

### One-liner (prebuilt, any distro)

Downloads the latest release tarball, verifies its checksum, and installs the
binaries into `/usr/local/bin` (or `~/.local/bin` if unprivileged):

```sh
curl -fsSL https://raw.githubusercontent.com/ealtun21/resonance/master/install.sh | bash
```

Pin a version with `RESONANCE_VERSION=v0.2.0` or change the target with
`PREFIX=~/.local`. The binaries are dynamic — see
[runtime requirements](#prebuilt-release-binaries).

### Arch Linux (AUR-style, from source)

The repo ships a `PKGBUILD`. Run the same script **inside a checkout** and it
builds a real pacman package instead of fetching prebuilts:

```sh
./install.sh        # makepkg -si — tracked by pacman, removable with: sudo pacman -R resonance
```

Or invoke makepkg directly:

```sh
makepkg -si
```

On non-Arch checkouts `./install.sh` falls back to `cargo build --release` +
install into the prefix. Force a source build with `FROM_SOURCE=1`.

### Prebuilt release binaries

Each tagged release attaches `resonance-<ver>-x86_64-linux.tar.gz`. The binaries are
dynamically linked: they need PipeWire installed at runtime (`libpipewire-0.3.so.0`,
shipped by every PipeWire distro) and glibc ≥ 2.39 (built on Ubuntu 24.04 — the
build floor is set by PipeWire 1.x dev headers, not glibc). The pure clients
(`resonance`, `resonance-tui`, `resonance-gui`) run
anywhere that floor is met; `resonanced` additionally needs a running PipeWire.

### From source (any distro)

Prerequisites (Arch/CachyOS):

```sh
sudo pacman -S pipewire libspa-0.3 pkg-config
```

Build:

```sh
cargo build --release --all
```

Binaries land in `target/release/`: `resonanced`, `resonance`, `resonance-tui`,
`resonance-gui`.

## Usage

Start the daemon:

```sh
RUST_LOG=info resonanced
```

Drive it with the CLI:

```sh
resonance status
resonance load /path/to/preset.fac
resonance set fidelity 70         # 0–100
resonance power on|off
resonance preamp -3.5
```

Or use the interactive clients:

```sh
resonance-tui      # ratatui terminal UI
resonance-gui      # egui desktop UI
```

The daemon listens on a Unix socket at `$XDG_RUNTIME_DIR/resonance.sock`
(override with `RESONANCE_SOCKET`).

## Architecture

```
resonance/
└── crates/
    ├── resonance-dsp/      platform-agnostic DSP engine (f64)
    ├── resonance-preset/   .fac + APO .txt parsers → Preset model
    ├── resonance-ipc/      serde protocol + length-prefixed postcard transport
    ├── resonance-daemon/   PipeWire node + tokio Unix-socket IPC server
    ├── resonance-cli/      CLI client  (resonance)
    ├── resonance-tui/      ratatui TUI client (resonance-tui)
    └── resonance-gui/      egui/eframe desktop client (resonance-gui)
```

Signal flow: PipeWire capture → ProcessorChain (APO filter bank → Fidelity →
Ambience → Surround → Dynamic Boost → Bass) → PipeWire playback.

## Development

```sh
make check     # fmt --check + clippy -D warnings + test --all
make fmt-fix   # apply rustfmt
cargo test --all
```

Conventional Commits. Run `make check` before every commit.

## License

[GPL-3.0-or-later](LICENSE).
