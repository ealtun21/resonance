//! Platform-dispatching audio backend.
//!
//! The daemon embeds a real-time audio node that pulls samples from a system
//! audio source, runs them through the DSP `ProcessorChain`, writes them back to
//! a system audio sink, and side-effects:
//!   - publishes raw post-DSP samples into a spectrum ring buffer
//!   - reports the currently fed output device's name on change
//!   - reports the live list of available sinks (with friendly descriptions)
//!   - applies IPC `AudioCommand`s to mutate the chain on the RT thread
//!   - accepts a preferred-output route hint from IPC
//!
//! Each platform implements the same `spawn` signature, defined below as the
//! single public entry point. `main.rs` calls `audio::spawn(...)`; the right
//! backend is selected at compile time.

use crate::state::AudioCommand;
use resonance_dsp::chain::ProcessorChain;
// Only the `BackendCtx`/`spawn` path (absent on Windows — the APO owns the DSP)
// needs these; `apply_command` and the consts below don't.
#[cfg(not(target_os = "windows"))]
use crate::meters::AtomicMeters;
#[cfg(not(target_os = "windows"))]
use anyhow::Result;
#[cfg(not(target_os = "windows"))]
use std::{sync::Arc, thread::JoinHandle};

/// Default channel count when nothing overrides it (stereo).
pub const CHANNELS: usize = 2;
/// Target sample rate for the DSP chain. Backends negotiate this with the
/// system and may fall back to whatever the device offers.
#[allow(dead_code)] // pipewire backend + stub use it; macOS reads device rate live
pub const SAMPLE_RATE: u32 = 48_000;

/// The channel count Resonance processes. Defaults to [`CHANNELS`] (stereo) and
/// can be overridden with `RESONANCE_CHANNELS` (1..=64) — the manual override for
/// surround devices until automatic device-layout following lands.
///
/// This is the **single source of truth** for buffer width: the backend sizes
/// its ports/scratch to it *and* the daemon builds its shadow chain at the same
/// count, so a `ReplaceChain` from the daemon can never hand the RT thread a
/// chain whose channel count disagrees with the live ports.
#[allow(dead_code)] // unused on Windows (APO owns the DSP + its own width)
pub fn target_channels() -> usize {
    std::env::var("RESONANCE_CHANNELS")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|&n| (1..=resonance_dsp::channel::MAX_CHANNELS).contains(&n))
        .unwrap_or(CHANNELS)
}
/// Capacity of the spectrum SPSC ring buffer (mono `f32` samples).
pub const SPECTRUM_BUF: usize = 8192;

// ── Platform backends ────────────────────────────────────────────────────────

/// Pure per-application stream helpers (key/identity), shared by all backends.
mod app_streams;

#[cfg(target_os = "linux")]
mod pipewire;
#[cfg(target_os = "linux")]
use pipewire as backend;

#[cfg(target_os = "macos")]
mod coreaudio;
#[cfg(target_os = "macos")]
mod hal_input;
#[cfg(target_os = "macos")]
pub(crate) mod mac_apps;
#[cfg(target_os = "macos")]
pub(crate) mod mac_sinks;
#[cfg(target_os = "macos")]
mod system_tap;
#[cfg(target_os = "macos")]
use coreaudio as backend;

// Windows has NO daemon-side audio backend: the in-engine APO (`resonance-apo`,
// loaded into audiodg.exe) owns the DSP, and the daemon is the control plane
// (see `main.rs`). Only the MMDevice helpers + the loopback *measurement*
// diagnostic live here.
#[cfg(target_os = "windows")]
pub(crate) mod win_apps;
#[cfg(target_os = "windows")]
pub(crate) mod win_devices;
#[cfg(target_os = "windows")]
mod win_measure;
#[cfg(target_os = "windows")]
pub(crate) mod win_sinks;
#[cfg(target_os = "windows")]
pub use win_measure::measure_loopback;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod stub;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
use stub as backend;

