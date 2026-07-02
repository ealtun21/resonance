//! On-disk config: named profiles + output→profile mappings.
//!
//! Layout under the platform config dir (`resonance_ipc::paths::config_dir`):
//!   - Linux: `$XDG_CONFIG_HOME/resonance` else `~/.config/resonance`
//!   - macOS: `~/Library/Application Support/resonance` (XDG vars honoured if set)
//!
//! Files:
//!   profiles/<name>.toml   — one saved chain state per file
//!   config.toml            — `[mappings]` table: output node.name → profile name

use resonance_dsp::chain::{FxEffect, ProcessorChain};
use resonance_dsp::filter::{ApoFilter, FilterType};
use resonance_ipc::{BandState, BandType, ChannelMask, DaemonState, EffectsState, FxEffectId};
use resonance_preset::model::Preset;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// A saved chain state. Mirrors the tunable parts of `DaemonState`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Profile {
    pub preamp_db: f64,
    pub enabled: bool,
    pub effects: EffectsState,
    pub bands: Vec<BandState>,
    /// Output dither target bit depth (`None` = off). `serde` default so profiles
    /// written before dither existed still load.
    #[serde(default)]
    pub dither_bits: Option<u32>,
    /// Convolution IR reference (`None` = off). Only the source path + enabled
    /// flag persist — the daemon re-reads the WAV when the profile is applied.
    /// `serde` default so profiles written before convolution existed still load.
    #[serde(default)]
    pub convolution: Option<ConvolutionProfile>,
}

/// The persisted slice of the convolution stage: enough to restore it from disk.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConvolutionProfile {
    /// Source WAV path.
    pub path: String,
    /// False = keep the IR loaded but bypassed.
    pub enabled: bool,
}

impl Profile {
    /// Capture the current daemon state into a profile.
    pub fn from_state(s: &DaemonState) -> Self {
        Self {
            preamp_db: s.preamp_db,
            enabled: s.enabled,
            effects: s.effects.clone(),
            bands: s.bands.clone(),
            dither_bits: s.dither_bits,
            convolution: s.convolution.as_ref().map(|c| ConvolutionProfile {
                path: c.path.clone(),
                enabled: c.enabled,
            }),
        }
    }

    /// Read a named profile from the config dir.
    pub fn load(name: &str) -> Result<Self, String> {
        load_profile(name)
    }

    /// Convert a parsed preset (`.fac` / APO `.txt`) into a saveable profile.
    /// The chain starts enabled; effect intensities/bands carry over verbatim.
    pub fn from_preset(p: &Preset) -> Self {
        let bands = p
            .bands
            .iter()
            .map(|b| {
                let ft: FilterType = b.filter_type.into();
                BandState {
                    band_type: BandType::from(ft),
                    freq: b.freq,
                    gain_db: b.gain_db,
                    q: b.q,
                    enabled: b.enabled,
                    channels: ChannelMask(b.channels),
                    // Presets (.fac / APO .txt) carry no portable slope token, so
                    // preset-derived bands default to 12 dB/oct (single biquad).
                    slope_db_oct: resonance_ipc::default_slope_db_oct(),
                    // Presets have no mid/side concept — default to full-stereo.
                    scope: resonance_ipc::BandScope::Stereo,
                }
            })
            .collect();

        let e = &p.effects;
        Self {
            preamp_db: p.preamp_db,
            enabled: true,
            effects: EffectsState {
                fidelity_intensity: e.fidelity.intensity,
                fidelity_enabled: e.fidelity.enabled,
                ambience_intensity: e.ambience.intensity,
                ambience_enabled: e.ambience.enabled,
                surround_intensity: e.surround.intensity,
                surround_enabled: e.surround.enabled,
                dynamic_boost_intensity: e.dynamic_boost.intensity,
                dynamic_boost_enabled: e.dynamic_boost.enabled,
                bass_intensity: e.bass.intensity,
                bass_enabled: e.bass.enabled,
                // Loudness is a Resonance-native effect with no FxSound/APO preset
                // equivalent, so it starts off for preset-derived profiles.
                loudness_intensity: 0.0,
                loudness_enabled: false,
                // Crossfeed likewise has no preset equivalent — off by default.
                crossfeed_intensity: 0.0,
                crossfeed_enabled: false,
            },
            bands,
            dither_bits: None,
            convolution: None,
        }
    }

