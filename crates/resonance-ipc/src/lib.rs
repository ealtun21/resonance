use resonance_dsp::chain::FxEffect;
use resonance_dsp::channel::{ChannelMask as DspMask, ChannelMatrix as DspMatrix};
use resonance_dsp::filter::{BandScope as DspScope, DynParams, FilterType};
use serde::{Deserialize, Serialize};

pub mod curve;
pub mod fr;
pub mod paths;
pub mod service;
pub mod singleton;
pub mod transport;
pub mod tray;

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

    #[must_use]
    pub fn single(ch: usize) -> Self {
        Self::from_dsp(DspMask::single(ch))
    }

    pub fn from_indices<I: IntoIterator<Item = usize>>(it: I) -> Self {
        Self::from_dsp(DspMask::from_indices(it))
    }

    #[must_use]
    pub fn contains(self, ch: usize) -> bool {
        self.to_dsp().contains(ch)
    }

    #[must_use]
    pub fn with(self, ch: usize) -> Self {
        Self::from_dsp(self.to_dsp().with(ch))
    }

    #[must_use]
    pub fn without(self, ch: usize) -> Self {
        Self::from_dsp(self.to_dsp().without(ch))
    }

    /// True when every channel in `0..channels` is selected (a global band).
    #[must_use]
    pub fn is_global(self, channels: usize) -> bool {
        self.to_dsp().is_global(channels)
    }

    #[must_use]
    pub fn to_dsp(self) -> DspMask {
        DspMask::from_bits(self.0)
    }

    #[must_use]
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
    #[must_use]
    pub fn to_dsp(&self) -> Option<DspMatrix> {
        DspMatrix::new(self.in_ch, self.out_ch, self.gains.clone())
    }

    #[must_use]
    pub fn from_dsp(m: &DspMatrix) -> Self {
        Self {
            in_ch: m.in_ch(),
            out_ch: m.out_ch(),
            gains: m.gains().to_vec(),
        }
    }

    #[must_use]
    pub fn identity(channels: usize) -> Self {
        Self::from_dsp(&DspMatrix::identity(channels))
    }

    #[must_use]
    pub fn swap(channels: usize, a: usize, b: usize) -> Self {
        Self::from_dsp(&DspMatrix::swap(channels, a, b))
    }
}

