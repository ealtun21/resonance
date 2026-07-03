//! Windows + macOS tray via `tray-icon`, driven by a `winit` event loop.
//!
//! winit owns the run loop because macOS requires a main-thread `NSApplication`
//! loop to host an `NSStatusItem`; `tray-icon` posts its menu/icon events into
//! the global `muda`/`tray_icon` crossbeam channels, which two forwarder
//! threads relay into the loop as [`UserEvent`]s via an `EventLoopProxy`. The
//! loop sits in [`ControlFlow::Wait`] so it blocks (near-zero CPU) while idle.
//!
//! Compiled only on Windows/macOS (see `backend.rs` cfg).

use crate::icons;
use crate::menu::{MenuAction, MenuModel};
use resonance_ipc::tray::{LeftClick, Ui};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use tray_icon::menu::{
    CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

/// Events pumped into the winit loop from the forwarder threads.
enum UserEvent {
    /// A refreshed display model arrived from the poll thread.
    Model(MenuModel),
    /// A `tray-icon` tray event (left click, hover, …).
    Tray(TrayIconEvent),
    /// A `muda` menu-item activation.
    Menu(MenuEvent),
}

/// Build the platform icon for the current power state.
///
/// **macOS:** renders [`icons::template()`] — a template image, so the OS
/// tints it to match the current menu-bar appearance (light/dark) instead of
/// us guessing a fixed color. Template images carry shape via the alpha
/// channel only; the OS discards RGB, so the active/bypassed color swap the
/// other platforms use wouldn't be visible here. Power state stays visible
/// through the checkable "Power" menu item and the tooltip text.
#[cfg(target_os = "macos")]
fn to_icon(_model: &MenuModel) -> Icon {
    let i = icons::template();
    Icon::from_rgba(i.rgba, i.width, i.height).expect("tray icon rgba is valid")
}

/// Build the platform icon for the current power state.
#[cfg(target_os = "windows")]
fn to_icon(model: &MenuModel) -> Icon {
    let i = if model.power {
        icons::active()
    } else {
        icons::bypassed()
    };
    Icon::from_rgba(i.rgba, i.width, i.height).expect("tray icon rgba is valid")
}

/// Whether a left click should pop the native context menu, per the
/// configured [`LeftClick`] mode. `Menu` → yes; `ToggleUi` → no, leaving our
/// own `MenuAction::LeftClick` → open-UI dispatch as the single action (tray-
/// icon's own default is `true`, which would otherwise double up with it).
fn menu_on_left_click(model: &MenuModel) -> bool {
    matches!(model.left_click, LeftClick::Menu)
}

/// Human-readable preset name: the file stem, falling back to the full path.
fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map_or_else(|| path.to_string(), |s| s.to_string_lossy().into_owned())
}

struct App {
    tray: Option<TrayIcon>,
    menu_ids: HashMap<MenuId, MenuAction>,
    model: MenuModel,
    actions: Sender<MenuAction>,
}

impl App {
    /// Rebuild the whole menu from the current model, repopulating the
    /// `menu id → action` map. Mirrors the Linux backend's structure.
    fn rebuild_menu(&mut self) -> Menu {
        self.menu_ids.clear();
        let m = &self.model;
        let menu = Menu::new();

        // Power: checkable, disabled while the daemon is down.
        let power = CheckMenuItem::new("Power", m.daemon_up, m.daemon_up && m.power, None);
        self.menu_ids
            .insert(power.id().clone(), MenuAction::TogglePower);
        let _ = menu.append(&power);

        // Presets submenu (only when the daemon is up and offers some). The
        // currently loaded preset (if any) is shown checked.
        if m.daemon_up && !m.presets.is_empty() {
            let sub = Submenu::new("Presets", true);
            for p in &m.presets {
                let is_current = m.current.as_deref() == Some(p.as_str());
                let item = CheckMenuItem::new(basename(p), true, is_current, None);
                self.menu_ids
                    .insert(item.id().clone(), MenuAction::LoadPreset(p.clone()));
                let _ = sub.append(&item);
            }
            let _ = menu.append(&sub);
        }

        let _ = menu.append(&PredefinedMenuItem::separator());

        // Open / Quit UI (only when a GUI or TUI is installed).
        if m.uis.iter().any(|u| matches!(u, Ui::Gui | Ui::Tui)) {
            let open = MenuItem::new(
                if m.gui_running { "Show UI" } else { "Open UI" },
                true,
                None,
            );
            self.menu_ids.insert(open.id().clone(), MenuAction::OpenUi);
            let _ = menu.append(&open);
            if m.gui_running {
                let quit = MenuItem::new("Quit UI", true, None);
                self.menu_ids.insert(quit.id().clone(), MenuAction::QuitUi);
                let _ = menu.append(&quit);
            }
        }

        // Daemon lifecycle submenu.
        let dsub = Submenu::new("Daemon", true);
        for (label, action) in [
            ("Start", MenuAction::DaemonStart),
            ("Stop", MenuAction::DaemonStop),
            ("Restart", MenuAction::DaemonRestart),
        ] {
            let it = MenuItem::new(label, true, None);
            self.menu_ids.insert(it.id().clone(), action);
            let _ = dsub.append(&it);
        }
        let _ = dsub.append(&PredefinedMenuItem::separator());
        let dauto = CheckMenuItem::new("Autostart", true, m.daemon_autostart, None);
        self.menu_ids
            .insert(dauto.id().clone(), MenuAction::DaemonAutostart);
        let _ = dsub.append(&dauto);
        let _ = menu.append(&dsub);

        let _ = menu.append(&PredefinedMenuItem::separator());

        let tauto = CheckMenuItem::new("Tray autostart at login", true, m.tray_autostart, None);
        self.menu_ids
            .insert(tauto.id().clone(), MenuAction::TrayAutostart);
        let _ = menu.append(&tauto);

        let quit = MenuItem::new(crate::menu::quit_label(m.quit_stops_daemon), true, None);
        self.menu_ids.insert(quit.id().clone(), MenuAction::Quit);
        let _ = menu.append(&quit);

        menu
    }

