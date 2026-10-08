//! Execute scenarios against the live daemon inside the e2e container.

use crate::checks::{
    PITCH_TOLERANCE, first_signal_frame, is_flowing, last_signal_frame, longest_zero_run,
    pitch_error,
};
use crate::common::{
    MAX_LAG_SECS, RunOpts, apply_profile, ch0, exact_checks, get_state, ipc, resample_checks,
};
use crate::compare::{CompareMode, compare};
use crate::latency::{arrival_lags, failure, judge, load_baselines, median, save_baselines};
use crate::linux::env::{self, DEVICE, DEVICE2, Daemon, RESONANCE_SINK};
use crate::linux::pw::{Play, PlayRec, RecordTarget, Timed, play_and_record};
use crate::ratechain::{Hop, LINUX_RESONANCE_STEPS, RateChain};
use crate::render::{BLOCK_FRAMES, load_exported_chain, render};
use crate::report::{Report, ScenarioResult, Status, settle};
use crate::scenario::{EventKind, Scenario};
use crate::stimulus::{Stimulus, chirp_train, generate};
use anyhow::{Context, Result, bail, ensure};
use resonance_ipc::Command;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const OS: &str = "linux";
const TRAINS: usize = 3;

/// The harness itself must be bit-exact before Resonance is judged: a null
/// sink's own loopback (no Resonance in the path) has to compare equal.
pub fn harness_check() -> Result<()> {
    let _ = env::destroy_device(DEVICE);
    env::force_graph_rate(Some(48_000))?;
    env::create_device(DEVICE, Some(48_000), 2)?;
    let stim = generate(48_000, 2, 1.0);
    let pr = play_and_record(
        &Play {
            node: DEVICE,
            rate: 48_000,
            channels: 2,
            samples: &stim.samples,
        },
        &[RecordTarget {
            node: DEVICE,
            rate: 48_000,
            channels: 2,
        }],
        48_000,
        Vec::new(),
        Duration::from_secs(20),
    )?;
    let o = compare(
        &stim.samples,
        &pr.recordings[0].samples,
        2,
        stim.body.clone(),
        48_000,
    );
    ensure!(
        o.passes(CompareMode::Exact),
        "harness loopback is not bit-exact: {}",
        o.describe()
    );
    Ok(())
}

/// Median total latency (ms) of a played chirp train on recording `rec_idx`.
fn latency_ms(pr: &PlayRec, rec_idx: usize, rate: u32) -> Result<f64> {
    let train = chirp_train(rate, 1);
    let rec = &pr.recordings[rec_idx];
    let template: Vec<f64> = train.samples[train.emit[0]..train.emit[0] + train.chirp_frames]
        .iter()
        .map(|&v| f64::from(v))
        .collect();
    let max_lag = (0.45 * f64::from(rate)) as usize;
    let lags = arrival_lags(
        &ch0(&rec.samples, rec.channels),
        &template,
        &train.emit,
        max_lag,
    );
    let (Some(cap0), Some(play0)) = (rec.first_tick, pr.play_first_tick) else {
        bail!("stream positions unavailable");
    };
    let offset = cap0 as f64 - play0 as f64;
    let ms = lags
        .iter()
        .map(|&l| (l as f64 + offset) * 1000.0 / f64::from(pr.graph_rate.max(1)));
    median(ms.collect()).context("no chirps")
}

/// Latency floor over [`TRAINS`] chirp trains. One train can land 1-2 graph
/// quanta high (the daemon's ring buffer fill varies run to run); the minimum
/// is the stable figure a regression gate can use.
fn play_train(target: &str, s: &Scenario) -> Result<f64> {
    (0..TRAINS)
        .map(|_| play_train_once(target, s))
        .try_fold(f64::INFINITY, |lo, ms| ms.map(|ms| lo.min(ms)))
}

fn play_train_once(target: &str, s: &Scenario) -> Result<f64> {
    let train = chirp_train(s.rate, s.channels);
    let pr = play_and_record(
        &Play {
            node: target,
            rate: s.rate,
            channels: s.channels,
            samples: &train.samples,
        },
        &[RecordTarget {
            node: DEVICE,
            rate: s.rate,
            channels: s.channels,
        }],
        s.rate as usize / 2,
        Vec::new(),
        Duration::from_secs(20),
    )?;
    latency_ms(&pr, 0, s.rate)
}

