//! `resonance verify` — automated live audio-path verification, replacing
//! by-ear feature checks.
//!
//! For each probe frequency the harness plays a real tone through the system's
//! audio path (`pw-play` on Linux, `afplay` on macOS — so it traverses the
//! daemon exactly like application audio), pulls the daemon's freshest
//! post-DSP output over IPC (`CaptureOutput`), and analyses it:
//!
//! - **pitch** — the FFT peak of the captured tone must sit at the played
//!   frequency (catches sample-rate mismatch / pitch bugs);
//! - **response** — the measured level per tone, offset-normalised (device
//!   and stream volumes cancel out), compared against the EQ's predicted
//!   frequency response — or against a saved **baseline** for features the
//!   prediction doesn't model (`FxSound` effects, convolution): run once with
//!   `--save-baseline`, change one thing, run again with `--baseline` and the
//!   deviation column shows exactly what that change did to the response.
//!
//! Windows note: the daemon owns no audio there (the APO inside audiodg does),
//! so `CaptureOutput` is empty — use `resonanced --measure-loopback` for the
//! real-path measurement instead.

use anyhow::{Context, Result, bail};
use resonance_ipc::{Command, DaemonState, FxEffectId, Response, fr};
use std::fmt::Write as _;
use std::path::PathBuf;

pub struct Options {
    pub freqs: Vec<f64>,
    pub tolerance_db: f64,
    pub amp: f64,
    pub settle_ms: u64,
    pub capture_ms: u64,
    pub save_baseline: Option<String>,
    pub baseline: Option<String>,
    pub json: bool,
}

/// A saved measured response, for A/B (before/after) comparisons.
#[derive(serde::Serialize, serde::Deserialize)]
struct Baseline {
    rate: f64,
    freqs: Vec<f64>,
    measured_db: Vec<f64>,
}

/// One probe tone's measurement.
struct Row {
    freq: f64,
    peak_hz: f64,
    measured_db: f64,
    /// False when the tone was at/below the measurement floor — either not
    /// routed at all, or annihilated by the response under test (deep stopband).
    present: bool,
}

/// Level below which a probe tone counts as absent (−90 dBFS).
const FLOOR_DB: f64 = -90.0;

/// What the measured response is compared against.
enum Mode {
    /// The EQ's predicted curve (only valid when nothing but EQ shapes FR).
    Predicted,
    /// A saved baseline measurement (A/B).
    Baseline(Baseline),
    /// Effects/convolution active and no baseline: measure + pitch-check only.
    MeasureOnly,
}

pub fn run(o: &Options) -> Result<()> {
    if o.freqs.is_empty() {
        bail!("no probe frequencies given");
    }
    let state = match crate::send(Command::GetState)? {
        Response::State(s) => s,
        other => bail!("unexpected reply to GetState: {other:?}"),
    };
    let rate = state.sample_rate;
    if rate <= 0.0 || rate.is_nan() {
        bail!("daemon reports no live sample rate — is audio running?");
    }
    for &f in &o.freqs {
        if f <= 0.0 || f.is_nan() || f >= rate / 2.0 {
            bail!(
                "probe frequency {f} Hz is outside (0, Nyquist {})",
                rate / 2.0
            );
        }
    }

    let mode = pick_mode(&state, o)?;

    // Quiet pre-check: other audio playing pollutes every measurement.
    let (_, pre) = capture((rate * 0.4) as u32)?;
    let noise_db = rms_db(&pre);
    if noise_db > -45.0 {
        eprintln!(
            "warning: system audio is playing ({noise_db:.0} dBFS) — results will be unreliable"
        );
    }

    // Probe every tone.
    let mut rows = Vec::with_capacity(o.freqs.len());
    for &f in &o.freqs {
        rows.push(probe_tone(f, rate, o)?);
    }
    // Every tone at the floor = nothing we played traversed the daemon at all
    // (player missing, or audio not routed through Resonance). A *subset* at
    // the floor is a legitimate measurement (e.g. a low-pass IR's stopband).
    if rows.iter().all(|r| !r.present) {
        bail!(
            "no probe tone reached the daemon — player missing, or audio is not routed \
             through Resonance (on Windows use `resonanced --measure-loopback`)"
        );
    }

    if let Some(path) = &o.save_baseline {
        let b = Baseline {
            rate,
            freqs: rows.iter().map(|r| r.freq).collect(),
            measured_db: rows.iter().map(|r| r.measured_db).collect(),
        };
        std::fs::write(path, serde_json::to_string_pretty(&b)?)
            .with_context(|| format!("write baseline '{path}'"))?;
    }

    report(&state, &mode, &rows, o, rate)
}

