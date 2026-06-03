use std::f64::consts::PI;

pub trait Effect: Send {
    fn process(&mut self, samples: &mut [f64], channels: usize);
    fn reset(&mut self);
    fn set_intensity(&mut self, value: f64);
    fn intensity(&self) -> f64;
    fn enabled(&self) -> bool;
    fn set_enabled(&mut self, on: bool);
}

/// Fidelity/Clarity: harmonic high-frequency exciter.
/// Adds odd harmonics above the crossover, scaled by intensity.
#[derive(Debug, Clone)]
pub struct FidelityEffect {
    intensity: f64,
    enabled: bool,
    crossover_hz: f64,
    sample_rate: f64,
    // single-pole high-pass state for each channel
    hp_states: Vec<f64>,
}

impl FidelityEffect {
    pub fn new(channels: usize, sample_rate: f64) -> Self {
        Self {
            intensity: 0.0,
            enabled: true,
            crossover_hz: 3500.0,
            sample_rate,
            hp_states: vec![0.0; channels],
        }
    }

    fn hp_coeff(&self) -> f64 {
        let rc = 1.0 / (2.0 * PI * self.crossover_hz);
        let dt = 1.0 / self.sample_rate;
        rc / (rc + dt)
    }
}

impl Effect for FidelityEffect {
    fn process(&mut self, samples: &mut [f64], channels: usize) {
        if !self.enabled || self.intensity == 0.0 {
            return;
        }
        let alpha = self.hp_coeff();
        let mix = self.intensity * 0.25;

        let frames = samples.len() / channels;
        for frame in 0..frames {
            for ch in 0..channels {
                let idx = frame * channels + ch;
                let x = samples[idx];
                let hp = alpha * (self.hp_states[ch] + x - samples[idx.saturating_sub(channels)]);
                self.hp_states[ch] = hp;
                let harmonic = hp * hp * hp;
                samples[idx] += mix * harmonic;
            }
        }
    }

