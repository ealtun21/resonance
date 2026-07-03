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
//! A separate **A/B compare** mode (`--save-capture` / `--compare`) plays a
//! deterministic broadband stimulus and captures the whole waveform, so it can
//! catch changes the per-tone response can't: run once with `--save-capture`,
//! change one thing, run again with `--compare` and it reports the per-band
//! tonal delta, the best-aligned null depth, and a verdict — TONAL / PHASE-ONLY
//! (tonally identical, only the timing differs, as with minimum vs linear phase)
//! / identical.
//!
//! On Windows the daemon owns no audio path (the APO inside audiodg does), so
//! instead of `CaptureOutput` the harness plays its tone with a WASAPI output
//! stream and captures the same endpoint's **loopback** (the engine's rendered
//! mix, post-APO) via cpal — the analysis and comparison are identical.

use anyhow::{Context, Result, bail};
use resonance_ipc::{Command, DaemonState, FxEffectId, Response, fr};
use std::fmt::Write as _;

pub struct Options {
    pub freqs: Vec<f64>,
    pub tolerance_db: f64,
    pub amp: f64,
    pub settle_ms: u64,
    pub capture_ms: u64,
    pub save_baseline: Option<String>,
    pub baseline: Option<String>,
    /// Save a full-waveform capture of the broadband stimulus for A/B compare.
    pub save_capture: Option<String>,
    /// Compare a fresh stimulus capture against a saved one (phase-audibility).
    pub compare: Option<String>,
    /// Length of the broadband stimulus capture, in seconds (compare mode).
    pub compare_secs: f64,
    pub json: bool,
}

/// A saved full-waveform capture of the broadband stimulus (A/B compare).
#[derive(serde::Serialize, serde::Deserialize)]
struct CaptureFile {
    rate: f64,
    samples: Vec<f32>,
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

    // A/B compare is a broadband full-waveform path, not the tone-probe loop.
    if o.save_capture.is_some() || o.compare.is_some() {
        return run_compare(o, rate);
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
    let pre = ambient_capture(rate, 400)?;
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
            "no probe tone made it through the audio path — player missing, output muted, \
             or audio is not routed through Resonance"
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
    }) || state.convolution.as_ref().is_some_and(|c| c.enabled)
        // A dynamic band's gain depends on the probe level, so the static EQ
        // prediction no longer holds either.
        || state
            .bands
            .iter()
            .any(|b| b.enabled && b.dynamics.is_some());
    if effects_active && state.enabled {
        eprintln!(
            "note: effects/convolution/dynamics active — the EQ prediction doesn't model them, \
             so this run measures + pitch-checks only. Use --save-baseline / --baseline for A/B."
        );
        return Ok(Mode::MeasureOnly);
    }
    Ok(Mode::Predicted)
}

