//! GUI FR-graph helpers: axis scaling, shading regions, and ticks. The
//! evaluation core (response, biquad magnitude, sampling) lives in the shared
//! `resonance_ipc::fr` module and is re-exported here so call sites are stable.

use resonance_ipc::BandState;

// Re-export the shared evaluation core under the names the GUI already uses.
pub use resonance_ipc::fr::{LOG_MAX, LOG_MIN, band_marker_x as clampf_log};

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

/// Sample the response over a log-frequency sub-range (so the curve stays dense
/// when the FR graph is zoomed), clamping to the GUI's ±[`MAX_DB`] axis cap.
pub fn curve_points_range(
    bands: &[BandState],
    sample_rate: f64,
    n: usize,
    log_min: f64,
    log_max: f64,
) -> Vec<(f64, f64)> {
    resonance_ipc::fr::curve_points_range(bands, sample_rate, n, log_min, log_max, MAX_DB)
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
