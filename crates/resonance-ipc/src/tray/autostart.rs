//! Autostart-at-login for the tray, independent of the daemon's autostart.
//! The tray needs a graphical session (a `StatusNotifier` host), so this uses the
//! desktop autostart mechanisms, not a system/service-manager unit.

use std::io;
use std::path::PathBuf;

#[cfg(target_os = "linux")]
mod imp {
    use super::{PathBuf, io};

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
        config_home()
            .join("autostart")
            .join("resonance-tray.desktop")
    }

    pub fn is_enabled() -> bool {
        match std::fs::read_to_string(unit_path()) {
            Ok(text) => !text
                .lines()
                .any(|l| l.trim().eq_ignore_ascii_case("Hidden=true")),
            Err(_) => false,
        }
    }

    pub fn enable() -> io::Result<()> {
        let exec = crate::tray::tray_bin();
        let text = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Resonance tray\n\
             Comment=Resonance system-tray controller\n\
             Exec={exec}\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n\
             Hidden=false\n",
            exec = exec.display(),
        );
        let path = unit_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, text)
    }

    pub fn disable() -> io::Result<()> {
        let path = unit_path();
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{PathBuf, io};

    pub fn unit_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home).join("Library/LaunchAgents/com.ealtun21.resonance-tray.plist")
    }

    pub fn is_enabled() -> bool {
        unit_path().is_file()
    }

    pub fn enable() -> io::Result<()> {
        let exec = crate::tray::tray_bin();
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\"><dict>\
             <key>Label</key><string>com.ealtun21.resonance-tray</string>\
             <key>ProgramArguments</key><array><string>{exec}</string></array>\
             <key>RunAtLoad</key><true/>\
             </dict></plist>\n",
            exec = exec.display(),
        );
        let path = unit_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, plist)?;
        let _ = std::process::Command::new("launchctl")
            .arg("load")
            .arg(&path)
            .output();
        Ok(())
    }

    pub fn disable() -> io::Result<()> {
        let path = unit_path();
        let _ = std::process::Command::new("launchctl")
            .arg("unload")
            .arg(&path)
            .output();
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use super::{PathBuf, io};
    // The tray is `#![windows_subsystem = "windows"]` too, so plain
    // `Command::new("reg")` would flash an empty console window on every call
    // (same issue `service/windows.rs`'s `hidden()` was written to fix — reuse
    // it here rather than duplicating the `CREATE_NO_WINDOW` flag).
    use crate::service::windows::hidden;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "ResonanceTray";

    pub fn unit_path() -> PathBuf {
        PathBuf::from(format!(r"HKCU\{RUN_KEY}\{VALUE}"))
    }

    pub fn is_enabled() -> bool {
        hidden("reg")
            .args(["query", &format!(r"HKCU\{RUN_KEY}"), "/v", VALUE])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn enable() -> io::Result<()> {
        let exec = crate::tray::tray_bin();
        let status = hidden("reg")
            .args([
                "add",
                &format!(r"HKCU\{RUN_KEY}"),
                "/v",
                VALUE,
                "/t",
                "REG_SZ",
                "/d",
                &exec.display().to_string(),
                "/f",
            ])
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other("reg add failed"))
        }
    }

    pub fn disable() -> io::Result<()> {
        // Deleting a missing value returns nonzero; treat that as already-disabled.
        let _ = hidden("reg")
            .args(["delete", &format!(r"HKCU\{RUN_KEY}"), "/v", VALUE, "/f"])
            .output()?;
        Ok(())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod imp {
    use super::{PathBuf, io};

    pub fn unit_path() -> PathBuf {
        PathBuf::from("resonance-tray-autostart")
    }

    pub fn is_enabled() -> bool {
        false
    }

    pub fn enable() -> io::Result<()> {
        Err(io::Error::other("autostart unsupported on this platform"))
    }

    pub fn disable() -> io::Result<()> {
        // Matches `enable`'s error rather than a bare `Ok(())` (which would trip
        // `clippy::unnecessary_wraps` under pedantic -D warnings) — see
        // `service/stub.rs`'s convention of a shared `unsupported()` error for
        // every operation on a platform with no real backend.
        Err(io::Error::other("autostart unsupported on this platform"))
    }
}

/// Path to the platform's tray autostart entry (Linux: the `.desktop` file;
/// macOS: the `LaunchAgent` plist; Windows: a sentinel `HKCU\...\Run` path for
/// display purposes since the Run key has no file).
#[must_use]
pub fn unit_path() -> PathBuf {
    imp::unit_path()
}

/// Whether the tray is currently set to start at login.
#[must_use]
pub fn is_enabled() -> bool {
    imp::is_enabled()
}

/// Enable tray autostart at login.
///
/// # Errors
/// Returns an error if the autostart entry cannot be written.
pub fn enable() -> io::Result<()> {
    imp::enable()
}

/// Disable tray autostart at login.
///
/// # Errors
/// Returns an error if the autostart entry cannot be removed.
pub fn disable() -> io::Result<()> {
    imp::disable()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn enable_writes_autostart_desktop_then_disable_removes_it() {
        // Held for the whole test body: serializes against every other
        // env-mutating test in the crate (paths.rs config-dir/socket tests,
        // tray.rs PATH test) so a sibling's set_var/remove_var can't fire
        // mid-way through this one and leak into the real $HOME/.config.
        let _env = crate::test_env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Isolate XDG_CONFIG_HOME so we don't touch the real autostart dir.
        let tmp = std::env::temp_dir().join(format!("res-tray-autostart-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // SAFETY: single-threaded test; restore below.
        let saved = std::env::var_os("XDG_CONFIG_HOME");
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &tmp);
        }

        assert!(!is_enabled());
        enable().unwrap();
        let path = unit_path();
        assert!(path.is_file(), "enable writes {}", path.display());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("resonance-tray"), "Exec points at the tray");
        assert!(is_enabled());
        disable().unwrap();
        assert!(!is_enabled(), "disable removes/hides the entry");

        unsafe {
            match saved {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }
}
