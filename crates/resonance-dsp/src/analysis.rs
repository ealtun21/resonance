//! Signal-analysis helpers shared by `resonance verify` and the e2e test
//! agent: FFT cross-correlation alignment, tone-peak detection, spectra.

use rustfft::{FftPlanner, num_complex::Complex};
use std::f64::consts::PI;

fn hann(i: usize, n: usize) -> f64 {
    0.5 * (1.0 - (2.0 * PI * i as f64 / n as f64).cos())
}

/// Frequency of the strongest spectral component within ±25 % of the probe
/// tone (Hann-windowed FFT argmax with parabolic interpolation). The window
/// keeps concurrent programme material (music) from hijacking the peak while
/// still exposing sample-rate-mismatch shifts (44.1↔48 kHz = 8.8 %, well
/// inside it; larger shifts move the tone out of the window entirely, which
/// the amplitude presence check reports as a hard failure).
#[must_use]
pub fn fft_peak_hz(samples: &[f32], rate: f64, probe_hz: f64) -> f64 {
    let n = samples.len();
    if n < 16 {
        return 0.0;
    }
    let mut buf: Vec<Complex<f64>> = samples
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let w = hann(i, n);
            Complex::new(f64::from(s) * w, 0.0)
        })
        .collect();
    FftPlanner::new().plan_fft_forward(n).process(&mut buf);

    let half = n / 2;
    let bin = |hz: f64| (hz * n as f64 / rate) as usize;
    let lo = bin(probe_hz * 0.75).clamp(1, half.saturating_sub(2));
    let hi = bin(probe_hz * 1.25).clamp(lo + 1, half.saturating_sub(1));
    let (mut peak, mut peak_mag) = (lo, 0.0f64);
    for (k, c) in buf.iter().enumerate().take(hi + 1).skip(lo) {
        let m = c.norm();
        if m > peak_mag {
            peak_mag = m;
            peak = k;
        }
    }
    // Parabolic refinement over log magnitudes of the neighbours.
    let mag = |k: usize| buf[k].norm().max(1e-30).ln();
    let delta = if peak > 0 && peak + 1 < half {
        let (a, b, c) = (mag(peak - 1), mag(peak), mag(peak + 1));
        let denom = a - 2.0 * b + c;
        if denom.abs() > 1e-12 {
            (0.5 * (a - c) / denom).clamp(-0.5, 0.5)
        } else {
            0.0
        }
    } else {
        0.0
    };
    (peak as f64 + delta) * rate / n as f64
}

/// Circular cross-correlation `c[L] = Σ a[i]·b[i+L]` via FFT. Length is the next
/// power of two ≥ 2·max(len); `c[0]` is lag 0, `c[m-1]` is lag −1 (wrapped).
#[must_use]
pub fn xcorr_fft(a: &[f64], b: &[f64]) -> Vec<f64> {
    let n = a.len().max(b.len());
    let m = (2 * n).next_power_of_two();
    let mut planner = FftPlanner::new();
    let fwd = planner.plan_fft_forward(m);
    let inv = planner.plan_fft_inverse(m);
    let mut fa = vec![Complex::new(0.0, 0.0); m];
    let mut fb = vec![Complex::new(0.0, 0.0); m];
    for (dst, &s) in fa.iter_mut().zip(a) {
        dst.re = s;
    }
    for (dst, &s) in fb.iter_mut().zip(b) {
        dst.re = s;
    }
    fwd.process(&mut fa);
    fwd.process(&mut fb);
    let mut c: Vec<Complex<f64>> = fa.iter().zip(&fb).map(|(a, b)| a.conj() * b).collect();
    inv.process(&mut c);
    c.iter().map(|z| z.re / m as f64).collect()
}

