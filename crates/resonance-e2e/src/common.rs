//! OS-independent scenario helpers shared by every backend runner.

use crate::checks::{
    PITCH_TOLERANCE, THDN_FRAMES, block_rms_db, coherent_hz, first_signal_frame, gain_db,
    octave_edges, phat_lag, pitch_error, pitch_error_at, segment_lags, tone_quality,
    transfer_bands, zero_fill_runs,
};
use crate::compare::compare;
use crate::render::{BLOCK_FRAMES, load_exported_chain, render};
use crate::report::ScenarioResult;
use crate::scenario::{Kind, Scenario, Tier};
use crate::stimulus::{PILOT_HZ, Stimulus, generate, tone};
use anyhow::{Context, Result, bail};
use resonance_ipc::transport::SyncClient;
use resonance_ipc::{Command, DaemonState, Response};
use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct Recording {
    pub node: String,
    pub channels: usize,
    pub rate: u32,
    pub first_tick: Option<u64>,
    pub samples: Vec<f32>,
    pub discontinuities: u32,
}

pub struct RunOpts {
    pub scenarios: Vec<Scenario>,
    pub tier: Tier,
    pub out_dir: PathBuf,
    pub daemon_bin: PathBuf,
    pub baselines_path: PathBuf,
    pub update_baseline: bool,
}

