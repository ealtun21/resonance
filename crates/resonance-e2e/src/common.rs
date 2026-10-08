//! OS-independent scenario helpers shared by every backend runner.

use crate::checks::{
    BAND_TOLERANCE_DB, PITCH_TOLERANCE, first_signal_frame, gain_db, octave_edges, pitch_error,
};
use crate::compare::compare;
use crate::render::{BLOCK_FRAMES, load_exported_chain, render};
use crate::report::ScenarioResult;
use crate::scenario::{Scenario, Tier};
use crate::stimulus::{Stimulus, generate};
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
    x.iter().step_by(channels).map(|&v| f64::from(v)).collect()
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
        if (m - e).abs() > BAND_TOLERANCE_DB {
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
}
