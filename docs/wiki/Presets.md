# Presets & ecosystem

Resonance is built for **preset interop** — the EQ does the tonal heavy lifting,
so presets from other tools translate faithfully.

## Supported formats

| Format | Extension | Notes |
| --- | --- | --- |
| FxSound preset | `.fac` | Effects map from MIDI knob values; EQ maps to the graphic EQ. |
| EqualizerAPO config | `.txt` | Full parametric bands (`Filter N: ON PK Fc … Gain … Q …`), preamp, REW-style lines. |
| REW correction export | `.txt` | Parsed as EqualizerAPO-style `Filter N:` lines. |
| AutoEq correction | `.txt` | Downloaded on demand (`resonance autoeq`). |

Load without saving:

```sh
resonance load preset.fac         # load and apply
resonance import preset.txt name  # parse + save as a profile, don't apply
resonance export my-eq.txt        # write the current EQ back out as APO .txt
```

`export` emits every band type as the correct APO keyword (`PK`, `LS`, `HS`,
`LP`, `HP`, `NO`, `AP`), so it round-trips.

## AutoEq

```sh
resonance autoeq "HD 600"
```

Downloads the community AutoEq correction profile for that headphone (the format
already matches APO `.txt`) and imports it.

## squig.link + reference measurements

In the GUI/TUI you can:

- overlay a **target curve** (Diffuse Field / Harman / PEQdB / custom) on the graph;
- **Browse** and download a headphone/IEM measurement straight from **squig.link**;
- shade the **listener-preference bounds** around the target;
- one-click **Auto-EQ** to fit the bands so the measured response matches the target;
- **Customize** the target (tilt, bass, ear-gain, treble) on top of any base curve.

See [Effects & DSP](Effects) for how the resulting bands are applied.

## Profiles

A *profile* is your saved daemon state (EQ + effects + preamp), stored under the
config dir. Profiles can carry their reference measurement with them.

```sh
resonance save night              # snapshot current state
resonance profile night           # restore it
resonance profiles                # list
resonance rename night evening
resonance rm-profile evening
```

## Preset metadata (sidecar)

A preset file can have a `<preset-file>.toml` sidecar with `author`,
`description`, and `tags`. The CLI `meta` subcommand reads/writes it, `resonance
list` shows it, and the TUI browser previews it.

## Preset library locations (XDG)

`resonance list` (and the browsers) scan the XDG preset dirs automatically:

- `$XDG_DATA_HOME/resonance/presets/`
- `$XDG_DATA_DIRS/*/resonance/presets/`
