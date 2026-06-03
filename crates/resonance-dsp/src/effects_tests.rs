use crate::{
    effects::{
        AmbienceEffect, BassBoostEffect, DynamicBoostEffect, Effect, FidelityEffect, SurroundEffect,
    },
    test_utils::{rms, sine, stereo_sine},
};
use std::f64::consts::PI;

const SR: f64 = 48000.0;
const SETTLE: usize = 9600; // 200 ms
const MEASURE: usize = 4800; // 100 ms

// ── Fidelity ──────────────────────────────────────────────────────────────────

#[test]
fn fidelity_zero_intensity_passthrough() {
    let mut fx = FidelityEffect::new(1, SR);
    fx.set_intensity(0.0);
    let input: Vec<f64> = sine(2000.0, SR, 512, 0);
    let mut buf = input.clone();
    fx.process(&mut buf, 1);
    assert_eq!(buf, input, "fidelity at 0 intensity must be passthrough");
}

#[test]
fn fidelity_disabled_passthrough() {
    let mut fx = FidelityEffect::new(1, SR);
    fx.set_intensity(1.0);
    fx.set_enabled(false);
    let input: Vec<f64> = sine(5000.0, SR, 512, 0);
    let mut buf = input.clone();
    fx.process(&mut buf, 1);
    assert_eq!(buf, input, "disabled fidelity must be passthrough");
}

#[test]
fn fidelity_adds_content_to_hf_signal() {
    let mut fx = FidelityEffect::new(1, SR);
    fx.set_intensity(1.0);
    let input: Vec<f64> = sine(5000.0, SR, SETTLE + MEASURE, 0);
    let mut buf = input.clone();
    fx.process(&mut buf, 1);
    let max_delta = buf
        .iter()
        .zip(input.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f64, f64::max);
    assert!(
        max_delta > 1e-6,
        "fidelity intensity=1 on HF should add content; max_delta={max_delta}"
    );
}

#[test]
fn fidelity_scales_with_intensity() {
    let input: Vec<f64> = sine(5000.0, SR, SETTLE + MEASURE, 0);
    let rms_delta = |intensity: f64| {
        let mut fx = FidelityEffect::new(1, SR);
        fx.set_intensity(intensity);
        let mut buf = input.clone();
        fx.process(&mut buf, 1);
        rms(&buf
            .iter()
            .zip(input.iter())
            .map(|(a, b)| a - b)
            .collect::<Vec<_>>())
    };
    let delta_lo = rms_delta(0.3);
    let delta_hi = rms_delta(0.9);
    assert!(
        delta_hi > delta_lo,
        "higher fidelity intensity → more effect: {delta_lo:.6} vs {delta_hi:.6}"
    );
}

// ── Ambience ──────────────────────────────────────────────────────────────────

#[test]
fn ambience_zero_intensity_passthrough() {
    let mut fx = AmbienceEffect::new(2, SR);
    fx.set_intensity(0.0);
    let input: Vec<f64> = sine(1000.0, SR, 512, 0)
        .into_iter()
        .flat_map(|s| [s, s])
        .collect();
    let mut buf = input.clone();
    fx.process(&mut buf, 2);
    assert_eq!(buf, input, "ambience at 0 intensity must be passthrough");
}

#[test]
fn ambience_adds_reverb_content() {
    let n = SETTLE + MEASURE;
    let mut fx = AmbienceEffect::new(2, SR);
    fx.set_intensity(1.0);
    let input: Vec<f64> = sine(1000.0, SR, n, 0)
        .into_iter()
        .flat_map(|s| [s, s])
        .collect();
    let mut buf = input.clone();
    fx.process(&mut buf, 2);
    let max_delta = buf
        .iter()
        .zip(input.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f64, f64::max);
    assert!(
        max_delta > 1e-6,
        "ambience intensity=1 should add reverb; max_delta={max_delta}"
    );
}

#[test]
fn ambience_reset_clears_state() {
    let mut fx = AmbienceEffect::new(2, SR);
    fx.set_intensity(1.0);
    let mut buf: Vec<f64> = sine(1000.0, SR, SETTLE, 0)
        .into_iter()
        .flat_map(|s| [s, s])
        .collect();
    fx.process(&mut buf, 2);
    fx.reset();
    let mut silence = vec![0.0f64; 256];
    fx.process(&mut silence, 2);
    let energy: f64 = silence.iter().map(|x| x * x).sum();
    assert!(
        energy < 1e-20,
        "ambience after reset on silence: energy={energy}"
    );
}

// ── Surround ──────────────────────────────────────────────────────────────────

#[test]
fn surround_zero_intensity_passthrough() {
    let mut fx = SurroundEffect::new(SR);
    fx.set_intensity(0.0);
    let input: Vec<f64> = stereo_sine(1000.0, SR, 256, 0.5);
    let mut buf = input.clone();
    fx.process(&mut buf, 2);
    assert_eq!(buf, input, "surround at 0 intensity must be passthrough");
}

#[test]
fn surround_preserves_mono_mid() {
    let n_frames = SETTLE + MEASURE;
    let mut fx = SurroundEffect::new(SR);
    fx.set_intensity(1.0);
    let omega = 2.0 * PI * 1000.0 / SR;
    let input: Vec<f64> = (0..n_frames)
        .flat_map(|i| {
            let s = (omega * i as f64).sin();
            [s, s]
        })
        .collect();
    let mut buf = input.clone();
    fx.process(&mut buf, 2);
    let mid_in = rms(&input
        .chunks(2)
        .map(|f| (f[0] + f[1]) * 0.5)
        .collect::<Vec<_>>());
    let mid_out = rms(&buf
        .chunks(2)
        .map(|f| (f[0] + f[1]) * 0.5)
        .collect::<Vec<_>>());
    let ratio_db = 20.0 * (mid_out / mid_in).log10();
    assert!(
        ratio_db.abs() < 3.0,
        "surround should preserve mid: ratio = {ratio_db:.2} dB"
    );
}

