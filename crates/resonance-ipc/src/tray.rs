//! Tray control surface shared by every client (config, autostart, process
//! control, UI discovery). The tray *binary* lives in the `resonance-tray`
//! crate; this module is the dep-light control API so CLI/TUI/GUI stay in sync.

pub mod config;

pub use config::{LeftClick, TrayConfig};
