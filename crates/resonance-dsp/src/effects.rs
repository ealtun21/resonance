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

    fn allpass_tick(
        buf: &mut Vec<f64>,
        idx: &mut usize,
        delay: usize,
        gain: f64,
        input: f64,
    ) -> f64 {
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

/// Surround: Haas-effect stereo widener with cross-feed for headphones.
#[derive(Debug, Clone)]
pub struct SurroundEffect {
    intensity: f64,
    enabled: bool,
    delay_buf: Vec<f64>,
    delay_idx: usize,
    delay_samples: usize,
}

impl SurroundEffect {
    pub fn new(sample_rate: f64) -> Self {
        let delay_samples = ((0.0002 * sample_rate) as usize).max(1);
        Self {
            intensity: 0.0,
            enabled: true,
            delay_buf: vec![0.0; delay_samples + 1],
            delay_idx: 0,
            delay_samples,
        }
    }
}

impl Effect for SurroundEffect {
    fn process(&mut self, samples: &mut [f64], channels: usize) {
        if !self.enabled || self.intensity == 0.0 || channels < 2 {
            return;
        }
        let width = self.intensity * 0.4;
        let frames = samples.len() / channels;

        for frame in 0..frames {
            let l = samples[frame * channels];
            let r = samples[frame * channels + 1];
            let mid = (l + r) * 0.5;
            let side = (l - r) * 0.5;

            let delayed = self.delay_buf[self.delay_idx];
            self.delay_buf[self.delay_idx] = side;
            self.delay_idx = (self.delay_idx + 1) % (self.delay_samples + 1);

            let widened_side = side + width * delayed;
            samples[frame * channels] = mid + widened_side;
            samples[frame * channels + 1] = mid - widened_side;
        }
    }

    fn reset(&mut self) {
        self.delay_buf.iter_mut().for_each(|s| *s = 0.0);
        self.delay_idx = 0;
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

/// Bass Boost: sub-harmonic synthesis + low-shelf boost.
#[derive(Debug, Clone)]
pub struct BassBoostEffect {
    intensity: f64,
    enabled: bool,
    // Single-pole LP to isolate fundamental for sub-octave synthesis
    lp_states: Vec<f64>,
    lp_coeff: f64,
    // Low-shelf via first-order shelving
    shelf_states: Vec<f64>,
    shelf_b0: f64,
    shelf_b1: f64,
    shelf_a1: f64,
}

impl BassBoostEffect {
    pub fn new(channels: usize, sample_rate: f64) -> Self {
        let fc = 200.0;
        let rc = 1.0 / (2.0 * PI * fc);
        let dt = 1.0 / sample_rate;
        let lp_coeff = dt / (rc + dt);

        let shelf_fc = 120.0;
        let shelf_gain_db = 6.0;
        let a = 10f64.powf(shelf_gain_db / 40.0);
        let w0 = 2.0 * PI * shelf_fc / sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / 2.0 * (a + 1.0 / a).sqrt();
        let sq = 2.0 * a.sqrt() * alpha;
        let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + sq;
        let shelf_b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + sq) / a0;
        let shelf_b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0) / a0;
        let shelf_a1 = -2.0 * ((a - 1.0) * cos_w0) / a0;

        Self {
            intensity: 0.0,
            enabled: true,
            lp_states: vec![0.0; channels],
            lp_coeff,
            shelf_states: vec![0.0; channels],
            shelf_b0,
            shelf_b1,
            shelf_a1,
        }
    }
}

impl Effect for BassBoostEffect {
    fn process(&mut self, samples: &mut [f64], channels: usize) {
        if !self.enabled || self.intensity == 0.0 {
            return;
        }
        let frames = samples.len() / channels;
        let sub_mix = self.intensity * 0.3;

        for frame in 0..frames {
            for ch in 0..channels {
                let idx = frame * channels + ch;
                let x = samples[idx];

                // Low-shelf boost (always active when effect is on)
                let shelf_out = self.shelf_b0 * x + self.shelf_b1 * self.shelf_states[ch]
                    - self.shelf_a1 * self.shelf_states[ch];
                self.shelf_states[ch] = x;

                // Sub-octave synthesis: LP + rectify + LP again
                self.lp_states[ch] += self.lp_coeff * (x - self.lp_states[ch]);
                let sub = (self.lp_states[ch] * 2.0).tanh() * self.lp_states[ch].signum();

                samples[idx] = (shelf_out + sub_mix * sub).clamp(-1.0, 1.0);
            }
        }
    }

    fn reset(&mut self) {
        self.lp_states.iter_mut().for_each(|s| *s = 0.0);
        self.shelf_states.iter_mut().for_each(|s| *s = 0.0);
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