#[test]
fn surround_widens_side_image() {
    let mut fx = SurroundEffect::new(SR);
    fx.set_intensity(1.0);
    // Settle
    let mut settle_buf = stereo_sine(1000.0, SR, SETTLE, 0.5);
    fx.process(&mut settle_buf, 2);
    // Measure
    let input_tail = stereo_sine(1000.0, SR, MEASURE, 0.5);
    let side_in = rms(&input_tail
        .chunks(2)
        .map(|f| (f[0] - f[1]) * 0.5)
        .collect::<Vec<_>>());
    let mut out_tail = input_tail.clone();
    fx.process(&mut out_tail, 2);
    let side_out = rms(&out_tail
        .chunks(2)
        .map(|f| (f[0] - f[1]) * 0.5)
        .collect::<Vec<_>>());
    assert!(
        side_out >= side_in * 0.8,
        "surround should preserve/widen side: {side_in:.4} -> {side_out:.4}"
    );
}

// ── DynamicBoost ──────────────────────────────────────────────────────────────

#[test]
fn dynamic_boost_zero_intensity_passthrough() {
    let mut fx = DynamicBoostEffect::new(SR);
    fx.set_intensity(0.0);
    let input: Vec<f64> = sine(1000.0, SR, 512, 0);
    let mut buf = input.clone();
    fx.process(&mut buf, 1);
    assert_eq!(
        buf, input,
        "dynamic boost at 0 intensity must be passthrough"
    );
}

#[test]
fn dynamic_boost_amplifies_quiet_signal() {
    let amplitude = 0.04; // far below threshold of 0.4
    let input = sine(1000.0, SR, SETTLE + MEASURE, 0)
        .into_iter()
        .map(|x| x * amplitude)
        .collect::<Vec<_>>();
    let in_rms = rms(&input[SETTLE..]);
    let mut fx = DynamicBoostEffect::new(SR);
    fx.set_intensity(1.0);
    let mut buf = input.clone();
    fx.process(&mut buf, 1);
    let out_rms = rms(&buf[SETTLE..]);
    assert!(
        out_rms > in_rms * 1.5,
        "quiet signal should be boosted ≥1.5×: {in_rms:.5} -> {out_rms:.5}"
    );
}

#[test]
fn dynamic_boost_does_not_over_boost_loud_signal() {
    let amplitude = 0.9; // above threshold
    let input = sine(1000.0, SR, SETTLE + MEASURE, 0)
        .into_iter()
        .map(|x| x * amplitude)
        .collect::<Vec<_>>();
    let in_rms = rms(&input[SETTLE..]);
    let mut fx = DynamicBoostEffect::new(SR);
    fx.set_intensity(1.0);
    let mut buf = input.clone();
    fx.process(&mut buf, 1);
    let out_rms = rms(&buf[SETTLE..]);
    let ratio_db = 20.0 * (out_rms / in_rms).log10();
    assert!(
        ratio_db.abs() < 3.0,
        "loud signal must not be over-boosted: {in_rms:.4} -> {out_rms:.4} ({ratio_db:.2} dB)"
    );
}

// ── BassBoost ────────────────────────────────────────────────────────────────

#[test]
fn bass_boost_zero_intensity_passthrough() {
    let mut fx = BassBoostEffect::new(1, SR);
    fx.set_intensity(0.0);
    let input: Vec<f64> = sine(60.0, SR, 512, 0);
    let mut buf = input.clone();
    fx.process(&mut buf, 1);
    assert_eq!(buf, input, "bass boost at 0 intensity must be passthrough");
}

#[test]
fn bass_boost_elevates_low_frequency() {
    let n = SETTLE + MEASURE;
    let input = sine(60.0, SR, n, 0)
        .into_iter()
        .map(|x| x * 0.3)
        .collect::<Vec<_>>();
    let in_rms = rms(&input[SETTLE..]);
    let mut fx = BassBoostEffect::new(1, SR);
    fx.set_intensity(1.0);
    let mut buf = input.clone();
    fx.process(&mut buf, 1);
    let out_rms = rms(&buf[SETTLE..]);
    assert!(
        out_rms > in_rms,
        "bass boost should elevate 60 Hz: {in_rms:.5} -> {out_rms:.5}"
    );
}

#[test]
fn bass_boost_greater_at_60hz_than_8khz() {
    let n = SETTLE + MEASURE;
    let amplitude = 0.3;
    let gain_at = |freq: f64| {
        let input = sine(freq, SR, n, 0)
            .into_iter()
            .map(|x| x * amplitude)
            .collect::<Vec<_>>();
        let in_rms = rms(&input[SETTLE..]);
        let mut fx = BassBoostEffect::new(1, SR);
        fx.set_intensity(1.0);
        let mut buf = input.clone();
        fx.process(&mut buf, 1);
        let out_rms = rms(&buf[SETTLE..]);
        20.0 * (out_rms / in_rms).log10()
    };
    let gain_60 = gain_at(60.0);
    let gain_8k = gain_at(8000.0);
    assert!(
        gain_60 > gain_8k,
        "bass boost should affect 60 Hz more than 8 kHz: {gain_60:.2} dB vs {gain_8k:.2} dB"
    );
}
