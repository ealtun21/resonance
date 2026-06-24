use crate::meters::AtomicMeters;
use resonance_dsp::{chain::FxEffect, chain::ProcessorChain};
use resonance_ipc::{BandState, BandType, DaemonState, EffectsState, FxEffectId};
use rtrb::Producer;
use std::sync::{Arc, Mutex};

pub const SPECTRUM_BINS: usize = 64;

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
    /// Available PipeWire Audio/Sink names (updated by pw_node).
    pub available_sinks: Vec<String>,
    /// Friendly `node.description` per sink as `(node_name, description)` (updated by pw_node).
    pub sink_descriptions: Vec<(String, String)>,
    /// Preferred output node name set by SetOutputTarget.
    pub preferred_output: Option<String>,
    /// Send a preferred-output name to the pw_node main-loop thread.
    pub route_tx: std::sync::mpsc::Sender<String>,
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
    ) -> Self {
        let chain = ProcessorChain::builder()
            .channels(2)
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
            available_sinks: Vec::new(),
            sink_descriptions: Vec::new(),
            preferred_output: None,
            route_tx,
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
            .map(|t| t.elapsed() < std::time::Duration::from_millis(1500))
            .unwrap_or(false);
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
            }
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
            })
            .collect();

        let mut effects = EffectsState::default();
        for id in FxEffectId::ALL {
            let (intensity, enabled) = chain.effect_params(FxEffect::from(id));
            effects.set(id, intensity, enabled);
        }

        let dsp_rate = inner.meters.sample_rate().unwrap_or(chain.sample_rate);

        DaemonState {
            enabled: chain.enabled,
            preamp_db: chain.preamp_db,
            eq_enabled: true,
            bands,
            effects,
            current_preset: inner.current_preset.clone(),
            // Prefer the live rate the RT thread reports (it follows device/graph
            // renegotiation); fall back to the mirror chain before audio starts.
            sample_rate: dsp_rate,
            // Capture rate; equals the DSP rate unless a backend is resampling.
            capture_rate: inner.meters.capture_rate().unwrap_or(dsp_rate),
            channels: chain.channels,
            spectrum: inner.spectrum.to_vec(),
            active_output: inner.active_output.clone(),
            mapped_profile: inner.mapped_profile.clone(),
            available_sinks: inner.available_sinks.clone(),
            sink_descriptions: inner.sink_descriptions.clone(),
            preferred_output: inner.preferred_output.clone(),
            meters: inner.meters.snapshot(),
        }
    }

    /// Update spectrum bins (called from the spectrum computation task).
    pub fn update_spectrum(&self, bins: [f32; SPECTRUM_BINS]) {
        self.0.lock().unwrap().spectrum = bins;
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
    /// twice — the RT thread and the GetState shadow each need their own
    /// instance — so it must produce an identical chain on each call.
    pub fn rebuild_chain(&self, build: impl Fn(usize, f64) -> ProcessorChain) {
        let (channels, sample_rate) = {
            let inner = self.0.lock().unwrap();
            (inner.chain.channels, inner.chain.sample_rate)
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
        SharedState::new(tx, route_tx, Arc::new(AtomicMeters::default()))
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
}