/// Standard channel position labels for a given count (WAVE / `PipeWire` order),
/// for UI display and APO `Channel:` mapping. Unknown counts fall back to
/// `CH0..CHn`.
#[must_use]
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
    labels
        .iter()
        .map(std::string::ToString::to_string)
        .collect()
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
    /// Set an `FxEffect` intensity (0.0–1.0)
    SetEffectIntensity { effect: FxEffectId, value: f64 },
    /// Enable or disable a specific `FxEffect`
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
    /// Export the current EQ (preamp + bands) to an `EqualizerAPO` `.txt` file
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
    /// Copy a saved profile to a new name (does NOT apply it)
    DuplicateProfile { from: String, to: String },
    /// Export a *named* saved profile (not the current chain) to a `.toml` file
    ExportProfileNamed { name: String, path: String },
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
    /// Set the output routing/remap matrix. In-graph (`PipeWire`) only a square
    /// remap at the current channel count is applied; up/downmix is daemon-path.
    SetChannelRouting { matrix: RoutingMatrix },
    /// Swap two output channels — convenience over a swap routing matrix, built
    /// by the daemon at the current channel count.
    SwapChannels { a: usize, b: usize },
    /// Clear routing: straight passthrough at the processing channel count.
    ClearRouting,
    // ── Per-app commands (append-only, see the N-channel note above) ──────────
    /// Set a per-application stream's linear volume (0.0–4.0; 1.0 = unity,
    /// values above 1.0 = boost). Backends clamp to what they support (e.g.
    /// Windows caps at 1.0). `key` matches an `AppStream::key` from
    /// `DaemonState::apps`.
    SetAppVolume { key: String, volume: f64 },
    /// Mute or unmute a per-application stream by its `AppStream::key`.
    SetAppMute { key: String, muted: bool },
    /// Set an output sink's volume (0.0–1.0, perceptual — matches the system
    /// mixer). `name` matches a `SinkVolume::name` from `DaemonState::sinks`.
    SetSinkVolume { name: String, volume: f64 },
    /// Mute or unmute an output sink by its `SinkVolume::name`.
    SetSinkMute { name: String, muted: bool },
    /// Set (or clear) the output dither target bit depth. `None` = off (bit-exact);
    /// `Some(16 | 20 | 24)` = TPDF-dither to that grid as the final DSP stage.
    SetDither { bits: Option<u32> },
    /// Set an EQ band's filter slope in dB/oct (12/24/48). Applies to shelves +
    /// HP/LP; ignored by single-biquad band types. `index` matches a band's
    /// position in `DaemonState::bands`.
    SetBandSlope { index: usize, slope_db_oct: u8 },
    /// Set an EQ band's stereo scope (Stereo/Mid/Side). `index` matches a band's
    /// position in `DaemonState::bands`.
    SetBandScope { index: usize, scope: BandScope },
    /// Load a WAV impulse response into the convolution stage (room/speaker
    /// correction, HRTF). The daemon reads the file, resamples it to the DSP
    /// rate and swaps the prepared kernel in; the stage arms on success.
    SetConvolutionIr { path: String },
    /// Drop the convolution IR entirely (passthrough, zero added latency).
    ClearConvolutionIr,
    /// Bypass or re-arm the convolution stage without dropping the loaded IR.
    SetConvolutionEnabled { enabled: bool },
    /// Return the freshest post-DSP output samples the daemon has buffered
    /// (mono, channel-averaged — the spectrum feed). Powers the `resonance
    /// verify` live audio-path harness.
    CaptureOutput { frames: u32 },
    /// Set (or clear) an EQ band's dynamic EQ (level-driven gain morph).
    /// Peaking bands only — the daemon rejects other types. `index` matches a
    /// band's position in `DaemonState::bands`.
    SetBandDynamics {
        index: usize,
        dynamics: Option<BandDynamics>,
    },
    /// Switch the EQ between minimum phase (biquads, zero latency — the
    /// default) and linear phase (static stereo bands rendered to a symmetric
    /// FIR; adds `eq_fir_latency_frames` of delay, no phase rotation).
    SetPhaseMode { linear: bool },
    /// Transiently audition a single EQ band. `Some(index)` auditions that band
    /// in `mode` (Solo = bypass others; Listen = band-pass the band's region);
    /// `None` clears. Never persisted; suspends linear-phase while active. The
    /// daemon clears any active audition on any band-mutating command as a
    /// stuck-audio guard.
    SetBandAudition {
        index: Option<usize>,
        mode: AuditionMode,
    },
}

/// Per-band audition mode (mirrors `resonance_dsp::chain::AuditionMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditionMode {
    Solo,
    Listen,
}

/// A transient single-band audition (mirrors `resonance_dsp::chain::BandAudition`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BandAudition {
    pub band: usize,
    pub mode: AuditionMode,
}

/// One of the two in-memory comparison slots for quick A/B auditioning.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AbSlot {
    A,
    B,
}

/// Serializable mirror of `FxEffect` (avoids depending on resonance-dsp in serde derives)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FxEffectId {
    Fidelity,
    Ambience,
    Surround,
    DynamicBoost,
    Bass,
    Loudness,
    Crossfeed,
}

impl FxEffectId {
    /// Every effect, in chain order. Adding a variant forces the array to be
    /// updated, which propagates exhaustively to every `ALL` iteration.
    pub const ALL: [FxEffectId; 7] = [
        FxEffectId::Fidelity,
        FxEffectId::Ambience,
        FxEffectId::Surround,
        FxEffectId::DynamicBoost,
        FxEffectId::Bass,
        FxEffectId::Loudness,
        FxEffectId::Crossfeed,
    ];

