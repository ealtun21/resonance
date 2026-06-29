//! Reference & measurement curves for the FR graph.
//!
//! A [`RefCurve`] is a frequency→dB point set: a headphone/IEM *measurement*
//! (fetched from squig.link) or a *target* response (a reference to EQ toward).
//! This module parses the squig.link / `AutoEq` text format (`freq  dB` rows,
//! tolerating header/footer noise), interpolates in log-frequency, averages L/R
//! channels in the amplitude domain, smooths by fractional octave, and
//! *generates* parametric targets — a base curve plus a spectral tilt and
//! bass / ear-gain / treble filters. These are the building blocks the GUI
//! overlays on the EQ response graph; nothing here touches the network.

use crate::fr::{LOG_MAX, LOG_MIN};
use resonance_dsp::filter::BiquadCoeffs;
use serde::{Deserialize, Serialize};

/// Sample rate for evaluating generated-target biquads. Targets live in the
/// audible band where the magnitude response is effectively rate-independent
/// (the same assumption [`crate::fr`] and the preset fitter make).
const GEN_SR: f64 = 48_000.0;

/// Number of log-spaced points generated/smoothed curves carry. ~1/40-octave
/// over 20 Hz–20 kHz — dense enough to draw smoothly, cheap to interpolate.
const GRID_N: usize = 480;

/// A frequency-response curve as `(frequency Hz, level dB)` points, ascending in
/// frequency. Measurements use absolute SPL levels (~60–90 dB); targets and
/// generated curves are relative. Display code removes the mean (or pins a
/// frequency) so any curve fits the graph's small ± dB axis — see
/// [`RefCurve::norm_offset_mean`] / [`RefCurve::norm_offset_at`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefCurve {
    pub points: Vec<(f64, f64)>,
}