/// Play one tone through the system path and measure the processed output.
fn probe_tone(freq: f64, rate: f64, o: &Options) -> Result<Row> {
    let (cap_rate, samples) = measure_tone(freq, rate, o)?;
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

// ── Broadband A/B compare (issue #57) ────────────────────────────────────────

/// Play a deterministic broadband stimulus, capture the post-DSP waveform, then
/// either save it (`--save-capture`) or compare it against a saved capture
/// (`--compare`) reporting per-band tonal delta, null depth, and a verdict.
fn run_compare(o: &Options, rate: f64) -> Result<()> {
    let secs = o.compare_secs.clamp(0.25, 30.0);

    // A null test is meaningless if other audio is playing.
    let pre = ambient_capture(rate, 400)?;
    if rms_db(&pre) > -45.0 {
        eprintln!("warning: system audio is playing — A/B compare results will be unreliable");
    }

    let (cap_rate, cur) = capture_stimulus(rate, secs, o)?;
    if (cur.len() as f64) < cap_rate * secs * 0.5 {
        bail!(
            "captured only {} samples — audio is not routed through Resonance",
            cur.len()
        );
    }

    if let Some(path) = &o.save_capture {
        let f = CaptureFile {
            rate: cap_rate,
            samples: cur.clone(),
        };
        std::fs::write(path, serde_json::to_string(&f)?)
            .with_context(|| format!("write capture '{path}'"))?;
        if o.compare.is_none() {
            if o.json {
                println!(
                    "{}",
                    serde_json::json!({"saved": path, "rate": cap_rate, "samples": cur.len()})
                );
            } else {
                println!(
                    "saved {}-sample capture to {path} @ {cap_rate:.0} Hz",
                    cur.len()
                );
            }
            return Ok(());
        }
    }

    let path = o
        .compare
        .as_ref()
        .expect("run_compare reached comparison without a --compare path");
    let text = std::fs::read_to_string(path).with_context(|| format!("read capture '{path}'"))?;
    let base: CaptureFile =
        serde_json::from_str(&text).with_context(|| format!("parse capture '{path}'"))?;
    if (base.rate - cap_rate).abs() > 1.0 {
        bail!(
            "baseline capture is {:.0} Hz but the live path is {cap_rate:.0} Hz — re-save it",
            base.rate
        );
    }

    let a: Vec<f64> = base.samples.iter().map(|&s| f64::from(s)).collect();
    let b: Vec<f64> = cur.iter().map(|&s| f64::from(s)).collect();
    report_compare(&a, &b, cap_rate, o);
    Ok(())
}

/// Print the A/B comparison: per-band tonal delta, best-aligned null depth, and
/// the TONAL / PHASE-ONLY / identical verdict.
fn report_compare(a: &[f64], b: &[f64], rate: f64, o: &Options) {
    let bands = band_deltas(a, b, rate);
    let null = null_depth_db(a, b, rate);
    // Reuse the FR tolerance as the per-band tonal threshold (floored so a tight
    // tolerance doesn't flag ordinary measurement noise as a tonal change).
    let tonal_thresh = o.tolerance_db.max(0.25);
    let null_floor = -40.0;
    let v = verdict(&bands, null, tonal_thresh, null_floor);

    if o.json {
        println!(
            "{}",
            serde_json::json!({
                "rate": rate,
                "null_depth_db": null,
                "bands": bands.iter().map(|d| serde_json::json!({
                    "center_hz": d.center_hz, "delta_db": d.delta_db
                })).collect::<Vec<_>>(),
                "verdict": match v {
                    Verdict::Identical => "identical",
                    Verdict::PhaseOnly => "phase-only",
                    Verdict::Tonal => "tonal",
                },
            })
        );
    } else {
        let mut out = String::new();
        let _ = writeln!(out, "{:>9}  {:>8}", "band", "delta");
        for d in &bands {
            let _ = writeln!(out, "{:>7.0}Hz  {:>+6.2}dB", d.center_hz, d.delta_db);
        }
        let _ = writeln!(out, "\nbest-aligned null depth: {null:+.1} dB");
        let label = match v {
            Verdict::Identical => "identical (deep null, matched spectrum)",
            Verdict::PhaseOnly => {
                "PHASE-ONLY difference (tonally identical, timing differs — \
                 audible to sensitive listeners)"
            }
            Verdict::Tonal => "TONAL difference (the magnitude spectrum moved)",
        };
        let _ = writeln!(out, "verdict: {label}");
        print!("{out}");
    }
}

/// Deterministic broadband stimulus (fixed-seed pink-ish noise, faded). Being
/// bit-identical every run is what makes two captures of it directly comparable.
fn stimulus_samples(rate: f64, secs: f64, amp: f64) -> Vec<f32> {
    let n = (rate * secs).max(1.0) as usize;
    let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut lp = 0.0f64;
    let fade = ((rate * 0.02) as usize).max(1); // 20 ms in/out — no clicks
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let white = ((s >> 33) as f64 / (1u64 << 31) as f64) - 1.0; // [-1, 1)
        lp += 0.15 * (white - lp); // gentle low tilt, keeps full-band content
        let env = ((i.min(n.saturating_sub(1) - i)) as f64 / fade as f64).min(1.0);
        out.push((amp * env * (0.6 * white + 0.4 * lp)) as f32);
    }
    out
}

