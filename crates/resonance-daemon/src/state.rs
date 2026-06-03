use resonance_dsp::{chain::ProcessorChain, effects::Effect};
use resonance_ipc::{BandState, DaemonState, EffectsState};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct SharedState(pub Arc<Mutex<Inner>>);

pub struct Inner {
    pub chain: ProcessorChain,
    pub current_preset: Option<String>,
}

impl SharedState {
    pub fn new() -> Self {
        let chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48000.0)
            .build();
        Self(Arc::new(Mutex::new(Inner {
            chain,
            current_preset: None,
        })))
    }

    pub fn snapshot(&self) -> DaemonState {
        let inner = self.0.lock().unwrap();
        let chain = &inner.chain;

        let bands = chain
            .filters
            .iter()
            .map(|f| BandState {
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
        }
    }
}
