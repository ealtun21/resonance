//! TUI EQ curve helpers. The evaluation core (response, biquad magnitude,
//! log-spaced sampling) lives in the shared `resonance_ipc::fr` module; this
//! module keeps the TUI's fixed full-range sampling + tick set.

use resonance_ipc::BandState;

// Re-export the shared band-marker helper under the name the TUI already uses.
pub use resonance_ipc::fr::band_marker_x;

/// TUI clamps the displayed curve to ±18 dB (its graph never auto-expands).
const TUI_CLAMP_DB: f64 = 18.0;

/// Sample the curve at `n` log-spaced frequencies across the full 20 Hz–20 kHz
/// range. Returns `(log10(freq), gain_db)` pairs (clamped to ±18 dB).
pub fn curve_points(bands: &[BandState], sample_rate: f64, n: usize) -> Vec<(f64, f64)> {
    use resonance_ipc::fr::{LOG_MAX, LOG_MIN};
    resonance_ipc::fr::curve_points_range(bands, sample_rate, n, LOG_MIN, LOG_MAX, TUI_CLAMP_DB)
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
