//! Per-scenario results, final status, and the human-readable report.

use crate::compare::CompareOutcome;
use crate::latency::LatencyVerdict;
use crate::ratechain::{By, RateChain, Resampling};
use serde::Serialize;
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Fail,
    ExpectedFail,
    UnexpectedPass,
    Flake,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "FAIL",
            Self::ExpectedFail => "expected-fail",
            Self::UnexpectedPass => "UNEXPECTED PASS",
            Self::Flake => "flake",
        }
    }
}

#[must_use]
pub fn settle(failures_empty: bool, expected_fail: bool) -> Status {
    match (failures_empty, expected_fail) {
        (true, false) => Status::Pass,
        (false, false) => Status::Fail,
        (false, true) => Status::ExpectedFail,
        (true, true) => Status::UnexpectedPass,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioResult {
    pub id: String,
    pub os: String,
    pub status: Status,
    pub failures: Vec<String>,
    pub expected_fail: Option<String>,
    pub compare: Option<CompareOutcome>,
    pub latency_on_ms: Option<f64>,
    pub latency_off_ms: Option<f64>,
    pub added_latency_ms: Option<f64>,
    pub latency_verdict: Option<LatencyVerdict>,
    pub rate_chain: Option<RateChain>,
    pub resamplings: Vec<Resampling>,
    pub discontinuities: u32,
    pub notes: Vec<String>,
    pub artifacts: Option<String>,
}

impl ScenarioResult {
    #[must_use]
    pub fn new(id: &str, os: &str) -> Self {
        Self {
            id: id.into(),
            os: os.into(),
            status: Status::Pass,
            failures: Vec::new(),
            expected_fail: None,
            compare: None,
            latency_on_ms: None,
            latency_off_ms: None,
            added_latency_ms: None,
            latency_verdict: None,
            rate_chain: None,
            resamplings: Vec::new(),
            discontinuities: 0,
            notes: Vec::new(),
            artifacts: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub os: String,
    pub tier: String,
    pub results: Vec<ScenarioResult>,
}

impl Report {
    #[must_use]
    pub fn failed(&self) -> bool {
        self.results
            .iter()
            .any(|r| matches!(r.status, Status::Fail | Status::UnexpectedPass))
    }

    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut md = format!("# Resonance e2e — {} ({})\n\n", self.os, self.tier);
        let count = |s: Status| self.results.iter().filter(|r| r.status == s).count();
        let _ = writeln!(
            md,
            "{} pass · {} fail · {} expected-fail · {} unexpected-pass · {} flake\n",
            count(Status::Pass),
            count(Status::Fail),
            count(Status::ExpectedFail),
            count(Status::UnexpectedPass),
            count(Status::Flake)
        );
        md.push_str("| Scenario | Status | Added latency | Resampling |\n|---|---|---|---|\n");
        for r in &self.results {
            let lat = r
                .added_latency_ms
                .map_or_else(|| "—".into(), |ms| format!("{ms:.2} ms"));
            let rs = if r.resamplings.is_empty() {
                "none".to_string()
            } else {
                r.resamplings
                    .iter()
                    .map(|x| x.step.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let _ = writeln!(md, "| {} | {} | {lat} | {rs} |", r.id, r.status.label());
        }
        let failing: Vec<_> = self
            .results
            .iter()
            .filter(|r| !r.failures.is_empty())
            .collect();
        if !failing.is_empty() {
            md.push_str("\n## Failures\n\n");
            for r in failing {
                let tag = r
                    .expected_fail
                    .as_deref()
                    .map_or_else(String::new, |f| format!(" (expected: {f})"));
                let _ = writeln!(md, "### {}{tag}\n", r.id);
                for f in &r.failures {
                    let _ = writeln!(md, "- {f}");
                }
                if let Some(a) = &r.artifacts {
                    let _ = writeln!(md, "- artifacts: `{a}`");
                }
                md.push('\n');
            }
        }
        md.push_str("\n## Resampling\n\n");
        let mut any = false;
        for r in &self.results {
            for x in &r.resamplings {
                any = true;
                let by = match x.by {
                    By::Os => "OS",
                    By::Resonance => "Resonance",
                };
                let why = x
                    .reason
                    .as_deref()
                    .map_or_else(|| "**undeclared**".into(), str::to_owned);
                let _ = writeln!(
                    md,
                    "- `{}`: {} {} → {} Hz by {by}: {why}",
                    r.id, x.step, x.from_rate, x.to_rate
                );
            }
        }
        if !any {
            md.push_str("No step resampled in any scenario.\n");
        }
        md
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ratechain::{By, Resampling};

    fn result(id: &str, status: Status) -> ScenarioResult {
        ScenarioResult {
            status,
            ..ScenarioResult::new(id, "linux")
        }
    }

    #[test]
    fn settle_covers_expected_fail_both_ways() {
        assert_eq!(settle(true, false), Status::Pass);
        assert_eq!(settle(false, false), Status::Fail);
        assert_eq!(settle(false, true), Status::ExpectedFail);
        assert_eq!(settle(true, true), Status::UnexpectedPass);
    }

    #[test]
    fn only_fail_and_unexpected_pass_fail_the_run() {
        for (s, failed) in [
            (Status::Pass, false),
            (Status::ExpectedFail, false),
            (Status::Flake, false),
            (Status::Fail, true),
            (Status::UnexpectedPass, true),
        ] {
            let r = Report {
                os: "linux".into(),
                tier: "quick".into(),
                results: vec![result("x", s)],
            };
            assert_eq!(r.failed(), failed, "{s:?}");
        }
    }

    #[test]
    fn markdown_has_grid_failures_and_resampling_section() {
        let mut bad = result("eq@48000x2", Status::Fail);
        bad.failures
            .push("first difference at frame 10 channel 1".into());
        let mut rs = result("sco@16000x1", Status::Pass);
        rs.resamplings.push(Resampling {
            step: "player->graph".into(),
            from_rate: 48_000,
            to_rate: 16_000,
            by: By::Os,
            reason: Some("48 k content into a 16 k headset".into()),
        });
        rs.resamplings.push(Resampling {
            step: "capture->dsp".into(),
            from_rate: 16_000,
            to_rate: 48_000,
            by: By::Resonance,
            reason: None,
        });
        let md = Report {
            os: "linux".into(),
            tier: "quick".into(),
            results: vec![bad, rs],
        }
        .to_markdown();
        assert!(md.contains("| eq@48000x2 | FAIL |"), "{md}");
        assert!(md.contains("first difference at frame 10 channel 1"));
        assert!(md.contains("## Resampling"));
        assert!(md.contains("48 k content into a 16 k headset"));
        assert!(md.contains("**undeclared**"));
    }
}
