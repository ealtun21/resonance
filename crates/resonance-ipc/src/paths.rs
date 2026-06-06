//! Platform-aware paths for the preset library, shared by the daemon and
//! all clients.
//!
//! Linux: XDG spec — user library at `$XDG_DATA_HOME/resonance/presets`
//! (default `~/.local/share/resonance/presets`); system dirs from
//! `$XDG_DATA_DIRS` (default `/usr/local/share:/usr/share`).
//!
//! macOS: writable library at `~/Library/Application Support/resonance/presets`
//! (Apple's documented per-user app-data location). System dirs are
//! `/Library/Application Support/resonance/presets` and
//! `/usr/local/share/resonance/presets` (Homebrew convention).
//!
//! `$XDG_DATA_HOME` / `$XDG_DATA_DIRS` are honoured on every platform if set,
//! so users who already standardise on XDG layout (e.g. dotfiles) keep working.

use std::path::PathBuf;

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

fn data_home() -> PathBuf {
    if let Ok(p) = std::env::var("XDG_DATA_HOME") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    #[cfg(target_os = "macos")]
    {
        home().join("Library").join("Application Support")
    }
    #[cfg(not(target_os = "macos"))]
    {
        home().join(".local").join("share")
    }
}

/// Writable user preset library.
pub fn user_preset_dir() -> PathBuf {
    data_home().join("resonance").join("presets")
}

/// Read-only system preset dirs.
fn system_preset_dirs() -> Vec<PathBuf> {
    let raw = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    if !raw.is_empty() {
        return raw
            .split(':')
            .filter(|s| !s.is_empty())
            .map(|s| PathBuf::from(s).join("resonance").join("presets"))
            .collect();
    }
    #[cfg(target_os = "macos")]
    {
        vec![
            PathBuf::from("/Library/Application Support/resonance/presets"),
            PathBuf::from("/usr/local/share/resonance/presets"),
        ]
    }
    #[cfg(not(target_os = "macos"))]
    {
        vec![
            PathBuf::from("/usr/local/share/resonance/presets"),
            PathBuf::from("/usr/share/resonance/presets"),
        ]
    }
}

/// All preset search dirs, user library first (so it shadows system entries).
pub fn preset_search_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![user_preset_dir()];
    dirs.extend(system_preset_dirs());
    dirs
}

/// Platform-aware config directory for the daemon (profiles, mappings,
/// known-sinks registry).
///
/// Linux: `$XDG_CONFIG_HOME/resonance` else `$HOME/.config/resonance`.
/// macOS: `$XDG_CONFIG_HOME/resonance` if set, else
///        `~/Library/Application Support/resonance` (Apple's per-user config
///        location — macOS has no separate "config" vs "data" split).
pub fn config_dir() -> PathBuf {
    if let Ok(p) = std::env::var("XDG_CONFIG_HOME") {
        if !p.is_empty() {
            return PathBuf::from(p).join("resonance");
        }
    }
    #[cfg(target_os = "macos")]
    {
        home()
            .join("Library")
            .join("Application Support")
            .join("resonance")
    }
    #[cfg(not(target_os = "macos"))]
    {
        home().join(".config").join("resonance")
    }
}

/// Per-user runtime directory for ephemeral state (socket, pidfile).
///
/// Linux: `$XDG_RUNTIME_DIR` (per the XDG Base Dir spec).
/// macOS: `$TMPDIR` (per-user temp dir, e.g. `/var/folders/.../T/`).
/// Fallback: `/tmp` (shared, but it works).
pub fn runtime_dir() -> PathBuf {
    if let Ok(p) = std::env::var("XDG_RUNTIME_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Ok(p) = std::env::var("TMPDIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    PathBuf::from("/tmp")
}

/// Resolve the IPC Unix-socket path: respects `$RESONANCE_SOCKET` (any path),
/// else falls back to `<runtime_dir()>/resonance.sock`. Shared by the daemon
/// and every client so they stay in lock-step.
pub fn default_socket_path() -> PathBuf {
    if let Ok(p) = std::env::var(crate::SOCKET_PATH_ENV) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    runtime_dir().join(crate::DEFAULT_SOCKET_FILENAME)
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

    #[test]
    fn config_dir_ends_with_resonance() {
        // True on every platform: …/resonance is the leaf.
        let d = config_dir();
        assert!(d.ends_with("resonance"), "got {}", d.display());
    }

    #[test]
    fn default_socket_path_filename_matches_const() {
        let p = default_socket_path();
        assert_eq!(
            p.file_name().and_then(|s| s.to_str()),
            Some(crate::DEFAULT_SOCKET_FILENAME)
        );
    }

    #[test]
    fn socket_path_env_var_takes_precedence() {
        // Mutating env in tests is racy across threads — but Rust's test
        // harness runs each #[test] in its own thread and the env mutation
        // here is short-lived and isolated to this single test, so the
        // window for races is essentially zero in practice. Keep it minimal.
        // SAFETY: set_var/remove_var must be serialized in multithreaded
        // tests; this single mutation/read pair is the only thing in this
        // test touching the var so it's not racy with itself.
        unsafe {
            std::env::set_var(crate::SOCKET_PATH_ENV, "/tmp/test-resonance.sock");
        }
        assert_eq!(
            default_socket_path(),
            PathBuf::from("/tmp/test-resonance.sock")
        );
        unsafe {
            std::env::remove_var(crate::SOCKET_PATH_ENV);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_config_dir_uses_application_support() {
        // Don't allow $XDG_CONFIG_HOME to perturb this — clear it for the
        // duration of the call. Other tests don't read this either.
        let saved = std::env::var_os("XDG_CONFIG_HOME");
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        let d = config_dir();
        if let Some(v) = saved {
            unsafe {
                std::env::set_var("XDG_CONFIG_HOME", v);
            }
        }
        let s = d.display().to_string();
        assert!(
            s.contains("Library/Application Support/resonance"),
            "macOS config dir should live under Application Support: got {s}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_config_dir_uses_dot_config() {
        let saved = std::env::var_os("XDG_CONFIG_HOME");
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        let d = config_dir();
        if let Some(v) = saved {
            unsafe {
                std::env::set_var("XDG_CONFIG_HOME", v);
            }
        }
        let s = d.display().to_string();
        assert!(
            s.contains(".config/resonance"),
            "Linux config dir should live under .config: got {s}"
        );
    }
}
