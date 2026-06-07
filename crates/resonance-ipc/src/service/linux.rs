//! Linux service backend: a thin dispatcher.
//!
//! Prefers the systemd *user* manager when `systemctl --user` is reachable
//! (the common case on most desktops, including SteamOS). Otherwise falls back
//! to the freedesktop.org Autostart entry plus direct process control, so the
//! daemon is still installable and controllable on init systems without a
//! per-user service manager (OpenRC, runit, SysV, or a bare session).
//!
//! The choice is made per call from the live environment, so a machine that
//! gains or loses `systemctl --user` between calls still behaves correctly.

use super::{systemd, xdg_autostart};
use std::io;
use std::path::PathBuf;

/// Display name; the active sub-backend uses its own unit/desktop name.
pub const UNIT_NAME: &str = "resonanced";

// The fallback backend is always available, so this is only ever surfaced in
// the (unreachable) case where even the file-based autostart can't be used.
pub const UNAVAILABLE_MESSAGE: &str = systemd::UNAVAILABLE_MESSAGE;

fn use_systemd() -> bool {
    systemd::manager_available()
}

pub fn unit_path() -> PathBuf {
    if use_systemd() {
        systemd::unit_path()
    } else {
        xdg_autostart::unit_path()
    }
}

/// The daemon is always manageable on Linux: systemd when present, otherwise
/// the file-based autostart fallback.
pub fn manager_available() -> bool {
    true
}

pub fn is_installed() -> bool {
    if use_systemd() {
        systemd::is_installed()
    } else {
        xdg_autostart::is_installed()
    }
}

pub fn is_active() -> bool {
    if use_systemd() {
        systemd::is_active()
    } else {
        xdg_autostart::is_active()
    }
}

pub fn is_enabled() -> bool {
    if use_systemd() {
        systemd::is_enabled()
    } else {
        xdg_autostart::is_enabled()
    }
}

pub fn install() -> io::Result<()> {
    if use_systemd() {
        systemd::install()
    } else {
        xdg_autostart::install()
    }
}

pub fn uninstall() -> io::Result<()> {
    if use_systemd() {
        systemd::uninstall()
    } else {
        xdg_autostart::uninstall()
    }
}

pub fn start() -> io::Result<()> {
    if use_systemd() {
        systemd::start()
    } else {
        xdg_autostart::start()
    }
}

pub fn stop() -> io::Result<()> {
    if use_systemd() {
        systemd::stop()
    } else {
        xdg_autostart::stop()
    }
}

pub fn restart() -> io::Result<()> {
    if use_systemd() {
        systemd::restart()
    } else {
        xdg_autostart::restart()
    }
}

pub fn enable() -> io::Result<()> {
    if use_systemd() {
        systemd::enable()
    } else {
        xdg_autostart::enable()
    }
}

pub fn disable() -> io::Result<()> {
    if use_systemd() {
        systemd::disable()
    } else {
        xdg_autostart::disable()
    }
}
