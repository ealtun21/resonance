//! Linux backend: systemd user-service control.
//!
//! All operations target the *user* manager (`systemctl --user`). The unit is
//! written into `$XDG_CONFIG_HOME/systemd/user/resonanced.service` on demand,
//! with an absolute `ExecStart` resolved from the running binary's directory
//! or `$PATH` so it works for package, `install.sh`, and `~/.local/bin`
//! installs alike.

use std::io;
use std::path::PathBuf;
use std::process::Command;

pub const UNIT_NAME: &str = "resonanced.service";

pub const UNAVAILABLE_MESSAGE: &str =
    "systemctl --user is not available — start the daemon by running `resonanced`";

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
    config_home().join("systemd").join("user").join(UNIT_NAME)
}

fn unit_text() -> String {
    let exec = super::daemon_bin();
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

pub fn manager_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "--version"])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn systemctl(args: &[&str]) -> io::Result<std::process::Output> {
    Command::new("systemctl").arg("--user").args(args).output()
}

fn check(out: &std::process::Output, what: &str) -> io::Result<()> {
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

pub fn is_installed() -> bool {
    unit_path().is_file()
}

pub fn is_active() -> bool {
    systemctl(&["is-active", "--quiet", UNIT_NAME]).is_ok_and(|o| o.status.success())
}

pub fn is_enabled() -> bool {
    systemctl(&["is-enabled", "--quiet", UNIT_NAME]).is_ok_and(|o| o.status.success())
}

pub fn install() -> io::Result<()> {
    let path = unit_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, unit_text())?;
    check(&systemctl(&["daemon-reload"])?, "daemon-reload")
}

pub fn uninstall() -> io::Result<()> {
    let _ = systemctl(&["disable", "--now", UNIT_NAME]);
    let path = unit_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    check(&systemctl(&["daemon-reload"])?, "daemon-reload")
}

pub fn start() -> io::Result<()> {
    check(&systemctl(&["start", UNIT_NAME])?, "start")
}

pub fn stop() -> io::Result<()> {
    check(&systemctl(&["stop", UNIT_NAME])?, "stop")
}

pub fn restart() -> io::Result<()> {
    check(&systemctl(&["restart", UNIT_NAME])?, "restart")
}

pub fn enable() -> io::Result<()> {
    // `systemctl enable` needs the unit file present; ensure it ourselves so
    // the facade doesn't have to pre-install.
    if !is_installed() {
        install()?;
    }
    check(&systemctl(&["enable", "--now", UNIT_NAME])?, "enable")
}

pub fn disable() -> io::Result<()> {
    check(&systemctl(&["disable", "--now", UNIT_NAME])?, "disable")
}
