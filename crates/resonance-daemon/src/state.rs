use crate::meters::AtomicMeters;
use resonance_dsp::channel::{ChannelMask, ChannelMatrix};
use resonance_dsp::{chain::FxEffect, chain::ProcessorChain};
use resonance_ipc::{
    AppStream, BandDynamics, BandScope, BandState, BandType, ConvolutionState, DaemonState,
    EffectsState, FxEffectId, RoutingMatrix, SinkVolume, default_channel_layout,
};
use rtrb::Producer;
use std::sync::{Arc, Mutex};

pub const SPECTRUM_BINS: usize = 64;

/// Rolling post-DSP capture depth in mono samples (~5.5 s at 48 kHz, 1 MiB).
/// Serves `Command::CaptureOutput` for the `resonance verify` harness.
pub const CAPTURE_BUF: usize = 1 << 18;

/// Commands sent from the IPC/tokio thread to the RT audio thread.
#[derive(Debug)]
pub enum AudioCommand {
    SetPower(bool),
    SetPreamp(f64),
    SetEffectIntensity {
        effect: FxEffect,
        value: f64,
    },
    SetEffectEnabled {
        effect: FxEffect,
        on: bool,
    },
    /// Set (or clear) the final-stage output dither target bit depth.
    SetDither {
        bits: Option<u32>,
    },
    ReplaceChain(Box<ProcessorChain>),
    #[allow(dead_code)]
    Reset,
    SetBand {
        index: usize,
        freq: f64,
        gain_db: f64,
        q: f64,
    },
    SetBandEnabled {
        index: usize,
        enabled: bool,
    },
    AddBand {
        band_type: BandType,
        freq: f64,
        gain_db: f64,
        q: f64,
    },
    RemoveBand {
        index: usize,
    },
    SetBandType {
        index: usize,
        band_type: BandType,
    },
    /// Change an EQ band's filter slope (12/24/48 dB/oct).
    SetBandSlope {
        index: usize,
        slope_db_oct: u8,
    },
    /// Change an EQ band's stereo scope (Stereo/Mid/Side).
    SetBandScope {
        index: usize,
        scope: BandScope,
    },
    /// Set (or clear) an EQ band's dynamic EQ (level-driven gain morph).
    SetBandDynamics {
        index: usize,
        dynamics: Option<BandDynamics>,
    },
    /// Retarget an existing band to a channel subset (per-channel EQ).
    SetBandChannels {
        index: usize,
        mask: ChannelMask,
    },
    /// Install (or clear) the output routing/remap matrix.
    SetRouting {
        matrix: Option<ChannelMatrix>,
    },
    /// Swap in a fully-prepared convolution engine (IR decoded, resampled and
    /// FFT-transformed on the IPC thread — the RT thread only installs it).
    SetConvolution(Box<resonance_dsp::convolution::ConvolutionEngine>),
    /// Bypass or re-arm the convolution stage without dropping its IR.
    SetConvolutionEnabled(bool),
    /// Drop the convolution IR entirely (passthrough, zero added latency).
    ClearConvolution,
}

/// Per-application volume/mute requests, forwarded from the IPC thread to
/// whichever component owns app control: the backend main-loop thread
/// (Linux/PipeWire, macOS taps) or a Windows WASAPI control task. Unlike
/// [`AudioCommand`] this never touches the RT DSP path — it's control-plane.
#[derive(Debug, Clone)]
pub enum AppControl {
    SetVolume { key: String, volume: f64 },
    SetMute { key: String, muted: bool },
}

/// Per-output-sink volume/mute requests, forwarded from the IPC thread to the
/// backend (`PipeWire` main-loop thread). Control-plane, like [`AppControl`].
#[derive(Debug, Clone)]
pub enum SinkCtl {
    SetVolume { name: String, volume: f64 },
    SetMute { name: String, muted: bool },
}

