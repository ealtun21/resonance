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
use resonance_ipc::{BandState, BandType, DaemonState, EffectsState, FxEffectId};
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
}

impl Profile {
    /// Capture the current daemon state into a profile.
    pub fn from_state(s: &DaemonState) -> Self {
        Self {
            preamp_db: s.preamp_db,
            enabled: s.enabled,
            effects: s.effects.clone(),
            bands: s.bands.clone(),
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
            },
            bands,
        }
    }

    /// Build a processing chain from this profile at the given format.
    pub fn into_chain(self, channels: usize, sample_rate: f64) -> ProcessorChain {
        let mut builder = ProcessorChain::builder()
            .channels(channels)
            .sample_rate(sample_rate)
            .preamp_db(self.preamp_db);

        for b in &self.bands {
            if let Ok(filter) = ApoFilter::builder()
                .filter_type(b.band_type.into())
                .freq(b.freq)
                .gain_db(b.gain_db)
                .q(b.q)
                .enabled(b.enabled)
                .channels(channels)
                .sample_rate(sample_rate)
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
    profiles_dir().join(format!("{name}.toml"))
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
    std::fs::write(profile_path(name), toml).map_err(|e| e.to_string())
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
        .filter_map(|e| e.ok())
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
        std::fs::write(mappings_path(), toml).map_err(|e| e.to_string())
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
        std::fs::write(known_sinks_path(), toml).map_err(|e| e.to_string())
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
    fn sanitize_name_strips_separators_and_dots() {
        assert_eq!(sanitize_name("  rock night  "), "rock night");
        assert_eq!(sanitize_name("rock/night"), "rock_night");
        assert_eq!(sanitize_name("a\\b"), "a_b");
        assert_eq!(sanitize_name("..foo.."), "foo");
    }
}
