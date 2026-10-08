//! Latency: chirp arrival lags, committed per-scenario baselines, verdict.

use anyhow::{Context, Result};
use resonance_dsp::analysis::best_integer_lag;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

#[must_use]
pub fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let m = v.len() / 2;
    Some(if v.len() % 2 == 1 {
        v[m]
    } else {
        f64::midpoint(v[m - 1], v[m])
    })
}

/// Arrival lag in frames of each chirp: `template` (the raw chirp) against
/// the recording window starting at each emission frame.
#[must_use]
pub fn arrival_lags(
    recorded_ch0: &[f64],
    template: &[f64],
    emit: &[usize],
    max_lag: usize,
) -> Vec<isize> {
    emit.iter()
        .map(|&e| {
            let end = (e + template.len() + max_lag).min(recorded_ch0.len());
            let seg = recorded_ch0.get(e..end).unwrap_or(&[]);
            best_integer_lag(template, seg, max_lag)
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum LatencyVerdict {
    Ok,
    Regressed { baseline_ms: f64 },
    CanLower { baseline_ms: f64 },
    NoBaseline,
}

/// Fail above baseline + max(1 ms, 10 %); report "can lower" below the same margin.
#[must_use]
pub fn judge(measured_ms: f64, baseline_ms: Option<f64>) -> LatencyVerdict {
    judge_with_margin(measured_ms, baseline_ms, 0.1, 1.0)
}

/// [`judge`] with a margin of `max(min_ms, rel * baseline)`, for paths whose latency jitters
/// more (a ring buffer whose fill depends on callback phase).
#[must_use]
pub fn judge_with_margin(
    measured_ms: f64,
    baseline_ms: Option<f64>,
    rel: f64,
    min_ms: f64,
) -> LatencyVerdict {
    let Some(b) = baseline_ms else {
        return LatencyVerdict::NoBaseline;
    };
    let margin = (rel * b.abs()).max(min_ms);
    if measured_ms > b + margin {
        LatencyVerdict::Regressed { baseline_ms: b }
    } else if measured_ms < b - margin {
        LatencyVerdict::CanLower { baseline_ms: b }
    } else {
        LatencyVerdict::Ok
    }
}

/// The failure message a verdict produces, if any.
#[must_use]
pub fn failure(verdict: LatencyVerdict, measured_ms: f64, update_baseline: bool) -> Option<String> {
    match verdict {
        LatencyVerdict::Regressed { baseline_ms } if !update_baseline => Some(format!(
            "added latency {measured_ms:.2} ms exceeds baseline {baseline_ms:.2} ms + margin"
        )),
        LatencyVerdict::NoBaseline if !update_baseline => Some(format!(
            "no latency baseline (measured {measured_ms:.2} ms); run with --update-baseline"
        )),
        _ => None,
    }
}

/// Scenario id → added latency (ms). A missing file is an empty map.
pub fn load_baselines(path: &Path) -> Result<BTreeMap<String, f64>> {
    match std::fs::read_to_string(path) {
        Ok(t) => toml::from_str(&t).with_context(|| format!("parse {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

pub fn save_baselines(path: &Path, m: &BTreeMap<String, f64>) -> Result<()> {
    let body = format!(
        "# Added latency (ms) per scenario. Rewritten by `cargo xtask e2e --update-baseline`.\n{}",
        toml::to_string(m)?
    );
    std::fs::write(path, body).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stimulus::chirp_train;

    #[test]
    fn median_handles_odd_even_and_empty() {
        assert_eq!(median(vec![3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(vec![4.0, 1.0, 3.0, 2.0]), Some(2.5));
        assert_eq!(median(vec![]), None);
    }

    #[test]
    fn arrival_lags_find_each_delayed_chirp() {
        let t = chirp_train(48_000, 1);
        let delay = 1234usize;
        let mut rec = vec![0.0f64; delay];
        rec.extend(t.samples.iter().map(|&s| f64::from(s)));
        let template: Vec<f64> = t.samples[t.emit[0]..t.emit[0] + t.chirp_frames]
            .iter()
            .map(|&s| f64::from(s))
            .collect();
        let lags = arrival_lags(&rec, &template, &t.emit, 20_000);
        assert_eq!(lags, vec![1234; 5]);
    }

    #[test]
    fn judge_uses_1ms_floor_and_10_percent_margin() {
        assert_eq!(judge(5.9, Some(5.0)), LatencyVerdict::Ok);
        assert_eq!(
            judge(6.1, Some(5.0)),
            LatencyVerdict::Regressed { baseline_ms: 5.0 }
        );
        assert_eq!(
            judge(3.9, Some(5.0)),
            LatencyVerdict::CanLower { baseline_ms: 5.0 }
        );
        assert_eq!(judge(188.0, Some(171.0)), LatencyVerdict::Ok); // 10 % of 171 = 17.1
        assert_eq!(
            judge(189.0, Some(171.0)),
            LatencyVerdict::Regressed { baseline_ms: 171.0 }
        );
    }

    // Review Focus 5: a missing baseline is a failure unless updating.
    #[test]
    fn missing_baseline_fails_unless_updating() {
        let msg = failure(judge(4.0, None), 4.0, false).expect("must fail");
        assert!(msg.contains("--update-baseline"), "{msg}");
        assert!(failure(judge(4.0, None), 4.0, true).is_none());
        assert!(failure(LatencyVerdict::CanLower { baseline_ms: 9.0 }, 4.0, false).is_none());
    }

    #[test]
    fn baselines_round_trip_and_missing_file_is_empty() {
        let p = std::env::temp_dir().join(format!("resonance-e2e-bl-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&p);
        assert!(load_baselines(&p).unwrap().is_empty());
        let m = BTreeMap::from([("flat@48000x2".to_string(), 1.25)]);
        save_baselines(&p, &m).unwrap();
        assert_eq!(load_baselines(&p).unwrap(), m);
    }
}