/// Fresh devices + graph rate for one scenario (no leftovers from the last).
fn reset_devices(s: &Scenario) -> Result<()> {
    let _ = env::destroy_device(DEVICE);
    let _ = env::destroy_device(DEVICE2);
    env::force_graph_rate(Some(s.graph_rate))?;
    let follows_graph = s
        .events
        .iter()
        .any(|e| matches!(e.kind, EventKind::ForceRate { .. }));
    env::create_device(DEVICE, (!follows_graph).then_some(s.rate), s.channels)?;
    for e in &s.events {
        if let EventKind::SwitchDevice { rate, channels } = e.kind {
            env::create_device(DEVICE2, Some(rate), channels)?;
        }
    }
    env::set_default_sink(DEVICE)
}

/// Wait until the daemon reports the scenario's format on the scenario's
/// device; on timeout, fail with the actual mismatch.
fn await_format(s: &Scenario) -> Result<()> {
    let check = || -> Result<()> {
        let st = get_state()?;
        env::check_on_state(
            st.channels,
            st.sample_rate,
            st.active_output.as_deref(),
            DEVICE,
            s.rate,
            s.channels,
        )
    };
    env::wait_for(
        "daemon on the scenario device",
        Duration::from_secs(10),
        || Ok(check().is_ok()),
    )
    .or_else(|_| check())
}

/// Recording targets and timed actions for a scenario's events.
fn event_actions(
    s: &Scenario,
    stim: &Stimulus,
    daemon: &Arc<Mutex<Option<Daemon>>>,
    daemon_bin: &Path,
    log: &Path,
) -> (Vec<RecordTarget<'static>>, Vec<Timed>) {
    let mut records = vec![RecordTarget {
        node: DEVICE,
        rate: s.rate,
        channels: s.channels,
    }];
    let mut timed = Vec::new();
    for e in &s.events {
        let at_frame = stim.body.start + (e.at_secs * f64::from(s.player_rate)) as usize;
        let action: Box<dyn FnOnce() + Send> = match e.kind {
            EventKind::ForceRate { rate } => Box::new(move || {
                let _ = env::force_graph_rate(Some(rate));
            }),
            EventKind::SwitchDevice { rate, channels } => {
                records.push(RecordTarget {
                    node: DEVICE2,
                    rate,
                    channels,
                });
                Box::new(|| {
                    let _ = ipc(Command::SetOutputTarget {
                        node_name: DEVICE2.into(),
                    });
                })
            }
            EventKind::RestartDaemon => {
                let (d, bin, log) = (
                    Arc::clone(daemon),
                    daemon_bin.to_path_buf(),
                    log.to_path_buf(),
                );
                Box::new(move || {
                    let mut g = d.lock().expect("daemon lock");
                    if let Some(old) = g.take() {
                        let _ = old.stop();
                    }
                    *g = Daemon::start(&bin, &log).ok();
                    let _ = ipc(Command::SetOutputTarget {
                        node_name: DEVICE.into(),
                    });
                })
            }
        };
        timed.push(Timed { at_frame, action });
    }
    (records, timed)
}

/// Mid-stream events: correct pitch after the last event, gap within the
/// limit, and audio still flowing at the end.
#[allow(clippy::single_match_else)] // two-device vs same-device split reads best as a match
fn event_checks(r: &mut ScenarioResult, s: &Scenario, stim: &Stimulus, pr: &PlayRec) {
    let last = pr.recordings.last().expect("at least one recording");
    let ch = last.channels;
    let rec_rate = f64::from(last.rate);
    let to_rec = |play_frame: f64| (play_frame * rec_rate / f64::from(s.player_rate)) as usize;
    let end = to_rec(stim.body.end as f64).min(last.samples.len() / ch);
    let last_event = s.events.iter().map(|e| e.at_secs).fold(0.0, f64::max);
    let after =
        to_rec(stim.body.start as f64 + (last_event + 0.5) * f64::from(s.player_rate)).min(end);
    let seg: Vec<f32> = last.samples[after * ch..end * ch]
        .iter()
        .step_by(ch)
        .copied()
        .collect();
    let pe = pitch_error(&seg, rec_rate);
    if pe > PITCH_TOLERANCE {
        r.failures.push(format!(
            "pilot pitch off by {:.4} % after the event",
            pe * 100.0
        ));
    }
    let gap_ms = match pr.recordings.as_slice() {
        // Device switch: old device's last audio → new device's first, on the graph clock.
        [a, b] => match (
            a.first_tick,
            b.first_tick,
            last_signal_frame(&a.samples, a.channels),
            first_signal_frame(&b.samples, b.channels),
        ) {
            (Some(ta), Some(tb), Some(la), Some(fb)) => {
                ((tb + fb as u64) as f64 - (ta + la as u64) as f64) * 1000.0
                    / f64::from(pr.graph_rate.max(1))
            }
            _ => f64::INFINITY,
        },
        // Same device: longest silence after the stimulus first arrived.
        _ => {
            let first = first_signal_frame(&last.samples, ch).unwrap_or(end);
            longest_zero_run(&last.samples, ch, first..end) as f64 * 1000.0 / rec_rate
        }
    };
    if gap_ms > s.expect.max_gap_ms {
        r.failures.push(format!(
            "audio gap {gap_ms:.0} ms > {} ms",
            s.expect.max_gap_ms
        ));
    }
    if !is_flowing(
        &last.samples,
        ch,
        end.saturating_sub((0.2 * rec_rate) as usize)..end,
    ) {
        r.failures
            .push("audio not flowing at the end of the stimulus".into());
    }
}

