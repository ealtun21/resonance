//! Tray control surface shared by every client (config, autostart, process
//! control, UI discovery). The tray *binary* lives in the `resonance-tray`
//! crate; this module is the dep-light control API so CLI/TUI/GUI stay in sync.

pub mod autostart;
pub mod config;

pub use config::{LeftClick, TrayConfig};

use std::path::PathBuf;

/// A client interface that the tray can launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ui {
    Gui,
    Tui,
    Cli,
}

impl Ui {
    /// Platform-correct executable name (Windows appends `.exe`).
    #[must_use]
    pub const fn bin_name(self) -> &'static str {
        match self {
            Ui::Gui => {
                if cfg!(windows) {
                    "resonance-gui.exe"
                } else {
                    "resonance-gui"
                }
            }
            Ui::Tui => {
                if cfg!(windows) {
                    "resonance-tui.exe"
                } else {
                    "resonance-tui"
                }
            }
            Ui::Cli => {
                if cfg!(windows) {
                    "resonance.exe"
                } else {
                    "resonance"
                }
            }
        }
    }

    /// Human-readable label for menus/status lines.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Ui::Gui => "GUI",
            Ui::Tui => "TUI",
            Ui::Cli => "CLI",
        }
    }
}

/// Resolve a binary by name: sibling of the current exe first (co-installed
/// bundle), then `$PATH`. Mirrors `service::daemon_bin` resolution.
fn resolve_bin(name: &str) -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        let resolved = exe.canonicalize().ok();
        for base in [Some(&exe), resolved.as_ref()].into_iter().flatten() {
            if let Some(dir) = base.parent() {
                let cand = dir.join(name);
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&path) {
            let cand = d.join(name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Resolve the given UI's binary: sibling of the current exe, then `$PATH`.
#[must_use]
pub fn ui_bin(ui: Ui) -> Option<PathBuf> {
    resolve_bin(ui.bin_name())
}

/// The interfaces present on this system, in preference order (GUI, TUI, CLI).
#[must_use]
pub fn installed_uis() -> Vec<Ui> {
    [Ui::Gui, Ui::Tui, Ui::Cli]
        .into_iter()
        .filter(|&u| ui_bin(u).is_some())
        .collect()
}

/// Resolve the `resonance-tray` binary (sibling → `$PATH` → bare name).
#[must_use]
pub fn tray_bin() -> PathBuf {
    let name = if cfg!(windows) {
        "resonance-tray.exe"
    } else {
        "resonance-tray"
    };
    resolve_bin(name).unwrap_or_else(|| PathBuf::from(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bin_names_are_platform_correct() {
        let exe = if cfg!(windows) { ".exe" } else { "" };
        assert_eq!(Ui::Gui.bin_name(), format!("resonance-gui{exe}"));
        assert_eq!(Ui::Cli.bin_name(), format!("resonance{exe}"));
    }

    #[test]
    fn found_binary_is_reported_and_missing_is_not() {
        // Point PATH at a temp dir holding a fake `resonance-gui` and nothing else.
        let dir = std::env::temp_dir().join(format!("res-tray-uitest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gui = dir.join(Ui::Gui.bin_name());
        std::fs::write(&gui, b"#!/bin/sh\n").unwrap();
        // SAFETY: single-threaded test mutating PATH for the duration of the
        // call, restoring it before returning; no other test in this crate
        // reads or writes PATH.
        let saved = std::env::var_os("PATH");
        unsafe {
            std::env::set_var("PATH", &dir);
        }
        let found = ui_bin(Ui::Gui);
        let missing = ui_bin(Ui::Tui);
        unsafe {
            match saved {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
        }
        assert_eq!(found.as_deref(), Some(gui.as_path()));
        assert!(missing.is_none(), "tui not on PATH must be None");
    }
}