// ── Platform measurement paths ───────────────────────────────────────────────
//
// Unix (Linux/macOS): play the tone with the system player (it routes through
// the daemon like any application) and read the daemon's post-DSP rolling
// buffer over IPC. Windows: the daemon has no audio path (the APO inside
// audiodg does the DSP), so play + capture happen right here over WASAPI —
// an output stream for the tone, loopback capture (post-APO) on the endpoint.

/// One tone's `(rate, mono samples)` measurement of the processed output.
#[cfg(not(windows))]
fn measure_tone(freq: f64, rate: f64, o: &Options) -> Result<(f64, Vec<f32>)> {
    let dur_ms = o.settle_ms + o.capture_ms + 500;
    let wav = write_tone_wav(freq, rate, o.amp, dur_ms)?;
    let mut child = spawn_player(&wav)?;

    std::thread::sleep(std::time::Duration::from_millis(o.settle_ms + o.capture_ms));
    let frames = (rate * o.capture_ms as f64 / 1000.0) as u32;
    let result = capture(frames);

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&wav);

    let (cap_rate, samples) = result?;
    if (samples.len() as f64) < f64::from(frames) * 0.5 {
        bail!(
            "daemon returned {} of {frames} requested samples — no audio is flowing through \
             Resonance",
            samples.len()
        );
    }
    Ok((cap_rate, samples))
}

#[cfg(windows)]
fn measure_tone(freq: f64, _rate: f64, o: &Options) -> Result<(f64, Vec<f32>)> {
    win::tone_and_loopback(freq, o.amp, o.settle_ms, o.capture_ms)
}

/// Play the broadband stimulus and capture `secs` of the processed output.
#[cfg(not(windows))]
fn capture_stimulus(rate: f64, secs: f64, o: &Options) -> Result<(f64, Vec<f32>)> {
    // A tail past the capture window so the captured region sits fully inside
    // the stimulus even with player start-up jitter.
    let stim = stimulus_samples(rate, secs + 1.0, o.amp);
    let wav = write_samples_wav(&stim, rate)?;
    let mut child = spawn_player(&wav)?;
    std::thread::sleep(std::time::Duration::from_millis(
        o.settle_ms + (secs * 1000.0) as u64,
    ));
    let frames = (rate * secs) as u32;
    let result = capture(frames);
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&wav);
    result
}

#[cfg(windows)]
fn capture_stimulus(_rate: f64, secs: f64, o: &Options) -> Result<(f64, Vec<f32>)> {
    win::stimulus_and_loopback(o.amp, o.settle_ms, (secs * 1000.0) as u64)
}

/// Ambient (no tone) capture for the quiet pre-check.
#[cfg(not(windows))]
fn ambient_capture(rate: f64, ms: u64) -> Result<Vec<f32>> {
    Ok(capture((rate * ms as f64 / 1000.0) as u32)?.1)
}

#[cfg(windows)]
fn ambient_capture(_rate: f64, ms: u64) -> Result<Vec<f32>> {
    Ok(win::capture_loopback(ms)?.1)
}

