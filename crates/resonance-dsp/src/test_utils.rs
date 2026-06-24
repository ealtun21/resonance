#![allow(dead_code)]

use crate::chain::ProcessorChain;
use crate::filter::ApoFilter;
use crate::resample::StreamResampler;
use rustfft::{FftPlanner, num_complex::Complex};
use std::f64::consts::PI;

/// Steady-state RMS gain of `filter` at `freq_hz`, measured over the second
/// half of a 400 ms run (first half is settlement).
pub fn filter_gain_db(filter: &mut ApoFilter, freq_hz: f64, sample_rate: f64) -> f64 {
    let total = (sample_rate * 0.4) as usize;
    let half = total / 2;
    let omega = 2.0 * PI * freq_hz / sample_rate;

    for i in 0..half {
        filter.process_channel((omega * i as f64).sin(), 0);
    }

    let mut in_sq = 0.0f64;
    let mut out_sq = 0.0f64;
    for i in 0..half {
        let x = (omega * (half + i) as f64).sin();
        let y = filter.process_channel(x, 0);
        in_sq += x * x;
        out_sq += y * y;
    }

    20.0 * (out_sq / in_sq).sqrt().log10()
}

/// RMS of a sample slice.
pub fn rms(samples: &[f64]) -> f64 {
    let sq: f64 = samples.iter().map(|x| x * x).sum();
    (sq / samples.len() as f64).sqrt()
}

/// Generate a mono sine at `freq_hz` with `n` samples starting at `offset`.
pub fn sine(freq_hz: f64, sample_rate: f64, n: usize, offset: usize) -> Vec<f64> {
    let omega = 2.0 * PI * freq_hz / sample_rate;
    (0..n)
        .map(|i| (omega * (offset + i) as f64).sin())
        .collect()
}

/// Generate interleaved stereo sine (L = +amplitude, R = -amplitude).
pub fn stereo_sine(freq_hz: f64, sample_rate: f64, frames: usize, amplitude: f64) -> Vec<f64> {
    let omega = 2.0 * PI * freq_hz / sample_rate;
    (0..frames)
        .flat_map(|i| {
            let s = amplitude * (omega * i as f64).sin();
            [s, -s]
        })
        .collect()
}

// ── Rate-conversion test harness (items 14/15) ─────────────────────────────────

/// Extract one channel from an interleaved buffer.
pub fn channel(interleaved: &[f64], channels: usize, ch: usize) -> Vec<f64> {
    interleaved
        .iter()
        .skip(ch)
        .step_by(channels)
        .copied()
        .collect()
}

/// Dominant (max-magnitude) frequency in a mono signal, via a Hann-windowed
/// FFT. The DC bin is excluded. Returns the frequency in Hz — used to prove a
/// rate conversion does not shift pitch (the item-14 pitch-bug guard).
pub fn fft_peak_hz(mono: &[f64], sample_rate: f64) -> f64 {
    let n = mono.len();
    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(n);
    let mut buf: Vec<Complex<f64>> = mono
        .iter()
        .enumerate()
        .map(|(i, &x)| {
            let w = 0.5 * (1.0 - (2.0 * PI * i as f64 / n as f64).cos());
            Complex::new(x * w, 0.0)
        })
        .collect();
    fft.process(&mut buf);
    let half = n / 2;
    let (mut best_bin, mut best_mag) = (1usize, 0.0f64);
    for (k, c) in buf.iter().enumerate().take(half).skip(1) {
        let m = c.norm();
        if m > best_mag {
            best_mag = m;
            best_bin = k;
        }
    }
    // Quadratic (parabolic) interpolation of the magnitude peak across the
    // neighbouring bins → sub-bin frequency accuracy, so the measured peak isn't
    // quantised to the FFT bin grid. This reveals the true conversion accuracy
    // (a rational-ratio resampler preserves frequency essentially exactly; the
    // raw bin-peak only looked off by ≤1 bin of measurement granularity).
    let mut peak = best_bin as f64;
    if best_bin >= 1 && best_bin + 1 < half {
        let a = buf[best_bin - 1].norm();
        let b = buf[best_bin].norm();
        let c = buf[best_bin + 1].norm();
        let denom = a - 2.0 * b + c;
        if denom.abs() > f64::EPSILON {
            let delta = 0.5 * (a - c) / denom;
            if delta.abs() <= 1.0 {
                peak += delta;
            }
        }
    }
    peak * sample_rate / n as f64
}

/// THD+N in dB of a mono tone whose fundamental lands exactly on an FFT bin
/// (coherent sampling — choose `fundamental_hz = m · sample_rate / mono.len()`
/// for integer m so spectral leakage is negligible). Energy in the fundamental
/// bin (±1) is the signal; everything else (excluding DC) is noise+distortion.
/// Lower is better; a high-quality resampler sits well below −80 dB.
pub fn thd_n_db(mono: &[f64], sample_rate: f64, fundamental_hz: f64) -> f64 {
    let n = mono.len();
    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(n);
    let mut buf: Vec<Complex<f64>> = mono.iter().map(|&x| Complex::new(x, 0.0)).collect();
    fft.process(&mut buf);
    let half = n / 2;
    let fund_bin = (fundamental_hz / sample_rate * n as f64).round() as isize;
    let (mut sig, mut noise) = (0.0f64, 0.0f64);
    for (k, c) in buf.iter().enumerate().take(half).skip(1) {
        let p = c.norm_sqr();
        if (k as isize - fund_bin).abs() <= 1 {
            sig += p;
        } else {
            noise += p;
        }
    }
    10.0 * (noise / sig.max(f64::MIN_POSITIVE)).log10()
}

/// Run an interleaved buffer through the full offline pipeline:
/// `capture_hz → resample → ProcessorChain(dsp_hz) → resample → playback_hz`,
/// deviceless. `chain` must already be built at `dsp_hz`. Returns the
/// interleaved playback-rate output. Resamplers bypass when adjacent rates are
/// equal, so the common single-rate path is a pure passthrough.
pub fn process_offline(
    input: &[f64],
    capture_hz: f64,
    dsp_hz: f64,
    playback_hz: f64,
    channels: usize,
    chain: &mut ProcessorChain,
) -> Vec<f64> {
    let mut cap_to_dsp = StreamResampler::<f64>::new(capture_hz, dsp_hz, channels);
    let mut dsp_buf = cap_to_dsp.process(input).to_vec();
    chain.process(&mut dsp_buf);
    let mut dsp_to_play = StreamResampler::<f64>::new(dsp_hz, playback_hz, channels);
    dsp_to_play.process(&dsp_buf).to_vec()
}
