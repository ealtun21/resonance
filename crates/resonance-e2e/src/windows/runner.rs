//! Execute scenarios on the Windows endpoint through the APO in audiodg.

use super::env::{self, Daemon};
use crate::common::{
    MAX_LAG_SECS, MIXER_HEADROOM_PEAK, RunOpts, apply_profile, exact_checks, headroom_shift, ipc,
    peak_abs, scale_pow2, wait_for,
};
use crate::compare::compare;
use crate::latency::{failure, judge, load_baselines, save_baselines};
use crate::native::play_and_record;
use crate::render::{BLOCK_FRAMES, load_exported_chain, render};
use crate::report::{Report, ScenarioResult, Status, settle};
use crate::scenario::Scenario;
use crate::stimulus::{Stimulus, generate};
use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait};
use resonance_dsp::analysis::best_integer_lag;
use resonance_ipc::Command;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

const OS: &str = "windows";

/// Resonance-off lags per device format, and the committed baselines.
struct LatencyState {
    off: BTreeMap<(u32, usize), isize>,
    baselines: BTreeMap<String, f64>,
}

/// The audio services come back asynchronously after an endpoint change:
/// wait until the default output reports the scenario's format.
fn wait_for_default_format(channels: usize, rate: u32) -> Result<()> {
    let mut last = String::from("no default output device");
    wait_for(
        "default output at the scenario format",
        Duration::from_secs(40),
        || {
            let cfg = cpal::default_host()
                .default_output_device()
                .and_then(|d| d.default_output_config().ok());
            last = cfg.as_ref().map_or_else(
                || "no default output device".into(),
                |c| format!("{} ch @ {} Hz", c.channels(), c.sample_rate()),
            );
            Ok(cfg
                .is_some_and(|c| usize::from(c.channels()) == channels && c.sample_rate() == rate))
        },
    )
    .with_context(|| format!("default output is {last}, want {channels} ch @ {rate} Hz"))?;
    // Settled: audiodg rebuilds its graph a moment after the format changes.
    std::thread::sleep(Duration::from_secs(2));
    Ok(())
}

/// Frames the chain itself delays `stim` (render vs input, channel 0): the part of the
/// live lag that is Resonance's, since the recording is aligned to the render.
fn chain_delay_frames(stim: &[f32], expected: &[f32], channels: usize, rate: u32) -> isize {
    let ch0 =
        |x: &[f32]| -> Vec<f64> { x.iter().step_by(channels).map(|&v| f64::from(v)).collect() };
    best_integer_lag(&ch0(stim), &ch0(expected), (0.5 * f64::from(rate)) as usize)
}

/// Total lag (frames) of a Resonance-absent loopback of `s`'s format, with the APO verifiably
/// detached: the device + engine path alone.
fn measure_off(s: &Scenario, scripts: &Path, name: &str, slot: u8) -> Result<isize> {
    env::set_apo_slot(scripts, name, None)?;
    wait_for_default_format(s.channels, s.rate)?;
    let stim = generate(s.rate, s.channels, 1.0);
    let dev = cpal::default_host()
        .default_output_device()
        .context("no default output device")?;
    let mark = env::apo_log_mark();
    let rec = play_and_record(
        &dev,
        &dev,
        false,
        &stim.samples,
        s.channels,
        s.rate,
        (MAX_LAG_SECS * f64::from(s.rate)) as usize,
    );
    // Always put the APO back, whatever happened.
    let log = env::apo_log_since(mark);
    env::set_apo_slot(scripts, name, Some(slot))?;
    let rec = rec?;
    env::verify_apo_absent(&log)?;
    let o = compare(
        &stim.samples,
        &rec.samples,
        s.channels,
        stim.body.clone(),
        (MAX_LAG_SECS * f64::from(s.rate)) as usize,
    );
    anyhow::ensure!(
        o.passes(crate::compare::CompareMode::Exact),
        "Resonance-off loopback is not bit-exact: {}",
        o.describe()
    );
    Ok(o.lag)
}

/// Device name fragment and the APO slot each endpoint uses.
fn endpoint_attach(which: super::Endpoint) -> (&'static str, u8) {
    match which {
        super::Endpoint::Hda => ("High", 7),
        super::Endpoint::Scream => ("Scream", 2),
    }
}

