//! Fallback audio backend for platforms without a real implementation.
//!
//! Spawns a thread that logs a warning and parks forever. The daemon still
//! starts (IPC works, GUI/TUI/CLI can talk to it) — audio just doesn't move.

use super::{CHANNELS, SAMPLE_RATE};
use anyhow::Result;
use std::thread::{self, JoinHandle};

pub fn spawn(_ctx: super::BackendCtx) -> Result<JoinHandle<()>> {
    tracing::warn!(
        "no audio backend compiled for this platform — daemon is a control plane only \
         (target CHANNELS={CHANNELS}, SAMPLE_RATE={SAMPLE_RATE})"
    );
    Ok(thread::Builder::new()
        .name("resonance-audio-stub".into())
        .spawn(|| {
            loop {
                thread::park();
            }
        })?)
}
