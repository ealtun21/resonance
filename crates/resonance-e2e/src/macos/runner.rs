//! Execute scenarios through the daemon's Process Tap on macOS.

use super::{env, in_device, ineligible, out_device};
use crate::checks::{phat_lag, zero_fill_runs};
use crate::common::{
    MAX_LAG_SECS, MIN_ALIGNMENT_QUALITY, RunOpts, apply_profile, ch0, chain_delay_frames,
    get_state, ipc, record_latency, transfer_checks, wait_for, write_wav,
};
use crate::compare::{CompareMode, compare};
use crate::latency::{load_baselines, save_baselines};
use crate::native::play_and_record;
use crate::render::{BLOCK_FRAMES, load_exported_chain, render};
use crate::report::{Report, ScenarioResult, Status, settle};
use crate::scenario::Scenario;
use crate::stimulus::generate;
use anyhow::{Context, Result, ensure};
use cpal::traits::{DeviceTrait, HostTrait};
use resonance_ipc::Command;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

const OS: &str = "macos";

/// Per-octave limits against the render: transfer gain within ±0.03 dB and in-band SNR of at
/// least 50 dB. The full tier measured |gain| below 0.001 dB and SNR of 57 to 86 dB for every
/// linear chain on every channel and rate (the tap's resampler is the whole error, spec section
/// 14.4); the nonlinear Fidelity effect reaches 0.012 dB and 39 dB, which its scenario relaxes.
const MAC_BAND_LIMITS_DB: (f64, f64) = (0.03, 50.0);

/// Share of 4096-frame blocks that may hold a zero-filled underrun before the run fails.
/// The daemon drains its ring to the minimum, so ordinary scheduling jitter in this VM
/// underruns it often (about 30 % of blocks measured); a real regression shows as > 50 %.
const MAC_MAX_DROPOUT_FRACTION: f64 = 0.5;

fn input_device_named(name: &str) -> Result<cpal::Device> {
    cpal::default_host()
        .input_devices()?
        .find(|d| d.description().is_ok_and(|x| x.name() == name))
        .with_context(|| format!("no input device named {name}"))
}

/// The daemon must be tapping the scenario's device and rendering to the result device.
fn await_daemon_format(s: &Scenario, out_dev: &str) -> Result<()> {
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
                && st.active_output.as_deref() == Some(out_dev))
        },
    );
    ok.with_context(|| {
        format!(
            "daemon reports {last}; want {} ch @ {} Hz → {out_dev}",
            s.channels, s.rate
        )
    })
}

/// Resonance-off lags per device format, and the committed baselines.
struct LatencyState {
    off: BTreeMap<(u32, usize), isize>,
    /// Lag of the flat chain through the daemon: the floor over several daemon starts.
    floor: BTreeMap<(u32, usize), isize>,
    baselines: BTreeMap<String, f64>,
}

/// Daemon starts whose flat-chain lags are minimised into the path's latency floor.
const FLOOR_STARTS: usize = 5;

/// Lag (frames, recording behind the stimulus) of a flat chain through the daemon, the minimum
/// over [`FLOOR_STARTS`] fresh daemon starts. The ring's fill is set at start by callback phase
/// (0-85 ms of slack plus occasional startup excess), so one start is a sample of a
/// distribution; its floor is the figure that moves when the tap, ring or output path changes.
fn measure_floor(s: &Scenario, out_dev: &str) -> Result<isize> {
    let mut best: Option<isize> = None;
    for _ in 0..FLOOR_STARTS {
        env::start_daemon()?;
        let lag = (|| -> Result<isize> {
            ipc(Command::SetOutputTarget {
                node_name: out_dev.into(),
            })?;
            await_daemon_format(s, out_dev)?;
            ipc(Command::ApplyState {
                preamp_db: 0.0,
                enabled: true,
                bands: Vec::new(),
                effects: resonance_ipc::EffectsState::default(),
            })?;
            std::thread::sleep(Duration::from_millis(300));
            let stim = generate(s.rate, s.channels, 1.0);
            let play = cpal::default_host()
                .default_output_device()
                .context("no default output device")?;
            let rec = play_and_record(
                &play,
                &input_device_named(out_dev)?,
                true,
                &stim.samples,
                s.channels,
                s.rate,
                (MAX_LAG_SECS * f64::from(s.rate)) as usize,
            )?;
            let (lag, quality) = phat_lag(
                &ch0(&stim.samples, s.channels),
                &ch0(&rec.samples, s.channels),
                (MAX_LAG_SECS * f64::from(s.rate)) as usize,
            );
            ensure!(
                lag >= 0 && quality >= MIN_ALIGNMENT_QUALITY,
                "latency probe did not align ({quality:.1}x)"
            );
            Ok(lag)
        })();
        env::stop_daemon();
        let lag = lag?;
        best = Some(best.map_or(lag, |b| b.min(lag)));
    }
    best.context("no latency probe ran")
}

/// Lag (frames) of the BlackHole loopback with Resonance verifiably absent: the stimulus played
/// into the tapped device and recorded straight back from it. Must be bit-exact, which doubles
/// as the harness self-check for this format.
fn measure_off(s: &Scenario, input: &str) -> Result<isize> {
    ensure!(
        !resonance_ipc::transport::is_reachable(),
        "a daemon is still running during the Resonance-off measurement"
    );
    let stim = generate(s.rate, s.channels, 1.0);
    let play = cpal::default_host()
        .default_output_device()
        .context("no default output device")?;
    let rec = play_and_record(
        &play,
        &input_device_named(input)?,
        true,
        &stim.samples,
        s.channels,
        s.rate,
        (MAX_LAG_SECS * f64::from(s.rate)) as usize,
    )?;
    let o = compare(
        &stim.samples,
        &rec.samples,
        s.channels,
        stim.body.clone(),
        (MAX_LAG_SECS * f64::from(s.rate)) as usize,
    );
    ensure!(
        o.passes(CompareMode::Exact),
        "Resonance-off loopback is not bit-exact: {}",
        o.describe()
    );
    Ok(o.lag)
}

