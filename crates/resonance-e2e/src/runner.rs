//! Execute scenarios against the live daemon inside the e2e container.

use crate::checks::{
    BAND_TOLERANCE_DB, PITCH_TOLERANCE, block_rms_db, first_signal_frame, is_flowing,
    last_signal_frame, longest_zero_run, pitch_error,
};
use crate::common::{
    MAX_LAG_SECS, RunOpts, apply_profile, ch0, exact_checks, get_state, ipc, resample_checks,
    stimulus_for, tone_checks, write_wav,
};
use crate::compare::{CompareMode, compare};
use crate::latency::{
    arrival_lags, failure, judge_with_margin, load_baselines, median, save_baselines,
};
use crate::linux::env::{self, DEVICE, DEVICE2, Daemon, RESONANCE_SINK};
use crate::linux::pw::{Play, PlayRec, RecordTarget, Timed, play_and_record};
use crate::ratechain::{Hop, LINUX_RESONANCE_STEPS, RateChain};
use crate::render::{BLOCK_FRAMES, load_exported_chain, render};
use crate::report::{Report, ScenarioResult, Status, settle};
use crate::scenario::{EventKind, Kind, Scenario};
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
    // One second device however many times the scenario switches to it (a duplicate node of the
    // same name would take the audio away from the recording).
    if let Some((rate, channels)) = s.events.iter().find_map(|e| match e.kind {
        EventKind::SwitchDevice { rate, channels } => Some((rate, channels)),
        _ => None,
    }) {
        env::create_device(DEVICE2, Some(rate), channels)?;
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
                if !records.iter().any(|t| t.node == DEVICE2) {
                    records.push(RecordTarget {
                        node: DEVICE2,
                        rate,
                        channels,
                    });
                }
                Box::new(|| {
                    let _ = ipc(Command::SetOutputTarget {
                        node_name: DEVICE2.into(),
                    });
                })
            }
            EventKind::SwitchBack => Box::new(|| {
                let _ = ipc(Command::SetOutputTarget {
                    node_name: DEVICE.into(),
                });
            }),
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

/// An event's outage may last `max_gap_ms`; the chain then gets this much longer to settle
/// before the stretch after it is judged.
const SETTLE_MARGIN_SECS: f64 = 0.5;
/// The first stretch is analysed from here after the stimulus starts; every stretch ends this
/// long before the next event (the player runs ahead of the recording, so an event can show up
/// in the recording ~0.1 s before its nominal time).
const FIRST_SEGMENT_START_SECS: f64 = 1.0;
const SEGMENT_END_MARGIN_SECS: f64 = 0.3;
/// How far before its nominal time an event can already show in the recording.
const EVENT_EARLY_SECS: f64 = 0.15;
/// Shortest stretch the pilot's pitch resolves in to well under [`PITCH_TOLERANCE`].
const MIN_SEGMENT_SECS: f64 = 1.5;
/// Settled stretches may differ from the first by at most this many dB in level.
const SEGMENT_LEVEL_DB: f64 = 0.5;
/// Longest all-channel silence allowed inside a settled stretch, in seconds.
const SEGMENT_MAX_GAP_SECS: f64 = 0.001;