/// Decide what to compare against, validating the baseline if given.
fn pick_mode(state: &DaemonState, o: &Options) -> Result<Mode> {
    if let Some(path) = &o.baseline {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("read baseline '{path}'"))?;
        let b: Baseline =
            serde_json::from_str(&text).with_context(|| format!("parse baseline '{path}'"))?;
        if b.freqs.len() != o.freqs.len()
            || b.freqs
                .iter()
                .zip(&o.freqs)
                .any(|(a, c)| (a - c).abs() > 1e-6)
        {
            bail!("baseline '{path}' was measured at different frequencies — re-save it");
        }
        return Ok(Mode::Baseline(b));
    }
    // An effect only shapes the response when it's enabled AND has non-zero
    // intensity (enabled-at-0% is a common resting state and a no-op).
    let effects_active = FxEffectId::ALL.iter().any(|&id| {
        let (intensity, enabled) = state.effects.get(id);
        enabled && intensity.abs() > 1e-6
    }) || state.convolution.as_ref().is_some_and(|c| c.enabled);
    if effects_active && state.enabled {
        eprintln!(
            "note: effects/convolution active — the EQ prediction doesn't model them, so this \
             run measures + pitch-checks only. Use --save-baseline / --baseline for A/B."
        );
        return Ok(Mode::MeasureOnly);
    }
    Ok(Mode::Predicted)
}

/// Play one tone through the system path and measure the daemon's output.
fn probe_tone(freq: f64, rate: f64, o: &Options) -> Result<Row> {
    let dur_ms = o.settle_ms + o.capture_ms + 500;
    let wav = write_tone_wav(freq, rate, o.amp, dur_ms)?;
    let mut child = spawn_player(&wav)?;

    std::thread::sleep(std::time::Duration::from_millis(o.settle_ms + o.capture_ms));
    let frames = (rate * o.capture_ms as f64 / 1000.0) as u32;
    let (cap_rate, samples) = capture(frames)?;

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&wav);

    if (samples.len() as f64) < f64::from(frames) * 0.5 {
        bail!(
            "daemon returned {} of {frames} requested samples — no audio is flowing through \
             Resonance (on Windows use `resonanced --measure-loopback` instead)",
            samples.len()
        );
    }
    let amp = sine_amplitude(&samples, cap_rate, freq);
    let measured_db = 20.0 * amp.max(1e-12).log10();
    let present = measured_db > FLOOR_DB;
    Ok(Row {
        freq,
        peak_hz: if present {
            fft_peak_hz(&samples, cap_rate, freq)
        } else {
            0.0
        },
        measured_db: measured_db.max(FLOOR_DB),
        present,
    })
}