    /// Full display name. (The TUI keeps its own narrower labels for the effects
    /// column; everything else should use this.)
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            FxEffectId::Fidelity => "Fidelity",
            FxEffectId::Ambience => "Ambience",
            FxEffectId::Surround => "Surround",
            FxEffectId::DynamicBoost => "Dynamic Boost",
            FxEffectId::Bass => "Bass",
            FxEffectId::Loudness => "Loudness",
            FxEffectId::Crossfeed => "Crossfeed",
        }
    }

    /// Bipolar effects (Surround, Bass) accept negative intensity (narrow / cut)
    /// down to −1; the rest are 0..1. This was duplicated — and had drifted —
    /// across the TUI (index `2|4`) and GUI (`matches!`) before.
    #[must_use]
    pub fn is_bipolar(self) -> bool {
        matches!(self, FxEffectId::Surround | FxEffectId::Bass)
    }

    /// Minimum intensity: −1 for bipolar effects, 0 otherwise.
    #[must_use]
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
            FxEffectId::Loudness => FxEffect::Loudness,
            FxEffectId::Crossfeed => FxEffect::Crossfeed,
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
    #[must_use]
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
    #[must_use]
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
    #[must_use]
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

    /// Whether the per-band filter slope ([`BandState::slope_db_oct`]) is
    /// meaningful for this type. Only shelves and high/low-pass filters build
    /// cascaded sections for steeper slopes; the single-biquad types (peaking,
    /// notch, band-pass, all-pass) ignore the slope. Front-ends gate their slope
    /// control on this.
    #[must_use]
    pub fn uses_slope(self) -> bool {
        matches!(
            self,
            BandType::LowShelf | BandType::HighShelf | BandType::LowPass | BandType::HighPass
        )
    }

    /// Whether dynamic EQ ([`BandState::dynamics`]) is available for this type.
    /// Peaking only (v1): its gain-only coefficient morph is cheap and it
    /// covers the real use cases (de-essing, resonance taming). Front-ends
    /// gate their dynamics control on this.
    #[must_use]
    pub fn uses_dynamics(self) -> bool {
        matches!(self, BandType::Peaking)
    }
}

/// Cycle a filter slope through the supported values 12 → 24 → 48 → 12 dB/oct.
/// Any unexpected value snaps back to 12. Shared by the front-ends' slope
/// controls.
#[must_use]
pub fn next_slope_db_oct(slope_db_oct: u8) -> u8 {
    match slope_db_oct {
        12 => 24,
        24 => 48,
        _ => 12,
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

/// Serializable stereo scope of a band — the wire/disk mirror of the DSP
/// `resonance_dsp::filter::BandScope`. `Stereo` (default) processes each channel
/// independently; `Mid`/`Side` process the mono sum / stereo difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BandScope {
    #[default]
    Stereo,
    Mid,
    Side,
}

impl BandScope {
    /// Short label for table columns.
    #[must_use]
    pub fn abbrev(self) -> &'static str {
        match self {
            BandScope::Stereo => "St",
            BandScope::Mid => "M",
            BandScope::Side => "S",
        }
    }

    /// Full human-readable name.
    #[must_use]
    pub fn full(self) -> &'static str {
        match self {
            BandScope::Stereo => "Stereo",
            BandScope::Mid => "Mid",
            BandScope::Side => "Side",
        }
    }

    /// Cycle order for a UI toggle: Stereo → Mid → Side → Stereo.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            BandScope::Stereo => BandScope::Mid,
            BandScope::Mid => BandScope::Side,
            BandScope::Side => BandScope::Stereo,
        }
    }
}

impl From<BandScope> for DspScope {
    fn from(s: BandScope) -> Self {
        match s {
            BandScope::Stereo => DspScope::Stereo,
            BandScope::Mid => DspScope::Mid,
            BandScope::Side => DspScope::Side,
        }
    }
}

