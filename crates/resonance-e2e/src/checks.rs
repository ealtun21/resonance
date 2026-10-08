//! Checks for scenarios that cannot be bit-exact (resampling, events).

use crate::stimulus::PILOT_HZ;
use resonance_dsp::analysis::{band_levels_db, fft_peak_hz};
use rustfft::{FftPlanner, num_complex::Complex};
use std::ops::Range;

/// ±0.01 %: 1/6 of a cent. The 44.1↔48 kHz pitch bug is 8.8 %.
pub const PITCH_TOLERANCE: f64 = 1e-4;
pub const BAND_TOLERANCE_DB: f64 = 0.1;

/// Relative pilot-frequency error of a mono recording at `rate`.
#[must_use]
pub fn pitch_error(samples_ch0: &[f32], rate: f64) -> f64 {
    ((fft_peak_hz(samples_ch0, rate, PILOT_HZ) - PILOT_HZ) / PILOT_HZ).abs()
}

/// Octave band edges from 125 Hz up to `max_hz` (below 125 Hz a 3 s window
/// has too few bins for a ±0.1 dB estimate).
#[must_use]
pub fn octave_edges(max_hz: f64) -> Vec<f64> {
    std::iter::successors(Some(125.0), |&f| Some(f * 2.0))
        .take_while(|&f| f <= max_hz)
        .collect()
}

/// Per-band gain (dB) of a path: level of `output` minus level of the
/// `input` that produced it. Levels are density-normalised, so input and
/// output may be at different sample rates (a resampling path). Compare a
/// live path's gain with the offline render's gain over the same bands.
#[must_use]
pub fn gain_db(
    input: &[f64],
    input_rate: f64,
    output: &[f64],
    output_rate: f64,
    edges: &[f64],
) -> Vec<f64> {
    let li = band_levels_db(input, input_rate, edges);
    let lo = band_levels_db(output, output_rate, edges);
    lo.iter().zip(&li).map(|(o, i)| o - i).collect()
}

fn frame_is_zero(x: &[f32], channels: usize, f: usize) -> bool {
    x[f * channels..(f + 1) * channels]
        .iter()
        .all(|&v| v == 0.0)
}

/// First frame with any non-zero channel (where the stimulus arrived).
#[must_use]
pub fn first_signal_frame(x: &[f32], channels: usize) -> Option<usize> {
    (0..x.len() / channels).find(|&f| !frame_is_zero(x, channels, f))
}

/// Last frame with any non-zero channel.
#[must_use]
pub fn last_signal_frame(x: &[f32], channels: usize) -> Option<usize> {
    (0..x.len() / channels)
        .rev()
        .find(|&f| !frame_is_zero(x, channels, f))
}

/// Number of runs of at least `min_len` frames of all-channel digital silence strictly
/// inside the signal (between its first and last non-zero frame): zero-filled underruns.
#[must_use]
pub fn zero_fill_runs(x: &[f32], channels: usize, min_len: usize) -> usize {
    let (Some(first), Some(last)) = (
        first_signal_frame(x, channels),
        last_signal_frame(x, channels),
    ) else {
        return 0;
    };
    let (mut runs, mut cur) = (0, 0);
    for f in first..=last {
        if frame_is_zero(x, channels, f) {
            cur += 1;
        } else {
            if cur >= min_len {
                runs += 1;
            }
            cur = 0;
        }
    }
    runs
}

/// Longest run of all-channel digital silence inside `frames`, in frames.
#[must_use]
pub fn longest_zero_run(x: &[f32], channels: usize, frames: Range<usize>) -> usize {
    let end = frames.end.min(x.len() / channels);
    let (mut best, mut cur) = (0, 0);
    for f in frames.start..end {
        cur = if frame_is_zero(x, channels, f) {
            cur + 1
        } else {
            0
        };
        best = best.max(cur);
    }
    best
}

