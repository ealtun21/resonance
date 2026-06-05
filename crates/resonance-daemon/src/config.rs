//! On-disk config: named profiles + output→profile mappings.
//!
//! Layout under `$XDG_CONFIG_HOME/resonance` (else `$HOME/.config/resonance`):
//!   profiles/<name>.toml   — one saved chain state per file
//!   config.toml            — `[mappings]` table: output node.name → profile name

use resonance_dsp::chain::{FxEffect, ProcessorChain};
use resonance_dsp::filter::ApoFilter;
use resonance_ipc::{BandState, DaemonState, EffectsState};
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

        let e = &self.effects;
        chain.set_effect_intensity(FxEffect::Fidelity, e.fidelity_intensity);
        chain.set_effect_enabled(FxEffect::Fidelity, e.fidelity_enabled);
        chain.set_effect_intensity(FxEffect::Ambience, e.ambience_intensity);
        chain.set_effect_enabled(FxEffect::Ambience, e.ambience_enabled);
        chain.set_effect_intensity(FxEffect::Surround, e.surround_intensity);
        chain.set_effect_enabled(FxEffect::Surround, e.surround_enabled);
        chain.set_effect_intensity(FxEffect::DynamicBoost, e.dynamic_boost_intensity);
        chain.set_effect_enabled(FxEffect::DynamicBoost, e.dynamic_boost_enabled);
        chain.set_effect_intensity(FxEffect::Bass, e.bass_intensity);
        chain.set_effect_enabled(FxEffect::Bass, e.bass_enabled);

        chain
    }
}

// ── Paths ────────────────────────────────────────────────────────────────────

/// `$XDG_CONFIG_HOME/resonance` else `$HOME/.config/resonance`.
pub fn config_dir() -> PathBuf {
    if let Ok(p) = std::env::var("XDG_CONFIG_HOME") {
        if !p.is_empty() {
            return PathBuf::from(p).join("resonance");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".config").join("resonance")
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

pub fn delete_profile(name: &str) -> Result<(), String> {
    std::fs::remove_file(profile_path(name)).map_err(|e| format!("profile '{name}': {e}"))
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