impl From<DspScope> for BandScope {
    fn from(s: DspScope) -> Self {
        match s {
            DspScope::Stereo => BandScope::Stereo,
            DspScope::Mid => BandScope::Mid,
            DspScope::Side => BandScope::Side,
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
    /// Raw post-DSP capture (reply to `CaptureOutput`): the DSP rate the
    /// samples were produced at, plus the freshest mono samples oldest-first.
    /// May be shorter than requested while the buffer is still filling; empty
    /// on platforms where the daemon owns no audio path (Windows/APO).
    /// Appended LAST — postcard encodes variants by ordinal (see `Command`).
    Capture {
        rate: f64,
        samples: Vec<f32>,
    },
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
    /// EQ phase behaviour: true = linear phase (FIR bank), false = minimum
    /// phase (biquads, the default).
    #[serde(default)]
    pub phase_mode_linear: bool,
    /// Added latency of the linear-phase realisation, in frames at
    /// `sample_rate` (0 when the mode is off / no kernel loaded).
    #[serde(default)]
    pub eq_fir_latency_frames: usize,
    /// Node name of the output device Resonance is currently feeding (if known)
    pub active_output: Option<String>,
    /// Profile mapped to the active output (auto-loaded), if any
    pub mapped_profile: Option<String>,
    /// All available `PipeWire` Audio/Sink node names (excluding Resonance itself)
    pub available_sinks: Vec<String>,
    /// Friendly `node.description` per sink, as `(node_name, description)` pairs.
    /// Commands still key by `node_name`; this is purely for user-facing display.
    pub sink_descriptions: Vec<(String, String)>,
    /// The preferred output node name set by `SetOutputTarget` (if any)
    pub preferred_output: Option<String>,
    /// Live level + DSP-load meters.
    pub meters: Meters,
    /// Per-application audio streams the daemon can control (volume/mute).
    /// Empty when the backend doesn't enumerate apps. Appended LAST + `serde`
    /// default so self-describing profiles/older readers stay compatible; the
    /// postcard IPC wire is version-locked regardless (clients ship together).
    #[serde(default)]
    pub apps: Vec<AppStream>,
    /// Output sinks (devices) the daemon can control (volume/mute). Empty when
    /// the backend doesn't enumerate sink volumes. Appended LAST + `serde`
    /// default, same compatibility note as `apps`.
    #[serde(default)]
    pub sinks: Vec<SinkVolume>,
    /// Output dither target bit depth (`None` = off). Appended LAST + `serde`
    /// default, same compatibility note as `apps`/`sinks`.
    #[serde(default)]
    pub dither_bits: Option<u32>,
    /// Convolution stage status (`None` = no IR loaded). Appended LAST +
    /// `serde` default, same compatibility note as `apps`/`sinks`.
    #[serde(default)]
    pub convolution: Option<ConvolutionState>,
    /// Transient per-band audition (`None` = none). Runtime-only — never
    /// persisted; published so clients render the active toggle + mode. Appended
    /// LAST + `serde` default, same compatibility note as `apps`/`sinks`.
    #[serde(default)]
    pub audition: Option<BandAudition>,
}

/// Status of the convolution/IR stage, for `status` output and the UIs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConvolutionState {
    /// Source WAV path (used to restore the IR from saved profiles).
    pub path: String,
    /// Display name (file stem).
    pub name: String,
    /// Native sample rate of the IR file (Hz).
    pub ir_sample_rate: f64,
    /// Channel count of the IR file (1 = applied to every audio channel).
    pub ir_channels: usize,
    /// Taps actually convolved at the DSP rate (after resample + length cap).
    pub taps: usize,
    /// Fixed added latency in frames at the DSP rate (0 while bypassed).
    pub latency_frames: usize,
    /// False = loaded but bypassed (`SetConvolutionEnabled`).
    pub enabled: bool,
}

impl DaemonState {
    /// User-facing label for a sink `node.name`: its `node.description` if known,
    /// else the last dot-segment of the node name, else `(default)` when empty.
    #[must_use]
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
    /// Filter slope in dB/oct — 12 (default, single biquad), 24, or 48. Applies
    /// to shelves + HP/LP; ignored by the single-biquad band types. `serde`
    /// default keeps profiles written before slopes existed loading as 12.
    #[serde(default = "default_slope_db_oct")]
    pub slope_db_oct: u8,
    /// Stereo scope: `Stereo` (default), `Mid`, or `Side`. `serde` default keeps
    /// profiles written before mid/side existed loading as `Stereo`.
    #[serde(default)]
    pub scope: BandScope,
    /// Dynamic EQ (level-driven gain morph), `None` = static band. Peaking
    /// bands only. `serde` default keeps profiles written before dynamics
    /// existed loading as static bands (postcard wire stays version-locked —
    /// see the `channels` note above).
    #[serde(default)]
    pub dynamics: Option<BandDynamics>,
}

/// Per-band dynamic EQ parameters — the wire/disk mirror of the DSP
/// `resonance_dsp::filter::DynParams`. The band's gain morphs from `gain_db`
/// toward `gain_db + range_db` as in-band level rises past `threshold_db`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BandDynamics {
    /// Detector level (dBFS) where the morph starts.
    pub threshold_db: f64,
    /// Signed max gain offset: negative = cut when loud, positive = boost.
    pub range_db: f64,
    /// Detector attack time constant (ms).
    pub attack_ms: f64,
    /// Detector release time constant (ms).
    pub release_ms: f64,
}