    /// Build a processing chain from this profile at the given format.
    pub fn into_chain(self, channels: usize, sample_rate: f64) -> ProcessorChain {
        let mut builder = ProcessorChain::builder()
            .channels(channels)
            .sample_rate(sample_rate)
            // Sanitize the preamp here: ApplyState and a hand-edited/corrupt
            // profile `.toml` both reach `into_chain` without going through the
            // SetPreamp finite-check, and a NaN preamp silences all output.
            .preamp_db(sane_preamp(self.preamp_db));

        for b in &self.bands {
            if let Ok(filter) = ApoFilter::builder()
                .filter_type(b.band_type.into())
                .freq(b.freq)
                .gain_db(b.gain_db)
                .q(b.q)
                .slope_db_oct(b.slope_db_oct)
                .scope(b.scope.into())
                .enabled(b.enabled)
                .channels(channels)
                .sample_rate(sample_rate)
                .channel_mask(b.channels.to_dsp())
                .build()
            {
                builder = builder.add_filter(filter);
            }
        }

        let mut chain = builder.build();
        chain.enabled = self.enabled;

        for id in FxEffectId::ALL {
            let (intensity, enabled) = self.effects.get(id);
            let fx = FxEffect::from(id);
            chain.set_effect_intensity(fx, intensity);
            chain.set_effect_enabled(fx, enabled);
        }
        chain.set_dither(self.dither_bits);

        chain
    }
}

// ── Paths ────────────────────────────────────────────────────────────────────

/// Platform-aware config dir. Re-exported through the daemon to keep callers
/// simple — defers to the shared `resonance_ipc::paths::config_dir`.
pub fn config_dir() -> PathBuf {
    resonance_ipc::paths::config_dir()
}

fn profiles_dir() -> PathBuf {
    config_dir().join("profiles")
}

fn profile_path(name: &str) -> PathBuf {
    // Sanitize at the single chokepoint every profile file path flows through,
    // so save/load/delete/rename can't be steered outside the profiles dir by a
    // name like "../../foo" coming from a client.
    let mut safe = sanitize_name(name);
    if safe.is_empty() {
        safe = "_".to_string();
    }
    profiles_dir().join(format!("{safe}.toml"))
}

/// Clamp a preamp value to a sane dB range, mapping non-finite to 0 dB.
/// `f64::clamp` would propagate NaN, so the finite check must come first.
fn sane_preamp(db: f64) -> f64 {
    if db.is_finite() {
        db.clamp(-60.0, 24.0)
    } else {
        0.0
    }
}

/// Atomically replace `path`'s contents: write a sibling temp file, then rename
/// over the target. A crash mid-write leaves the old file intact instead of a
/// truncated one that the lenient loaders would silently treat as empty.
fn write_atomic(path: &std::path::Path, contents: &str) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn mappings_path() -> PathBuf {
    config_dir().join("config.toml")
}

fn known_sinks_path() -> PathBuf {
    config_dir().join("known_sinks.toml")
}

// ── Profile I/O ──────────────────────────────────────────────────────────────

pub fn save_profile(name: &str, profile: &Profile) -> Result<(), String> {
    let dir = profiles_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let toml = toml::to_string_pretty(profile).map_err(|e| e.to_string())?;
    write_atomic(&profile_path(name), &toml)
}

pub fn load_profile(name: &str) -> Result<Profile, String> {
    let content = std::fs::read_to_string(profile_path(name))
        .map_err(|e| format!("profile '{name}': {e}"))?;
    toml::from_str(&content).map_err(|e| e.to_string())
}

/// Read a profile from an arbitrary `.toml` path (our own export format), used
/// when importing/exporting outside the managed profiles directory.
pub fn load_profile_file(path: &str) -> Result<Profile, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read '{path}': {e}"))?;
    toml::from_str(&content).map_err(|e| format!("parse '{path}': {e}"))
}

/// Serialise a profile to our `.toml` format and write it to an arbitrary path.
pub fn export_profile_file(path: &str, profile: &Profile) -> Result<(), String> {
    let toml = toml::to_string_pretty(profile).map_err(|e| e.to_string())?;
    std::fs::write(path, toml).map_err(|e| format!("write '{path}': {e}"))
}

pub fn delete_profile(name: &str) -> Result<(), String> {
    std::fs::remove_file(profile_path(name)).map_err(|e| format!("profile '{name}': {e}"))
}

