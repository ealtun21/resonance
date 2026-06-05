use resonance_dsp::chain::FxEffect;
use resonance_dsp::filter::FilterType;
use serde::{Deserialize, Serialize};

pub mod transport;

pub const SOCKET_PATH_ENV: &str = "RESONANCE_SOCKET";
pub const DEFAULT_SOCKET_FILENAME: &str = "resonance.sock";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    /// Load preset from file path (.fac or APO .txt, detected by extension)
    LoadPreset { path: String },
    /// Parse a preset file and save it as a named profile (does NOT apply it).
    /// `name` defaults to the file stem when None.
    ImportPreset { path: String, name: Option<String> },
    /// Rename a saved profile.
    RenameProfile { from: String, to: String },
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
    /// Add a new EQ band of the given type
    AddBand {
        band_type: BandType,
        freq: f64,
        gain_db: f64,
        q: f64,
    },
    /// Remove EQ band by index
    RemoveBand { index: usize },
    /// Change the filter type of an existing band
    SetBandType { index: usize, band_type: BandType },
    /// Reset to defaults: flat EQ (no bands), all effects off, 0 dB preamp
    Reset,
    /// Export the current EQ (preamp + bands) to an EqualizerAPO `.txt` file
    ExportApo { path: String },
    /// Store the current chain state into an in-memory A/B slot ("a" or "b")
    StoreSlot { slot: AbSlot },
    /// Recall a previously stored A/B slot onto the chain
    RecallSlot { slot: AbSlot },
    /// Set overall preamp gain in dB
    SetPreamp { db: f64 },
    /// Enable or disable the entire processing chain
    SetPower { enabled: bool },
    /// Request current state snapshot
    GetState,
    /// List available presets in a directory
    ListPresets { dir: String },
    /// Save the current chain state as a named profile in the config dir
    SaveProfile { name: String },
    /// Load a named profile from the config dir
    LoadProfile { name: String },
    /// Delete a named profile from the config dir
    DeleteProfile { name: String },
    /// List saved profile names
    ListProfiles,
    /// Map the current active output device to the given profile
    MapOutput { profile: String },
    /// Remove the mapping for the current active output device
    UnmapOutput,
    /// List all output→profile mappings
    ListMappings,
    /// Route the filter output to a specific PipeWire sink by node.name
    SetOutputTarget { node_name: String },
    /// Subscribe to state-change events (TUI stream)
    Subscribe,
    /// Stop the daemon
    Shutdown,
}

/// One of the two in-memory comparison slots for quick A/B auditioning.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AbSlot {
    A,
    B,
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

/// Serializable, UI-facing band filter type (collapses the DSP `FilterType`
/// variants — e.g. the several low-shelf flavours — into one canonical set).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BandType {
    Peaking,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    BandPass,
    Notch,
    AllPass,
}

impl BandType {
    /// Full human-readable name (used when the table is wide enough).
    pub fn full(self) -> &'static str {
        match self {
            BandType::Peaking => "Peaking",
            BandType::LowShelf => "Low Shelf",
            BandType::HighShelf => "High Shelf",
            BandType::LowPass => "Low Pass",
            BandType::HighPass => "High Pass",
            BandType::BandPass => "Band Pass",
            BandType::Notch => "Notch",
            BandType::AllPass => "All Pass",
        }
    }

    /// Short label for table columns.
    pub fn abbrev(self) -> &'static str {
        match self {
            BandType::Peaking => "PK",
            BandType::LowShelf => "LS",
            BandType::HighShelf => "HS",
            BandType::LowPass => "LP",
            BandType::HighPass => "HP",
            BandType::BandPass => "BP",
            BandType::Notch => "NO",
            BandType::AllPass => "AP",
        }
    }

    /// Cycle order used by the TUI `t` key.
    pub fn next(self) -> Self {
        match self {
            BandType::Peaking => BandType::LowShelf,
            BandType::LowShelf => BandType::HighShelf,
            BandType::HighShelf => BandType::LowPass,
            BandType::LowPass => BandType::HighPass,
            BandType::HighPass => BandType::Notch,
            BandType::Notch => BandType::AllPass,
            BandType::AllPass => BandType::BandPass,
            BandType::BandPass => BandType::Peaking,
        }
    }
}

impl From<FilterType> for BandType {
    fn from(t: FilterType) -> Self {
        match t {
            FilterType::Peaking => BandType::Peaking,
            FilterType::LowShelf | FilterType::LowShelf12Db | FilterType::LowShelfQ => {
                BandType::LowShelf
            }
            FilterType::HighShelf | FilterType::HighShelf12Db | FilterType::HighShelfQ => {
                BandType::HighShelf
            }
            FilterType::LowPass | FilterType::LowPassQ => BandType::LowPass,
            FilterType::HighPass | FilterType::HighPassQ => BandType::HighPass,
            FilterType::BandPass => BandType::BandPass,
            FilterType::Notch => BandType::Notch,
            FilterType::AllPass => BandType::AllPass,
        }
    }
}

