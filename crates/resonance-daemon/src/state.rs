use resonance_dsp::{chain::FxEffect, chain::ProcessorChain, effects::Effect};
use resonance_ipc::{BandState, BandType, DaemonState, EffectsState};
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
    /// Latest spectrum — updated by the spectrum task, read by IPC handler.
    pub spectrum: [f32; SPECTRUM_BINS],
    /// Available PipeWire Audio/Sink names (updated by pw_node).
    pub available_sinks: Vec<String>,
    /// Preferred output node name set by SetOutputTarget.
    pub preferred_output: Option<String>,
    /// Send a preferred-output name to the pw_node main-loop thread.
    pub route_tx: std::sync::mpsc::Sender<String>,
    /// In-memory A/B comparison slots ([A, B]); filled by `StoreSlot`.
    pub ab_slots: [Option<crate::config::Profile>; 2],
}

#[derive(Clone)]
pub struct SharedState(pub Arc<Mutex<Inner>>);

impl SharedState {
    pub fn new(
        audio_tx: Producer<AudioCommand>,
        route_tx: std::sync::mpsc::Sender<String>,
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
            spectrum: [0.0; SPECTRUM_BINS],
            available_sinks: Vec::new(),
            preferred_output: None,
            route_tx,
            ab_slots: [None, None],
        })))
    }

    pub fn send(&self, cmd: AudioCommand, shadow_update: impl FnOnce(&mut ProcessorChain)) {
        let mut inner = self.0.lock().unwrap();
        shadow_update(&mut inner.chain);
        let _ = inner.audio_tx.push(cmd);
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

        DaemonState {
            enabled: chain.enabled,
            preamp_db: chain.preamp_db,
            eq_enabled: true,
            bands,
            effects: EffectsState {
                fidelity_intensity: chain.fidelity.intensity(),
                fidelity_enabled: chain.fidelity.enabled(),
                ambience_intensity: chain.ambience.intensity(),
                ambience_enabled: chain.ambience.enabled(),
                surround_intensity: chain.surround.intensity(),
                surround_enabled: chain.surround.enabled(),
                dynamic_boost_intensity: chain.dynamic_boost.intensity(),
                dynamic_boost_enabled: chain.dynamic_boost.enabled(),
                bass_intensity: chain.bass.intensity(),
                bass_enabled: chain.bass.enabled(),
            },
            current_preset: inner.current_preset.clone(),
            sample_rate: chain.sample_rate,
            channels: chain.channels,
            spectrum: inner.spectrum.to_vec(),
            active_output: inner.active_output.clone(),
            mapped_profile: inner.mapped_profile.clone(),
            available_sinks: inner.available_sinks.clone(),
            preferred_output: inner.preferred_output.clone(),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use resonance_dsp::effects::Effect;
    use resonance_dsp::filter::{ApoFilter, FilterType};

    fn shared() -> SharedState {
        let (tx, _rx) = rtrb::RingBuffer::<AudioCommand>::new(16);
        let (route_tx, _route_rx) = std::sync::mpsc::channel();
        SharedState::new(tx, route_tx)
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
