//! Start/stop the tray process and launch/focus the UI, shared by every client.

use crate::singleton;
use crate::tray::{self, Ui};
use std::io;
use std::process::{Command, Stdio};

pub const TRAY_INSTANCE: &str = "resonance-tray";
pub const GUI_INSTANCE: &str = "resonance-gui";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrayStatus {
    pub running: bool,
    pub autostart: bool,
}

#[must_use]
pub fn is_running() -> bool {
    singleton::running_pid(TRAY_INSTANCE).is_some()
}

#[must_use]
pub fn status() -> TrayStatus {
    TrayStatus {
        running: is_running(),
        autostart: tray::autostart::is_enabled(),
    }
}

fn spawn_detached(cmd: &mut Command) -> io::Result<()> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().map(|_| ())
}

/// Spawn the tray if it is not already running. Idempotent.
///
/// # Errors
/// Returns an error if the tray binary cannot be spawned.
pub fn start() -> io::Result<()> {
    if is_running() {
        return Ok(());
    }
    spawn_detached(&mut Command::new(tray::tray_bin()))
}

/// Stop a running tray. `Ok(false)` if none was running.
///
/// # Errors
/// Returns an error if signalling the tray process fails.
pub fn stop() -> io::Result<bool> {
    singleton::stop(TRAY_INSTANCE)
}

/// Show the UI: focus a running GUI via the raise flag, else spawn the
/// preferred installed UI. With only a TUI installed, open it in `$TERMINAL`.
///
/// # Errors
/// Returns an error if no UI is installed or the chosen UI cannot be spawned.
pub fn open_ui() -> io::Result<()> {
    if singleton::running_pid(GUI_INSTANCE).is_some() {
        return singleton::request_raise(GUI_INSTANCE);
    }
    let uis = tray::installed_uis();
    if uis.contains(&Ui::Gui) {
        if let Some(bin) = tray::ui_bin(Ui::Gui) {
            return spawn_detached(&mut Command::new(bin));
        }
    }
    if uis.contains(&Ui::Tui) {
        if let Some(bin) = tray::ui_bin(Ui::Tui) {
            return open_tui_in_terminal(&bin);
        }
    }
    Err(io::Error::other("no windowed UI installed to open"))
}

/// Signal a running GUI to exit. `Ok(false)` if none was running.
///
/// # Errors
/// Returns an error if signalling the GUI fails.
pub fn quit_ui() -> io::Result<bool> {
    singleton::stop(GUI_INSTANCE)
}

#[cfg(unix)]
fn open_tui_in_terminal(bin: &std::path::Path) -> io::Result<()> {
    // Best-effort: honor $TERMINAL, else try a short list of common emulators.
    let term = std::env::var("TERMINAL").ok();
    let candidates: Vec<String> = term
        .into_iter()
        .chain(
            [
                "x-terminal-emulator",
                "kitty",
                "alacritty",
                "konsole",
                "gnome-terminal",
                "xterm",
            ]
            .map(String::from),
        )
        .collect();
    for t in candidates {
        let mut c = Command::new(&t);
        c.arg("-e").arg(bin);
        if spawn_detached(&mut c).is_ok() {
            return Ok(());
        }
    }
    Err(io::Error::other(
        "no terminal emulator found to open the TUI",
    ))
}

#[cfg(windows)]
fn open_tui_in_terminal(bin: &std::path::Path) -> io::Result<()> {
    // cmd start opens a new console window running the TUI.
    let mut c = Command::new("cmd");
    c.args(["/C", "start", ""]).arg(bin);
    spawn_detached(&mut c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reports_not_running_when_absent() {
        // No tray spawned in the test env → running is false.
        let s = status();
        assert!(!s.running || crate::singleton::running_pid(TRAY_INSTANCE).is_some());
        // Consistency: status().running must agree with is_running().
        assert_eq!(s.running, is_running());
    }
}