/// Everything a backend's [`spawn`] needs: the IPC↔RT channels, the initial
/// chain, and the shared meter handle. Bundled into one named type so the
/// backend contract is a single edit point instead of seven positional
/// arguments duplicated across every platform's `spawn`.
///
/// Not defined on Windows: there's no daemon-side backend there (the APO owns
/// the DSP), so nothing constructs or spawns it.
#[cfg(not(target_os = "windows"))]
pub struct BackendCtx {
    /// IPC → RT: chain-mutating commands.
    pub cmd_rx: rtrb::Consumer<AudioCommand>,
    /// RT → IPC: post-DSP mono samples for the spectrum view.
    pub spectrum_tx: rtrb::Producer<f32>,
    /// The chain the backend starts with.
    pub initial_chain: ProcessorChain,
    /// RT → IPC: the fed output device's name, on change.
    pub output_tx: tokio::sync::mpsc::UnboundedSender<String>,
    /// IPC → RT: a preferred-output route hint (empty string = follow default).
    pub route_rx: std::sync::mpsc::Receiver<String>,
    /// RT → IPC: the live list of available sinks (node name, description).
    pub sinks_tx: tokio::sync::mpsc::UnboundedSender<Vec<(String, String)>>,
    /// Shared peak/RMS/clip meters published from the RT thread.
    pub meters: Arc<AtomicMeters>,
    /// Backend → IPC: the live per-application stream list (control-plane).
    pub apps_tx: tokio::sync::mpsc::UnboundedSender<Vec<resonance_ipc::AppStream>>,
    /// IPC → backend: per-application volume/mute requests (control-plane, not
    /// the RT path). Drained on the backend's main-loop/control thread.
    pub app_ctl_rx: std::sync::mpsc::Receiver<crate::state::AppControl>,
    /// Backend → IPC: the live output-sink volume list (control-plane).
    pub sinks_vol_tx: tokio::sync::mpsc::UnboundedSender<Vec<resonance_ipc::SinkVolume>>,
    /// IPC → backend: per-output-sink volume/mute requests (control-plane).
    pub sink_ctl_rx: std::sync::mpsc::Receiver<crate::state::SinkCtl>,
}

/// Spawn the audio backend on its own dedicated real-time thread.
///
/// Returns a `JoinHandle` so the daemon's `main` can wait on shutdown. Not
/// present on Windows (the APO owns the DSP; `main.rs` skips it there).
#[cfg(not(target_os = "windows"))]
pub fn spawn(ctx: BackendCtx) -> Result<JoinHandle<()>> {
    backend::spawn(ctx)
}

// ── Shared RT helpers ────────────────────────────────────────────────────────

