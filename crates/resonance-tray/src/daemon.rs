//! Talks to the running daemon over the existing IPC socket, and dispatches the
//! non-daemon actions (service lifecycle, UI launch, autostart).
//!
//! Backend wiring (calling these from the tray event loop) lands in Task 12;
//! until then nothing but the inline tests exercises this module, so the
//! module-level allow keeps that from tripping `-D warnings`.
// wired in Task 12
#![allow(dead_code)]

use crate::menu::{MenuAction, plan_command};
use resonance_ipc::transport::SyncClient;
use resonance_ipc::{Command, DaemonState, Response};
use std::time::Duration;

const IPC_TIMEOUT: Duration = Duration::from_millis(400);

fn client() -> Option<SyncClient> {
    SyncClient::connect_with_timeout(IPC_TIMEOUT).ok()
}

/// Latest state, or `None` if the daemon is unreachable.
#[must_use]
pub fn poll_state() -> Option<DaemonState> {
    client()?.get_state().ok()
}

/// Extract up to `limit` preset paths from a `ListPresets` response.
#[must_use]
pub fn presets_from_response(resp: &Response, limit: usize) -> Vec<String> {
    if let Response::PresetList(list) = resp {
        list.iter().take(limit).map(|p| preset_path(p)).collect()
    } else {
        Vec::new()
    }
}

fn preset_path(entry: &str) -> String {
    entry.to_owned()
}

/// Fetch preset paths (best-effort; empty when the daemon is down).
#[must_use]
pub fn fetch_presets(limit: usize) -> Vec<String> {
    let Some(mut c) = client() else {
        return Vec::new();
    };
    match c.send_recv(Command::ListPresets { dir: None }) {
        Ok(resp) => presets_from_response(&resp, limit),
        Err(_) => Vec::new(),
    }
}

/// Perform an action: daemon commands go over IPC; lifecycle/UI/autostart use
/// the shared `resonance-ipc` control modules.
///
/// # Errors
/// Returns an error if the underlying IPC send or control operation fails.
pub fn execute(action: &MenuAction) -> anyhow::Result<()> {
    use resonance_ipc::{service, tray};
    // Daemon-facing commands. Only `TogglePower` needs a fresh state (to
    // invert it); fetching it unconditionally would open a second socket
    // (state poll + send) for every action, including ones like `LoadPreset`
    // that ignore state entirely.
    let state = matches!(action, MenuAction::TogglePower)
        .then(poll_state)
        .flatten();
    if let Some(cmd) = plan_command(action, state.as_ref()) {
        let mut c = client().ok_or_else(|| anyhow::anyhow!("daemon not reachable"))?;
        c.send(cmd)?;
        return Ok(());
    }
    match action {
        MenuAction::DaemonStart => service::start()?,
        MenuAction::DaemonStop => service::stop()?,
        MenuAction::DaemonRestart => service::restart()?,
        MenuAction::DaemonAutostart => {
            if service::is_enabled() {
                service::disable()?;
            } else {
                service::enable()?;
            }
        }
        MenuAction::OpenUi => tray::control::open_ui()?,
        MenuAction::QuitUi => {
            tray::control::quit_ui()?;
        }
        MenuAction::TrayAutostart => {
            if tray::autostart::is_enabled() {
                tray::autostart::disable()?;
            } else {
                tray::autostart::enable()?;
            }
        }
        MenuAction::LeftClick => {
            if matches!(
                tray::TrayConfig::load().left_click,
                tray::LeftClick::ToggleUi
            ) {
                tray::control::open_ui()?;
            }
        }
        MenuAction::Quit => std::process::exit(0),
        // Handled by plan_command above.
        MenuAction::TogglePower | MenuAction::LoadPreset(_) => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use resonance_ipc::Response;

    #[test]
    fn preset_list_is_limited_and_extracts_paths() {
        let resp = Response::PresetList(vec!["a.fac".into(), "b.fac".into(), "c.fac".into()]);
        let out = presets_from_response(&resp, 2);
        // Truncated to `limit`, paths extracted in order.
        assert_eq!(out, vec!["a.fac".to_string(), "b.fac".to_string()]);
    }

    #[test]
    fn non_list_response_yields_empty() {
        assert!(presets_from_response(&Response::Ok, 8).is_empty());
    }
}
