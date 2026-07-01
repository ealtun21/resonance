use resonance_ipc::BandType;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prefs {
    #[serde(default = "default_fps")]
    pub fps: u64,
    #[serde(default = "default_refresh_ms")]
    pub refresh_ms: u64,
    #[serde(default = "default_confirm")]
    pub confirm_on_delete: bool,
    #[serde(default = "default_band_q")]
    pub default_band_q: f64,
    #[serde(default = "default_band_type")]
    pub default_band_type: BandType,
    #[serde(default = "default_show_spectrum")]
    pub show_spectrum: bool,
    /// Show the per-application volume panel (toggle with `A`). Defaults on, so
    /// the panel still appears automatically once the daemon reports streams.
    #[serde(default = "default_true")]
    pub show_apps: bool,
    /// Show the per-output-sink volume panel (toggle with `O`). Defaults off —
    /// opt-in, so a plain launch stays uncluttered on small terminals.
    #[serde(default = "default_false")]
    pub show_sinks: bool,
    /// Advanced-feature visibility toggles — all default off so a fresh launch
    /// shows a clean UI. Each gates a control + its keybinding; power users opt
    /// in from Settings → Preferences. Channels also auto-shows on >2ch devices
    /// (see `App::show_ch`).
    #[serde(default = "default_false")]
    pub show_slope: bool,
    #[serde(default = "default_false")]
    pub show_scope: bool,
    #[serde(default = "default_false")]
    pub show_dither: bool,
    #[serde(default = "default_false")]
    pub show_channels: bool,
}

fn default_fps() -> u64 {
    144
}
fn default_refresh_ms() -> u64 {
    // ~30 Hz. The daemon poll carries the meters and spectrum, which animate, so
    // this is effectively the perceived frame rate of the live readouts — not a
    // background-only refresh. 250 ms (4 Hz) made the meters/spectrum visibly
    // step and read as "low fps"; 33 ms matches the GUI's proven state poll and
    // stays cheap (the snapshot round-trip is sub-millisecond on a local socket).
    33
}
fn default_confirm() -> bool {
    true
}
fn default_band_q() -> f64 {
    1.4
}
fn default_band_type() -> BandType {
    BandType::Peaking
}
fn default_show_spectrum() -> bool {
    true
}
fn default_true() -> bool {
    true
}
fn default_false() -> bool {
    false
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            fps: default_fps(),
            refresh_ms: default_refresh_ms(),
            confirm_on_delete: default_confirm(),
            default_band_q: default_band_q(),
            default_band_type: default_band_type(),
            show_spectrum: default_show_spectrum(),
            show_apps: default_true(),
            show_sinks: default_false(),
            show_slope: default_false(),
            show_scope: default_false(),
            show_dither: default_false(),
            show_channels: default_false(),
        }
    }
}

impl Prefs {
    pub fn config_dir() -> PathBuf {
        // Defer to the shared platform-aware resolver: Linux → XDG, macOS →
        // ~/Library/Application Support/resonance, XDG vars honoured everywhere.
        resonance_ipc::paths::config_dir()
    }

    pub fn path() -> PathBuf {
        Self::config_dir().join("tui.toml")
    }

    pub fn load() -> Self {
        let p = Self::path();
        std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        // Unit tests exercise the panel toggles (which persist prefs); they must
        // never write the user's real config, or one test's save would leak into
        // another `App::new()`/`Prefs::load()`.
        #[cfg(test)]
        let _ = self;
        #[cfg(not(test))]
        {
            let p = Self::path();
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(s) = toml::to_string(self) {
                let _ = std::fs::write(p, s);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advanced_toggles_default_off() {
        // An empty config (all serde defaults) must leave every advanced
        // feature hidden for a clean first-run UI.
        let p: Prefs = toml::from_str("").unwrap();
        assert!(!p.show_slope);
        assert!(!p.show_scope);
        assert!(!p.show_dither);
        assert!(!p.show_channels);
    }

    #[test]
    fn advanced_toggles_roundtrip() {
        let mut p = Prefs::default();
        p.show_slope = true;
        p.show_channels = true;
        let s = toml::to_string(&p).unwrap();
        let back: Prefs = toml::from_str(&s).unwrap();
        assert!(back.show_slope);
        assert!(!back.show_scope);
        assert!(!back.show_dither);
        assert!(back.show_channels);
    }
}
