//! Checks for scenarios that cannot be bit-exact (resampling, events).

use crate::stimulus::PILOT_HZ;
use resonance_dsp::analysis::{band_levels_db, fft_peak_hz};
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

#[cfg(test)]
mod tests {
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
