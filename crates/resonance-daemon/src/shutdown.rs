//! Process lifecycle: single-instance pidfile, runtime paths, and cleanup of
//! the socket + pidfile on exit (graceful, signalled, or `Shutdown` command).

use std::path::PathBuf;

/// Unix socket path. Re-exported from `resonance_ipc::paths` so daemon and
/// every client agree on the location.
#[cfg(unix)]
pub fn socket_path() -> PathBuf {
    resonance_ipc::paths::default_socket_path()
}

/// `<runtime_dir>/resonanced.pid` — single-instance lockfile.
pub fn pidfile_path() -> PathBuf {
    resonance_ipc::paths::runtime_dir().join("resonanced.pid")
}

/// Remove the IPC endpoint (Unix socket / Windows port file) and the pidfile.
/// Idempotent; missing files are ignored.
pub fn cleanup() {
    #[cfg(unix)]
    let _ = std::fs::remove_file(socket_path());
    #[cfg(windows)]
    let _ = std::fs::remove_file(resonance_ipc::paths::port_file_path());
    let _ = std::fs::remove_file(pidfile_path());
}

/// Take the single-instance lock by writing our PID to the pidfile.
///
/// If the pidfile names a process that is still alive, refuse to start. A stale
/// pidfile (process gone) is silently reclaimed.
pub fn acquire_pidfile() -> Result<(), String> {
    let path = pidfile_path();
    if let Ok(contents) = std::fs::read_to_string(&path) {
        if let Ok(pid) = contents.trim().parse::<u32>() {
            if pid != std::process::id() && process_alive(pid) {
                return Err(format!(
                    "another resonanced is already running (pid {pid}); \
                     remove {} if this is wrong",
                    path.display()
                ));
            }
        }
    }
    std::fs::write(&path, std::process::id().to_string())
        .map_err(|e| format!("write pidfile {}: {e}", path.display()))
}

/// Whether a process with this PID currently exists.
///
/// Unix: `kill(pid, 0)` — returns 0 if the signal could be sent (process is
/// alive), `ESRCH` if no such process, `EPERM` if it exists but we can't signal
/// it (still alive). Avoids the Linux-specific `/proc` probe.
#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: `kill(pid, 0)` performs no signal delivery; it only checks
    // whether `pid` denotes a process the caller could signal. Pure syscall.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    errno == libc::EPERM
}

/// Windows: open the process with the minimal query right. A valid handle means
/// the PID is live; closing it immediately is required to avoid a handle leak.
/// A reaped/never-existed PID yields a null handle.
#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // A still-running process reports exit code STILL_ACTIVE (259).
    const STILL_ACTIVE: u32 = 259;
    if pid == 0 {
        return false;
    }
    // SAFETY: OpenProcess returns null on failure (no such process / access
    // denied). On success we own the handle and must close it.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        // OpenProcess STILL SUCCEEDS on a process that has been terminated but not
        // yet reaped — so it can't distinguish "running" from "exiting". Without
        // this check, restarting the daemon (taskkill old → spawn new) had the new
        // instance see the dying old PID as alive and bail on the single-instance
        // guard. Treat anything not STILL_ACTIVE as dead.
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        ok && code == STILL_ACTIVE
    }
}

/// Install termination handlers that clean up the IPC endpoint + pidfile and
/// exit. Unix listens for SIGINT/SIGTERM; Windows for Ctrl-C / Ctrl-Break.
pub fn install_signal_handlers() {
    #[cfg(unix)]
    tokio::spawn(async {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("SIGTERM handler: {e}");
                return;
            }
        };
        let mut int = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("SIGINT handler: {e}");
                return;
            }
        };
        tokio::select! {
            _ = term.recv() => tracing::info!("SIGTERM received"),
            _ = int.recv() => tracing::info!("SIGINT received"),
        }
        cleanup();
        std::process::exit(0);
    });

    #[cfg(windows)]
    tokio::spawn(async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!("Ctrl-C handler: {e}");
            return;
        }
        tracing::info!("Ctrl-C received");
        cleanup();
        std::process::exit(0);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_alive() {
        assert!(process_alive(std::process::id()));
    }

    #[test]
    fn pid_zero_is_not_alive() {
        // PID 0 is reserved (kernel scheduler) — kill(0, …) targets the
        // whole process group, not pid 0 the process. Treat it as "not
        // alive" so a corrupted/empty pidfile doesn't wedge startup.
        assert!(!process_alive(0));
    }

    #[test]
    fn improbable_pid_is_not_alive() {
        // PID_MAX_LIMIT is 2^22 on Linux; macOS hands out far smaller pids.
        // Either way this value will not be in use.
        assert!(!process_alive(4_194_305));
    }

    #[cfg(unix)]
    #[test]
    fn pid_1_is_alive_on_unix() {
        // pid 1 is the init/launchd process on every Unix — always running.
        // We use this instead of a `fork()`-based test to keep tests pure.
        // On macOS we may get EPERM (process exists, caller can't signal it)
        // — `process_alive` treats that as "alive", which is correct.
        assert!(process_alive(1));
    }
}
