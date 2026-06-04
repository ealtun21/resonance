use crate::state::SharedState;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use resonance_preset::{apo::parse_apo, fac::parse_fac};
use std::{path::PathBuf, sync::mpsc, time::Duration};
use tracing::{info, warn};

/// Watches the preset file stored in `SharedState::watched_preset`.
/// Reloads and applies it automatically on write events.
#[allow(unused_variables, unused_assignments)]
pub async fn run(state: SharedState) {
    let mut current_path: Option<PathBuf> = None;
    let mut interval = tokio::time::interval(Duration::from_millis(200));

    // Channel for notify events (sync → async bridge)
    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher: Option<RecommendedWatcher> = None;

    loop {
        interval.tick().await;

        // Check if the watched path changed
        let wanted = state
            .0
            .lock()
            .unwrap()
            .watched_preset
            .as_deref()
            .map(PathBuf::from);

        if wanted != current_path {
            // Drop old watcher
            watcher = None;
            current_path = wanted.clone();

            if let Some(path) = &wanted {
                let tx2 = tx.clone();
                match RecommendedWatcher::new(
                    move |res| {
                        let _ = tx2.send(res);
                    },
                    notify::Config::default(),
                ) {
                    Ok(mut w) => {
                        if let Err(e) = w.watch(path, RecursiveMode::NonRecursive) {
                            warn!("watch failed for {}: {e}", path.display());
                        } else {
                            info!("watching {}", path.display());
                            watcher = Some(w);
                        }
                    }
                    Err(e) => warn!("create watcher failed: {e}"),
                }
            }
        }

        // Drain pending file events
        while let Ok(Ok(event)) = rx.try_recv() {
            let is_write = matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_));
            if is_write {
                if let Some(path) = &current_path {
                    reload_preset(path, &state);
                }
            }
        }
    }
}

fn reload_preset(path: &PathBuf, state: &SharedState) {
    let path_str = path.to_string_lossy();
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            warn!("watch reload {path_str}: read error: {e}");
            return;
        }
    };

    let preset_result = if path_str.ends_with(".fac") {
        parse_fac(&content).map_err(|e| e.to_string())
    } else {
        parse_apo(&content).map_err(|e| e.to_string())
    };

    match preset_result {
        Ok(preset) => {
            let (sr, channels) = {
                let inner = state.0.lock().unwrap();
                (inner.chain.sample_rate, inner.chain.channels)
            };
            let new_chain = preset.into_chain(channels, sr);
            state.send(
                crate::state::AudioCommand::ReplaceChain(Box::new(new_chain)),
                |_| {},
            );
            let owned = path_str.into_owned();
            info!("auto-reloaded {owned}");
            state.0.lock().unwrap().current_preset = Some(owned);
        }
        Err(e) => warn!("watch reload {path_str}: parse error: {e}"),
    }
}
