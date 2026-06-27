use resonance_dsp::chain::FxEffect;
use resonance_dsp::channel::{ChannelMask as DspMask, ChannelMatrix as DspMatrix};
use resonance_dsp::filter::FilterType;
use serde::{Deserialize, Serialize};

pub mod curve;
pub mod fr;
pub mod paths;
pub mod service;
pub mod transport;

pub const SOCKET_PATH_ENV: &str = "RESONANCE_SOCKET";
pub const DEFAULT_SOCKET_FILENAME: &str = "resonance.sock";

/// Serializable channel-targeting bitset — the wire/disk mirror of
/// `resonance_dsp::channel::ChannelMask` (kept here so resonance-dsp stays
/// serde-free, exactly as [`BandType`] mirrors the DSP `FilterType`).
/// `ALL` is the default and means "every channel", independent of count.
///
/// Serde note: the bitset is (de)serialized as an `i64` bit-cast of the `u64`,
/// **not** as a plain `u64`. TOML integers are signed 64-bit, so the default
/// `ALL` (`u64::MAX`) would overflow a `u64` field on save; `u64::MAX as i64`
/// is `-1`, which round-trips cleanly through TOML, JSON and postcard alike. A
/// hand-written impl is required because `skip_serializing_if` is unusable here
/// (postcard is not self-describing, so a skipped field corrupts the stream).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelMask(pub u64);

impl Serialize for ChannelMask {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(self.0 as i64)
    }
}

impl<'de> Deserialize<'de> for ChannelMask {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(ChannelMask(i64::deserialize(d)? as u64))
    }
}

impl ChannelMask {
    pub const ALL: ChannelMask = ChannelMask(u64::MAX);
    pub const NONE: ChannelMask = ChannelMask(0);

    pub fn single(ch: usize) -> Self {
        Self::from_dsp(DspMask::single(ch))
    }

    pub fn from_indices<I: IntoIterator<Item = usize>>(it: I) -> Self {
        Self::from_dsp(DspMask::from_indices(it))
    }

    pub fn contains(self, ch: usize) -> bool {
        self.to_dsp().contains(ch)
    }

    pub fn with(self, ch: usize) -> Self {
        Self::from_dsp(self.to_dsp().with(ch))
    }

    pub fn without(self, ch: usize) -> Self {
        Self::from_dsp(self.to_dsp().without(ch))
    }

    /// True when every channel in `0..channels` is selected (a global band).
    pub fn is_global(self, channels: usize) -> bool {
        self.to_dsp().is_global(channels)
    }

    pub fn to_dsp(self) -> DspMask {
        DspMask::from_bits(self.0)
    }

    pub fn from_dsp(m: DspMask) -> Self {
        ChannelMask(m.bits())
    }
}

impl Default for ChannelMask {
    fn default() -> Self {
        ChannelMask::ALL
    }
}

/// Serializable channel routing/remap matrix — the wire mirror of
/// `resonance_dsp::channel::ChannelMatrix` (`out_ch × in_ch`, row-major gains).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingMatrix {
    pub in_ch: usize,
    pub out_ch: usize,
    /// Row-major, length `out_ch * in_ch`: `gains[o * in_ch + i]`.
    pub gains: Vec<f64>,
}

impl RoutingMatrix {
    /// Validate + convert to the DSP matrix. `None` if dimensions are bad.
    pub fn to_dsp(&self) -> Option<DspMatrix> {
        DspMatrix::new(self.in_ch, self.out_ch, self.gains.clone())
    }

    pub fn from_dsp(m: &DspMatrix) -> Self {
        Self {
            in_ch: m.in_ch(),
            out_ch: m.out_ch(),
            gains: m.gains().to_vec(),
        }
    }

    pub fn identity(channels: usize) -> Self {
        Self::from_dsp(&DspMatrix::identity(channels))
    }

    pub fn swap(channels: usize, a: usize, b: usize) -> Self {
        Self::from_dsp(&DspMatrix::swap(channels, a, b))
    }
}