impl BandDynamics {
    /// Front-end starting point when enabling dynamics on a band (mirrors
    /// `DynParams::DEFAULT`).
    pub const DEFAULT: Self = Self {
        threshold_db: -30.0,
        range_db: -6.0,
        attack_ms: 5.0,
        release_ms: 150.0,
    };
}

impl From<BandDynamics> for DynParams {
    fn from(d: BandDynamics) -> Self {
        Self {
            threshold_db: d.threshold_db,
            range_db: d.range_db,
            attack_ms: d.attack_ms,
            release_ms: d.release_ms,
        }
    }
}

impl From<DynParams> for BandDynamics {
    fn from(d: DynParams) -> Self {
        Self {
            threshold_db: d.threshold_db,
            range_db: d.range_db,
            attack_ms: d.attack_ms,
            release_ms: d.release_ms,
        }
    }
}

/// Serde default for [`BandState::slope_db_oct`] — 12 dB/oct (single biquad),
/// the pre-slope behaviour.
#[must_use]
pub fn default_slope_db_oct() -> u8 {
    12
}

/// One application's audio stream that the daemon can volume/mute independently.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppStream {
    /// Stable identity across polls; backend-specific (`PipeWire` binary+serial,
    /// Windows session id, macOS bundle id / pid). Commands key off this.
    pub key: String,
    /// Human-facing name (`application.name`, session display name, app name).
    pub display_name: String,
    /// OS process id, when the backend knows it.
    pub pid: Option<u32>,
    /// Linear gain, `0.0..=4.0` (`1.0` = unity, `>1` = boost where supported).
    pub volume: f64,
    pub muted: bool,
    /// Currently producing audio (vs. listed but idle).
    pub active: bool,
}

/// One output sink (device) the daemon can volume/mute.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SinkVolume {
    /// Stable identity (`PipeWire node.name`); commands key off this.
    pub name: String,
    /// Human-facing device name (`node.description`), for display.
    pub description: String,
    /// Volume `0.0..=1.0`, perceptual (matches the system mixer's %).
    pub volume: f64,
    pub muted: bool,
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
    // `#[serde(default)]` so self-describing profiles written before Loudness
    // existed still load (default off). The postcard IPC wire is version-locked
    // regardless — clients + daemon ship together.
    #[serde(default)]
    pub loudness_intensity: f64,
    #[serde(default)]
    pub loudness_enabled: bool,
    #[serde(default)]
    pub crossfeed_intensity: f64,
    #[serde(default)]
    pub crossfeed_enabled: bool,
}

