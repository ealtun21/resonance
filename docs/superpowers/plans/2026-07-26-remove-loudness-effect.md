# Remove Loudness Effect Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete the `Loudness` effect (ISO 226:2023 equal-loudness compensation) entirely — every crate, front-end, and doc reference — per `docs/superpowers/specs/2026-07-26-remove-loudness-effect-design.md`.

**Architecture:** This is pure deletion, no new behavior. The dependency order is `resonance-dsp` (defines `FxEffect`/`LoudnessEffect`) → `resonance-ipc` (defines the serializable `FxEffectId`/`EffectsState` mirror, depends on `resonance-dsp`) and `resonance-apo` (Windows shared-memory `ChainSnapshot`, depends on `resonance-dsp` only) → `resonance-daemon`/`resonance-cli`/`resonance-gui`/`resonance-tui` (all depend on `resonance-ipc`). Each task is scoped to one crate so its own `cargo test -p <crate>` is the gate; the full workspace only recompiles end-to-end once every task lands (expected — the crates in between stay broken until their consumers are fixed in a later task, same as any cross-crate rename).

**Tech Stack:** Rust workspace, `cargo test -p <crate>` per task, final `make check` (fmt --check + clippy -D warnings + test --all) as the last gate.

## Global Constraints

- No `deny_unknown_fields` anywhere in the codebase — old `.toml` profiles carrying `loudness_intensity`/`loudness_enabled` keys keep loading fine with no migration code (confirmed in the design doc).
- The `postcard` IPC wire and the Windows APO shared-memory layout both shift by removing `Loudness` (`Crossfeed`'s discriminant/byte-offset moves) — this is expected and matches every past `STATE_VERSION` bump; no compatibility shim.
- Conventional Commits, all lowercase, no AI-attribution trailer in commit messages (project convention).
- `make check` must stay green throughout (run it as the final Task 8 gate; per-crate `cargo test -p` is the gate for each earlier task).
- Do not touch any other effect, `.fac`/APO preset parsing, or any behavior beyond what's listed below.

---

### Task 1: `resonance-dsp` — delete `LoudnessEffect`, `iso226`, and its tests

**Files:**
- Modify: `crates/resonance-dsp/src/effects.rs` (two non-contiguous ranges: the `iso226` module + `LoudnessEffect` definition, and its own `#[cfg(test)]` module at the end of the file)
- Modify: `crates/resonance-dsp/src/effects_tests.rs` (import line + the Loudness black-box test block)
- Modify: `crates/resonance-dsp/src/chain.rs` (12 reference sites: import, enum variant, `ALL` array, struct field, `process()` call, 3 match-arm blocks, `reset()`, `rebind_sample_rate()`, `set_channels()`, builder `build()`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `FxEffect` (6 variants: Fidelity, Ambience, Surround, DynamicBoost, Bass, Crossfeed) and `FxEffect::ALL: [FxEffect; 6]` — every later task in this plan reads these two exact names/sizes.

- [ ] **Step 1: Confirm the current baseline passes**

```bash
cargo test -p resonance-dsp
```

Expected: `test result: ok. 177 passed; 0 failed; 1 ignored`.

- [ ] **Step 2: Delete the `LoudnessEffect` definition (struct + impls + `iso226` module) from `effects.rs`**

Find (starts right after `impl Effect for BassBoostEffect`'s closing brace, ends right before the Crossfeed section's header comment):

```rust
// ── Loudness compensation (ISO 226:2023 equal-loudness contours) ──────────────
//
// At low listening levels the ear is less sensitive to bass and treble (the
// equal-loudness contours). This effect boosts low + high frequencies, relative
// to 1 kHz, by the contour difference between a reference level (80 phon, the
// "flat" level) and a lower assumed level — so perceived tonal balance stays
// constant as the volume drops. `intensity` drives the assumed level (0 = off /
// flat at the reference; 1 = maximum compensation at LOUDNESS_MIN_PHON), since a
// processing daemon has no OS volume knob to read like a native app would.
// Implemented as a small bank of peaking filters whose gains track the contour,
// rebuilt when intensity (or the rate / channel count) changes.

/// ISO 226:2023 equal-loudness contours. Table 1 coefficients (αf, Lu, Tf) at
/// the 29 preferred one-third-octave frequencies; contour SPL via Formula (1).
mod iso226 {
    pub const FREQS: [f64; 29] = [
        20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0,
        500.0, 630.0, 800.0, 1000.0, 1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0,
        6300.0, 8000.0, 10000.0, 12500.0,
    ];
    const ALPHA: [f64; 29] = [
        0.635, 0.602, 0.569, 0.537, 0.509, 0.482, 0.456, 0.433, 0.412, 0.391, 0.373, 0.357, 0.343,
        0.330, 0.320, 0.311, 0.303, 0.300, 0.295, 0.292, 0.290, 0.290, 0.289, 0.289, 0.289, 0.293,
        0.303, 0.323, 0.354,
    ];
    const LU: [f64; 29] = [
        -31.5, -27.2, -23.1, -19.3, -16.1, -13.1, -10.4, -8.2, -6.3, -4.6, -3.2, -2.1, -1.2, -0.5,
        0.0, 0.4, 0.5, 0.0, -2.7, -4.2, -1.2, 1.4, 2.3, 1.0, -2.3, -7.2, -11.2, -10.9, -3.5,
    ];
    const TF: [f64; 29] = [
        78.1, 68.7, 59.5, 51.1, 44.0, 37.5, 31.5, 26.5, 22.1, 17.9, 14.4, 11.4, 8.6, 6.2, 4.4, 3.0,
        2.2, 2.4, 3.5, 1.7, -1.3, -4.2, -6.0, -5.4, -1.5, 6.0, 12.6, 13.9, 12.3,
    ];
    const REF_IDX: usize = 17; // 1000 Hz
    const ALPHA_R: f64 = 0.300; // reference loudness exponent at 1 kHz
    const REF_P_SQ: f64 = 4e-10;

    /// SPL of the equal-loudness contour at `phon` for each of the 29 frequencies.
    #[must_use]
    pub fn contour_spl(phon: f64) -> [f64; 29] {
        let phon = phon.clamp(20.0, 90.0);
        let tf_ref = TF[REF_IDX];
        let mut out = [0.0f64; 29];
        for i in 0..29 {
            let af = ALPHA[i];
            let excitation = REF_P_SQ.powf(ALPHA_R - af)
                * (10f64.powf(ALPHA_R * phon / 10.0) - 10f64.powf(ALPHA_R * tf_ref / 10.0))
                + 10f64.powf(ALPHA_R * (TF[i] + LU[i]) / 10.0);
            out[i] = (10.0 / af) * excitation.log10() - LU[i];
        }
        out
    }

    /// Per-frequency compensation gain (dB) at `phon` relative to `reference_phon`,
    /// normalized so 1 kHz is 0 dB — we shape balance, not overall level.
    #[must_use]
    pub fn compensation_gains(phon: f64, reference_phon: f64) -> [f64; 29] {
        let r = contour_spl(reference_phon);
        let c = contour_spl(phon);
        let (r1k, c1k) = (r[REF_IDX], c[REF_IDX]);
        let mut g = [0.0f64; 29];
        for i in 0..29 {
            g[i] = (c[i] - c1k) - (r[i] - r1k);
        }
        g
    }
}

/// Reference phon level treated as "flat" (no compensation). 80 phon ≈ 94 dB SPL.
const LOUDNESS_REFERENCE_PHON: f64 = 80.0;
/// Assumed listening level at full intensity; kept well above 20 phon so the
/// compensation gains stay musically moderate.
const LOUDNESS_MIN_PHON: f64 = 40.0;
/// Per-band boost clamp (dB) — keeps bass-heavy content from clipping.
const LOUDNESS_MAX_GAIN_DB: f64 = 12.0;
/// Indices into `iso226::FREQS` used as peaking-filter centers (log-spaced; the
/// 1 kHz reference is omitted since its compensation gain is 0 by definition).
const LOUDNESS_BAND_IDX: [usize; 10] = [0, 2, 5, 8, 11, 14, 20, 23, 26, 28];

#[derive(Debug, Clone)]
struct LoudnessBand {
    coeffs: crate::filter::BiquadCoeffs,
    states: Vec<crate::filter::BiquadState>,
}

#[derive(Debug, Clone)]
pub struct LoudnessEffect {
    intensity: f64,
    enabled: bool,
    sample_rate: f64,
    channels: usize,
    bands: Vec<LoudnessBand>,
}

impl LoudnessEffect {
    #[must_use]
    pub fn new(channels: usize, sample_rate: f64) -> Self {
        let mut e = Self {
            intensity: 0.0,
            enabled: true,
            sample_rate,
            channels: channels.max(1),
            bands: Vec::new(),
        };
        e.rebuild();
        e
    }

    /// Rebuild the peaking-filter bank for the current intensity: the assumed
    /// listening level drops from the reference (intensity 0) toward
    /// `LOUDNESS_MIN_PHON` (intensity 1), and the bank's gains follow the ISO 226
    /// contour difference, clamped, skipping negligible bands.
    fn rebuild(&mut self) {
        self.bands.clear();
        if self.intensity <= 0.0 {
            return;
        }
        let phon = LOUDNESS_REFERENCE_PHON
            - self.intensity * (LOUDNESS_REFERENCE_PHON - LOUDNESS_MIN_PHON);
        let gains = iso226::compensation_gains(phon, LOUDNESS_REFERENCE_PHON);
        for &i in &LOUDNESS_BAND_IDX {
            let gain = gains[i].clamp(-LOUDNESS_MAX_GAIN_DB, LOUDNESS_MAX_GAIN_DB);
            if gain.abs() < 0.1 {
                continue;
            }
            // Q ≈ 1.41 for ~1-octave-spaced peaking bands (matches the graphic EQ).
            if let Ok(coeffs) =
                crate::filter::BiquadCoeffs::peaking(iso226::FREQS[i], gain, 1.41, self.sample_rate)
            {
                self.bands.push(LoudnessBand {
                    coeffs,
                    states: vec![crate::filter::BiquadState::default(); self.channels],
                });
            }
        }
    }
}

impl Effect for LoudnessEffect {
    fn process(&mut self, samples: &mut [f64], channels: usize) {
        if !self.enabled || self.intensity <= 0.0 || self.bands.is_empty() {
            return;
        }
        let frames = samples.len() / channels;
        // Band-major cascade (each peaking biquad makes one pass over the buffer),
        // matching the EQ filter loop — cache-friendly and a proper serial cascade.
        for band in &mut self.bands {
            let chans = channels.min(band.states.len());
            for frame in 0..frames {
                for ch in 0..chans {
                    let idx = frame * channels + ch;
                    samples[idx] = band.states[ch].process(samples[idx], &band.coeffs);
                }
            }
        }
    }

    fn reset(&mut self) {
        for b in &mut self.bands {
            b.states
                .iter_mut()
                .for_each(crate::filter::BiquadState::reset);
        }
    }

    fn set_intensity(&mut self, v: f64) {
        self.intensity = v.clamp(0.0, 1.0);
        self.rebuild();
    }
    fn intensity(&self) -> f64 {
        self.intensity
    }
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }
}

```

Replace with: nothing (delete the whole span above, including its trailing blank line). The file must read directly from `BassBoostEffect`'s closing `}` / one blank line / straight into `// ── Crossfeed — Bauer/Meier headphone crossfeed ───...`.

- [ ] **Step 3: Delete the `loudness_contour_tests` module at the end of `effects.rs`**

Find (this is the very end of the file, right after `impl Effect for CrossfeedEffect`'s closing brace):

```rust

#[cfg(test)]
mod loudness_contour_tests {
    use super::{Effect, LoudnessEffect, iso226};

    #[test]
    fn contour_at_1khz_equals_phon() {
        // By definition the equal-loudness contour at 1 kHz (index 17) for L phon
        // is L dB SPL — a sanity check on the ISO 226 Formula (1) port.
        for &p in &[40.0, 60.0, 80.0] {
            assert!((iso226::contour_spl(p)[17] - p).abs() < 0.01, "phon {p}");
        }
    }

    #[test]
    fn reference_phon_has_zero_compensation() {
        // At the reference level the compensation curve is flat.
        for g in iso226::compensation_gains(80.0, 80.0) {
            assert!(g.abs() < 1e-9);
        }
    }

    #[test]
    fn low_level_boosts_bass_and_treble() {
        let g = iso226::compensation_gains(40.0, 80.0);
        assert!(
            g[2] > 1.0,
            "31.5 Hz should boost at low level, got {}",
            g[2]
        );
        assert!(
            g[28] > 1.0,
            "12.5 kHz should boost at low level, got {}",
            g[28]
        );
        assert!(g[17].abs() < 1e-9, "1 kHz is the reference (0 dB)");
    }

    #[test]
    #[allow(clippy::float_cmp)] // intensity 0 returns early → buffer is bit-identical
    fn intensity_zero_is_passthrough() {
        let mut e = LoudnessEffect::new(2, 48000.0);
        e.set_intensity(0.0);
        let mut buf = vec![0.1, -0.2, 0.3, -0.4];
        let orig = buf.clone();
        e.process(&mut buf, 2);
        assert_eq!(buf, orig);
    }
}
```

Replace with: nothing (the file should now end right after `impl Effect for CrossfeedEffect`'s closing `}`, with no trailing test module).

- [ ] **Step 4: Remove the `LoudnessEffect` import and the Loudness test block from `effects_tests.rs`**

Find:

```rust
use crate::effects::{
    AmbienceEffect, BassBoostEffect, CrossfeedEffect, DynamicBoostEffect, Effect, FidelityEffect,
    LoudnessEffect, SurroundEffect,
};
```

Replace with:

```rust
use crate::effects::{
    AmbienceEffect, BassBoostEffect, CrossfeedEffect, DynamicBoostEffect, Effect, FidelityEffect,
    SurroundEffect,
};
```

Then find (this sits between the Bass tests and the Crossfeed test-section divider):

```rust
// ── Loudness compensation ──────────────────────────────────────────────────────

/// Measured gain (dB) of the loudness effect at `freq` for a given `intensity`.
fn loudness_gain_db_at(intensity: f64, freq: f64) -> f64 {
    let mut e = LoudnessEffect::new(1, SR);
    e.set_intensity(intensity);
    let n = 16384;
    let input = sine(freq, n, 0.4);
    let mut out = input.clone();
    e.process(&mut out, 1);
    let skip = n / 4; // drop the biquad warm-up transient
    20.0 * (rms(&out[skip..]) / rms(&input[skip..])).log10()
}

#[test]
fn loudness_zero_intensity_passthrough() {
    for &f in &[60.0, 1000.0, 10000.0] {
        assert!(
            loudness_gain_db_at(0.0, f).abs() < 0.01,
            "loudness off must pass through at {f} Hz"
        );
    }
}

#[test]
fn loudness_boosts_bass_and_treble_relative_to_mid() {
    let bass = loudness_gain_db_at(1.0, 60.0);
    let mid = loudness_gain_db_at(1.0, 1000.0);
    let treble = loudness_gain_db_at(1.0, 12000.0);
    assert!(
        bass > 3.0,
        "bass should boost at full intensity, got {bass:.1} dB"
    );
    assert!(
        treble > 3.0,
        "treble should boost at full intensity, got {treble:.1} dB"
    );
    assert!(
        bass > mid + 2.0 && treble > mid + 2.0,
        "equal-loudness smile: bass {bass:.1} & treble {treble:.1} dB should exceed mid {mid:.1} dB"
    );
}

#[test]
fn loudness_gain_grows_with_intensity() {
    let low = loudness_gain_db_at(0.3, 60.0);
    let high = loudness_gain_db_at(1.0, 60.0);
    assert!(
        high > low && low > 0.0,
        "more intensity = more bass boost ({low:.1} → {high:.1} dB)"
    );
}

```

Replace with: nothing (the file should read directly from the Bass tests' last closing brace / one blank line / straight into the `// ─────... CROSSFEED ...` divider comment).

- [ ] **Step 5: Fix up `chain.rs` — remove the import, enum variant, `ALL` entry, struct field, and every call site**

Find:

```rust
use crate::effects::{
    AmbienceEffect, BassBoostEffect, CrossfeedEffect, DynamicBoostEffect, Effect, FidelityEffect,
    LoudnessEffect, SurroundEffect,
};
```

Replace with:

```rust
use crate::effects::{
    AmbienceEffect, BassBoostEffect, CrossfeedEffect, DynamicBoostEffect, Effect, FidelityEffect,
    SurroundEffect,
};
```

Find:

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FxEffect {
    Fidelity,
    Ambience,
    Surround,
    DynamicBoost,
    Bass,
    Loudness,
    Crossfeed,
}
```

Replace with:

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FxEffect {
    Fidelity,
    Ambience,
    Surround,
    DynamicBoost,
    Bass,
    Crossfeed,
}
```

Find:

```rust
    pub const ALL: [FxEffect; 7] = [
        FxEffect::Fidelity,
        FxEffect::Ambience,
        FxEffect::Surround,
        FxEffect::DynamicBoost,
        FxEffect::Bass,
        FxEffect::Loudness,
        FxEffect::Crossfeed,
    ];
```

Replace with:

```rust
    pub const ALL: [FxEffect; 6] = [
        FxEffect::Fidelity,
        FxEffect::Ambience,
        FxEffect::Surround,
        FxEffect::DynamicBoost,
        FxEffect::Bass,
        FxEffect::Crossfeed,
    ];
```

Find:

```rust
    pub bass: BassBoostEffect,
    pub loudness: LoudnessEffect,
    pub crossfeed: CrossfeedEffect,
```

Replace with:

```rust
    pub bass: BassBoostEffect,
    pub crossfeed: CrossfeedEffect,
```

Find:

```rust
        self.bass.process(buf, channels);
        self.loudness.process(buf, channels);
        // Crossfeed narrows the final stereo image, so it runs last — after every
        // other effect (including Surround, which widens it) has shaped the sound.
        self.crossfeed.process(buf, channels);
```

Replace with:

```rust
        self.bass.process(buf, channels);
        // Crossfeed narrows the final stereo image, so it runs last — after every
        // other effect (including Surround, which widens it) has shaped the sound.
        self.crossfeed.process(buf, channels);
```

Find:

```rust
    pub fn set_effect_intensity(&mut self, effect: FxEffect, value: f64) {
        match effect {
            FxEffect::Fidelity => self.fidelity.set_intensity(value),
            FxEffect::Ambience => self.ambience.set_intensity(value),
            FxEffect::Surround => self.surround.set_intensity(value),
            FxEffect::DynamicBoost => self.dynamic_boost.set_intensity(value),
            FxEffect::Bass => self.bass.set_intensity(value),
            FxEffect::Loudness => self.loudness.set_intensity(value),
            FxEffect::Crossfeed => self.crossfeed.set_intensity(value),
        }
    }
```

Replace with:

```rust
    pub fn set_effect_intensity(&mut self, effect: FxEffect, value: f64) {
        match effect {
            FxEffect::Fidelity => self.fidelity.set_intensity(value),
            FxEffect::Ambience => self.ambience.set_intensity(value),
            FxEffect::Surround => self.surround.set_intensity(value),
            FxEffect::DynamicBoost => self.dynamic_boost.set_intensity(value),
            FxEffect::Bass => self.bass.set_intensity(value),
            FxEffect::Crossfeed => self.crossfeed.set_intensity(value),
        }
    }
```

Find:

```rust
    pub fn effect_params(&self, effect: FxEffect) -> (f64, bool) {
        match effect {
            FxEffect::Fidelity => (self.fidelity.intensity(), self.fidelity.enabled()),
            FxEffect::Ambience => (self.ambience.intensity(), self.ambience.enabled()),
            FxEffect::Surround => (self.surround.intensity(), self.surround.enabled()),
            FxEffect::DynamicBoost => {
                (self.dynamic_boost.intensity(), self.dynamic_boost.enabled())
            }
            FxEffect::Bass => (self.bass.intensity(), self.bass.enabled()),
            FxEffect::Loudness => (self.loudness.intensity(), self.loudness.enabled()),
            FxEffect::Crossfeed => (self.crossfeed.intensity(), self.crossfeed.enabled()),
        }
    }
```

Replace with:

```rust
    pub fn effect_params(&self, effect: FxEffect) -> (f64, bool) {
        match effect {
            FxEffect::Fidelity => (self.fidelity.intensity(), self.fidelity.enabled()),
            FxEffect::Ambience => (self.ambience.intensity(), self.ambience.enabled()),
            FxEffect::Surround => (self.surround.intensity(), self.surround.enabled()),
            FxEffect::DynamicBoost => {
                (self.dynamic_boost.intensity(), self.dynamic_boost.enabled())
            }
            FxEffect::Bass => (self.bass.intensity(), self.bass.enabled()),
            FxEffect::Crossfeed => (self.crossfeed.intensity(), self.crossfeed.enabled()),
        }
    }
```

Find:

```rust
    pub fn set_effect_enabled(&mut self, effect: FxEffect, on: bool) {
        match effect {
            FxEffect::Fidelity => self.fidelity.set_enabled(on),
            FxEffect::Ambience => self.ambience.set_enabled(on),
            FxEffect::Surround => self.surround.set_enabled(on),
            FxEffect::DynamicBoost => self.dynamic_boost.set_enabled(on),
            FxEffect::Bass => self.bass.set_enabled(on),
            FxEffect::Loudness => self.loudness.set_enabled(on),
            FxEffect::Crossfeed => self.crossfeed.set_enabled(on),
        }
    }
```

Replace with:

```rust
    pub fn set_effect_enabled(&mut self, effect: FxEffect, on: bool) {
        match effect {
            FxEffect::Fidelity => self.fidelity.set_enabled(on),
            FxEffect::Ambience => self.ambience.set_enabled(on),
            FxEffect::Surround => self.surround.set_enabled(on),
            FxEffect::DynamicBoost => self.dynamic_boost.set_enabled(on),
            FxEffect::Bass => self.bass.set_enabled(on),
            FxEffect::Crossfeed => self.crossfeed.set_enabled(on),
        }
    }
```

Find:

```rust
        self.fidelity.reset();
        self.ambience.reset();
        self.surround.reset();
        self.dynamic_boost.reset();
        self.bass.reset();
        self.loudness.reset();
        self.crossfeed.reset();
```

Replace with:

```rust
        self.fidelity.reset();
        self.ambience.reset();
        self.surround.reset();
        self.dynamic_boost.reset();
        self.bass.reset();
        self.crossfeed.reset();
```

Find:

```rust
        self.bass = carry_settings(&self.bass, BassBoostEffect::new(ch, sample_rate));
        self.loudness = carry_settings(&self.loudness, LoudnessEffect::new(ch, sample_rate));
        self.crossfeed = carry_settings(&self.crossfeed, CrossfeedEffect::new(ch, sample_rate));
```

Replace with:

```rust
        self.bass = carry_settings(&self.bass, BassBoostEffect::new(ch, sample_rate));
        self.crossfeed = carry_settings(&self.crossfeed, CrossfeedEffect::new(ch, sample_rate));
```

Find:

```rust
        self.bass = carry_settings(&self.bass, BassBoostEffect::new(channels, sr));
        self.loudness = carry_settings(&self.loudness, LoudnessEffect::new(channels, sr));
        self.crossfeed = carry_settings(&self.crossfeed, CrossfeedEffect::new(channels, sr));
```

Replace with:

```rust
        self.bass = carry_settings(&self.bass, BassBoostEffect::new(channels, sr));
        self.crossfeed = carry_settings(&self.crossfeed, CrossfeedEffect::new(channels, sr));
```

Find:

```rust
            bass: BassBoostEffect::new(channels, sr),
            loudness: LoudnessEffect::new(channels, sr),
            crossfeed: CrossfeedEffect::new(channels, sr),
```

Replace with:

```rust
            bass: BassBoostEffect::new(channels, sr),
            crossfeed: CrossfeedEffect::new(channels, sr),
```

- [ ] **Step 6: Confirm no reference remains, and the suite passes with 7 fewer tests**

```bash
grep -rn "LoudnessEffect\|iso226\|FxEffect::Loudness\|loudness_gain_db_at" crates/resonance-dsp/src/
cargo test -p resonance-dsp
```

Expected: the `grep` prints nothing. The test run shows `test result: ok. 170 passed; 0 failed; 1 ignored` (177 − 7: 4 tests from `loudness_contour_tests` + 3 from `effects_tests.rs`).

- [ ] **Step 7: Commit**

```bash
git add crates/resonance-dsp/src/effects.rs crates/resonance-dsp/src/effects_tests.rs crates/resonance-dsp/src/chain.rs
git commit -m "refactor(dsp): remove loudness effect"
```

---

### Task 2: `resonance-ipc` — delete `FxEffectId::Loudness` and `EffectsState`'s loudness fields

**Files:**
- Modify: `crates/resonance-ipc/src/lib.rs` (enum variant, `ALL` array, `label()`, `From<FxEffectId> for FxEffect`, `EffectsState` struct fields, `get()`/`set()` match arms, 2 test fixtures)

**Interfaces:**
- Consumes: `FxEffect` (6 variants, from Task 1).
- Produces: `FxEffectId` (6 variants: Fidelity, Ambience, Surround, DynamicBoost, Bass, Crossfeed), `FxEffectId::ALL: [FxEffectId; 6]`, and `EffectsState` with no `loudness_intensity`/`loudness_enabled` fields — every later task reads these.

- [ ] **Step 1: Confirm baseline**

```bash
cargo test -p resonance-ipc
```

Expected: `test result: ok. 45 passed; 0 failed; 0 ignored`.

- [ ] **Step 2: Remove the enum variant, `ALL` entry, `label()` arm, and `From` impl arm**

Find:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FxEffectId {
    Fidelity,
    Ambience,
    Surround,
    DynamicBoost,
    Bass,
    Loudness,
    Crossfeed,
}
```

Replace with:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FxEffectId {
    Fidelity,
    Ambience,
    Surround,
    DynamicBoost,
    Bass,
    Crossfeed,
}
```

Find:

```rust
    pub const ALL: [FxEffectId; 7] = [
        FxEffectId::Fidelity,
        FxEffectId::Ambience,
        FxEffectId::Surround,
        FxEffectId::DynamicBoost,
        FxEffectId::Bass,
        FxEffectId::Loudness,
        FxEffectId::Crossfeed,
    ];
```

Replace with:

```rust
    pub const ALL: [FxEffectId; 6] = [
        FxEffectId::Fidelity,
        FxEffectId::Ambience,
        FxEffectId::Surround,
        FxEffectId::DynamicBoost,
        FxEffectId::Bass,
        FxEffectId::Crossfeed,
    ];
```

Find:

```rust
            FxEffectId::Fidelity => "Fidelity",
            FxEffectId::Ambience => "Ambience",
            FxEffectId::Surround => "Surround",
            FxEffectId::DynamicBoost => "Dynamic Boost",
            FxEffectId::Bass => "Bass",
            FxEffectId::Loudness => "Loudness",
            FxEffectId::Crossfeed => "Crossfeed",
```

Replace with:

```rust
            FxEffectId::Fidelity => "Fidelity",
            FxEffectId::Ambience => "Ambience",
            FxEffectId::Surround => "Surround",
            FxEffectId::DynamicBoost => "Dynamic Boost",
            FxEffectId::Bass => "Bass",
            FxEffectId::Crossfeed => "Crossfeed",
```

Find:

```rust
impl From<FxEffectId> for FxEffect {
    fn from(id: FxEffectId) -> Self {
        match id {
            FxEffectId::Fidelity => FxEffect::Fidelity,
            FxEffectId::Ambience => FxEffect::Ambience,
            FxEffectId::Surround => FxEffect::Surround,
            FxEffectId::DynamicBoost => FxEffect::DynamicBoost,
            FxEffectId::Bass => FxEffect::Bass,
            FxEffectId::Loudness => FxEffect::Loudness,
            FxEffectId::Crossfeed => FxEffect::Crossfeed,
        }
    }
}
```

Replace with:

```rust
impl From<FxEffectId> for FxEffect {
    fn from(id: FxEffectId) -> Self {
        match id {
            FxEffectId::Fidelity => FxEffect::Fidelity,
            FxEffectId::Ambience => FxEffect::Ambience,
            FxEffectId::Surround => FxEffect::Surround,
            FxEffectId::DynamicBoost => FxEffect::DynamicBoost,
            FxEffectId::Bass => FxEffect::Bass,
            FxEffectId::Crossfeed => FxEffect::Crossfeed,
        }
    }
}
```

- [ ] **Step 3: Remove the `EffectsState` fields and its `get()`/`set()` match arms**

Find:

```rust
    pub bass_intensity: f64,
    pub bass_enabled: bool,
    // `#[serde(default)]` so self-describing profiles written before Loudness
    // existed still load (default off). The postcard IPC wire is version-locked
    // regardless — clients + daemon ship together.
    #[serde(default)]
    pub loudness_intensity: f64,
    #[serde(default)]
    pub loudness_enabled: bool,
    #[serde(default)]
    pub crossfeed_intensity: f64,
    #[serde(default)]
    pub crossfeed_enabled: bool,
```

Replace with:

```rust
    pub bass_intensity: f64,
    pub bass_enabled: bool,
    #[serde(default)]
    pub crossfeed_intensity: f64,
    #[serde(default)]
    pub crossfeed_enabled: bool,
```

Find:

```rust
            FxEffectId::Fidelity => (self.fidelity_intensity, self.fidelity_enabled),
            FxEffectId::Ambience => (self.ambience_intensity, self.ambience_enabled),
            FxEffectId::Surround => (self.surround_intensity, self.surround_enabled),
            FxEffectId::DynamicBoost => (self.dynamic_boost_intensity, self.dynamic_boost_enabled),
            FxEffectId::Bass => (self.bass_intensity, self.bass_enabled),
            FxEffectId::Loudness => (self.loudness_intensity, self.loudness_enabled),
            FxEffectId::Crossfeed => (self.crossfeed_intensity, self.crossfeed_enabled),
```

Replace with:

```rust
            FxEffectId::Fidelity => (self.fidelity_intensity, self.fidelity_enabled),
            FxEffectId::Ambience => (self.ambience_intensity, self.ambience_enabled),
            FxEffectId::Surround => (self.surround_intensity, self.surround_enabled),
            FxEffectId::DynamicBoost => (self.dynamic_boost_intensity, self.dynamic_boost_enabled),
            FxEffectId::Bass => (self.bass_intensity, self.bass_enabled),
            FxEffectId::Crossfeed => (self.crossfeed_intensity, self.crossfeed_enabled),
```

Find:

```rust
            FxEffectId::Loudness => {
                self.loudness_intensity = intensity;
                self.loudness_enabled = enabled;
            }
            FxEffectId::Bass => {
                self.bass_intensity = intensity;
                self.bass_enabled = enabled;
            }
```

Replace with:

```rust
            FxEffectId::Bass => {
                self.bass_intensity = intensity;
                self.bass_enabled = enabled;
            }
```

(Note: this keeps `Bass`'s arm and only removes the `Loudness` arm that sat above it — `set()`'s match arms aren't in `ALL`-order, `Loudness` was between `DynamicBoost` and `Bass`.)

- [ ] **Step 4: Remove the two test fixtures' loudness fields**

Find:

```rust
                bass_intensity: 0.0,
                bass_enabled: false,
                loudness_intensity: 0.0,
                loudness_enabled: false,
                crossfeed_intensity: 0.3,
                crossfeed_enabled: true,
```

Replace with:

```rust
                bass_intensity: 0.0,
                bass_enabled: false,
                crossfeed_intensity: 0.3,
                crossfeed_enabled: true,
```

Find:

```rust
                bass_intensity: -1.0,
                bass_enabled: true,
                loudness_intensity: 0.6,
                loudness_enabled: true,
                crossfeed_intensity: 0.4,
                crossfeed_enabled: true,
```

Replace with:

```rust
                bass_intensity: -1.0,
                bass_enabled: true,
                crossfeed_intensity: 0.4,
                crossfeed_enabled: true,
```

- [ ] **Step 5: Verify**

```bash
grep -n "FxEffectId::Loudness\|loudness_intensity\|loudness_enabled" crates/resonance-ipc/src/lib.rs
cargo test -p resonance-ipc
```

Expected: `grep` prints nothing; `test result: ok. 45 passed; 0 failed; 0 ignored` (same count — only fixture fields changed, no tests added/removed).

- [ ] **Step 6: Commit**

```bash
git add crates/resonance-ipc/src/lib.rs
git commit -m "refactor(ipc): remove loudness effect id and state"
```

---

### Task 3: `resonance-apo` — remove `ChainSnapshot.loudness`, bump `STATE_VERSION` 9→10

**Files:**
- Modify: `crates/resonance-apo/src/state.rs` (version doc-comment + constant, `ChainSnapshot` field, its `Default` impl, the `effect()` helper, `from_chain()`, and the shared `build_chain()`/`apply_to()` per-effect block)

**Interfaces:**
- Consumes: `FxEffect` (6 variants, from Task 1).
- Produces: `ChainSnapshot` with no `loudness` field, `STATE_VERSION == 10`. The daemon (Task 4) and the Windows APO DLL must be rebuilt together after this — same requirement as every prior version bump.

- [ ] **Step 1: Confirm baseline**

```bash
cargo test -p resonance-apo -- --test-threads=1
```

Expected: `test result: ok. 24 passed; 0 failed; 0 ignored` (this crate's `hires_harness` shares global state across tests — always run with `--test-threads=1` or you'll see false failures unrelated to this change).

- [ ] **Step 2: Bump the version doc-comment and constant**

Find:

```rust
/// Layout version; bump on any `#[repr(C)]` change below.
/// v3: per-band channel mask + square routing matrix.
/// v4: + Loudness effect snapshot.
/// v5: + convolution (enabled flag + IR-blob generation; samples live in the
///     sidecar blob file, see [`default_ir_path`]).
/// v6: + per-band dynamic EQ (enabled flag + threshold/range/attack/release).
/// v7: + linear-phase EQ mode flag.
/// v8: + transient per-band solo (audition one band; `SOLO_NONE` = off).
/// v9: + audition mode (solo/listen) beside the `solo_band` index.
pub const STATE_VERSION: u32 = 9;
```

Replace with:

```rust
/// Layout version; bump on any `#[repr(C)]` change below.
/// v3: per-band channel mask + square routing matrix.
/// v4: + Loudness effect snapshot.
/// v5: + convolution (enabled flag + IR-blob generation; samples live in the
///     sidecar blob file, see [`default_ir_path`]).
/// v6: + per-band dynamic EQ (enabled flag + threshold/range/attack/release).
/// v7: + linear-phase EQ mode flag.
/// v8: + transient per-band solo (audition one band; `SOLO_NONE` = off).
/// v9: + audition mode (solo/listen) beside the `solo_band` index.
/// v10: − Loudness effect removed (buggy, redundant with Dynamic Boost).
pub const STATE_VERSION: u32 = 10;
```

(The `v4` history line stays — it documents what that past layout version added, same as every other version note here; only `v10` is new and the constant changes.)

- [ ] **Step 3: Remove the `ChainSnapshot` field and its `Default` entry**

Find:

```rust
    pub bass: EffectSnapshot,
    pub loudness: EffectSnapshot,
    pub crossfeed: EffectSnapshot,
```

Replace with:

```rust
    pub bass: EffectSnapshot,
    pub crossfeed: EffectSnapshot,
```

Find:

```rust
            bass: EffectSnapshot::default(),
            loudness: EffectSnapshot::default(),
            crossfeed: EffectSnapshot::default(),
```

Replace with:

```rust
            bass: EffectSnapshot::default(),
            crossfeed: EffectSnapshot::default(),
```

- [ ] **Step 4: Remove the `effect()` helper's match arm and `from_chain()`'s field**

Find:

```rust
        FxEffect::Bass => (chain.bass.intensity(), chain.bass.enabled()),
        FxEffect::Loudness => (chain.loudness.intensity(), chain.loudness.enabled()),
        FxEffect::Crossfeed => (chain.crossfeed.intensity(), chain.crossfeed.enabled()),
    };
```

Replace with:

```rust
        FxEffect::Bass => (chain.bass.intensity(), chain.bass.enabled()),
        FxEffect::Crossfeed => (chain.crossfeed.intensity(), chain.crossfeed.enabled()),
    };
```

Find:

```rust
            bass: effect(chain, FxEffect::Bass),
            loudness: effect(chain, FxEffect::Loudness),
            crossfeed: effect(chain, FxEffect::Crossfeed),
```

Replace with:

```rust
            bass: effect(chain, FxEffect::Bass),
            crossfeed: effect(chain, FxEffect::Crossfeed),
```

- [ ] **Step 5: Remove the shared per-effect block from BOTH `build_chain()` and `apply_to()` in one pass**

`build_chain()` and `apply_to()` each contain a byte-identical 7-line tail (Bass → Crossfeed → `set_dither`) — use `replace_all` so both call sites are fixed together.

Find (`replace_all: true`):

```rust
        chain.set_effect_intensity(FxEffect::Bass, self.bass.intensity);
        chain.set_effect_enabled(FxEffect::Bass, self.bass.enabled != 0);
        chain.set_effect_intensity(FxEffect::Loudness, self.loudness.intensity);
        chain.set_effect_enabled(FxEffect::Loudness, self.loudness.enabled != 0);
        chain.set_effect_intensity(FxEffect::Crossfeed, self.crossfeed.intensity);
        chain.set_effect_enabled(FxEffect::Crossfeed, self.crossfeed.enabled != 0);
        chain.set_dither((self.dither_bits != 0).then_some(self.dither_bits));
```

Replace with:

```rust
        chain.set_effect_intensity(FxEffect::Bass, self.bass.intensity);
        chain.set_effect_enabled(FxEffect::Bass, self.bass.enabled != 0);
        chain.set_effect_intensity(FxEffect::Crossfeed, self.crossfeed.intensity);
        chain.set_effect_enabled(FxEffect::Crossfeed, self.crossfeed.enabled != 0);
        chain.set_dither((self.dither_bits != 0).then_some(self.dither_bits));
```

- [ ] **Step 6: Verify**

```bash
grep -n "Loudness" crates/resonance-apo/src/state.rs
cargo test -p resonance-apo -- --test-threads=1
```

Expected: `grep` prints exactly 2 lines — the `v4: + Loudness effect snapshot.` and `v10: − Loudness effect removed...` history-comment lines, nothing else. Test run: `test result: ok. 24 passed; 0 failed; 0 ignored` (unchanged count; this crate has no dedicated Loudness test to remove).

- [ ] **Step 7: Commit**

```bash
git add crates/resonance-apo/src/state.rs
git commit -m "refactor(apo): remove loudness from chain snapshot, bump state version to 10"
```

---

### Task 4: `resonance-daemon` — drop the loudness default in `Profile::from_preset`

**Files:**
- Modify: `crates/resonance-daemon/src/config.rs`

**Interfaces:**
- Consumes: `EffectsState` with no `loudness_intensity`/`loudness_enabled` fields (Task 2).

- [ ] **Step 1: Confirm baseline**

```bash
cargo test -p resonance-daemon
```

Expected: `test result: ok. 66 passed; 0 failed; 0 ignored`.

- [ ] **Step 2: Remove the fields and their comment**

Find:

```rust
                bass_intensity: e.bass.intensity,
                bass_enabled: e.bass.enabled,
                // Loudness is a Resonance-native effect with no FxSound/APO preset
                // equivalent, so it starts off for preset-derived profiles.
                loudness_intensity: 0.0,
                loudness_enabled: false,
                // Crossfeed likewise has no preset equivalent — off by default.
                crossfeed_intensity: 0.0,
                crossfeed_enabled: false,
```

Replace with:

```rust
                bass_intensity: e.bass.intensity,
                bass_enabled: e.bass.enabled,
                // Crossfeed has no preset equivalent either — off by default.
                crossfeed_intensity: 0.0,
                crossfeed_enabled: false,
```

- [ ] **Step 3: Verify**

```bash
grep -n -i "loudness" crates/resonance-daemon/src/config.rs
cargo test -p resonance-daemon
```

Expected: `grep` prints nothing; `test result: ok. 66 passed; 0 failed; 0 ignored`.

- [ ] **Step 4: Commit**

```bash
git add crates/resonance-daemon/src/config.rs
git commit -m "refactor(daemon): drop loudness default from preset-derived profiles"
```

---

### Task 5: `resonance-cli` — remove `loudness` from the effect name parser/printer

**Files:**
- Modify: `crates/resonance-cli/src/main.rs` (`Set` command doc-comment, `effect_cli_name()`, `parse_effect()`, one stale comment)

**Interfaces:**
- Consumes: `FxEffectId` (6 variants, from Task 2).

- [ ] **Step 1: Confirm baseline**

```bash
cargo test -p resonance-cli
```

Expected: `test result: ok. 32 passed; 0 failed; 0 ignored`.

- [ ] **Step 2: Update the `Set` command's doc-comment**

Find:

```rust
        /// Effect: fidelity / ambience / surround / `dynamic_boost` / bass / loudness / crossfeed
```

Replace with:

```rust
        /// Effect: fidelity / ambience / surround / `dynamic_boost` / bass / crossfeed
```

- [ ] **Step 3: Remove the stale "(Loudness, Crossfeed, …)" example in a comment**

Find:

```rust
    // Effects with intensity bars. Iterate `FxEffectId::ALL` so new effects
    // (Loudness, Crossfeed, …) show up automatically and stay in chain order.
```

Replace with:

```rust
    // Effects with intensity bars. Iterate `FxEffectId::ALL` so new effects
    // (Crossfeed, …) show up automatically and stay in chain order.
```

- [ ] **Step 4: Remove `Loudness` from `effect_cli_name()`**

Find:

```rust
fn effect_cli_name(id: FxEffectId) -> &'static str {
    match id {
        FxEffectId::Fidelity => "fidelity",
        FxEffectId::Ambience => "ambience",
        FxEffectId::Surround => "surround",
        FxEffectId::DynamicBoost => "dynamic_boost",
        FxEffectId::Bass => "bass",
        FxEffectId::Loudness => "loudness",
        FxEffectId::Crossfeed => "crossfeed",
    }
}
```

Replace with:

```rust
fn effect_cli_name(id: FxEffectId) -> &'static str {
    match id {
        FxEffectId::Fidelity => "fidelity",
        FxEffectId::Ambience => "ambience",
        FxEffectId::Surround => "surround",
        FxEffectId::DynamicBoost => "dynamic_boost",
        FxEffectId::Bass => "bass",
        FxEffectId::Crossfeed => "crossfeed",
    }
}
```

- [ ] **Step 5: Remove `"loudness"` from `parse_effect()`**

Find:

```rust
fn parse_effect(s: &str) -> Result<FxEffectId> {
    match s {
        "fidelity" => Ok(FxEffectId::Fidelity),
        "ambience" => Ok(FxEffectId::Ambience),
        "surround" => Ok(FxEffectId::Surround),
        "dynamic_boost" | "dynamic" => Ok(FxEffectId::DynamicBoost),
        "bass" => Ok(FxEffectId::Bass),
        "loudness" => Ok(FxEffectId::Loudness),
        "crossfeed" => Ok(FxEffectId::Crossfeed),
        _ => bail!(
            "unknown effect '{s}': use fidelity/ambience/surround/dynamic_boost/bass/loudness/crossfeed"
        ),
    }
}
```

Replace with:

```rust
fn parse_effect(s: &str) -> Result<FxEffectId> {
    match s {
        "fidelity" => Ok(FxEffectId::Fidelity),
        "ambience" => Ok(FxEffectId::Ambience),
        "surround" => Ok(FxEffectId::Surround),
        "dynamic_boost" | "dynamic" => Ok(FxEffectId::DynamicBoost),
        "bass" => Ok(FxEffectId::Bass),
        "crossfeed" => Ok(FxEffectId::Crossfeed),
        _ => bail!(
            "unknown effect '{s}': use fidelity/ambience/surround/dynamic_boost/bass/crossfeed"
        ),
    }
}
```

- [ ] **Step 6: Verify**

```bash
grep -n -i "loudness" crates/resonance-cli/src/main.rs
cargo test -p resonance-cli
```

Expected: `grep` prints nothing; `test result: ok. 32 passed; 0 failed; 0 ignored`.

- [ ] **Step 7: Commit**

```bash
git add crates/resonance-cli/src/main.rs
git commit -m "refactor(cli): remove loudness from the effect name parser"
```

---

### Task 6: `resonance-gui` — drop loudness from the demo state fixture

**Files:**
- Modify: `crates/resonance-gui/src/app.rs` (`demo_state()`'s `EffectsState` literal — the only Loudness reference in this crate; no other GUI code special-cases any effect by name)

**Interfaces:**
- Consumes: `EffectsState` with no `loudness_intensity`/`loudness_enabled` fields (Task 2).

- [ ] **Step 1: Confirm baseline**

```bash
cargo test -p resonance-gui
```

Expected: `test result: ok. 51 passed; 0 failed; 0 ignored`.

- [ ] **Step 2: Remove the two fields**

Find:

```rust
            bass_intensity: 0.71,
            bass_enabled: true,
            loudness_intensity: 0.4,
            loudness_enabled: true,
            crossfeed_intensity: 0.25,
            crossfeed_enabled: true,
```

Replace with:

```rust
            bass_intensity: 0.71,
            bass_enabled: true,
            crossfeed_intensity: 0.25,
            crossfeed_enabled: true,
```

- [ ] **Step 3: Verify**

```bash
grep -n -i "loudness" crates/resonance-gui/src/app.rs
cargo test -p resonance-gui
```

Expected: `grep` prints nothing; `test result: ok. 51 passed; 0 failed; 0 ignored`.

- [ ] **Step 4: Commit**

```bash
git add crates/resonance-gui/src/app.rs
git commit -m "refactor(gui): remove loudness from the demo state fixture"
```

---

### Task 7: `resonance-tui` — drop `"Loudness"` from `EFFECT_NAMES`

**Files:**
- Modify: `crates/resonance-tui/src/app.rs` (`EFFECT_NAMES` — the only Loudness reference in this crate; the array is already sized off `FxEffectId::ALL.len()`, so no separate length constant needs updating)

**Interfaces:**
- Consumes: `FxEffectId::ALL` (6 entries, from Task 2).

- [ ] **Step 1: Confirm baseline**

```bash
cargo test -p resonance-tui
```

Expected: `test result: ok. 53 passed; 0 failed; 0 ignored`.

- [ ] **Step 2: Remove the entry**

Find:

```rust
pub const EFFECT_NAMES: [&str; FxEffectId::ALL.len()] = [
    "Fidelity",
    "Ambience",
    "Surround",
    "Dyn Boost",
    "Bass",
    "Loudness",
    "Crossfeed",
];
```

Replace with:

```rust
pub const EFFECT_NAMES: [&str; FxEffectId::ALL.len()] = [
    "Fidelity",
    "Ambience",
    "Surround",
    "Dyn Boost",
    "Bass",
    "Crossfeed",
];
```

- [ ] **Step 3: Verify**

```bash
grep -n -i "loudness" crates/resonance-tui/src/app.rs
cargo test -p resonance-tui
```

Expected: `grep` prints nothing; `test result: ok. 53 passed; 0 failed; 0 ignored`.

- [ ] **Step 4: Commit**

```bash
git add crates/resonance-tui/src/app.rs
git commit -m "refactor(tui): remove loudness from the effects column"
```

---

### Task 8: Docs cleanup + final workspace verification

**Files:**
- Modify: `docs/ROADMAP.md` (drop the loudness-compensation bullet)
- Modify: `CLAUDE.md` (local-only, gitignored via `.git/info/exclude` — NOT committed; two mentions to remove)

**Interfaces:**
- Consumes: nothing (docs only).
- Produces: a fully green `make check` across the whole workspace — the final proof this refactor is complete.

- [ ] **Step 1: Remove the ROADMAP.md bullet**

Find:

```markdown
effect emulation (Fidelity/Ambience/Surround/Dynamic Boost/Bass);
**loudness compensation (ISO 226:2023 equal-loudness)** — the "loudness" button
most consumer EQs have; **headphone crossfeed** (Bauer/Meier); **adjustable
```

Replace with:

```markdown
effect emulation (Fidelity/Ambience/Surround/Dynamic Boost/Bass);
**headphone crossfeed** (Bauer/Meier); **adjustable
```

- [ ] **Step 2: Remove the two CLAUDE.md mentions (local file, do not `git add`/commit it)**

Find:

```
  ├── Bass       (peaking biquad 90 Hz, Q 2.5, bipolar −15 → +15 dB)
  ├── Loudness   (ISO 226:2023 equal-loudness compensation)
  ├── Crossfeed  (Bauer/Meier headphone crossfeed: one-pole LP ~700 Hz on the
```

Replace with:

```
  ├── Bass       (peaking biquad 90 Hz, Q 2.5, bipolar −15 → +15 dB)
  ├── Crossfeed  (Bauer/Meier headphone crossfeed: one-pole LP ~700 Hz on the
```

Find:

```
>   future item — see `docs/ROADMAP.md`. See [[per-app-volume-feature]].
> - **Loudness compensation** — ISO 226:2023 equal-loudness effect (`FxEffect::Loudness`).
> - **Advanced-feature visibility settings** — per-feature toggles (slope, scope,
```

Replace with:

```
>   future item — see `docs/ROADMAP.md`. See [[per-app-volume-feature]].
> - **Advanced-feature visibility settings** — per-feature toggles (slope, scope,
```

- [ ] **Step 3: Repo-wide check — only the two intentional historical-comment mentions in `resonance-apo` remain**

```bash
grep -rn -i "loudness" --include="*.rs" crates/
grep -n -i "loudness" docs/ROADMAP.md
```

Expected: the first command prints exactly 2 lines, both in `crates/resonance-apo/src/state.rs` (the `v4:`/`v10:` history-comment lines from Task 3); the second command prints nothing.

- [ ] **Step 4: Full workspace verification**

```bash
make check
```

Expected: `cargo fmt --all -- --check` clean, `cargo clippy --all-targets --all-features -- -D warnings` clean, `cargo test --all` all green (170 dsp + 45 ipc + 24 apo + 66 daemon + 32 cli + 51 gui + 53 tui + unchanged counts for `resonance-preset`/`resonance-reference`/`resonance-autoeq`/`resonance-tray`).

- [ ] **Step 5: Commit the ROADMAP.md change**

```bash
git add docs/ROADMAP.md
git commit -m "docs: remove loudness effect from the roadmap"
```

(`CLAUDE.md` stays edited on disk but is never staged — it's gitignored per `.git/info/exclude`.)