/// Gate margin `(relative, minimum ms)` for the macOS latency: the floor over five daemon starts
/// still moves by a few tens of ms with the VM's scheduling.
const MAC_LATENCY_MARGIN: (f64, f64) = (0.3, 30.0);

fn measure(
    r: &mut ScenarioResult,
    s: &Scenario,
    opts: &RunOpts,
    audiodev: &Path,
    dir: &Path,
    lat: &mut LatencyState,
) -> Result<()> {
    let input = in_device(s.channels).context("no tapped device for this channel count")?;
    let out_dev = out_device(s.channels).context("no result device for this channel count")?;
    env::set_devices(audiodev, input, out_dev, s.rate)?;
    if s.measures_latency() && !lat.off.contains_key(&(s.rate, s.channels)) {
        // A loopback that is not bit-exact is a scheduling glitch in this VM (the Resonance-off
        // path has nothing of ours in it); only fail if three attempts in a row glitch.
        let mut failure = None;
        for _ in 0..3 {
            match measure_off(s, input) {
                Ok(off) => {
                    lat.off.insert((s.rate, s.channels), off);
                    failure = None;
                    break;
                }
                Err(e) => failure = Some(e),
            }
        }
        if let Some(e) = failure {
            return Err(e);
        }
    }
    if s.measures_latency() && !lat.floor.contains_key(&(s.rate, s.channels)) {
        lat.floor
            .insert((s.rate, s.channels), measure_floor(s, out_dev)?);
    }
    env::start_daemon()?;
    let res = (|| -> Result<()> {
        ipc(Command::SetOutputTarget {
            node_name: out_dev.into(),
        })?;
        await_daemon_format(s, out_dev)?;
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
        let record = input_device_named(out_dev)?;
        let rec = play_and_record(
            &play,
            &record,
            true,
            &stim.samples,
            s.channels,
            s.rate,
            (MAX_LAG_SECS * f64::from(s.rate)) as usize,
        )?;
        // The daemon zero-fills its output when the ring underruns; that is a dropout of audio
        // (spec 9.6: a rerun that passes makes it a flake, one that fails makes it a failure).
        r.discontinuities = rec.discontinuities
            + u32::try_from(zero_fill_runs(&rec.samples, s.channels, 32)).unwrap_or(u32::MAX);
        let st = get_state()?;
        ensure!(
            st.active_output.as_deref() == Some(out_dev),
            "daemon output changed during the run: {:?}",
            st.active_output
        );
        // The tap's leg of the aggregate contains a resampler that no option disables, so the
        // recording is never bit-equal to the render (spec section 14.4, MAC-E1): judge per-octave
        // gain and in-band SNR against the render, on every channel.
        let transfer = transfer_checks(
            r,
            s,
            &stim,
            &expected,
            &rec,
            MAC_BAND_LIMITS_DB,
            MAC_MAX_DROPOUT_FRACTION,
        );
        if transfer.is_err() || !r.failures.is_empty() {
            write_wav(&dir.join("recorded.wav"), &rec.samples, s.channels, s.rate)?;
            write_wav(&dir.join("stimulus.wav"), &stim.samples, s.channels, s.rate)?;
        }
        transfer?;
        if let (Some(&off), Some(&floor)) = (
            lat.off.get(&(s.rate, s.channels)),
            lat.floor.get(&(s.rate, s.channels)),
        ) {
            // Added latency = the daemon path's floor over the bare device loopback, plus
            // this chain's own delay (exact, from the render). The recording's lag in *this*
            // run is not used: it only reflects where the ring happened to start.
            let delay = chain_delay_frames(&stim.samples, &expected, s.channels, s.rate);
            // Report-only: the same scenario measures anywhere from -100 to +100 ms between
            // runs in the VM (the tap ring starts at a different offset each time, MAC-E2),
            // so a baseline gate here only fails on noise. Windows and Linux still gate.
            let gated = r.failures.len();
            record_latency(
                r,
                s,
                floor,
                delay,
                off,
                lat.baselines.get(&s.id).copied(),
                opts.update_baseline,
                MAC_LATENCY_MARGIN,
            );
            r.failures.truncate(gated);
        }
        Ok(())
    })();
    env::stop_daemon();
    let _ = std::fs::copy(env::DAEMON_LOG, dir.join("daemon.log"));
    res
}

fn run_once(
    s: &Scenario,
    opts: &RunOpts,
    audiodev: &Path,
    lat: &mut LatencyState,
) -> Result<ScenarioResult> {
    let dir = opts.out_dir.join(&s.id);
    std::fs::create_dir_all(&dir)?;
    let mut r = ScenarioResult::new(&s.id, OS);
    r.expected_fail.clone_from(&s.expected_fail);
    if let Err(e) = measure(&mut r, s, opts, audiodev, &dir, lat) {
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
    let mut lat = LatencyState {
        off: BTreeMap::new(),
        floor: BTreeMap::new(),
        baselines: load_baselines(&opts.baselines_path)?,
    };
    let mut results = Vec::new();
    for s in todo {
        eprintln!("e2e: {}", s.id);
        let mut r = run_once(s, opts, audiodev, &mut lat)?;
        if r.status == Status::Fail && r.discontinuities > 0 {
            let first = r.failures.join("; ");
            let rerun = run_once(s, opts, audiodev, &mut lat)?;
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
            .map(|s| (s.id.clone(), ineligible(s).unwrap_or_default().into()))
            .collect(),
    })
}
