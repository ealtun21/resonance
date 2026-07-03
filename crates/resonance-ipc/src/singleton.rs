//! Generic per-user single-instance guard + cross-process "raise" flag, shared
//! by the GUI, TUI, and tray. Modeled on the daemon's `shutdown.rs`: an
//! exclusive advisory `flock` on Unix (kernel releases it on exit/crash — no
//! stale-PID race) and a PID-liveness check on Windows.

use std::path::PathBuf;

fn runtime_dir() -> PathBuf {
    crate::paths::runtime_dir()
}

fn lock_path(name: &str) -> PathBuf {
    runtime_dir().join(format!("{name}.lock"))
}
fn pid_path(name: &str) -> PathBuf {
    runtime_dir().join(format!("{name}.pid"))
}
fn raise_path(name: &str) -> PathBuf {
    runtime_dir().join(format!("{name}.raise"))
}

/// Held for the process lifetime. Dropping it removes the pidfile; on Unix the
/// kernel drops the `flock` when the inner file closes.
pub struct InstanceGuard {
    #[cfg(unix)]
    _file: std::fs::File,
    pid_path: PathBuf,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.pid_path);
    }
}

/// Try to become the single live instance named `name`.
///
/// # Errors
/// Returns an error if the lock/pid file cannot be created or written.
#[cfg(unix)]
pub fn acquire(name: &str) -> std::io::Result<Option<InstanceGuard>> {
    use std::os::unix::io::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path(name))?;
    // SAFETY: valid open fd; LOCK_NB fails fast with EWOULDBLOCK instead of blocking.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        return match err.raw_os_error() {
            Some(libc::EWOULDBLOCK) => Ok(None),
            _ => Err(err),
        };
    }
    let pid_path = pid_path(name);
    let _ = std::fs::write(&pid_path, std::process::id().to_string());
    Ok(Some(InstanceGuard {
        _file: file,
        pid_path,
    }))
}

/// # Errors
/// Returns an error if the pidfile cannot be written.
#[cfg(windows)]
pub fn acquire(name: &str) -> std::io::Result<Option<InstanceGuard>> {
    if let Some(pid) = running_pid(name) {
        if pid != std::process::id() {
            return Ok(None);
        }
    }
    let pid_path = pid_path(name);
    std::fs::write(&pid_path, std::process::id().to_string())?;
    Ok(Some(InstanceGuard { pid_path }))
}

/// The live PID recorded for `name`, if the process is still alive.
#[must_use]
pub fn running_pid(name: &str) -> Option<u32> {
    let pid = std::fs::read_to_string(pid_path(name))
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()?;
    process_alive(pid).then_some(pid)
}

/// Ask the running instance named `name` to terminate.
///
/// # Errors
/// Returns an error if signalling the process fails for a reason other than it
/// already being gone.
pub fn stop(name: &str) -> std::io::Result<bool> {
    let Some(pid) = running_pid(name) else {
        return Ok(false);
    };
    #[cfg(unix)]
    {
        // SAFETY: kill(pid, SIGTERM) only delivers a signal.
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    #[cfg(windows)]
    {
        std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output()?;
    }
    Ok(true)
}

/// Touch the raise-flag file so a running instance re-shows/focuses its window.
///
/// # Errors
/// Returns an error if the flag file cannot be created.
pub fn request_raise(name: &str) -> std::io::Result<()> {
    std::fs::write(raise_path(name), b"1")
}

/// Consume the raise flag; `true` exactly once per `request_raise`.
#[must_use]
pub fn take_raise(name: &str) -> bool {
    std::fs::remove_file(raise_path(name)).is_ok()
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: kill(pid, 0) delivers no signal — pure liveness probe.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const STILL_ACTIVE: u32 = 259;
    if pid == 0 {
        return false;
    }
    // SAFETY: OpenProcess returns null on failure; on success we own + close the handle.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &raw mut code) != 0;
        CloseHandle(handle);
        ok && code == STILL_ACTIVE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_is_exclusive_then_reentrant_after_drop() {
        let name = "resonance-test-singleton-a";
        let g = acquire(name).unwrap();
        assert!(g.is_some(), "first acquire should win");
        assert_eq!(running_pid(name), Some(std::process::id()));
        // Second acquire while the first guard is alive must fail.
        assert!(acquire(name).unwrap().is_none(), "second acquire must lose");
        drop(g);
        // After releasing, a fresh acquire succeeds again.
        assert!(
            acquire(name).unwrap().is_some(),
            "acquire after drop should win"
        );
    }

    #[test]
    fn raise_flag_round_trips() {
        let name = "resonance-test-singleton-b";
        assert!(!take_raise(name), "no raise pending initially");
        request_raise(name).unwrap();
        assert!(take_raise(name), "raise should be observed once");
        assert!(!take_raise(name), "raise is consumed");
    }
}
