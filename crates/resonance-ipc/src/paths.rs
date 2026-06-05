//! XDG paths for the preset library, shared by the daemon and all clients.
//!
//! Source preset files (`.fac` / APO `.txt`) live under
//! `$XDG_DATA_HOME/resonance/presets` (the writable user library) and the
//! read-only system dirs derived from `$XDG_DATA_DIRS`.

use std::path::PathBuf;

fn data_home() -> PathBuf {
    if let Ok(p) = std::env::var("XDG_DATA_HOME") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".local").join("share")
}

/// Writable user preset library: `$XDG_DATA_HOME/resonance/presets`.
pub fn user_preset_dir() -> PathBuf {
    data_home().join("resonance").join("presets")
}

/// Read-only system preset dirs from `$XDG_DATA_DIRS` (defaults per the spec).
fn system_preset_dirs() -> Vec<PathBuf> {
    let raw = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let raw = if raw.is_empty() {
        "/usr/local/share:/usr/share".to_string()
    } else {
        raw
    };
    raw.split(':')
        .filter(|s| !s.is_empty())
        .map(|s| PathBuf::from(s).join("resonance").join("presets"))
        .collect()
}

/// All preset search dirs, user library first (so it shadows system entries).
pub fn preset_search_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![user_preset_dir()];
    dirs.extend(system_preset_dirs());
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_dir_ends_with_resonance_presets() {
        let d = user_preset_dir();
        assert!(d.ends_with("resonance/presets"), "got {}", d.display());
    }

    #[test]
    fn search_dirs_start_with_user_dir() {
        let dirs = preset_search_dirs();
        assert_eq!(dirs[0], user_preset_dir());
        assert!(dirs.len() >= 2, "expected user + at least one system dir");
    }
}