/// Integer sample lag of `b` relative to `a` (positive = `b` lags `a`), searched
/// over ±`max_lag`, that maximises their cross-correlation.
#[must_use]
pub fn best_integer_lag(a: &[f64], b: &[f64], max_lag: usize) -> isize {
    let c = xcorr_fft(a, b);
    let m = c.len();
    let at = |lag: isize| c[(((lag % m as isize) + m as isize) % m as isize) as usize];
    let range = max_lag.min(m / 2 - 1) as isize;
    let mut best = 0isize;
    let mut best_v = f64::NEG_INFINITY;
    for lag in -range..=range {
        let v = at(lag);
        if v > best_v {
            best_v = v;
            best = lag;
        }
    }
    best
}

/// Hann-windowed power spectrum (`|X[k]|²`), bins `0..n/2`.
#[must_use]
pub fn power_spectrum(x: &[f64]) -> Vec<f64> {
    let n = x.len();
    let mut buf: Vec<Complex<f64>> = x
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let w = hann(i, n);
            Complex::new(s * w, 0.0)
        })
        .collect();
    FftPlanner::new().plan_fft_forward(n).process(&mut buf);
    buf[..n / 2].iter().map(Complex::norm_sqr).collect()
}

/// Mean power spectral density per band, in dB, for the bands
/// `[edges_hz[i], edges_hz[i + 1])`. Hann-windowed and normalised by
/// `rate · Σw²`, so a stationary signal reads the same level at any length,
/// and levels are comparable across sample rates (per Hz). A band containing
/// no FFT bin reads `f64::NEG_INFINITY`.
#[must_use]
pub fn band_levels_db(x: &[f64], rate: f64, edges_hz: &[f64]) -> Vec<f64> {
    let n = x.len();
    if n < 2 || edges_hz.len() < 2 {
        return Vec::new();
    }
    let p = power_spectrum(x);
    let w2: f64 = (0..n).map(|i| hann(i, n).powi(2)).sum();
    let norm = rate * w2;
    let hz_per_bin = rate / n as f64;
    edges_hz
        .windows(2)
        .map(|e| {
            let lo = (e[0] / hz_per_bin).ceil() as usize;
            let hi = ((e[1] / hz_per_bin).ceil() as usize).min(p.len());
            if lo >= hi {
                return f64::NEG_INFINITY;
            }
            let mean = p[lo..hi].iter().sum::<f64>() / (hi - lo) as f64 / norm;
            10.0 * mean.max(1e-300).log10()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u64) -> Vec<f64> {
        let mut s = seed | 1;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                ((s >> 33) as f64 / (1u64 << 31) as f64) - 1.0
            })
            .collect()
    }

    #[test]
    fn band_levels_are_density_normalised_across_rates_and_lengths() {
        let edges = [1000.0, 8000.0];
        // Same per-sample variance spread over twice the bandwidth: 96 k reads
        // 10·log10(2) ≈ 3.01 dB lower per Hz than 48 k.
        let l48 = band_levels_db(&noise(144_000, 3), 48_000.0, &edges)[0];
        let l96 = band_levels_db(&noise(288_000, 5), 96_000.0, &edges)[0];
        assert!((l48 - l96 - 3.0103).abs() < 0.1, "48k {l48} 96k {l96}");
        // Length does not change a density.
        let short = band_levels_db(&noise(96_000, 7), 48_000.0, &edges)[0];
        let long = band_levels_db(&noise(192_000, 9), 48_000.0, &edges)[0];
        assert!((short - long).abs() < 0.1, "short {short} long {long}");
    }

    #[test]
    fn empty_band_reads_negative_infinity() {
        let l = band_levels_db(&noise(4800, 1), 48_000.0, &[30_000.0, 31_000.0]);
        assert_eq!(l, vec![f64::NEG_INFINITY]);
    }

    #[test]
    fn best_integer_lag_recovers_a_known_shift() {
        let a = noise(8192, 7);
        let mut b = vec![0.0; a.len()];
        b[137..].copy_from_slice(&a[..a.len() - 137]);
        assert_eq!(best_integer_lag(&a, &b, 1024), 137);
    }
}