pub fn rename_profile(from: &str, to: &str) -> Result<(), String> {
    let to = sanitize_name(to);
    if to.is_empty() {
        return Err("new name is empty".to_string());
    }
    let src = profile_path(from);
    if !src.exists() {
        return Err(format!("profile '{from}' not found"));
    }
    let dst = profile_path(&to);
    if dst.exists() {
        return Err(format!("profile '{to}' already exists"));
    }
    std::fs::rename(src, dst).map_err(|e| format!("rename profile '{from}': {e}"))
}

/// Reduce an arbitrary string to a safe single-segment profile name (no path
/// separators, no surrounding whitespace).
pub fn sanitize_name(name: &str) -> String {
    name.trim()
        .replace(['/', '\\'], "_")
        .trim_matches('.')
        .to_string()
}

pub fn list_profiles() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(profiles_dir()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(std::result::Result::ok)
        .filter_map(|e| {
            let path = e.path();
            (path.extension()?.to_str()? == "toml")
                .then(|| path.file_stem()?.to_str().map(str::to_owned))
                .flatten()
        })
        .collect();
    names.sort();
    names
}

// ── Mappings (output node.name → profile name) ───────────────────────────────

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Mappings {
    #[serde(default)]
    pub mappings: BTreeMap<String, String>,
}

