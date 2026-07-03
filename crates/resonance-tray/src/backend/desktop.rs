//! Windows/macOS tray backend (tray-icon + winit).
//!
//! Placeholder stub: the real implementation lands in Task 11. It exists now
//! only so `cargo fmt` can resolve the cfg-gated `mod desktop;` declaration
//! (rustfmt follows module declarations even when they are cfg'd out for the
//! host target). Compiled only on Windows/macOS.
// wired in Task 11
#![allow(dead_code)]

use crate::menu::{MenuAction, MenuModel};
use std::sync::mpsc::{Receiver, Sender};

/// Run the desktop tray backend, blocking for the process lifetime.
///
/// # Errors
/// Always errors for now; the real backend is implemented in Task 11.
pub fn run(
    init: MenuModel,
    updates: Receiver<MenuModel>,
    actions: Sender<MenuAction>,
) -> anyhow::Result<()> {
    let _ = (init, updates, actions);
    anyhow::bail!("desktop tray backend not yet implemented")
}
