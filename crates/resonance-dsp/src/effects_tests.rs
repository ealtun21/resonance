use crate::effects::{
    AmbienceEffect, BassBoostEffect, DynamicBoostEffect, Effect, FidelityEffect, LoudnessEffect,
    SurroundEffect,
};
use rustfft::{FftPlanner, num_complex::Complex};
use std::f64::consts::PI;

const SR: f64 = 48000.0;

// ── Spectral helpers ──────────────────────────────────────────────────────────

fn spectrum(signal: &[f64]) -> Vec<f64> {
    let n = signal.len();
    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(n);
    let mut buf: Vec<Complex<f64>> = signal
        .iter()
        .enumerate()
        .map(|(i, &x)| {
            let w = 0.5 * (1.0 - (2.0 * PI * i as f64 / n as f64).cos());
            Complex::new(x * w, 0.0)
        })
        .collect();
    fft.process(&mut buf);
    let scale = 2.0 / n as f64;
    buf[..n / 2].iter().map(|c| c.norm() * scale).collect()
}

fn bin(freq: f64, n: usize) -> usize {
    ((freq / SR * n as f64).round() as usize).min(n / 2 - 1)
}

fn peak_near(spec: &[f64], center: usize, radius: usize) -> f64 {
    let lo = center.saturating_sub(radius);
    let hi = (center + radius + 1).min(spec.len());
    spec[lo..hi].iter().copied().fold(0.0f64, f64::max)
}

fn rms(s: &[f64]) -> f64 {
    (s.iter().map(|x| x * x).sum::<f64>() / s.len() as f64).sqrt()
}