/// Apply a `AudioCommand` to the live `ProcessorChain` from the RT thread.
/// Shared by every backend so the command-dispatch logic stays in one place.
pub fn apply_command(chain: &mut ProcessorChain, cmd: AudioCommand) {
    use resonance_dsp::filter::ApoFilter;
    match cmd {
        AudioCommand::SetPower(on) => chain.enabled = on,
        AudioCommand::SetPreamp(db) => chain.preamp_db = db,
        AudioCommand::SetEffectIntensity { effect, value } => {
            chain.set_effect_intensity(effect, value);
        }
        AudioCommand::SetEffectEnabled { effect, on } => chain.set_effect_enabled(effect, on),
        AudioCommand::SetDither { bits } => chain.set_dither(bits),
        AudioCommand::ReplaceChain(c) => {
            // The incoming chain carries the shadow/preset construction rate
            // (often 48 kHz), but the RT thread may be running at the live
            // device rate (e.g. 44.1/96 kHz, set by the backend at stream
            // setup). Adopt the new bands and effects, then re-bind to the live
            // rate so the EQ coefficients stay correct and `status` reports the
            // true rate — the sample-rate analog of the channel-count guarantee
            // the daemon makes for ReplaceChain. `rebind_sample_rate` no-ops
            // when the rates already match.
            let live_rate = chain.sample_rate;
            *chain = *c;
            chain.rebind_sample_rate(live_rate);
        }
        AudioCommand::Reset => chain.reset(),
        AudioCommand::SetBand {
            index,
            freq,
            gain_db,
            q,
        } => {
            let sr = chain.sample_rate;
            if let Some(f) = chain.filters.get_mut(index) {
                let _ = f.update(f.filter_type, freq, gain_db, q, sr);
            }
        }
        AudioCommand::SetBandEnabled { index, enabled } => {
            if let Some(f) = chain.filters.get_mut(index) {
                f.enabled = enabled;
            }
        }
        AudioCommand::AddBand {
            band_type,
            freq,
            gain_db,
            q,
        } => {
            if let Ok(f) = ApoFilter::builder()
                .filter_type(band_type.into())
                .freq(freq)
                .gain_db(gain_db)
                .q(q)
                .enabled(true)
                .channels(chain.channels)
                .sample_rate(chain.sample_rate)
                .build()
            {
                chain.filters.push(f);
            }
        }
        AudioCommand::RemoveBand { index } => {
            if index < chain.filters.len() {
                chain.filters.remove(index);
            }
        }
        AudioCommand::SetBandType { index, band_type } => {
            let sr = chain.sample_rate;
            if let Some(f) = chain.filters.get_mut(index) {
                let _ = f.update(band_type.into(), f.freq, f.gain_db, f.q, sr);
            }
        }
        AudioCommand::SetBandSlope {
            index,
            slope_db_oct,
        } => {
            let sr = chain.sample_rate;
            if let Some(f) = chain.filters.get_mut(index) {
                let _ = f.set_slope(slope_db_oct, sr);
            }
        }
        AudioCommand::SetBandScope { index, scope } => {
            if let Some(f) = chain.filters.get_mut(index) {
                f.scope = scope.into();
            }
        }
        AudioCommand::SetBandDynamics { index, dynamics } => {
            let sr = chain.sample_rate;
            if let Some(f) = chain.filters.get_mut(index) {
                let _ = f.set_dynamics(dynamics.map(Into::into), sr);
            }
        }
        AudioCommand::SetBandChannels { index, mask } => {
            if let Some(f) = chain.filters.get_mut(index) {
                f.mask = mask;
            }
        }
        AudioCommand::SetRouting { matrix } => {
            chain.routing = matrix;
        }
        AudioCommand::SetConvolution(engine) => {
            chain.convolution = *engine;
            // The engine was prepared at the rate/width the IPC thread believed
            // was live; force-match the RT chain's actual format (no-ops when
            // they already agree — the common case).
            chain.convolution.rebind_sample_rate(chain.sample_rate);
            chain.convolution.set_channels(chain.channels);
        }
        AudioCommand::SetConvolutionEnabled(on) => chain.convolution.set_enabled(on),
        AudioCommand::ClearConvolution => chain.convolution.clear(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use resonance_dsp::filter::FilterType;
    use resonance_ipc::BandType;

    fn chain() -> ProcessorChain {
        ProcessorChain::builder()
            .channels(CHANNELS)
            .sample_rate(f64::from(SAMPLE_RATE))
            .build()
    }

    #[test]
    fn add_band_uses_requested_type() {
        let mut c = chain();
        apply_command(
            &mut c,
            AudioCommand::AddBand {
                band_type: BandType::HighShelf,
                freq: 8000.0,
                gain_db: 4.0,
                q: 0.7,
            },
        );
        assert_eq!(c.filters.len(), 1);
        assert_eq!(c.filters[0].filter_type, FilterType::HighShelf);
    }

    #[test]
    fn set_band_type_preserves_freq_gain_q() {
        let mut c = chain();
        apply_command(
            &mut c,
            AudioCommand::AddBand {
                band_type: BandType::Peaking,
                freq: 1000.0,
                gain_db: 6.0,
                q: 2.0,
            },
        );
        apply_command(
            &mut c,
            AudioCommand::SetBandType {
                index: 0,
                band_type: BandType::LowPass,
            },
        );
        let f = &c.filters[0];
        assert_eq!(f.filter_type, FilterType::LowPassQ);
        assert!((f.freq - 1000.0).abs() < 1e-9);
        assert!((f.gain_db - 6.0).abs() < 1e-9);
        assert!((f.q - 2.0).abs() < 1e-9);
    }

    #[test]
    fn remove_band_out_of_range_is_noop() {
        let mut c = chain();
        apply_command(
            &mut c,
            AudioCommand::AddBand {
                band_type: BandType::Peaking,
                freq: 1000.0,
                gain_db: 0.0,
                q: 1.0,
            },
        );
        apply_command(&mut c, AudioCommand::RemoveBand { index: 5 });
        assert_eq!(c.filters.len(), 1);
        apply_command(&mut c, AudioCommand::RemoveBand { index: 0 });
        assert_eq!(c.filters.len(), 0);
    }

    #[test]
    fn preamp_and_power_commands_apply() {
        let mut c = chain();
        apply_command(&mut c, AudioCommand::SetPreamp(-6.0));
        apply_command(&mut c, AudioCommand::SetPower(false));
        assert!((c.preamp_db + 6.0).abs() < 1e-9);
        assert!(!c.enabled);
    }

    #[test]
    fn set_band_channels_retargets_mask() {
        use resonance_dsp::channel::ChannelMask;
        let mut c = chain();
        apply_command(
            &mut c,
            AudioCommand::AddBand {
                band_type: BandType::Peaking,
                freq: 1000.0,
                gain_db: 6.0,
                q: 1.0,
            },
        );
        assert_eq!(c.filters[0].mask, ChannelMask::ALL, "new band is global");
        apply_command(
            &mut c,
            AudioCommand::SetBandChannels {
                index: 0,
                mask: ChannelMask::single(1),
            },
        );
        assert_eq!(c.filters[0].mask, ChannelMask::single(1));
    }

    #[test]
    fn set_band_preserves_mask_on_param_edit() {
        use resonance_dsp::channel::ChannelMask;
        let mut c = chain();
        apply_command(
            &mut c,
            AudioCommand::AddBand {
                band_type: BandType::Peaking,
                freq: 1000.0,
                gain_db: 6.0,
                q: 1.0,
            },
        );
        apply_command(
            &mut c,
            AudioCommand::SetBandChannels {
                index: 0,
                mask: ChannelMask::single(0),
            },
        );
        // A freq/gain/q edit must NOT reset the channel target.
        apply_command(
            &mut c,
            AudioCommand::SetBand {
                index: 0,
                freq: 2000.0,
                gain_db: 3.0,
                q: 1.5,
            },
        );
        assert_eq!(c.filters[0].mask, ChannelMask::single(0), "mask preserved");
        assert!((c.filters[0].freq - 2000.0).abs() < 1e-9);
    }

    #[test]
    fn set_routing_installs_and_clears() {
        use resonance_dsp::channel::ChannelMatrix;
        let mut c = chain();
        apply_command(
            &mut c,
            AudioCommand::SetRouting {
                matrix: Some(ChannelMatrix::swap(2, 0, 1)),
            },
        );
        assert!(c.routing.as_ref().is_some_and(|m| !m.is_identity()));
        apply_command(&mut c, AudioCommand::SetRouting { matrix: None });
        assert!(c.routing.is_none());
    }

    #[test]
    fn replace_chain_preserves_the_live_sample_rate() {
        // The RT thread runs at the live device rate (here 96 kHz); a
        // ReplaceChain carrying a chain built at the 48 kHz construction default
        // (the frozen shadow, or a preset/profile built before the live rate is
        // known) must not drag the live rate back down. The bands/effects are
        // adopted but re-bound to the live rate. Guards the macOS device-rate
        // follow path — and any non-48k backend — against the resync/profile
        // clobber that otherwise mis-set the EQ coefficients and `status` rate.
        let mut live = ProcessorChain::builder()
            .channels(CHANNELS)
            .sample_rate(96_000.0)
            .build();
        let replacement = ProcessorChain::builder()
            .channels(CHANNELS)
            .sample_rate(48_000.0)
            .build();
        apply_command(&mut live, AudioCommand::ReplaceChain(Box::new(replacement)));
        assert!(
            (live.sample_rate - 96_000.0).abs() < 1e-9,
            "ReplaceChain kept {} Hz; must stay at the live 96 kHz device rate",
            live.sample_rate
        );
    }
}
