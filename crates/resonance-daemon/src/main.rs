mod config;
mod ipc_server;
mod pw_node;
mod spectrum;
mod state;

use anyhow::Result;
use config::{Mappings, Profile};
use resonance_dsp::chain::ProcessorChain;
use rtrb::RingBuffer;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    info!("resonanced starting");

    let (cmd_tx, cmd_rx) = RingBuffer::<state::AudioCommand>::new(256);
    let (spectrum_tx, spectrum_rx) = RingBuffer::<f32>::new(pw_node::SPECTRUM_BUF);
    let (route_tx, route_rx) = std::sync::mpsc::channel::<String>();
    let (sinks_tx, mut sinks_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<String>>();

    let initial_chain = ProcessorChain::builder()
        .channels(2)
        .sample_rate(48000.0)
        .build();

    let shared = state::SharedState::new(cmd_tx, route_tx);

    // Spectrum computation task
    let spectrum_state = shared.clone();
    tokio::spawn(async move {
        spectrum::run(spectrum_rx, spectrum_state).await;
    });

    // Output-device change task: when PipeWire reports a new real sink, auto-load
    // the profile mapped to it (if any).
    let (output_tx, mut output_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let output_state = shared.clone();
    tokio::spawn(async move {
        while let Some(output) = output_rx.recv().await {
            info!("active output changed: {output}");
            let mapped = Mappings::load().get(&output).map(str::to_owned);
            {
                let mut inner = output_state.0.lock().unwrap();
                inner.active_output = Some(output.clone());
                inner.mapped_profile = mapped.clone();
            }
            if let Some(name) = mapped {
                match apply_profile(&name, &output_state) {
                    Ok(()) => info!("auto-loaded profile '{name}' for output '{output}'"),
                    Err(e) => warn!("auto-load profile '{name}' failed: {e}"),
                }
            }
        }
    });

    // Available-sinks update task: keep SharedState in sync with PipeWire graph.
    let sinks_state = shared.clone();
    tokio::spawn(async move {
        while let Some(sinks) = sinks_rx.recv().await {
            sinks_state.0.lock().unwrap().available_sinks = sinks;
        }
    });

    // PipeWire filter node on dedicated RT thread
    let pw_handle = pw_node::spawn(
        cmd_rx,
        spectrum_tx,
        initial_chain,
        output_tx,
        route_rx,
        sinks_tx,
    )?;

    // IPC server (blocks until shutdown)
    ipc_server::run(shared).await?;

    pw_handle.join().ok();

    info!("resonanced stopped");
    Ok(())
}

/// Load a named profile and swap it onto the chain (used by the output-mapping task).
fn apply_profile(name: &str, state: &state::SharedState) -> Result<(), String> {
    let profile = Profile::load(name)?;
    let (sr, channels) = {
        let inner = state.0.lock().unwrap();
        (inner.chain.sample_rate, inner.chain.channels)
    };
    let chain_rt = profile.clone().into_chain(channels, sr);
    let chain_shadow = profile.into_chain(channels, sr);
    state.replace_chain(chain_rt, chain_shadow);
    Ok(())
}
