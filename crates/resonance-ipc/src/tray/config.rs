use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LeftClick {
    /// Left click shows/hides (toggles) the UI window.
    #[default]
    ToggleUi,
    /// Left click opens the tray menu; the UI is reached from a menu item.
    Menu,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrayConfig {
    pub left_click: LeftClick,
    pub poll_secs: u64,
    pub close_gui_to_tray: bool,
    pub recent_count: usize,
}

impl Default for TrayConfig {
    fn default() -> Self {
        Self {
            left_click: LeftClick::ToggleUi,
            poll_secs: 3,
            close_gui_to_tray: false,
            recent_count: 8,
        }
    }
}

/// `<config_dir>/tray.toml`.
#[must_use]
pub fn config_path() -> PathBuf {
    crate::paths::config_dir().join("tray.toml")
}

impl TrayConfig {
    /// Load the config, returning defaults when the file is absent or malformed
    /// (the tray must always start).
    #[must_use]
    pub fn load() -> Self {
        std::fs::read_to_string(config_path())
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Persist the config, creating the config dir if needed.
    ///
    /// # Errors
    /// Returns an error if the config dir or file cannot be written.
    pub fn save(&self) -> std::io::Result<()> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_low_resource() {
        let c = TrayConfig::default();
        assert_eq!(c.left_click, LeftClick::ToggleUi);
        assert_eq!(c.poll_secs, 3);
        assert!(!c.close_gui_to_tray, "off by default = lowest RAM");
        assert_eq!(c.recent_count, 8);
    }

    #[test]
    fn toml_round_trips() {
        let c = TrayConfig {
            left_click: LeftClick::Menu,
            poll_secs: 5,
            close_gui_to_tray: true,
            recent_count: 4,
        };
        let text = toml::to_string(&c).unwrap();
        let back: TrayConfig = toml::from_str(&text).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn missing_fields_fall_back_to_default() {
        // A partial file (e.g. written by an older client) must not fail to load.
        let back: TrayConfig = toml::from_str("poll_secs = 10\n").unwrap();
        assert_eq!(back.poll_secs, 10);
        assert_eq!(back.left_click, LeftClick::ToggleUi);
        assert_eq!(back.recent_count, 8);
    }
}