fn sine(freq_hz: f64, n: usize, amplitude: f64) -> Vec<f64> {
    let omega = 2.0 * PI * freq_hz / SR;
    (0..n)
        .map(|i| amplitude * (omega * i as f64).sin())
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// FIDELITY — harmonic exciter: Butterworth HP split + tanh odd + DC-blocked
// squared even path. Even path dominates at low intensity, odd at high.
// ─────────────────────────────────────────────────────────────────────────────

/// 5 kHz input above the HP crossover should gain a 3rd harmonic at 15 kHz (odd path).
#[test]
fn fidelity_creates_odd_harmonic_above_crossover() {
    const N: usize = 32768;
    let raw = sine(5000.0, N, 0.8);
    let noise = peak_near(&spectrum(&raw), bin(15000.0, N), 4);

    let mut fx = FidelityEffect::new(1, SR);
    fx.set_intensity(1.0);
    let mut out = raw.clone();
    fx.process(&mut out, 1);

    let h3 = peak_near(&spectrum(&out), bin(15000.0, N), 4);
    assert!(
        h3 > noise * 10.0,
        "3rd harmonic at 15 kHz expected; noise={noise:.5} got={h3:.5}"
    );
}

/// The odd `sin` exciter is symmetric, so it must not introduce a DC offset.
#[test]
fn fidelity_has_no_dc_offset() {
    const N: usize = 32768;
    let mut out = sine(5000.0, N, 0.8);
    let mut fx = FidelityEffect::new(1, SR);
    fx.set_intensity(0.6);
    fx.process(&mut out, 1);
    let mean = out[2000..].iter().sum::<f64>() / (out.len() - 2000) as f64;
    assert!(mean.abs() < 1e-3, "unexpected DC offset: mean={mean:.6}");
}

/// Signal well below the HP crossover must not gain meaningful harmonics.
#[test]
fn fidelity_does_not_harmonise_signal_below_crossover() {
    const N: usize = 32768;
    let raw = sine(300.0, N, 0.8);
    let spec_ref = spectrum(&raw);
    let h3_ref = peak_near(&spec_ref, bin(900.0, N), 4);

    let mut fx = FidelityEffect::new(1, SR);
    fx.set_intensity(1.0);
    let mut out = raw.clone();
    fx.process(&mut out, 1);

    let h3 = peak_near(&spectrum(&out), bin(900.0, N), 4);
    assert!(
        h3 < h3_ref * 5.0 + 0.001,
        "no 3rd harmonic expected at 900 Hz for 300 Hz input; ref={h3_ref:.5} got={h3:.5}"
    );
}

/// More intensity → more odd-harmonic energy.
#[test]
fn fidelity_harmonic_level_increases_with_intensity() {
    const N: usize = 32768;
    let input = sine(5000.0, N, 0.8);

    let harmonic_at = |intensity: f64| {
        let mut fx = FidelityEffect::new(1, SR);
        fx.set_intensity(intensity);
        let mut buf = input.clone();
        fx.process(&mut buf, 1);
        peak_near(&spectrum(&buf), bin(15000.0, N), 4)
    };

    assert!(
        harmonic_at(1.0) > harmonic_at(0.3),
        "higher intensity must produce more 3rd harmonic"
    );
}

/// Zero intensity must be a bit-exact passthrough.
#[test]
fn fidelity_zero_intensity_passthrough() {
    let input = sine(5000.0, 4096, 0.8);
    let mut fx = FidelityEffect::new(1, SR);
    fx.set_intensity(0.0);
    let mut buf = input.clone();
    fx.process(&mut buf, 1);
    assert_eq!(buf, input);
}

// ─────────────────────────────────────────────────────────────────────────────
// AMBIENCE — Freeverb (8 damped combs + 4 allpass per channel, stereo spread).
//   knob warped ×0.34 (MUSIC_MODE2): decay 0.095→~0.21, wet 0→0.273,
//   dry 0.897→1.0, bypass below raw MIDI 12/127.
// ─────────────────────────────────────────────────────────────────────────────

/// Shortest comb at 48 kHz ≈ 1214 samples; a reverb tail must appear after it returns.
#[test]
fn ambience_produces_reverb_tail_after_impulse() {
    let n = 8192;
    let mut buf = vec![0.0f64; n];
    buf[0] = 1.0;
    let mut fx = AmbienceEffect::new(1, SR);
    fx.set_intensity(1.0);
    fx.process(&mut buf, 1);
    let tail: f64 = buf[1300..8000].iter().map(|x| x * x).sum();
    assert!(
        tail > 1e-6,
        "reverb tail expected after first comb returns; energy={tail:.2e}"
    );
}

/// Higher intensity (larger room/feedback) → louder, longer tail.
#[test]
fn ambience_tail_louder_at_higher_intensity() {
    let energy = |intensity: f64| {
        let n = 16384;
        let mut buf = vec![0.0f64; n];
        buf[0] = 1.0;
        let mut fx = AmbienceEffect::new(1, SR);
        fx.set_intensity(intensity);
        fx.process(&mut buf, 1);
        buf[1..].iter().map(|x| x * x).sum::<f64>()
    };
    let e_lo = energy(0.3);
    let e_hi = energy(1.0);
    assert!(
        e_hi > e_lo,
        "higher intensity → more reverb energy: 0.3→{e_lo:.4} 1.0→{e_hi:.4}"
    );
}

#[test]
fn ambience_tail_decays_over_time() {
    let drive = (SR * 0.1) as usize;
    let silence_len = (SR * 0.4) as usize;
    let mut fx = AmbienceEffect::new(2, SR);
    fx.set_intensity(1.0);

    let omega = 2.0 * PI * 1000.0 / SR;
    let mut drive_buf: Vec<f64> = (0..drive)
        .flat_map(|i| {
            let s = (omega * i as f64).sin();
            [s, s]
        })
        .collect();
    fx.process(&mut drive_buf, 2);

    let window = silence_len / 4;
    let mut silence = vec![0.0f64; silence_len * 2];
    fx.process(&mut silence, 2);

    let energy = |w: usize| -> f64 {
        let s = w * window * 2;
        silence[s..s + window * 2].iter().map(|x| x * x).sum()
    };
    assert!(
        energy(0) > energy(3),
        "reverb tail must decay: e0={:.2e} e3={:.2e}",
        energy(0),
        energy(3)
    );
}

/// Zero intensity must be a bit-exact passthrough.
#[test]
fn ambience_zero_intensity_passthrough() {
    let input: Vec<f64> = sine(1000.0, 512, 0.5)
        .into_iter()
        .flat_map(|s| [s, s])
        .collect();
    let mut fx = AmbienceEffect::new(2, SR);
    fx.set_intensity(0.0);
    let mut buf = input.clone();
    fx.process(&mut buf, 2);
    assert_eq!(buf, input);
}

/// Below the bypass threshold (intensity < ~0.094) output must be a bit-exact passthrough.
#[test]
fn ambience_bypassed_at_very_low_intensity() {
    let n = 1024;
    let input = sine(1000.0, n, 0.5);
    let mut fx = AmbienceEffect::new(1, SR);
    fx.set_intensity(0.05);
    let mut out = input.clone();
    fx.process(&mut out, 1);
    assert_eq!(out, input);
}

/// The first impulse sample is the dry signal scaled by the `FxSound` dry gain
/// (0.897 at full intensity); combs return later.
#[test]
fn ambience_impulse_first_sample_is_dry_gain() {
    let mut buf = vec![0.0f64; 8192];
    buf[0] = 1.0;
    let mut fx = AmbienceEffect::new(1, SR);
    fx.set_intensity(1.0);
    fx.process(&mut buf, 1);
    assert!(
        (buf[0] - 0.897).abs() < 0.01,
        "first impulse output should be dry gain ≈ 0.897; got {:.6}",
        buf[0]
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// SURROUND — bass-protected mid/side widener, bipolar.
//   gain_side = 1 + 2·intensity (≥0), 1 + intensity (<0). HP at 250 Hz on side.
// ─────────────────────────────────────────────────────────────────────────────

fn width_energy(intensity: f64) -> f64 {
    let n_frames = 4800;
    let omega = 2.0 * PI * 1000.0 / SR;
    let stereo: Vec<f64> = (0..n_frames)
        .flat_map(|i| {
            let l = (omega * f64::from(i)).sin();
            let r = (omega * f64::from(i) + 0.5).sin();
            [l, r]
        })
        .collect();
    let mut fx = SurroundEffect::new(SR);
    fx.set_intensity(intensity);
    let mut buf = stereo.clone();
    fx.process(&mut buf, 2);
    buf.chunks(2).map(|f| (f[0] - f[1]).powi(2)).sum::<f64>()
}

/// Positive intensity widens a stereo signal (more L-R difference energy).
#[test]
fn surround_increases_stereo_width_for_stereo_input() {
    assert!(
        width_energy(1.0) > width_energy(0.0),
        "surround must widen stereo at positive intensity"
    );
}

/// Higher positive intensity → greater width.
#[test]
fn surround_more_width_at_higher_intensity() {
    assert!(
        width_energy(1.0) > width_energy(0.3),
        "higher intensity → more L-R width"
    );
}

/// Negative intensity narrows toward mono (less width than the dry signal).
#[test]
fn surround_negative_intensity_narrows() {
    let dry = width_energy(0.0);
    let narrowed = width_energy(-1.0);
    assert!(
        narrowed < dry,
        "negative intensity must narrow: dry={dry:.4} narrowed={narrowed:.4}"
    );
}

/// Zero intensity must be a bit-exact passthrough.
#[test]
fn surround_zero_intensity_passthrough() {
    let input: Vec<f64> = (0..512)
        .flat_map(|i| {
            let s = (2.0 * PI * 1000.0 / SR * f64::from(i)).sin() * 0.5;
            [s, -s]
        })
        .collect();
    let mut fx = SurroundEffect::new(SR);
    fx.set_intensity(0.0);
    let mut buf = input.clone();
    fx.process(&mut buf, 2);
    assert_eq!(buf, input);
}

// ─────────────────────────────────────────────────────────────────────────────
// DYNAMIC BOOST — loudness maximizer: makeup gain + lookahead brickwall limiter.
//   makeup = 10^(intensity·12/20), ceiling 0.9, lookahead 0.75 ms.
// ─────────────────────────────────────────────────────────────────────────────

/// Quiet/mid signals get the full makeup gain; loud signals are limited below it.
#[test]
fn dynamic_boost_makeup_then_limits() {
    let n = (SR * 0.5) as usize;
    let settle = n / 2;
    let makeup = 10.0_f64.powf(12.0 / 20.0); // intensity 1.0

    let gain_at = |amp: f64| {
        let input = sine(1000.0, n, amp);
        let in_rms = rms(&input[settle..]);
        let mut fx = DynamicBoostEffect::new(SR);
        fx.set_intensity(1.0);
        let mut buf = input.clone();
        fx.process(&mut buf, 1);
        rms(&buf[settle..]) / in_rms
    };

    let g_quiet = gain_at(0.02);
    let g_loud = gain_at(0.85);

    assert!(
        (g_quiet - makeup).abs() < 0.2,
        "quiet signal should receive ~full makeup ({makeup:.2}); got {g_quiet:.2}"
    );
    assert!(
        g_loud < g_quiet,
        "loud signal must be limited below quiet gain: loud={g_loud:.2} quiet={g_quiet:.2}"
    );
}

/// Quiet signal must receive significant boost (makeup gain).
#[test]
fn dynamic_boost_amplifies_quiet_signal() {
    let n = (SR * 0.5) as usize;
    let settle = n / 2;
    let input = sine(1000.0, n, 0.02);
    let in_rms = rms(&input[settle..]);
    let mut fx = DynamicBoostEffect::new(SR);
    fx.set_intensity(1.0);
    let mut buf = input.clone();
    fx.process(&mut buf, 1);
    let gain = rms(&buf[settle..]) / in_rms;
    assert!(
        gain > 1.5,
        "quiet signal must get ≥1.5× gain; got {gain:.3}"
    );
}

/// After a loud burst, gain reduction must release during subsequent quiet.
#[test]
fn dynamic_boost_gain_recovers_from_loud_to_quiet() {
    let sr = SR as usize;
    let omega = 2.0 * PI * 1000.0 / SR;
    let mut fx = DynamicBoostEffect::new(SR);
    fx.set_intensity(1.0);

    let mut loud: Vec<f64> = (0..sr / 5)
        .map(|i| 0.9 * (omega * i as f64).sin())
        .collect();
    fx.process(&mut loud, 1);

    let quiet_n = sr * 2 / 5;
    let amp = 0.05;
    let mut quiet: Vec<f64> = (0..quiet_n)
        .map(|i| amp * (omega * (sr / 5 + i) as f64).sin())
        .collect();
    fx.process(&mut quiet, 1);

    let window = (SR * 0.05) as usize;
    let rms_early = rms(&quiet[..window]);
    let rms_late = rms(&quiet[quiet_n - window..]);
    assert!(
        rms_late > rms_early,
        "gain must recover: early={rms_early:.5} late={rms_late:.5}"
    );
}

/// A loud signal must be held at or below the ceiling (peak limiting).
#[test]
fn dynamic_boost_limits_peaks_to_ceiling() {
    let n = (SR * 0.6) as usize;
    let settle = n / 2;
    let input = sine(1000.0, n, 0.9);
    let mut fx = DynamicBoostEffect::new(SR);
    fx.set_intensity(1.0);
    let mut buf = input.clone();
    fx.process(&mut buf, 1);
    let peak = buf[settle..].iter().fold(0.0f64, |m, &x| m.max(x.abs()));
    assert!(
        peak <= 0.9 + 0.05,
        "output peak must stay near ceiling 0.9; got {peak:.4}"
    );
}

/// With intensity>0 the output is delayed by the lookahead (~0.75 ms).
#[test]
fn dynamic_boost_has_lookahead_delay() {
    let delay_samples = ((0.75e-3) * SR).ceil() as usize;
    let n = delay_samples + 200;
    let mut buf = vec![0.0f64; n];
    buf[0] = 0.5;

    let mut fx = DynamicBoostEffect::new(SR);
    fx.set_intensity(0.5);
    fx.process(&mut buf, 1);

    let pre_impulse_energy: f64 = buf[..delay_samples.saturating_sub(1)]
        .iter()
        .map(|x| x * x)
        .sum();
    assert!(
        pre_impulse_energy < 1e-20,
        "output before lookahead window must be silent; energy={pre_impulse_energy:.2e}"
    );
}

/// Zero intensity must be a bit-exact passthrough.
#[test]
fn dynamic_boost_zero_intensity_passthrough() {
    let input = sine(1000.0, 512, 0.5);
    let mut fx = DynamicBoostEffect::new(SR);
    fx.set_intensity(0.0);
    let mut buf = input.clone();
    fx.process(&mut buf, 1);
    assert_eq!(buf, input);
}

// ─────────────────────────────────────────────────────────────────────────────
// BASS BOOST — peaking bell at 90 Hz, Q 2.5, gain = intensity·15 dB, bipolar.
// ─────────────────────────────────────────────────────────────────────────────

fn effect_gain_db(fx: &mut dyn Effect, freq: f64) -> f64 {
    let n = (SR * 0.4) as usize;
    let half = n / 2;
    let omega = 2.0 * PI * freq / SR;
    let mut dummy: Vec<f64> = (0..half).map(|i| (omega * i as f64).sin()).collect();
    fx.process(&mut dummy, 1);
    let mut in_sq = 0.0;
    let mut out_sq = 0.0;
    let mut measure: Vec<f64> = (0..half)
        .map(|i| (omega * (half + i) as f64).sin())
        .collect();
    let orig = measure.clone();
    fx.process(&mut measure, 1);
    for (x, y) in orig.iter().zip(&measure) {
        in_sq += x * x;
        out_sq += y * y;
    }
    20.0 * (out_sq / in_sq).sqrt().log10()
}

/// At full intensity the gain at 90 Hz must be approximately +15 dB.
#[test]
fn bass_boost_peak_gain_at_90hz() {
    let mut fx = BassBoostEffect::new(1, SR);
    fx.set_intensity(1.0);
    let g = effect_gain_db(&mut fx, 90.0);
    assert!(
        (g - 15.0).abs() < 2.0,
        "bass boost at 90 Hz should be +15 dB; got {g:.2} dB"
    );
}

/// Negative intensity cuts the low end (gain < 0 at 90 Hz).
#[test]
fn bass_boost_negative_intensity_cuts() {
    let mut fx = BassBoostEffect::new(1, SR);
    fx.set_intensity(-1.0);
    let g = effect_gain_db(&mut fx, 90.0);
    assert!(
        (g + 15.0).abs() < 2.0,
        "bass cut at 90 Hz should be -15 dB; got {g:.2} dB"
    );
}

/// At 5 kHz (well above the 90 Hz bell) gain must be near 0 dB — it's a bell, not a shelf.
#[test]
fn bass_boost_high_frequency_unchanged() {
    let mut fx = BassBoostEffect::new(1, SR);
    fx.set_intensity(1.0);
    let g = effect_gain_db(&mut fx, 5000.0);
    assert!(
        g.abs() < 1.0,
        "bass boost bell must be ~0 dB at 5 kHz; got {g:.2} dB"
    );
}

/// Higher intensity → higher gain at 100 Hz.
#[test]
fn bass_boost_scales_with_intensity() {
    let gain_at = |intensity: f64| {
        let mut fx = BassBoostEffect::new(1, SR);
        fx.set_intensity(intensity);
        effect_gain_db(&mut fx, 100.0)
    };
    assert!(
        gain_at(0.8) > gain_at(0.3),
        "higher intensity → more bass boost"
    );
}

/// Zero intensity must be a bit-exact passthrough.
#[test]
fn bass_boost_zero_intensity_passthrough() {
    let input = sine(100.0, 512, 0.5);
    let mut fx = BassBoostEffect::new(1, SR);
    fx.set_intensity(0.0);
    let mut buf = input.clone();
    fx.process(&mut buf, 1);
    assert_eq!(buf, input);
}

// ── Loudness compensation ──────────────────────────────────────────────────────

/// Measured gain (dB) of the loudness effect at `freq` for a given `intensity`.
fn loudness_gain_db_at(intensity: f64, freq: f64) -> f64 {
    let mut e = LoudnessEffect::new(1, SR);
    e.set_intensity(intensity);
    let n = 16384;
    let input = sine(freq, n, 0.4);
    let mut out = input.clone();
    e.process(&mut out, 1);
    let skip = n / 4; // drop the biquad warm-up transient
    20.0 * (rms(&out[skip..]) / rms(&input[skip..])).log10()
}

#[test]
fn loudness_zero_intensity_passthrough() {
    for &f in &[60.0, 1000.0, 10000.0] {
        assert!(
            loudness_gain_db_at(0.0, f).abs() < 0.01,
            "loudness off must pass through at {f} Hz"
        );
    }
}

#[test]
fn loudness_boosts_bass_and_treble_relative_to_mid() {
    let bass = loudness_gain_db_at(1.0, 60.0);
    let mid = loudness_gain_db_at(1.0, 1000.0);
    let treble = loudness_gain_db_at(1.0, 12000.0);
    assert!(
        bass > 3.0,
        "bass should boost at full intensity, got {bass:.1} dB"
    );
    assert!(
        treble > 3.0,
        "treble should boost at full intensity, got {treble:.1} dB"
    );
    assert!(
        bass > mid + 2.0 && treble > mid + 2.0,
        "equal-loudness smile: bass {bass:.1} & treble {treble:.1} dB should exceed mid {mid:.1} dB"
    );
}

#[test]
fn loudness_gain_grows_with_intensity() {
    let low = loudness_gain_db_at(0.3, 60.0);
    let high = loudness_gain_db_at(1.0, 60.0);
    assert!(
        high > low && low > 0.0,
        "more intensity = more bass boost ({low:.1} → {high:.1} dB)"
    );
}
