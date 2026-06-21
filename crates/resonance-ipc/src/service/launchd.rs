//! macOS backend: launchd LaunchAgent control for `resonanced`.
//!
//! Layout:
//!   - Plist at `~/Library/LaunchAgents/com.ealtun21.resonanced.plist`
//!   - Loaded into the per-user GUI domain (`gui/<uid>`) via `launchctl`.
//!
//! Operations map naturally to launchd:
//!   - `install`  → write the plist (RunAtLoad=false) + `launchctl bootstrap gui/<uid> <path>`
//!   - `uninstall`→ `launchctl bootout gui/<uid>/<label>` + delete plist
//!   - `start`    → `launchctl kickstart gui/<uid>/<label>`
//!   - `stop`     → `launchctl kill SIGTERM gui/<uid>/<label>`
//!   - `enable`   → rewrite the plist with `RunAtLoad=true`, (re)bootstrap, kickstart now
//!   - `disable`  → rewrite the plist with `RunAtLoad=false`, then `launchctl bootout`
//!
//! "Enabled" here mirrors systemd/xdg: autostart-at-login ON *and* running now,
//! while "disabled" is autostart OFF *and* stopped. Unlike launchd's own notion
//! of "loaded into a domain", we anchor *enabled* to the persistent on-disk
//! `RunAtLoad` flag (read by `is_enabled`), so the answer survives a reboot and
//! doesn't depend on transient domain load-state. `install` writes the plist
//! with `RunAtLoad=false` (installed but not autostarting, matching systemd's
//! install-vs-enable split); `enable`/`disable` rewrite the plist to flip it.

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

