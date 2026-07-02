# Resonance roadmap

Feature backlog derived from a comparison against eqMac, EqualizerAPO/Peace,
FxSound, Boom3D/SoundSource, and consumer EQs (Wavelet/ViPER/Poweramp). Ordered
by value. Each item notes where competitors have it and a rough implementation
angle so it can be picked up directly.

> Tip: to task an item, reference it by its heading (e.g. "do *Per-app EQ*").

## High value

- **Per-app EQ** — a *different EQ curve per application* (not just per-app
  volume, which is done). SoundSource/Boom3D's headline feature and the biggest
  remaining differentiator. Angle: this is "Plan B" of per-app processing; the
  macOS per-app tap-mixer already sums per-app streams, so per-app filter banks
  slot in there. Linux: per-stream target routing or per-app filter graphs.
  Model/IPC needs a per-app band set (relates to backlog item 16's `BandScope`).

## Medium value

- **Input-device / source selection** — eqMac lets you pick the capture source;
  we only follow the output device. Expose an input picker.

## Lower / niche

- **Per-band solo/listen** — audition one band in isolation while tuning. Small
  DSP + a UI affordance. ( Add eye icon or listen ear icon )

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
2026-07-03 — closes backlog item 8 entirely). Most competitors do a subset of
these on a single OS.
