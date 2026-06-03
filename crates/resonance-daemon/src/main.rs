mod ipc_server;
mod pw_node;
mod state;

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

    // Command channel: IPC thread → audio thread (capacity 256 commands)
    let (cmd_tx, cmd_rx) = RingBuffer::<state::AudioCommand>::new(256);

    let initial_chain = ProcessorChain::builder()
        .channels(2)
        .sample_rate(48000.0)
        .build();

    let shared = state::SharedState::new(cmd_tx);

    // Start PipeWire filter node on a dedicated RT thread
    let pw_handle = pw_node::spawn(cmd_rx, initial_chain)?;

    // Start IPC server (blocks until shutdown)
    ipc_server::run(shared).await?;

    pw_handle.join().ok();

    info!("resonanced stopped");
    Ok(())
}
