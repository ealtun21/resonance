use resonance_dsp::chain::FxEffect;
use serde::{Deserialize, Serialize};

pub mod transport;

pub const SOCKET_PATH_ENV: &str = "RESONANCE_SOCKET";
pub const DEFAULT_SOCKET_FILENAME: &str = "resonance.sock";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    /// Load preset from file path (.fac or APO .txt, detected by extension)
    LoadPreset { path: String },
    /// Set an FxEffect intensity (0.0–1.0)
    SetEffectIntensity { effect: FxEffectId, value: f64 },
    /// Enable or disable a specific FxEffect
    SetEffectEnabled { effect: FxEffectId, enabled: bool },
    /// Set EQ band parameters by band index
    SetBand {
        index: usize,
        freq: f64,
        gain_db: f64,
        q: f64,
    },
    /// Enable or disable EQ band by index
    SetBandEnabled { index: usize, enabled: bool },
    /// Set overall preamp gain in dB
    SetPreamp { db: f64 },
    /// Enable or disable the entire processing chain
    SetPower { enabled: bool },
    /// Request current state snapshot
    GetState,
    /// List available presets in a directory
    ListPresets { dir: String },
    /// Subscribe to state-change events (TUI stream)
    Subscribe,
    /// Stop the daemon
    Shutdown,
}

/// Serializable mirror of FxEffect (avoids depending on resonance-dsp in serde derives)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FxEffectId {
    Fidelity,
    Ambience,
    Surround,
    DynamicBoost,
    Bass,
}

impl From<FxEffectId> for FxEffect {
    fn from(id: FxEffectId) -> Self {
        match id {
            FxEffectId::Fidelity => FxEffect::Fidelity,
            FxEffectId::Ambience => FxEffect::Ambience,
            FxEffectId::Surround => FxEffect::Surround,
            FxEffectId::DynamicBoost => FxEffect::DynamicBoost,
            FxEffectId::Bass => FxEffect::Bass,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    Ok,
    State(DaemonState),
    PresetList(Vec<String>),
    Error(String),
    /// Pushed by daemon for Subscribe clients
    StateChanged(DaemonState),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonState {
    pub enabled: bool,
    pub preamp_db: f64,
    pub eq_enabled: bool,
    pub bands: Vec<BandState>,
    pub effects: EffectsState,
    pub current_preset: Option<String>,
    pub sample_rate: f64,
    pub channels: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BandState {
    pub freq: f64,
    pub gain_db: f64,
    pub q: f64,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectsState {
    pub fidelity_intensity: f64,
    pub fidelity_enabled: bool,
    pub ambience_intensity: f64,
    pub ambience_enabled: bool,
    pub surround_intensity: f64,
    pub surround_enabled: bool,
    pub dynamic_boost_intensity: f64,
    pub dynamic_boost_enabled: bool,
    pub bass_intensity: f64,
    pub bass_enabled: bool,
}
