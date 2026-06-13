//! Fit a GraphicEQ *target curve* to a bank of parametric filters.
//!
//! EqualizerAPO's `GraphicEQ:` directive is a desired magnitude response sampled
//! at a list of frequencies — **not** a set of filters. Realising it as one
//! peaking filter per point is wrong: the bells overlap and sum, so the result
//! is far more extreme than the target. Instead we *fit* a small bank of
//! parametric filters (a broadband preamp, a low-shelf, a high-shelf, and a
//! handful of peaking filters) to the curve so the summed response matches the
//! target. This mirrors AutoEq's parametric export and keeps the result as
//! ordinary, editable EQ bands.
//!
//! The fit is a full nonlinear least-squares optimisation (Levenberg–Marquardt)
//! over every filter's frequency, gain and Q — so sharp resonances are captured,
//! not just broad tilts. It's deliberately compute-heavy: import is a one-time
//! cost, so we seed intelligently (linear shelf pre-fit + greedy residual peak
//! placement), refine with LM, and search several filter counts, keeping the
//! best. A pure-linear fit is the fallback if optimisation doesn't help.

use crate::model::{ApoFilterType, EqBand};
use resonance_dsp::filter::BiquadCoeffs;
use std::f64::consts::PI;

/// Sample rate used to evaluate the prototype filter responses. The fit lives in
/// the audible band where the response is effectively rate-independent; bands are
/// rebuilt at the real playback rate when the chain is constructed.
const FIT_SR: f64 = 48_000.0;
/// Shelf slope (maximally flat) — shelves optimise frequency + gain, not Q.
const SHELF_Q: f64 = 0.707;
/// Drop fitted bands whose |gain| is below this (dB).
const MIN_GAIN_DB: f64 = 0.1;

// Parameter bounds for the optimiser. `FC_MAX` stays well below the 44.1 kHz
// Nyquist (22.05 kHz) so a fitted peak never sits on the edge where the biquad
// response distorts at common playback rates.
const FC_MIN: f64 = 18.0;
const FC_MAX: f64 = 18_000.0;
const Q_MIN: f64 = 0.3;
const Q_MAX: f64 = 10.0;
const GAIN_MAX: f64 = 24.0;
/// Ridge penalty on band gains: breaks degenerate cancelling-pair solutions and
/// keeps the optimiser from parking high-gain peaks at the band edges. Small
/// enough not to bias the fit meaningfully.
const GAIN_RIDGE: f64 = 2e-3;

/// Peak counts to try; the best RMS wins (parsimony breaks ties — see
/// [`fit_graphic_eq`]).
const PEAK_COUNTS: [usize; 5] = [4, 6, 8, 10, 12];
/// Max LM iterations per fit.
const MAX_ITERS: usize = 250;
/// Upper bound on target points fed to the optimiser. The fit cost is roughly
/// O(points × params² × iters × fits), so an unbounded point list from a hostile
/// preset is a CPU denial-of-service on load. Real AutoEq/REW exports are well
/// under this; denser curves are uniformly downsampled, which the fit — placing
/// at most a dozen bands — tolerates easily.
const MAX_FIT_POINTS: usize = 256;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    LowShelf,
    HighShelf,
    Peaking,
}

#[derive(Clone, Copy)]
struct Filt {
    kind: Kind,
    fc: f64,
    gain: f64,
    q: f64,
}

/// Lightweight summary of an EqualizerAPO `GraphicEQ:` target line: point count
/// plus frequency and gain spans. For previews — it does NOT run the (expensive)
/// curve fit that [`fit_graphic_eq`] / import performs.
#[derive(Debug, Clone, Copy)]
pub struct GraphicEqSummary {
    pub points: usize,
    pub min_hz: f64,
    pub max_hz: f64,
    pub min_gain: f64,
    pub max_gain: f64,
}

