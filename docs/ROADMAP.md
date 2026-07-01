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

- **Convolution / impulse-response (IR) loader** — load a `.wav` IR for room
  correction, speaker correction, or HRTF/spatialization. EqualizerAPO and ViPER
  have it. Angle: FFT partitioned convolution via `rustfft` on the RT path
  (shares the linear-phase FIR work in backlog item 8); a new `ProcessorChain`
  stage + a "load IR" surface in CLI/GUI/TUI. Reports added latency.

- **Loudness compensation (ISO-226 equal-loudness)** — the "loudness" button:
  auto bass/treble lift at low listening levels tracking the equal-loudness
  contours. Every consumer EQ has it; flagged in the FineTune comparison. Angle:
  a level-dependent shelf/tilt derived from ISO-226, gated by the current
  volume; a single toggle + reference level. Small–medium effort.

## Medium value

- **Crossfeed for headphones** (Bauer/Meier) — reduce hard L/R isolation to cut
  listening fatigue. Already in backlog item 8. New `ProcessorChain` effect.

- **Adjustable filter slopes** — selectable 12/24/48 dB/oct on shelves and
  HP/LP (we're fixed at 2nd-order biquads). Pro-EQ standard. Angle: cascade N
  biquads per band; extend `BandType`/`ApoFilter` with an order field.

- **Dynamic EQ** — per-band gain driven by input level (de-essing,
  compression-style). Backlog item 8. Sidechain envelope follower per biquad.

- **Linear-phase EQ mode** — FIR path avoiding biquad phase rotation. Backlog
  item 8; pairs with the convolution engine above. Significant latency.

- **Mid/side EQ mode** — process mono sum / stereo difference independently.
  Backlog item 8; a `BandScope` (`Stereo | Mid | Side`) per band.

- **Global hotkeys** — toggle power/EQ or cycle presets from anywhere (FxSound
  and consumer apps have them). Per-OS global hotkey registration.

- **Input-device / source selection** — eqMac lets you pick the capture source;
  we only follow the output device. Expose an input picker.

## Lower / niche

- **Per-band solo/listen** — audition one band in isolation while tuning. Small
  DSP + a UI affordance.

- **VST plugin hosting** — EqualizerAPO hosts VST. Large effort, niche on Linux.

- **Spectrum-grab / EQ-match / draw-curve** — match a target spectrum or draw an
  EQ shape freehand (FabFilter). Large; overlaps the existing reference/Auto-EQ
  work.

- **Output dithering** — TPDF dither before F32 truncation. Backlog item 8.

## Where Resonance is already ahead

Cross-platform from one codebase (Linux/PipeWire + Windows/APO + macOS/CoreAudio);
squig.link + Auto-EQ integration with a live reference/measurement overlay;
N-channel + per-channel EQ + routing matrix; per-application **and** per-output
volume/mute on all three platforms; three front-ends (GUI/TUI/CLI); FxSound
effect emulation (Fidelity/Ambience/Surround/Dynamic Boost/Bass). Most
competitors do a subset of these on a single OS.