/// Everything between "daemon started" and "daemon stopped" for one scenario.
fn measure(
    r: &mut ScenarioResult,
    s: &Scenario,
    opts: &RunOpts,
    off_ms: &BTreeMap<(u32, usize), f64>,
    baselines: &BTreeMap<String, f64>,
    daemon: &Arc<Mutex<Option<Daemon>>>,
    dir: &Path,
) -> Result<()> {
    ipc(Command::SetOutputTarget {
        node_name: DEVICE.into(),
    })?;
    await_format(s)?;
    apply_profile(s, dir)?;
    env::verify_on(DEVICE, &get_state()?, s.rate, s.channels)?;

    let export = dir.join("chain.bin");
    let export_str = export.to_string_lossy().into_owned();
    if s.measures_latency() {
        ipc(Command::ResetAndExportChain {
            path: export_str.clone(),
        })?;
        let on = play_train(RESONANCE_SINK, s)?;
        let off = *off_ms
            .get(&(s.rate, s.channels))
            .context("no Resonance-off latency for this format")?;
        let added = on - off;
        let v = judge(added, baselines.get(&s.id).copied());
        (
            r.latency_on_ms,
            r.latency_off_ms,
            r.added_latency_ms,
            r.latency_verdict,
        ) = (Some(on), Some(off), Some(added), Some(v));
        r.failures.extend(failure(v, added, opts.update_baseline));
    }

    ipc(Command::ResetAndExportChain { path: export_str })?;
    let state = get_state()?;
    let stim = generate(s.player_rate, s.channels, s.body_secs);
    let (records, timed) =
        event_actions(s, &stim, daemon, &opts.daemon_bin, &dir.join("daemon.log"));
    let timeout =
        Duration::from_secs_f64(3.0 * stim.frames() as f64 / f64::from(s.player_rate) + 15.0);
    let pr = play_and_record(
        &Play {
            node: RESONANCE_SINK,
            rate: s.player_rate,
            channels: s.channels,
            samples: &stim.samples,
        },
        &records,
        (MAX_LAG_SECS * f64::from(s.rate)) as usize,
        timed,
        timeout,
    )?;
    r.discontinuities =
        pr.play_discontinuities + pr.recordings.iter().map(|x| x.discontinuities).sum::<u32>();

    let end_state = get_state()?;
    let device_node = pr.recordings.last().map_or(DEVICE, |x| x.node.as_str());
    let rates = RateChain {
        hops: vec![
            Hop {
                name: "player".into(),
                rate: s.player_rate,
            },
            Hop {
                name: "graph".into(),
                rate: pr.graph_rate,
            },
            Hop {
                name: "capture".into(),
                rate: end_state.capture_rate as u32,
            },
            Hop {
                name: "dsp".into(),
                rate: end_state.sample_rate as u32,
            },
            Hop {
                name: "device".into(),
                rate: env::node_rate(&env::pw_dump()?, device_node).unwrap_or(pr.graph_rate),
            },
        ],
    };
    r.resamplings = rates.resamplings(&s.expect.resample, LINUX_RESONANCE_STEPS);
    for x in r.resamplings.iter().filter(|x| x.reason.is_none()) {
        r.failures.push(format!(
            "undeclared resampling at {} ({} → {} Hz)",
            x.step, x.from_rate, x.to_rate
        ));
    }
    r.rate_chain = Some(rates);

    if !s.events.is_empty() {
        event_checks(r, s, &stim, &pr);
    } else if s.player_rate == s.rate && s.graph_rate == s.rate {
        let (mut chain, notes) = load_exported_chain(&export, state.channels, state.sample_rate)?;
        r.notes.extend(notes);
        let expected = render(&mut chain, &stim.samples, BLOCK_FRAMES);
        exact_checks(r, s, &stim, &pr.recordings[0], &expected, dir)?;
    } else {
        resample_checks(r, s, &stim, &pr.recordings[0], &export, &state)?;
    }
    Ok(())
}

