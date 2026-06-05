//! Process lifecycle: single-instance pidfile, runtime paths, and cleanup of
//! the socket + pidfile on exit (graceful, signalled, or `Shutdown` command).

use std::path::PathBuf;

/// `$XDG_RUNTIME_DIR` (falls back to `/tmp`).
fn runtime_dir() -> PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

/// Unix socket path: `$RESONANCE_SOCKET` else `$XDG_RUNTIME_DIR/resonance.sock`.
pub fn socket_path() -> PathBuf {
    if let Ok(p) = std::env::var(resonance_ipc::SOCKET_PATH_ENV) {
        return PathBuf::from(p);
    }
    runtime_dir().join(resonance_ipc::DEFAULT_SOCKET_FILENAME)
}

/// `$XDG_RUNTIME_DIR/resonanced.pid`.
pub fn pidfile_path() -> PathBuf {
    runtime_dir().join("resonanced.pid")
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

/// Whether a process with this PID currently exists (Linux `/proc`).
fn process_alive(pid: u32) -> bool {
    PathBuf::from(format!("/proc/{pid}")).exists()
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
    fn improbable_pid_is_not_alive() {
        // PID_MAX_LIMIT is 2^22; this is above any real pid.
        assert!(!process_alive(4_194_305));
    }
}
