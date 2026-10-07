//! The sample rate at every step of the audio path, and which steps resample.

use crate::scenario::AllowedHop;
use serde::Serialize;

/// Linux steps where Resonance itself (not PipeWire) would resample.
pub const LINUX_RESONANCE_STEPS: &[&str] = &["capture->dsp"];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hop {
    pub name: String,
    pub rate: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RateChain {
    pub hops: Vec<Hop>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum By {
    Os,
    Resonance,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Resampling {
    /// `"<from hop>-><to hop>"`.
    pub step: String,
    pub from_rate: u32,
    pub to_rate: u32,
    pub by: By,
    /// The scenario's declared reason; `None` = undeclared (a failure).
    pub reason: Option<String>,
}

impl RateChain {
    /// Every step whose rate changes, attributed, and matched against the
    /// scenario's declared resampling.
    #[must_use]
    pub fn resamplings(&self, allowed: &[AllowedHop], resonance_steps: &[&str]) -> Vec<Resampling> {
        self.hops
            .windows(2)
            .filter(|w| w[0].rate != w[1].rate)
            .map(|w| {
                let step = format!("{}->{}", w[0].name, w[1].name);
                Resampling {
                    from_rate: w[0].rate,
                    to_rate: w[1].rate,
                    by: if resonance_steps.contains(&step.as_str()) {
                        By::Resonance
                    } else {
                        By::Os
                    },
                    reason: allowed
                        .iter()
                        .find(|a| a.hop == step)
                        .map(|a| a.reason.clone()),
                    step,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(rates: [u32; 5]) -> RateChain {
        let names = ["player", "graph", "capture", "dsp", "device"];
        RateChain {
            hops: names
                .iter()
                .zip(rates)
                .map(|(n, r)| Hop {
                    name: (*n).into(),
                    rate: r,
                })
                .collect(),
        }
    }

    #[test]
    fn matched_rates_never_resample() {
        assert!(
            chain([48_000; 5])
                .resamplings(&[], LINUX_RESONANCE_STEPS)
                .is_empty()
        );
    }

    #[test]
    fn declared_os_step_carries_its_reason() {
        let allowed = [AllowedHop {
            hop: "player->graph".into(),
            reason: "48 k content into 16 k device".into(),
        }];
        let r = chain([48_000, 16_000, 16_000, 16_000, 16_000])
            .resamplings(&allowed, LINUX_RESONANCE_STEPS);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].step, "player->graph");
        assert_eq!(
            (r[0].from_rate, r[0].to_rate, r[0].by),
            (48_000, 16_000, By::Os)
        );
        assert_eq!(
            r[0].reason.as_deref(),
            Some("48 k content into 16 k device")
        );
    }

    #[test]
    fn undeclared_resonance_step_has_no_reason() {
        let r =
            chain([48_000, 48_000, 44_100, 48_000, 48_000]).resamplings(&[], LINUX_RESONANCE_STEPS);
        let cap = r.iter().find(|x| x.step == "capture->dsp").unwrap();
        assert_eq!(cap.by, By::Resonance);
        assert!(cap.reason.is_none());
    }
}
