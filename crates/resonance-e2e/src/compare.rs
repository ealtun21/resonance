//! Align a recording to its expected render, then compare every sample.

use resonance_dsp::analysis::best_integer_lag;
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// How a scenario's recording must match its offline render.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(from = "CompareToml")]
pub enum CompareMode {
    /// Every f32 sample equal (`==`).
    #[default]
    Exact,
    /// Largest absolute error at or below `dbfs` (relative to full scale).
    Tolerance { dbfs: f64 },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CompareToml {
    Named(ExactTag),
    Tolerance { tolerance_dbfs: f64 },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExactTag {
    Exact,
}

impl From<CompareToml> for CompareMode {
    fn from(t: CompareToml) -> Self {
        match t {
            CompareToml::Named(ExactTag::Exact) => Self::Exact,
            CompareToml::Tolerance { tolerance_dbfs } => Self::Tolerance {
                dbfs: tolerance_dbfs,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CompareOutcome {
    /// Frames the recording lags the expected render (channel 0).
    pub lag: isize,
    /// First channel whose own best lag differs from channel 0's.
    pub channel_lag_mismatch: Option<(usize, isize)>,
    pub max_abs_err: f32,
    /// First differing sample in the window: `(frame, channel)`.
    pub first_diff: Option<(usize, usize)>,
    /// The aligned window ran outside the recording.
    pub truncated: bool,
    /// The recording is all zeros over the aligned window.
    pub silent: bool,
    /// Frames the end of the window sits off from where the start aligned (negative: the
    /// recording skipped audio; positive: it repeated some), when that is not zero.
    pub slip: Option<isize>,
}

impl CompareOutcome {
    #[must_use]
    pub fn passes(&self, mode: CompareMode) -> bool {
        if self.silent || self.truncated || self.channel_lag_mismatch.is_some() {
            return false;
        }
        match mode {
            CompareMode::Exact => self.first_diff.is_none(),
            CompareMode::Tolerance { dbfs } => {
                f64::from(self.max_abs_err) <= 10f64.powf(dbfs / 20.0)
            }
        }
    }

    /// One-line human summary for reports.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.silent {
            return "recording is silent".into();
        }
        if self.truncated {
            return format!(
                "recording does not cover the stimulus (lag {} frames)",
                self.lag
            );
        }
        if let Some((c, l)) = self.channel_lag_mismatch {
            return format!(
                "channel {c} is offset by {l} frames, channel 0 by {}",
                self.lag
            );
        }
        match self.first_diff {
            None => format!("bit-exact (lag {} frames)", self.lag),
            Some((f, _)) if self.slip.is_some() => {
                let n = self.slip.unwrap_or_default();
                format!(
                    "slip of {} frames ({}) at frame {f}: the end of the recording is offset by {n:+} frames from its start; max error {:.1} dBFS",
                    n.abs(),
                    if n < 0 { "skipped" } else { "repeated" },
                    20.0 * f64::from(self.max_abs_err).max(1e-30).log10()
                )
            }
            Some((f, c)) => {
                let db = 20.0 * f64::from(self.max_abs_err).max(1e-30).log10();
                format!("first difference at frame {f} channel {c}; max error {db:.1} dBFS")
            }
        }
    }
}

fn channel(x: &[f32], channels: usize, c: usize) -> Vec<f64> {
    x.iter()
        .skip(c)
        .step_by(channels)
        .map(|&v| f64::from(v))
        .collect()
}

/// Find the recording's lag against `expected` on every channel (all must
/// agree), then compare `expected[window]` with the recording shifted by it.
#[must_use]
#[allow(clippy::float_cmp)] // bit-exactness is the point of this function
pub fn compare(
    expected: &[f32],
    recorded: &[f32],
    channels: usize,
    window: Range<usize>,
    max_lag: usize,
) -> CompareOutcome {
    let lag = best_integer_lag(
        &channel(expected, channels, 0),
        &channel(recorded, channels, 0),
        max_lag,
    );
    let channel_lag_mismatch = (1..channels).find_map(|c| {
        let l = best_integer_lag(
            &channel(expected, channels, c),
            &channel(recorded, channels, c),
            max_lag,
        );
        (l != lag).then_some((c, l))
    });
    let rec_frames = recorded.len() / channels;
    let window_end = window.end;
    let (mut max_abs_err, mut first_diff, mut truncated, mut silent) = (0.0f32, None, false, true);
    for f in window {
        let Some(rf) = f.checked_add_signed(lag).filter(|&rf| rf < rec_frames) else {
            truncated = true;
            break;
        };
        for c in 0..channels {
            let (e, r) = (expected[f * channels + c], recorded[rf * channels + c]);
            silent &= r == 0.0;
            if e != r {
                max_abs_err = max_abs_err.max((e - r).abs());
                first_diff.get_or_insert((f, c));
            }
        }
    }
    let slip = first_diff.filter(|_| !truncated && !silent).and_then(|_| {
        tail_slip(
            &channel(expected, channels, 0),
            &channel(recorded, channels, 0),
            window_end,
            lag,
        )
    });
    CompareOutcome {
        lag,
        channel_lag_mismatch,
        max_abs_err,
        first_diff,
        truncated,
        silent,
        slip,
    }
}

/// Re-align the last `TAIL` frames before `end` on their own: the extra lag (beyond `lag`) of
/// that stretch, if any. A one-time lost or repeated run of frames shows up as a constant offset.
fn tail_slip(exp: &[f64], rec: &[f64], end: usize, lag: isize) -> Option<isize> {
    const TAIL: usize = 16384;
    const SEARCH: usize = 4096;
    let start = end.checked_sub(TAIL)?;
    let a = exp.get(start..end)?;
    let b0 = start.checked_add_signed(lag)?.checked_sub(SEARCH)?;
    let b = rec.get(b0..b0 + TAIL + 2 * SEARCH)?;
    let extra = best_integer_lag(a, b, 2 * SEARCH) - SEARCH as isize;
    (extra != 0).then_some(extra)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(frames: usize, ch: usize) -> Vec<f32> {
        let mut s = 0x1234_5678_9ABC_DEF1u64;
        (0..frames * ch)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                ((s >> 40) as f32 / (1u32 << 24) as f32) - 0.5
            })
            .collect()
    }

    fn delayed(x: &[f32], ch: usize, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * ch];
        out.extend_from_slice(x);
        out
    }

    fn padded(x: &[f32], ch: usize) -> Vec<f32> {
        // Expected render: 200 frames of silence, then the signal.
        delayed(x, ch, 200)
    }

    #[test]
    fn identical_after_delay_passes_exact() {
        let exp = padded(&noise(4000, 2), 2);
        let rec = delayed(&exp, 2, 37);
        let o = compare(&exp, &rec, 2, 200..4200, 512);
        assert_eq!(o.lag, 37);
        assert!(o.passes(CompareMode::Exact), "{}", o.describe());
    }

    #[test]
    fn one_flipped_lsb_fails_exact_and_reports_where() {
        let exp = padded(&noise(4000, 2), 2);
        let mut rec = delayed(&exp, 2, 37);
        let i = (37 + 1234) * 2 + 1;
        rec[i] = f32::from_bits(rec[i].to_bits() ^ 1);
        let o = compare(&exp, &rec, 2, 200..4200, 512);
        assert_eq!(o.first_diff, Some((1234, 1)));
        assert!(!o.passes(CompareMode::Exact));
        assert!(o.passes(CompareMode::Tolerance { dbfs: -120.0 }));
    }

    #[test]
    fn swapped_channels_fail() {
        let exp = padded(&noise(4000, 2), 2);
        let rec: Vec<f32> = exp.chunks(2).flat_map(|f| [f[1], f[0]]).collect();
        assert!(!compare(&exp, &rec, 2, 200..4200, 512).passes(CompareMode::Exact));
    }

    #[test]
    fn per_channel_slip_is_reported() {
        let exp = padded(&noise(4000, 2), 2);
        let mut rec = delayed(&exp, 2, 37);
        // Channel 1 arrives one frame later than channel 0.
        let frames = rec.len() / 2;
        for f in (1..frames).rev() {
            rec[f * 2 + 1] = rec[(f - 1) * 2 + 1];
        }
        rec[1] = 0.0;
        let o = compare(&exp, &rec, 2, 200..4200, 512);
        assert_eq!(o.channel_lag_mismatch, Some((1, 38)));
        assert!(!o.passes(CompareMode::Exact));
    }

    #[test]
    fn dropped_frame_fails() {
        let exp = padded(&noise(4000, 2), 2);
        let mut rec = delayed(&exp, 2, 37);
        rec.drain((37 + 2000) * 2..(37 + 2001) * 2);
        assert!(!compare(&exp, &rec, 2, 200..4200, 512).passes(CompareMode::Exact));
    }

    #[test]
    fn skipped_run_is_reported_as_a_slip() {
        let exp = padded(&noise(40000, 2), 2);
        let mut rec = delayed(&exp, 2, 37);
        rec.drain((37 + 20000) * 2..(37 + 20768) * 2);
        let o = compare(&exp, &rec, 2, 200..35000, 512);
        assert_eq!(o.slip, Some(-768));
        assert_eq!(o.first_diff.map(|d| d.0), Some(20000));
        assert!(
            o.describe()
                .contains("slip of 768 frames (skipped) at frame 20000")
        );
    }

    #[test]
    fn recording_ending_early_is_truncated() {
        let exp = padded(&noise(4000, 2), 2);
        let mut rec = delayed(&exp, 2, 37);
        rec.truncate((37 + 3000) * 2);
        let o = compare(&exp, &rec, 2, 200..4200, 512);
        assert!(o.truncated && !o.passes(CompareMode::Exact));
    }

    // Review Focus 3: capture linked after the body began.
    #[test]
    fn capture_started_after_the_body_began_is_truncated() {
        let exp = padded(&noise(4000, 2), 2);
        let rec = exp[(200 + 300) * 2..].to_vec(); // first 300 body frames never recorded
        let o = compare(&exp, &rec, 2, 200..4200, 512);
        assert!(
            o.truncated && !o.passes(CompareMode::Exact),
            "{}",
            o.describe()
        );
    }

    // Review Focus 2: a silent recording must fail clearly.
    #[test]
    fn silent_recording_fails_and_says_so() {
        let exp = padded(&noise(4000, 2), 2);
        let rec = vec![0.0f32; exp.len() + 200];
        let o = compare(&exp, &rec, 2, 200..4200, 512);
        assert!(o.silent && !o.passes(CompareMode::Tolerance { dbfs: 0.0 }));
        assert!(o.describe().contains("silent"));
    }

    #[test]
    fn compare_mode_parses_from_scenario_toml() {
        #[derive(serde::Deserialize)]
        struct W {
            compare: CompareMode,
        }
        let e: W = toml::from_str(r#"compare = "exact""#).unwrap();
        assert_eq!(e.compare, CompareMode::Exact);
        let t: W = toml::from_str("compare = { tolerance_dbfs = -120 }").unwrap();
        assert_eq!(t.compare, CompareMode::Tolerance { dbfs: -120.0 });
    }
}