/// Standard channel position labels for a given count (WAVE / PipeWire order),
/// for UI display and APO `Channel:` mapping. Unknown counts fall back to
/// `CH0..CHn`.
pub fn default_channel_layout(channels: usize) -> Vec<String> {
    let labels: &[&str] = match channels {
        0 => &[],
        1 => &["MONO"],
        2 => &["FL", "FR"],
        3 => &["FL", "FR", "FC"],
        4 => &["FL", "FR", "RL", "RR"],
        5 => &["FL", "FR", "FC", "RL", "RR"],
        6 => &["FL", "FR", "FC", "LFE", "RL", "RR"],
        7 => &["FL", "FR", "FC", "LFE", "RC", "SL", "SR"],
        8 => &["FL", "FR", "FC", "LFE", "RL", "RR", "SL", "SR"],
        _ => return (0..channels).map(|i| format!("CH{i}")).collect(),
    };
    labels.iter().map(|s| s.to_string()).collect()
}

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
    /// Replace the whole editable chain state at once (used by TUI undo/redo).
    ApplyState {
        preamp_db: f64,
        enabled: bool,
        bands: Vec<BandState>,
        effects: EffectsState,
    },
    /// Reset to defaults: flat EQ (no bands), all effects off, 0 dB preamp
    Reset,
    /// Export the current EQ (preamp + bands) to an EqualizerAPO `.txt` file
    ExportApo { path: String },
    /// Export the full current chain state to our native `.toml` profile format
    /// at an arbitrary path (round-trips via `ImportPreset` on a `.toml` file)
    ExportProfile { path: String },
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
    /// List available preset files. `dir` = a specific directory, or None to
    /// scan the XDG preset library + system dirs.
    ListPresets { dir: Option<String> },
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
    /// Map a specific output device (by node.name) to a profile
    MapOutputFor { node_name: String, profile: String },
    /// Remove the mapping for a specific output device (by node.name)
    UnmapOutputFor { node_name: String },
    /// Forget a remembered output device: drop it from the known-sinks registry
    /// and its mapping. It re-appears when next plugged in / used as output.
    ForgetSink { node_name: String },
    /// List all output→profile mappings
    ListMappings,
    /// Route the filter output to a specific output device by name (pins it).
    SetOutputTarget { node_name: String },
    /// Stop pinning an output and follow the OS default output device instead
    /// (auto-switch when the default changes, e.g. plugging headphones).
    FollowSystemOutput,
    /// Stop the daemon
    Shutdown,
    // ── N-channel commands ───────────────────────────────────────────────────
    // IMPORTANT: keep these LAST. postcard encodes enum variants by ordinal with
    // no names, and the IPC wire is unversioned, so inserting a variant mid-enum
    // shifts every later variant's ordinal and silently misdecodes commands from
    // a mismatched (older) client. New variants must always be appended here.
    /// Restrict (or widen) which channels an existing band applies to.
    /// `ChannelMask::ALL` makes the band global again (the default).
    SetBandChannels { index: usize, channels: ChannelMask },
    /// Set the output routing/remap matrix. In-graph (PipeWire) only a square
    /// remap at the current channel count is applied; up/downmix is daemon-path.
    SetChannelRouting { matrix: RoutingMatrix },
    /// Swap two output channels — convenience over a swap routing matrix, built
    /// by the daemon at the current channel count.
    SwapChannels { a: usize, b: usize },
    /// Clear routing: straight passthrough at the processing channel count.
    ClearRouting,
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

impl FxEffectId {
    /// Every effect, in chain order. Adding a variant forces the array to be
    /// updated, which propagates exhaustively to every `ALL` iteration.
    pub const ALL: [FxEffectId; 5] = [
        FxEffectId::Fidelity,
        FxEffectId::Ambience,
        FxEffectId::Surround,
        FxEffectId::DynamicBoost,
        FxEffectId::Bass,
    ];

    /// Full display name. (The TUI keeps its own narrower labels for the effects
    /// column; everything else should use this.)
    pub fn label(self) -> &'static str {
        match self {
            FxEffectId::Fidelity => "Fidelity",
            FxEffectId::Ambience => "Ambience",
            FxEffectId::Surround => "Surround",
            FxEffectId::DynamicBoost => "Dynamic Boost",
            FxEffectId::Bass => "Bass",
        }
    }

    /// Bipolar effects (Surround, Bass) accept negative intensity (narrow / cut)
    /// down to −1; the rest are 0..1. This was duplicated — and had drifted —
    /// across the TUI (index `2|4`) and GUI (`matches!`) before.
    pub fn is_bipolar(self) -> bool {
        matches!(self, FxEffectId::Surround | FxEffectId::Bass)
    }

    /// Minimum intensity: −1 for bipolar effects, 0 otherwise.
    pub fn min(self) -> f64 {
        if self.is_bipolar() { -1.0 } else { 0.0 }
    }
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

