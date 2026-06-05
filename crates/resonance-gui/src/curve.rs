//! EQ frequency-response evaluation (shared logic with the TUI).
//!
//! Builds the real DSP biquad coefficients for each band and evaluates the
//! cascade magnitude response; dB contributions are additive across stages.

use resonance_dsp::filter::BiquadCoeffs;
use resonance_ipc::{BandState, BandType};

pub const LOG_MIN: f64 = 1.301_029_9; // log10(20)
pub const LOG_MAX: f64 = 4.301_029_9; // log10(20000)
pub const DB_RANGE: f64 = 18.0;

/// Combined frequency response in dB at `freq_hz`.
pub fn response_db(bands: &[BandState], freq_hz: f64, sample_rate: f64) -> f64 {
    bands
        .iter()
        .filter(|b| b.enabled)
        .filter_map(|b| coeffs_for(b, sample_rate))
        .map(|c| biquad_mag_db(&c, freq_hz, sample_rate))
        .sum()
}

fn coeffs_for(b: &BandState, sr: f64) -> Option<BiquadCoeffs> {
    match b.band_type {
        BandType::Peaking => BiquadCoeffs::peaking(b.freq, b.gain_db, b.q, sr).ok(),
        BandType::LowShelf => BiquadCoeffs::low_shelf(b.freq, b.gain_db, b.q, sr).ok(),
        BandType::HighShelf => BiquadCoeffs::high_shelf(b.freq, b.gain_db, b.q, sr).ok(),
        BandType::LowPass => BiquadCoeffs::low_pass(b.freq, b.q, sr).ok(),
        BandType::HighPass => BiquadCoeffs::high_pass(b.freq, b.q, sr).ok(),
        BandType::BandPass => BiquadCoeffs::band_pass(b.freq, b.q, sr).ok(),
        BandType::Notch => BiquadCoeffs::notch(b.freq, b.q, sr).ok(),
        BandType::AllPass => None, // flat magnitude
    }
}

/// Sample the response at `n` log-spaced points from 20 Hz to 20 kHz.
/// Returns `(log10(freq), gain_db)` pairs, with sub-sampling so narrow high-Q
/// peaks are not skipped between points.
pub fn curve_points(bands: &[BandState], sample_rate: f64, n: usize) -> Vec<(f64, f64)> {
    let span = LOG_MAX - LOG_MIN;
    let step = span / (n.max(2) - 1) as f64;
    (0..n)
        .map(|i| {
            let t = i as f64 / (n - 1) as f64;
            let log_freq = LOG_MIN + t * span;
            let mut best = 0.0;
            let mut best_abs = -1.0;
            for s in [-0.5, -0.25, 0.0, 0.25, 0.5] {
                let lf = log_freq + s * step;
                let db = response_db(bands, 10f64.powf(lf), sample_rate);
                if db.abs() > best_abs {
                    best_abs = db.abs();
                    best = db;
                }
            }
            (log_freq, best.clamp(-DB_RANGE, DB_RANGE))
        })
        .collect()
}

/// Map a frequency to its clamped log10 x-axis coordinate.
pub fn clampf_log(freq: f64) -> f64 {
    freq.clamp(20.0, 20000.0).log10()
}

/// x-axis tick positions (log10 of freq) and labels.
pub fn x_axis_ticks() -> Vec<(f64, &'static str)> {
    vec![
        (20f64.log10(), "20"),
        (50f64.log10(), "50"),
        (100f64.log10(), "100"),
        (200f64.log10(), "200"),
        (500f64.log10(), "500"),
        (1000f64.log10(), "1k"),
        (2000f64.log10(), "2k"),
        (5000f64.log10(), "5k"),
        (10000f64.log10(), "10k"),
        (20000f64.log10(), "20k"),
    ]
}

fn biquad_mag_db(c: &BiquadCoeffs, freq: f64, sr: f64) -> f64 {
    use std::f64::consts::PI;
    let w = 2.0 * PI * freq / sr;
    let (c1, s1) = (w.cos(), w.sin());
    let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());

    let num = mag(c.b0 + c.b1 * c1 + c.b2 * c2, -(c.b1 * s1 + c.b2 * s2));
    let den = mag(1.0 + c.a1 * c1 + c.a2 * c2, -(c.a1 * s1 + c.a2 * s2));

    if den < 1e-12 {
        return 0.0;
    }
    20.0 * (num / den).log10()
}

fn mag(re: f64, im: f64) -> f64 {
    (re * re + im * im).sqrt()
}