/// Rate and device renegotiations under a steady pilot: every stretch between events, once the
/// chain has had its allowed outage plus [`SETTLE_MARGIN_SECS`], must carry the pilot at the right pitch and level with no
/// gap, on whichever device the daemon is feeding at that time (recording 0 = the primary,
/// recording 1 = the second device).
fn stress_checks(r: &mut ScenarioResult, s: &Scenario, stim: &Stimulus, pr: &PlayRec) {
    let settle = s.expect.max_gap_ms / 1000.0 + SETTLE_MARGIN_SECS;
    let mut events: Vec<_> = s.events.iter().collect();
    events.sort_by(|a, b| a.at_secs.total_cmp(&b.at_secs));
    let mut active = 0usize;
    let mut starts = vec![(FIRST_SEGMENT_START_SECS, 0usize)];
    let mut ends = Vec::new();
    for e in &events {
        ends.push(e.at_secs);
        match e.kind {
            EventKind::SwitchDevice { .. } => active = 1,
            EventKind::SwitchBack | EventKind::RestartDaemon => active = 0,
            EventKind::ForceRate { .. } => {}
        }
        starts.push((e.at_secs + settle, active));
    }
    ends.push(s.body_secs);
    let body0 = stim.body.start as f64 / f64::from(s.player_rate);
    // Outage after each event: silence on the recording that carries the audio afterwards.
    for (k, e) in events.iter().enumerate() {
        let Some(rec) = pr.recordings.get(starts[k + 1].1) else {
            continue;
        };
        let rate = f64::from(rec.rate);
        let lo = ((body0 + e.at_secs - EVENT_EARLY_SECS) * rate) as usize;
        let hi =
            (((body0 + e.at_secs + settle) * rate) as usize).min(rec.samples.len() / rec.channels);
        let outage_ms = longest_zero_run(&rec.samples, rec.channels, lo..hi) as f64 * 1000.0 / rate;
        r.notes.push(format!(
            "event {k} ({:?} at {:.0} s): outage {outage_ms:.0} ms",
            e.kind, e.at_secs
        ));
        if outage_ms > s.expect.max_gap_ms {
            r.failures.push(format!(
                "event {k} ({:?} at {:.0} s): audio gap {outage_ms:.0} ms > {} ms",
                e.kind, e.at_secs, s.expect.max_gap_ms
            ));
        }
    }
    let mut first_level: Option<f64> = None;
    for (k, (&(from, idx), &to)) in starts.iter().zip(&ends).enumerate() {
        let to = to - SEGMENT_END_MARGIN_SECS;
        if to - from < MIN_SEGMENT_SECS {
            r.failures.push(format!(
                "segment {k}: only {:.2} s settled, need {MIN_SEGMENT_SECS} s (scenario events too close)",
                to - from
            ));
            continue;
        }
        let Some(rec) = pr.recordings.get(idx) else {
            r.failures.push(format!("segment {k}: no recording {idx}"));
            continue;
        };
        let rate = f64::from(rec.rate);
        let lo = ((body0 + from) * rate) as usize;
        let hi = (((body0 + to) * rate) as usize).min(rec.samples.len() / rec.channels);
        if hi <= lo {
            r.failures
                .push(format!("segment {k}: recording ends early"));
            continue;
        }
        let x: Vec<f32> = rec.samples[lo * rec.channels..hi * rec.channels]
            .iter()
            .step_by(rec.channels)
            .copied()
            .collect();
        let pe = pitch_error(&x, rate);
        let level = block_rms_db(&x, x.len())[0];
        let first = *first_level.get_or_insert(level);
        let gap = longest_zero_run(&x, 1, 0..x.len()) as f64 / rate;
        r.notes.push(format!(
            "segment {k} ({from:.1}-{to:.1} s, {} @ {} Hz x{}): pitch error {:.5} %, level {:+.3} dB vs first, longest gap {:.1} ms",
            rec.node,
            rec.rate,
            rec.channels,
            pe * 100.0,
            level - first,
            gap * 1000.0
        ));
        if pe > PITCH_TOLERANCE {
            r.failures.push(format!(
                "segment {k}: pilot pitch off by {:.4} % after settling",
                pe * 100.0
            ));
        }
        if !is_flowing(&x, 1, 0..x.len()) {
            r.failures
                .push(format!("segment {k}: no audio after settling"));
        } else if (level - first).abs() > SEGMENT_LEVEL_DB {
            r.failures.push(format!(
                "segment {k}: level {:+.2} dB vs the first stretch (limit +-{SEGMENT_LEVEL_DB} dB)",
                level - first
            ));
        }
        if gap > SEGMENT_MAX_GAP_SECS {
            r.failures.push(format!(
                "segment {k}: {:.1} ms gap after settling",
                gap * 1000.0
            ));
        }
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
        // The graph quantises both measurements to its quantum, so on-minus-off can flip by one
        // quantum between runs (flat@*x6 showed -21.3 and 0.0 ms on the unchanged M1 code): a
        // margin below that would gate on noise. The observed flip is 21.33 ms at 48, 96 and
        // 192 kHz alike (the quantum scales with the rate), so the floor is time-based.
        let quantum_ms =
            (1024.0 * 1000.0 / f64::from(s.graph_rate)).max(1024.0 * 1000.0 / 48_000.0);
        let v = judge_with_margin(added, baselines.get(&s.id).copied(), 0.1, 1.1 * quantum_ms);
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
    let stim = stimulus_for(s);
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
    // A stress run ends back on the primary device; other event runs end on the last one.
    let device_node = match (s.kind, pr.recordings.last()) {
        (Kind::Stress, _) | (_, None) => DEVICE,
        (_, Some(x)) => x.node.as_str(),
    };
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

    if s.kind == Kind::Stress {
        stress_checks(r, s, &stim, &pr);
        if !r.failures.is_empty() {
            for (i, rec) in pr.recordings.iter().enumerate() {
                write_wav(
                    &dir.join(format!("recorded-{i}.wav")),
                    &rec.samples,
                    rec.channels,
                    rec.rate,
                )?;
            }
        }
        // The daemon must still answer: a wedged control thread would fail here.
        get_state().context("daemon unresponsive after the rate stress")?;
    } else if s.kind != Kind::Render {
        tone_checks(r, s, &stim, &pr.recordings[0]);
    } else if !s.events.is_empty() {
        event_checks(r, s, &stim, &pr);
    } else if s.player_rate == s.rate && s.graph_rate == s.rate {
        let (mut chain, notes) = load_exported_chain(&export, state.channels, state.sample_rate)?;
        r.notes.extend(notes);
        let expected = render(&mut chain, &stim.samples, BLOCK_FRAMES);
        exact_checks(r, s, &stim, &pr.recordings[0], &expected, dir)?;
    } else {
        resample_checks(
            r,
            s,
            &stim,
            &pr.recordings[0],
            &export,
            &state,
            BAND_TOLERANCE_DB,
        )?;
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