/// Compare, print and pass/fail.
fn report(state: &DaemonState, mode: &Mode, rows: &[Row], o: &Options, rate: f64) -> Result<()> {
    let expected: Option<Vec<f64>> = match mode {
        Mode::Predicted => Some(
            rows.iter()
                .map(|r| {
                    if state.enabled {
                        fr::response_db(&state.bands, r.freq, rate)
                    } else {
                        0.0 // power off = bypass = flat
                    }
                })
                .collect(),
        ),
        Mode::Baseline(b) => Some(b.measured_db.clone()),
        Mode::MeasureOnly => None,
    };

    // Absolute levels depend on stream/device gain; only the SHAPE is
    // meaningful. Remove the MEDIAN offset before comparing — the median keeps
    // a few wildly-off tones (e.g. a low-pass stopband) from dragging the
    // reference level away from the well-behaved majority.
    let deviations: Option<Vec<f64>> = expected.as_ref().map(|exp| {
        let mut offsets: Vec<f64> = rows
            .iter()
            .zip(exp)
            .map(|(r, e)| r.measured_db - e)
            .collect();
        offsets.sort_by(f64::total_cmp);
        let offset = offsets[offsets.len() / 2];
        rows.iter()
            .zip(exp)
            .map(|(r, e)| r.measured_db - e - offset)
            .collect()
    });

    // Pitch is only meaningful when the tone survived the chain; a floored
    // tone already fails the response comparison on its own.
    let pitch_ok = |r: &Row| {
        !r.present || (r.peak_hz - r.freq).abs() <= (r.freq * 0.03).max(2.0 * rate / 8192.0)
    };
    let mut all_pass = true;

    let mut out = String::new();
    let header = ["freq", "pitch", "measured", "expected", "dev", "result"];
    let _ = writeln!(
        out,
        "{:>9}  {:>10}  {:>9}  {:>9}  {:>8}  {}",
        header[0], header[1], header[2], header[3], header[4], header[5]
    );
    for (i, r) in rows.iter().enumerate() {
        let p_ok = pitch_ok(r);
        let (exp_s, dev_s, pass) = match (&expected, &deviations) {
            (Some(exp), Some(dev)) => {
                let ok = dev[i].abs() <= o.tolerance_db;
                (
                    format!("{:+8.1}", exp[i]),
                    format!("{:+7.2}", dev[i]),
                    ok && p_ok,
                )
            }
            _ => ("       —".to_string(), "      —".to_string(), p_ok),
        };
        all_pass &= pass;
        let _ = writeln!(
            out,
            "{:>7.0}Hz  {:>8.1}Hz  {:>+8.1}dB  {exp_s}dB  {dev_s}dB  {}",
            r.freq,
            r.peak_hz,
            r.measured_db,
            if pass { "ok" } else { "FAIL" }
        );
    }

    if o.json {
        let rows_json: Vec<serde_json::Value> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                serde_json::json!({
                    "freq": r.freq,
                    "peak_hz": r.peak_hz,
                    "measured_db": r.measured_db,
                    "expected_db": expected.as_ref().map(|e| e[i]),
                    "deviation_db": deviations.as_ref().map(|d| d[i]),
                    "pitch_ok": pitch_ok(r),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "rate": rate,
                "mode": match mode {
                    Mode::Predicted => "predicted",
                    Mode::Baseline(_) => "baseline",
                    Mode::MeasureOnly => "measure-only",
                },
                "rows": rows_json,
                "pass": all_pass,
            })
        );
    } else {
        print!("{out}");
        println!(
            "\n{} ({} tones @ {rate:.0} Hz, tolerance ±{} dB)",
            if all_pass { "PASS" } else { "FAIL" },
            rows.len(),
            o.tolerance_db
        );
    }

    if all_pass {
        Ok(())
    } else {
        bail!("audio-path verification failed")
    }
}

/// Fetch the freshest post-DSP samples from the daemon.
fn capture(frames: u32) -> Result<(f64, Vec<f32>)> {
    match crate::send(Command::CaptureOutput { frames })? {
        Response::Capture { rate, samples } => Ok((rate, samples)),
        Response::Error(e) => bail!("daemon: {e}"),
        other => bail!(
            "daemon does not support CaptureOutput (got {other:?}) — rebuild/restart resonanced"
        ),
    }
}

/// Write a mono float32 sine WAV into the temp dir and return its path.
fn write_tone_wav(freq: f64, rate: f64, amp: f64, dur_ms: u64) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("resonance-verify-{freq:.0}hz.wav"));
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate as u32,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(&path, spec).context("write tone wav")?;
    let n = (rate * dur_ms as f64 / 1000.0) as usize;
    let fade = (rate * 0.01) as usize; // 10 ms fade in/out — no clicks
    for i in 0..n {
        let env = ((i.min(n - 1 - i) as f64) / fade as f64).min(1.0);
        let s = amp * env * (2.0 * std::f64::consts::PI * freq * i as f64 / rate).sin();
        w.write_sample(s as f32)?;
    }
    w.finalize()?;
    Ok(path)
}

/// Start the platform's audio player on the file; audio must route through the
/// system's default output so the daemon processes it like any application.
fn spawn_player(path: &std::path::Path) -> Result<std::process::Child> {
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = std::process::Command::new("pw-play");
        c.arg(path);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("afplay");
        c.arg(path);
        c
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("powershell");
        c.args([
            "-NoProfile",
            "-Command",
            &format!(
                "(New-Object Media.SoundPlayer '{}').PlaySync()",
                path.display()
            ),
        ]);
        c
    };
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("start audio player (pw-play / afplay)")
}

// ── Analysis ─────────────────────────────────────────────────────────────────

fn rms_db(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return -120.0;
    }
    let ms = samples
        .iter()
        .map(|&s| f64::from(s) * f64::from(s))
        .sum::<f64>()
        / samples.len() as f64;
    10.0 * ms.max(1e-24).log10()
}