pub struct Inner {
    pub chain: ProcessorChain,
    pub current_preset: Option<String>,
    /// Node name of the output device Resonance is currently feeding.
    pub active_output: Option<String>,
    /// Profile auto-loaded for the active output (if mapped).
    pub mapped_profile: Option<String>,
    pub audio_tx: Producer<AudioCommand>,
    /// Set when a command couldn't be pushed (ring full): the RT chain has
    /// fallen behind the authoritative shadow and must be resynced wholesale on
    /// the next push that finds room.
    pub needs_resync: bool,
    /// Latest spectrum — updated by the spectrum task, read by IPC handler.
    pub spectrum: [f32; SPECTRUM_BINS],
    /// Rolling buffer of the freshest post-DSP mono samples (fed by the
    /// spectrum task from the RT sample ring, capped at [`CAPTURE_BUF`]).
    /// Read by `CaptureOutput` so `resonance verify` can measure the live
    /// output without a soundcard loopback.
    pub capture: std::collections::VecDeque<f32>,
    /// Available `PipeWire` Audio/Sink names (updated by `pw_node`).
    pub available_sinks: Vec<String>,
    /// Friendly `node.description` per sink as `(node_name, description)` (updated by `pw_node`).
    pub sink_descriptions: Vec<(String, String)>,
    /// Preferred output node name set by `SetOutputTarget`.
    pub preferred_output: Option<String>,
    /// Send a preferred-output name to the `pw_node` main-loop thread.
    pub route_tx: std::sync::mpsc::Sender<String>,
    /// Latest per-application stream list (pushed by the backend / a platform
    /// app-enumeration task, read on snapshot).
    pub apps: Vec<AppStream>,
    /// Forward per-app volume/mute to the component that owns app control
    /// (backend thread or Windows control task).
    pub app_ctl_tx: std::sync::mpsc::Sender<AppControl>,
    /// Latest output-sink volume list (pushed by the backend, read on snapshot).
    pub sinks: Vec<SinkVolume>,
    /// Forward per-sink volume/mute to the backend (`PipeWire` main-loop thread).
    pub sink_ctl_tx: std::sync::mpsc::Sender<SinkCtl>,
    /// In-memory A/B comparison slots ([A, B]); filled by `StoreSlot`.
    pub ab_slots: [Option<crate::config::Profile>; 2],
    /// Live meters written by the RT thread, read on snapshot.
    pub meters: Arc<AtomicMeters>,
    /// Windows only: writes chain state to the shared file the APO reads. The
    /// daemon does no audio on Windows — it drives the in-graph APO instead.
    pub apo_writer: Option<resonance_apo::state::ApoStateWriter>,
    /// Last time a client polled `GetState`; used on Windows to enable APO
    /// telemetry (meters/spectrum) only while something is watching.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub last_poll: Option<std::time::Instant>,
}

#[derive(Clone)]
pub struct SharedState(pub Arc<Mutex<Inner>>);

impl SharedState {
    pub fn new(
        audio_tx: Producer<AudioCommand>,
        route_tx: std::sync::mpsc::Sender<String>,
        meters: Arc<AtomicMeters>,
        app_ctl_tx: std::sync::mpsc::Sender<AppControl>,
        sink_ctl_tx: std::sync::mpsc::Sender<SinkCtl>,
    ) -> Self {
        let chain = ProcessorChain::builder()
            .channels(crate::audio::target_channels())
            .sample_rate(48000.0)
            .build();
        Self(Arc::new(Mutex::new(Inner {
            chain,
            current_preset: None,
            active_output: None,
            mapped_profile: None,
            audio_tx,
            needs_resync: false,
            spectrum: [0.0; SPECTRUM_BINS],
            capture: std::collections::VecDeque::with_capacity(CAPTURE_BUF),
            available_sinks: Vec::new(),
            sink_descriptions: Vec::new(),
            preferred_output: None,
            route_tx,
            apps: Vec::new(),
            app_ctl_tx,
            sinks: Vec::new(),
            sink_ctl_tx,
            ab_slots: [None, None],
            meters,
            apo_writer: None,
            last_poll: None,
        })))
    }

