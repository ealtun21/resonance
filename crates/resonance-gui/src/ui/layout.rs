//! Central-area layout: the disconnected start screen and the responsive shell
//! (FR graph + spectrum, with a width-driven 3-column / single-column accordion
//! lower area).

use crate::app::{GuiApp, ServiceAction, ServiceFn};
use crate::ui::widgets::{accordion, padded_scroll};
use eframe::egui;
use resonance_ipc::{DaemonState, service};

/// Default / fallback widths of the Effects & Devices side panels in the wide
/// 3-column layout; EQ bands (central) takes whatever's left so its table is
/// never the one that squishes.
pub(crate) const EFFECTS_W: f32 = 300.0;
pub(crate) const DEVICES_W: f32 = 380.0;

/// Width at/above which the lower area uses the 3-column layout; below it the
/// sections stack into a single-column accordion. Chosen so the central EQ-bands
/// table has room for its (responsively-collapsing) columns at the breakpoint. A
/// 24px hysteresis band (kept in temp memory) stops a window parked near the
/// threshold from flip-flopping.
pub(crate) const WIDE_MIN: f32 = 1120.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum LayoutMode {
    Wide,
    Narrow,
}

/// Pick the layout mode from the available width directly (never from a
/// measured-content width — that fed back and jittered). Hysteresis: once
/// narrow, the window must grow to `WIDE_MIN` to return to columns; once wide,
/// it can shrink to `WIDE_MIN - 24` before collapsing.
fn layout_mode(ctx: &egui::Context, avail_w: f32) -> LayoutMode {
    let id = egui::Id::new("layout_is_wide");
    let was_wide = ctx
        .data(|d| d.get_temp::<bool>(id))
        .unwrap_or(avail_w >= WIDE_MIN);
    let wide = if was_wide {
        avail_w >= WIDE_MIN - 24.0
    } else {
        avail_w >= WIDE_MIN
    };
    ctx.data_mut(|d| d.insert_temp(id, wide));
    if wide {
        LayoutMode::Wide
    } else {
        LayoutMode::Narrow
    }
}

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

    /// The main content shell: FR graph (hero) + spectrum (slim band) always
    /// span the width; the lower sections render as 3 columns when the window is
    /// wide, or a stacked accordion when narrow. Shown only when connected.
    pub(crate) fn shell(&mut self, ui: &mut egui::Ui) {
        if self.state.is_none() {
            egui::CentralPanel::default().show_inside(ui, |ui| self.disconnected(ui));
            return;
        }
        let state = self.state.clone();
        // FR graph is the hero (~52% height); spectrum a slim band (~14%). Both
        // resizable — default_size only applies until the user drags a splitter.
        let fr_h = (ui.available_height() * 0.52).max(70.0);
        let spec_h = (ui.available_height() * 0.14).max(28.0);
        egui::Panel::top("fr")
            .resizable(true)
            .default_size(fr_h)
            .min_size(70.0)
            .show_inside(ui, |ui| {
                if let Some(s) = &state {
                    self.eq_curve(ui, s);
                }
            });
        egui::Panel::bottom("spectrum")
            .resizable(true)
            .default_size(spec_h)
            .min_size(28.0)
            .show_inside(ui, |ui| {
                if let Some(s) = &state {
                    self.spectrum(ui, s);
                }
            });
        match layout_mode(ui.ctx(), ui.available_width()) {
            LayoutMode::Wide => self.lower_columns(ui, &state),
            LayoutMode::Narrow => {
                egui::CentralPanel::default()
                    .show_inside(ui, |ui| self.accordion_stack(ui, &state));
            }
        }
    }

    /// Wide layout: fixed-width Effects (left) + Devices/Profiles (right) side
    /// panels; EQ bands (central) takes the rest so its table never squishes.
    fn lower_columns(&mut self, ui: &mut egui::Ui, state: &Option<DaemonState>) {
        egui::Panel::left("fx_pane")
            .resizable(false)
            .default_size(EFFECTS_W)
            .show_inside(ui, |ui| {
                if let Some(s) = state {
                    padded_scroll(ui, "effects_scroll", |ui| self.effects_section(ui, s));
                }
            });
        egui::Panel::right("dev_pane")
            .resizable(false)
            .default_size(DEVICES_W)
            .show_inside(ui, |ui| {
                padded_scroll(ui, "side", |ui| self.devices_profiles(ui));
            });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            if let Some(s) = state {
                padded_scroll(ui, "bands_scroll", |ui| self.bands_section(ui, s));
            }
        });
    }

    /// Narrow layout: the lower sections stacked as collapsible cards inside one
    /// scroll area, so every section stays reachable without horizontal crowding.
    fn accordion_stack(&mut self, ui: &mut egui::Ui, state: &Option<DaemonState>) {
        egui::ScrollArea::vertical()
            .id_salt("accordion")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Frame::default()
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        accordion(ui, "acc_fx", "Effects", true, |ui| {
                            if let Some(s) = state {
                                self.effects_section(ui, s);
                            }
                        });
                        accordion(ui, "acc_bands", "EQ bands", true, |ui| {
                            if let Some(s) = state {
                                self.bands_section(ui, s);
                            }
                        });
                        accordion(ui, "acc_map", "Device Mapping", false, |ui| {
                            self.device_mapping_section(ui)
                        });
                        accordion(ui, "acc_prof", "Profiles", false, |ui| {
                            self.profiles_panel(ui)
                        });
                    });
            });
    }
}