/// Parse the first `GraphicEQ:` line in `content` into a [`GraphicEqSummary`].
/// `None` if there is no such line or it has no valid `freq gain` points.
/// Accepts both `"freq gain"` and the space-less `"freq-gain"` point forms.
pub fn graphic_eq_summary(content: &str) -> Option<GraphicEqSummary> {
    let line = content
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("GraphicEQ:"))?;
    let rest = line.strip_prefix("GraphicEQ:").unwrap_or("").trim();
    let mut points = 0usize;
    let (mut min_hz, mut max_hz) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut min_gain, mut max_gain) = (f64::INFINITY, f64::NEG_INFINITY);
    for pair in rest.split(';').filter(|p| !p.trim().is_empty()) {
        let mut it = pair.split_whitespace();
        let Some(first) = it.next() else { continue };
        let (fs, gs) = match it.next() {
            Some(g) => (first, g),
            None => match first.find('-') {
                Some(i) => (&first[..i], &first[i..]),
                None => continue,
            },
        };
        if let (Ok(f), Ok(g)) = (fs.parse::<f64>(), gs.parse::<f64>()) {
            points += 1;
            min_hz = min_hz.min(f);
            max_hz = max_hz.max(f);
            min_gain = min_gain.min(g);
            max_gain = max_gain.max(g);
        }
    }
    (points > 0).then_some(GraphicEqSummary {
        points,
        min_hz,
        max_hz,
        min_gain,
        max_gain,
    })
}