    fn reset(&mut self) {
        self.hp_states.iter_mut().for_each(|s| *s = 0.0);
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

/// Ambience: simple Schroeder allpass reverb tail mixed at low wet level.
#[derive(Debug, Clone)]
pub struct AmbienceEffect {
    intensity: f64,
    enabled: bool,
    // 4 allpass stages per channel
    ap_buffers: Vec<Vec<Vec<f64>>>,
    ap_indices: Vec<Vec<usize>>,
    ap_delays: [usize; 4],
    ap_gain: f64,
}

impl AmbienceEffect {
    const AP_DELAYS_SAMPLES_48K: [usize; 4] = [347, 113, 37, 13];

    pub fn new(channels: usize, sample_rate: f64) -> Self {
        let ratio = sample_rate / 48000.0;
        let ap_delays: [usize; 4] =
            Self::AP_DELAYS_SAMPLES_48K.map(|d| ((d as f64 * ratio) as usize).max(1));

        let ap_buffers = (0..channels)
            .map(|_| ap_delays.map(|d| vec![0.0f64; d + 1]).to_vec())
            .collect();
        let ap_indices = vec![vec![0usize; 4]; channels];

        Self {
            intensity: 0.0,
            enabled: true,
            ap_buffers,
            ap_indices,
            ap_delays,
            ap_gain: 0.7,
        }
    }

    fn allpass_tick(buf: &mut [f64], idx: &mut usize, delay: usize, gain: f64, input: f64) -> f64 {
        let delayed = buf[*idx];
        let w = input + gain * delayed;
        buf[*idx] = w;
        *idx = (*idx + 1) % (delay + 1);
        delayed - gain * w
    }
}

impl Effect for AmbienceEffect {
    fn process(&mut self, samples: &mut [f64], channels: usize) {
        if !self.enabled || self.intensity == 0.0 {
            return;
        }
        let wet = self.intensity * 0.15;
        let frames = samples.len() / channels;

        for frame in 0..frames {
            for ch in 0..channels {
                let idx = frame * channels + ch;
                let mut sig = samples[idx];
                for stage in 0..4 {
                    sig = Self::allpass_tick(
                        &mut self.ap_buffers[ch][stage],
                        &mut self.ap_indices[ch][stage],
                        self.ap_delays[stage],
                        self.ap_gain,
                        sig,
                    );
                }
                samples[idx] += wet * sig;
            }
        }
    }

    fn reset(&mut self) {
        for ch in self.ap_buffers.iter_mut() {
            for buf in ch.iter_mut() {
                buf.iter_mut().for_each(|s| *s = 0.0);
            }
        }
        for ch in self.ap_indices.iter_mut() {
            ch.iter_mut().for_each(|i| *i = 0);
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

/// Surround: Haas-effect stereo widener.
/// Delays the R channel by ~0.2 ms, creating audible inter-channel difference
/// for any input signal (including mono), perceived as stereo width.
#[derive(Debug, Clone)]
pub struct SurroundEffect {
    intensity: f64,
    enabled: bool,
    // Delay line for R channel (Haas effect)
    r_delay_buf: Vec<f64>,
    r_delay_idx: usize,
    delay_samples: usize,
}

impl SurroundEffect {
    pub fn new(sample_rate: f64) -> Self {
        let delay_samples = ((0.0002 * sample_rate) as usize).max(1);
        Self {
            intensity: 0.0,
            enabled: true,
            r_delay_buf: vec![0.0; delay_samples + 1],
            r_delay_idx: 0,
            delay_samples,
        }
    }
}

impl Effect for SurroundEffect {
    fn process(&mut self, samples: &mut [f64], channels: usize) {
        if !self.enabled || self.intensity == 0.0 || channels < 2 {
            return;
        }
        let mix = self.intensity;
        let frames = samples.len() / channels;

        for frame in 0..frames {
            let r = samples[frame * channels + 1];

            // Haas delay on R: blend between dry R and delayed R
            let delayed_r = self.r_delay_buf[self.r_delay_idx];
            self.r_delay_buf[self.r_delay_idx] = r;
            self.r_delay_idx = (self.r_delay_idx + 1) % (self.delay_samples + 1);

            // L stays dry; R cross-fades toward the delayed version
            samples[frame * channels + 1] = (1.0 - mix) * r + mix * delayed_r;
        }
    }

    fn reset(&mut self) {
        self.r_delay_buf.iter_mut().for_each(|s| *s = 0.0);
        self.r_delay_idx = 0;
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

/// Dynamic Boost: soft-knee upward compander (boost quiet, limit loud).
#[derive(Debug, Clone)]
pub struct DynamicBoostEffect {
    intensity: f64,
    enabled: bool,
    envelope: f64,
    attack_coeff: f64,
    release_coeff: f64,
}

impl DynamicBoostEffect {
    pub fn new(sample_rate: f64) -> Self {
        Self {
            intensity: 0.0,
            enabled: true,
            envelope: 0.0,
            attack_coeff: 1.0 - (-2.2 / (0.005 * sample_rate)).exp(),
            release_coeff: 1.0 - (-2.2 / (0.100 * sample_rate)).exp(),
        }
    }
}

impl Effect for DynamicBoostEffect {
    fn process(&mut self, samples: &mut [f64], channels: usize) {
        if !self.enabled || self.intensity == 0.0 {
            return;
        }
        let frames = samples.len() / channels;
        let threshold = 1.0 - self.intensity * 0.6;
        let boost_ratio = 1.0 + self.intensity * 1.5;

        for frame in 0..frames {
            let mut peak = 0.0f64;
            for ch in 0..channels {
                peak = peak.max(samples[frame * channels + ch].abs());
            }

            let coeff = if peak > self.envelope {
                self.attack_coeff
            } else {
                self.release_coeff
            };
            self.envelope = self.envelope + coeff * (peak - self.envelope);

            let gain = if self.envelope < threshold {
                1.0 + (threshold - self.envelope) / threshold * (boost_ratio - 1.0)
            } else {
                1.0
            };

            for ch in 0..channels {
                samples[frame * channels + ch] =
                    (samples[frame * channels + ch] * gain).clamp(-1.0, 1.0);
            }
        }
    }

    fn reset(&mut self) {
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

/// Bass Boost: sub-harmonic synthesis via zero-crossing octave divider + low-shelf boost.
///
/// Sub-octave generation: LP-filter the input to isolate the bass fundamental,
/// then use a zero-crossing toggle (frequency divider ÷2) to produce a signal
/// at exactly half the fundamental frequency, then smooth with a second LP pass.
/// This produces true sub-octave content (e.g. 120 Hz → 60 Hz).
#[derive(Debug, Clone)]
pub struct BassBoostEffect {
    intensity: f64,
    enabled: bool,
    // First LP: isolate bass fundamental for the divider
    lp_states: Vec<f64>,
    lp_coeff: f64,
    // Zero-crossing frequency divider state
    prev_lp: Vec<f64>,
    sub_toggle: Vec<f64>,
    // Second LP: smooth the sub-octave output
    sub_lp: Vec<f64>,
    // Low-shelf biquad
    shelf_coeffs: crate::filter::BiquadCoeffs,
    shelf_states: Vec<crate::filter::BiquadState>,
}

impl BassBoostEffect {
    pub fn new(channels: usize, sample_rate: f64) -> Self {
        let lp_fc = 200.0;
        let rc = 1.0 / (2.0 * PI * lp_fc);
        let dt = 1.0 / sample_rate;
        let lp_coeff = dt / (rc + dt);

        let shelf_coeffs = crate::filter::BiquadCoeffs::low_shelf(120.0, 6.0, sample_rate)
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
            lp_states: vec![0.0; channels],
            lp_coeff,
            prev_lp: vec![0.0; channels],
            sub_toggle: vec![1.0; channels],
            sub_lp: vec![0.0; channels],
            shelf_coeffs,
            shelf_states: vec![crate::filter::BiquadState::default(); channels],
        }
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
                let x = samples[idx];

                // Low-shelf boost
                let shelf_out = self.shelf_states[ch].process(x, &self.shelf_coeffs);

                // LP isolates the bass fundamental
                self.lp_states[ch] += self.lp_coeff * (x - self.lp_states[ch]);
                let lp = self.lp_states[ch];

                // Frequency divider ÷2: toggle on every positive zero-crossing
                if self.prev_lp[ch] <= 0.0 && lp > 0.0 {
                    self.sub_toggle[ch] = -self.sub_toggle[ch];
                }
                self.prev_lp[ch] = lp;

                // Sub-octave: toggle × |lp| → signal at half the fundamental frequency
                let sub_raw = self.sub_toggle[ch] * lp.abs();

                // Smooth with a second LP pass (removes toggle-switching artefacts)
                self.sub_lp[ch] += self.lp_coeff * (sub_raw - self.sub_lp[ch]);

                samples[idx] =
                    (shelf_out + self.intensity * 0.3 * self.sub_lp[ch]).clamp(-1.0, 1.0);
            }
        }
    }

    fn reset(&mut self) {
        self.lp_states.iter_mut().for_each(|s| *s = 0.0);
        self.prev_lp.iter_mut().for_each(|s| *s = 0.0);
        self.sub_toggle.iter_mut().for_each(|s| *s = 1.0);
        self.sub_lp.iter_mut().for_each(|s| *s = 0.0);
        self.shelf_states.iter_mut().for_each(|s| s.reset());
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
