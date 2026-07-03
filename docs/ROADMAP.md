# Resonance roadmap

Feature backlog derived from a comparison against eqMac, EqualizerAPO/Peace,
FxSound, Boom3D/SoundSource, and consumer EQs (Wavelet/ViPER/Poweramp). Ordered
by value. Each item notes where competitors have it and a rough implementation
angle so it can be picked up directly.

> Tip: to task an item, reference it by its heading (e.g. "do *Per-app EQ*").

## High value

_(Per-app EQ — a different EQ curve per application — was dropped: it cannot reach
full three-platform parity. Windows' APO is an endpoint-level effect with no
per-audio-session DSP hook, so per-app EQ would be macOS + Linux only. The
parity-first rule keeps it off the backlog until a Windows per-session path
exists.)_

## Medium value

- **Input-device / source selection** — eqMac lets you pick the capture source;
  we only follow the output device. Expose an input picker.

## Lower / niche

- **Per-band listen/bandpass** — the richer follow-up to per-band solo: hear
  *only the frequency region a band covers* (a band-pass audition,
  FabFilter-style), rather than the whole signal with the other bands bypassed.
  Needs a temporary band-pass around the soloed band's centre/width in the DSP.

## Where Resonance is already ahead

Cross-platform from one codebase (Linux/PipeWire + Windows/APO + macOS/CoreAudio);
squig.link + Auto-EQ integration with a live reference/measurement overlay;
N-channel + per-channel EQ + routing matrix; per-application **and** per-output
volume/mute on all three platforms; three front-ends (GUI/TUI/CLI); FxSound
effect emulation (Fidelity/Ambience/Surround/Dynamic Boost/Bass);
**loudness compensation (ISO 226:2023 equal-loudness)** — the "loudness" button
most consumer EQs have; **headphone crossfeed** (Bauer/Meier); **adjustable
filter slopes** (12/24/48 dB/oct Butterworth on shelves + HP/LP); **mid/side
EQ** (per-band `Stereo | Mid | Side`); **output dithering** (TPDF); a
**convolution / impulse-response loader** (partitioned FFT, WAV IRs for room
correction/HRTF, reported latency, persisted in profiles — shipped 2026-07-02);
**dynamic EQ** (per-band level-driven gain morph — threshold/range/attack/
release, band-passed sidechain, zero added latency; peaking bands, shipped
2026-07-02); and a **linear-phase EQ mode** (static bands rendered to a
symmetric FIR through the partitioned engine — no phase rotation, ~171 ms
added latency at 48 kHz, hybrid with M/S + dynamic bands staying IIR; shipped
2026-07-03 — closes backlog item 8 entirely); and **per-band solo** (audition
one EQ band in isolation while tuning — a transient chain flag that bypasses
every other band, never persisted, suspends linear-phase while active; CLI
`band-solo`, GUI ear toggle, TUI `L`; all three platforms via the shared
`ProcessorChain` + APO snapshot v8; shipped 2026-07-03). Most competitors do a
subset of these on a single OS.
