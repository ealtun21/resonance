//! Windows backend: per-user autostart via the `HKCU\…\Run` registry key.
//!
//! A Windows *Service* (SCM) runs in session 0, which has no audio endpoints,
//! and a Task Scheduler task can't be created from a non-elevated process
//! ("Access is denied"). The per-user Run key needs no elevation and starts the
//! daemon at logon in the interactive session (where WASAPI endpoints live) —
//! the practical equivalent of the systemd/launchd *user* service.
//!
//! Mapping to the cross-platform `service` API:
//!   - install   → write a marker file (so `service::is_installed()` is true).
//!   - enable    → add the Run key (autostart at logon) and start now.
//!   - disable   → remove the Run key and stop the daemon.
//!   - start/stop/restart → spawn / kill `resonanced.exe`.
//!   - is_active  → a `resonanced.exe` process is running.
//!   - is_enabled → the Run key value is present.

use std::io;
use std::path::PathBuf;
use std::process::Command;

pub const UNIT_NAME: &str = "Resonance";

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Resonance";

fn config_dir() -> PathBuf {
    crate::paths::config_dir()
}

/// Install marker — its presence is what `service::is_installed()` checks.
pub fn unit_path() -> PathBuf {
    config_dir().join("resonanced-autostart.txt")
}

/// The Run key is always available (no manager to probe).
pub fn manager_available() -> bool {
    true
}

/// The daemon is up if a `resonanced.exe` process exists.
pub fn is_active() -> bool {
    let Ok(out) = Command::new("tasklist")
        .args(["/fi", "imagename eq resonanced.exe", "/nh"])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&out.stdout)
        .to_lowercase()
        .contains("resonanced.exe")
}

/// Enabled = the Run key value exists.
pub fn is_enabled() -> bool {
    Command::new("reg")
        .args(["query", RUN_KEY, "/v", RUN_VALUE])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn daemon_exe() -> String {
    super::daemon_bin().display().to_string()
}

pub fn install() -> io::Result<()> {
    let path = unit_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, format!("{}\n", daemon_exe()))?;
    Ok(())
}

pub fn uninstall() -> io::Result<()> {
    let _ = disable();
    let path = unit_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// Spawn the daemon detached (no console window, survives the launching process).
fn spawn_daemon() -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Command::new(super::daemon_bin())
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()?;
    Ok(())
}

pub fn start() -> io::Result<()> {
    // The daemon's pidfile guard makes a redundant spawn a no-op.
    spawn_daemon()
}

pub fn stop() -> io::Result<()> {
    let _ = Command::new("taskkill")
        .args(["/im", "resonanced.exe", "/f"])
        .output();
    Ok(())
}

pub fn restart() -> io::Result<()> {
    let _ = stop();
    start()
}

pub fn enable() -> io::Result<()> {
    let exe = daemon_exe();
    let out = Command::new("reg")
        .args([
            "add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &exe, "/f",
        ])
        .output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(io::Error::other(format!("reg add Run key: {}", err.trim())));
    }
    if !is_active() {
        let _ = start();
    }
    Ok(())
}

pub fn disable() -> io::Result<()> {
    let _ = Command::new("reg")
        .args(["delete", RUN_KEY, "/v", RUN_VALUE, "/f"])
        .output();
    stop()
}