    /// Record that a client just polled state (drives Windows telemetry gating).
    pub fn mark_polled(&self) {
        self.0.lock().unwrap().last_poll = Some(std::time::Instant::now());
    }

    /// Windows telemetry pump: enable APO telemetry while a client is watching,
    /// and copy the APO's meters/spectrum into `SharedState` for clients.
    #[cfg(target_os = "windows")]
    pub fn pump_telemetry(&self) {
        let mut guard = self.0.lock().unwrap();
        let inner = &mut *guard;
        let watching = inner
            .last_poll
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(1500));
        let Some(w) = inner.apo_writer.as_ref() else {
            return;
        };
        // The gate is a daemon write — its store reaches the file the APO reads.
        w.set_telemetry_enabled(watching);
        if watching {
            // Read telemetry FRESH: the daemon's long-lived mapped view doesn't
            // observe the APO's cross-session writes, but a fresh file read does.
            let path = resonance_apo::state::default_state_path();
            if let Some(t) = resonance_apo::state::read_telemetry_fresh(&path) {
                let n = inner.spectrum.len().min(t.spectrum.len());
                inner.spectrum[..n].copy_from_slice(&t.spectrum[..n]);
                inner.meters.store(crate::meters::Sample {
                    in_peak: t.in_peak,
                    out_peak: t.out_peak,
                    in_rms: t.in_rms,
                    out_rms: t.out_rms,
                    clip: t.out_peak >= 1.0,
                    dsp_load: 0.0,
                    dsp_frame_us: 0,
                });
                // The Windows daemon has no audio backend, so the APO is the only
                // source of the live endpoint rate; surface it so `status` shows
                // the real rate (e.g. 96 kHz) instead of the mirror chain's
                // construction default. 0 until the APO has locked a format.
                if t.sample_rate > 0.0 {
                    inner.meters.set_sample_rate(f64::from(t.sample_rate));
                    inner.meters.set_capture_rate(f64::from(t.sample_rate));
                }
            }
        } else {
            // Gate closed (no client watching): clear the cached spectrum so a
            // client that reconnects starts from silence rather than the frozen
            // last frame held in SharedState.
            inner.spectrum.fill(0.0);
        }
    }

    pub fn send(&self, cmd: AudioCommand, shadow_update: impl FnOnce(&mut ProcessorChain)) {
        let mut guard = self.0.lock().unwrap();
        let inner = &mut *guard;
        // The shadow chain is the source of truth and is always updated.
        shadow_update(&mut inner.chain);
        // The RT thread is one command behind the shadow. If the ring is full
        // the command is dropped and the two diverge; remember that so the next
        // push with room can resync the RT thread to the authoritative shadow
        // (one wholesale ReplaceChain, recovering any commands lost in between).
        if inner.audio_tx.push(cmd).is_err() {
            inner.needs_resync = true;
        } else if inner.needs_resync {
            let resync = AudioCommand::ReplaceChain(Box::new(inner.chain.clone()));
            if inner.audio_tx.push(resync).is_ok() {
                inner.needs_resync = false;
            }
        }
        // Mirror the new state to the Windows APO (no-op when no writer).
        if let Some(w) = inner.apo_writer.as_mut() {
            w.publish(&inner.chain);
        }
    }

    /// Install the Windows APO state writer and publish the current chain once.
    #[cfg(target_os = "windows")]
    pub fn set_apo_writer(&self, writer: resonance_apo::state::ApoStateWriter) {
        let mut guard = self.0.lock().unwrap();
        let inner = &mut *guard;
        inner.apo_writer = Some(writer);
        if let Some(w) = inner.apo_writer.as_mut() {
            w.publish(&inner.chain);
        }
    }

    pub fn snapshot(&self) -> DaemonState {
        let inner = self.0.lock().unwrap();
        let chain = &inner.chain;

        let bands = chain
            .filters
            .iter()
            .map(|f| BandState {
                band_type: BandType::from(f.filter_type),
                freq: f.freq,
                gain_db: f.gain_db,
                q: f.q,
                enabled: f.enabled,
                channels: resonance_ipc::ChannelMask::from_dsp(f.mask),
                slope_db_oct: f.slope_db_oct,
                scope: BandScope::from(f.scope),
                dynamics: f.dynamics().map(BandDynamics::from),
            })
            .collect();

        let mut effects = EffectsState::default();
        for id in FxEffectId::ALL {
            let (intensity, enabled) = chain.effect_params(FxEffect::from(id));
            effects.set(id, intensity, enabled);
        }

        let dsp_rate = inner.meters.sample_rate().unwrap_or(chain.sample_rate);
        // Prefer the live channel width the RT thread reports — the mirror chain
        // stays frozen at its construction width while the backend follows the
        // output device's channel count, so reading `chain.channels` would report
        // a stale width after a device-channel hot-swap. Same workaround as the
        // sample rate above. Falls back to the mirror before audio starts.
        let live_channels = inner.meters.channels().unwrap_or(chain.channels);

        DaemonState {
            enabled: chain.enabled,
            preamp_db: chain.preamp_db,
            eq_enabled: true,
            bands,
            effects,
            dither_bits: chain.dither.bits(),
            current_preset: inner.current_preset.clone(),
            // Prefer the live rate the RT thread reports (it follows device/graph
            // renegotiation); fall back to the mirror chain before audio starts.
            sample_rate: dsp_rate,
            // Capture rate; equals the DSP rate unless a backend is resampling.
            capture_rate: inner.meters.capture_rate().unwrap_or(dsp_rate),
            channels: live_channels,
            // No routing ⇒ out == live width; with a (square) remap the count is
            // the matrix's, which travels with the mirror chain's routing field.
            out_channels: if chain.routing.is_some() {
                chain.out_channels()
            } else {
                live_channels
            },
            channel_layout: default_channel_layout(live_channels),
            routing: chain.routing.as_ref().map(RoutingMatrix::from_dsp),
            spectrum: inner.spectrum.to_vec(),
            active_output: inner.active_output.clone(),
            mapped_profile: inner.mapped_profile.clone(),
            available_sinks: inner.available_sinks.clone(),
            sink_descriptions: inner.sink_descriptions.clone(),
            preferred_output: inner.preferred_output.clone(),
            meters: inner.meters.snapshot(),
            apps: inner.apps.clone(),
            sinks: inner.sinks.clone(),
            convolution: chain.convolution.info().map(|i| ConvolutionState {
                path: i.path,
                name: i.name,
                ir_sample_rate: i.ir_sample_rate,
                ir_channels: i.ir_channels,
                taps: i.taps,
                latency_frames: i.latency_frames,
                enabled: chain.convolution.enabled(),
            }),
        }
    }

    /// Update spectrum bins (called from the spectrum computation task).
    pub fn update_spectrum(&self, bins: [f32; SPECTRUM_BINS]) {
        self.0.lock().unwrap().spectrum = bins;
    }

    /// Replace the output-sink volume list (called by the backend whenever a
    /// sink's volume/mute changes or the set of sinks changes).
    pub fn set_sinks(&self, sinks: Vec<SinkVolume>) {
        self.0.lock().unwrap().sinks = sinks;
    }

    /// Forward a per-sink volume/mute request to the backend. Best effort.
    pub fn forward_sink_ctl(&self, ctl: SinkCtl) {
        let _ = self.0.lock().unwrap().sink_ctl_tx.send(ctl);
    }

    pub fn set_apps(&self, apps: Vec<AppStream>) {
        self.0.lock().unwrap().apps = apps;
    }

    /// Forward a per-app volume/mute request to the owning component. Best
    /// effort: a dropped receiver (e.g. no backend) is ignored.
    pub fn forward_app_ctl(&self, ctl: AppControl) {
        let _ = self.0.lock().unwrap().app_ctl_tx.send(ctl);
    }

    /// Swap the whole chain: hand `rt` to the RT thread and mirror `shadow` so
    /// `GetState` reflects it. Callers build two identical chains (the RT one is
    /// moved across the ring buffer; the shadow stays here).
    pub fn replace_chain(&self, rt: ProcessorChain, shadow: ProcessorChain) {
        self.send(AudioCommand::ReplaceChain(Box::new(rt)), move |s| {
            *s = shadow;
        });
    }

    /// Read the live `(channels, sample_rate)` and rebuild both the RT and the
    /// shadow chain from the same builder, then swap them in. `build` is invoked
    /// twice — the RT thread and the `GetState` shadow each need their own
    /// instance — so it must produce an identical chain on each call.
    pub fn rebuild_chain(&self, build: impl Fn(usize, f64) -> ProcessorChain) {
        let (channels, sample_rate) = {
            let inner = self.0.lock().unwrap();
            // Prefer the live rate the RT thread reports — the shadow chain's
            // `sample_rate` stays frozen at its construction rate (the RT chain
            // follows the graph via `rebind_sample_rate`, but never writes back),
            // so building from it would briefly run wrong-rate coefficients on an
            // off-48k graph until the next RT block re-binds. Same workaround as
            // `snapshot`.
            let sr = inner
                .meters
                .sample_rate()
                .unwrap_or(inner.chain.sample_rate);
            // Same reason channels is read from the meters, not the mirror: the RT
            // path follows the output device's channel count, but the mirror chain
            // stays frozen at its construction width. Building from the stale mirror
            // would hand the RT thread a chain whose width disagrees with the live
            // ports — the DSP then runs on a misframed buffer (wrong per-channel EQ,
            // channel cross-talk) until the next device-follow rebuild.
            let channels = inner.meters.channels().unwrap_or(inner.chain.channels);
            (channels, sr)
        };
        self.replace_chain(build(channels, sample_rate), build(channels, sample_rate));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use resonance_dsp::effects::Effect;
    use resonance_dsp::filter::{ApoFilter, FilterType};

    fn shared() -> SharedState {
        let (tx, _rx) = rtrb::RingBuffer::<AudioCommand>::new(16);
        let (route_tx, _route_rx) = std::sync::mpsc::channel();
        let (app_ctl_tx, _app_ctl_rx) = std::sync::mpsc::channel();
        let (sink_ctl_tx, _sink_ctl_rx) = std::sync::mpsc::channel();
        SharedState::new(
            tx,
            route_tx,
            Arc::new(AtomicMeters::default()),
            app_ctl_tx,
            sink_ctl_tx,
        )
    }

    #[test]
    fn snapshot_maps_filter_type_to_band_type() {
        let state = shared();
        {
            let mut inner = state.0.lock().unwrap();
            inner.chain = ProcessorChain::builder()
                .channels(2)
                .sample_rate(48000.0)
                .add_filter(
                    ApoFilter::builder()
                        .filter_type(FilterType::HighShelf)
                        .freq(8000.0)
                        .gain_db(3.0)
                        .q(0.707)
                        .enabled(true)
                        .channels(2)
                        .sample_rate(48000.0)
                        .build()
                        .unwrap(),
                )
                .build();
        }
        let snap = state.snapshot();
        assert_eq!(snap.bands.len(), 1);
        assert_eq!(snap.bands[0].band_type, BandType::HighShelf);
        assert!((snap.bands[0].freq - 8000.0).abs() < 1e-9);
    }

    #[test]
    fn snapshot_reports_effect_and_preamp_state() {
        let state = shared();
        {
            let mut inner = state.0.lock().unwrap();
            inner.chain.preamp_db = -4.0;
            inner.chain.bass.set_intensity(-0.5);
        }
        let snap = state.snapshot();
        assert!((snap.preamp_db + 4.0).abs() < 1e-9);
        assert!((snap.effects.bass_intensity + 0.5).abs() < 1e-9);
    }

    // Regression: after the PipeWire backend follows the output device's channel
    // count, the mirror chain stays frozen at its startup width. Both `snapshot`
    // and `rebuild_chain` must read the live width from the meters, or `status`
    // misreports the channel count and a preset load rebuilds at the wrong width
    // (handing the RT thread a chain that misframes the live buffer).
    #[test]
    fn snapshot_reports_live_channel_count_after_device_follow() {
        let state = shared();
        {
            let mut inner = state.0.lock().unwrap();
            inner.chain = ProcessorChain::builder()
                .channels(2)
                .sample_rate(48000.0)
                .build();
            // RT thread followed the output device up to 5.1 (6 ch); the mirror
            // chain above is left frozen at the stereo startup width.
            inner.meters.set_channels(6);
        }
        let snap = state.snapshot();
        assert_eq!(
            snap.channels, 6,
            "status must report the live width, not the frozen mirror"
        );
        assert_eq!(snap.out_channels, 6);
        assert_eq!(snap.channel_layout.len(), 6);
    }

    #[test]
    fn rebuild_chain_builds_at_live_width_not_stale_mirror() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let state = shared();
        {
            let mut inner = state.0.lock().unwrap();
            inner.chain = ProcessorChain::builder()
                .channels(2)
                .sample_rate(48000.0)
                .build();
            inner.meters.set_channels(6); // device-follow to 5.1
            inner.meters.set_sample_rate(48000.0);
        }
        let seen = Arc::new(AtomicUsize::new(0));
        let seen_w = Arc::clone(&seen);
        state.rebuild_chain(move |channels, sr| {
            seen_w.store(channels, Ordering::Relaxed);
            ProcessorChain::builder()
                .channels(channels)
                .sample_rate(sr)
                .build()
        });
        assert_eq!(
            seen.load(Ordering::Relaxed),
            6,
            "rebuild_chain must build at the live device width, not the stale mirror"
        );
        // The mirror chain is now the rebuilt one, so it matches the live width.
        assert_eq!(state.0.lock().unwrap().chain.channels, 6);
    }

    #[test]
    fn snapshot_reports_convolution_state() {
        let state = shared();
        assert!(
            state.snapshot().convolution.is_none(),
            "no IR loaded → None"
        );
        {
            let mut inner = state.0.lock().unwrap();
            let ir = resonance_dsp::convolution::IrData {
                name: "room".into(),
                path: "/irs/room.wav".into(),
                sample_rate: 48_000.0,
                channels: vec![vec![1.0, 0.5, 0.25]],
            };
            inner
                .chain
                .convolution
                .load_ir(std::sync::Arc::new(ir))
                .unwrap();
        }
        let conv = state.snapshot().convolution.expect("IR loaded → Some");
        assert_eq!(conv.name, "room");
        assert_eq!(conv.path, "/irs/room.wav");
        assert_eq!(conv.ir_channels, 1);
        assert_eq!(conv.taps, 3);
        assert!(conv.enabled);
        assert_eq!(
            conv.latency_frames,
            resonance_dsp::convolution::BLOCK,
            "active convolution reports one block of latency"
        );
    }

    #[test]
    fn snapshot_falls_back_to_mirror_width_before_audio_starts() {
        // No backend has reported a live width yet (meters channels == 0): both
        // snapshot and rebuild must fall back to the mirror chain's width.
        let state = shared();
        {
            let mut inner = state.0.lock().unwrap();
            inner.chain = ProcessorChain::builder()
                .channels(4)
                .sample_rate(48000.0)
                .build();
        }
        assert_eq!(state.snapshot().channels, 4);
    }
}
