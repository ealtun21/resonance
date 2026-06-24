//! Rate-conversion regression tests (backlog item 15) — the direct guard for
//! the item-14 pitch bug. These run the offline deviceless harness in
//! `test_utils`, so no PipeWire/cpal/CoreAudio device is needed and the results
//! are deterministic + CI-friendly.

use crate::chain::ProcessorChain;
use crate::resample::StreamResampler;
use crate::test_utils::{channel, fft_peak_hz, process_offline, sine, stereo_sine, thd_n_db};

/// Build a flat, all-effects-off passthrough chain at `rate`. The default chain
/// is bit-perfect passthrough (see `chain::tests`), so any frequency shift in
/// the output is attributable to the rate conversion alone.
fn passthrough(rate: f64) -> ProcessorChain {
    ProcessorChain::builder()
        .channels(2)
        .sample_rate(rate)
        .build()
}

/// The rate pairs that exercise the pitch bug: upsample, downsample, the extreme
/// SCO 16 kHz case, and hi-res (88.2/96/176.4/192 kHz) in both directions.
/// `dsp == playback` (the canonical choice), so each case engages exactly one
/// resampler (capture → playback).
const RATE_PAIRS: [(f64, f64); 9] = [
    (44_100.0, 48_000.0),
    (48_000.0, 96_000.0),
    (16_000.0, 48_000.0),
    (48_000.0, 44_100.0),
    // Hi-res: prove rates well above 48 kHz round-trip without pitch error.
    (48_000.0, 192_000.0),
    (192_000.0, 48_000.0),
    (96_000.0, 192_000.0),
    (44_100.0, 176_400.0),
    (88_200.0, 96_000.0),
];

#[test]
fn pitch_preserved_across_mismatched_rates() {
    const TONE: f64 = 1_000.0;
    for (capture, playback) in RATE_PAIRS {
        // ~1.5 s of a 1 kHz tone captured at the source rate.
        let frames = (capture * 1.5) as usize;
        let input = stereo_sine(TONE, capture, frames, 0.5);

        let mut chain = passthrough(playback);
        let out = process_offline(&input, capture, playback, playback, 2, &mut chain);

        let mono = channel(&out, 2, 0);
        let peak = fft_peak_hz(&mono, playback);
        let err = (peak - TONE).abs();
        // Sub-bin peak accuracy (see fft_peak_hz) → 1 Hz here is a tight guard;
        // the actual measured error is ~0.03 Hz (the resampler preserves pitch
        // essentially exactly — a rational-ratio conversion).
        assert!(
            err < 1.0,
            "rate {capture}→{playback}: output peak {peak:.1} Hz, expected {TONE} Hz \
             (err {err:.1} Hz) — pitch shifted!"
        );
    }
}

#[test]
fn old_no_resampler_path_shifts_pitch_then_resampler_fixes_it() {
    // Before/after proof of the macOS bug in one shot.
    //
    // OLD path: the HAL tap captured at 44.1 kHz and pushed samples straight into
    // the ring; the output stream clocked them out at the 48 kHz device rate with
    // NO conversion. Same sample array, faster clock → the playback rate
    // reinterprets the buffer and the pitch shifts by exactly out/in.
    //
    // NEW path: `StreamResampler` converts capture → output rate first, so the
    // pitch is preserved. This test fails on the old behaviour and passes on the
    // new one — the regression guard for the fix.
    const CAPTURE: f64 = 44_100.0;
    const PLAYBACK: f64 = 48_000.0;
    const TONE: f64 = 1_000.0;
    let frames = (CAPTURE * 1.5) as usize;
    let captured = sine(TONE, CAPTURE, frames, 0);

    // OLD (buggy): no resample — the captured samples reinterpreted at the
    // playback rate. The apparent frequency scales by PLAYBACK / CAPTURE.
    let peak_old = fft_peak_hz(&captured, PLAYBACK);
    let shifted = TONE * PLAYBACK / CAPTURE; // ≈ 1088.4 Hz
    assert!(
        (peak_old - shifted).abs() < 5.0 && (peak_old - TONE).abs() > 50.0,
        "old path must shift {TONE} Hz → ~{shifted:.0} Hz (the bug); got {peak_old:.1} Hz"
    );

    // NEW (fixed): resample capture → playback, then measure.
    let mut rs = StreamResampler::<f64>::new(CAPTURE, PLAYBACK, 1);
    let fixed = rs.process(&captured).to_vec();
    let peak_new = fft_peak_hz(&fixed, PLAYBACK);
    assert!(
        (peak_new - TONE).abs() < 5.0,
        "new path must preserve {TONE} Hz (the fix); got {peak_new:.1} Hz"
    );
}

#[test]
fn bypass_is_bit_exact_when_rates_match() {
    // capture == dsp == playback: no resampler engages, passthrough chain →
    // output must equal input bit-for-bit.
    let input = stereo_sine(997.0, 48_000.0, 4096, 0.6);
    let mut chain = passthrough(48_000.0);
    let out = process_offline(&input, 48_000.0, 48_000.0, 48_000.0, 2, &mut chain);
    assert_eq!(
        out, input,
        "matching-rate path must be bit-exact passthrough"
    );
}

#[test]
fn resampler_thd_n_below_floor() {
    // Coherent measurement: fundamental lands exactly on an FFT bin at the
    // output rate. 48000 / 48000 = 1 Hz bins; 1000 Hz → bin 1000.
    const PLAYBACK: f64 = 48_000.0;
    const CAPTURE: f64 = 44_100.0;
    const TONE: f64 = 1_000.0;
    const N: usize = 48_000; // 1 s window at the output rate → 1 Hz bins.

    // Resample a clean mono tone capture→playback (no DSP), then analyse a
    // steady-state window past the resampler's priming + group delay.
    let frames_in = (CAPTURE * 1.6) as usize;
    let mut rs = StreamResampler::<f64>::new(CAPTURE, PLAYBACK, 1);
    let input: Vec<f64> = crate::test_utils::sine(TONE, CAPTURE, frames_in, 0);
    let out = rs.process(&input).to_vec();

    let skip = 4096; // discard leading transient (priming + group delay)
    assert!(out.len() >= skip + N, "not enough output: {}", out.len());
    let window = &out[skip..skip + N];

    let thd_n = thd_n_db(window, PLAYBACK, TONE);
    assert!(
        thd_n < -80.0,
        "resampler THD+N {thd_n:.1} dB exceeds −80 dB floor"
    );
}

#[test]
fn filter_at_zero_gain_is_identity() {
    // A peaking band at 0 dB has numerator == denominator → H(z) = 1, so it
    // must be a bit-exact passthrough. Locks the cookbook coefficient math.
    use crate::filter::{ApoFilter, FilterType};
    let mut f = ApoFilter::builder()
        .filter_type(FilterType::Peaking)
        .freq(1_000.0)
        .gain_db(0.0)
        .q(2.0)
        .channels(1)
        .sample_rate(48_000.0)
        .build()
        .unwrap();
    let input: Vec<f64> = (0..512).map(|i| ((i as f64) * 0.021).sin() * 0.7).collect();
    for &x in &input {
        let y = f.process_channel(x, 0);
        assert_eq!(y.to_bits(), x.to_bits(), "0 dB peaking must be identity");
    }
}
