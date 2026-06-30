//! Best-effort diagnostic log for the APO running inside `audiodg.exe`.
//!
//! audiodg has no console and a restricted token, so we append to a file under
//! `%ProgramData%\Resonance` (or `RESONANCE_APO_LOG`). All failures are ignored
//! — logging must never affect the audio path. Disabled unless the file's
//! parent directory exists, which the daemon/installer create.

use std::io::Write;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn log_path() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("RESONANCE_APO_LOG") {
        return std::path::PathBuf::from(p);
    }
    let base = std::env::var_os("ProgramData").map_or_else(
        || std::path::PathBuf::from(r"C:\ProgramData"),
        std::path::PathBuf::from,
    );
    base.join("Resonance").join("apo.log")
}

/// Append a line to the APO log. Never panics, never blocks the RT path beyond a
/// short mutex; intended for lifecycle events, not per-frame logging.
pub fn line(msg: &str) {
    let _g = LOCK.lock();
    let path = log_path();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "pid={} {}", std::process::id(), msg);
    }
}
