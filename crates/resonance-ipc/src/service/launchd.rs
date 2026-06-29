//! macOS backend: launchd `LaunchAgent` control for `resonanced`.
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

/// Stable, non-TCC-protected location the launchd agent runs the daemon from:
/// `~/Library/Application Support/resonance/bin/resonanced`.
fn staged_bin() -> PathBuf {
    crate::paths::config_dir().join("bin").join("resonanced")
}

/// Whether `path` lives somewhere macOS guards with TCC, where a background
/// launchd agent is denied read access — so exec'ing a binary there hangs in the
/// loader's `open()` and the daemon never comes up. Covers the per-user
/// protected folders plus the cloud-storage mount points.
fn is_tcc_protected(path: &std::path::Path) -> bool {
    let h = home();
    let s = path.to_string_lossy();
    ["Documents", "Desktop", "Downloads"]
        .iter()
        .any(|d| path.starts_with(h.join(d)))
        || s.contains("/Library/Mobile Documents/") // iCloud Drive
        || s.contains("/Library/CloudStorage/") // OneDrive / Dropbox / etc.
}

/// Resolve the path the launchd agent should run the daemon from, staging the
/// binary into app-support first when (and only when) the resolved binary lives
/// in a TCC-protected folder.
///
/// macOS denies a background launchd agent read access to `~/Documents`,
/// `~/Desktop`, `~/Downloads` and cloud mounts. If `ExecStart` points into one
/// — e.g. a dev build under `~/Documents/.../target/debug/resonanced` — the
/// agent's `exec` blocks in the loader's `open()` forever and the daemon never
/// comes up (no socket, no log). So we copy the binary out to app-support and
/// point the plist there. The *client* does the copy, in the user's own
/// file-access context, so it can read a source the agent could not.
///
/// A binary already in a readable location (a packaged install, or the
/// `Resonance.app` bundle under `/Applications`) is left in place and run
/// directly — no copy, so it keeps tracking its real binary across upgrades. On
/// any copy error we fall back to the original path rather than failing the op.
fn stage_binary() -> io::Result<PathBuf> {
    let src = super::daemon_bin();
    if !is_tcc_protected(&src) {
        return Ok(src);
    }
    let dst = staged_bin();
    if src == dst {
        return Ok(dst);
    }
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Copy to a sibling temp then atomically rename over the destination, so a
    // running daemon (which keeps its own already-mapped inode) is never served
    // a half-written file on its next relaunch.
    let tmp = dst.with_extension("staging");
    std::fs::copy(&src, &tmp)?;
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&tmp)?.permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&tmp, perm)?;
    }
    std::fs::rename(&tmp, &dst)?;
    Ok(dst)
}

