mod ipc_server;
mod pw_node;
mod spectrum;
mod state;
mod watcher;

use anyhow::Result;
use resonance_dsp::chain::ProcessorChain;
use rtrb::RingBuffer;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    info!("resonanced starting");

    let (cmd_tx, cmd_rx) = RingBuffer::<state::AudioCommand>::new(256);
    let (spectrum_tx, spectrum_rx) = RingBuffer::<f32>::new(pw_node::SPECTRUM_BUF);

    let initial_chain = ProcessorChain::builder()
        .channels(2)
        .sample_rate(48000.0)
        .build();

    let shared = state::SharedState::new(cmd_tx);

    // Spectrum computation task
    let spectrum_state = shared.clone();
    tokio::spawn(async move {
        spectrum::run(spectrum_rx, spectrum_state).await;
    });

    // File watcher task (watches presets for auto-reload)
    let watcher_state = shared.clone();
    tokio::spawn(async move {
        watcher::run(watcher_state).await;
    });

    // PipeWire filter node on dedicated RT thread
    let pw_handle = pw_node::spawn(cmd_rx, spectrum_tx, initial_chain)?;

    // IPC server (blocks until shutdown)
    ipc_server::run(shared).await?;

    pw_handle.join().ok();

    info!("resonanced stopped");
    Ok(())
}
