//! Talks to the running daemon over the existing IPC socket, and dispatches the
//! non-daemon actions (service lifecycle, UI launch, autostart).

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

/// Extract up to `limit` names from a `PresetList` response (the reply type
/// shared by the profile and preset listings), in order.
#[must_use]
pub fn names_from_response(resp: &Response, limit: usize) -> Vec<String> {
    if let Response::PresetList(list) = resp {
        list.iter().take(limit).cloned().collect()
    } else {
        Vec::new()
    }
}

/// Fetch profile names (best-effort; empty when the daemon is down). The tray's
/// quick-load list mirrors the GUI, which loads *profiles* (the saved `.toml`
/// configs) via `LoadProfile` — not the on-disk `.fac`/`.txt` preset files that
/// `ListPresets` returns.
#[must_use]
pub fn fetch_profiles(limit: usize) -> Vec<String> {
    let Some(mut c) = client() else {
        return Vec::new();
    };
    match c.send_recv(Command::ListProfiles) {
        Ok(resp) => names_from_response(&resp, limit),
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
    // (state poll + send) for every action, including ones like `LoadProfile`
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
        MenuAction::Quit => {
            // "Quit Resonance" (the default) tears down the daemon too; plain
            // "Quit tray" leaves it running. Best-effort: a failed stop must not
            // block the tray from exiting.
            if tray::TrayConfig::load().quit_stops_daemon {
                let _ = service::stop();
            }
            std::process::exit(0);
        }
        // Handled by plan_command above.
        MenuAction::TogglePower | MenuAction::LoadProfile(_) => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use resonance_ipc::Response;

    #[test]
    fn name_list_is_limited_in_order() {
        let resp = Response::PresetList(vec!["Rock".into(), "Jazz".into(), "Flat".into()]);
        let out = names_from_response(&resp, 2);
        // Truncated to `limit`, names in order.
        assert_eq!(out, vec!["Rock".to_string(), "Jazz".to_string()]);
    }

    #[test]
    fn non_list_response_yields_empty() {
        assert!(names_from_response(&Response::Ok, 8).is_empty());
    }
}
