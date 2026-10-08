//! Deterministic test signals: the bit-exact stimulus and the latency chirps.

use std::f64::consts::PI;
use std::ops::Range;

/// Pilot tone mixed into every stimulus channel; resampling and event
/// scenarios read their pitch check from it.
pub const PILOT_HZ: f64 = 997.0;
const LEAD_SECS: f64 = 0.5;
const TAIL_SECS: f64 = 0.5;
// Component peaks sum to 0.25 (−12 dBFS), so no stage can clip it.
const NOISE_AMP: f64 = 0.125;
const SWEEP_AMP: f64 = 0.0625;
const PILOT_AMP: f64 = 0.0625;

/// Interleaved f32 signal with digital silence around the body.
#[derive(Debug, Clone)]
pub struct Stimulus {
    pub rate: u32,
    pub channels: usize,
    pub samples: Vec<f32>,
    /// Frame range of the non-silent body.
    pub body: Range<usize>,
}

impl Stimulus {
    #[must_use]
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels
    }
}

/// xorshift64* → uniform in [-1, 1). A distinct seed per channel means a
/// swapped or misrouted channel can never compare equal.
struct Noise(u64);

impl Noise {
    fn new(channel: usize) -> Self {
        Self(0x9E37_79B9_7F4A_7C15 ^ ((channel as u64 + 1).wrapping_mul(0xD1B5_4A32_D192_ED03)))
    }

    fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let v = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
        v as f64 / (1u64 << 52) as f64 - 1.0
    }
}

/// `body_secs` of noise + exponential sweep (20 Hz → 0.45·rate) + pilot at
/// `rate`, framed by 0.5 s of digital silence on each side.
#[must_use]
pub fn generate(rate: u32, channels: usize, body_secs: f64) -> Stimulus {
    let r = f64::from(rate);
    let lead = (LEAD_SECS * r) as usize;
    let body = (body_secs * r) as usize;
    let tail = (TAIL_SECS * r) as usize;
    let mut samples = vec![0.0f32; (lead + body + tail) * channels];
    let (f0, f1) = (20.0, 0.45 * r);
    let k = (f1 / f0).ln();
    let dur = body as f64 / r;
    for c in 0..channels {
        let mut noise = Noise::new(c);
        for i in 0..body {
            let t = i as f64 / r;
            let sweep = (2.0 * PI * f0 * dur / k * ((k * t / dur).exp() - 1.0)).sin();
            let pilot = (2.0 * PI * PILOT_HZ * t).sin();
            let s = NOISE_AMP * noise.next() + SWEEP_AMP * sweep + PILOT_AMP * pilot;
            samples[(lead + i) * channels + c] = s as f32;
        }
    }
    Stimulus {
        rate,
        channels,
        samples,
        body: lead..lead + body,
    }
}

pub const CHIRPS: usize = 5;

/// [`CHIRPS`] identical 50 ms linear chirps (200 Hz → 8 kHz, −12 dBFS, all
/// channels), 0.5 s apart after 0.2 s of silence. `emit[j]` is chirp j's
/// first frame.
#[derive(Debug, Clone)]
pub struct ChirpTrain {
    pub rate: u32,
    pub channels: usize,
    pub samples: Vec<f32>,
    pub emit: Vec<usize>,
    pub chirp_frames: usize,
}

#[must_use]
pub fn chirp_train(rate: u32, channels: usize) -> ChirpTrain {
    let r = f64::from(rate);
    let chirp_frames = (0.05 * r) as usize;
    let spacing = (0.5 * r) as usize;
    let lead = (0.2 * r) as usize;
    let frames = lead + spacing * CHIRPS + (0.5 * r) as usize;
    let mut samples = vec![0.0f32; frames * channels];
    let (f0, f1) = (200.0, 8000.0_f64.min(0.45 * r));
    let dur = chirp_frames as f64 / r;
    let emit: Vec<usize> = (0..CHIRPS).map(|j| lead + j * spacing).collect();
    for &start in &emit {
        for i in 0..chirp_frames {
            let t = i as f64 / r;
            let s = (0.25 * (2.0 * PI * (f0 * t + (f1 - f0) * t * t / (2.0 * dur))).sin()) as f32;
            for c in 0..channels {
                samples[(start + i) * channels + c] = s;
            }
        }
    }
    ChirpTrain {
        rate,
        channels,
        samples,
        emit,
        chirp_frames,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)] // determinism + digital silence are exact by design
    use super::*;
    use resonance_dsp::analysis::fft_peak_hz;

    #[test]
    fn stimulus_is_deterministic() {
        assert_eq!(
            generate(48_000, 2, 0.5).samples,
            generate(48_000, 2, 0.5).samples
        );
    }

    #[test]
    fn stimulus_channels_differ() {
        let s = generate(48_000, 2, 0.5);
        let differing = s
            .body
            .clone()
            .filter(|&i| s.samples[i * 2] != s.samples[i * 2 + 1])
            .count();
        assert!(differing > s.body.len() * 9 / 10, "{differing}");
    }

    #[test]
    fn stimulus_is_framed_by_silence_and_peaks_at_or_below_minus_12_dbfs() {
        let s = generate(96_000, 3, 0.5);
        assert!(s.samples[..s.body.start * 3].iter().all(|&v| v == 0.0));
        assert!(s.samples[s.body.end * 3..].iter().all(|&v| v == 0.0));
        let peak = s.samples.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(peak <= 0.2512 && peak > 0.1, "{peak}");
    }

    #[test]
    fn pilot_is_detectable_in_the_body() {
        let s = generate(48_000, 1, 3.0);
        let hz = fft_peak_hz(&s.samples[s.body.clone()], 48_000.0, PILOT_HZ);
        assert!(((hz - PILOT_HZ) / PILOT_HZ).abs() < 1e-4, "{hz}");
    }

    #[test]
    fn chirp_train_marks_each_emission() {
        let t = chirp_train(48_000, 2);
        assert_eq!(t.emit.len(), CHIRPS);
        for &e in &t.emit {
            assert_eq!(t.samples[(e - 1) * 2], 0.0, "silence before chirp");
            assert!(t.samples[(e + 20) * 2].abs() > 0.0, "chirp present");
        }
    }
}
