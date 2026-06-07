//! Per-user service control for `resonanced`, shared by every client.
//!
//! Cross-platform façade with one implementation per OS:
//!   - Linux: systemd user-service unit at `$XDG_CONFIG_HOME/systemd/user/resonanced.service`,
//!     or, where `systemctl --user` is absent (OpenRC/runit/SysV/bare session),
//!     a freedesktop Autostart `.desktop` plus direct process control.
//!   - macOS: launchd LaunchAgent plist at `~/Library/LaunchAgents/com.ealtun21.resonanced.plist`
//!
//! Every backend exposes the same operations so callers stay platform-agnostic:
//! `status()`, `install()`, `uninstall()`, `start()`, `stop()`, `restart()`,
//! `enable()`, `disable()`, and the helper booleans `is_installed/active/enabled`.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod systemd;
#[cfg(target_os = "linux")]
mod xdg_autostart;
#[cfg(target_os = "linux")]
use linux as backend;

#[cfg(target_os = "macos")]
use crate::launchd as backend;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as backend;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod stub;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
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
    // Platform-correct binary name (Windows appends `.exe`).
    const BIN: &str = if cfg!(windows) {
        "resonanced.exe"
    } else {
        "resonanced"
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join(BIN);
            if cand.is_file() {
                return cand;
            }
        }
    }
    // `split_paths` uses the platform's PATH separator (`:` Unix, `;` Windows).
    if let Some(path) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&path) {
            let cand = d.join(BIN);
            if cand.is_file() {
                return cand;
            }
        }
    }
    PathBuf::from(BIN)
}

/// Whether the underlying service manager is reachable for this user.
pub fn manager_available() -> bool {
    backend::manager_available()
}

/// Backend-specific explanation shown when `manager_available()` is false, so a
/// client never prints "systemctl" on macOS or Windows.
pub fn manager_unavailable_message() -> &'static str {
    backend::UNAVAILABLE_MESSAGE
}

/// True if the service is installed (unit/plist on disk, or the autostart
/// mechanism present on platforms without a unit file).
pub fn is_installed() -> bool {
    backend::is_installed()
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
