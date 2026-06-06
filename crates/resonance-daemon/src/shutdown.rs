//! Process lifecycle: single-instance pidfile, runtime paths, and cleanup of
//! the socket + pidfile on exit (graceful, signalled, or `Shutdown` command).

use std::path::PathBuf;

/// Unix socket path. Re-exported from `resonance_ipc::paths` so daemon and
/// every client agree on the location.
pub fn socket_path() -> PathBuf {
    resonance_ipc::paths::default_socket_path()
}

/// `<runtime_dir>/resonanced.pid` — single-instance lockfile.
pub fn pidfile_path() -> PathBuf {
    resonance_ipc::paths::runtime_dir().join("resonanced.pid")
}

/// Remove the socket and pidfile. Idempotent; missing files are ignored.
pub fn cleanup() {
    let _ = std::fs::remove_file(socket_path());
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
/// Uses `kill(pid, 0)`: returns 0 if the signal could be sent (process is
/// alive), -1 with `errno = ESRCH` if no such process. This is the POSIX way
/// — works on Linux, macOS, BSD, and avoids the Linux-specific `/proc` probe.
fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: `kill(pid, 0)` performs no signal delivery; it only checks
    // whether `pid` denotes a process the caller could signal. No memory
    // safety concerns — pure syscall.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    // Errno ESRCH = process gone (reclaim pidfile). EPERM = process exists but
    // we lack permission to signal it (still alive — refuse to start).
    // SAFETY: errno_location returns a thread-local pointer that's always valid.
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    errno == libc::EPERM
}

/// Install SIGINT/SIGTERM handlers that clean up and exit.
pub fn install_signal_handlers() {
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

    #[test]
    fn pid_1_is_alive_on_unix() {
        // pid 1 is the init/launchd process on every Unix — always running.
        // We use this instead of a `fork()`-based test to keep tests pure.
        // On macOS we may get EPERM (process exists, caller can't signal it)
        // — `process_alive` treats that as "alive", which is correct.
        assert!(process_alive(1));
    }
}
