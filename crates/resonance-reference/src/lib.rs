//! Client-agnostic reference/measurement support, shared by the GUI and TUI.
//!
//! The curve maths, target/measurement state model, persistence snapshot, and
//! the background squig.link downloader live here; each client supplies its own
//! presentation layer (egui for the GUI, ratatui for the TUI). The downloader
//! is decoupled from any UI framework via a [`download::Wake`] callback the
//! client passes to wake its event loop when new data arrives.

pub mod download;
pub mod reference;
