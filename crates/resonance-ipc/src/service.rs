//! systemd user-service control for `resonanced`, shared by every client
//! (CLI / TUI / GUI) so the daemon can be started, stopped, enabled for
//! autostart, and installed without the user ever typing a `systemctl` line.
//!
//! All operations target the *user* manager (`systemctl --user`). The unit is
//! written into `$XDG_CONFIG_HOME/systemd/user/resonanced.service` on demand,
//! with an absolute `ExecStart` resolved from the running binary's directory or
//! `$PATH` so it works for package, `install.sh`, and `~/.local/bin` installs
//! alike.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Unit file name for the daemon's systemd user service.
pub const UNIT_NAME: &str = "resonanced.service";

/// Installed/active/enabled snapshot of the user service.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Status {
    /// The unit file exists under the user systemd config dir.
    pub installed: bool,
    /// The service is currently running (`systemctl is-active`).
    pub active: bool,
    /// The service is enabled for autostart (`systemctl is-enabled`).
    pub enabled: bool,
}

fn config_home() -> PathBuf {
    if let Ok(p) = std::env::var("XDG_CONFIG_HOME") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".config")
}

/// Path the unit file is written to: `$XDG_CONFIG_HOME/systemd/user/resonanced.service`.
pub fn unit_path() -> PathBuf {
    config_home().join("systemd").join("user").join(UNIT_NAME)
}

/// Resolve the `resonanced` binary: prefer a sibling of the current executable
/// (covers a co-installed CLI/daemon pair), then `$PATH`, else the bare name.
pub fn daemon_bin() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("resonanced");
            if cand.is_file() {
                return cand;
            }
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for d in path.split(':').filter(|s| !s.is_empty()) {
            let cand = Path::new(d).join("resonanced");
            if cand.is_file() {
                return cand;
            }
        }
    }
    PathBuf::from("resonanced")
}

/// Render the unit file text with an absolute `ExecStart`.
fn unit_text() -> String {
    let exec = daemon_bin();
    format!(
        "[Unit]\n\
         Description=Resonance terminal EQ daemon (PipeWire)\n\
         Documentation=https://github.com/ealtun21/resonance\n\
         After=pipewire.service pipewire-pulse.service wireplumber.service\n\
         Wants=pipewire.service\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exec}\n\
         Environment=RUST_LOG=info\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         KillSignal=SIGTERM\n\
         TimeoutStopSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exec = exec.display(),
    )
}

/// Whether `systemctl` is present and a user manager is reachable.
pub fn systemd_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn systemctl(args: &[&str]) -> io::Result<std::process::Output> {
    Command::new("systemctl").arg("--user").args(args).output()
}

/// Map a non-zero `systemctl` exit into an `io::Error` carrying stderr.
fn check(out: std::process::Output, what: &str) -> io::Result<()> {
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        Err(io::Error::other(if err.is_empty() {
            format!("systemctl {what} failed")
        } else {
            format!("systemctl {what}: {err}")
        }))
    }
}

/// True if the unit file has been written.
pub fn is_installed() -> bool {
    unit_path().is_file()
}

/// True if the service is currently running.
pub fn is_active() -> bool {
    systemctl(&["is-active", "--quiet", UNIT_NAME])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// True if the service is enabled for autostart.
pub fn is_enabled() -> bool {
    systemctl(&["is-enabled", "--quiet", UNIT_NAME])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Combined installed/active/enabled snapshot.
pub fn status() -> Status {
    Status {
        installed: is_installed(),
        active: is_active(),
        enabled: is_enabled(),
    }
}

/// Write (or refresh) the unit file and reload the user manager. Idempotent.
pub fn install() -> io::Result<()> {
    let path = unit_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, unit_text())?;
    check(systemctl(&["daemon-reload"])?, "daemon-reload")
}

/// Remove the unit file (disabling first) and reload the user manager.
pub fn uninstall() -> io::Result<()> {
    let _ = systemctl(&["disable", "--now", UNIT_NAME]);
    let path = unit_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    check(systemctl(&["daemon-reload"])?, "daemon-reload")
}

/// Ensure the unit is installed, then start the service now.
pub fn start() -> io::Result<()> {
    if !is_installed() {
        install()?;
    }
    check(systemctl(&["start", UNIT_NAME])?, "start")
}

/// Stop the running service.
pub fn stop() -> io::Result<()> {
    check(systemctl(&["stop", UNIT_NAME])?, "stop")
}

/// Restart the service (installing the unit first if needed).
pub fn restart() -> io::Result<()> {
    if !is_installed() {
        install()?;
    }
    check(systemctl(&["restart", UNIT_NAME])?, "restart")
}

/// Enable autostart and start now (installing the unit first if needed).
pub fn enable() -> io::Result<()> {
    if !is_installed() {
        install()?;
    }
    check(systemctl(&["enable", "--now", UNIT_NAME])?, "enable")
}

/// Disable autostart and stop now.
pub fn disable() -> io::Result<()> {
    check(systemctl(&["disable", "--now", UNIT_NAME])?, "disable")
}