// `State(DaemonState)` is large and the by-far most common reply; boxing it
// would add an allocation to the hot path for no real memory win (a Response is
// short-lived and never held in bulk).
#[allow(clippy::large_enum_variant)]
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonState {
    pub enabled: bool,
    pub preamp_db: f64,
    pub eq_enabled: bool,
    pub bands: Vec<BandState>,
    pub effects: EffectsState,
    pub current_preset: Option<String>,
    /// The live DSP/playback sample rate the audio path is running at (Hz).
    pub sample_rate: f64,
    /// The capture-side sample rate (Hz). Differs from `sample_rate` only when a
    /// backend is resampling the capture clock to the DSP/output clock (e.g. a
    /// macOS tap clocked differently from the output device). Equal to
    /// `sample_rate` on the no-resample path (Linux/PipeWire in-graph filter).
    pub capture_rate: f64,
    /// Channel count the DSP chain *processes* (capture/input width).
    pub channels: usize,
    /// Channel count the chain *emits* after routing. Equals `channels` with no
    /// remap; differs when a routing matrix up/downmixes.
    #[serde(default)]
    pub out_channels: usize,
    /// Position labels for the processing channels (e.g. `["FL","FR","FC",…]`),
    /// for UI display and per-channel band targeting.
    #[serde(default)]
    pub channel_layout: Vec<String>,
    /// Active output routing matrix, if any (None = identity passthrough).
    #[serde(default)]
    pub routing: Option<RoutingMatrix>,
    /// 16 spectrum bins (20 Hz–20 kHz, log-spaced), values 0.0–1.0 peak-normalised
    pub spectrum: Vec<f32>,
    /// Node name of the output device Resonance is currently feeding (if known)
    pub active_output: Option<String>,
    /// Profile mapped to the active output (auto-loaded), if any
    pub mapped_profile: Option<String>,
    /// All available PipeWire Audio/Sink node names (excluding Resonance itself)
    pub available_sinks: Vec<String>,
    /// Friendly `node.description` per sink, as `(node_name, description)` pairs.
    /// Commands still key by `node_name`; this is purely for user-facing display.
    pub sink_descriptions: Vec<(String, String)>,
    /// The preferred output node name set by SetOutputTarget (if any)
    pub preferred_output: Option<String>,
    /// Live level + DSP-load meters.
    pub meters: Meters,
}

impl DaemonState {
    /// User-facing label for a sink `node.name`: its `node.description` if known,
    /// else the last dot-segment of the node name, else `(default)` when empty.
    pub fn sink_label(&self, node: &str) -> String {
        if node.is_empty() {
            return "(default)".to_string();
        }
        self.sink_descriptions
            .iter()
            .find(|(name, _)| name == node)
            .map(|(_, desc)| desc.clone())
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| node.rsplit('.').next().unwrap_or(node).to_string())
    }
}