/// Fit `points` (frequency Hz, target gain dB) to a parametric bank.
///
/// Returns `(preamp_db, bands)`: the broadband level lands in `preamp_db`, the
/// shape in the bands. Returns an empty band list if there are too few points.
pub fn fit_graphic_eq(points: &[(f64, f64)]) -> (f64, Vec<EqBand>) {
    let mut pts: Vec<(f64, f64)> = points
        .iter()
        .copied()
        .filter(|(f, g)| f.is_finite() && *f > 0.0 && *f < FIT_SR / 2.0 && g.is_finite())
        .collect();
    if pts.len() < 3 {
        return (0.0, Vec::new());
    }
    // Bound optimiser work: downsample a too-dense curve uniformly (keep the
    // endpoints so the fitted span still covers the full range).
    if pts.len() > MAX_FIT_POINTS {
        let stride = pts.len() / MAX_FIT_POINTS + 1;
        let last = pts.len() - 1;
        pts = pts
            .iter()
            .enumerate()
            .filter(|(i, _)| *i % stride == 0 || *i == last)
            .map(|(_, p)| *p)
            .collect();
    }
    let freqs: Vec<f64> = pts.iter().map(|(f, _)| *f).collect();
    let target: Vec<f64> = pts.iter().map(|(_, g)| *g).collect();

    // Try several filter counts; keep the best fit. Prefer fewer bands when the
    // RMS gain from adding more is marginal.
    let mut best: Option<(f64, Vec<Filt>, f64)> = None; // (preamp, filts, rms)
    for &n_peaks in &PEAK_COUNTS {
        let (preamp0, filts0) = seed(&freqs, &target, n_peaks);
        let (preamp, filts, rms) = lm_optimize(preamp0, filts0, &freqs, &target);
        let better = match &best {
            None => true,
            // Accept a larger bank only if it cuts RMS by a worthwhile margin
            // (sub-0.1 dB differences are inaudible — prefer fewer bands).
            Some((_, _, best_rms)) => rms < best_rms - 0.1,
        };
        if better {
            best = Some((preamp, filts, rms));
        }
    }

    let Some((preamp, filts, _)) = best else {
        return (0.0, Vec::new());
    };

    let mut bands: Vec<EqBand> = filts
        .iter()
        .filter(|f| f.gain.abs() >= MIN_GAIN_DB)
        .map(|f| EqBand {
            filter_type: match f.kind {
                Kind::LowShelf => ApoFilterType::LowShelf,
                Kind::HighShelf => ApoFilterType::HighShelf,
                Kind::Peaking => ApoFilterType::Peaking,
            },
            freq: (f.fc * 10.0).round() / 10.0,
            gain_db: (f.gain * 10.0).round() / 10.0,
            q: (f.q * 100.0).round() / 100.0,
            enabled: true,
        })
        .collect();
    bands.sort_by(|a, b| {
        a.freq
            .partial_cmp(&b.freq)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    (((preamp * 10.0).round()) / 10.0, bands)
}

// ── Seeding ──────────────────────────────────────────────────────────────────

/// Build an initial guess: a low-/high-shelf + preamp fitted linearly to the
/// broad shape, then `n_peaks` peaking filters placed greedily at the largest
/// remaining residuals (so sharp features get a dedicated band).
fn seed(freqs: &[f64], target: &[f64], n_peaks: usize) -> (f64, Vec<Filt>) {
    let ls = Filt {
        kind: Kind::LowShelf,
        fc: 105.0,
        gain: 0.0,
        q: SHELF_Q,
    };
    let hs = Filt {
        kind: Kind::HighShelf,
        fc: 10_000.0,
        gain: 0.0,
        q: SHELF_Q,
    };
    // Linear pre-fit of [preamp, ls_gain, hs_gain] against the target.
    let ls_shape: Vec<f64> = freqs.iter().map(|&f| unit_db(&ls, f)).collect();
    let hs_shape: Vec<f64> = freqs.iter().map(|&f| unit_db(&hs, f)).collect();
    let cols: [&[f64]; 2] = [&ls_shape, &hs_shape];
    let n = freqs.len();
    let ncol = 3; // const + 2 shelves
    let at = |c: usize, r: usize| if c == 0 { 1.0 } else { cols[c - 1][r] };
    let mut m = vec![vec![0.0; ncol]; ncol];
    let mut b = vec![0.0; ncol];
    for (i, (mrow, brow)) in m.iter_mut().zip(b.iter_mut()).enumerate() {
        for (j, cell) in mrow.iter_mut().enumerate() {
            *cell = (0..n).map(|r| at(i, r) * at(j, r)).sum();
        }
        *brow = (0..n).map(|r| at(i, r) * target[r]).sum();
    }
    let (mut preamp, ls_gain, hs_gain) = match solve(m, b) {
        Some(g) => (g[0], g[1], g[2]),
        None => (target.iter().sum::<f64>() / n as f64, 0.0, 0.0),
    };

    let mut filts = vec![
        Filt {
            gain: ls_gain,
            ..ls
        },
        Filt {
            gain: hs_gain,
            ..hs
        },
    ];

    // Greedy residual peak placement.
    let mut resid: Vec<f64> = (0..n)
        .map(|r| target[r] - preamp - ls_gain * ls_shape[r] - hs_gain * hs_shape[r])
        .collect();
    for _ in 0..n_peaks {
        let (idx, &peak) = resid
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .unwrap();
        if peak.abs() < 0.05 {
            break;
        }
        let f = Filt {
            kind: Kind::Peaking,
            fc: freqs[idx].clamp(FC_MIN, FC_MAX),
            gain: peak.clamp(-GAIN_MAX, GAIN_MAX),
            q: 3.0,
        };
        for (r, res) in resid.iter_mut().enumerate() {
            *res -= unit_db(&f, freqs[r]) * f.gain;
        }
        filts.push(f);
    }
    // Re-centre preamp on the mean residual so LM starts balanced.
    preamp += resid.iter().sum::<f64>() / n as f64;
    (preamp, filts)
}

// ── Levenberg–Marquardt ────────────────────────────────────────────────────

/// Pack the free parameters into a flat vector: `[preamp, (log10 fc, gain[, log10
/// q])…]`. Shelves contribute fc+gain (Q fixed); peaks contribute fc+gain+q.
fn pack(preamp: f64, filts: &[Filt]) -> Vec<f64> {
    let mut p = vec![preamp];
    for f in filts {
        p.push(f.fc.log10());
        p.push(f.gain);
        if f.kind == Kind::Peaking {
            p.push(f.q.log10());
        }
    }
    p
}

/// Inverse of [`pack`], using `template` for filter kinds/order; clamps to bounds.
fn unpack(p: &[f64], template: &[Filt]) -> (f64, Vec<Filt>) {
    let preamp = p[0];
    let mut i = 1;
    let mut filts = Vec::with_capacity(template.len());
    for t in template {
        let fc = 10f64.powf(p[i]).clamp(FC_MIN, FC_MAX);
        i += 1;
        let gain = p[i].clamp(-GAIN_MAX, GAIN_MAX);
        i += 1;
        let q = if t.kind == Kind::Peaking {
            let q = 10f64.powf(p[i]).clamp(Q_MIN, Q_MAX);
            i += 1;
            q
        } else {
            SHELF_Q
        };
        filts.push(Filt {
            kind: t.kind,
            fc,
            gain,
            q,
        });
    }
    (preamp, filts)
}

fn residuals(p: &[f64], template: &[Filt], freqs: &[f64], target: &[f64]) -> Vec<f64> {
    let (preamp, filts) = unpack(p, template);
    freqs
        .iter()
        .zip(target)
        .map(|(&f, &t)| model_db(preamp, &filts, f) - t)
        .collect()
}

fn cost(r: &[f64]) -> f64 {
    r.iter().map(|x| x * x).sum()
}

/// Refine `(preamp, filts)` to minimise the summed-square error against the
/// target. Returns the optimised parameters and the final RMS error (dB).
fn lm_optimize(
    preamp: f64,
    filts: Vec<Filt>,
    freqs: &[f64],
    target: &[f64],
) -> (f64, Vec<Filt>, f64) {
    let template = filts.clone();
    let mut p = pack(preamp, &filts);
    let n_pts = freqs.len();
    let n_par = p.len();

    // Parameter indices that hold a band gain (for the ridge penalty). Layout
    // matches `pack`: [preamp, (log fc, gain[, log q])…].
    let mut gain_idx = Vec::with_capacity(template.len());
    {
        let mut i = 1;
        for t in &template {
            i += 1; // log fc
            gain_idx.push(i); // gain
            i += 1;
            if t.kind == Kind::Peaking {
                i += 1; // log q
            }
        }
    }

    let mut r = residuals(&p, &template, freqs, target);
    let mut err = cost(&r);
    let mut lambda = 1e-2;

    for _ in 0..MAX_ITERS {
        // Forward-difference Jacobian: J[k][j] = ∂r_k/∂p_j.
        let mut jac = vec![vec![0.0f64; n_par]; n_pts];
        for j in 0..n_par {
            let h = (p[j].abs() * 1e-5).max(1e-6);
            let mut pp = p.clone();
            pp[j] += h;
            let rp = residuals(&pp, &template, freqs, target);
            for k in 0..n_pts {
                jac[k][j] = (rp[k] - r[k]) / h;
            }
        }
        // Normal equations JᵀJ and Jᵀr.
        let mut jtj = vec![vec![0.0f64; n_par]; n_par];
        let mut jtr = vec![0.0f64; n_par];
        for a in 0..n_par {
            for b in a..n_par {
                let s: f64 = (0..n_pts).map(|k| jac[k][a] * jac[k][b]).sum();
                jtj[a][b] = s;
                jtj[b][a] = s;
            }
            jtr[a] = (0..n_pts).map(|k| jac[k][a] * r[k]).sum();
        }
        // Gain ridge: minimise ‖r‖² + GAIN_RIDGE·Σ gainᵢ². Adds to the gain
        // diagonal and pulls each gain toward zero in the gradient.
        for &gi in &gain_idx {
            jtj[gi][gi] += GAIN_RIDGE;
            jtr[gi] += GAIN_RIDGE * p[gi];
        }

        // Inner loop: grow damping until a step reduces the error.
        let mut stepped = false;
        for _ in 0..12 {
            let mut aug = jtj.clone();
            for a in 0..n_par {
                aug[a][a] += lambda * jtj[a][a].max(1e-9);
            }
            let rhs: Vec<f64> = jtr.iter().map(|x| -x).collect();
            let Some(delta) = solve(aug, rhs) else {
                lambda *= 4.0;
                continue;
            };
            let p_new: Vec<f64> = p.iter().zip(&delta).map(|(a, d)| a + d).collect();
            let r_new = residuals(&p_new, &template, freqs, target);
            let err_new = cost(&r_new);
            if err_new < err {
                p = p_new;
                r = r_new;
                err = err_new;
                lambda = (lambda * 0.5).max(1e-9);
                stepped = true;
                break;
            }
            lambda *= 4.0;
            if lambda > 1e9 {
                break;
            }
        }
        if !stepped {
            break;
        }
    }

    let (preamp, filts) = unpack(&p, &template);
    let rms = (err / n_pts as f64).sqrt();
    (preamp, filts, rms)
}

// ── Biquad response helpers ──────────────────────────────────────────────────

fn coeffs_of(f: &Filt) -> Option<BiquadCoeffs> {
    match f.kind {
        Kind::Peaking => BiquadCoeffs::peaking(f.fc, f.gain, f.q, FIT_SR),
        Kind::LowShelf => BiquadCoeffs::low_shelf(f.fc, f.gain, f.q, FIT_SR),
        Kind::HighShelf => BiquadCoeffs::high_shelf(f.fc, f.gain, f.q, FIT_SR),
    }
    .ok()
}

/// dB response of one filter at its own parameters, at frequency `f`.
fn filt_db(filt: &Filt, f: f64) -> f64 {
    coeffs_of(filt).map(|c| biquad_db(&c, f)).unwrap_or(0.0)
}

/// dB response of a filter at a +1 dB reference gain (the shape that scales
/// ~linearly with gain). Used for the linear shelf pre-fit only.
fn unit_db(filt: &Filt, f: f64) -> f64 {
    let unit = Filt { gain: 1.0, ..*filt };
    filt_db(&unit, f)
}

/// Summed model response (dB) at frequency `f`.
fn model_db(preamp: f64, filts: &[Filt], f: f64) -> f64 {
    preamp + filts.iter().map(|x| filt_db(x, f)).sum::<f64>()
}

/// Magnitude response (dB) of a normalised biquad (a0 = 1) at frequency `f`.
fn biquad_db(c: &BiquadCoeffs, f: f64) -> f64 {
    let w = 2.0 * PI * f / FIT_SR;
    let (cw, sw) = (w.cos(), w.sin());
    let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
    let nr = c.b0 + c.b1 * cw + c.b2 * c2;
    let ni = -(c.b1 * sw + c.b2 * s2);
    let dr = 1.0 + c.a1 * cw + c.a2 * c2;
    let di = -(c.a1 * sw + c.a2 * s2);
    let num = (nr * nr + ni * ni).sqrt();
    let den = (dr * dr + di * di).sqrt();
    if den <= f64::EPSILON {
        return 0.0;
    }
    20.0 * (num / den).log10()
}

/// Solve `m x = b` by Gaussian elimination with partial pivoting. `None` if
/// singular.
#[allow(clippy::needless_range_loop)] // index-based elimination reads clearest here
fn solve(mut m: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for col in 0..n {
        let mut piv = col;
        let mut best = m[col][col].abs();
        for r in (col + 1)..n {
            let v = m[r][col].abs();
            if v > best {
                best = v;
                piv = r;
            }
        }
        if best < 1e-12 {
            return None;
        }
        m.swap(col, piv);
        b.swap(col, piv);
        for r in (col + 1)..n {
            let factor = m[r][col] / m[col][col];
            if factor == 0.0 {
                continue;
            }
            for c in col..n {
                m[r][c] -= factor * m[col][c];
            }
            b[r] -= factor * b[col];
        }
    }
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let mut s = b[i];
        for j in (i + 1)..n {
            s -= m[i][j] * x[j];
        }
        x[i] = s / m[i][i];
    }
    Some(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response_db(preamp: f64, bands: &[EqBand], f: f64) -> f64 {
        let mut db = preamp;
        for band in bands {
            let kind = match band.filter_type {
                ApoFilterType::Peaking => Kind::Peaking,
                ApoFilterType::LowShelf => Kind::LowShelf,
                ApoFilterType::HighShelf => Kind::HighShelf,
                _ => continue,
            };
            db += filt_db(
                &Filt {
                    kind,
                    fc: band.freq,
                    gain: band.gain_db,
                    q: band.q,
                },
                f,
            );
        }
        db
    }

    fn rms_against(preamp: f64, bands: &[EqBand], pts: &[(f64, f64)]) -> f64 {
        let sse: f64 = pts
            .iter()
            .map(|(f, t)| (response_db(preamp, bands, *f) - t).powi(2))
            .sum();
        (sse / pts.len() as f64).sqrt()
    }

    #[test]
    fn fits_a_smooth_curve() {
        let ls = Filt {
            kind: Kind::LowShelf,
            fc: 150.0,
            gain: -6.0,
            q: 0.707,
        };
        let pk = Filt {
            kind: Kind::Peaking,
            fc: 3000.0,
            gain: 4.0,
            q: 1.5,
        };
        let pts: Vec<(f64, f64)> = (0..60)
            .map(|i| {
                let f = 20.0 * 2f64.powf(i as f64 / 6.0);
                (f, filt_db(&ls, f) + filt_db(&pk, f))
            })
            .filter(|(f, _)| *f < 20_000.0)
            .collect();
        let (preamp, bands) = fit_graphic_eq(&pts);
        assert!(!bands.is_empty());
        assert!(rms_against(preamp, &bands, &pts) < 0.3);
    }

    #[test]
    fn captures_a_sharp_resonance() {
        // A flat curve with one narrow +6 dB spike at 4.3 kHz — the case the
        // old fixed-Q bank smoothed over. The optimiser should track it.
        let spike = Filt {
            kind: Kind::Peaking,
            fc: 4300.0,
            gain: 6.0,
            q: 7.0,
        };
        let pts: Vec<(f64, f64)> = (0..120)
            .map(|i| {
                let f = 20.0 * 2f64.powf(i as f64 / 12.0);
                (f, filt_db(&spike, f))
            })
            .filter(|(f, _)| *f < 20_000.0)
            .collect();
        let (preamp, bands) = fit_graphic_eq(&pts);
        // Peak of the fitted curve near 4.3 kHz should be clearly boosted.
        let at_peak = response_db(preamp, &bands, 4300.0);
        assert!(at_peak > 4.0, "peak only {at_peak:.1} dB");
        assert!(rms_against(preamp, &bands, &pts) < 0.6);
    }

    #[test]
    fn non_finite_points_are_dropped_not_fitted() {
        // A NaN gain among the points must not panic the seeder's max_by.
        let pts = vec![
            (20.0, -3.0),
            (100.0, f64::NAN),
            (1000.0, 0.0),
            (5000.0, 2.0),
            (f64::INFINITY, 1.0),
        ];
        let (_preamp, _bands) = fit_graphic_eq(&pts);
    }

    #[test]
    fn dense_curve_is_downsampled_not_unbounded() {
        // A curve with far more points than the cap must be bounded before the
        // optimiser: it's downsampled to MAX_FIT_POINTS, so the fit works on a
        // bounded set and returns promptly no matter how dense the input.
        let pts: Vec<(f64, f64)> = (0..5_000)
            .map(|i| (20.0 + i as f64 * 4.0, 0.0))
            .filter(|(f, _)| *f < 20_000.0)
            .collect();
        assert!(pts.len() > MAX_FIT_POINTS);
        let (_preamp, bands) = fit_graphic_eq(&pts);
        assert!(bands.len() <= 14);
    }

    #[test]
    fn includes_shelves() {
        let pts = vec![
            (20.0, -8.0),
            (50.0, -6.0),
            (200.0, -3.0),
            (1000.0, -2.0),
            (4000.0, 3.0),
            (8000.0, -1.0),
            (16000.0, -3.0),
        ];
        let (_preamp, bands) = fit_graphic_eq(&pts);
        assert!(
            bands
                .iter()
                .any(|b| matches!(b.filter_type, ApoFilterType::LowShelf))
        );
    }
}
