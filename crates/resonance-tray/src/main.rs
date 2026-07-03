//! `resonance-tray` — optional standalone system-tray controller.
//! Not standalone: refuses to run without at least one installed UI, and is
//! never embedded in the daemon.

use menu::{MenuAction, MenuModel, build_model};
use resonance_ipc::tray::{self, Ui, control::GUI_INSTANCE};
use std::sync::mpsc::channel;
use std::time::Duration;

mod backend;
mod daemon;
mod icons;
mod menu;

/// The tray is an add-on to a UI; without any interface installed there is
/// nothing to "Open" and the tray must not run as an orphan.
fn should_run(uis: &[Ui]) -> bool {
    !uis.is_empty()
}

/// Assemble the current display model: fresh config, daemon state, profiles,
/// and the tray/daemon-autostart + GUI-running flags. Called on every poll
/// tick and after every action so the UI never drifts from live state.
fn snapshot(uis: &[Ui]) -> MenuModel {
    let cfg = tray::TrayConfig::load();
    let state = daemon::poll_state();
    let profiles = daemon::fetch_profiles(cfg.recent_count);
    build_model(
        state.as_ref(),
        &cfg,
        uis,
        &profiles,
        tray::autostart::is_enabled(),
        resonance_ipc::service::is_enabled(),
        resonance_ipc::singleton::running_pid(GUI_INSTANCE).is_some(),
    )
}

fn main() -> anyhow::Result<()> {
    let uis = tray::installed_uis();
    if !should_run(&uis) {
        eprintln!(
            "resonance-tray: no interface installed (need one of resonance-gui, \
             resonance-tui, or resonance). The tray cannot run standalone."
        );
        std::process::exit(2);
    }
    // Single instance: a second tray exits cleanly.
    let Some(_guard) = resonance_ipc::singleton::acquire(tray::control::TRAY_INSTANCE)? else {
        eprintln!("resonance-tray: already running");
        return Ok(());
    };

    let (upd_tx, upd_rx) = channel::<MenuModel>();
    let (act_tx, act_rx) = channel::<MenuAction>();

    // Poll thread: refresh the model on the configured cadence (config
    // re-read each cycle so GUI/CLI edits take effect live).
    {
        let upd_tx = upd_tx.clone();
        let uis = uis.clone();
        std::thread::Builder::new()
            .name("resonance-tray-poll".into())
            .spawn(move || {
                loop {
                    if upd_tx.send(snapshot(&uis)).is_err() {
                        break;
                    }
                    let secs = tray::TrayConfig::load().poll_secs.clamp(1, 60);
                    std::thread::sleep(Duration::from_secs(secs));
                }
            })?;
    }

    // Action thread: execute, then push an immediate refreshed model.
    {
        let upd_tx = upd_tx.clone();
        let uis = uis.clone();
        std::thread::Builder::new()
            .name("resonance-tray-act".into())
            .spawn(move || {
                for action in act_rx {
                    if let Err(e) = daemon::execute(&action) {
                        eprintln!("resonance-tray: action failed: {e}");
                    }
                    let _ = upd_tx.send(snapshot(&uis));
                }
            })?;
    }

    let init = snapshot(&uis);
    backend::run(init, upd_rx, act_tx)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_run_without_a_ui() {
        assert!(!should_run(&[]));
        assert!(should_run(&[Ui::Cli]));
    }
}