    /// Push the current model into the live tray (menu, icon, tooltip, and
    /// the left-click-opens-menu setting, so a config reload takes effect
    /// without restarting the tray).
    fn refresh_tray(&mut self) {
        let menu = self.rebuild_menu();
        let icon = to_icon(&self.model);
        let show_menu_on_left_click = menu_on_left_click(&self.model);
        if let Some(tray) = &self.tray {
            tray.set_menu(Some(Box::new(menu)));
            // `set_icon_with_as_template` is a COMPLETE no-op on Windows in
            // tray-icon 0.19.3 (it discards both args and never applies the
            // icon), so the icon update is dropped there, not just the
            // template flag. Use the plain setter on Windows and the
            // template-aware one on macOS, where it keeps the icon marked as
            // a template so the OS re-tints it.
            #[cfg(target_os = "macos")]
            let _ = tray.set_icon_with_as_template(Some(icon), true);
            #[cfg(target_os = "windows")]
            let _ = tray.set_icon(Some(icon));
            let _ = tray.set_tooltip(Some(&self.model.tooltip));
            tray.set_show_menu_on_left_click(show_menu_on_left_click);
        }
    }

    /// Dispatch a menu-item activation to its mapped action. `Quit` is handled
    /// by the action thread (`daemon::execute`), which stops the daemon when
    /// configured and then exits the whole process — so the winit loop must not
    /// also exit here (that would race the daemon teardown to termination).
    fn on_menu(&mut self, id: &MenuId) {
        if let Some(action) = self.menu_ids.get(id).cloned() {
            let _ = self.actions.send(action);
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, _el: &ActiveEventLoop) {
        if self.tray.is_none() {
            let menu = self.rebuild_menu();
            let icon = to_icon(&self.model);
            self.tray = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip(&self.model.tooltip)
                .with_icon(icon)
                // No-op on Windows; on macOS marks the icon as a template so
                // the OS tints it for the current menu-bar appearance.
                .with_icon_as_template(cfg!(target_os = "macos"))
                .with_menu_on_left_click(menu_on_left_click(&self.model))
                .build()
                .ok();
        }
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, _ev: WindowEvent) {}

    fn user_event(&mut self, _el: &ActiveEventLoop, ev: UserEvent) {
        match ev {
            UserEvent::Model(m) => {
                self.model = m;
                self.refresh_tray();
            }
            UserEvent::Menu(me) => self.on_menu(&me.id),
            // Treat a completed left click on the icon as `LeftClick`; ignore
            // hover/move/enter/leave and non-left buttons so we fire once.
            UserEvent::Tray(TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }) => {
                let _ = self.actions.send(MenuAction::LeftClick);
            }
            UserEvent::Tray(_) => {}
        }
    }
}

/// Run the desktop tray backend, blocking for the process lifetime.
///
/// # Errors
/// Returns an error if the winit event loop cannot be built or if it exits
/// abnormally.
pub fn run(
    init: MenuModel,
    updates: Receiver<MenuModel>,
    actions: Sender<MenuAction>,
) -> anyhow::Result<()> {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    // Idle = blocked, near-zero CPU; the forwarder threads wake us on demand.
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();

    // Forward model updates from the poll thread into the loop.
    {
        let proxy = proxy.clone();
        std::thread::spawn(move || {
            for m in updates {
                if proxy.send_event(UserEvent::Model(m)).is_err() {
                    break;
                }
            }
        });
    }
    // Forward the muda menu channel (blocking recv = no polling).
    {
        let proxy = proxy.clone();
        std::thread::spawn(move || {
            let rx = MenuEvent::receiver();
            while let Ok(ev) = rx.recv() {
                if proxy.send_event(UserEvent::Menu(ev)).is_err() {
                    break;
                }
            }
        });
    }
    // Forward the tray-icon event channel (blocking recv = no polling).
    {
        let proxy = proxy.clone();
        std::thread::spawn(move || {
            let rx = TrayIconEvent::receiver();
            while let Ok(ev) = rx.recv() {
                if proxy.send_event(UserEvent::Tray(ev)).is_err() {
                    break;
                }
            }
        });
    }

    let mut app = App {
        tray: None,
        menu_ids: HashMap::new(),
        model: init,
        actions,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}