/// At least half the frames in `frames` carry signal.
#[must_use]
pub fn is_flowing(x: &[f32], channels: usize, frames: Range<usize>) -> bool {
    let end = frames.end.min(x.len() / channels);
    let n = end.saturating_sub(frames.start);
    n > 0
        && (frames.start..end)
            .filter(|&f| !frame_is_zero(x, channels, f))
            .count()
            * 2
            >= n
}

/// One octave of an estimated transfer function.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransferBand {
    pub from_hz: f64,
    pub gain_db: f64,
    /// Magnitude-squared coherence (1 = the output is a linear function of the input).
    pub coherence: f64,
}

const TRANSFER_BLOCK: usize = 4096;

/// Transfer function from `expected` to `recorded` (`recorded[i + lag]` aligns with
/// `expected[i]`), averaged over Hann-windowed 4096-frame blocks inside `range` (indices of
/// `expected`). H1 estimator per octave: `|sum(Pab)| / sum(Paa)`. Blocks of the recording
/// holding a zero-filled dropout (`>= 16` exactly-zero samples in a row) are skipped and
/// counted: the caller decides whether that many dropouts is acceptable. Alignment-robust
/// where per-band levels over shifted windows are not (the stimulus' sweep dwells ~0.5 s
/// per octave).
#[must_use]
pub fn transfer_bands(
    expected: &[f64],
    recorded: &[f64],
    lag: isize,
    range: Range<usize>,
    rate: f64,
    edges: &[f64],
) -> (Vec<TransferBand>, usize, usize) {
    let n = TRANSFER_BLOCK;
    let fft = FftPlanner::<f64>::new().plan_fft_forward(n);
    let win: Vec<f64> = (0..n)
        .map(|i| 0.5 * (1.0 - (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()))
        .collect();
    let bins = n / 2;
    let (mut paa, mut pbb) = (vec![0.0; bins], vec![0.0; bins]);
    let mut pab = vec![Complex::new(0.0, 0.0); bins];
    let (mut used, mut skipped) = (0usize, 0usize);
    let mut i = range.start;
    while i + n <= range.end {
        let Some(j) = i
            .checked_add_signed(lag)
            .filter(|&j| j + n <= recorded.len())
        else {
            break;
        };
        let rb = &recorded[j..j + n];
        let dropout = rb.windows(16).any(|w| w.iter().all(|&v| v == 0.0));
        if dropout {
            skipped += 1;
        } else {
            let spec = |x: &[f64]| -> Vec<Complex<f64>> {
                let mut b: Vec<Complex<f64>> = x
                    .iter()
                    .zip(&win)
                    .map(|(&v, &w)| Complex::new(v * w, 0.0))
                    .collect();
                fft.process(&mut b);
                b
            };
            let (a, b) = (spec(&expected[i..i + n]), spec(rb));
            for k in 0..bins {
                paa[k] += a[k].norm_sqr();
                pbb[k] += b[k].norm_sqr();
                pab[k] += b[k] * a[k].conj();
            }
            used += 1;
        }
        i += n / 2;
    }
    let bin_hz = rate / n as f64;
    let out = edges
        .iter()
        .map(|&lo| {
            let ks = ((lo / bin_hz) as usize).max(1)..(((2.0 * lo) / bin_hz) as usize).min(bins);
            let (saa, sbb): (f64, f64) = (
                ks.clone().map(|k| paa[k]).sum(),
                ks.clone().map(|k| pbb[k]).sum(),
            );
            let sab: Complex<f64> = ks.map(|k| pab[k]).sum();
            TransferBand {
                from_hz: lo,
                gain_db: 20.0 * (sab.norm() / saa.max(1e-30)).max(1e-30).log10(),
                coherence: sab.norm_sqr() / (saa * sbb).max(1e-30),
            }
        })
        .collect();
    (out, used, skipped)
}

#[cfg(test)]
mod tests {
    #[test]
    fn transfer_of_a_scaled_delayed_copy_is_flat_at_the_scale() {
        let x: Vec<f64> = (0..40_000u64)
            .map(|i| ((i.wrapping_mul(2_654_435_761) >> 7) % 2000) as f64 / 1000.0 - 1.0)
            .collect();
        let lag = 37usize;
        let y: Vec<f64> = (0..x.len() + lag)
            .map(|i| if i >= lag { 0.5 * x[i - lag] } else { 0.0 })
            .collect();
        let (bands, used, skipped) = transfer_bands(
            &x,
            &y,
            lag as isize,
            0..x.len() - 100,
            48_000.0,
            &[125.0, 500.0, 2000.0],
        );
        assert!(used > 5 && skipped == 0);
        for b in bands {
            assert!((b.gain_db + 6.02).abs() < 0.01, "{b:?}");
            assert!(b.coherence > 0.999, "{b:?}");
        }
    }

    #[test]
    fn zero_fill_runs_counts_only_interior_silence() {
        let mut x = vec![0.5f32; 100];
        for v in &mut x[20..60] {
            *v = 0.0;
        }
        for v in &mut x[70..75] {
            *v = 0.0;
        }
        let lead: Vec<f32> = std::iter::repeat_n(0.0, 50)
            .chain(x)
            .chain(std::iter::repeat_n(0.0, 50))
            .collect();
        assert_eq!(zero_fill_runs(&lead, 1, 32), 1);
        assert_eq!(zero_fill_runs(&lead, 1, 4), 2);
        assert_eq!(zero_fill_runs(&[0.0; 10], 1, 1), 0);
    }

    use super::*;
    use crate::stimulus::generate;

    fn ch0(x: &[f32], ch: usize) -> Vec<f64> {
        x.iter().step_by(ch).map(|&v| f64::from(v)).collect()
    }

    #[test]
    fn pitch_error_is_zero_for_the_stimulus_and_catches_the_44k1_48k_bug() {
        let s = generate(48_000, 1, 3.0);
        let body = &s.samples[s.body.clone()];
        assert!(pitch_error(body, 48_000.0) < PITCH_TOLERANCE);
        // Captured at 44.1 k, replayed as 48 k: pilot reads 8.8 % sharp.
        assert!(pitch_error(body, 48_000.0 * 48_000.0 / 44_100.0) > 0.05);
    }

    #[test]
    fn unity_path_has_zero_gain_and_doubling_reads_6db() {
        let s = generate(48_000, 1, 3.0);
        let x = ch0(&s.samples[s.body.clone()], 1);
        let edges = octave_edges(20_000.0);
        assert!(
            gain_db(&x, 48_000.0, &x, 48_000.0, &edges)
                .iter()
                .all(|g| g.abs() < 1e-9)
        );
        let louder: Vec<f64> = x.iter().map(|v| v * 2.0).collect();
        let g = gain_db(&x, 48_000.0, &louder, 48_000.0, &edges);
        assert!(g.iter().all(|g| (g - 6.0206).abs() < 0.01), "{g:?}");
    }

    #[test]
    fn signal_edges_skip_leading_and_trailing_silence() {
        let mut x = vec![0.0f32; 2 * 100];
        x[2 * 10 + 1] = 0.3; // channel 1 only
        x[2 * 70] = -0.2;
        assert_eq!(first_signal_frame(&x, 2), Some(10));
        assert_eq!(last_signal_frame(&x, 2), Some(70));
        assert_eq!(first_signal_frame(&[0.0; 8], 2), None);
    }

    #[test]
    fn zero_runs_and_flow() {
        let mut x = vec![0.5f32; 2 * 1000];
        for v in &mut x[2 * 400..2 * 650] {
            *v = 0.0;
        }
        assert_eq!(longest_zero_run(&x, 2, 0..1000), 250);
        assert!(is_flowing(&x, 2, 900..1000));
        assert!(!is_flowing(&x, 2, 400..650));
    }

    #[test]
    fn octave_edges_start_at_125_hz_and_stop_below_max() {
        let e = octave_edges(10_000.0);
        assert!((e[0] - 125.0).abs() < 1e-9);
        assert!(*e.last().unwrap() <= 10_000.0);
    }
}