impl RefCurve {
    /// Parse the squig.link / `AutoEq` / REW text format: one `frequency dB` pair
    /// per line (whitespace-, comma-, tab-, or semicolon-separated; a third
    /// column such as phase is ignored). Header and footer lines that don't
    /// start with two numbers are skipped, so real AudioTools/REW exports (blank
    /// line + `Frequency / dB / Unweighted` header + `saved`/`peak` footer) parse
    /// cleanly. Returns `None` if fewer than two valid points are found.
    #[must_use]
    // float_cmp: dedup_by compares parsed frequencies for exact equality to drop
    // identical-frequency rows after sorting — exact-key dedup, not a tolerance compare.
    #[allow(clippy::float_cmp)]
    pub fn parse(text: &str) -> Option<RefCurve> {
        let mut pts: Vec<(f64, f64)> = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty()
                || line.starts_with('#')
                || line.starts_with('*')
                || line.starts_with("//")
            {
                continue;
            }
            let nums: Vec<f64> = line
                .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
                .filter_map(|t| t.trim().parse::<f64>().ok())
                .collect();
            if nums.len() >= 2 && nums[0].is_finite() && nums[1].is_finite() && nums[0] > 0.0 {
                pts.push((nums[0], nums[1]));
            }
        }
        pts.sort_by(|a, b| a.0.total_cmp(&b.0));
        pts.dedup_by(|a, b| a.0 == b.0);
        (pts.len() >= 2).then_some(RefCurve { points: pts })
    }

    /// Build directly from points (already validated/generated). Sorts to keep
    /// the ascending-frequency invariant.
    #[must_use]
    pub fn from_points(mut points: Vec<(f64, f64)>) -> RefCurve {
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        RefCurve { points }
    }

    /// Level (dB) at `hz`, linearly interpolated in log-frequency. Clamps flat
    /// to the nearest endpoint outside the measured range (no extrapolation).
    #[must_use]
    pub fn interp(&self, hz: f64) -> f64 {
        let p = &self.points;
        let n = p.len();
        if n == 0 {
            return 0.0;
        }
        if hz <= p[0].0 {
            return p[0].1;
        }
        if hz >= p[n - 1].0 {
            return p[n - 1].1;
        }
        let i = p.partition_point(|&(f, _)| f < hz).max(1);
        let (f0, d0) = p[i - 1];
        let (f1, d1) = p[i];
        let t = (hz.ln() - f0.ln()) / (f1.ln() - f0.ln());
        d0 + t * (d1 - d0)
    }

    /// Resample onto `n` log-spaced points over `[log_min, log_max]` (log10 Hz).
    /// Returns `(log10(freq), dB)` — the form the GUI draws with.
    #[must_use]
    pub fn resample_log(&self, n: usize, log_min: f64, log_max: f64) -> Vec<(f64, f64)> {
        let n = n.max(2);
        (0..n)
            .map(|i| {
                let lf = log_min + (i as f64 / (n - 1) as f64) * (log_max - log_min);
                (lf, self.interp(10f64.powf(lf)))
            })
            .collect()
    }

    /// Mean level (dB) over `[log_min, log_max]`, sampled on a fixed grid.
    #[must_use]
    pub fn mean_db(&self, log_min: f64, log_max: f64) -> f64 {
        const N: usize = 64;
        let s: f64 = (0..N)
            .map(|i| {
                let lf = log_min + (i as f64 / (N - 1) as f64) * (log_max - log_min);
                self.interp(10f64.powf(lf))
            })
            .sum();
        s / N as f64
    }

    /// dB offset that zeroes this curve's mean over the band — so an absolute
    /// SPL curve sits centred on the graph's ± dB axis.
    #[must_use]
    pub fn norm_offset_mean(&self, log_min: f64, log_max: f64) -> f64 {
        -self.mean_db(log_min, log_max)
    }

    /// dB offset that pins this curve to 0 dB at `hz` (`CrinGraph` "normalize at a
    /// frequency"; 500 Hz is the IEC-recommended default).
    #[must_use]
    pub fn norm_offset_at(&self, hz: f64) -> f64 {
        -self.interp(hz)
    }

    /// Fractional-octave smoothing (running average in log-frequency). `oct` is
    /// the window *width* in octaves (e.g. `1.0/24.0`); `<= 0` returns a copy.
    #[must_use]
    pub fn smoothed(&self, oct: f64) -> RefCurve {
        if oct <= 0.0 || self.points.len() < 3 {
            return self.clone();
        }
        let grid = self.resample_log(GRID_N, LOG_MIN, LOG_MAX);
        let step = (LOG_MAX - LOG_MIN) / (GRID_N - 1) as f64;
        let hw = ((oct * 0.5) * std::f64::consts::LOG10_2 / step).round() as isize;
        let pts = (0..GRID_N)
            .map(|i| {
                let lo = (i as isize - hw).max(0) as usize;
                let hi = (i as isize + hw).min(GRID_N as isize - 1) as usize;
                let mean =
                    grid[lo..=hi].iter().map(|&(_, d)| d).sum::<f64>() / (hi - lo + 1) as f64;
                (10f64.powf(grid[i].0), mean)
            })
            .collect();
        RefCurve { points: pts }
    }

    /// Average two curves (e.g. L and R channels) in the **amplitude** domain, so
    /// the mono result never dips below either channel the way a dB average would.
    #[must_use]
    pub fn average(a: &RefCurve, b: &RefCurve) -> RefCurve {
        let pts = (0..GRID_N)
            .map(|i| {
                let lf = LOG_MIN + (i as f64 / (GRID_N - 1) as f64) * (LOG_MAX - LOG_MIN);
                let f = 10f64.powf(lf);
                let la = 10f64.powf(a.interp(f) / 20.0);
                let lb = 10f64.powf(b.interp(f) / 20.0);
                (f, 20.0 * ((la + lb) * 0.5).log10())
            })
            .collect();
        RefCurve { points: pts }
    }
}

// ── Parametric target generation ────────────────────────────────────────────

/// A shaping filter used to *build* a target curve (low-shelf bass, ear-gain
/// peak, treble high-shelf). Mirrors the three-filter model used by the `PEQdB`
/// "Optimized Headphone Target" paper and the `CrinGraph` target customizer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TKind {
    LowShelf,
    Peak,
    HighShelf,
}