/// Exact amplitude of the `freq` component via least-squares sine fit: solves
/// `x[n] ≈ a·sin(ωn) + b·cos(ωn)` with the full 2×2 normal equations, so a
/// non-integer number of periods in the window doesn't bias the estimate the
/// way a rectangular DFT bin (scalloping) would.
fn sine_amplitude(samples: &[f32], rate: f64, freq: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * freq / rate;
    let (mut ss, mut sc, mut cc, mut xs, mut xc) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (n, &x) in samples.iter().enumerate() {
        let (s, c) = (w * n as f64).sin_cos();
        let x = f64::from(x);
        ss += s * s;
        sc += s * c;
        cc += c * c;
        xs += x * s;
        xc += x * c;
    }
    let det = ss * cc - sc * sc;
    if det.abs() < 1e-12 {
        return 0.0;
    }
    let a = (xs * cc - xc * sc) / det;
    let b = (xc * ss - xs * sc) / det;
    (a * a + b * b).sqrt()
}

/// Frequency of the strongest spectral component within ±25 % of the probe
/// tone (Hann-windowed FFT argmax with parabolic interpolation). The window
/// keeps concurrent programme material (music) from hijacking the peak while
/// still exposing sample-rate-mismatch shifts (44.1↔48 kHz = 8.8 %, well
/// inside it; larger shifts move the tone out of the window entirely, which
/// the amplitude presence check reports as a hard failure).
fn fft_peak_hz(samples: &[f32], rate: f64, probe_hz: f64) -> f64 {
    use rustfft::{FftPlanner, num_complex::Complex};
    let n = samples.len();
    if n < 16 {
        return 0.0;
    }
    let mut buf: Vec<Complex<f64>> = samples
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let w = 0.5 * (1.0 - (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f64, rate: f64, amp: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / rate).sin()) as f32)
            .collect()
    }

    #[test]
    fn sine_amplitude_is_exact_for_non_integer_periods() {
        // 997 Hz over 0.5 s at 48 kHz is nowhere near an integer period count —
        // the LSQ fit must still recover the amplitude to well under 0.1 dB.
        let x = tone(997.0, 48_000.0, 0.25, 24_000);
        let amp = sine_amplitude(&x, 48_000.0, 997.0);
        let err_db = (20.0 * amp.log10() - 20.0 * 0.25f64.log10()).abs();
        assert!(err_db < 0.05, "amplitude error {err_db} dB");
    }

    #[test]
    fn sine_amplitude_rejects_other_frequencies() {
        let x = tone(1000.0, 48_000.0, 0.25, 24_000);
        let off = sine_amplitude(&x, 48_000.0, 3000.0);
        assert!(
            20.0 * (off / 0.25).log10() < -40.0,
            "a 3 kHz probe must see almost nothing of a 1 kHz tone"
        );
    }

    #[test]
    fn fft_peak_finds_the_tone() {
        let x = tone(440.0, 48_000.0, 0.3, 24_000);
        let p = fft_peak_hz(&x, 48_000.0, 440.0);
        assert!((p - 440.0).abs() < 3.0, "peak at {p} Hz, expected 440");
    }

    #[test]
    fn fft_peak_detects_pitch_shift() {
        // The pitch-bug scenario: audio captured at 44.1 k but replayed as 48 k
        // shifts a 1 kHz tone to ~1088 Hz. The peak detector must report the
        // shifted frequency, not the nominal probe frequency.
        let x = tone(1000.0 * 48_000.0 / 44_100.0, 48_000.0, 0.3, 24_000);
        let p = fft_peak_hz(&x, 48_000.0, 1000.0);
        assert!(
            (p - 1088.4).abs() < 5.0,
            "peak at {p} Hz, expected ~1088 (shifted)"
        );
    }

    #[test]
    fn fft_peak_ignores_louder_out_of_band_content() {
        // Concurrent programme material (e.g. a loud bass line) far from the
        // probe must not hijack the peak search.
        let mut x = tone(2500.0, 48_000.0, 0.1, 24_000);
        let bass = tone(110.0, 48_000.0, 0.8, 24_000);
        for (a, b) in x.iter_mut().zip(bass) {
            *a += b;
        }
        let p = fft_peak_hz(&x, 48_000.0, 2500.0);
        assert!(
            (p - 2500.0).abs() < 10.0,
            "peak at {p} Hz should stay near the 2.5 kHz probe"
        );
    }

    #[test]
    fn rms_db_of_silence_is_floor() {
        assert!(rms_db(&[]) <= -120.0);
        assert!(rms_db(&vec![0.0f32; 1000]) < -100.0);
    }
}
