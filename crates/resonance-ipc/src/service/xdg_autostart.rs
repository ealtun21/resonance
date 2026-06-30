//! Linux fallback backend: no service manager, just the freedesktop.org
//! Autostart spec plus direct process control.
//!
//! Used on systems without a reachable `systemctl --user` (Artix/OpenRC,
//! Void/runit, Devuan/SysV, Gentoo, Alpine, or a bare session). A per-user
//! audio daemon does not belong in a *system* init (those run as root, once,
//! before the user's `PipeWire` session exists), so instead of an init script we:
//!
//!   - autostart at login via `$XDG_CONFIG_HOME/autostart/resonanced.desktop`
//!     (honoured by every desktop session regardless of the init system), and
//!   - start/stop the process directly, taking the same single-instance
//!     pidfile the daemon writes (`<runtime_dir>/resonanced.pid`).
//!
//! `Hidden=true` in the .desktop is the spec's "installed but disabled" state,
//! which lets us mirror systemd's install-vs-enable distinction.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub const UNIT_NAME: &str = "resonanced.desktop";

fn config_home() -> PathBuf {
    if let Ok(p) = std::env::var("XDG_CONFIG_HOME") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".config")
}

pub fn unit_path() -> PathBuf {
    config_home().join("autostart").join(UNIT_NAME)
}

fn pidfile_path() -> PathBuf {
    crate::paths::runtime_dir().join("resonanced.pid")
}

/// Build the autostart .desktop entry. `enabled = false` writes `Hidden=true`
/// so the entry is kept on disk but skipped at login (the spec's disabled
/// state), matching systemd's "installed but not enabled".
fn desktop_text(enabled: bool) -> String {
    let exec = super::daemon_bin();
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Resonance EQ daemon\n\
         Comment=Resonance terminal EQ daemon (PipeWire)\n\
         Exec={exec}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled={enabled}\n\
         Hidden={hidden}\n",
        exec = exec.display(),
        enabled = enabled,
        hidden = !enabled,
    )
}

fn write_desktop(enabled: bool) -> io::Result<()> {
    let path = unit_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, desktop_text(enabled))
}

/// Whether a PID currently names a live process. Linux-only path, so probe
/// `/proc/<pid>` directly (no libc dependency).
fn process_alive(pid: u32) -> bool {
    pid != 0 && PathBuf::from(format!("/proc/{pid}")).exists()
}

fn running_pid() -> Option<u32> {
    let contents = std::fs::read_to_string(pidfile_path()).ok()?;
    let pid = contents.trim().parse::<u32>().ok()?;
    process_alive(pid).then_some(pid)
}

pub fn is_installed() -> bool {
    unit_path().is_file()
}

pub fn is_active() -> bool {
    running_pid().is_some()
}

pub fn is_enabled() -> bool {
    match std::fs::read_to_string(unit_path()) {
        // Present and not hidden → it will autostart at login.
        Ok(text) => !text
            .lines()
            .any(|l| l.trim().eq_ignore_ascii_case("Hidden=true")),
        Err(_) => false,
    }
}

pub fn install() -> io::Result<()> {
    // Write the entry disabled: present on disk, skipped at login until enabled.
    if unit_path().is_file() {
        return Ok(());
    }
    write_desktop(false)
}

pub fn uninstall() -> io::Result<()> {
    let _ = stop();
    let path = unit_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

pub fn start() -> io::Result<()> {
    if is_active() {
        return Ok(());
    }
    let exe = super::daemon_bin();
    // Detach from the caller's controlling terminal and process group so the
    // daemon outlives a CLI/GUI that triggered it, and discard its stdio.
    Command::new(&exe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
        .map_err(|e| io::Error::other(format!("spawn {}: {e}", exe.display())))
}

pub fn stop() -> io::Result<()> {
    let Some(pid) = running_pid() else {
        return Ok(());
    };
    // SAFETY: `kill(pid, SIGTERM)` only delivers a signal; no memory effects.
    // The daemon's SIGTERM handler unlinks its socket + pidfile and exits.
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn restart() -> io::Result<()> {
    stop()?;
    // Give the old process a moment to release the pidfile and socket.
    std::thread::sleep(std::time::Duration::from_millis(200));
    start()
}

pub fn enable() -> io::Result<()> {
    write_desktop(true)?;
    start()
}

pub fn disable() -> io::Result<()> {
    // Keep the entry on disk but hidden, so it no longer autostarts.
    write_desktop(false)?;
    stop()
}