impl Mappings {
    pub fn load() -> Self {
        std::fs::read_to_string(mappings_path())
            .ok()
            .and_then(|c| toml::from_str(&c).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        std::fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
        let toml = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        write_atomic(&mappings_path(), &toml)
    }

    pub fn get(&self, output: &str) -> Option<&str> {
        self.mappings.get(output).map(String::as_str)
    }

    pub fn set(&mut self, output: String, profile: String) {
        self.mappings.insert(output, profile);
    }

    pub fn remove(&mut self, output: &str) {
        self.mappings.remove(output);
    }

    pub fn list(&self) -> Vec<(String, String)> {
        self.mappings
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

// ── Known sinks (every output device ever seen → its friendly description) ────
//
// PipeWire only reports the *currently present* sinks. Once a device unplugs we
// lose its `node.description`, so any output→profile mapping for it would render
// as a bare node name and be hard to recognise. We persist a registry of every
// sink we have ever observed; clients merge it in so labels and mappings survive
// a device being removed and reconnected.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct KnownSinks {
    #[serde(default)]
    pub devices: BTreeMap<String, String>,
}

impl KnownSinks {
    pub fn load() -> Self {
        std::fs::read_to_string(known_sinks_path())
            .ok()
            .and_then(|c| toml::from_str(&c).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        std::fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
        let toml = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        write_atomic(&known_sinks_path(), &toml)
    }

    /// Record a `node.name → description`. Returns true if it added or changed an
    /// entry (so the caller can skip writing the file when nothing changed).
    pub fn remember(&mut self, name: String, desc: String) -> bool {
        if desc.is_empty() {
            return false;
        }
        match self.devices.get(&name) {
            Some(d) if d == &desc => false,
            _ => {
                self.devices.insert(name, desc);
                true
            }
        }
    }

    pub fn list(&self) -> Vec<(String, String)> {
        self.devices
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use resonance_preset::model::{ApoFilterType, EffectState, EqBand, FxEffects, Preset};

    #[test]
    fn from_preset_maps_bands_preamp_and_effects() {
        let preset = Preset {
            name: "T".into(),
            preamp_db: -4.5,
            eq_enabled: true,
            bands: vec![EqBand {
                filter_type: ApoFilterType::HighShelf,
                freq: 10_000.0,
                gain_db: 3.0,
                q: 0.7,
                enabled: true,
                channels: u64::MAX,
            }],
            effects: FxEffects {
                fidelity: EffectState {
                    enabled: true,
                    intensity: 0.5,
                },
                bass: EffectState {
                    enabled: true,
                    intensity: 0.25,
                },
                ..Default::default()
            },
        };

        let p = Profile::from_preset(&preset);
        assert_eq!(p.bands.len(), 1);
        assert_eq!(p.bands[0].band_type, BandType::HighShelf);
        assert!((p.bands[0].freq - 10_000.0).abs() < 1e-9);
        assert!((p.preamp_db + 4.5).abs() < 1e-9);
        assert!(p.enabled, "imported chain should start enabled");
        assert!(p.effects.fidelity_enabled);
        assert!((p.effects.fidelity_intensity - 0.5).abs() < 1e-9);
        assert!((p.effects.bass_intensity - 0.25).abs() < 1e-9);
        assert!(!p.effects.surround_enabled);
    }

    #[test]
    fn profile_loads_legacy_toml_without_channels_as_global() {
        // A profile saved before per-channel EQ has no `channels` key on its
        // bands; `#[serde(default)]` must load them as global (ALL).
        let toml = "preamp_db = -3.0\n\
             enabled = true\n\
             \n\
             [effects]\n\
             fidelity_intensity = 0.0\n\
             fidelity_enabled = false\n\
             ambience_intensity = 0.0\n\
             ambience_enabled = false\n\
             surround_intensity = 0.0\n\
             surround_enabled = false\n\
             dynamic_boost_intensity = 0.0\n\
             dynamic_boost_enabled = false\n\
             bass_intensity = 0.0\n\
             bass_enabled = false\n\
             \n\
             [[bands]]\n\
             band_type = \"Peaking\"\n\
             freq = 1000.0\n\
             gain_db = 3.0\n\
             q = 1.0\n\
             enabled = true\n";
        let p: Profile = toml::from_str(toml).unwrap();
        assert_eq!(p.bands.len(), 1);
        assert_eq!(p.bands[0].channels, ChannelMask::ALL);
    }

    #[test]
    fn profile_with_per_channel_band_round_trips_toml() {
        // The global default is u64::MAX, which exceeds TOML's i64 range; the
        // ChannelMask i64 bit-cast serde impl must let it serialize + reload.
        let profile = Profile {
            preamp_db: 0.0,
            enabled: true,
            effects: EffectsState::default(),
            dither_bits: None,
            convolution: None,
            bands: vec![
                BandState {
                    band_type: BandType::Peaking,
                    freq: 1000.0,
                    gain_db: 3.0,
                    q: 1.0,
                    enabled: true,
                    channels: ChannelMask::ALL,
                    slope_db_oct: 12,
                    scope: resonance_ipc::BandScope::Stereo,
                },
                BandState {
                    band_type: BandType::Peaking,
                    freq: 2000.0,
                    gain_db: -2.0,
                    q: 1.0,
                    enabled: true,
                    channels: ChannelMask::single(0),
                    slope_db_oct: 24,
                    scope: resonance_ipc::BandScope::Side,
                },
            ],
        };
        let text =
            toml::to_string_pretty(&profile).expect("serialize (u64::MAX must not overflow)");
        let back: Profile = toml::from_str(&text).unwrap();
        assert_eq!(back.bands[0].channels, ChannelMask::ALL);
        assert_eq!(back.bands[1].channels, ChannelMask::single(0));
    }

    #[test]
    fn profile_convolution_round_trips_toml_and_defaults_to_none() {
        let profile = Profile {
            preamp_db: 0.0,
            enabled: true,
            effects: EffectsState::default(),
            bands: vec![],
            dither_bits: None,
            convolution: Some(ConvolutionProfile {
                path: "/irs/room.wav".into(),
                enabled: true,
            }),
        };
        let text = toml::to_string_pretty(&profile).unwrap();
        let back: Profile = toml::from_str(&text).unwrap();
        assert_eq!(back.convolution, profile.convolution);

        // A profile written before convolution existed loads with None.
        let legacy = "preamp_db = 0.0\nenabled = true\n\n[effects]\n\
             fidelity_intensity = 0.0\nfidelity_enabled = false\n\
             ambience_intensity = 0.0\nambience_enabled = false\n\
             surround_intensity = 0.0\nsurround_enabled = false\n\
             dynamic_boost_intensity = 0.0\ndynamic_boost_enabled = false\n\
             bass_intensity = 0.0\nbass_enabled = false\n\n\
             [[bands]]\nband_type = \"Peaking\"\nfreq = 1000.0\n\
             gain_db = 3.0\nq = 1.0\nenabled = true\n";
        let p: Profile = toml::from_str(legacy).unwrap();
        assert!(p.convolution.is_none());
    }

    #[test]
    fn sanitize_name_strips_separators_and_dots() {
        assert_eq!(sanitize_name("  rock night  "), "rock night");
        assert_eq!(sanitize_name("rock/night"), "rock_night");
        assert_eq!(sanitize_name("a\\b"), "a_b");
        assert_eq!(sanitize_name("..foo.."), "foo");
    }
}
