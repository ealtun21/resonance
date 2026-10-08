//! Windows backend of the e2e agent: cpal plays on the default endpoint (the
//! Scream virtual device in the e2e VM) and records its WASAPI loopback, with
//! the Resonance APO in audiodg in between. The daemon is control-plane only.

use crate::scenario::Scenario;

pub mod env;
#[cfg(windows)]
pub mod runner;

/// The two playback devices in the e2e VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    /// QEMU HDA: stereo, fixed at 48 kHz.
    Hda,
    /// Scream virtual device: 8 channels, 44.1-192 kHz.
    Scream,
}

/// The endpoint that offers this format, if any.
#[must_use]
pub fn endpoint_for(channels: usize, rate: u32) -> Option<Endpoint> {
    match (channels, rate) {
        (2, 48_000) => Some(Endpoint::Hda),
        // Scream's rate table lacks 176.4 kHz; an unsupported DeviceFormat makes Windows
        // drop the endpoint altogether.
        (8, 44_100 | 48_000 | 88_200 | 96_000 | 192_000) => Some(Endpoint::Scream),
        _ => None,
    }
}

/// Why this scenario cannot run on the Windows endpoints, if it cannot. The
/// shared-mode engine owns the device rate and there is no graph to steer, so
/// only steady matched-rate scenarios on a format one of the VM's devices
/// offers apply.
#[must_use]
pub fn ineligible(s: &Scenario) -> Option<&'static str> {
    if !s.events.is_empty() {
        Some("mid-stream events need a steerable audio graph (Linux only)")
    } else if s.player_rate != s.rate || s.graph_rate != s.rate {
        Some("rate conversion is the Windows audio engine's, not observable per hop")
    } else if endpoint_for(s.channels, s.rate).is_none() {
        Some("no VM playback device offers this format (HDA: 2 ch @ 48 kHz; Scream: 8 ch)")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::{Tier, load_dir, select};

    fn scenarios() -> Vec<Scenario> {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contrib/e2e/scenarios");
        load_dir(&dir).expect("repo scenarios load")
    }

    #[test]
    fn endpoints_follow_the_vm_devices() {
        assert_eq!(endpoint_for(2, 48_000), Some(Endpoint::Hda));
        assert_eq!(endpoint_for(8, 192_000), Some(Endpoint::Scream));
        assert_eq!(endpoint_for(2, 96_000), None, "HDA is fixed at 48 kHz");
        assert_eq!(endpoint_for(16, 48_000), None);
        assert_eq!(endpoint_for(8, 176_400), None, "not in Scream's rate table");
    }

    #[test]
    fn repo_scenarios_split_into_runnable_and_reasoned_skips() {
        let all = scenarios();
        let on_windows = select(&all, Tier::Full, None, "windows");
        let runnable = on_windows
            .iter()
            .filter(|s| ineligible(s).is_none())
            .count();
        assert!(runnable >= 10, "only {runnable} scenarios apply to Windows");
        // Events and rate conversion never apply: they need a steerable graph.
        assert!(
            on_windows
                .iter()
                .filter(|s| !s.events.is_empty())
                .all(|s| ineligible(s).is_some())
        );
        assert!(
            on_windows
                .iter()
                .filter(|s| s.player_rate != s.rate)
                .all(|s| ineligible(s).is_some())
        );
    }
}
