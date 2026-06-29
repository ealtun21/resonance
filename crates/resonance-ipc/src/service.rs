//! Per-user service control for `resonanced`, shared by every client.
//!
//! Cross-platform façade with one implementation per OS:
//!   - Linux: systemd user-service unit at `$XDG_CONFIG_HOME/systemd/user/resonanced.service`,
//!     or, where `systemctl --user` is absent (OpenRC/runit/SysV/bare session),
//!     a freedesktop Autostart `.desktop` plus direct process control.
//!   - macOS: launchd `LaunchAgent` plist at `~/Library/LaunchAgents/com.ealtun21.resonanced.plist`
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
mod launchd;
#[cfg(target_os = "macos")]
use launchd as backend;

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
#[must_use]
pub fn unit_path() -> PathBuf {
    backend::unit_path()
}

/// Resolve the `resonanced` binary: prefer a sibling of the current executable
/// (covers a co-installed CLI/daemon pair), then `$PATH`, else the bare name.
#[must_use]
pub fn daemon_bin() -> PathBuf {
    // Platform-correct binary name (Windows appends `.exe`).
    const BIN: &str = if cfg!(windows) {
        "resonanced.exe"
    } else {
        "resonanced"
    };
    if let Ok(exe) = std::env::current_exe() {
        // Check the sibling next to the executable as-invoked AND next to its
        // canonicalised path. macOS `current_exe()` returns the path used to
        // launch — for a symlinked CLI (e.g. `~/.local/bin/resonance` →
        // `…/Resonance.app/Contents/MacOS/resonance`) that's the symlink, whose
        // directory has no `resonanced` sibling. Resolving the link lands us
        // inside the bundle where `resonanced` lives. Without this the launchd
        // plist gets a bare `resonanced` with no path and exits EX_CONFIG.
        let resolved = exe.canonicalize().ok();
        for base in [Some(&exe), resolved.as_ref()].into_iter().flatten() {
            if let Some(dir) = base.parent() {
                let cand = dir.join(BIN);
                if cand.is_file() {
                    return cand;
                }
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
#[must_use]
pub fn manager_available() -> bool {
    backend::manager_available()
}

/// Backend-specific explanation shown when `manager_available()` is false, so a
/// client never prints "systemctl" on macOS or Windows.
#[must_use]
pub fn manager_unavailable_message() -> &'static str {
    backend::UNAVAILABLE_MESSAGE
}

/// True if the service is installed (unit/plist on disk, or the autostart
/// mechanism present on platforms without a unit file).
#[must_use]
pub fn is_installed() -> bool {
    backend::is_installed()
}

/// True if the service is currently running.
#[must_use]
pub fn is_active() -> bool {
    backend::is_active()
}

/// True if the service is enabled for autostart.
#[must_use]
pub fn is_enabled() -> bool {
    backend::is_enabled()
}

/// Combined installed/active/enabled snapshot.
#[must_use]
pub fn status() -> Status {
    Status {
        installed: is_installed(),
        active: is_active(),
        enabled: is_enabled(),
    }
}

/// Generous ceiling for a start/restart to actually begin serving: the daemon
/// initialises its audio backend (device enumeration + the system tap) before
/// it binds the IPC socket, which can take a beat on first launch.
const START_SETTLE: std::time::Duration = std::time::Duration::from_secs(6);
/// Ceiling for a stop to stop serving (SIGTERM → audio teardown → socket gone).
const STOP_SETTLE: std::time::Duration = std::time::Duration::from_secs(3);

/// Block until the daemon's IPC endpoint reaches the desired reachability, or
/// `timeout` elapses. Service-manager state transitions (process spawn, the
/// SIGTERM teardown of the audio engine) are asynchronous, so reading status
/// the instant a lifecycle op returns races the transition and reports the OLD
/// state — the source of the "Start says running but nothing's there yet" and
/// "Stop still says running" glitches. Polling the actual socket makes the
/// reported `status()` (and the GUI's "busy" gate) reflect reality.
fn settle(reachable: bool, timeout: std::time::Duration) {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if crate::transport::is_reachable() == reachable {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
}

/// Write (or refresh) the unit file and reload the user manager. Idempotent.
///
/// # Errors
/// Returns an error if the unit file cannot be written or the service manager
/// reload fails.
pub fn install() -> io::Result<()> {
    backend::install()
}

/// Remove the unit file (disabling first) and reload the user manager.
///
/// # Errors
/// Returns an error if the unit file cannot be removed or the service manager
/// reload fails.
pub fn uninstall() -> io::Result<()> {
    backend::uninstall()
}

/// Ensure the unit is installed, then start the service now. Returns once the
/// daemon is actually reachable (or the settle window elapses).
///
/// # Errors
/// Returns an error if installing the unit or asking the service manager to
/// start it fails.
pub fn start() -> io::Result<()> {
    if !is_installed() {
        install()?;
    }
    backend::start()?;
    settle(true, START_SETTLE);
    Ok(())
}

/// Stop the running service. Returns once it has actually stopped serving.
///
/// # Errors
/// Returns an error if the service manager fails to stop the service.
pub fn stop() -> io::Result<()> {
    backend::stop()?;
    settle(false, STOP_SETTLE);
    Ok(())
}

/// Restart the service (installing the unit first if needed). Returns once the
/// daemon is serving again.
///
/// # Errors
/// Returns an error if installing the unit or asking the service manager to
/// restart it fails.
pub fn restart() -> io::Result<()> {
    if !is_installed() {
        install()?;
    }
    backend::restart()?;
    settle(true, START_SETTLE);
    Ok(())
}

/// Enable autostart and start now. Each backend's `enable` ensures the unit is
/// installed itself (the macOS plist bootstrap is part of `enable`), so the
/// facade does not pre-install — doing so double-bootstrapped launchd.
///
/// # Errors
/// Returns an error if the service manager fails to enable or start the service.
pub fn enable() -> io::Result<()> {
    backend::enable()?;
    settle(true, START_SETTLE);
    Ok(())
}

/// Disable autostart and stop now.
///
/// # Errors
/// Returns an error if the service manager fails to disable or stop the service.
pub fn disable() -> io::Result<()> {
    backend::disable()?;
    settle(false, STOP_SETTLE);
    Ok(())
}