#[derive(Debug, Clone, Copy)]
pub struct TFilter {
    pub kind: TKind,
    pub fc: f64,
    pub gain: f64,
    pub q: f64,
}

fn tfilter_db(filt: &TFilter, hz: f64) -> f64 {
    let coeffs = match filt.kind {
        TKind::LowShelf => BiquadCoeffs::low_shelf(filt.fc, filt.gain, filt.q, GEN_SR),
        TKind::Peak => BiquadCoeffs::peaking(filt.fc, filt.gain, filt.q, GEN_SR),
        TKind::HighShelf => BiquadCoeffs::high_shelf(filt.fc, filt.gain, filt.q, GEN_SR),
    };
    coeffs.ok().map_or(0.0, |c| biquad_db(&c, hz))
}

/// Generate a target curve = `base` (or flat) + a spectral `tilt` (dB/octave,
/// pivoting at 1 kHz) + the sum of `filters`. The result is a dense
/// [`RefCurve`] ready to overlay or normalise against.
#[must_use]
pub fn generate_target(base: Option<&RefCurve>, tilt_db_oct: f64, filters: &[TFilter]) -> RefCurve {
    let pts = (0..GRID_N)
        .map(|i| {
            let lf = LOG_MIN + (i as f64 / (GRID_N - 1) as f64) * (LOG_MAX - LOG_MIN);
            let f = 10f64.powf(lf);
            let mut db = base.map_or(0.0, |b| b.interp(f));
            db += tilt_db_oct * (f / 1000.0).log2();
            db += filters.iter().map(|filt| tfilter_db(filt, f)).sum::<f64>();
            (f, db)
        })
        .collect();
    RefCurve { points: pts }
}

/// The on-the-fly customizer's three shaping filters (bass low-shelf, ~2.75 kHz
/// ear-gain peak, treble high-shelf) for the given gains. Fixed
/// frequencies/Q follow the `CrinGraph` target customizer.
#[must_use]
pub fn customizer_filters(bass_db: f64, ear_db: f64, treble_db: f64) -> Vec<TFilter> {
    vec![
        TFilter {
            kind: TKind::LowShelf,
            fc: 105.0,
            gain: bass_db,
            q: 0.707,
        },
        TFilter {
            kind: TKind::Peak,
            fc: 2750.0,
            gain: ear_db,
            q: 1.0,
        },
        TFilter {
            kind: TKind::HighShelf,
            fc: 2500.0,
            gain: treble_db,
            q: 0.42,
        },
    ]
}

/// `PEQdB` Diamond β = Diffuse Field + these filters (paper Table 2).
#[must_use]
pub fn diamond_beta_filters() -> Vec<TFilter> {
    vec![
        TFilter {
            kind: TKind::LowShelf,
            fc: 107.0,
            gain: 11.8,
            q: 0.6,
        },
        TFilter {
            kind: TKind::Peak,
            fc: 2724.0,
            gain: -3.6,
            q: 1.2,
        },
        TFilter {
            kind: TKind::HighShelf,
            fc: 4448.0,
            gain: 2.7,
            q: 0.7,
        },
    ]
}

/// `PEQdB` Ultra = Diffuse Field + these filters (paper Table 1 / Figure 1).
#[must_use]
pub fn ultra_filters() -> Vec<TFilter> {
    vec![
        TFilter {
            kind: TKind::LowShelf,
            fc: 145.0,
            gain: 11.7,
            q: 0.6,
        },
        TFilter {
            kind: TKind::Peak,
            fc: 2700.0,
            gain: -4.6,
            q: 1.2,
        },
        TFilter {
            kind: TKind::HighShelf,
            fc: 4000.0,
            gain: 2.4,
            q: 0.7,
        },
    ]
}