impl From<BandType> for FilterType {
    fn from(t: BandType) -> Self {
        match t {
            BandType::Peaking => FilterType::Peaking,
            BandType::LowShelf => FilterType::LowShelf,
            BandType::HighShelf => FilterType::HighShelf,
            BandType::LowPass => FilterType::LowPassQ,
            BandType::HighPass => FilterType::HighPassQ,
            BandType::BandPass => FilterType::BandPass,
            BandType::Notch => FilterType::Notch,
            BandType::AllPass => FilterType::AllPass,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    Ok,
    State(DaemonState),
    PresetList(Vec<String>),
    /// Name of the profile a preset was imported as (reply to `ImportPreset`)
    Imported(String),
    /// List of output→profile mappings (output node.name, profile name)
    Mappings(Vec<(String, String)>),
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
    /// 16 spectrum bins (20 Hz–20 kHz, log-spaced), values 0.0–1.0 peak-normalised
    pub spectrum: Vec<f32>,
    /// Node name of the output device Resonance is currently feeding (if known)
    pub active_output: Option<String>,
    /// Profile mapped to the active output (auto-loaded), if any
    pub mapped_profile: Option<String>,
    /// All available PipeWire Audio/Sink node names (excluding Resonance itself)
    pub available_sinks: Vec<String>,
    /// The preferred output node name set by SetOutputTarget (if any)
    pub preferred_output: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BandState {
    pub band_type: BandType,
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

#[cfg(test)]
mod tests {
    use super::*;
    use postcard::{from_bytes, to_stdvec};

    /// Encode → decode → re-encode must be byte-identical for every command we send.
    fn command_round_trip(cmd: &Command) {
        let bytes = to_stdvec(cmd).expect("encode");
        let decoded: Command = from_bytes(&bytes).expect("decode");
        let re = to_stdvec(&decoded).expect("re-encode");
        assert_eq!(bytes, re, "round-trip mismatch for {cmd:?}");
    }

    #[test]
    fn commands_round_trip_through_postcard() {
        command_round_trip(&Command::GetState);
        command_round_trip(&Command::SetPower { enabled: true });
        command_round_trip(&Command::SetPreamp { db: -3.5 });
        command_round_trip(&Command::SetEffectIntensity {
            effect: FxEffectId::Surround,
            value: -0.5,
        });
        command_round_trip(&Command::AddBand {
            band_type: BandType::HighShelf,
            freq: 8000.0,
            gain_db: 4.5,
            q: 0.7,
        });
        command_round_trip(&Command::SetBandType {
            index: 2,
            band_type: BandType::LowPass,
        });
        command_round_trip(&Command::RemoveBand { index: 1 });
        command_round_trip(&Command::ImportPreset {
            path: "/tmp/rock.fac".into(),
            name: None,
        });
        command_round_trip(&Command::ImportPreset {
            path: "/tmp/rock.fac".into(),
            name: Some("Rock".into()),
        });
        command_round_trip(&Command::RenameProfile {
            from: "Rock".into(),
            to: "Rock Night".into(),
        });
        command_round_trip(&Command::SaveProfile {
            name: "night".into(),
        });
        command_round_trip(&Command::LoadProfile {
            name: "night".into(),
        });
        command_round_trip(&Command::MapOutput {
            profile: "night".into(),
        });
        command_round_trip(&Command::UnmapOutput);
        command_round_trip(&Command::ListMappings);
    }

    #[test]
    fn band_type_maps_to_filter_type_and_back() {
        for bt in [
            BandType::Peaking,
            BandType::LowShelf,
            BandType::HighShelf,
            BandType::LowPass,
            BandType::HighPass,
            BandType::BandPass,
            BandType::Notch,
            BandType::AllPass,
        ] {
            let ft: FilterType = bt.into();
            let back: BandType = ft.into();
            assert_eq!(bt, back, "band/filter type round-trip failed for {bt:?}");
        }
    }

    #[test]
    fn band_type_cycle_visits_all_eight() {
        let mut seen = std::collections::HashSet::new();
        let mut t = BandType::Peaking;
        for _ in 0..8 {
            assert!(seen.insert(t.abbrev()), "duplicate in cycle: {t:?}");
            t = t.next();
        }
        assert_eq!(t, BandType::Peaking, "cycle must return to start");
        assert_eq!(seen.len(), 8);
    }

    #[test]
    fn daemon_state_round_trips() {
        let st = DaemonState {
            enabled: true,
            preamp_db: 2.0,
            eq_enabled: true,
            bands: vec![BandState {
                band_type: BandType::Notch,
                freq: 1000.0,
                gain_db: 0.0,
                q: 8.0,
                enabled: true,
            }],
            effects: EffectsState {
                fidelity_intensity: 0.5,
                fidelity_enabled: true,
                ambience_intensity: 0.0,
                ambience_enabled: true,
                surround_intensity: -0.3,
                surround_enabled: true,
                dynamic_boost_intensity: 0.8,
                dynamic_boost_enabled: false,
                bass_intensity: -1.0,
                bass_enabled: true,
            },
            current_preset: Some("x.fac".into()),
            sample_rate: 48000.0,
            channels: 2,
            spectrum: vec![0.1, 0.2, 0.3],
            active_output: Some("alsa_output.pci".into()),
            mapped_profile: None,
            available_sinks: vec!["alsa_output.pci".into()],
            preferred_output: None,
        };
        let bytes = to_stdvec(&Response::State(st)).expect("encode");
        let _: Response = from_bytes(&bytes).expect("decode");
    }
}
