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

#[cfg(windows)]
fn home() -> PathBuf {
    if let Ok(p) = std::env::var("USERPROFILE") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    PathBuf::from("C:\\")
}

/// `$VAR` as a path, if it is set and non-empty. The empty-string guard matters:
/// an exported-but-blank env var should fall through to the next candidate, not
/// resolve to the current directory.
fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(not(windows))]
fn home() -> PathBuf {
    env_dir("HOME").unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn data_home() -> PathBuf {
    if let Some(p) = env_dir("XDG_DATA_HOME") {
        return p;
    }
    #[cfg(windows)]
    {
        // Per-user, non-roaming app data: %LOCALAPPDATA%.
        if let Some(p) = env_dir("LOCALAPPDATA") {
            return p;
        }
        home().join("AppData").join("Local")
    }
    #[cfg(target_os = "macos")]
    {
        home().join("Library").join("Application Support")
    }
    #[cfg(all(not(target_os = "macos"), not(windows)))]
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
    #[cfg(windows)]
    {
        // Machine-wide app data: %ProgramData%\resonance\presets.
        if let Ok(p) = std::env::var("ProgramData") {
            if !p.is_empty() {
                return vec![PathBuf::from(p).join("resonance").join("presets")];
            }
        }
        vec![PathBuf::from("C:\\ProgramData\\resonance\\presets")]
    }
    #[cfg(target_os = "macos")]
    {
        vec![
            PathBuf::from("/Library/Application Support/resonance/presets"),
            PathBuf::from("/usr/local/share/resonance/presets"),
        ]
    }
    #[cfg(all(not(target_os = "macos"), not(windows)))]
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

/// Writable user curve library: reference *target* curves and saved/imported
/// measurements the user drops in or exports from the target customizer. Sits
/// beside the preset library so the existing file browser can reach it.
pub fn user_curve_dir() -> PathBuf {
    data_home().join("resonance").join("curves")
}

/// Per-user cache root for data that can always be re-fetched (so it belongs in
/// the cache, not the data, dir).
///   Linux:   `$XDG_CACHE_HOME` else `~/.cache`
///   macOS:   `~/Library/Caches`
///   Windows: `%LOCALAPPDATA%` (Windows has no data/cache split)
fn cache_home() -> PathBuf {
    if let Some(p) = env_dir("XDG_CACHE_HOME") {
        return p;
    }
    #[cfg(windows)]
    {
        data_home()
    }
    #[cfg(target_os = "macos")]
    {
        home().join("Library").join("Caches")
    }
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    {
        home().join(".cache")
    }
}

/// Cache dir for downloaded squig.link measurement curves and database indexes.
pub fn curve_cache_dir() -> PathBuf {
    cache_home().join("resonance").join("curves")
}

/// Platform-aware config directory for the daemon (profiles, mappings,
/// known-sinks registry).
///
/// Linux: `$XDG_CONFIG_HOME/resonance` else `$HOME/.config/resonance`.
/// macOS: `$XDG_CONFIG_HOME/resonance` if set, else
///        `~/Library/Application Support/resonance` (Apple's per-user config
///        location — macOS has no separate "config" vs "data" split).
pub fn config_dir() -> PathBuf {
    if let Some(p) = env_dir("XDG_CONFIG_HOME") {
        return p.join("resonance");
    }
    #[cfg(windows)]
    {
        data_home().join("resonance")
    }
    #[cfg(target_os = "macos")]
    {
        home()
            .join("Library")
            .join("Application Support")
            .join("resonance")
    }
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    {
        home().join(".config").join("resonance")
    }
}

/// Log file for a directly-spawned daemon (when no service manager is capturing
/// its output). Creates the parent directory; the caller opens the file.
///   macOS:   `~/Library/Logs/resonance/resonanced.log`
///   Windows: `<config_dir>/resonanced.log`
///   Linux:   `/tmp/resonanced.log`
pub fn daemon_log_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    let dir = home().join("Library").join("Logs").join("resonance");
    #[cfg(windows)]
    let dir = config_dir();
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    let dir = PathBuf::from("/tmp");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("resonanced.log")
}

/// Per-user runtime directory for ephemeral state (socket, pidfile).
///
/// Linux: `$XDG_RUNTIME_DIR` (per the XDG Base Dir spec).
/// macOS: `$TMPDIR` (per-user temp dir, e.g. `/var/folders/.../T/`).
/// Fallback: `/tmp` (shared, but it works).
pub fn runtime_dir() -> PathBuf {
    if let Some(p) = env_dir("XDG_RUNTIME_DIR") {
        return p;
    }
    if let Some(p) = env_dir("TMPDIR") {
        return p;
    }
    #[cfg(windows)]
    {
        for var in ["TEMP", "TMP"] {
            if let Some(p) = env_dir(var) {
                return p;
            }
        }
        data_home().join("resonance")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/tmp")
    }
}

/// Windows-only: the port file the daemon writes its loopback TCP port into.
/// Clients read it to discover where to connect. Honours `$RESONANCE_SOCKET`
/// (treated as a full path) for parity with the Unix socket override.
#[cfg(windows)]
pub fn port_file_path() -> PathBuf {
    if let Ok(p) = std::env::var(crate::SOCKET_PATH_ENV) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    runtime_dir().join("resonance.port")
}

/// Windows-only: read the daemon's loopback TCP port from the port file.
#[cfg(windows)]
pub fn read_port_file() -> Option<u16> {
    std::fs::read_to_string(port_file_path())
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
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
    fn socket_path_default_then_env_override() {
        // Both assertions live in ONE test so the `RESONANCE_SOCKET` mutation
        // can't race a sibling test reading the default — env is process-global
        // and the harness runs #[test]s on parallel threads. Assert the default
        // first (env unset), then the override.
        let p = default_socket_path();
        assert_eq!(
            p.file_name().and_then(|s| s.to_str()),
            Some(crate::DEFAULT_SOCKET_FILENAME)
        );
        // SAFETY: this is the only test touching SOCKET_PATH_ENV, and it sets
        // then removes the var within this single thread, so it isn't racy.
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
