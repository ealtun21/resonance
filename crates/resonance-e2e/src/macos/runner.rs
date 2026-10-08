//! Execute scenarios through the daemon's Process Tap on macOS.

use super::{OUT_DEVICE, env, in_device, ineligible};
use crate::common::{MAX_LAG_SECS, RunOpts, apply_profile, exact_checks, get_state, ipc, wait_for};
use crate::native::play_and_record;
use crate::render::{BLOCK_FRAMES, load_exported_chain, render};
use crate::report::{Report, ScenarioResult, Status, settle};
use crate::scenario::Scenario;
use crate::stimulus::generate;
use anyhow::{Context, Result, ensure};
use cpal::traits::{DeviceTrait, HostTrait};
use resonance_ipc::Command;
use std::path::Path;
use std::time::Duration;

const OS: &str = "macos";

fn input_device_named(name: &str) -> Result<cpal::Device> {
    cpal::default_host()
        .input_devices()?
        .find(|d| d.description().is_ok_and(|x| x.name() == name))
        .with_context(|| format!("no input device named {name}"))
}

/// The daemon must be tapping the scenario's device and rendering to the result device.
fn await_daemon_format(s: &Scenario) -> Result<()> {
    let mut last = String::new();
    let ok = wait_for(
        "daemon on the scenario format",
        Duration::from_secs(20),
        || {
            let st = get_state()?;
            last = format!(
                "{} ch @ {} Hz, output {:?}",
                st.channels, st.sample_rate, st.active_output
            );
            Ok(st.channels == s.channels
                && (st.sample_rate - f64::from(s.rate)).abs() < 0.5
                && st.active_output.as_deref() == Some(OUT_DEVICE))
        },
    );
    ok.with_context(|| {
        format!(
            "daemon reports {last}; want {} ch @ {} Hz → {OUT_DEVICE}",
            s.channels, s.rate
        )
    })
}

fn measure(
    r: &mut ScenarioResult,
    s: &Scenario,
    opts: &RunOpts,
    audiodev: &Path,
    dir: &Path,
) -> Result<()> {
    let input = in_device(s.channels).context("no tapped device for this channel count")?;
    env::set_devices(audiodev, input, OUT_DEVICE, s.rate)?;
    env::start_daemon()?;
    let res = (|| -> Result<()> {
        ipc(Command::SetOutputTarget {
            node_name: OUT_DEVICE.into(),
        })?;
        await_daemon_format(s)?;
        apply_profile(s, dir)?;
        let export = dir.join("chain.bin");
        ipc(Command::ResetAndExportChain {
            path: export.to_string_lossy().into_owned(),
        })?;
        std::thread::sleep(Duration::from_millis(300));
        let stim = generate(s.rate, s.channels, s.body_secs);
        let (mut chain, notes) = load_exported_chain(&export, s.channels, f64::from(s.rate))?;
        r.notes.extend(notes);
        let expected = render(&mut chain, &stim.samples, BLOCK_FRAMES);

        let play = cpal::default_host()
            .default_output_device()
            .context("no default output device")?;
        let record = input_device_named(OUT_DEVICE)?;
        let rec = play_and_record(
            &play,
            &record,
            true,
            &stim.samples,
            s.channels,
            s.rate,
            (MAX_LAG_SECS * f64::from(s.rate)) as usize,
        )?;
        r.discontinuities = rec.discontinuities;
        let st = get_state()?;
        ensure!(
            st.active_output.as_deref() == Some(OUT_DEVICE),
            "daemon output changed during the run: {:?}",
            st.active_output
        );
        exact_checks(r, s, &stim, &rec, &expected, dir)
    })();
    env::stop_daemon();
    let _ = std::fs::copy(env::DAEMON_LOG, dir.join("daemon.log"));
    let _ = opts;
    res
}

fn run_once(s: &Scenario, opts: &RunOpts, audiodev: &Path) -> Result<ScenarioResult> {
    let dir = opts.out_dir.join(&s.id);
    std::fs::create_dir_all(&dir)?;
    let mut r = ScenarioResult::new(&s.id, OS);
    r.expected_fail.clone_from(&s.expected_fail);
    if let Err(e) = measure(&mut r, s, opts, audiodev, &dir) {
        r.failures.push(format!("{e:#}"));
    }
    if !r.failures.is_empty() {
        r.artifacts = Some(dir.to_string_lossy().into_owned());
    }
    r.status = settle(r.failures.is_empty(), s.expected_fail.is_some());
    Ok(r)
}

pub fn run(opts: &RunOpts, audiodev: &Path) -> Result<Report> {
    let (mut todo, skipped): (Vec<_>, Vec<_>) =
        opts.scenarios.iter().partition(|s| ineligible(s).is_none());
    todo.sort_by_key(|s| (s.channels, s.rate));
    let mut results = Vec::new();
    for s in todo {
        eprintln!("e2e: {}", s.id);
        let mut r = run_once(s, opts, audiodev)?;
        if r.status == Status::Fail && r.discontinuities > 0 {
            let first = r.failures.join("; ");
            let rerun = run_once(s, opts, audiodev)?;
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
            .map(|s| (s.id.clone(), ineligible(s).unwrap_or_default().into()))
            .collect(),
    })
}
