//! Fallback audio backend for platforms without a real implementation.
//!
//! Spawns a thread that logs a warning and parks forever. The daemon still
//! starts (IPC works, GUI/TUI/CLI can talk to it) — audio just doesn't move.

use super::{CHANNELS, SAMPLE_RATE};
use crate::meters::AtomicMeters;
use crate::state::AudioCommand;
use anyhow::Result;
use resonance_dsp::chain::ProcessorChain;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

#[allow(clippy::too_many_arguments)]
pub fn spawn(
    _cmd_rx: rtrb::Consumer<AudioCommand>,
    _spectrum_tx: rtrb::Producer<f32>,
    _initial_chain: ProcessorChain,
    _output_tx: tokio::sync::mpsc::UnboundedSender<String>,
    _route_rx: std::sync::mpsc::Receiver<String>,
    _sinks_tx: tokio::sync::mpsc::UnboundedSender<Vec<(String, String)>>,
    _meters: Arc<AtomicMeters>,
) -> Result<JoinHandle<()>> {
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