/// Live level + performance meters sampled on the audio RT thread.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct Meters {
    /// Pre-DSP peak (linear, 0–1+), max over both channels of the last block.
    pub in_peak: f32,
    /// Post-DSP peak (linear, 0–1+).
    pub out_peak: f32,
    /// Pre-DSP RMS (linear).
    pub in_rms: f32,
    /// Post-DSP RMS (linear).
    pub out_rms: f32,
    /// Output reached/exceeded 0 dBFS since the last snapshot (latched, then cleared).
    pub clip: bool,
    /// DSP time as a fraction of the block's real-time budget (0–1+; >1 = xrun risk).
    pub dsp_load: f32,
    /// Last `ProcessorChain::process` duration in microseconds.
    pub dsp_frame_us: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BandState {
    pub band_type: BandType,
    pub freq: f64,
    pub gain_db: f64,
    pub q: f64,
    pub enabled: bool,
    /// Which channels this band applies to. `#[serde(default)]` makes older
    /// profile `.toml`/JSON files (self-describing formats) that omit the field
    /// load as `ChannelMask::ALL`, a global band. NOTE: this does **not** give
    /// postcard-wire back-compat — postcard is non-self-describing and reads a
    /// fixed field count, so the IPC wire is version-locked to the daemon build
    /// (clients + daemon ship together).
    #[serde(default)]
    pub channels: ChannelMask,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

impl EffectsState {
    /// `(intensity, enabled)` for one effect — the single place the flat fields
    /// map to the effect enum, so iterating `FxEffectId::ALL` needs no unroll.
    pub fn get(&self, id: FxEffectId) -> (f64, bool) {
        match id {
            FxEffectId::Fidelity => (self.fidelity_intensity, self.fidelity_enabled),
            FxEffectId::Ambience => (self.ambience_intensity, self.ambience_enabled),
            FxEffectId::Surround => (self.surround_intensity, self.surround_enabled),
            FxEffectId::DynamicBoost => (self.dynamic_boost_intensity, self.dynamic_boost_enabled),
            FxEffectId::Bass => (self.bass_intensity, self.bass_enabled),
        }
    }

    pub fn set(&mut self, id: FxEffectId, intensity: f64, enabled: bool) {
        match id {
            FxEffectId::Fidelity => {
                self.fidelity_intensity = intensity;
                self.fidelity_enabled = enabled;
            }
            FxEffectId::Ambience => {
                self.ambience_intensity = intensity;
                self.ambience_enabled = enabled;
            }
            FxEffectId::Surround => {
                self.surround_intensity = intensity;
                self.surround_enabled = enabled;
            }
            FxEffectId::DynamicBoost => {
                self.dynamic_boost_intensity = intensity;
                self.dynamic_boost_enabled = enabled;
            }
            FxEffectId::Bass => {
                self.bass_intensity = intensity;
                self.bass_enabled = enabled;
            }
        }
    }
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
        command_round_trip(&Command::ApplyState {
            preamp_db: -2.0,
            enabled: true,
            bands: vec![BandState {
                band_type: BandType::Peaking,
                freq: 1000.0,
                gain_db: 3.0,
                q: 1.41,
                enabled: true,
                channels: ChannelMask::single(0),
            }],
            effects: EffectsState {
                fidelity_intensity: 0.5,
                fidelity_enabled: true,
                ambience_intensity: 0.0,
                ambience_enabled: false,
                surround_intensity: 0.0,
                surround_enabled: false,
                dynamic_boost_intensity: 0.0,
                dynamic_boost_enabled: false,
                bass_intensity: 0.0,
                bass_enabled: false,
            },
        });
        command_round_trip(&Command::SetBandChannels {
            index: 1,
            channels: ChannelMask::from_indices([0, 2, 4]),
        });
        command_round_trip(&Command::SetChannelRouting {
            matrix: RoutingMatrix::swap(2, 0, 1),
        });
        command_round_trip(&Command::SwapChannels { a: 0, b: 1 });
        command_round_trip(&Command::ClearRouting);
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
    fn channel_mask_wire_mirrors_dsp() {
        // The serde mirror and the DSP type must agree on membership semantics.
        let m = ChannelMask::from_indices([0, 3, 5]);
        assert!(m.contains(0) && m.contains(3) && m.contains(5) && !m.contains(1));
        assert_eq!(m.to_dsp().bits(), m.0);
        assert!(ChannelMask::ALL.is_global(8));
        assert!(!m.is_global(8));
        // round-trips through postcard
        let bytes = to_stdvec(&m).unwrap();
        let back: ChannelMask = from_bytes(&bytes).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn routing_matrix_wire_round_trips_to_dsp() {
        // 2→1 downmix matrix, built as a literal (fields are public).
        let rm = RoutingMatrix {
            in_ch: 2,
            out_ch: 1,
            gains: vec![0.5, 0.5],
        };
        let dsp = rm.to_dsp().expect("valid dims");
        assert_eq!(RoutingMatrix::from_dsp(&dsp), rm);
        // identity + swap helpers build valid matrices
        assert!(RoutingMatrix::identity(6).to_dsp().is_some());
        assert!(RoutingMatrix::swap(2, 0, 1).to_dsp().is_some());
        // bad dimensions are rejected
        assert!(
            RoutingMatrix {
                in_ch: 2,
                out_ch: 2,
                gains: vec![1.0],
            }
            .to_dsp()
            .is_none()
        );
    }

    #[test]
    fn channel_mask_defaults_to_all() {
        // `BandState.channels` uses `#[serde(default)]`; the default must be the
        // global mask so pre-per-channel profiles load as global bands. (The
        // disk-format back-compat is covered in the daemon's config tests, where
        // a real `.toml` without the field is round-tripped.)
        assert_eq!(ChannelMask::default(), ChannelMask::ALL);
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
                channels: ChannelMask::single(1),
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
            capture_rate: 48000.0,
            channels: 2,
            out_channels: 2,
            channel_layout: default_channel_layout(2),
            routing: Some(RoutingMatrix::swap(2, 0, 1)),
            spectrum: vec![0.1, 0.2, 0.3],
            active_output: Some("alsa_output.pci".into()),
            mapped_profile: None,
            available_sinks: vec!["alsa_output.pci".into()],
            sink_descriptions: vec![("alsa_output.pci".into(), "Built-in Audio".into())],
            preferred_output: None,
            meters: Meters::default(),
        };
        let bytes = to_stdvec(&Response::State(st)).expect("encode");
        let _: Response = from_bytes(&bytes).expect("decode");
    }

    #[test]
    fn default_channel_layout_all_counts() {
        let lbl = |n| -> Vec<String> { default_channel_layout(n) };
        assert!(lbl(0).is_empty());
        assert_eq!(lbl(1), vec!["MONO"]);
        assert_eq!(lbl(2), vec!["FL", "FR"]);
        assert_eq!(lbl(3), vec!["FL", "FR", "FC"]);
        assert_eq!(lbl(4), vec!["FL", "FR", "RL", "RR"]);
        assert_eq!(lbl(6), vec!["FL", "FR", "FC", "LFE", "RL", "RR"]);
        assert_eq!(
            lbl(8),
            vec!["FL", "FR", "FC", "LFE", "RL", "RR", "SL", "SR"]
        );
        // Beyond standard layouts → CHn fallback.
        let n9 = lbl(9);
        assert_eq!(n9.len(), 9);
        assert_eq!(n9[0], "CH0");
        assert_eq!(n9[8], "CH8");
    }

    #[test]
    fn channel_mask_high_bits_survive_postcard() {
        // The i64 bit-cast must preserve the high bits (ALL, bit 63) losslessly.
        for m in [
            ChannelMask::ALL,
            ChannelMask::NONE,
            ChannelMask::single(0),
            ChannelMask::single(63),
            ChannelMask::from_indices([0, 7, 31, 62]),
        ] {
            let bytes = to_stdvec(&m).unwrap();
            let back: ChannelMask = from_bytes(&bytes).unwrap();
            assert_eq!(m, back, "mask {:#x} must round-trip", m.0);
        }
    }

    #[test]
    fn routing_matrix_rectangular_to_dsp() {
        // Valid non-square (down/upmix) accepted; wrong gain count / zero dims rejected.
        assert!(
            RoutingMatrix {
                in_ch: 4,
                out_ch: 2,
                gains: vec![0.0; 8]
            }
            .to_dsp()
            .is_some()
        );
        assert!(
            RoutingMatrix {
                in_ch: 2,
                out_ch: 4,
                gains: vec![0.0; 8]
            }
            .to_dsp()
            .is_some()
        );
        assert!(
            RoutingMatrix {
                in_ch: 3,
                out_ch: 3,
                gains: vec![0.0; 8]
            }
            .to_dsp()
            .is_none(),
            "wrong gain count rejected"
        );
        assert!(
            RoutingMatrix {
                in_ch: 0,
                out_ch: 2,
                gains: vec![]
            }
            .to_dsp()
            .is_none(),
            "zero dim rejected"
        );
    }

    #[test]
    fn response_variants_round_trip() {
        for r in [
            Response::Ok,
            Response::PresetList(vec!["a".into(), "b".into()]),
            Response::Imported("rock".into()),
            Response::Mappings(vec![("dev".into(), "prof".into())]),
            Response::Error("boom".into()),
        ] {
            let bytes = to_stdvec(&r).expect("encode");
            let _: Response = from_bytes(&bytes).expect("decode");
        }
    }
}