fn measure(
    r: &mut ScenarioResult,
    s: &Scenario,
    opts: &RunOpts,
    scripts: &Path,
    dir: &Path,
    lat: &mut LatencyState,
) -> Result<()> {
    let which = super::endpoint_for(s.channels, s.rate).context("no endpoint for this format")?;
    env::select_endpoint(scripts, which, false)?;
    env::set_endpoint_format(scripts, s.channels, s.rate)?;
    if let Err(first) = wait_for_default_format(s.channels, s.rate) {
        // A rejected format leaves the endpoint unavailable until it is re-enabled:
        // recover so the next scenario starts clean, but still report this one.
        if which == super::Endpoint::Scream {
            let _ = env::set_endpoint_format(scripts, 8, 48_000);
        }
        let _ = env::select_endpoint(scripts, which, true);
        return Err(first);
    }
    let daemon = Daemon::start(&opts.daemon_bin, &dir.join("daemon.log"))?;
    let res = (|| -> Result<()> {
        apply_profile(s, dir)?;
        let export = dir.join("chain.bin");
        ipc(Command::ResetAndExportChain {
            path: export.to_string_lossy().into_owned(),
        })?;
        // The APO polls the shared state every 30 ms; give it a few polls.
        std::thread::sleep(Duration::from_millis(300));
        let mut stim = generate(s.rate, s.channels, s.body_secs);
        let render_stim = |stim: &Stimulus| -> Result<(Vec<f32>, Vec<String>)> {
            let (mut chain, notes) = load_exported_chain(&export, s.channels, f64::from(s.rate))?;
            Ok((render(&mut chain, &stim.samples, BLOCK_FRAMES), notes))
        };
        let (mut expected, notes) = render_stim(&stim)?;
        r.notes.extend(notes);
        // The engine limits mixes near full scale: keep the render under it.
        let shift = headroom_shift(peak_abs(&expected), MIXER_HEADROOM_PEAK);
        if shift > 0 {
            scale_pow2(&mut stim.samples, shift);
            expected = render_stim(&stim)?.0;
            r.notes.push(format!(
                "stimulus attenuated by {} dB: the render peaks above the mixer's limiter threshold",
                6 * shift
            ));
        }

        let dev = cpal::default_host()
            .default_output_device()
            .context("no default output device")?;
        let mark = env::apo_log_mark();
        let rec = play_and_record(
            &dev,
            &dev,
            false,
            &stim.samples,
            s.channels,
            s.rate,
            (MAX_LAG_SECS * f64::from(s.rate)) as usize,
        )?;
        r.discontinuities = rec.discontinuities;
        // Let audiodg flush its log line for the last buffers.
        std::thread::sleep(Duration::from_millis(500));
        if let Err(e) = env::verify_apo_on(&env::apo_log_since(mark), s.channels, s.rate) {
            r.failures.push(format!("{e:#}"));
        }
        exact_checks(r, s, &stim, &rec, &expected, dir)?;
        if s.measures_latency() {
            let (name, slot) = endpoint_attach(which);
            let off = match lat.off.get(&(s.rate, s.channels)) {
                Some(&v) => v,
                None => {
                    let v = measure_off(s, scripts, name, slot)?;
                    lat.off.insert((s.rate, s.channels), v);
                    v
                }
            };
            let on_lag = r
                .compare
                .as_ref()
                .context("no comparison to take the lag from")?
                .lag;
            let total_on =
                on_lag + chain_delay_frames(&stim.samples, &expected, s.channels, s.rate);
            let ms = |frames: isize| frames as f64 * 1000.0 / f64::from(s.rate);
            let added = ms(total_on - off);
            let v = judge(added, lat.baselines.get(&s.id).copied());
            (
                r.latency_on_ms,
                r.latency_off_ms,
                r.added_latency_ms,
                r.latency_verdict,
            ) = (Some(ms(total_on)), Some(ms(off)), Some(added), Some(v));
            r.failures.extend(failure(v, added, opts.update_baseline));
        }
        Ok(())
    })();
    daemon.stop();
    res
}

fn run_once(
    s: &Scenario,
    opts: &RunOpts,
    scripts: &Path,
    lat: &mut LatencyState,
) -> Result<ScenarioResult> {
    let dir = opts.out_dir.join(&s.id);
    std::fs::create_dir_all(&dir)?;
    let mut r = ScenarioResult::new(&s.id, OS);
    r.expected_fail.clone_from(&s.expected_fail);
    if let Err(e) = measure(&mut r, s, opts, scripts, &dir, lat) {
        r.failures.push(format!("{e:#}"));
    }
    if !r.failures.is_empty() {
        r.artifacts = Some(dir.to_string_lossy().into_owned());
    }
    r.status = settle(r.failures.is_empty(), s.expected_fail.is_some());
    Ok(r)
}

pub fn run(opts: &RunOpts, scripts: &Path) -> Result<Report> {
    let (todo, skipped): (Vec<_>, Vec<_>) = opts
        .scenarios
        .iter()
        .partition(|s| super::ineligible(s).is_none());
    // Fewest endpoint reconfigurations: group by format.
    let mut todo = todo;
    todo.sort_by_key(|s| (s.channels, s.rate)); // stereo (HDA) first, then 8 ch by rate
    let mut lat = LatencyState {
        off: BTreeMap::new(),
        baselines: load_baselines(&opts.baselines_path)?,
    };
    let mut results = Vec::new();
    for s in todo {
        eprintln!("e2e: {}", s.id);
        let mut r = run_once(s, opts, scripts, &mut lat)?;
        if r.status == Status::Fail && r.discontinuities > 0 {
            let first = r.failures.join("; ");
            let rerun = run_once(s, opts, scripts, &mut lat)?;
            r = if rerun.status == Status::Pass {
                ScenarioResult {
                    status: Status::Flake,
                    notes: vec![format!("first attempt: {first}")],
                    ..rerun
                }
            } else {
                rerun
            };
        }
        if opts.update_baseline {
            if let Some(a) = r.added_latency_ms {
                lat.baselines
                    .insert(r.id.clone(), (a * 100.0).round() / 100.0);
            }
        }
        results.push(r);
    }
    if opts.update_baseline {
        save_baselines(&opts.baselines_path, &lat.baselines)?;
    }
    Ok(Report {
        os: OS.into(),
        tier: format!("{:?}", opts.tier).to_lowercase(),
        results,
        skipped: skipped
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    super::ineligible(s).unwrap_or_default().into(),
                )
            })
            .collect(),
    })
}
