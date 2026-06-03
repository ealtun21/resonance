use crate::state::SharedState;
use anyhow::Result;
use std::thread::{self, JoinHandle};
use tracing::info;

/// Spawn the PipeWire filter node on a dedicated thread.
/// This is a stub that will be replaced with the real pw_filter implementation.
pub fn spawn(state: SharedState) -> Result<JoinHandle<()>> {
    let handle = thread::spawn(move || {
        info!("PipeWire thread starting (stub)");
        // TODO: Initialize pipewire::MainLoop, create filter node,
        // connect to the DSP chain, run the main loop.
        //
        // Real implementation will:
        //   1. pipewire::init()
        //   2. Create a Core + Registry
        //   3. Create a pw_filter with capture + playback ports
        //   4. In the process callback, lock state, call chain.process(buf)
        //   5. Run the pipewire MainLoop
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    });
    Ok(handle)
}
