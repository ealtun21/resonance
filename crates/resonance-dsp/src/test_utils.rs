use crate::filter::ApoFilter;
use std::f64::consts::PI;

/// Steady-state RMS gain of `filter` at `freq_hz`, measured over the second
/// half of a 400 ms run (first half is settlement).
pub fn filter_gain_db(filter: &mut ApoFilter, freq_hz: f64, sample_rate: f64) -> f64 {
    let total = (sample_rate * 0.4) as usize;
    let half = total / 2;
    let omega = 2.0 * PI * freq_hz / sample_rate;

    for i in 0..half {
        filter.process_channel((omega * i as f64).sin(), 0);
    }

    let mut in_sq = 0.0f64;
    let mut out_sq = 0.0f64;
    for i in 0..half {
        let x = (omega * (half + i) as f64).sin();
        let y = filter.process_channel(x, 0);
        in_sq += x * x;
        out_sq += y * y;
    }

    20.0 * (out_sq / in_sq).sqrt().log10()
}

/// RMS of a sample slice.
pub fn rms(samples: &[f64]) -> f64 {
    let sq: f64 = samples.iter().map(|x| x * x).sum();
    (sq / samples.len() as f64).sqrt()
}

/// Generate a mono sine at `freq_hz` with `n` samples starting at `offset`.
pub fn sine(freq_hz: f64, sample_rate: f64, n: usize, offset: usize) -> Vec<f64> {
    let omega = 2.0 * PI * freq_hz / sample_rate;
    (0..n)
        .map(|i| (omega * (offset + i) as f64).sin())
        .collect()
}

/// Generate interleaved stereo sine (L = +amplitude, R = -amplitude).
pub fn stereo_sine(freq_hz: f64, sample_rate: f64, frames: usize, amplitude: f64) -> Vec<f64> {
    let omega = 2.0 * PI * freq_hz / sample_rate;
    (0..frames)
        .flat_map(|i| {
            let s = amplitude * (omega * i as f64).sin();
            [s, -s]
        })
        .collect()
}
