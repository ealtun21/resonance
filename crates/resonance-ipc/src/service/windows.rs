//! Windows backend: per-user autostart via the `HKCU\…\Run` registry key.
//!
//! A Windows *Service* (SCM) runs in session 0, which has no audio endpoints,
//! and a Task Scheduler task can't be created from a non-elevated process
//! ("Access is denied"). The per-user Run key needs no elevation and starts the
//! daemon at logon in the interactive session (where WASAPI endpoints live) —
//! the practical equivalent of the systemd/launchd *user* service.
//!
//! Mapping to the cross-platform `service` API:
//!   - install   → nothing to do (no unit file; autostart is the Run key).
//!   - enable    → add the Run key (autostart at logon) and start now.
//!   - disable   → remove the Run key and stop the daemon.
//!   - start/stop/restart → spawn / kill `resonanced.exe`.
//!   - is_installed → the daemon binary is resolvable (it's what the installer ships).
//!   - is_active  → a `resonanced.exe` process is running.
//!   - is_enabled → the Run key value is present.

use std::io;
use std::path::PathBuf;
use std::process::Command;

/// `Command` for a console program (`reg`, `taskkill`) that does NOT pop a
/// console window. The GUI/daemon are `#![windows_subsystem = "windows"]`, so a
/// console-subsystem child with no creation flags makes Windows allocate a fresh
/// console window that flashes open/closed — the reported "cmd windows flashing
/// under the resonance-gui icon" was the status poll spawning `tasklist`/`reg`
/// here every ~1.5 s.
fn hidden(program: &str) -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut c = Command::new(program);
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

pub const UNIT_NAME: &str = "Resonance";

pub const UNAVAILABLE_MESSAGE: &str =
    "the autostart registry is not writable — start the daemon by running resonanced.exe";

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Resonance";

/// Informational only (Windows has no unit file — autostart lives in the
/// HKCU Run key). Returned for display/parity with the other backends.
pub fn unit_path() -> PathBuf {
    PathBuf::from(format!(r"{RUN_KEY}\{RUN_VALUE}"))
}

/// The Run key is always available (no manager to probe).
pub fn manager_available() -> bool {
    true
}

/// No unit file to write: the daemon binary is the install (shipped by the
/// installer), and autostart is a separate `enable` (the Run key).
pub fn is_installed() -> bool {
    super::daemon_bin().is_file()
}

/// The daemon is up if it's accepting IPC connections. Uses the loopback
/// liveness probe (no subprocess) rather than `tasklist` — accurate (a bound
/// port is what actually matters) and never pops a console window.
pub fn is_active() -> bool {
    crate::transport::is_reachable()
}

/// Enabled = the Run key value exists.
pub fn is_enabled() -> bool {
    hidden("reg")
        .args(["query", RUN_KEY, "/v", RUN_VALUE])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn daemon_exe() -> String {
    super::daemon_bin().display().to_string()
}

/// Nothing to install: there is no unit file, and the daemon binary is placed
/// by the installer. Autostart is configured separately via `enable`.
pub fn install() -> io::Result<()> {
    Ok(())
}

pub fn uninstall() -> io::Result<()> {
    disable()
}

/// Spawn the daemon detached (no console window, survives the launching process).
fn spawn_daemon() -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // Null the daemon's stdio. Otherwise the detached child inherits the
    // launcher's stdout/stderr handles: a piped CLI (`resonance daemon restart`)
    // then blocks until the daemon exits (the pipe stays open), and the daemon's
    // own stderr (e.g. a startup bail) leaks back onto the launcher's output.
    Command::new(super::daemon_bin())
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

pub fn start() -> io::Result<()> {
    // Idempotent: if the daemon already answers IPC, don't spawn a second
    // `resonanced.exe` that would just bail on the pidfile guard (a momentary
    // ghost process, and on a locked-down box an unnecessary launch that can
    // trip security/AV prompts). The pidfile guard still backstops a race.
    if is_active() {
        return Ok(());
    }
    spawn_daemon()
}

pub fn stop() -> io::Result<()> {
    // `/t` kills the process tree; taskkill blocks until termination is
    // initiated. A new daemon spawned right after still reclaims the pidfile:
    // the daemon's single-instance check treats a terminating PID as dead
    // (GetExitCodeProcess != STILL_ACTIVE), so no exit-poll is needed here.
    let _ = hidden("taskkill")
        .args(["/im", "resonanced.exe", "/t", "/f"])
        .output();
    Ok(())
}

pub fn restart() -> io::Result<()> {
    stop()?;
    // `taskkill` returns once termination is *initiated*, before the OS reaps the
    // PID. If we spawn immediately, the new daemon's single-instance pidfile
    // guard can still see the old PID mid-exit and bail, leaving nothing running.
    // Wait for the old instance to actually stop accepting IPC, bounded so a
    // wedged process can't hang `daemon restart` (commit 4c277ac dropped the old
    // ~2.5s tasklist poll for being slow — this is the minimal fast equivalent
    // using the no-subprocess loopback probe, capped at ~500ms / ~15ms steps).
    for _ in 0..33 {
        if !is_active() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    start()
}

pub fn enable() -> io::Result<()> {
    // Quote the path: a spaced "C:\Program Files\..." Run value must be quoted
    // to launch correctly at logon. Matches what the installer writes.
    let exe = format!("\"{}\"", daemon_exe());
    let out = hidden("reg")
        .args([
            "add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &exe, "/f",
        ])
        .output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(io::Error::other(format!("reg add Run key: {}", err.trim())));
    }
    // `enable` must leave the daemon running now (matches systemd `enable --now`
    // / xdg `enable`), not merely set the Run key for next logon. Propagate a
    // spawn failure so the UI sees a real error instead of a silent no-op.
    if !is_active() {
        start()?;
    }
    Ok(())
}

pub fn disable() -> io::Result<()> {
    let _ = hidden("reg")
        .args(["delete", RUN_KEY, "/v", RUN_VALUE, "/f"])
        .output();
    stop()
}
