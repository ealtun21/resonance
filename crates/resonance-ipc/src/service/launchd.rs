//! macOS backend: launchd LaunchAgent control for `resonanced`.
//!
//! Layout:
//!   - Plist at `~/Library/LaunchAgents/com.ealtun21.resonanced.plist`
//!   - Loaded into the per-user GUI domain (`gui/<uid>`) via `launchctl`.
//!
//! Operations map naturally to launchd:
//!   - `install`  → write the plist + `launchctl bootstrap gui/<uid> <path>` if not loaded
//!   - `uninstall`→ `launchctl bootout gui/<uid>/<label>` + delete plist
//!   - `start`    → `launchctl kickstart gui/<uid>/<label>`
//!   - `stop`     → `launchctl kill SIGTERM gui/<uid>/<label>`
//!   - `enable`   → set `RunAtLoad=true` in plist + bootstrap
//!   - `disable`  → `launchctl bootout` (load on demand, not at login)
//!
//! launchd's "enabled" notion = "loaded into a domain". Our `enable` ensures
//! it's loaded AND runs at login; `disable` boots it out so it's neither
//! running nor auto-loaded.

use std::io;
use std::path::PathBuf;
use std::process::Command;

pub const UNIT_NAME: &str = "com.ealtun21.resonanced";

pub const UNAVAILABLE_MESSAGE: &str =
    "launchctl is not available — start the daemon by running `resonanced`";

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

pub fn unit_path() -> PathBuf {
    home()
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{UNIT_NAME}.plist"))
}

/// Render the LaunchAgent plist. `RunAtLoad=true` so the agent comes up at
/// login (matches systemd's `WantedBy=default.target`).
fn plist_text() -> String {
    let exec = super::daemon_bin();
    let log_dir = home().join("Library").join("Logs").join("resonance");
    let stdout_log = log_dir.join("resonanced.out.log");
    let stderr_log = log_dir.join("resonanced.err.log");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exec}</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>RUST_LOG</key>
        <string>info</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ProcessType</key>
    <string>Interactive</string>
    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
</dict>
</plist>
"#,
        label = UNIT_NAME,
        exec = exec.display(),
        stdout = stdout_log.display(),
        stderr = stderr_log.display(),
    )
}

fn uid() -> u32 {
    // SAFETY: getuid never fails and has no preconditions.
    unsafe { libc::getuid() }
}

fn service_target() -> String {
    format!("gui/{}/{}", uid(), UNIT_NAME)
}

fn domain_target() -> String {
    format!("gui/{}", uid())
}

fn launchctl(args: &[&str]) -> io::Result<std::process::Output> {
    Command::new("launchctl").args(args).output()
}

/// Treat launchctl's "already loaded" (errno 37) and "no such process" (errno
/// 113) as non-fatal — they're the success cases for idempotent transitions.
fn check_launchctl(out: std::process::Output, what: &str) -> io::Result<()> {
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let err = err.trim().to_string();
    Err(io::Error::other(if err.is_empty() {
        format!("launchctl {what} failed")
    } else {
        format!("launchctl {what}: {err}")
    }))
}

pub fn manager_available() -> bool {
    Command::new("launchctl")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `launchctl print` exits 0 when the service exists in the domain. Pair with
/// PID inspection to distinguish "loaded" from "loaded and running".
pub fn is_installed() -> bool {
    unit_path().is_file()
}

pub fn is_active() -> bool {
    let Ok(out) = launchctl(&["print", &service_target()]) else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // `state = running` or a non-zero `pid = N` both indicate active.
    text.contains("state = running")
        || text
            .lines()
            .any(|l| l.trim_start().starts_with("pid =") && !l.contains("pid = 0"))
}

/// "Enabled" means the service is loaded into the user domain (so it will
/// start at login per `RunAtLoad`). `launchctl print` exits 0 iff loaded.
pub fn is_enabled() -> bool {
    launchctl(&["print", &service_target()])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ensure_log_dir() -> io::Result<()> {
    let dir = home().join("Library").join("Logs").join("resonance");
    std::fs::create_dir_all(&dir)
}

pub fn install() -> io::Result<()> {
    let path = unit_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    ensure_log_dir()?;
    std::fs::write(&path, plist_text())?;

    // If a previous version was already loaded, bootout so the new plist is
    // honoured on the next bootstrap. Ignore failures (not loaded → fine).
    let _ = launchctl(&["bootout", &service_target()]);
    let plist = path.to_string_lossy().into_owned();
    check_launchctl(
        launchctl(&["bootstrap", &domain_target(), &plist])?,
        "bootstrap",
    )
}

pub fn uninstall() -> io::Result<()> {
    let _ = launchctl(&["bootout", &service_target()]);
    let path = unit_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

pub fn start() -> io::Result<()> {
    // `kickstart` will start the service in its current domain (and load it
    // first if KeepAlive demands). If not loaded at all, bootstrap first.
    if !is_enabled() {
        install()?;
    }
    check_launchctl(
        launchctl(&["kickstart", "-k", &service_target()])?,
        "kickstart",
    )
}

pub fn stop() -> io::Result<()> {
    check_launchctl(launchctl(&["kill", "SIGTERM", &service_target()])?, "kill")
}

pub fn restart() -> io::Result<()> {
    if !is_enabled() {
        install()?;
    }
    check_launchctl(
        launchctl(&["kickstart", "-k", &service_target()])?,
        "kickstart -k",
    )
}

pub fn enable() -> io::Result<()> {
    install()
}

pub fn disable() -> io::Result<()> {
    let _ = launchctl(&["bootout", &service_target()]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_path_lives_under_launch_agents() {
        let p = unit_path();
        let s = p.display().to_string();
        assert!(
            s.contains("Library/LaunchAgents"),
            "expected LaunchAgents path, got {s}"
        );
        assert!(s.ends_with(".plist"), "expected .plist suffix, got {s}");
        assert!(s.contains(UNIT_NAME), "expected unit name in path, got {s}");
    }

    #[test]
    fn service_target_includes_uid_and_label() {
        let t = service_target();
        let expected_suffix = format!("/{}", UNIT_NAME);
        assert!(t.starts_with("gui/"), "expected gui/<uid> prefix, got {t}");
        assert!(
            t.ends_with(&expected_suffix),
            "expected /{} suffix, got {t}",
            UNIT_NAME
        );
    }

    #[test]
    fn plist_contains_essential_keys() {
        let xml = plist_text();
        // Verify the plist has the keys launchd needs and matches our label.
        for key in [
            "<key>Label</key>",
            "<key>ProgramArguments</key>",
            "<key>RunAtLoad</key>",
            "<key>KeepAlive</key>",
            "<string>com.ealtun21.resonanced</string>",
            "<true/>",
        ] {
            assert!(xml.contains(key), "plist missing `{key}`: \n{xml}");
        }
        assert!(
            xml.starts_with("<?xml"),
            "plist must start with XML declaration"
        );
    }
}