/// Magnitude response (dB) of a normalised biquad (a0 = 1) at `hz`. Mirrors the
/// evaluation in [`crate::fr`] and the preset fitter.
fn biquad_db(c: &BiquadCoeffs, hz: f64) -> f64 {
    use std::f64::consts::PI;
    let w = 2.0 * PI * hz / GEN_SR;
    let (cw, sw) = (w.cos(), w.sin());
    let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
    let nr = c.b0 + c.b1 * cw + c.b2 * c2;
    let ni = -(c.b1 * sw + c.b2 * s2);
    let dr = 1.0 + c.a1 * cw + c.a2 * c2;
    let di = -(c.a1 * sw + c.a2 * s2);
    let den = (dr * dr + di * di).sqrt();
    if den <= f64::EPSILON {
        return 0.0;
    }
    20.0 * ((nr * nr + ni * ni).sqrt() / den).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    // A blank first line, a text header, tab-separated freq/dB/phase rows, then a
    // text footer — the real AudioTools/squig export shape.
    const SAMPLE: &str = "\nFrequency\tdB\tUnweighted\n20.0\t75.0\t0.0\n200.0\t80.0\t0.0\n2000.0\t85.0\t0.0\n20000.0\t70.0\t0.0\nsaved\n";

    #[test]
    fn parses_with_header_and_footer() {
        let c = RefCurve::parse(SAMPLE).expect("parse");
        assert_eq!(c.points.len(), 4);
        assert_eq!(c.points[0], (20.0, 75.0));
        assert_eq!(c.points[3], (20000.0, 70.0));
    }

    #[test]
    fn rejects_curve_with_no_points() {
        assert!(RefCurve::parse("Frequency dB\nsaved\n").is_none());
    }

    #[test]
    // float_cmp: the clamp path returns the stored endpoint level verbatim, so the
    // out-of-range asserts check exact expected literals (0.0 / 10.0).
    #[allow(clippy::float_cmp)]
    fn interp_is_log_linear_and_clamped() {
        let c = RefCurve::from_points(vec![(100.0, 0.0), (1000.0, 10.0)]);
        // Geometric midpoint (~316 Hz) sits halfway in dB.
        assert!((c.interp(316.227) - 5.0).abs() < 0.05);
        // Outside the range clamps flat.
        assert_eq!(c.interp(20.0), 0.0);
        assert_eq!(c.interp(20000.0), 10.0);
    }

    #[test]
    fn resample_log_has_requested_length() {
        let c = RefCurve::from_points(vec![(20.0, 1.0), (20000.0, 2.0)]);
        assert_eq!(c.resample_log(128, LOG_MIN, LOG_MAX).len(), 128);
    }

    #[test]
    fn tilt_makes_lows_louder_than_highs() {
        // A negative dB/oct tilt should leave bass above treble.
        let t = generate_target(None, -1.0, &[]);
        assert!(t.interp(50.0) > t.interp(10000.0));
    }

    #[test]
    fn customizer_bass_raises_the_low_end() {
        let flat = generate_target(None, 0.0, &customizer_filters(0.0, 0.0, 0.0));
        let bassy = generate_target(None, 0.0, &customizer_filters(8.0, 0.0, 0.0));
        assert!(bassy.interp(40.0) > flat.interp(40.0) + 5.0);
        // The shelf shouldn't lift the top octave appreciably.
        assert!((bassy.interp(12000.0) - flat.interp(12000.0)).abs() < 1.0);
    }

    #[test]
    fn amplitude_average_stays_between_channels() {
        let l = RefCurve::from_points(vec![(100.0, 0.0), (10000.0, 0.0)]);
        let r = RefCurve::from_points(vec![(100.0, 6.0), (10000.0, 6.0)]);
        let m = RefCurve::average(&l, &r);
        let v = m.interp(1000.0);
        assert!(v > 0.0 && v < 6.0, "mono {v} should sit between L and R");
    }

    #[test]
    fn diamond_beta_boosts_bass_over_a_flat_df() {
        // With a flat stand-in base, Diamond β's +11.8 dB low-shelf must lift 50 Hz.
        let t = generate_target(None, 0.0, &diamond_beta_filters());
        assert!(
            t.interp(50.0) > 8.0,
            "bass shelf only {:.1} dB",
            t.interp(50.0)
        );
    }
}
