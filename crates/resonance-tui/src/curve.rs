/// Computes the EQ frequency response curve from a list of band states.
///
/// Uses peaking biquad formula (Audio EQ Cookbook) at log-spaced frequencies.
/// Gains in dB are additive across independent biquad stages.
use resonance_ipc::BandState;
use std::f64::consts::PI;

const LOG_MIN: f64 = 1.301_029_9; // log10(20)
const LOG_MAX: f64 = 4.301_029_9; // log10(20000)

/// Evaluate combined frequency response in dB at `freq_hz`.
pub fn response_db(bands: &[BandState], freq_hz: f64, sample_rate: f64) -> f64 {
    bands
        .iter()
        .filter(|b| b.enabled && b.gain_db.abs() > 0.001)
        .map(|b| peaking_db(freq_hz, b.freq, b.gain_db, b.q, sample_rate))
        .sum()
}

/// Sample the curve at `n_points` log-spaced frequencies from 20 to 20 kHz.
/// Returns `(x, y)` pairs where `x = log10(freq)` and `y = gain_db`.
pub fn curve_points(bands: &[BandState], sample_rate: f64, n: usize) -> Vec<(f64, f64)> {
    (0..n)
        .map(|i| {
            let t = i as f64 / (n - 1) as f64;
            let log_freq = LOG_MIN + t * (LOG_MAX - LOG_MIN);
            let freq = 10f64.powf(log_freq);
            let db = response_db(bands, freq, sample_rate).clamp(-30.0, 30.0);
            (log_freq, db)
        })
        .collect()
}

/// x-axis tick positions and labels (log10 of freq).
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

// ── Peaking biquad frequency response ─────────────────────────────────────

fn peaking_db(freq: f64, center: f64, gain_db: f64, q: f64, sr: f64) -> f64 {
    let q = q.max(0.01);
    let a = 10f64.powf(gain_db / 40.0);
    let w0 = 2.0 * PI * center / sr;
    let (sin_w0, cos_w0) = (w0.sin(), w0.cos());
    let alpha = sin_w0 / (2.0 * q);
    let a0 = 1.0 + alpha / a;

    let b0 = (1.0 + alpha * a) / a0;
    let b1 = (-2.0 * cos_w0) / a0;
    let b2 = (1.0 - alpha * a) / a0;
    let a1 = (-2.0 * cos_w0) / a0;
    let a2 = (1.0 - alpha / a) / a0;

    let w = 2.0 * PI * freq / sr;
    let (c1, s1) = (w.cos(), w.sin());
    let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());

    let num = mag(b0 + b1 * c1 + b2 * c2, -(b1 * s1 + b2 * s2));
    let den = mag(1.0 + a1 * c1 + a2 * c2, -(a1 * s1 + a2 * s2));

    if den < 1e-10 {
        return gain_db;
    }
    20.0 * (num / den).log10()
}

fn mag(re: f64, im: f64) -> f64 {
    (re * re + im * im).sqrt()
}
