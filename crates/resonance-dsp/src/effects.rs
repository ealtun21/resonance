use std::f64::consts::PI;

pub trait Effect: Send {
    fn process(&mut self, samples: &mut [f64], channels: usize);
    fn reset(&mut self);
    fn set_intensity(&mut self, value: f64);
    fn intensity(&self) -> f64;
    fn enabled(&self) -> bool;
    fn set_enabled(&mut self, on: bool);
}

// ── Shared helpers ────────────────────────────────────────────────────────────

/// 2nd-order Butterworth HP biquad coefficients (Audio EQ Cookbook).
/// Returns (b0, b1, b2, a1, a2) already divided by a0.
fn butterworth_hp(fc: f64, sr: f64) -> (f64, f64, f64, f64, f64) {
    // Keep the corner below Nyquist: at sample rates under ~2·fc the raw corner
    // exceeds π rad/sample and the biquad becomes unstable, eventually emitting
    // NaN. Clamp to 0.45·sr so the HP stays well-formed at any negotiated rate.
    let fc = fc.min(0.45 * sr);
    let w0 = 2.0 * PI * fc / sr;
    let cos_w = w0.cos();
    let sin_w = w0.sin();
    let alpha = sin_w / (2.0_f64.sqrt()); // Q = 1/√2 (Butterworth)
    let a0 = 1.0 + alpha;
    (
        f64::midpoint(1.0, cos_w) / a0,
        (-(1.0 + cos_w)) / a0,
        f64::midpoint(1.0, cos_w) / a0,
        (-2.0 * cos_w) / a0,
        (1.0 - alpha) / a0,
    )
}

/// Transposed direct-form II biquad tick. State = [s1, s2].
#[inline]
fn biquad_tick(x: f64, state: &mut [f64; 2], b0: f64, b1: f64, b2: f64, a1: f64, a2: f64) -> f64 {
    let y = b0 * x + state[0];
    state[0] = b1 * x - a1 * y + state[1];
    state[1] = b2 * x - a2 * y;
    y
}

// ── Fidelity — FxSound Aural exciter ──────────────────────────────────────────
//
// Matches FxSound's `dspsAural`: a 2nd-order Butterworth HP isolates the high
// band, which is driven and added back via an odd-harmonic generator.
//   hp   = butterworth_hp(x)            (crossover MIDI 53 → ~4465 Hz)
//   out  = x + AURAL_ODD · sin(drive · hp)
// `drive` scales 0 → 3.393 with intensity; the odd-mix coefficient is FIXED at
// 1.5 (DSP_AURAL_ODD_MAX = 1.0 · DSP_AURAL_WET_BOOST). The even path is disabled
// in FxSound (DSP_AURAL_EVEN_MAX_VALUE = 0), so there is none here — the old
// even/DC-block path made it sound stronger and muddier than FxSound.

const AURAL_HP_HZ: f64 = 4465.0; // DSP_PLAY_AURAL_TUNE_MIDI 53 on [500, 10000] Hz
// DSP_AURAL_DRIVE_MAX_VALUE · PLY_FIDELITY_INTENSITY_MAX_SCALE
//   = (2π/4 · 1.8 · 2 · 0.75) · 0.8 ≈ 3.393
const AURAL_DRIVE_MAX: f64 = (PI / 2.0) * 1.8 * 2.0 * 0.75 * 0.8;
const AURAL_ODD: f64 = 1.5; // fixed odd-harmonic mix (DSP_AURAL_ODD_MAX)

#[derive(Debug, Clone)]
pub struct FidelityEffect {
    intensity: f64,
    enabled: bool,
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    states: Vec<[f64; 2]>,
}

impl FidelityEffect {
    #[must_use]
    pub fn new(channels: usize, sample_rate: f64) -> Self {
        let (b0, b1, b2, a1, a2) = butterworth_hp(AURAL_HP_HZ, sample_rate);
        Self {
            intensity: 0.0,
            enabled: true,
            b0,
            b1,
            b2,
            a1,
            a2,
            states: vec![[0.0; 2]; channels],
        }
    }
}