/// Fetch the freshest post-DSP samples from the daemon.
#[cfg(not(windows))]
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
#[cfg(not(windows))]
fn write_tone_wav(freq: f64, rate: f64, amp: f64, dur_ms: u64) -> Result<std::path::PathBuf> {
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

/// Write a mono float32 WAV of arbitrary samples (the broadband stimulus).
#[cfg(not(windows))]
fn write_samples_wav(samples: &[f32], rate: f64) -> Result<std::path::PathBuf> {
    let path = std::env::temp_dir().join("resonance-verify-stimulus.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate as u32,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(&path, spec).context("write stimulus wav")?;
    for &s in samples {
        w.write_sample(s)?;
    }
    w.finalize()?;
    Ok(path)
}

/// Start the platform's audio player on the file; audio must route through the
/// system's default output so the daemon processes it like any application.
#[cfg(not(windows))]
fn spawn_player(path: &std::path::Path) -> Result<std::process::Child> {
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = std::process::Command::new("pw-play");
        c.arg(path);
        c
    };
    #[cfg(not(target_os = "linux"))]
    let mut cmd = {
        let mut c = std::process::Command::new("afplay");
        c.arg(path);
        c
    };
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("start audio player (pw-play / afplay)")
}

/// WASAPI tone playback + loopback capture (Windows only). Loopback yields the
/// engine's rendered mix for the endpoint — i.e. after the Resonance APO has
/// processed it inside audiodg — so this measures what the speakers get.
#[cfg(windows)]
mod win {
    use anyhow::{Context, Result, bail};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    /// What the render stream plays while we capture the loopback.
    enum Source {
        /// Silence (amp 0) — keeps loopback packets flowing for an ambient read.
        Silence,
        /// A steady sine (frequency-response probe).
        Tone(f64, f64),
        /// The deterministic broadband stimulus at the given amplitude (A/B).
        Stimulus(f64),
    }

    /// Capture `ms` of the default render endpoint's loopback, mono-averaged.
    pub fn capture_loopback(ms: u64) -> Result<(f64, Vec<f32>)> {
        run(&Source::Silence, 0, ms)
    }

    /// Play a sine at `freq`/`amp` on the default endpoint, wait `settle_ms`,
    /// then capture `capture_ms` of the endpoint's loopback.
    pub fn tone_and_loopback(
        freq: f64,
        amp: f64,
        settle_ms: u64,
        capture_ms: u64,
    ) -> Result<(f64, Vec<f32>)> {
        run(&Source::Tone(freq, amp), settle_ms, capture_ms)
    }

    /// Play the broadband stimulus, wait `settle_ms`, then capture `capture_ms`.
    pub fn stimulus_and_loopback(
        amp: f64,
        settle_ms: u64,
        capture_ms: u64,
    ) -> Result<(f64, Vec<f32>)> {
        run(&Source::Stimulus(amp), settle_ms, capture_ms)
    }

    fn run(source: &Source, settle_ms: u64, capture_ms: u64) -> Result<(f64, Vec<f32>)> {
        let device = cpal::default_host()
            .default_output_device()
            .context("no default output device")?;
        let config = device
            .default_output_config()
            .context("query output config")?;
        if config.sample_format() != cpal::SampleFormat::F32 {
            // WASAPI's shared-mode mix format is float32 in practice; anything
            // else means an exclusive-mode oddity this harness doesn't handle.
            bail!("unsupported endpoint format {:?}", config.sample_format());
        }
        let rate = f64::from(config.sample_rate());
        let channels = usize::from(config.channels());
        let stream_config: cpal::StreamConfig = config.into();

        // Loopback: WASAPI lets an *output* device open an input stream that
        // yields the engine's rendered (post-APO) mix.
        let (tx, rx) = std::sync::mpsc::channel::<Vec<f32>>();
        let in_stream = device
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let _ = tx.send(data.to_vec());
                },
                |e| eprintln!("loopback capture error: {e}"),
                None,
            )
            .context("open WASAPI loopback capture on the output endpoint")?;
        in_stream.play().context("start loopback capture")?;

        // Render stream — always active: WASAPI loopback only produces packets
        // while something is rendering, so the ambient (no-tone) capture drives
        // the endpoint with silence (amplitude 0) to keep data flowing.
        let (freq, amp) = match source {
            Source::Silence => (440.0, 0.0),
            Source::Tone(f, a) => (*f, *a),
            Source::Stimulus(_) => (0.0, 0.0),
        };
        // The broadband stimulus is pre-generated at the discovered endpoint rate
        // and looped from the buffer; tone/silence use the phase oscillator.
        let stim: Option<Vec<f32>> = match source {
            Source::Stimulus(a) => {
                let secs = (settle_ms + capture_ms) as f64 / 1000.0 + 1.0;
                Some(super::stimulus_samples(rate, secs, *a))
            }
            _ => None,
        };
        let mut phase = 0.0f64;
        let mut idx = 0usize;
        let step = 2.0 * std::f64::consts::PI * freq / rate;
        let out_stream = device
            .build_output_stream(
                &stream_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    for frame in data.chunks_mut(channels) {
                        let s = if let Some(buf) = &stim {
                            let v = buf[idx % buf.len()];
                            idx += 1;
                            v
                        } else {
                            let v = (amp * phase.sin()) as f32;
                            phase = (phase + step) % (2.0 * std::f64::consts::PI);
                            v
                        };
                        frame.fill(s);
                    }
                },
                |e| eprintln!("tone playback error: {e}"),
                None,
            )
            .context("open tone output stream")?;
        out_stream.play().context("start tone")?;

        std::thread::sleep(std::time::Duration::from_millis(settle_ms));
        // Drop whatever arrived during settle; keep only steady-state audio.
        while rx.try_recv().is_ok() {}

        let want = (rate * capture_ms as f64 / 1000.0) as usize;
        let mut mono = Vec::with_capacity(want);
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(capture_ms * 4 + 2000);
        while mono.len() < want {
            let now = std::time::Instant::now();
            if now >= deadline {
                bail!(
                    "loopback capture stalled ({} of {want} samples)",
                    mono.len()
                );
            }
            match rx.recv_timeout(deadline - now) {
                Ok(chunk) => mono.extend(
                    chunk
                        .chunks(channels)
                        .map(|f| f.iter().sum::<f32>() / channels as f32),
                ),
                Err(_) => bail!(
                    "loopback capture stalled ({} of {want} samples)",
                    mono.len()
                ),
            }
        }
        mono.truncate(want);
        Ok((rate, mono))
    }
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

