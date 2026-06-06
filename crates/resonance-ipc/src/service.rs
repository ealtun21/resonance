//! Per-user service control for `resonanced`, shared by every client.
//!
//! Cross-platform façade with one implementation per OS:
//!   - Linux: systemd user-service unit at `$XDG_CONFIG_HOME/systemd/user/resonanced.service`
//!   - macOS: launchd LaunchAgent plist at `~/Library/LaunchAgents/com.ealtun21.resonanced.plist`
//!
//! Every backend exposes the same operations so callers stay platform-agnostic:
//! `status()`, `install()`, `uninstall()`, `start()`, `stop()`, `restart()`,
//! `enable()`, `disable()`, and the helper booleans `is_installed/active/enabled`.

#[cfg(target_os = "linux")]
mod systemd;
#[cfg(target_os = "linux")]
use systemd as backend;

#[cfg(target_os = "macos")]
use crate::launchd as backend;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod stub;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use stub as backend;

use std::io;
use std::path::PathBuf;

/// Display name for the per-user service unit (systemd unit or launchd label,
/// chosen by the active backend).
pub const UNIT_NAME: &str = backend::UNIT_NAME;

/// Installed/active/enabled snapshot of the user service.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Status {
    /// The unit file exists in its OS-specific location.
    pub installed: bool,
    /// The service is currently running.
    pub active: bool,
    /// The service is enabled for autostart.
    pub enabled: bool,
}

/// Path the unit file is written to (OS-specific).
pub fn unit_path() -> PathBuf {
    backend::unit_path()
}

/// Resolve the `resonanced` binary: prefer a sibling of the current executable
/// (covers a co-installed CLI/daemon pair), then `$PATH`, else the bare name.
pub fn daemon_bin() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("resonanced");
            if cand.is_file() {
                return cand;
            }
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for d in path.split(':').filter(|s| !s.is_empty()) {
            let cand = std::path::Path::new(d).join("resonanced");
            if cand.is_file() {
                return cand;
            }
        }
    }
    PathBuf::from("resonanced")
}

/// Whether the underlying service manager is reachable for this user.
pub fn systemd_available() -> bool {
    backend::manager_available()
}

/// True if the unit file has been written.
pub fn is_installed() -> bool {
    unit_path().is_file()
}

/// True if the service is currently running.
pub fn is_active() -> bool {
    backend::is_active()
}

/// True if the service is enabled for autostart.
pub fn is_enabled() -> bool {
    backend::is_enabled()
}

/// Combined installed/active/enabled snapshot.
pub fn status() -> Status {
    Status {
        installed: is_installed(),
        active: is_active(),
        enabled: is_enabled(),
    }
}

/// Write (or refresh) the unit file and reload the user manager. Idempotent.
pub fn install() -> io::Result<()> {
    backend::install()
}

/// Remove the unit file (disabling first) and reload the user manager.
pub fn uninstall() -> io::Result<()> {
    backend::uninstall()
}

/// Ensure the unit is installed, then start the service now.
pub fn start() -> io::Result<()> {
    if !is_installed() {
        install()?;
    }
    backend::start()
}

/// Stop the running service.
pub fn stop() -> io::Result<()> {
    backend::stop()
}

/// Restart the service (installing the unit first if needed).
pub fn restart() -> io::Result<()> {
    if !is_installed() {
        install()?;
    }
    backend::restart()
}

/// Enable autostart and start now (installing the unit first if needed).
pub fn enable() -> io::Result<()> {
    if !is_installed() {
        install()?;
    }
    backend::enable()
}

/// Disable autostart and stop now.
pub fn disable() -> io::Result<()> {
    backend::disable()
}