fn run_once(
    s: &Scenario,
    opts: &RunOpts,
    off_ms: &BTreeMap<(u32, usize), f64>,
    baselines: &BTreeMap<String, f64>,
) -> Result<ScenarioResult> {
    let dir = opts.out_dir.join(&s.id);
    std::fs::create_dir_all(&dir)?;
    let mut r = ScenarioResult::new(&s.id, OS);
    r.expected_fail.clone_from(&s.expected_fail);
    reset_devices(s)?;
    let daemon = Arc::new(Mutex::new(Some(Daemon::start(
        &opts.daemon_bin,
        &dir.join("daemon.log"),
    )?)));
    if let Err(e) = measure(&mut r, s, opts, off_ms, baselines, &daemon, &dir) {
        r.failures.push(format!("{e:#}"));
    }
    if let Some(d) = daemon.lock().expect("daemon lock").take() {
        let _ = d.stop();
    }
    if !r.failures.is_empty() {
        if let Ok(d) = env::pw_dump() {
            let _ = std::fs::write(
                dir.join("pw-dump.json"),
                serde_json::to_vec_pretty(&d).unwrap_or_default(),
            );
        }
        r.artifacts = Some(dir.to_string_lossy().into_owned());
    }
    r.status = settle(r.failures.is_empty(), s.expected_fail.is_some());
    Ok(r)
}

/// Resonance-off latency per device format, measured with Resonance verifiably absent.
fn measure_off(scenarios: &[&Scenario]) -> Result<BTreeMap<(u32, usize), f64>> {
    let mut off = BTreeMap::new();
    for s in scenarios.iter().filter(|s| s.measures_latency()) {
        if off.contains_key(&(s.rate, s.channels)) {
            continue;
        }
        reset_devices(s)?;
        env::verify_off(DEVICE)?;
        off.insert((s.rate, s.channels), play_train(DEVICE, s)?);
    }
    Ok(off)
}

pub fn run(opts: &RunOpts) -> Result<Report> {
    harness_check().context("harness self-check (null-sink loopback)")?;
    let mut baselines = load_baselines(&opts.baselines_path)?;
    let selected: Vec<&Scenario> = opts.scenarios.iter().collect();
    let off = measure_off(&selected).context("measuring Resonance-off latency")?;
    let mut results = Vec::new();
    for s in &selected {
        eprintln!("e2e: {}", s.id);
        let mut r = run_once(s, opts, &off, &baselines)?;
        if r.status == Status::Fail && r.discontinuities > 0 {
            let first = r.failures.join("; ");
            let rerun = run_once(s, opts, &off, &baselines)?;
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
                baselines.insert(r.id.clone(), (a * 100.0).round() / 100.0);
            }
        }
        results.push(r);
    }
    if opts.update_baseline {
        save_baselines(&opts.baselines_path, &baselines)?;
    }
    Ok(Report {
        os: OS.into(),
        tier: format!("{:?}", opts.tier).to_lowercase(),
        results,
        skipped: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::synthetic_ir;

    #[test]
    fn synthetic_ir_is_a_decaying_stereo_float_wav() {
        let p = std::env::temp_dir().join(format!("resonance-e2e-ir-{}.wav", std::process::id()));
        synthetic_ir(&p, 48_000).unwrap();
        let r = hound::WavReader::open(&p).unwrap();
        let spec = r.spec();
        assert_eq!(
            (spec.channels, spec.sample_rate, spec.sample_format),
            (2, 48_000, hound::SampleFormat::Float)
        );
        let s: Vec<f32> = r.into_samples::<f32>().map(Result::unwrap).collect();
        assert_eq!(s.len(), 2 * 4096);
        let head: f32 = s[..512].iter().map(|v| v.abs()).sum();
        let tail: f32 = s[s.len() - 512..].iter().map(|v| v.abs()).sum();
        assert!(head > 10.0 * tail, "decays: head {head} tail {tail}");
    }
}
