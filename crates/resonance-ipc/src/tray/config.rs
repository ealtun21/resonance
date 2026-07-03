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

/// Settings upper bound for `recent_count`. The top position of the count
/// control means "All" (no limit), stored as [`RECENT_ALL`].
pub const RECENT_MAX: usize = 20;

/// Sentinel `recent_count` meaning "show every recent preset" (no limit).
/// `Vec::truncate` / `Iterator::take` treat `usize::MAX` as unbounded, so no
/// consumer of `recent_count` has to special-case it.
pub const RECENT_ALL: usize = usize::MAX;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrayConfig {
    pub left_click: LeftClick,
    pub poll_secs: u64,
    /// When set, the tray's "Quit Resonance" item also stops the daemon — that
    /// quit closes *everything*. On by default. When cleared, it exits just the
    /// tray, leaving the daemon (and the rest of the stack) running. Closing a
    /// UI window never stops the daemon regardless of this flag.
    pub quit_stops_daemon: bool,
    /// Number of recent presets to list in the tray menu. [`RECENT_ALL`] =
    /// unlimited (show every one).
    pub recent_count: usize,
    /// When set, launching the GUI also starts the tray (unless it is already
    /// running — e.g. from autostart). On by default. GUI-scoped: this
    /// start-tray-with-GUI behaviour only applies to the GUI process.
    pub start_tray_with_gui: bool,
}

impl Default for TrayConfig {
    fn default() -> Self {
        Self {
            left_click: LeftClick::ToggleUi,
            poll_secs: 3,
            quit_stops_daemon: true,
            recent_count: 8,
            start_tray_with_gui: true,
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
        assert!(c.quit_stops_daemon, "quit closes everything by default");
        assert_eq!(c.recent_count, 8);
        assert!(
            c.start_tray_with_gui,
            "GUI starts the tray by default (idempotent — no-op if already up)"
        );
    }

    #[test]
    fn toml_round_trips() {
        let c = TrayConfig {
            left_click: LeftClick::Menu,
            poll_secs: 5,
            quit_stops_daemon: true,
            recent_count: RECENT_ALL,
            start_tray_with_gui: false,
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

    #[test]
    fn stale_close_to_tray_field_is_ignored() {
        // Configs written before close-to-tray was removed must still load
        // (serde ignores unknown fields), falling back to the new defaults.
        let back: TrayConfig = toml::from_str("close_gui_to_tray = true\n").unwrap();
        assert!(
            back.quit_stops_daemon,
            "unknown field ignored; new field defaults"
        );
        assert_eq!(back.recent_count, 8);
    }
}
