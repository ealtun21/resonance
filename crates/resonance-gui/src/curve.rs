//! EQ frequency-response evaluation (shared logic with the TUI).
//!
//! Builds the real DSP biquad coefficients for each band and evaluates the
//! cascade magnitude response; dB contributions are additive across stages.

use resonance_dsp::filter::BiquadCoeffs;
use resonance_ipc::{BandState, BandType};

pub const LOG_MIN: f64 = 1.301_029_9; // log10(20)
pub const LOG_MAX: f64 = 4.301_029_9; // log10(20000)
/// Smallest (default) ± dB the FR graph shows; it auto-expands past this.
pub const DB_RANGE: f64 = 18.0;
/// Absolute cap the response is clamped to (and the largest the axis grows to).
pub const MAX_DB: f64 = 60.0;

/// Pick a "nice" ± dB axis range (and grid step) that fits `peak_db`. The axis
/// starts at ±[`DB_RANGE`] and grows through fixed stops up to ±[`MAX_DB`].
pub fn display_range(peak_db: f64) -> (f64, f64) {
    const STOPS: [(f64, f64); 6] = [
        (DB_RANGE, 6.0),
        (24.0, 8.0),
        (30.0, 10.0),
        (40.0, 10.0),
        (50.0, 10.0),
        (60.0, 20.0),
    ];
    for (range, step) in STOPS {
        if peak_db <= range * 0.98 {
            return (range, step);
        }
    }
    (MAX_DB, 20.0)
}

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

/// Sample the response at `n` log-spaced points over `[log_min, log_max]`
/// (log10 Hz). Returns `(log10(freq), gain_db)` pairs, with sub-sampling so
/// narrow high-Q peaks are not skipped between points. Used over a sub-range
/// so the curve stays dense when the FR graph is zoomed in.
pub fn curve_points_range(
    bands: &[BandState],
    sample_rate: f64,
    n: usize,
    log_min: f64,
    log_max: f64,
) -> Vec<(f64, f64)> {
    let span = log_max - log_min;
    let step = span / (n.max(2) - 1) as f64;
    (0..n)
        .map(|i| {
            let t = i as f64 / (n - 1) as f64;
            let log_freq = log_min + t * span;
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
            (log_freq, best.clamp(-MAX_DB, MAX_DB))
        })
        .collect()
}

/// Map a frequency to its clamped log10 x-axis coordinate.
pub fn clampf_log(freq: f64) -> f64 {
    freq.clamp(20.0, 20000.0).log10()
}

/// Named frequency regions for the FR graph background shading.
/// `(start_hz, end_hz, label)` — continuous coverage across 20 Hz–20 kHz.
pub fn freq_bands() -> [(f64, f64, &'static str); 7] {
    [
        (20.0, 80.0, "sub bass"),
        (80.0, 300.0, "mid bass"),
        (300.0, 1000.0, "lo-mid"),
        (1000.0, 4000.0, "hi-mid"),
        (4000.0, 6000.0, "presence"),
        (6000.0, 10000.0, "mid treble"),
        (10000.0, 20000.0, "air"),
    ]
}

/// x-axis ticks whose frequency falls within `[log_min, log_max]`. A richer
/// candidate set keeps a zoomed window from going label-less.
pub fn x_axis_ticks_range(log_min: f64, log_max: f64) -> Vec<(f64, &'static str)> {
    const CANDIDATES: &[(f64, &str)] = &[
        (20.0, "20"),
        (30.0, "30"),
        (50.0, "50"),
        (80.0, "80"),
        (100.0, "100"),
        (150.0, "150"),
        (200.0, "200"),
        (300.0, "300"),
        (500.0, "500"),
        (800.0, "800"),
        (1000.0, "1k"),
        (1500.0, "1.5k"),
        (2000.0, "2k"),
        (3000.0, "3k"),
        (5000.0, "5k"),
        (8000.0, "8k"),
        (10000.0, "10k"),
        (15000.0, "15k"),
        (20000.0, "20k"),
    ];
    CANDIDATES
        .iter()
        .map(|&(f, l)| (f.log10(), l))
        .filter(|&(lf, _)| lf >= log_min - 1e-9 && lf <= log_max + 1e-9)
        .collect()
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
