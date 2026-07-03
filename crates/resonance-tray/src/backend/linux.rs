//! Linux tray via ksni (`StatusNotifierItem` over D-Bus). No GTK.

use crate::icons;
use crate::menu::{MenuAction, MenuModel};
use ksni::menu::{MenuItem, StandardItem, SubMenu};
use resonance_ipc::tray::Ui;
use std::sync::mpsc::{Receiver, Sender};

struct TrayApp {
    model: MenuModel,
    actions: Sender<MenuAction>,
}

impl TrayApp {
    fn emit(&self, a: MenuAction) {
        let _ = self.actions.send(a);
    }
}

impl ksni::Tray for TrayApp {
    fn id(&self) -> String {
        // Stable id so hosts keep the item across menu rebuilds.
        "resonance-tray".into()
    }

    fn title(&self) -> String {
        self.model.status.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Resonance".into(),
            description: self.model.tooltip.clone(),
            ..Default::default()
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let i = if self.model.power {
            icons::active()
        } else {
            icons::bypassed()
        };
        vec![ksni::Icon {
            width: i.width as i32,
            height: i.height as i32,
            data: rgba_to_argb(&i.rgba),
        }]
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        // Left click.
        self.emit(MenuAction::LeftClick);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let m = &self.model;
        let mut items: Vec<MenuItem<Self>> = Vec::new();

        // Power (checkable via label marker), disabled when the daemon is down.
        items.push(
            StandardItem {
                label: check_label("Power", m.daemon_up && m.power),
                enabled: m.daemon_up,
                activate: Box::new(|t: &mut Self| t.emit(MenuAction::TogglePower)),
                ..Default::default()
            }
            .into(),
        );

        // Presets submenu — the saved profiles (what the GUI quick-loads). The
        // currently loaded one (if any) is checked.
        if m.daemon_up && !m.profiles.is_empty() {
            let sub: Vec<MenuItem<Self>> = m
                .profiles
                .iter()
                .map(|p| {
                    let name = p.clone();
                    let is_current = m.current.as_deref() == Some(p.as_str());
                    StandardItem {
                        label: check_label(p, is_current),
                        activate: Box::new(move |t: &mut Self| {
                            t.emit(MenuAction::LoadProfile(name.clone()));
                        }),
                        ..Default::default()
                    }
                    .into()
                })
                .collect();
            items.push(
                SubMenu {
                    label: "Presets".into(),
                    submenu: sub,
                    ..Default::default()
                }
                .into(),
            );
        }

        items.push(MenuItem::Separator);

        // Open / Quit UI (adapts to installed UIs).
        if m.uis.iter().any(|u| matches!(u, Ui::Gui | Ui::Tui)) {
            items.push(
                StandardItem {
                    label: if m.gui_running {
                        "Show UI".into()
                    } else {
                        "Open UI".into()
                    },
                    activate: Box::new(|t: &mut Self| t.emit(MenuAction::OpenUi)),
                    ..Default::default()
                }
                .into(),
            );
            if m.gui_running {
                items.push(
                    StandardItem {
                        label: "Quit UI".into(),
                        activate: Box::new(|t: &mut Self| t.emit(MenuAction::QuitUi)),
                        ..Default::default()
                    }
                    .into(),
                );
            }
        }

        // Daemon submenu.
        items.push(
            SubMenu {
                label: "Daemon".into(),
                submenu: vec![
                    StandardItem {
                        label: "Start".into(),
                        activate: Box::new(|t: &mut Self| t.emit(MenuAction::DaemonStart)),
                        ..Default::default()
                    }
                    .into(),
                    StandardItem {
                        label: "Stop".into(),
                        activate: Box::new(|t: &mut Self| t.emit(MenuAction::DaemonStop)),
                        ..Default::default()
                    }
                    .into(),
                    StandardItem {
                        label: "Restart".into(),
                        activate: Box::new(|t: &mut Self| t.emit(MenuAction::DaemonRestart)),
                        ..Default::default()
                    }
                    .into(),
                    MenuItem::Separator,
                    StandardItem {
                        label: check_label("Autostart", m.daemon_autostart),
                        activate: Box::new(|t: &mut Self| t.emit(MenuAction::DaemonAutostart)),
                        ..Default::default()
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
        );

        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: check_label("Tray autostart at login", m.tray_autostart),
                activate: Box::new(|t: &mut Self| t.emit(MenuAction::TrayAutostart)),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: crate::menu::quit_label(m.quit_stops_daemon).into(),
                activate: Box::new(|t: &mut Self| t.emit(MenuAction::Quit)),
                ..Default::default()
            }
            .into(),
        );

        items
    }
}

/// Prefix a check mark when `on`, otherwise pad to keep labels aligned.
fn check_label(text: &str, on: bool) -> String {
    if on {
        format!("\u{2713} {text}")
    } else {
        format!("   {text}")
    }
}

/// ksni wants ARGB32 (network byte order); `image` gives RGBA8.
fn rgba_to_argb(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for px in rgba.chunks_exact(4) {
        out.push(px[3]); // A
        out.push(px[0]); // R
        out.push(px[1]); // G
        out.push(px[2]); // B
    }
    out
}

/// Run the ksni tray service, applying model updates until the channel closes.
///
/// # Errors
/// Currently infallible on this path (the ksni service is spawned on its own
/// thread), but returns `Result` to match the cross-platform `run` signature.
// Result is required for signature parity with the win/mac backend and
// `backend::run`; the ksni path just cannot fail here yet.
#[allow(clippy::unnecessary_wraps)]
pub fn run(
    init: MenuModel,
    updates: Receiver<MenuModel>,
    actions: Sender<MenuAction>,
) -> anyhow::Result<()> {
    let service = ksni::TrayService::new(TrayApp {
        model: init,
        actions,
    });
    let handle = service.handle();
    service.spawn();
    // Apply model updates on this thread until the channel closes.
    for model in updates {
        handle.update(move |t: &mut TrayApp| t.model = model);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_label_marks_only_when_on() {
        assert!(check_label("Power", true).starts_with('\u{2713}'));
        assert!(!check_label("Power", false).contains('\u{2713}'));
        assert!(check_label("Power", false).contains("Power"));
    }

    #[test]
    fn rgba_to_argb_reorders_channels() {
        // one opaque pixel R=1 G=2 B=3 A=4 -> A R G B
        assert_eq!(rgba_to_argb(&[1, 2, 3, 4]), vec![4, 1, 2, 3]);
    }
}