// ── A/B compare analysis (issue #57) ─────────────────────────────────────────
//
// Two full-waveform captures of the *same* deterministic broadband stimulus, one
// per DSP configuration. Because the input is identical, every difference is what
// the config change did. We report it three ways:
//   • per-band magnitude delta   → did the TONE change? (phase-invariant)
//   • best-aligned null depth     → after removing delay+gain, what residual is
//                                   left? A pure phase/timing difference cannot be
//                                   nulled, so a shallow null with matched bands is
//                                   the "sounds different but measures identical"
//                                   signature (e.g. minimum vs linear phase).
//   • verdict                     → TONAL / PHASE-ONLY / identical.

/// One octave band's magnitude change of `b` relative to `a`.
#[derive(Clone, Copy)]
struct BandDelta {
    center_hz: f64,
    delta_db: f64,
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Deep null and matched bands: the two captures are the same signal.
    Identical,
    /// Bands matched but the residual won't null — a pure phase/timing change,
    /// tonally identical yet audible to sensitive listeners.
    PhaseOnly,
    /// The magnitude spectrum itself moved — an audible tonal change.
    Tonal,
}

/// Circular cross-correlation `c[L] = Σ a[i]·b[i+L]` via FFT. Length is the next
/// power of two ≥ 2·max(len); `c[0]` is lag 0, `c[m-1]` is lag −1 (wrapped).
fn xcorr_fft(a: &[f64], b: &[f64]) -> Vec<f64> {
    use rustfft::{FftPlanner, num_complex::Complex};
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
fn best_integer_lag(a: &[f64], b: &[f64], max_lag: usize) -> isize {
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

/// Shift `x` by a fractional number of samples via a frequency-domain phase ramp
/// (`(Dₐ x)[i] = x[i−d]`), so sub-sample misalignment can be removed.
fn apply_fractional_delay(x: &[f64], d: f64) -> Vec<f64> {
    use rustfft::{FftPlanner, num_complex::Complex};
    let n = x.len();
    if n == 0 {
        return Vec::new();
    }
    let mut planner = FftPlanner::new();
    let fwd = planner.plan_fft_forward(n);
    let inv = planner.plan_fft_inverse(n);
    let mut buf: Vec<Complex<f64>> = x.iter().map(|&v| Complex::new(v, 0.0)).collect();
    fwd.process(&mut buf);
    let two_pi = 2.0 * std::f64::consts::PI;
    for (k, z) in buf.iter_mut().enumerate() {
        // Signed frequency bin keeps the spectrum conjugate-symmetric → real out.
        let kk = if k <= n / 2 {
            k as f64
        } else {
            k as f64 - n as f64
        };
        let phase = -two_pi * kk * d / n as f64;
        *z *= Complex::new(phase.cos(), phase.sin());
    }
    inv.process(&mut buf);
    buf.iter().map(|z| z.re / n as f64).collect()
}

/// Best-aligned null depth in dB (more negative = closer match): align `b` to `a`
/// by integer + fractional delay, remove the optimal broadband gain, and compare
/// the residual RMS to the signal RMS. A frequency-dependent phase difference
/// cannot be aligned away, so it leaves a shallow (high) null.
fn null_depth_db(a: &[f64], b: &[f64], rate: f64) -> f64 {
    let n = a.len().min(b.len());
    if n < 64 {
        return 0.0;
    }
    let (a, b) = (&a[..n], &b[..n]);
    // Cover realistic processing latencies (linear-phase EQ ≈ 171 ms @48 k).
    let max_lag = ((rate * 0.4) as usize).clamp(1, n / 2 - 2);
    let lag = best_integer_lag(a, b, max_lag);
    // Correlation at a lag over the overlapping region only (Σ a[i]·b[i+l]).
    let corr_at = |l: isize| -> f64 {
        if l >= 0 {
            let l = l as usize;
            a[..n - l].iter().zip(&b[l..]).map(|(x, y)| x * y).sum()
        } else {
            let l = (-l) as usize;
            a[l..].iter().zip(&b[..n - l]).map(|(x, y)| x * y).sum()
        }
    };
    // Parabolic vertex of the three correlation points → sub-sample offset.
    let (ym, y0, yp) = (corr_at(lag - 1), corr_at(lag), corr_at(lag + 1));
    let denom = ym - 2.0 * y0 + yp;
    let frac = if denom.abs() > 1e-18 {
        (0.5 * (ym - yp) / denom).clamp(-0.5, 0.5)
    } else {
        0.0
    };

    // Overlapping regions after integer alignment (b advanced by `lag`).
    let (a_seg, b_seg): (&[f64], Vec<f64>) = if lag >= 0 {
        let l = lag as usize;
        (&a[..n - l], b[l..].to_vec())
    } else {
        let l = (-lag) as usize;
        (&a[l..], b[..n - l].to_vec())
    };
    // Advance b by the fractional remainder to line the peak up exactly.
    let b_frac = apply_fractional_delay(&b_seg, -frac);

    // Drop the ends: fractional delay wraps circularly and the integer-shift
    // boundary is undefined there.
    let edge = (b_frac.len() / 16).clamp(1, b_frac.len() / 2);
    let a_t = &a_seg[edge..a_seg.len() - edge];
    let b_t = &b_frac[edge..b_frac.len() - edge];

    // Remove the best broadband gain before nulling.
    let cross: f64 = a_t.iter().zip(b_t).map(|(x, y)| x * y).sum();
    let energy: f64 = b_t.iter().map(|y| y * y).sum();
    let g = if energy > 1e-30 { cross / energy } else { 0.0 };

    let sig: f64 = a_t.iter().map(|x| x * x).sum::<f64>() / a_t.len() as f64;
    let res: f64 = a_t
        .iter()
        .zip(b_t)
        .map(|(x, y)| {
            let e = x - g * y;
            e * e
        })
        .sum::<f64>()
        / a_t.len() as f64;
    (10.0 * (res / sig.max(1e-30)).max(1e-12).log10()).max(-120.0)
}

/// Per-octave-band magnitude delta of `b` relative to `a`, in dB. Magnitude only
/// (phase-invariant), so it isolates *tonal* change from timing. Bands with no
/// meaningful energy in the reference are dropped.
fn band_deltas(a: &[f64], b: &[f64], rate: f64) -> Vec<BandDelta> {
    let n = a.len().min(b.len());
    if n < 64 {
        return Vec::new();
    }
    let pa = power_spectrum(&a[..n]);
    let pb = power_spectrum(&b[..n]);
    let total_a: f64 = pa.iter().sum();
    let bin_hz = rate / n as f64;
    let half = pa.len();
    let band_power = |p: &[f64], lo: f64, hi: f64| -> f64 {
        let lo_k = (lo / bin_hz).ceil() as usize;
        let hi_k = ((hi / bin_hz).floor() as usize).min(half - 1);
        (lo_k..=hi_k).filter(|&k| k < half).map(|k| p[k]).sum()
    };
    let root2 = std::f64::consts::SQRT_2;
    let mut out = Vec::new();
    let mut center = 31.25;
    while center < rate / 2.0 {
        let (lo, hi) = (center / root2, center * root2);
        let sa = band_power(&pa, lo, hi);
        if sa > total_a * 1e-6 {
            let sb = band_power(&pb, lo, hi);
            let delta = 10.0 * (sb / sa).max(1e-12).log10();
            out.push(BandDelta {
                center_hz: center,
                delta_db: delta,
            });
        }
        center *= 2.0;
    }
    out
}

/// Hann-windowed power spectrum (`|X[k]|²`), bins `0..n/2`.
fn power_spectrum(x: &[f64]) -> Vec<f64> {
    use rustfft::{FftPlanner, num_complex::Complex};
    let n = x.len();
    let mut buf: Vec<Complex<f64>> = x
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let w = 0.5 * (1.0 - (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos());
            Complex::new(s * w, 0.0)
        })
        .collect();
    FftPlanner::new().plan_fft_forward(n).process(&mut buf);
    buf[..n / 2].iter().map(Complex::norm_sqr).collect()
}

/// Classify the difference: a moved band means a TONAL change; otherwise a
/// shallow null (won't null despite matched bands) means a PHASE-ONLY change;
/// a deep null means the captures are identical.
fn verdict(bands: &[BandDelta], null_db: f64, tonal_thresh_db: f64, null_floor_db: f64) -> Verdict {
    if bands.iter().any(|d| d.delta_db.abs() > tonal_thresh_db) {
        Verdict::Tonal
    } else if null_db > null_floor_db {
        Verdict::PhaseOnly
    } else {
        Verdict::Identical
    }
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

    // ── A/B compare (issue #57) ──────────────────────────────────────────────

    /// Deterministic broadband noise (fixed-seed LCG), the stimulus stand-in.
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
    fn best_integer_lag_recovers_a_known_shift() {
        // b is a delayed by 137 samples; the aligner must report +137.
        let a = noise(8192, 7);
        let shift = 137usize;
        let mut b = vec![0.0; a.len()];
        b[shift..].copy_from_slice(&a[..a.len() - shift]);
        assert_eq!(best_integer_lag(&a, &b, 1024), shift as isize);
    }

    #[test]
    fn null_depth_is_deep_for_identical_signals() {
        let a = noise(16384, 3);
        // Same signal, scaled — gain compensation should still null it deeply.
        let b: Vec<f64> = a.iter().map(|x| x * 1.7).collect();
        assert!(
            null_depth_db(&a, &b, 48_000.0) < -60.0,
            "identical-up-to-gain must null deeply"
        );
    }

    #[test]
    fn null_depth_is_deep_for_a_delayed_copy() {
        // A pure delay (integer + fractional) must be removed by alignment.
        let n = 16384usize;
        let w = 2.0 * std::f64::consts::PI * 997.0 / 48_000.0;
        let a: Vec<f64> = (0..n).map(|i| (w * i as f64).sin()).collect();
        let d = 40.5; // 40 whole + half a sample
        let b: Vec<f64> = (0..n).map(|i| (w * (i as f64 - d)).sin()).collect();
        assert!(
            null_depth_db(&a, &b, 48_000.0) < -40.0,
            "a delayed copy must null after fractional alignment"
        );
    }

    #[test]
    fn null_depth_is_shallow_for_a_pure_phase_difference() {
        // Two tones; in `b` the second is 90° shifted. No single delay+gain can
        // null both at once → this is the PHASE-ONLY signature.
        let n = 16384usize;
        let (w1, w2) = (
            2.0 * std::f64::consts::PI * 500.0 / 48_000.0,
            2.0 * std::f64::consts::PI * 5000.0 / 48_000.0,
        );
        let a: Vec<f64> = (0..n)
            .map(|i| (w1 * i as f64).sin() + (w2 * i as f64).sin())
            .collect();
        let b: Vec<f64> = (0..n)
            .map(|i| (w1 * i as f64).sin() + (w2 * i as f64).cos())
            .collect();
        assert!(
            null_depth_db(&a, &b, 48_000.0) > -20.0,
            "a frequency-dependent phase shift must not null"
        );
    }

    #[test]
    fn band_deltas_flag_a_broadband_gain() {
        let a = noise(16384, 11);
        let b: Vec<f64> = a.iter().map(|x| x * 2.0).collect(); // +6 dB everywhere
        let bands = band_deltas(&a, &b, 48_000.0);
        assert!(!bands.is_empty());
        for band in &bands {
            assert!(
                (band.delta_db - 6.0).abs() < 1.0,
                "band {} Hz: {} dB, expected ~+6",
                band.center_hz,
                band.delta_db
            );
        }
    }

    #[test]
    fn band_deltas_localise_a_high_shelf_cut() {
        // Remove the top: bass bands unchanged, treble bands drop hard.
        let n = 16384usize;
        let (w_lo, w_hi) = (
            2.0 * std::f64::consts::PI * 200.0 / 48_000.0,
            2.0 * std::f64::consts::PI * 9000.0 / 48_000.0,
        );
        let a: Vec<f64> = (0..n)
            .map(|i| (w_lo * i as f64).sin() + (w_hi * i as f64).sin())
            .collect();
        let b: Vec<f64> = (0..n).map(|i| (w_lo * i as f64).sin()).collect(); // hi removed
        let bands = band_deltas(&a, &b, 48_000.0);
        let lo = bands.iter().find(|x| x.center_hz < 400.0).unwrap();
        let hi = bands.iter().find(|x| x.center_hz > 6000.0).unwrap();
        assert!(lo.delta_db.abs() < 1.0, "bass band should be untouched");
        assert!(hi.delta_db < -20.0, "treble band should collapse");
    }

    #[test]
    fn verdict_identical_when_matched_and_nulls() {
        let bands = vec![BandDelta {
            center_hz: 1000.0,
            delta_db: 0.05,
        }];
        assert_eq!(verdict(&bands, -65.0, 0.5, -40.0), Verdict::Identical);
    }

    #[test]
    fn verdict_phase_only_when_matched_but_wont_null() {
        let bands = vec![BandDelta {
            center_hz: 1000.0,
            delta_db: 0.05,
        }];
        assert_eq!(verdict(&bands, -6.0, 0.5, -40.0), Verdict::PhaseOnly);
    }

    #[test]
    fn allpass_on_noise_reads_as_phase_only() {
        // The real #57 case: an all-pass (flat magnitude, frequency-dependent
        // phase) is exactly what min- vs linear-phase EQ looks like. Bands must
        // stay flat, the null must stay shallow, and the verdict PHASE-ONLY.
        let a = noise(32768, 21);
        let c = 0.7; // first-order all-pass: y[n] = c·x[n] + x[n-1] − c·y[n-1]
        let mut b = vec![0.0; a.len()];
        let (mut x1, mut y1) = (0.0, 0.0);
        for (i, &x) in a.iter().enumerate() {
            let y = c * x + x1 - c * y1;
            b[i] = y;
            x1 = x;
            y1 = y;
        }
        let bands = band_deltas(&a, &b, 48_000.0);
        assert!(!bands.is_empty());
        for band in &bands {
            assert!(
                band.delta_db.abs() < 0.5,
                "all-pass must not move band {} Hz ({} dB)",
                band.center_hz,
                band.delta_db
            );
        }
        let null = null_depth_db(&a, &b, 48_000.0);
        assert!(null > -25.0, "all-pass must not null (got {null} dB)");
        assert_eq!(verdict(&bands, null, 0.5, -40.0), Verdict::PhaseOnly);
    }

    #[test]
    fn verdict_tonal_when_a_band_moves() {
        let bands = vec![
            BandDelta {
                center_hz: 1000.0,
                delta_db: 0.05,
            },
            BandDelta {
                center_hz: 8000.0,
                delta_db: 3.2,
            },
        ];
        // Even a deep null is TONAL if the magnitude spectrum changed.
        assert_eq!(verdict(&bands, -65.0, 0.5, -40.0), Verdict::Tonal);
    }
}
