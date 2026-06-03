/// Black-box audio tests for all five FxSound effects.
///
/// Tests only observable audio properties: frequency content (via FFT),
/// signal levels, inter-channel correlation, and dynamic gain curves.
/// No knowledge of internal implementation is assumed.
use crate::effects::{
    AmbienceEffect, BassBoostEffect, DynamicBoostEffect, Effect, FidelityEffect, SurroundEffect,
};
use rustfft::{FftPlanner, num_complex::Complex};
use std::f64::consts::PI;

const SR: f64 = 48000.0;

// ── Spectral analysis helpers ─────────────────────────────────────────────────

/// Hann-windowed FFT → one-sided magnitude spectrum, peak-normalised by window.
/// Returns magnitudes for bins 0..N/2 representing 0..SR/2.
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

    // Two-sided amplitude → one-sided peak: scale = 2/N (window normalisation)
    let scale = 2.0 / n as f64;
    buf[..n / 2].iter().map(|c| c.norm() * scale).collect()
}

/// Bin index for a given frequency (nearest).
fn bin(freq: f64, n: usize) -> usize {
    ((freq / SR * n as f64).round() as usize).min(n / 2 - 1)
}

/// Peak magnitude in a ±`radius`-bin window around `center_bin`.
fn peak_near(spec: &[f64], center: usize, radius: usize) -> f64 {
    let lo = center.saturating_sub(radius);
    let hi = (center + radius + 1).min(spec.len());
    spec[lo..hi].iter().cloned().fold(0.0f64, f64::max)
}

/// RMS of a sample slice.
fn rms(s: &[f64]) -> f64 {
    (s.iter().map(|x| x * x).sum::<f64>() / s.len() as f64).sqrt()
}

/// Pearson correlation coefficient between two equal-length slices.
fn correlation(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let mean_a = a.iter().sum::<f64>() / n;
    let mean_b = b.iter().sum::<f64>() / n;
    let cov: f64 = a
        .iter()
        .zip(b)
        .map(|(&ai, &bi)| (ai - mean_a) * (bi - mean_b))
        .sum();
    let var_a: f64 = a.iter().map(|&ai| (ai - mean_a).powi(2)).sum();
    let var_b: f64 = b.iter().map(|&bi| (bi - mean_b).powi(2)).sum();
    if var_a == 0.0 || var_b == 0.0 {
        return 1.0;
    }
    cov / (var_a * var_b).sqrt()
}

