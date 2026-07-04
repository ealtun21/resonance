<div align="center">

<img src="https://raw.githubusercontent.com/wiki/ealtun21/resonance/banner.png" alt="Resonance" width="720">

</div>

**Resonance** is a system-wide equalizer and audio-effects engine for **Linux,
macOS, and Windows**. It captures your system audio, runs it through a
high-precision (`f64`) DSP chain — a full parametric EQ plus FxSound-style
effects — and plays the processed result back. One daemon drives three
front-ends: a desktop **GUI**, a terminal **TUI**, and a scriptable **CLI**.

<div align="center">

<img src="https://raw.githubusercontent.com/wiki/ealtun21/resonance/gui-breeze.png" alt="Resonance GUI" width="820">

</div>

## Wiki contents

- **[Installation](Installation)** — per-platform install (Linux, macOS, Windows) + autostart.
- **[Usage](Usage)** — full CLI reference, TUI/GUI, keyboard shortcuts, socket paths.
- **[Presets](Presets)** — `.fac` / EqualizerAPO `.txt`, profiles, AutoEq, REW, squig.link, metadata.
- **[Effects & DSP](Effects)** — the signal chain and every effect explained.
- **[Configuration](Configuration)** — profiles, per-output mappings, the tray, advanced-feature toggles.
- **[Troubleshooting](Troubleshooting)** — platform gotchas and the `resonance verify` tool.
- **[Architecture](Architecture)** — crates, signal flow, IPC, the real-time path.

## Quick start

```sh
# Linux — download, verify, install (also handles immutable distros)
curl -fsSL https://raw.githubusercontent.com/ealtun21/resonance/master/install.sh | bash

resonanced &                 # start the daemon
resonance status             # check it's running
resonance-gui                # open the desktop UI
```

See **[Installation](Installation)** for macOS and Windows, and every other install method.
