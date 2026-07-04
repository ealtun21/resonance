# Architecture

## Crate layout

```
resonance/
└── crates/
    ├── resonance-dsp/      platform-agnostic DSP engine (f64)
    ├── resonance-preset/   .fac + APO .txt parsers → Preset model
    ├── resonance-ipc/      serde protocol + length-prefixed postcard transport,
    │                       platform-aware paths + service control
    │                       (systemd / launchd / Windows Run key)
    ├── resonance-daemon/   audio node + tokio IPC server. Audio backend
    │                       dispatches by target_os:
    │                         linux   → PipeWire two-stream node
    │                         macos   → cpal / Core Audio duplex
    │                         windows → control plane only; the APO runs the DSP
    ├── resonance-apo/      Windows APO: Rust DSP engine (C-ABI staticlib) +
    │                       C++ CBaseAudioProcessingObject shell (COM) +
    │                       memory-mapped daemon⇄APO control/telemetry bridge
    ├── resonance-cli/      CLI client  (resonance)
    ├── resonance-tui/      ratatui TUI client (resonance-tui)
    ├── resonance-gui/      egui/eframe desktop client (resonance-gui)
    └── resonance-tray/     optional system-tray controller (resonance-tray)
```

## Signal flow per platform

- **Linux** — apps → virtual **"Resonance EQ"** sink → PipeWire capture →
  `ProcessorChain` → PipeWire real device. Set as default via WirePlumber
  metadata.
- **macOS** — `CATapDescription` (stereo mixdown of every process) →
  `AudioHardwareCreateProcessTap` → private `AudioAggregateDevice` → cpal input
  stream → ring buffer → output callback runs the same `ProcessorChain` → default
  output. The tap is `MutedWhenTapped`, so the user hears only the processed
  signal.
- **Windows** — apps → audio engine (`audiodg.exe`) → Resonance **APO** running
  the same `ProcessorChain` in-graph on the render endpoint → device. The daemon
  publishes EQ/effect changes to the APO over a memory-mapped file (seqlock) and
  reads meters + spectrum back the same way.

The **same `ProcessorChain`** (in `resonance-dsp`) runs on all three platforms —
only the capture/playback plumbing differs.

## IPC

- Transport: length-prefixed **postcard** encoding over a Unix socket
  (Linux/macOS) or a localhost TCP port (Windows).
- Path: `$XDG_RUNTIME_DIR/resonance.sock` (Linux), `$TMPDIR/resonance.sock`
  (macOS), or a `resonance.port` file (Windows). Override with
  `RESONANCE_SOCKET`.
- Thread separation: a tokio IPC thread hands control messages to the audio
  real-time thread over an **`rtrb` SPSC** ring — no allocations on the hot path.

## DSP design notes

- **`f64` throughout** for coefficient accuracy and sample precision.
- **Builder pattern** for constructing DSP chains; functional style (iterators,
  closures) preferred.
- Effect strength is matched to **FxSound** so imported presets translate
  faithfully — see [Effects & DSP](Effects) for the exact formulas.

## Building & testing

```sh
cargo build --release --all
make check      # rustfmt --check + clippy -D warnings + test --all
cargo test --all
```

Conventional Commits; run `make check` before every commit. See the
[repository](https://github.com/ealtun21/resonance) for CI and contribution
details.
