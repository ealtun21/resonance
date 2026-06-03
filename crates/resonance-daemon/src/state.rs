use resonance_dsp::{chain::FxEffect, chain::ProcessorChain, effects::Effect};
use resonance_ipc::{BandState, DaemonState, EffectsState};
use rtrb::Producer;
use std::sync::{Arc, Mutex};

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
}

pub struct Inner {
    /// Shadow state for status queries (not used for audio processing).
    pub chain: ProcessorChain,
    pub current_preset: Option<String>,
    /// Channel to send commands to the audio thread.
    pub audio_tx: Producer<AudioCommand>,
}

#[derive(Clone)]
pub struct SharedState(pub Arc<Mutex<Inner>>);

impl SharedState {
    pub fn new(audio_tx: Producer<AudioCommand>) -> Self {
        let chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48000.0)
            .build();
        Self(Arc::new(Mutex::new(Inner {
            chain,
            current_preset: None,
            audio_tx,
        })))
    }

    /// Send a command to the audio thread and update the shadow chain.
    pub fn send(&self, cmd: AudioCommand, shadow_update: impl FnOnce(&mut ProcessorChain)) {
        let mut inner = self.0.lock().unwrap();
        shadow_update(&mut inner.chain);
        // Best-effort: if the ring buffer is full, the command is dropped.
        // This can only happen if the audio thread is not running.
        let _ = inner.audio_tx.push(cmd);
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
