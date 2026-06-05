//! Computes the EQ frequency response curve from a list of band states.
//!
//! Builds the actual DSP biquad coefficients for each band's filter type
//! (peaking, shelves, pass filters, notch, …) and evaluates the cascade
//! magnitude response. dB contributions are additive across stages.
use resonance_dsp::filter::BiquadCoeffs;
use resonance_ipc::{BandState, BandType};

const LOG_MIN: f64 = 1.301_029_9; // log10(20)
const LOG_MAX: f64 = 4.301_029_9; // log10(20000)

/// Evaluate combined frequency response in dB at `freq_hz`.
pub fn response_db(bands: &[BandState], freq_hz: f64, sample_rate: f64) -> f64 {
    bands
        .iter()
        .filter(|b| b.enabled)
        .filter_map(|b| coeffs_for(b, sample_rate))
        .map(|c| biquad_mag_db(&c, freq_hz, sample_rate))
        .sum()
}

/// Build biquad coefficients for a band according to its filter type.
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

/// Map a frequency to its log10 x-axis coordinate (Hz → curve x).
pub fn band_marker_x(freq: f64) -> f64 {
    freq.clamp(20.0, 20000.0).log10()
}

/// Sample the curve at `n_points` log-spaced frequencies from 20 to 20 kHz.
/// Returns `(x, y)` pairs where `x = log10(freq)` and `y = gain_db`.
///
/// Each point is sub-sampled across its own log-frequency step and the
/// largest-magnitude value is kept, so narrow high-Q peaks/notches are not
/// skipped between samples (which made the displayed curve look Q-independent).
pub fn curve_points(bands: &[BandState], sample_rate: f64, n: usize) -> Vec<(f64, f64)> {
    let span = LOG_MAX - LOG_MIN;
    let step = span / (n.max(2) - 1) as f64;
    (0..n)
        .map(|i| {
            let t = i as f64 / (n - 1) as f64;
            let log_freq = LOG_MIN + t * span;
            // Sub-sample within ±half a step to capture peaks between points.
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
            (log_freq, best.clamp(-18.0, 18.0))
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

// ── Biquad magnitude response ─────────────────────────────────────────────

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

#[cfg(test)]
mod tests {
    use super::*;
    use resonance_ipc::{BandState, BandType};

    fn peaking(q: f64) -> Vec<BandState> {
        vec![BandState {
            band_type: BandType::Peaking,
            freq: 1000.0,
            gain_db: 12.0,
            q,
            enabled: true,
        }]
    }

    #[test]
    fn peak_gain_is_independent_of_q() {
        let sr = 48000.0;
        for q in [0.5, 1.0, 4.0, 8.0] {
            let g = response_db(&peaking(q), 1000.0, sr);
            assert!((g - 12.0).abs() < 0.1, "Q={q} peak gain {g}");
        }
    }

    #[test]
    fn higher_q_is_narrower() {
        let sr = 48000.0;
        let off = 1414.0; // half-octave above Fc
        let wide = response_db(&peaking(1.0), off, sr);
        let narrow = response_db(&peaking(8.0), off, sr);
        assert!(
            wide > narrow + 3.0,
            "higher Q must fall off faster: Q1={wide:.2} Q8={narrow:.2}"
        );
    }

    #[test]
    fn curve_points_capture_high_q_peak() {
        // A narrow peak must still reach near full height in the sampled curve.
        let pts = curve_points(&peaking(8.0), 48000.0, 400);
        let max = pts.iter().fold(0.0f64, |m, &(_, y)| m.max(y));
        assert!(
            max > 11.0,
            "high-Q peak should render near +12 dB; got {max:.2}"
        );
    }
}