/// Generate `n` samples of a sine at `freq_hz` with given `amplitude`.
fn sine(freq_hz: f64, n: usize, amplitude: f64) -> Vec<f64> {
    let omega = 2.0 * PI * freq_hz / SR;
    (0..n)
        .map(|i| amplitude * (omega * i as f64).sin())
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// FIDELITY — harmonic exciter
// ─────────────────────────────────────────────────────────────────────────────

/// Fidelity must add odd harmonics to signal above the 3500 Hz crossover.
/// Input 5 kHz → expect detectable energy at 15 kHz (3rd harmonic).
#[test]
fn fidelity_creates_third_harmonic_above_crossover() {
    const N: usize = 32768; // ~682 ms, ~1.46 Hz/bin
    const F0: f64 = 5000.0;

    let raw_input = sine(F0, N, 0.8);

    // Noise floor at 15 kHz before processing
    let spec_in = spectrum(&raw_input);
    let noise_floor = peak_near(&spec_in, bin(15000.0, N), 4);

    let mut fx = FidelityEffect::new(1, SR);
    fx.set_intensity(1.0);
    let mut processed = raw_input.clone();
    fx.process(&mut processed, 1);

    let spec_out = spectrum(&processed);
    let harmonic_mag = peak_near(&spec_out, bin(15000.0, N), 4);

    assert!(
        harmonic_mag > noise_floor * 10.0,
        "fidelity should create 3rd harmonic at 15 kHz: noise_floor={noise_floor:.5}, \
         harmonic={harmonic_mag:.5}"
    );
}

/// Signal well below the 3500 Hz HP crossover should NOT gain significant harmonics.
/// The HP filter rejects it before cubing.
#[test]
fn fidelity_does_not_harmonise_signal_below_crossover() {
    const N: usize = 32768;
    const F0: f64 = 300.0; // far below 3500 Hz crossover

    let raw_input = sine(F0, N, 0.8);

    // Reference: process with ZERO intensity (pure passthrough)
    let mut ref_fx = FidelityEffect::new(1, SR);
    ref_fx.set_intensity(0.0);
    let mut ref_out = raw_input.clone();
    ref_fx.process(&mut ref_out, 1);
    let spec_ref = spectrum(&ref_out);
    let third_harmonic_ref = peak_near(&spec_ref, bin(900.0, N), 4);

    // At full intensity
    let mut fx = FidelityEffect::new(1, SR);
    fx.set_intensity(1.0);
    let mut out = raw_input.clone();
    fx.process(&mut out, 1);
    let spec_out = spectrum(&out);
    let third_harmonic_out = peak_near(&spec_out, bin(900.0, N), 4);

    assert!(
        third_harmonic_out < third_harmonic_ref * 5.0 + 0.001,
        "fidelity below crossover: harmonic at 900 Hz should be negligible \
         (ref={third_harmonic_ref:.5}, got={third_harmonic_out:.5})"
    );
}

/// More fidelity intensity → more harmonic energy at 15 kHz.
#[test]
fn fidelity_harmonic_level_increases_with_intensity() {
    const N: usize = 32768;
    const F0: f64 = 5000.0;
    let input = sine(F0, N, 0.8);

    let harmonic_at = |intensity: f64| {
        let mut fx = FidelityEffect::new(1, SR);
        fx.set_intensity(intensity);
        let mut buf = input.clone();
        fx.process(&mut buf, 1);
        peak_near(&spectrum(&buf), bin(15000.0, N), 4)
    };

    let h_lo = harmonic_at(0.3);
    let h_hi = harmonic_at(1.0);

    assert!(
        h_hi > h_lo,
        "higher intensity should produce more 3rd harmonic: {h_lo:.5} vs {h_hi:.5}"
    );
}

/// Zero intensity must be a bit-exact passthrough.
#[test]
fn fidelity_zero_intensity_exact_passthrough() {
    let input = sine(5000.0, 4096, 0.8);
    let mut fx = FidelityEffect::new(1, SR);
    fx.set_intensity(0.0);
    let mut buf = input.clone();
    fx.process(&mut buf, 1);
    assert_eq!(buf, input);
}

// ─────────────────────────────────────────────────────────────────────────────
// AMBIENCE — Schroeder allpass reverb
// ─────────────────────────────────────────────────────────────────────────────

/// Feed a unit impulse; the output must have non-zero energy well after the
/// impulse (reverb tail extends beyond the original transient).
#[test]
fn ambience_produces_reverb_tail_after_impulse() {
    let n = 4096;
    let mut buf = vec![0.0f64; n];
    buf[0] = 1.0; // unit impulse

    let mut fx = AmbienceEffect::new(1, SR);
    fx.set_intensity(1.0);
    fx.process(&mut buf, 1);

    // Energy in the tail 50–500 samples after the impulse
    let tail_energy: f64 = buf[50..500].iter().map(|x| x * x).sum();
    assert!(
        tail_energy > 1e-6,
        "ambience must produce reverb tail; tail_energy={tail_energy:.2e}"
    );
}

/// Reverb tail energy at higher intensity must be greater than at lower intensity.
#[test]
fn ambience_tail_louder_at_higher_intensity() {
    let n = 2048;

    let tail_energy = |intensity: f64| {
        let mut buf = vec![0.0f64; n];
        buf[0] = 1.0;
        let mut fx = AmbienceEffect::new(1, SR);
        fx.set_intensity(intensity);
        fx.process(&mut buf, 1);
        buf[50..500].iter().map(|x| x * x).sum::<f64>()
    };

    let e_lo = tail_energy(0.2);
    let e_hi = tail_energy(1.0);
    assert!(
        e_hi > e_lo,
        "higher ambience intensity → louder reverb tail: {e_lo:.2e} vs {e_hi:.2e}"
    );
}

/// After driving with signal then cutting to silence, the reverb must decay
/// (i.e., not ring forever). Energy at 50 ms > 200 ms into silence.
#[test]
fn ambience_tail_decays_over_time() {
    let drive = (SR * 0.1) as usize; // 100 ms of signal
    let silence_len = (SR * 0.3) as usize; // 300 ms of silence to measure

    let mut fx = AmbienceEffect::new(2, SR);
    fx.set_intensity(1.0);

    // Drive with a 1 kHz stereo tone
    let omega = 2.0 * PI * 1000.0 / SR;
    let mut drive_buf: Vec<f64> = (0..drive)
        .flat_map(|i| {
            let s = (omega * i as f64).sin();
            [s, s]
        })
        .collect();
    fx.process(&mut drive_buf, 2);

    // Now measure the decay: split silence into four 75 ms windows
    let window = silence_len / 4;
    let mut silence = vec![0.0f64; silence_len * 2];
    fx.process(&mut silence, 2);

    let energy_at = |w: usize| -> f64 {
        let start = w * window * 2;
        let end = start + window * 2;
        silence[start..end].iter().map(|x| x * x).sum()
    };

    let e0 = energy_at(0);
    let e2 = energy_at(2);

    assert!(
        e0 > e2,
        "reverb tail must decay: window-0 energy {e0:.2e} should exceed window-2 energy {e2:.2e}"
    );
}

/// Zero intensity must be a bit-exact passthrough.
#[test]
fn ambience_zero_intensity_exact_passthrough() {
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

// ─────────────────────────────────────────────────────────────────────────────
// SURROUND — Haas-effect stereo widener
// ─────────────────────────────────────────────────────────────────────────────

/// Mono signal (L=R) fed through surround must lose L=R identity.
/// Pearson correlation between L and R channels must decrease below 1.
#[test]
fn surround_reduces_lr_correlation_for_mono_input() {
    let n_frames = 9600; // 200 ms settle
    let measure = 4800;
    let omega = 2.0 * PI * 1000.0 / SR;

    let mut fx = SurroundEffect::new(SR);
    fx.set_intensity(1.0);

    // Settle
    let mut settle: Vec<f64> = (0..n_frames)
        .flat_map(|i| {
            let s = (omega * i as f64).sin();
            [s, s]
        })
        .collect();
    fx.process(&mut settle, 2);

    // Measure
    let mut out: Vec<f64> = (0..measure)
        .flat_map(|i| {
            let s = (omega * (n_frames + i) as f64).sin();
            [s, s]
        })
        .collect();
    fx.process(&mut out, 2);

    let ls: Vec<f64> = out.chunks(2).map(|f| f[0]).collect();
    let rs: Vec<f64> = out.chunks(2).map(|f| f[1]).collect();
    let corr = correlation(&ls, &rs);

    assert!(
        corr < 0.999,
        "surround must decorrelate L and R channels: corr={corr:.5} (expected < 0.999)"
    );
}

/// Higher surround intensity → lower L-R correlation.
#[test]
fn surround_more_decorrelation_at_higher_intensity() {
    let n_frames = 9600;
    let measure = 4800;
    let omega = 2.0 * PI * 1000.0 / SR;

    let correlation_at = |intensity: f64| {
        let mut fx = SurroundEffect::new(SR);
        fx.set_intensity(intensity);

        let mut settle: Vec<f64> = (0..n_frames)
            .flat_map(|i| {
                let s = (omega * i as f64).sin();
                [s, s]
            })
            .collect();
        fx.process(&mut settle, 2);

        let mut out: Vec<f64> = (0..measure)
            .flat_map(|i| {
                let s = (omega * (n_frames + i) as f64).sin();
                [s, s]
            })
            .collect();
        fx.process(&mut out, 2);

        let ls: Vec<f64> = out.chunks(2).map(|f| f[0]).collect();
        let rs: Vec<f64> = out.chunks(2).map(|f| f[1]).collect();
        correlation(&ls, &rs)
    };

    let corr_lo = correlation_at(0.3);
    let corr_hi = correlation_at(1.0);

    assert!(
        corr_lo > corr_hi,
        "higher intensity should produce lower L-R correlation: \
         intensity=0.3 → {corr_lo:.4}, intensity=1.0 → {corr_hi:.4}"
    );
}

/// The Haas delay is ~0.2 ms. Cross-correlating L and R after processing a
/// pure mono signal should show a peak at a lag of approximately that many samples.
#[test]
fn surround_inter_channel_delay_is_detectable() {
    let n_frames = 9600;
    let measure = 4800;
    let omega = 2.0 * PI * 1000.0 / SR;

    let mut fx = SurroundEffect::new(SR);
    fx.set_intensity(1.0);

    let mut settle: Vec<f64> = (0..n_frames)
        .flat_map(|i| {
            let s = (omega * i as f64).sin();
            [s, s]
        })
        .collect();
    fx.process(&mut settle, 2);

    let mut out: Vec<f64> = (0..measure)
        .flat_map(|i| {
            let s = (omega * (n_frames + i) as f64).sin();
            [s, s]
        })
        .collect();
    fx.process(&mut out, 2);

    let ls: Vec<f64> = out.chunks(2).map(|f| f[0]).collect();
    let rs: Vec<f64> = out.chunks(2).map(|f| f[1]).collect();

    // Compute normalised cross-correlation for lags 0..20 samples
    let max_lag = 20usize;
    let search_n = measure - max_lag;
    let xc: Vec<f64> = (0..=max_lag)
        .map(|lag| {
            ls[..search_n]
                .iter()
                .zip(&rs[lag..lag + search_n])
                .map(|(a, b)| a * b)
                .sum::<f64>()
        })
        .collect();

    // Peak at lag 0 means no delay; peak elsewhere means delay
    let peak_lag = xc
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .map(|(i, _)| i)
        .unwrap();

    assert!(
        peak_lag > 0,
        "surround should introduce a non-zero inter-channel delay; peak at lag={peak_lag}"
    );
}

/// Zero intensity must be a bit-exact passthrough.
#[test]
fn surround_zero_intensity_exact_passthrough() {
    let input: Vec<f64> = (0..512)
        .flat_map(|i| {
            let s = (2.0 * PI * 1000.0 / SR * i as f64).sin() * 0.5;
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
// DYNAMIC BOOST — upward compander
// ─────────────────────────────────────────────────────────────────────────────

/// The gain applied to the signal must decrease as input amplitude increases.
/// Tests three amplitude points: 0.02, 0.15, 0.85 (quiet → loud).
#[test]
fn dynamic_boost_gain_curve_is_downward_sloping() {
    let n = (SR * 0.5) as usize; // 500 ms: settle + measure
    let settle = n / 2;

    let gain_at = |amp: f64| {
        let input = sine(1000.0, n, amp);
        let in_rms = rms(&input[settle..]);
        let mut fx = DynamicBoostEffect::new(SR);
        fx.set_intensity(1.0);
        let mut buf = input.clone();
        fx.process(&mut buf, 1);
        let out_rms = rms(&buf[settle..]);
        out_rms / in_rms
    };

    let g_quiet = gain_at(0.02);
    let g_mid = gain_at(0.15);
    let g_loud = gain_at(0.85);

    assert!(
        g_quiet > g_mid,
        "quiet should have more gain than mid: {g_quiet:.3} vs {g_mid:.3}"
    );
    assert!(
        g_mid > g_loud,
        "mid should have more gain than loud: {g_mid:.3} vs {g_loud:.3}"
    );
    assert!(
        g_loud < 1.5,
        "loud signal gain should be near unity: {g_loud:.3}"
    );
}

/// Quiet signal (amplitude << threshold) must receive significant gain.
/// At intensity=1.0 the threshold is 0.4; amplitude=0.02 is well below it.
#[test]
fn dynamic_boost_amplifies_subthreshold_signal() {
    let n = (SR * 0.5) as usize;
    let settle = n / 2;
    let amp = 0.02;

    let input = sine(1000.0, n, amp);
    let in_rms = rms(&input[settle..]);

    let mut fx = DynamicBoostEffect::new(SR);
    fx.set_intensity(1.0);
    let mut buf = input.clone();
    fx.process(&mut buf, 1);
    let out_rms = rms(&buf[settle..]);

    let gain = out_rms / in_rms;
    assert!(
        gain > 1.5,
        "sub-threshold signal should receive ≥1.5× gain; got {gain:.3}"
    );
}

/// After loud burst → quiet signal, gain must recover (attack/release).
/// The first 50 ms of quiet should have lower gain than the next 50 ms.
#[test]
fn dynamic_boost_gain_recovers_from_loud_to_quiet() {
    let sr = SR as usize;
    let omega = 2.0 * PI * 1000.0 / SR;

    let mut fx = DynamicBoostEffect::new(SR);
    fx.set_intensity(1.0);

    // Loud burst: 200 ms at amplitude 0.9
    let mut loud: Vec<f64> = (0..sr / 5)
        .map(|i| 0.9 * (omega * i as f64).sin())
        .collect();
    fx.process(&mut loud, 1);

    // Quiet recovery: 400 ms at amplitude 0.02
    let quiet_n = sr * 2 / 5;
    let amp = 0.02;
    let mut quiet: Vec<f64> = (0..quiet_n)
        .map(|i| amp * (omega * (sr / 5 + i) as f64).sin())
        .collect();
    fx.process(&mut quiet, 1);

    let window = (SR * 0.05) as usize; // 50 ms windows
    let rms_early = rms(&quiet[..window]);
    let rms_late = rms(&quiet[quiet_n - window..]);

    // Early quiet (right after loud) should have lower amplitude than late quiet
    assert!(
        rms_late > rms_early,
        "gain should recover after loud burst: early_rms={rms_early:.5}, late_rms={rms_late:.5}"
    );
}

/// Zero intensity must be a bit-exact passthrough.
#[test]
fn dynamic_boost_zero_intensity_exact_passthrough() {
    let input = sine(1000.0, 512, 0.5);
    let mut fx = DynamicBoostEffect::new(SR);
    fx.set_intensity(0.0);
    let mut buf = input.clone();
    fx.process(&mut buf, 1);
    assert_eq!(buf, input);
}

// ─────────────────────────────────────────────────────────────────────────────
// BASS BOOST — sub-harmonic synthesis + low-shelf
// ─────────────────────────────────────────────────────────────────────────────

/// Input at 120 Hz → should produce detectable sub-octave energy at 60 Hz.
/// This verifies the LP→tanh sub-harmonic synthesis path.
#[test]
fn bass_boost_synthesises_sub_octave_content() {
    const N: usize = 65536; // ~1.37 s, ~0.73 Hz/bin → clean 60/120 Hz resolution
    const F_FUND: f64 = 120.0;
    const F_SUB: f64 = 60.0;

    let input = sine(F_FUND, N, 0.5);

    // Reference: noise floor at 60 Hz without processing
    let spec_in = spectrum(&input);
    let noise_60hz = peak_near(&spec_in, bin(F_SUB, N), 3);

    let mut fx = BassBoostEffect::new(1, SR);
    fx.set_intensity(1.0);
    let mut out = input.clone();
    fx.process(&mut out, 1);

    let spec_out = spectrum(&out);
    let sub_mag = peak_near(&spec_out, bin(F_SUB, N), 3);

    assert!(
        sub_mag > noise_60hz * 10.0 + 0.002,
        "bass boost should synthesise 60 Hz sub-octave from 120 Hz input: \
         noise_floor={noise_60hz:.5}, sub_mag={sub_mag:.5}"
    );
}

/// Low-shelf component: gain at 60 Hz must exceed gain at 5 kHz.
#[test]
fn bass_boost_shelf_boost_at_low_frequency() {
    let n = (SR * 0.6) as usize;
    let settle = n / 2;

    let gain_at = |freq: f64| {
        let input = sine(freq, n, 0.3);
        let in_rms = rms(&input[settle..]);
        let mut fx = BassBoostEffect::new(1, SR);
        fx.set_intensity(1.0);
        let mut buf = input.clone();
        fx.process(&mut buf, 1);
        let out_rms = rms(&buf[settle..]);
        20.0 * (out_rms / in_rms).log10()
    };

    let gain_60 = gain_at(60.0);
    let gain_5k = gain_at(5000.0);

    assert!(
        gain_60 > gain_5k + 1.0,
        "bass boost must boost 60 Hz more than 5 kHz: \
         60Hz={gain_60:.2} dB, 5kHz={gain_5k:.2} dB"
    );
}

/// More intensity → more sub-harmonic energy at 60 Hz.
#[test]
fn bass_boost_sub_harmonic_scales_with_intensity() {
    const N: usize = 65536;
    let input = sine(120.0, N, 0.5);

    let sub_mag_at = |intensity: f64| {
        let mut fx = BassBoostEffect::new(1, SR);
        fx.set_intensity(intensity);
        let mut buf = input.clone();
        fx.process(&mut buf, 1);
        peak_near(&spectrum(&buf), bin(60.0, N), 3)
    };

    let mag_lo = sub_mag_at(0.3);
    let mag_hi = sub_mag_at(1.0);

    assert!(
        mag_hi > mag_lo,
        "higher bass boost intensity → more 60 Hz sub-harmonic: {mag_lo:.5} vs {mag_hi:.5}"
    );
}

/// Zero intensity must be a bit-exact passthrough.
#[test]
fn bass_boost_zero_intensity_exact_passthrough() {
    let input = sine(60.0, 512, 0.5);
    let mut fx = BassBoostEffect::new(1, SR);
    fx.set_intensity(0.0);
    let mut buf = input.clone();
    fx.process(&mut buf, 1);
    assert_eq!(buf, input);
}