/// Render the `LaunchAgent` plist. `enabled` controls `RunAtLoad`: `true` makes
/// the agent come up at login (matches systemd's `WantedBy=default.target`),
/// `false` leaves it installed but not autostarting (matches systemd's
/// install-without-enable). `enable`/`disable` rewrite the plist to flip this.
///
/// `KeepAlive` interaction: `KeepAlive { SuccessfulExit = false }` only relaunches
/// the daemon after a *crash* (non-zero exit), NOT after a clean exit. A user
/// `stop()` sends SIGTERM and the daemon's handler exits 0, so launchd will not
/// fight the user by relaunching it. This is independent of `RunAtLoad`, which
/// only governs the initial start when the agent is (re)loaded at login — so a
/// disabled agent (`RunAtLoad=false`) that the user manually starts still gets
/// crash-restart from `KeepAlive`, but won't come back on its own after a reboot.
fn plist_text(enabled: bool, exec: &std::path::Path) -> String {
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
    <key>ThrottleInterval</key>
    <integer>1</integer>
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
// Terminal consumer of a one-shot launchctl `Output`; callers never reuse it.
#[allow(clippy::needless_pass_by_value)]
fn check_launchctl(out: std::process::Output, what: &str) -> io::Result<()> {
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let err = err.trim().to_string();
    // Idempotent transitions: launchctl reports a non-zero status when the
    // requested state already holds. "No such process" / "Could not find …"
    // (errno 3/113) is the success case for `stop`/`bootout` on an already-
    // stopped service; "already loaded" (errno 37) is the success case for
    // `bootstrap` on a loaded one. Treating these as failures made a redundant
    // Stop (or a stop on a never-started daemon) surface a spurious error.
    let low = err.to_ascii_lowercase();
    // "No process to signal." is `launchctl kill`'s reply when the service is
    // already stopped — the idempotent success case for a redundant Stop.
    if low.contains("no such process")
        || low.contains("could not find")
        || low.contains("already loaded")
        || low.contains("no process to signal")
    {
        return Ok(());
    }
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
        .is_ok_and(|o| o.status.success())
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
    // Run the agent from a readable, non-TCC-protected copy (see `stage_binary`);
    // fall back to the resolved path if staging fails.
    let exec = stage_binary().unwrap_or_else(|_| super::daemon_bin());
    std::fs::write(&path, plist_text(enabled, &exec))?;

    // If a previous version is already loaded, bootout so the new plist is
    // honoured on re-bootstrap, then wait for the domain to actually drop the
    // label: bootstrapping while it's still registered fails with "Bootstrap
    // failed: 5: Input/output error" (a race seen when upgrading over a running
    // daemon). Bounded (~500 ms) so a wedged service can't hang install.
    let _ = launchctl(&["bootout", &service_target()]);
    for _ in 0..50 {
        if !is_loaded() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let plist = path.to_string_lossy().into_owned();
    let out = launchctl(&["bootstrap", &domain_target(), &plist])?;
    // Tolerate a residual bootstrap race: if the service ended up loaded anyway,
    // the new plist is in effect, so treat it as success rather than surfacing a
    // spurious "already bootstrapped" I/O error.
    if out.status.success() || is_loaded() {
        Ok(())
    } else {
        check_launchctl(out, "bootstrap")
    }
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
        .is_ok_and(|o| o.status.success())
}

pub fn start() -> io::Result<()> {
    // `kickstart` starts the service in its current domain. If it isn't loaded
    // at all, bootstrap first (install() writes the plist + bootstraps). We key
    // off domain load-state, not `is_enabled` — a started-but-not-autostarting
    // service is a valid state (RunAtLoad=false, loaded, running now).
    if !is_loaded() {
        install()?;
    }
    // Plain `kickstart` (no `-k`): start the daemon if it's stopped, and leave an
    // already-running daemon alone. `-k` would kill and immediately relaunch it,
    // so clicking "Start" while it's already up would needlessly interrupt live
    // audio — the reported "Start does an odd restart". Only `restart()` kills.
    check_launchctl(launchctl(&["kickstart", &service_target()])?, "kickstart")
}

pub fn stop() -> io::Result<()> {
    check_launchctl(launchctl(&["kill", "SIGTERM", &service_target()])?, "kill")
}

pub fn restart() -> io::Result<()> {
    if !is_loaded() {
        install()?;
    }
    // Refresh the staged binary so a rebuilt daemon is what comes back up.
    let _ = stage_binary();
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
    // `install_with` already booted the agent out and back in with RunAtLoad=true
    // (which starts it), so a plain `kickstart` just guarantees it's running now.
    // Avoid `-k` here: toggling autostart on shouldn't kill-restart a daemon that
    // the rebootstrap already brought up.
    check_launchctl(launchctl(&["kickstart", &service_target()])?, "kickstart")
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
        assert!(
            std::path::Path::new(&s)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("plist")),
            "expected .plist suffix, got {s}"
        );
        assert!(s.contains(UNIT_NAME), "expected unit name in path, got {s}");
    }

    #[test]
    fn service_target_includes_uid_and_label() {
        let t = service_target();
        let expected_suffix = format!("/{UNIT_NAME}");
        assert!(t.starts_with("gui/"), "expected gui/<uid> prefix, got {t}");
        assert!(
            t.ends_with(&expected_suffix),
            "expected /{UNIT_NAME} suffix, got {t}"
        );
    }

    #[test]
    fn tcc_protected_paths_are_detected() {
        let h = home();
        // Dev build under ~/Documents (and the other guarded folders) → staged.
        assert!(is_tcc_protected(
            &h.join("Documents/resonance/target/debug/resonanced")
        ));
        assert!(is_tcc_protected(&h.join("Desktop/resonanced")));
        assert!(is_tcc_protected(&h.join("Downloads/resonanced")));
        assert!(is_tcc_protected(std::path::Path::new(
            "/Users/x/Library/Mobile Documents/com~apple~CloudDocs/resonanced"
        )));
        // Readable locations → run in place, no staging.
        assert!(!is_tcc_protected(std::path::Path::new(
            "/Applications/Resonance.app/Contents/MacOS/resonanced"
        )));
        assert!(!is_tcc_protected(&h.join(".local/bin/resonanced")));
        assert!(!is_tcc_protected(
            &h.join("Library/Application Support/resonance/bin/resonanced")
        ));
    }

    #[test]
    fn plist_contains_essential_keys() {
        let xml = plist_text(true, std::path::Path::new("/usr/local/bin/resonanced"));
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
        let exec = std::path::Path::new("/usr/local/bin/resonanced");
        let enabled = plist_text(true, exec);
        let disabled = plist_text(false, exec);
        let after = |s: &str| {
            s.split("<key>RunAtLoad</key>")
                .nth(1)
                .is_some_and(|t| {
                    let tr = t.find("<true/>");
                    let fa = t.find("<false/>");
                    matches!((tr, fa), (Some(a), Some(b)) if a < b)
                        || matches!((tr, fa), (Some(_), None))
                })
        };
        assert!(after(&enabled), "enabled plist must have RunAtLoad <true/>");
        assert!(
            !after(&disabled),
            "disabled plist must have RunAtLoad <false/>"
        );
    }
}
