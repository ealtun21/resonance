//! Execute scenarios on the Windows endpoint through the APO in audiodg.

use super::env::{self, Daemon};
use crate::common::{
    MAX_LAG_SECS, MIXER_HEADROOM_PEAK, RunOpts, apply_profile, exact_checks, headroom_shift, ipc,
    peak_abs, scale_pow2, wait_for,
};
use crate::native::play_and_record;
use crate::render::{BLOCK_FRAMES, load_exported_chain, render};
use crate::report::{Report, ScenarioResult, Status, settle};
use crate::scenario::Scenario;
use crate::stimulus::{Stimulus, generate};
use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait};
use resonance_ipc::Command;
use std::path::Path;
use std::time::Duration;

const OS: &str = "windows";

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

fn measure(
    r: &mut ScenarioResult,
    s: &Scenario,
    opts: &RunOpts,
    scripts: &Path,
    dir: &Path,
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
        Ok(())
    })();
    daemon.stop();
    res
}

fn run_once(s: &Scenario, opts: &RunOpts, scripts: &Path) -> Result<ScenarioResult> {
    let dir = opts.out_dir.join(&s.id);
    std::fs::create_dir_all(&dir)?;
    let mut r = ScenarioResult::new(&s.id, OS);
    r.expected_fail.clone_from(&s.expected_fail);
    if let Err(e) = measure(&mut r, s, opts, scripts, &dir) {
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
    let mut results = Vec::new();
    for s in todo {
        eprintln!("e2e: {}", s.id);
        let mut r = run_once(s, opts, scripts)?;
        if r.status == Status::Fail && r.discontinuities > 0 {
            let first = r.failures.join("; ");
            let rerun = run_once(s, opts, scripts)?;
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
        results.push(r);
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
