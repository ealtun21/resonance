//! Fallback service backend for platforms without a real implementation.
//!
//! Returns "manager unavailable" everywhere; CLI/TUI surface a friendly
//! "no service manager on this platform" rather than crashing.

use std::io;
use std::path::PathBuf;

pub const UNIT_NAME: &str = "resonanced";

pub const UNAVAILABLE_MESSAGE: &str =
    "no service manager on this platform — start the daemon by running `resonanced`";

pub fn unit_path() -> PathBuf {
    PathBuf::from("/dev/null")
}

pub fn manager_available() -> bool {
    false
}

pub fn is_active() -> bool {
    false
}
pub fn is_enabled() -> bool {
    false
}

fn unsupported() -> io::Result<()> {
    Err(io::Error::other(
        "service control is not supported on this platform",
    ))
}

pub fn install() -> io::Result<()> {
    unsupported()
}
pub fn uninstall() -> io::Result<()> {
    unsupported()
}
pub fn start() -> io::Result<()> {
    unsupported()
}
pub fn stop() -> io::Result<()> {
    unsupported()
}
pub fn restart() -> io::Result<()> {
    unsupported()
}
pub fn enable() -> io::Result<()> {
    unsupported()
}
pub fn disable() -> io::Result<()> {
    unsupported()
}
