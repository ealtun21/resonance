//! Backend-agnostic menu model + the pure action→Command mapping.

use resonance_ipc::tray::{LeftClick, TrayConfig, Ui};
use resonance_ipc::{Command, DaemonState};

/// A user-initiated tray menu action. Only a subset map to a daemon `Command`
/// (see [`plan_command`]) — the rest are side-effecting (service lifecycle, UI
/// process launch, local config) and handled directly by `daemon.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    TogglePower,
    LoadProfile(String),
    DaemonStart,
    DaemonStop,
    DaemonRestart,
    DaemonAutostart,
    OpenUi,
    QuitUi,
    TrayAutostart,
    LeftClick,
    Quit,
}

/// Everything the tray menu needs to render itself, derived from the latest
/// daemon snapshot (or its absence) plus local config/discovery state.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuModel {
    pub daemon_up: bool,
    pub power: bool,
    pub status: String,
    pub tooltip: String,
    /// Quick-load list shown in the menu: profile names (the saved `.toml`
    /// configs), mirroring the GUI's `LoadProfile` quick-load — NOT the on-disk
    /// `.fac`/`.txt` preset files.
    pub profiles: Vec<String>,
    pub current: Option<String>,
    pub uis: Vec<Ui>,
    pub gui_running: bool,
    pub tray_autostart: bool,
    pub daemon_autostart: bool,
    pub left_click: LeftClick,
    /// When true, the quit item tears down the daemon too — so it reads
    /// "Quit Resonance" rather than "Quit tray".
    pub quit_stops_daemon: bool,
}

/// Build the display model from the latest daemon snapshot (or `None` when the
/// daemon is unreachable) plus local config/discovery.
#[must_use]
pub fn build_model(
    state: Option<&DaemonState>,
    cfg: &TrayConfig,
    uis: &[Ui],
    profiles: &[String],
    tray_autostart: bool,
    daemon_autostart: bool,
    gui_running: bool,
) -> MenuModel {
    let (daemon_up, power, status, tooltip, current) = match state {
        Some(s) => {
            let status = if s.enabled {
                "Resonance — active".to_string()
            } else {
                "Resonance — bypassed".to_string()
            };
            let tip = format!(
                "{} • {:.0} kHz • {} ch{}",
                if s.enabled { "active" } else { "bypassed" },
                s.sample_rate / 1000.0,
                s.channels,
                s.current_preset
                    .as_deref()
                    .map(|p| format!(" • {p}"))
                    .unwrap_or_default(),
            );
            (true, s.enabled, status, tip, s.current_preset.clone())
        }
        None => (
            false,
            false,
            "Resonance — daemon stopped".to_string(),
            "daemon not running".to_string(),
            None,
        ),
    };
    let mut profiles = profiles.to_vec();
    profiles.truncate(cfg.recent_count);
    MenuModel {
        daemon_up,
        power,
        status,
        tooltip,
        profiles,
        current,
        uis: uis.to_vec(),
        gui_running,
        tray_autostart,
        daemon_autostart,
        left_click: cfg.left_click,
        quit_stops_daemon: cfg.quit_stops_daemon,
    }
}

/// The quit menu item's label, adapting to what quitting actually does:
/// "Quit Resonance" when it also tears down the daemon (everything), else the
/// tray-only "Quit tray".
#[must_use]
pub fn quit_label(quit_stops_daemon: bool) -> &'static str {
    if quit_stops_daemon {
        "Quit Resonance"
    } else {
        "Quit tray"
    }
}

/// Map an action to a daemon `Command`, or `None` if it is handled by
/// side-effecting code (service lifecycle, UI launch, autostart, quit).
#[must_use]
pub fn plan_command(action: &MenuAction, state: Option<&DaemonState>) -> Option<Command> {
    match action {
        MenuAction::TogglePower => {
            let enabled = state.is_none_or(|s| s.enabled);
            Some(Command::SetPower { enabled: !enabled })
        }
        MenuAction::LoadProfile(name) => Some(Command::LoadProfile { name: name.clone() }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> TrayConfig {
        TrayConfig::default()
    }

    /// A complete, valid `DaemonState` for tests. `DaemonState` does not derive
    /// `Default` (it is a shared IPC type with no natural "empty" daemon state),
    /// so this constructs every field explicitly, mirroring the fixture in
    /// `resonance-ipc`'s own `daemon_state_round_trips` test.
    fn sample_state() -> DaemonState {
        DaemonState {
            enabled: true,
            preamp_db: 0.0,
            eq_enabled: true,
            bands: vec![],
            effects: resonance_ipc::EffectsState::default(),
            current_preset: None,
            sample_rate: 48000.0,
            capture_rate: 48000.0,
            channels: 2,
            out_channels: 2,
            channel_layout: resonance_ipc::default_channel_layout(2),
            routing: None,
            spectrum: vec![],
            phase_mode_linear: false,
            eq_fir_latency_frames: 0,
            active_output: None,
            mapped_profile: None,
            available_sinks: vec![],
            sink_descriptions: vec![],
            preferred_output: None,
            meters: resonance_ipc::Meters::default(),
            apps: vec![],
            sinks: vec![],
            dither_bits: None,
            convolution: None,
            audition: None,
        }
    }

    #[test]
    fn daemon_down_disables_power_but_offers_open() {
        let m = build_model(None, &cfg(), &[Ui::Gui], &[], false, false, false);
        assert!(!m.daemon_up);
        assert!(
            m.status.to_lowercase().contains("stopped") || m.status.to_lowercase().contains("not")
        );
        assert!(m.uis.contains(&Ui::Gui));
    }

    #[test]
    fn toggle_power_inverts_current_state() {
        let mut st = sample_state();
        st.enabled = true;
        assert_eq!(
            plan_command(&MenuAction::TogglePower, Some(&st)),
            Some(Command::SetPower { enabled: false })
        );
        st.enabled = false;
        assert_eq!(
            plan_command(&MenuAction::TogglePower, Some(&st)),
            Some(Command::SetPower { enabled: true })
        );
    }

    #[test]
    fn load_profile_maps_to_load_command() {
        assert_eq!(
            plan_command(&MenuAction::LoadProfile("Rock".into()), None),
            Some(Command::LoadProfile {
                name: "Rock".into()
            })
        );
    }

    #[test]
    fn quit_label_reflects_whether_it_stops_the_daemon() {
        assert_eq!(quit_label(true), "Quit Resonance");
        assert_eq!(quit_label(false), "Quit tray");
    }

    #[test]
    fn build_model_carries_quit_stops_daemon() {
        let mut cfg = cfg();
        cfg.quit_stops_daemon = false;
        let m = build_model(None, &cfg, &[Ui::Gui], &[], false, false, false);
        assert!(!m.quit_stops_daemon);
    }

    #[test]
    fn lifecycle_and_ui_actions_have_no_daemon_command() {
        for a in [
            MenuAction::DaemonStart,
            MenuAction::OpenUi,
            MenuAction::TrayAutostart,
            MenuAction::Quit,
        ] {
            assert_eq!(plan_command(&a, None), None);
        }
    }
}
