# Effects & DSP

Everything runs in **`f64`** end to end — coefficient accuracy and sample
precision over CPU efficiency. Audio is captured, converted to `f64`, pushed
through the chain below, and converted back for playback.

## Signal flow

```
capture ─▶ ProcessorChain ─▶ playback

ProcessorChain:
  ├─ APO filter bank      parametric EQ — 14 biquad filter types
  ├─ Convolution          WAV impulse response (partitioned FFT; off by default)
  ├─ Fidelity             FxSound Aural harmonic exciter
  ├─ Ambience             FxSound Lex reverb (Freeverb topology)
  ├─ Surround             FxSound Wide32 mid/side widener
  ├─ Dynamic Boost        loudness maximizer (makeup + lookahead limiter)
  ├─ Bass                 peaking shelf at 90 Hz, ±15 dB
  ├─ Loudness             ISO 226:2023 equal-loudness compensation
  ├─ Crossfeed            Bauer/Meier headphone crossfeed (runs last)
  └─ Dither               TPDF dither to a target bit depth (off by default)
```

## Parametric EQ

- **14 biquad filter types** (Audio EQ Cookbook formulas): peaking, low/high
  shelf, low/high pass, band-pass, notch, all-pass, and more.
- **Per-band slope** — 12 / 24 / 48 dB/oct (Butterworth cascade on shelves + HP/LP).
- **Per-band scope** — `Stereo | Mid | Side` (process the mono sum or the stereo
  difference independently).
- **Dynamic EQ** — per-band, level-driven gain morph with threshold / range /
  attack / release and a band-passed linked sidechain. Zero added latency.
- **Linear-phase EQ** (optional) — static stereo bands rendered to a symmetric
  FIR via the partitioned convolution engine; avoids biquad phase rotation at the
  cost of latency (~171 ms @ 48 kHz). Hybrid: M/S and dynamic bands stay IIR.
- **Per-band audition** — *solo* (bypass every other band) and *listen*
  (type-aware band-pass of the band's region) to hear what a band is doing.

## Convolution / impulse-response loader

Two-stage non-uniform partitioned overlap-save FFT (256-sample head partitions
for fixed low latency + 4096-sample tail partitions spread across the cycle). The
IR is resampled to the DSP rate (gain-compensated), capped at 10 s; a mono IR
broadcasts, multi-channel maps per channel. Load a WAV IR with `resonance ir
<file.wav>`, the GUI Convolution section, or the TUI `I` picker. Persists in
profiles.

## FxSound-matched effects

Effect strength matches **FxSound** so imported `.fac` presets sound like
FxSound. Preset MIDI knob values (`Main N`, 0–127) map to `intensity = MIDI/127`,
then each effect applies FxSound's own scaling. **MUSIC_MODE2 is always active**
(FxSound v13 forces it), which warps Ambience ×0.34 and Dynamic Boost ×1.8 before
their curves.

- **Fidelity** (Aural exciter) — `out = x + 1.5·sin(drive·hp)`, where `hp` is a
  2nd-order Butterworth high-pass at ~4.5 kHz and `drive = intensity · 3.393`.
  Odd-harmonic exciter; the even path is disabled (as in FxSound).
- **Ambience** (Lex reverb via Freeverb) — knob warped `w = intensity·0.34`, comb
  feedback `0.095·10^w` (a short room, max ≈ 0.21), wet 0.273 / dry 0.897.
- **Surround** (Wide32) — full-band mid/side: `gain_side = 1 + 3·(intensity·0.7)`,
  centre compensation `1 − 0.3·(intensity·0.7)`. Bipolar (negative narrows toward
  mono).
- **Dynamic Boost** — loudness maximizer: makeup gain (≤ +12 dB) then a 0.75 ms
  lookahead brickwall limiter holding peaks ≤ 0.9.
- **Bass** — peaking biquad at 90 Hz, Q 2.5, bipolar −15 → +15 dB.

## Extra effects

- **Loudness** — ISO 226:2023 equal-loudness compensation (the "loudness" button
  most consumer EQs have).
- **Crossfeed** — Bauer/Meier headphone crossfeed (one-pole low-pass ~700 Hz on
  the opposite channel, mono-level compensated). Runs last of the effects.
- **Dither** — final-stage TPDF dither to a target bit depth (off by default).