impl Effect for FidelityEffect {
    fn process(&mut self, samples: &mut [f64], channels: usize) {
        if !self.enabled || self.intensity == 0.0 {
            return;
        }

        let drive = self.intensity * AURAL_DRIVE_MAX;
        let frames = samples.len() / channels;

        for frame in 0..frames {
            for ch in 0..channels {
                let idx = frame * channels + ch;
                let x = samples[idx];
                let hp = biquad_tick(
                    x,
                    &mut self.states[ch],
                    self.b0,
                    self.b1,
                    self.b2,
                    self.a1,
                    self.a2,
                );
                let odd = (drive * hp).sin();
                samples[idx] = (x + AURAL_ODD * odd).clamp(-1.0, 1.0);
            }
        }
    }

    fn reset(&mut self) {
        self.states.iter_mut().for_each(|s| *s = [0.0; 2]);
    }
    fn set_intensity(&mut self, v: f64) {
        self.intensity = v.clamp(0.0, 1.0);
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

// ── Ambience — Freeverb (Schroeder–Moorer) reverb ─────────────────────────────
//
// The classic, well-behaved public-domain reverb: 8 parallel damped comb
// filters per channel feeding 4 series allpass diffusers, with the right
// channel's delays offset by `STEREO_SPREAD` for a wide stereo image.
// The previous version used very short combs (4–8 ms) that produced an
// audible metallic flutter; Freeverb's tunings (25–37 ms) sound smooth.
//
// Wet/dry, decay and the allpass (lat6) coefficient follow FxSound's `dfxp_
// CommunicateAmbience` exactly (the Freeverb topology stands in for the Lex
// reverb, but the gains/decay match so levels track FxSound).
//
// FxSound only ever runs MUSIC_MODE2 (the only mode since DFX v13), which warps
// the ambience knob ×0.34 (DFX_MUSIC_MODE2_AMBIENCE_FACTOR) *before* the decay
// and wet/dry curves are computed. The bypass test, however, runs on the RAW
// knob value (dfxp_CommAmbienceBypass: raw MIDI ≤ 12 → silent). So:
//   warped = intensity · 0.34
//   decay  = 0.095 · 10^warped  (≤ ~0.21, so a short room) → comb feedback
//   lat6   = clamp(decay + 0.15, 0.25, 0.50)               → allpass feedback
//   warped·127 > 40 → wet 0.273, dry 0.897 (fixed)
//   warped·127 ≤ 40 → wet/dry warp toward dry-only (wet → 0)
//   raw intensity ≤ 12/127 → bypass
// Applying the ×0.34 warp is what keeps ambience subtle, as in FxSound; without
// it the reverb tail and wet level were several times too strong.

const AMBIENCE_BYPASS_THRESHOLD: f64 = 12.0 / 127.0; // ≈ 0.0945 (raw knob)
const MUSIC_MODE2_AMBIENCE_FACTOR: f64 = 0.34; // DFX_MUSIC_MODE2_AMBIENCE_FACTOR
const COMB_TUNING_44K: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASS_TUNING_44K: [usize; 4] = [556, 441, 341, 225];
const STEREO_SPREAD_44K: usize = 23;
const COMB_DAMP: f64 = 0.25;
const AMBIENCE_WET: f64 = 0.21 * 1.3; // 0.273
const AMBIENCE_DRY: f64 = 0.69 * 1.3; // 0.897

#[derive(Debug, Clone)]
struct Comb {
    buf: Vec<f64>,
    idx: usize,
    filter_store: f64,
}

impl Comb {
    fn new(delay: usize) -> Self {
        Self {
            buf: vec![0.0; delay.max(1)],
            idx: 0,
            filter_store: 0.0,
        }
    }

    #[inline]
    fn tick(&mut self, input: f64, feedback: f64, damp: f64) -> f64 {
        let out = self.buf[self.idx];
        self.filter_store = out * (1.0 - damp) + self.filter_store * damp;
        self.buf[self.idx] = input + self.filter_store * feedback;
        self.idx += 1;
        if self.idx >= self.buf.len() {
            self.idx = 0;
        }
        out
    }

    fn reset(&mut self) {
        self.buf.iter_mut().for_each(|x| *x = 0.0);
        self.idx = 0;
        self.filter_store = 0.0;
    }
}

#[derive(Debug, Clone)]
struct Allpass {
    buf: Vec<f64>,
    idx: usize,
}

impl Allpass {
    fn new(delay: usize) -> Self {
        Self {
            buf: vec![0.0; delay.max(1)],
            idx: 0,
        }
    }

    #[inline]
    fn tick(&mut self, input: f64, feedback: f64) -> f64 {
        let buf_out = self.buf[self.idx];
        let output = -input + buf_out;
        self.buf[self.idx] = input + buf_out * feedback;
        self.idx += 1;
        if self.idx >= self.buf.len() {
            self.idx = 0;
        }
        output
    }

    fn reset(&mut self) {
        self.buf.iter_mut().for_each(|x| *x = 0.0);
        self.idx = 0;
    }
}

#[derive(Debug, Clone)]
pub struct AmbienceEffect {
    intensity: f64,
    enabled: bool,
    combs: Vec<[Comb; 8]>,
    allpasses: Vec<[Allpass; 4]>,
}

impl AmbienceEffect {
    #[must_use]
    pub fn new(channels: usize, sample_rate: f64) -> Self {
        let scale = sample_rate / 44100.0;
        let combs = (0..channels)
            .map(|ch| {
                let spread = STEREO_SPREAD_44K * (ch & 1);
                std::array::from_fn(|i| {
                    Comb::new((((COMB_TUNING_44K[i] + spread) as f64) * scale) as usize)
                })
            })
            .collect();
        let allpasses = (0..channels)
            .map(|ch| {
                let spread = STEREO_SPREAD_44K * (ch & 1);
                std::array::from_fn(|i| {
                    Allpass::new((((ALLPASS_TUNING_44K[i] + spread) as f64) * scale) as usize)
                })
            })
            .collect();
        Self {
            intensity: 0.0,
            enabled: true,
            combs,
            allpasses,
        }
    }
}

impl Effect for AmbienceEffect {
    fn process(&mut self, samples: &mut [f64], channels: usize) {
        if !self.enabled || self.intensity == 0.0 || self.intensity < AMBIENCE_BYPASS_THRESHOLD {
            return;
        }

        // MUSIC_MODE2 warps the knob ×0.34 (always on in FxSound v13) before the
        // decay and wet/dry curves; the bypass above stays on the raw knob.
        let warped = self.intensity * MUSIC_MODE2_AMBIENCE_FACTOR;

        // Decay (comb feedback) on the FxSound exponential curve; room size
        // MIDI 64 ≈ exponent 1.0, so decay^roomsize ≈ decay.
        let decay = (0.095 * 10.0_f64.powf(warped)).min(0.95);
        let lat6 = (decay + 0.15).clamp(0.25, 0.50);

        // Wet/dry warp from FxSound (MIDI scale, on the warped knob value).
        let midi = warped * 127.0;
        let (wet, dry) = if midi > 40.0 {
            (AMBIENCE_WET, AMBIENCE_DRY)
        } else {
            let t = (midi - 12.0) / (40.0 - 12.0); // 0..1 across the warp band
            (
                t * AMBIENCE_WET,
                AMBIENCE_DRY + (1.0 - t) * (1.0 - AMBIENCE_DRY),
            )
        };
        let frames = samples.len() / channels;

        for frame in 0..frames {
            for ch in 0..channels {
                let idx = frame * channels + ch;
                let x = samples[idx];

                let mut acc = 0.0;
                for comb in &mut self.combs[ch] {
                    acc += comb.tick(x, decay, COMB_DAMP);
                }
                acc *= 1.0 / 8.0;

                for ap in &mut self.allpasses[ch] {
                    acc = ap.tick(acc, lat6);
                }

                samples[idx] = dry * x + wet * acc;
            }
        }
    }

    fn reset(&mut self) {
        for set in &mut self.combs {
            set.iter_mut().for_each(Comb::reset);
        }
        for set in &mut self.allpasses {
            set.iter_mut().for_each(Allpass::reset);
        }
    }

    fn set_intensity(&mut self, v: f64) {
        self.intensity = v.clamp(0.0, 1.0);
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

// ── Surround — FxSound mid/side stereo widener ────────────────────────────────
//
// Matches FxSound's `Wide32`: a full-band mid/side widener with mono-level
// compensation (no HP filter — the previous bass-protected version widened
// differently and sounded stronger).
//   scaled = intensity · 0.7              (PLY_WIDENER_BOOST_MAX_SCALE)
//   gain_side = 1 + 3·scaled              (FxSound: 1 + 3·intensity)
//   comp      = 1 − 0.3·scaled            (reduces centre as width increases)
//   out = mono·comp + gain_side·(in − mono)
// Bipolar: negative intensity narrows toward mono (gain_side clamped ≥ 0).
// At intensity 0 the path is a bit-exact bypass.
//
// Stereo only: mid/side is defined for an L/R pair. For any non-stereo layout
// (mono, 5.1, 7.1, …) the effect passes through unchanged rather than guess how
// to widen a multichannel bed — there is no meaningful L/R pair to operate on.

const SURROUND_INTENSITY_SCALE: f64 = 0.7;

#[derive(Debug, Clone)]
pub struct SurroundEffect {
    intensity: f64,
    enabled: bool,
}

impl SurroundEffect {
    #[must_use]
    pub fn new(_sample_rate: f64) -> Self {
        Self {
            intensity: 0.0,
            enabled: true,
        }
    }
}

impl Effect for SurroundEffect {
    fn process(&mut self, samples: &mut [f64], channels: usize) {
        // Stereo-only: mid/side needs exactly one L/R pair (see note above).
        if !self.enabled || self.intensity == 0.0 || channels != 2 {
            return;
        }

        let scaled = self.intensity * SURROUND_INTENSITY_SCALE;
        let gain_side = (1.0 + 3.0 * scaled).max(0.0);
        let comp = 1.0 - 0.3 * scaled;
        let frames = samples.len() / channels;

        for frame in 0..frames {
            let l = samples[frame * channels];
            let r = samples[frame * channels + 1];
            let mono = (l + r) * 0.5;
            let out_l = mono * comp + gain_side * (l - mono);
            let out_r = mono * comp + gain_side * (r - mono);
            samples[frame * channels] = out_l.clamp(-1.0, 1.0);
            samples[frame * channels + 1] = out_r.clamp(-1.0, 1.0);
        }
    }

    fn reset(&mut self) {}
    fn set_intensity(&mut self, v: f64) {
        self.intensity = v.clamp(-1.0, 1.0);
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

// ── Dynamic Boost — loudness maximizer (makeup + lookahead brickwall) ─────────
//
// Applies makeup gain to raise overall loudness, then a lookahead peak limiter
// holds the output below a ceiling. Unlike the previous version it does NOT
// force every signal toward a fixed target level, so quiet passages (and their
// noise floor) are not pumped up — only peaks are tamed.
//
//   makeup  = 10^(intensity · 12 dB / 20)
//   ceiling = 0.9 (peak)
//   lookahead 0.75 ms; fast attack, ~80 ms release on the gain-reduction env.

const MAXI_LOOKAHEAD_MS: f64 = 0.75;
const MAXI_BOOST_MAX_DB: f64 = 12.0;
const MAXI_CEILING: f64 = 0.9;
const MAXI_ATTACK_MS: f64 = 0.2;
const MAXI_RELEASE_MS: f64 = 80.0;

#[derive(Debug, Clone)]
pub struct DynamicBoostEffect {
    intensity: f64,
    enabled: bool,
    delay_buf: Vec<f64>,
    delay_idx: usize,
    delay_len: usize,
    delay_samples: usize,
    envelope: f64, // peak envelope of the makeup-scaled signal
    attack_beta: f64,
    release_beta: f64,
    channels_alloc: usize,
}

impl DynamicBoostEffect {
    #[must_use]
    pub fn new(sample_rate: f64) -> Self {
        let delay_samples = (MAXI_LOOKAHEAD_MS / 1000.0 * sample_rate).ceil() as usize;
        let attack_beta = (-2.2_f64 / (MAXI_ATTACK_MS / 1000.0 * sample_rate)).exp();
        let release_beta = (-2.2_f64 / (MAXI_RELEASE_MS / 1000.0 * sample_rate)).exp();
        let channels_alloc = 2;
        let delay_len = (delay_samples + 1) * channels_alloc;
        Self {
            intensity: 0.0,
            enabled: true,
            delay_buf: vec![0.0; delay_len],
            delay_idx: 0,
            delay_len,
            delay_samples,
            envelope: 0.0,
            attack_beta,
            release_beta,
            channels_alloc,
        }
    }

    fn ensure_channels(&mut self, channels: usize) {
        if self.channels_alloc != channels {
            self.channels_alloc = channels;
            self.delay_len = (self.delay_samples + 1) * channels;
            self.delay_buf = vec![0.0; self.delay_len];
            self.delay_idx = 0;
        }
    }
}

impl Effect for DynamicBoostEffect {
    fn process(&mut self, samples: &mut [f64], channels: usize) {
        if !self.enabled || self.intensity == 0.0 {
            return;
        }
        self.ensure_channels(channels);

        let makeup = 10.0_f64.powf(self.intensity * MAXI_BOOST_MAX_DB / 20.0);
        let frames = samples.len() / channels;

        for frame in 0..frames {
            // Peak of the makeup-scaled current frame (lookahead target).
            let peak = (0..channels)
                .map(|ch| (samples[frame * channels + ch] * makeup).abs())
                .fold(0.0f64, f64::max);

            // Fast attack toward rising peaks, slow release.
            self.envelope = if peak > self.envelope {
                self.envelope * self.attack_beta + peak * (1.0 - self.attack_beta)
            } else {
                self.envelope * self.release_beta + peak * (1.0 - self.release_beta)
            };

            // Write makeup-scaled samples into the lookahead ring buffer.
            for ch in 0..channels {
                let write_pos = (self.delay_idx + ch) % self.delay_len;
                self.delay_buf[write_pos] = samples[frame * channels + ch] * makeup;
            }

            // Gain reduction: only attenuate when the envelope exceeds the ceiling.
            let gr = if self.envelope > MAXI_CEILING {
                MAXI_CEILING / self.envelope
            } else {
                1.0
            };

            let read_start = (self.delay_idx + channels) % self.delay_len;
            for ch in 0..channels {
                let read_pos = (read_start + ch) % self.delay_len;
                samples[frame * channels + ch] = (self.delay_buf[read_pos] * gr).clamp(-1.0, 1.0);
            }

            self.delay_idx = (self.delay_idx + channels) % self.delay_len;
        }
    }

    fn reset(&mut self) {
        self.delay_buf.iter_mut().for_each(|x| *x = 0.0);
        self.delay_idx = 0;
        self.envelope = 0.0;
    }

    fn set_intensity(&mut self, v: f64) {
        self.intensity = v.clamp(0.0, 1.0);
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

// ── Bass Boost — peaking (bell) biquad at 90 Hz, bipolar ──────────────────────
//
// FxSound Play bass component: Audio EQ Cookbook peaking EQ.
//   center 90 Hz, Q 2.5, gain = intensity · 15 dB.
// Bipolar: negative intensity cuts low end, positive boosts.

const BASS_CENTER_HZ: f64 = 90.0;
const BASS_Q: f64 = 2.5;
const BASS_MAX_DB: f64 = 15.0;

#[derive(Debug, Clone)]
pub struct BassBoostEffect {
    intensity: f64,
    enabled: bool,
    sample_rate: f64,
    states: Vec<crate::filter::BiquadState>,
    coeffs: crate::filter::BiquadCoeffs,
}

impl BassBoostEffect {
    #[must_use]
    pub fn new(channels: usize, sample_rate: f64) -> Self {
        let coeffs = crate::filter::BiquadCoeffs::peaking(BASS_CENTER_HZ, 0.0, BASS_Q, sample_rate)
            .unwrap_or(crate::filter::BiquadCoeffs {
                b0: 1.0,
                b1: 0.0,
                b2: 0.0,
                a1: 0.0,
                a2: 0.0,
            });
        Self {
            intensity: 0.0,
            enabled: true,
            sample_rate,
            states: vec![crate::filter::BiquadState::default(); channels],
            coeffs,
        }
    }

    fn rebuild_coeffs(&mut self) {
        let gain_db = self.intensity * BASS_MAX_DB;
        self.coeffs =
            crate::filter::BiquadCoeffs::peaking(BASS_CENTER_HZ, gain_db, BASS_Q, self.sample_rate)
                .unwrap_or(crate::filter::BiquadCoeffs {
                    b0: 1.0,
                    b1: 0.0,
                    b2: 0.0,
                    a1: 0.0,
                    a2: 0.0,
                });
    }
}

impl Effect for BassBoostEffect {
    fn process(&mut self, samples: &mut [f64], channels: usize) {
        if !self.enabled || self.intensity == 0.0 {
            return;
        }
        let frames = samples.len() / channels;
        for frame in 0..frames {
            for ch in 0..channels {
                let idx = frame * channels + ch;
                samples[idx] = self.states[ch].process(samples[idx], &self.coeffs);
            }
        }
    }

    fn reset(&mut self) {
        self.states
            .iter_mut()
            .for_each(super::filter::BiquadState::reset);
    }

    fn set_intensity(&mut self, v: f64) {
        self.intensity = v.clamp(-1.0, 1.0);
        self.rebuild_coeffs();
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