/// Poll `f` every 100 ms until it returns true; fail with `what` after `timeout`.
pub fn wait_for(what: &str, timeout: Duration, mut f: impl FnMut() -> Result<bool>) -> Result<()> {
    let end = Instant::now() + timeout;
    loop {
        if f()? {
            return Ok(());
        }
        if Instant::now() > end {
            bail!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Latency search range; also how long recording continues after the stimulus.
pub const MAX_LAG_SECS: f64 = 1.0;

#[allow(clippy::needless_pass_by_value)] // call sites build the command inline
pub fn ipc(cmd: Command) -> Result<()> {
    match SyncClient::connect()?.send_recv(cmd.clone())? {
        Response::Ok => Ok(()),
        Response::Error(e) => bail!("{cmd:?}: {e}"),
        other => bail!("{cmd:?}: unexpected {other:?}"),
    }
}

pub fn get_state() -> Result<DaemonState> {
    Ok(SyncClient::connect()?.get_state()?)
}

/// Deterministic stereo IR (exponentially decaying noise, 4096 taps) for
/// `ir = "synthetic:room"`, written as float WAV at `rate`.
pub fn synthetic_ir(path: &Path, rate: u32) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    let mut s = 0x0123_4567_89AB_CDEFu64;
    for i in 0..4096 {
        for _ in 0..2 {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let n = (s >> 40) as f32 / (1u32 << 24) as f32 - 0.5;
            w.write_sample(n * (-(i as f32) / 600.0).exp())?;
        }
    }
    w.finalize()?;
    Ok(())
}

pub fn ch0(x: &[f32], channels: usize) -> Vec<f64> {
    chan(x, channels, 0)
}

/// Channel `c` of an interleaved buffer.
pub fn chan(x: &[f32], channels: usize, c: usize) -> Vec<f64> {
    x.iter()
        .skip(c)
        .step_by(channels)
        .map(|&v| f64::from(v))
        .collect()
}

pub fn write_wav(path: &Path, x: &[f32], channels: usize, rate: u32) -> Result<()> {
    let spec = hound::WavSpec {
        channels: u16::try_from(channels)?,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &v in x {
        w.write_sample(v)?;
    }
    w.finalize()?;
    Ok(())
}

pub fn apply_profile(s: &Scenario, dir: &Path) -> Result<()> {
    let p = &s.profile;
    match &p.preset {
        Some(preset) => ipc(Command::LoadPreset {
            path: preset.to_string_lossy().into_owned(),
        })?,
        None => ipc(Command::ApplyState {
            preamp_db: p.preamp_db,
            enabled: true,
            bands: p.bands.clone(),
            effects: p.effects.clone(),
        })?,
    }
    ipc(Command::SetDither {
        bits: p.dither_bits,
    })?;
    ipc(Command::SetPhaseMode {
        linear: p.linear_phase,
    })?;
    if let Some(ir) = &p.ir {
        let path = if ir.starts_with("synthetic:") {
            let path = dir.join("synthetic-ir.wav");
            synthetic_ir(&path, s.rate)?;
            path.to_string_lossy().into_owned()
        } else {
            ir.clone()
        };
        ipc(Command::SetConvolutionIr { path })?;
    }
    Ok(())
}

/// Steady matched-rate path: every sample must equal the offline render.
pub fn exact_checks(
    r: &mut ScenarioResult,
    s: &Scenario,
    stim: &Stimulus,
    rec: &Recording,
    expected: &[f32],
    dir: &Path,
) -> Result<()> {
    // Body plus up to 0.25 s of processed tail (reverb, FIR, IR).
    let window = stim.body.start..stim.body.end + stim.body.len().min(s.rate as usize / 4);
    let o = compare(
        expected,
        &rec.samples,
        s.channels,
        window,
        (MAX_LAG_SECS * f64::from(s.rate)) as usize,
    );
    if !o.passes(s.expect.compare_for(std::env::consts::OS)) {
        r.failures.push(o.describe());
        write_wav(&dir.join("recorded.wav"), &rec.samples, s.channels, s.rate)?;
        write_wav(&dir.join("expected.wav"), expected, s.channels, s.rate)?;
        let shift = usize::try_from(o.lag).unwrap_or(0) * s.channels;
        let diff: Vec<f32> = rec
            .samples
            .iter()
            .skip(shift)
            .zip(expected)
            .map(|(a, b)| a - b)
            .collect();
        write_wav(&dir.join("diff.wav"), &diff, s.channels, s.rate)?;
    }
    r.compare = Some(o);
    Ok(())
}

/// In-band signal-to-error ratio (dB) implied by a magnitude-squared coherence.
#[must_use]
pub fn coherence_snr_db(coherence: f64) -> f64 {
    10.0 * (coherence / (1.0 - coherence).max(1e-12)).log10()
}

/// The path is not bit-transparent by design (macOS: the Process Tap resamples, see spec
/// section 14.4), so judge the recording against the offline render as a system, on every
/// channel: per-octave transfer gain must be within `tolerance_db` of 0 dB and the in-band
/// signal-to-error ratio against the render at least `min_snr_db`. The octaves stop well below
/// the tap's roll-off (about 0.85 of Nyquist), where its response is flat to a few millidB.
pub fn transfer_checks(
    r: &mut ScenarioResult,
    s: &Scenario,
    stim: &Stimulus,
    expected: &[f32],
    rec: &Recording,
    (default_tolerance_db, default_min_snr_db): (f64, f64),
    max_dropout_fraction: f64,
) -> Result<isize> {
    let tolerance_db = s
        .expect
        .transfer_tolerance_db
        .unwrap_or(default_tolerance_db);
    let min_snr_db = s.expect.transfer_min_snr_db.unwrap_or(default_min_snr_db);
    let ch = s.channels;
    let rate = f64::from(s.rate);
    let exp0 = ch0(expected, ch);
    let rec0 = ch0(&rec.samples, rec.channels);
    let max_lag = (MAX_LAG_SECS * rate) as usize;
    let (lag, quality) = phat_lag(&exp0, &rec0, max_lag);
    if quality < MIN_ALIGNMENT_QUALITY {
        r.failures.push(format!(
            "the recording does not line up with the render (correlation peak {quality:.1}x the median, need {MIN_ALIGNMENT_QUALITY})"
        ));
    }
    let (lags, slips) = segment_lags(&exp0, &rec0, stim.body.clone(), rate, lag, SLIP_FRAMES);
    let ex = crate::checks::segment_exactness(expected, &rec.samples, rec.channels, &lags);
    r.notes.push(format!(
        "{} of {} segments bit-exact, {:.4} of samples equal, worst difference {:.1} dBFS",
        ex.exact_segments, ex.segments, ex.equal_fraction, ex.max_err_dbfs
    ));
    let edges = octave_edges(0.25 * rate);
    let (mut used, mut skipped) = (0, 0);
    // Worst case over channels per octave: (largest |gain|, lowest SNR).
    let mut worst = vec![(0.0f64, f64::INFINITY); edges.len()];
    let mut failures = Vec::new();
    for c in 0..ch {
        let (bands, u, sk) = transfer_bands(
            &chan(expected, ch, c),
            &chan(&rec.samples, rec.channels, c),
            &lags,
            stim.body.clone(),
            rate,
            &edges,
        );
        if c == 0 {
            (used, skipped) = (u, sk);
        }
        for (b, w) in bands.iter().zip(&mut worst) {
            let snr = coherence_snr_db(b.coherence);
            w.0 = w.0.max(b.gain_db.abs());
            w.1 = w.1.min(snr);
            if b.gain_db.abs() > tolerance_db {
                failures.push(format!(
                    "channel {c}, octave from {:.0} Hz: transfer gain {:+.3} dB vs the render (limit ±{tolerance_db} dB)",
                    b.from_hz, b.gain_db
                ));
            }
            if snr < min_snr_db {
                failures.push(format!(
                    "channel {c}, octave from {:.0} Hz: in-band SNR {snr:.1} dB against the render (limit {min_snr_db} dB)",
                    b.from_hz
                ));
            }
        }
    }
    r.notes.push(format!(
        "worst over channels per octave (from kHz: |gain| dB, SNR dB): {}",
        edges
            .iter()
            .zip(&worst)
            .map(|(f, w)| format!("{:.2}k: {:.3}, {:.0}", f / 1000.0, w.0, w.1))
            .collect::<Vec<_>>()
            .join("; ")
    ));
    let total = used + skipped;
    r.notes.push(format!(
        "aligned at {lag} frames; {skipped} of {total} blocks hold a zero-filled dropout (excluded from the gain estimate); {slips} segments slipped"
    ));
    // A delay jump is a dropout of the same kind as a zero fill (the ring dropped its backlog).
    r.discontinuities += u32::try_from(slips).unwrap_or(u32::MAX);
    if used < 8 || skipped as f64 > max_dropout_fraction * total as f64 {
        r.failures.push(format!(
            "{skipped} of {total} blocks hold a zero-filled dropout (limit {:.0} %)",
            max_dropout_fraction * 100.0
        ));
    }
    let more = failures.len().saturating_sub(8);
    r.failures.extend(failures.into_iter().take(8));
    if more > 0 {
        r.failures.push(format!("... and {more} more"));
    }
    let seg: Vec<f32> = rec0
        .get(stim.body.start.saturating_add_signed(lag)..stim.body.end.saturating_add_signed(lag))
        .unwrap_or_default()
        .iter()
        .map(|&v| v as f32)
        .collect();
    let pe = pitch_error(&seg, rate);
    if pe > PITCH_TOLERANCE {
        r.failures
            .push(format!("pilot pitch off by {:.4} %", pe * 100.0));
    }
    Ok(lag)
}

/// The stimulus a scenario plays: the render stimulus, or a steady tone for the tone kinds.
#[must_use]
pub fn stimulus_for(s: &Scenario) -> Stimulus {
    match s.kind {
        Kind::Render => generate(s.player_rate, s.channels, s.body_secs),
        Kind::Thdn => tone(
            s.player_rate,
            s.channels,
            s.body_secs,
            thdn_hz(s, f64::from(s.rate)),
        ),
        Kind::Soak | Kind::Stress => tone(s.player_rate, s.channels, s.body_secs, PILOT_HZ),
    }
}

/// The bin-centred frequency a THD+N scenario plays and measures at the recording `rate`.
fn thdn_hz(s: &Scenario, rate: f64) -> f64 {
    coherent_hz(rate, s.tone_hz.unwrap_or(PILOT_HZ))
}

/// Soak windows: long enough that the pilot's pitch resolves far below [`PITCH_TOLERANCE`].
pub const SOAK_WINDOW_SECS: f64 = 10.0;
/// The tone's level may not move between windows by more than this (dB). A steady chain holds
/// it to a few thousandths.
pub const LEVEL_TOLERANCE_DB: f64 = 0.05;
/// A 20 ms block whose level differs from the run's median by more than this (dB) is a dropout
/// or a glitch (a 1 ms hole in a 20 ms block already reads -0.4 dB).
pub const BLOCK_DIP_DB: f64 = 0.25;
const BLOCK_SECS: f64 = 0.02;
/// Frames of all-channel digital silence inside a tone that count as a dropout.
const DROPOUT_FRAMES: usize = 16;

/// Judge one channel of a steady-tone recording in [`SOAK_WINDOW_SECS`] windows: pitch drift,
/// level change, short dropouts. `x` is the steady part only.
pub fn soak_channel(r: &mut ScenarioResult, label: &str, x: &[f32], rate: f64) {
    let win = (SOAK_WINDOW_SECS * rate) as usize;
    let windows: Vec<&[f32]> = x.chunks_exact(win).collect();
    if windows.is_empty() {
        r.failures
            .push(format!("{label}: recording shorter than one soak window"));
        return;
    }
    let pitch: Vec<f64> = windows.iter().map(|w| pitch_error(w, rate)).collect();
    let level: Vec<f64> = windows
        .iter()
        .map(|w| block_rms_db(w, w.len())[0])
        .collect();
    let worst_pitch = pitch.iter().copied().fold(0.0, f64::max);
    let span = level.iter().copied().fold(f64::MIN, f64::max)
        - level.iter().copied().fold(f64::MAX, f64::min);
    let mut blocks = block_rms_db(x, (BLOCK_SECS * rate) as usize);
    blocks.sort_by(f64::total_cmp);
    let median = blocks[blocks.len() / 2];
    let dips = blocks
        .iter()
        .filter(|&&b| (b - median).abs() > BLOCK_DIP_DB)
        .count();
    let worst_block = blocks
        .iter()
        .map(|&b| (b - median).abs())
        .fold(0.0, f64::max);
    let holes = zero_fill_runs(x, 1, DROPOUT_FRAMES);
    r.notes.push(format!(
        "soak {label}: {} windows of {SOAK_WINDOW_SECS} s, worst pitch error {:.5} %, level span {span:.4} dB, \
         worst 20 ms block {worst_block:.3} dB from the median, {dips} dipped blocks, {holes} zero-filled gaps",
        windows.len(),
        worst_pitch * 100.0
    ));
    if let Some((i, p)) = pitch
        .iter()
        .enumerate()
        .find(|(_, p)| **p > PITCH_TOLERANCE)
    {
        r.failures.push(format!(
            "{label}: pilot pitch drifted by {:.4} % in window {i} (limit {:.4} %)",
            p * 100.0,
            PITCH_TOLERANCE * 100.0
        ));
    }
    if span > LEVEL_TOLERANCE_DB {
        r.failures.push(format!(
            "{label}: level moved by {span:.3} dB across windows (limit {LEVEL_TOLERANCE_DB} dB)"
        ));
    }
    if dips > 0 || holes > 0 {
        r.failures.push(format!(
            "{label}: {holes} zero-filled gaps and {dips} dipped 20 ms blocks (worst {worst_block:.2} dB): dropouts"
        ));
    }
}

fn channel_of(x: &[f32], channels: usize, c: usize) -> Vec<f32> {
    x.iter().skip(c).step_by(channels).copied().collect()
}

/// Recording frames that hold the steady tone: one second in from each end of the body.
fn steady_part(stim: &Stimulus, rec: &Recording, player_rate: u32) -> (usize, usize) {
    let to_rec =
        |frame: usize| (frame as f64 * f64::from(rec.rate) / f64::from(player_rate)) as usize;
    let margin = rec.rate as usize;
    let a = to_rec(stim.body.start) + margin;
    let b = to_rec(stim.body.end)
        .saturating_sub(margin)
        .min(rec.samples.len() / rec.channels);
    (a, b.max(a))
}

/// Steady-tone judgement for [`Kind::Soak`] and [`Kind::Thdn`], per channel. The measured
/// numbers go into the result's notes either way.
pub fn tone_checks(r: &mut ScenarioResult, s: &Scenario, stim: &Stimulus, rec: &Recording) {
    let (a, b) = steady_part(stim, rec, s.player_rate);
    let rate = f64::from(rec.rate);
    let steady = &rec.samples[a * rec.channels..b * rec.channels];
    for c in 0..rec.channels {
        let label = format!("ch{c}");
        let x = channel_of(steady, rec.channels, c);
        match s.kind {
            Kind::Soak => soak_channel(r, &label, &x, rate),
            Kind::Thdn => thdn_channel(r, s, &label, &x, rate),
            Kind::Render | Kind::Stress => {}
        }
    }
}

fn thdn_channel(r: &mut ScenarioResult, s: &Scenario, label: &str, x: &[f32], rate: f64) {
    if x.len() < THDN_FRAMES {
        r.failures.push(format!(
            "{label}: only {} steady frames, need {THDN_FRAMES}",
            x.len()
        ));
        return;
    }
    let hz = thdn_hz(s, rate);
    let x = &x[..THDN_FRAMES];
    let q = tone_quality(x, rate, hz);
    let pe = pitch_error_at(x, rate, hz);
    r.notes.push(format!(
        "thd+n {label}: {:.1} dB, thd {:.1} dB, snr {:.1} dB at {hz:.3} Hz (pitch error {:.5} %)",
        q.thd_n_db,
        q.thd_db,
        q.snr_db,
        pe * 100.0
    ));
    let (max_thdn, min_snr) = (
        s.expect.max_thdn_db.unwrap_or(f64::INFINITY),
        s.expect.min_snr_db.unwrap_or(f64::NEG_INFINITY),
    );
    if q.thd_n_db > max_thdn {
        r.failures.push(format!(
            "{label}: THD+N {:.1} dB above the {max_thdn} dB limit",
            q.thd_n_db
        ));
    }
    if q.snr_db < min_snr {
        r.failures.push(format!(
            "{label}: SNR {:.1} dB below the {min_snr} dB limit",
            q.snr_db
        ));
    }
    if pe > PITCH_TOLERANCE {
        r.failures
            .push(format!("{label}: tone pitch off by {:.4} %", pe * 100.0));
    }
}

/// A correlation peak this many times the median means the lag is real.
pub const MIN_ALIGNMENT_QUALITY: f64 = 8.0;

/// A segment whose delay differs from the run's by more than this many frames slipped.
pub const SLIP_FRAMES: isize = 16;

/// A rate converter is in the path, so not bit-exact by design: the pilot's
/// pitch and the path's per-octave gain must match the offline render's
/// gain at the DSP rate. Windows are aligned on the first arriving sample.
pub fn resample_checks(
    r: &mut ScenarioResult,
    s: &Scenario,
    stim: &Stimulus,
    rec: &Recording,
    export: &Path,
    state: &DaemonState,
    band_tolerance_db: f64,
) -> Result<()> {
    let ch = s.channels;
    let rec_rate = f64::from(rec.rate);
    let start = first_signal_frame(&rec.samples, rec.channels).context("recording is silent")?;
    let len = (stim.body.len() as f64 * rec_rate / f64::from(s.player_rate)) as usize;
    let end = (start + len).min(rec.samples.len() / rec.channels);
    let rec0 = ch0(
        &rec.samples[start * rec.channels..end * rec.channels],
        rec.channels,
    );
    let rec0_f32: Vec<f32> = rec0.iter().map(|&v| v as f32).collect();
    let pe = pitch_error(&rec0_f32, rec_rate);
    if pe > PITCH_TOLERANCE {
        r.failures
            .push(format!("pilot pitch off by {:.4} %", pe * 100.0));
    }
    let dsp_rate = state.sample_rate;
    let stim_dsp = generate(dsp_rate as u32, ch, s.body_secs);
    let (mut chain, _) = load_exported_chain(export, state.channels, dsp_rate)?;
    let rendered = render(&mut chain, &stim_dsp.samples, BLOCK_FRAMES);
    let body = |x: &[f32], st: &Stimulus| ch0(&x[st.body.start * ch..st.body.end * ch], ch);
    let edges = octave_edges(0.45 * f64::from(s.player_rate.min(s.rate)).min(dsp_rate));
    let measured = gain_db(
        &body(&stim.samples, stim),
        f64::from(s.player_rate),
        &rec0,
        rec_rate,
        &edges,
    );
    let expected = gain_db(
        &body(&stim_dsp.samples, &stim_dsp),
        dsp_rate,
        &body(&rendered, &stim_dsp),
        dsp_rate,
        &edges,
    );
    for ((hz, m), e) in edges.iter().zip(&measured).zip(&expected) {
        if (m - e).abs() > band_tolerance_db {
            r.failures.push(format!(
                "octave from {hz:.0} Hz: path gain {m:+.2} dB, render {e:+.2} dB"
            ));
        }
    }
    Ok(())
}

/// Largest output sample the OS mixers pass untouched. Windows' audio engine
/// limits float mixes that approach full scale, so a scenario whose offline
/// render peaks higher is run with an attenuated stimulus instead.
pub const MIXER_HEADROOM_PEAK: f32 = 0.89;

/// Smallest `n` such that `peak / 2^n <= limit`.
#[must_use]
pub fn headroom_shift(peak: f32, limit: f32) -> u32 {
    let mut n = 0;
    let mut p = peak;
    while p > limit && n < 24 {
        p *= 0.5;
        n += 1;
    }
    n
}

/// Multiply by `2^-shift`: exact in binary floating point, so the live path and
/// the offline render still see the very same input.
pub fn scale_pow2(x: &mut [f32], shift: u32) {
    let k = 0.5f32.powi(i32::try_from(shift).unwrap_or(24));
    for v in x {
        *v *= k;
    }
}

#[must_use]
pub fn peak_abs(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, v| m.max(v.abs()))
}

/// Frames the chain itself delays `stim` (render vs input, channel 0): the part of a live
/// lag that is Resonance's when the recording is aligned to the render.
#[must_use]
pub fn chain_delay_frames(stim: &[f32], expected: &[f32], channels: usize, rate: u32) -> isize {
    // GCC-PHAT: an EQ'd render of a sweep fools plain cross-correlation (-135 ms measured).
    phat_lag(
        &ch0(stim, channels),
        &ch0(expected, channels),
        (0.5 * f64::from(rate)) as usize,
    )
    .0
}

/// Fill in the latency fields of `r` and judge them against the baseline. `on_lag` is the
/// recording's lag behind the render, `chain_delay` the render's own delay, `off_lag` the
/// Resonance-absent path's lag (all frames at `s.rate`).
#[allow(clippy::too_many_arguments)] // flat numeric inputs read best at the two call sites
pub fn record_latency(
    r: &mut ScenarioResult,
    s: &Scenario,
    on_lag: isize,
    chain_delay: isize,
    off_lag: isize,
    baseline_ms: Option<f64>,
    update_baseline: bool,
    margin: (f64, f64),
) {
    let ms = |frames: isize| frames as f64 * 1000.0 / f64::from(s.rate);
    let total_on = on_lag + chain_delay;
    let added = ms(total_on - off_lag);
    let v = crate::latency::judge_with_margin(added, baseline_ms, margin.0, margin.1);
    (
        r.latency_on_ms,
        r.latency_off_ms,
        r.added_latency_ms,
        r.latency_verdict,
    ) = (Some(ms(total_on)), Some(ms(off_lag)), Some(added), Some(v));
    r.failures
        .extend(crate::latency::failure(v, added, update_baseline));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headroom_shift_halves_until_under_the_limit() {
        assert_eq!(headroom_shift(0.5, 0.89), 0);
        assert_eq!(headroom_shift(1.0, 0.89), 1);
        assert_eq!(headroom_shift(2.14, 0.89), 2);
        assert_eq!(headroom_shift(0.0, 0.89), 0);
    }

    #[test]
    fn power_of_two_scaling_is_exact_and_reversible() {
        let orig = [0.3f32, -0.7, 1e-7, 0.123_456_79];
        let mut x = orig;
        scale_pow2(&mut x, 3);
        assert_eq!(x[0], orig[0] / 8.0);
        scale_pow2(&mut x, 0);
        let back: Vec<f32> = x.iter().map(|v| v * 8.0).collect();
        assert_eq!(back, orig);
    }

    fn pilot(rate: u32, secs: f64, hz: f64) -> Vec<f32> {
        tone(rate, 1, secs, hz).samples[(0.5 * f64::from(rate)) as usize..]
            [..(secs * f64::from(rate)) as usize]
            .to_vec()
    }

    #[test]
    fn a_clean_tone_passes_the_soak_and_a_hole_fails_it() {
        let mut r = ScenarioResult::new("t", "linux");
        let mut x = pilot(48_000, 30.0, PILOT_HZ);
        soak_channel(&mut r, "ch0", &x, 48_000.0);
        assert!(r.failures.is_empty(), "{:?}", r.failures);
        for v in &mut x[700_000..700_100] {
            *v = 0.0;
        }
        soak_channel(&mut r, "ch0", &x, 48_000.0);
        assert!(
            r.failures.iter().any(|f| f.contains("dropouts")),
            "{:?}",
            r.failures
        );
    }

    #[test]
    fn a_slow_clock_fails_the_soak_on_pitch() {
        let mut r = ScenarioResult::new("t", "linux");
        // 0.02 % slow: twice the tolerance.
        let x = pilot(48_000, 20.0, PILOT_HZ * 0.9998);
        soak_channel(&mut r, "ch0", &x, 48_000.0);
        assert!(
            r.failures.iter().any(|f| f.contains("pitch")),
            "{:?}",
            r.failures
        );
    }
}