impl EffectsState {
    /// `(intensity, enabled)` for one effect — the single place the flat fields
    /// map to the effect enum, so iterating `FxEffectId::ALL` needs no unroll.
    #[must_use]
    pub fn get(&self, id: FxEffectId) -> (f64, bool) {
        match id {
            FxEffectId::Fidelity => (self.fidelity_intensity, self.fidelity_enabled),
            FxEffectId::Ambience => (self.ambience_intensity, self.ambience_enabled),
            FxEffectId::Surround => (self.surround_intensity, self.surround_enabled),
            FxEffectId::DynamicBoost => (self.dynamic_boost_intensity, self.dynamic_boost_enabled),
            FxEffectId::Bass => (self.bass_intensity, self.bass_enabled),
            FxEffectId::Loudness => (self.loudness_intensity, self.loudness_enabled),
            FxEffectId::Crossfeed => (self.crossfeed_intensity, self.crossfeed_enabled),
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
            FxEffectId::Loudness => {
                self.loudness_intensity = intensity;
                self.loudness_enabled = enabled;
            }
            FxEffectId::Bass => {
                self.bass_intensity = intensity;
                self.bass_enabled = enabled;
            }
            FxEffectId::Crossfeed => {
                self.crossfeed_intensity = intensity;
                self.crossfeed_enabled = enabled;
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
                slope_db_oct: 24,
                scope: BandScope::Mid,
                dynamics: None,
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
                loudness_intensity: 0.0,
                loudness_enabled: false,
                crossfeed_intensity: 0.3,
                crossfeed_enabled: true,
            },
        });
        command_round_trip(&Command::SetDither { bits: Some(16) });
        command_round_trip(&Command::SetDither { bits: None });
        command_round_trip(&Command::SetConvolutionIr {
            path: "/irs/room.wav".into(),
        });
        command_round_trip(&Command::ClearConvolutionIr);
        command_round_trip(&Command::SetConvolutionEnabled { enabled: false });
        command_round_trip(&Command::CaptureOutput { frames: 48_000 });
        command_round_trip(&Command::SetBandChannels {
            index: 1,
            channels: ChannelMask::from_indices([0, 2, 4]),
        });
        command_round_trip(&Command::SetChannelRouting {
            matrix: RoutingMatrix::swap(2, 0, 1),
        });
        command_round_trip(&Command::SwapChannels { a: 0, b: 1 });
        command_round_trip(&Command::ClearRouting);
        command_round_trip(&Command::SetAppVolume {
            key: "firefox.42".into(),
            volume: 1.5,
        });
        command_round_trip(&Command::SetAppMute {
            key: "firefox.42".into(),
            muted: true,
        });
        command_round_trip(&Command::SetSinkVolume {
            name: "alsa_output.pci".into(),
            volume: 0.8,
        });
        command_round_trip(&Command::SetSinkMute {
            name: "alsa_output.pci".into(),
            muted: true,
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
    fn band_dynamics_round_trips() {
        // Wire (postcard) round-trip of a dynamic band + the appended command.
        let b = BandState {
            band_type: BandType::Peaking,
            freq: 6000.0,
            gain_db: 0.0,
            q: 3.0,
            enabled: true,
            channels: ChannelMask::ALL,
            slope_db_oct: 12,
            scope: BandScope::Stereo,
            dynamics: Some(BandDynamics {
                threshold_db: -30.0,
                range_db: -6.0,
                attack_ms: 5.0,
                release_ms: 150.0,
            }),
        };
        let bytes = to_stdvec(&b).unwrap();
        let back: BandState = from_bytes(&bytes).unwrap();
        assert_eq!(back.dynamics, b.dynamics);

        command_round_trip(&Command::SetBandDynamics {
            index: 3,
            dynamics: Some(BandDynamics::DEFAULT),
        });
        command_round_trip(&Command::SetBandDynamics {
            index: 3,
            dynamics: None,
        });
    }

    #[test]
    fn band_dynamics_defaults_to_none() {
        // `BandState.dynamics` uses `#[serde(default)]` so pre-dynamics profile
        // `.toml`s load as static bands (disk back-compat is round-tripped in
        // the daemon's config tests, same split as the channel-mask note below).
        assert_eq!(Option::<BandDynamics>::default(), None);
        // The dsp mirror conversion round-trips.
        let d = BandDynamics {
            threshold_db: -40.0,
            range_db: 3.0,
            attack_ms: 1.0,
            release_ms: 80.0,
        };
        let dsp: resonance_dsp::filter::DynParams = d.into();
        assert_eq!(BandDynamics::from(dsp), d);
    }

    #[test]
    fn only_peaking_uses_dynamics() {
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
            assert_eq!(
                bt.uses_dynamics(),
                bt == BandType::Peaking,
                "uses_dynamics wrong for {bt:?}"
            );
        }
    }

    #[test]
    fn phase_mode_command_round_trips() {
        command_round_trip(&Command::SetPhaseMode { linear: true });
        command_round_trip(&Command::SetPhaseMode { linear: false });
    }

    #[test]
    fn band_audition_command_round_trips() {
        command_round_trip(&Command::SetBandAudition {
            index: Some(2),
            mode: AuditionMode::Solo,
        });
        command_round_trip(&Command::SetBandAudition {
            index: Some(2),
            mode: AuditionMode::Listen,
        });
        command_round_trip(&Command::SetBandAudition {
            index: None,
            mode: AuditionMode::Solo,
        });
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
                slope_db_oct: 48,
                scope: BandScope::Side,
                dynamics: None,
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
                loudness_intensity: 0.6,
                loudness_enabled: true,
                crossfeed_intensity: 0.4,
                crossfeed_enabled: true,
            },
            current_preset: Some("x.fac".into()),
            sample_rate: 48000.0,
            capture_rate: 48000.0,
            channels: 2,
            out_channels: 2,
            channel_layout: default_channel_layout(2),
            routing: Some(RoutingMatrix::swap(2, 0, 1)),
            spectrum: vec![0.1, 0.2, 0.3],
            phase_mode_linear: true,
            eq_fir_latency_frames: 8448,
            active_output: Some("alsa_output.pci".into()),
            mapped_profile: None,
            available_sinks: vec!["alsa_output.pci".into()],
            sink_descriptions: vec![("alsa_output.pci".into(), "Built-in Audio".into())],
            preferred_output: None,
            meters: Meters::default(),
            apps: vec![AppStream {
                key: "firefox.42".into(),
                display_name: "Firefox".into(),
                pid: Some(4242),
                volume: 1.5,
                muted: false,
                active: true,
            }],
            sinks: vec![SinkVolume {
                name: "alsa_output.pci".into(),
                description: "Built-in Audio".into(),
                volume: 0.8,
                muted: false,
            }],
            dither_bits: Some(24),
            convolution: Some(ConvolutionState {
                path: "/irs/room.wav".into(),
                name: "room".into(),
                ir_sample_rate: 44_100.0,
                ir_channels: 2,
                taps: 65_536,
                latency_frames: 256,
                enabled: true,
            }),
            audition: Some(BandAudition {
                band: 1,
                mode: AuditionMode::Listen,
            }),
        };
        let bytes = to_stdvec(&Response::State(st)).expect("encode");
        let _: Response = from_bytes(&bytes).expect("decode");
    }

    #[test]
    fn convolution_state_round_trips_through_postcard() {
        let c = ConvolutionState {
            path: "/irs/hrtf.wav".into(),
            name: "hrtf".into(),
            ir_sample_rate: 48_000.0,
            ir_channels: 1,
            taps: 512,
            latency_frames: 256,
            enabled: false,
        };
        let bytes = to_stdvec(&c).unwrap();
        let back: ConvolutionState = from_bytes(&bytes).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn sink_volume_round_trips_through_postcard() {
        let sink = SinkVolume {
            name: "alsa_output.pci".into(),
            description: "Built-in Audio".into(),
            volume: 0.6,
            muted: true,
        };
        let bytes = to_stdvec(&sink).unwrap();
        let back: SinkVolume = from_bytes(&bytes).unwrap();
        assert_eq!(sink, back);
    }

    #[test]
    fn app_stream_round_trips_through_postcard() {
        let app = AppStream {
            key: "firefox.0".into(),
            display_name: "Firefox".into(),
            pid: Some(4242),
            volume: 1.5,
            muted: false,
            active: true,
        };
        let bytes = to_stdvec(&app).unwrap();
        let back: AppStream = from_bytes(&bytes).unwrap();
        assert_eq!(app, back);
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
            Response::Capture {
                rate: 48_000.0,
                samples: vec![0.0, 0.5, -0.5],
            },
        ] {
            let bytes = to_stdvec(&r).expect("encode");
            let _: Response = from_bytes(&bytes).expect("decode");
        }
    }
}
