mod ipc_server;
mod pw_node;
mod state;

use anyhow::Result;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    info!("resonanced starting");

    let state = state::SharedState::new();

    // Start PipeWire filter node in a dedicated thread
    let pw_handle = pw_node::spawn(state.clone())?;

    // Start IPC server
    ipc_server::run(state).await?;

    pw_handle.join().ok();

    info!("resonanced stopped");
    Ok(())
}