/// Render the LaunchAgent plist. `enabled` controls `RunAtLoad`: `true` makes
/// the agent come up at login (matches systemd's `WantedBy=default.target`),
/// `false` leaves it installed but not autostarting (matches systemd's
/// install-without-enable). `enable`/`disable` rewrite the plist to flip this.
///
/// KeepAlive interaction: `KeepAlive { SuccessfulExit = false }` only relaunches
/// the daemon after a *crash* (non-zero exit), NOT after a clean exit. A user
/// `stop()` sends SIGTERM and the daemon's handler exits 0, so launchd will not
/// fight the user by relaunching it. This is independent of `RunAtLoad`, which
/// only governs the initial start when the agent is (re)loaded at login — so a
/// disabled agent (`RunAtLoad=false`) that the user manually starts still gets
/// crash-restart from KeepAlive, but won't come back on its own after a reboot.
fn plist_text(enabled: bool) -> String {
    let exec = super::daemon_bin();
    let log_dir = home().join("Library").join("Logs").join("resonance");
    let stdout_log = log_dir.join("resonanced.out.log");
    let stderr_log = log_dir.join("resonanced.err.log");
    let run_at_load = if enabled { "<true/>" } else { "<false/>" };
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
    {run_at_load}
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
        run_at_load = run_at_load,
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

/// "Enabled" means the on-disk plist has `RunAtLoad=true`, i.e. the agent will
/// start at login. We read the plist (not `launchctl print`) so the answer
/// reflects the persistent autostart intent rather than the transient
/// domain-load state. Scan for the `<key>RunAtLoad</key>` element and look at
/// the next `<true/>`/`<false/>` token, tolerant of whitespace/newlines.
pub fn is_enabled() -> bool {
    let Ok(text) = std::fs::read_to_string(unit_path()) else {
        return false;
    };
    let Some(after_key) = text.split("<key>RunAtLoad</key>").nth(1) else {
        return false;
    };
    // Find which boolean token appears first after the key.
    let true_at = after_key.find("<true/>");
    let false_at = after_key.find("<false/>");
    match (true_at, false_at) {
        (Some(t), Some(f)) => t < f,
        (Some(_), None) => true,
        _ => false,
    }
}

fn ensure_log_dir() -> io::Result<()> {
    let dir = home().join("Library").join("Logs").join("resonance");
    std::fs::create_dir_all(&dir)
}

/// Write the plist with the given autostart intent and (re)bootstrap it into the
/// user domain. Shared by `install` (enabled=false: installed, not autostarting)
/// and `enable` (enabled=true: autostart at login), mirroring systemd's split
/// between writing the unit and enabling it.
fn install_with(enabled: bool) -> io::Result<()> {
    let path = unit_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    ensure_log_dir()?;
    std::fs::write(&path, plist_text(enabled))?;

    // If a previous version was already loaded, bootout so the new plist is
    // honoured on the next bootstrap. Ignore failures (not loaded → fine).
    let _ = launchctl(&["bootout", &service_target()]);
    let plist = path.to_string_lossy().into_owned();
    check_launchctl(
        launchctl(&["bootstrap", &domain_target(), &plist])?,
        "bootstrap",
    )
}

pub fn install() -> io::Result<()> {
    // Installed but not autostarting (RunAtLoad=false), like systemd writing the
    // unit without `enable`. `enable` rewrites the plist with RunAtLoad=true.
    install_with(false)
}

pub fn uninstall() -> io::Result<()> {
    let _ = launchctl(&["bootout", &service_target()]);
    let path = unit_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// Whether the service is currently loaded into the user domain (distinct from
/// our `is_enabled`, which reflects the persistent `RunAtLoad` autostart intent).
/// `launchctl print` exits 0 iff the service exists in the domain.
fn is_loaded() -> bool {
    launchctl(&["print", &service_target()])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn start() -> io::Result<()> {
    // `kickstart` starts the service in its current domain. If it isn't loaded
    // at all, bootstrap first (install() writes the plist + bootstraps). We key
    // off domain load-state, not `is_enabled` — a started-but-not-autostarting
    // service is a valid state (RunAtLoad=false, loaded, running now).
    if !is_loaded() {
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
    if !is_loaded() {
        install()?;
    }
    check_launchctl(
        launchctl(&["kickstart", "-k", &service_target()])?,
        "kickstart -k",
    )
}

/// Enable autostart AND start now (mirrors systemd `enable --now` / xdg
/// `write_desktop(true) + start()`): rewrite the plist with `RunAtLoad=true`,
/// (re)bootstrap it, then kickstart so the daemon is running immediately.
pub fn enable() -> io::Result<()> {
    install_with(true)?;
    // `-k` kills any existing instance and (re)starts, so enable always leaves
    // the daemon running now, not just autostarting at next login.
    check_launchctl(
        launchctl(&["kickstart", "-k", &service_target()])?,
        "kickstart",
    )
}

/// Disable autostart AND stop now (mirrors systemd `disable --now` / xdg
/// `write_desktop(false) + stop()`): rewrite the plist with `RunAtLoad=false`
/// so it won't come back at login, then bootout to unload and stop it. The
/// bootout result is propagated as an error (no silent `let _ = …`).
pub fn disable() -> io::Result<()> {
    install_with(false)?;
    check_launchctl(launchctl(&["bootout", &service_target()])?, "bootout")
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
        let xml = plist_text(true);
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

    #[test]
    fn plist_run_at_load_tracks_enabled_flag() {
        // enabled=true renders RunAtLoad <true/>, enabled=false renders <false/>.
        let enabled = plist_text(true);
        let disabled = plist_text(false);
        let after = |s: &str| {
            s.split("<key>RunAtLoad</key>")
                .nth(1)
                .map(|t| {
                    let tr = t.find("<true/>");
                    let fa = t.find("<false/>");
                    matches!((tr, fa), (Some(a), Some(b)) if a < b)
                        || matches!((tr, fa), (Some(_), None))
                })
                .unwrap_or(false)
        };
        assert!(after(&enabled), "enabled plist must have RunAtLoad <true/>");
        assert!(
            !after(&disabled),
            "disabled plist must have RunAtLoad <false/>"
        );
    }
}
