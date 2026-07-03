//! Platform tray backend. One implementation per OS, selected by cfg. The
//! chosen `run()` consumes `MenuModel` updates and emits `MenuAction`s; it blocks
//! for the process lifetime (required so macOS can own the main run loop).
//!
//! `run` is not called until the process is wired up in Task 12, so the whole
//! module is dead until then.
// wired in Task 12
#![allow(dead_code)]

use crate::menu::{MenuAction, MenuModel};
use std::sync::mpsc::{Receiver, Sender};

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod desktop;
#[cfg(target_os = "linux")]
mod linux;

/// Run the platform tray backend, blocking for the process lifetime.
///
/// # Errors
/// Returns an error if the tray backend cannot be created, or on an
/// unsupported platform.
pub fn run(
    init: MenuModel,
    updates: Receiver<MenuModel>,
    actions: Sender<MenuAction>,
) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux::run(init, updates, actions)
    }
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        desktop::run(init, updates, actions)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        let _ = (init, updates, actions);
        anyhow::bail!("no tray backend for this platform")
    }
}
