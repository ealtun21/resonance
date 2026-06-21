//! Central-area layout: the disconnected start screen and the narrow-window
//! tabbed fallback that collapses the three lower sections into one pane.

use crate::app::{GuiApp, ServiceAction, ServiceFn};
use crate::state::LowerTab;
use crate::ui::widgets::{centered, padded_scroll};
use eframe::egui;
use resonance_ipc::{DaemonState, service};

/// Default widths of the Effects / Devices side columns; EQ bands (central)
/// takes whatever's left so its 8-column table is never the one that squishes.
/// Default / fallback widths of the Effects & Devices side panels (used as
/// `default_size` and when no resized width is stored yet). The tab-vs-columns
/// decision is measured at runtime, not derived from these.
pub(crate) const EFFECTS_W: f32 = 300.0;
pub(crate) const DEVICES_W: f32 = 420.0;
/// Fallback natural width of the EQ bands table before it's been measured —
/// keeps the first frame in column layout at the default window size.
pub(crate) const DEFAULT_BANDS_W: f32 = 500.0;

impl GuiApp {
    /// Centre screen shown while no daemon is connected: a one-click start
    /// button instead of asking the user to type `resonanced`.
    pub(crate) fn disconnected(&mut self, ui: &mut egui::Ui) {
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.heading("No daemon connected");
                ui.add_space(8.0);
                ui.label(&self.status);
                ui.add_space(16.0);
                if service::manager_available() {
                    let busy = self.service_busy;
                    let btn = egui::Button::new(
                        egui::RichText::new(if busy {
                            "starting…"
                        } else {
                            "▶  Start daemon"
                        })
                        .size(18.0)
                        .color(egui::Color32::WHITE),
                    )
                    .fill(self.palette.boost)
                    .min_size(egui::vec2(180.0, 40.0));
                    if ui.add_enabled(!busy, btn).clicked() {
                        self.service_busy = true;
                        let _ = self.service_tx.send(ServiceAction::Run {
                            label: "Start",
                            f: service::start,
                        });
                    }
                    ui.add_space(6.0);
                    let mut autostart = self.daemon_status.enabled;
                    let auto = ui.add_enabled(
                        !busy,
                        egui::Checkbox::new(&mut autostart, "Start automatically at login"),
                    );
                    if auto.changed() {
                        self.service_busy = true;
                        let f: ServiceFn = if autostart {
                            service::enable
                        } else {
                            service::disable
                        };
                        let _ = self.service_tx.send(ServiceAction::Run {
                            label: "autostart",
                            f,
                        });
                    }
                } else {
                    ui.label(service::manager_unavailable_message());
                }
            });
        });
    }

    /// Narrow-window fallback: the lower sections as one tabbed pane. A centred
    /// tab bar picks Effects / EQ bands / Device Profile Mapping / Profiles; the
    /// chosen section fills the full width below.
    pub(crate) fn lower_tabs(&mut self, ui: &mut egui::Ui, state: &Option<DaemonState>) {
        ui.add_space(4.0);
        let tabs = [
            (LowerTab::Effects, "Effects"),
            (LowerTab::Bands, "EQ bands"),
            (LowerTab::Mapping, "Device Profile Mapping"),
            (LowerTab::Profiles, "Profiles"),
        ];
        // `centered` pads from the row's measured width so the (variable-length)
        // tab bar sits centred.
        centered(ui, "lower_tabs", |ui| {
            ui.horizontal(|ui| {
                for (tab, label) in tabs {
                    let sel = self.lower_tab == tab;
                    if ui.add(egui::Button::selectable(sel, label)).clicked() {
                        self.lower_tab = tab;
                    }
                }
            });
        });
        ui.separator();
        match self.lower_tab {
            LowerTab::Effects => {
                padded_scroll(ui, "tab_effects", |ui| {
                    if let Some(s) = state {
                        self.effects_section(ui, s);
                    }
                });
            }
            LowerTab::Bands => {
                padded_scroll(ui, "tab_bands", |ui| {
                    if let Some(s) = state {
                        self.bands_section(ui, s);
                    }
                });
            }
            LowerTab::Mapping => {
                padded_scroll(ui, "tab_mapping", |ui| self.device_mapping_section(ui));
            }
            LowerTab::Profiles => {
                padded_scroll(ui, "tab_profiles", |ui| self.profiles_panel(ui));
            }
        }
    }
}
