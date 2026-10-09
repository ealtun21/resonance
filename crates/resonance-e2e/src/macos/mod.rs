//! macOS backend of the e2e agent. The scenario's audio is played into a
//! BlackHole device that is the system default output, which the daemon's
//! device-bound Process Tap captures at that device's channel count and rate;
//! the daemon renders its result into a *second* BlackHole device (a preferred
//! output), whose input the agent records. A different device for the result
//! keeps the unprocessed audio out of the recording.

use crate::scenario::{Kind, Scenario};

pub mod env;
#[cfg(any(target_os = "macos", feature = "cross-check"))]
pub mod runner;

/// The device the daemon renders into (and the agent records): one step wider than the
/// tapped device, so the unprocessed audio never shares a device with the result.
#[must_use]
pub fn out_device(channels: usize) -> Option<&'static str> {
    match channels {
        2 => Some("BlackHole 16ch"),
        16 => Some("BlackHole 64ch"),
        _ => None,
    }
}

/// The device the scenario's audio is played into (and tapped from): the tap
/// takes the device's whole channel layout, so the scenario's channel count
/// must equal the device's.
#[must_use]
pub fn in_device(channels: usize) -> Option<&'static str> {
    match channels {
        2 => Some("BlackHole 2ch"),
        16 => Some("BlackHole 16ch"),
        _ => None,
    }
}

/// Why this scenario cannot run here, if it cannot.
#[must_use]
pub fn ineligible(s: &Scenario) -> Option<&'static str> {
    if s.kind != Kind::Render {
        Some(
            "steady-tone soak / THD+N need a gap-free recording, and this VM's audio threads drop whole 512-frame buffers (MAC-E2, measured: 11 in 8 s, THD+N -18 dB); the tap's resampler also sets a floor near -60 dB (MAC-E1)",
        )
    } else if !s.events.is_empty() {
        Some("mid-stream events need a steerable audio graph (Linux only)")
    } else if s.player_rate != s.rate || s.graph_rate != s.rate {
        Some("rate conversion is CoreAudio's, not observable per hop")
    } else if in_device(s.channels).is_none() {
        Some("the tap takes the device's whole layout: only BlackHole 2ch / 16ch exist")
    } else if !(44_100..=192_000).contains(&s.rate) {
        Some("rate outside the BlackHole devices' range")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::{Tier, load_dir, select};

    #[test]
    fn channel_counts_map_to_the_tapped_device() {
        assert_eq!(in_device(2), Some("BlackHole 2ch"));
        assert_eq!(in_device(16), Some("BlackHole 16ch"));
        assert_eq!(in_device(8), None);
    }

    #[test]
    fn repo_scenarios_have_a_mac_subset() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contrib/e2e/scenarios");
        let all = load_dir(&dir).unwrap();
        let on_mac = select(&all, Tier::Full, None, "macos");
        let runnable = on_mac.iter().filter(|s| ineligible(s).is_none()).count();
        assert!(runnable >= 10, "only {runnable} scenarios apply to macOS");
        assert!(
            on_mac
                .iter()
                .filter(|s| !s.events.is_empty())
                .all(|s| ineligible(s).is_some())
        );
    }
}
