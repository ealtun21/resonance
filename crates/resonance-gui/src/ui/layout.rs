//! Central-area layout: the disconnected start screen and the responsive shell
//! (FR graph + spectrum, with a width-driven 3-column / single-column accordion
//! lower area).

use crate::app::{GuiApp, ServiceAction, ServiceFn};
use crate::ui::widgets::{accordion, padded_scroll, section};
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

    /// The main content shell. The FR graph is the elastic **hero**: it's the
    /// `CentralPanel`, so it absorbs all width/height the toolbar and controls
    /// don't use (a wide window widens the graph instead of stranding empty space
    /// — the FabFilter/Pro-Q model). The live spectrum is drawn *inside* the graph
    /// (see `eq_curve`), so there's no separate panel to go black on silence. The
    /// controls cluster is a resizable bottom panel: 3 clamped columns when wide,
    /// a stacked accordion when narrow.
    pub(crate) fn shell(&mut self, ui: &mut egui::Ui) {
        if self.state.is_none() {
            egui::CentralPanel::default().show_inside(ui, |ui| self.disconnected(ui));
            return;
        }
        let state = self.state.clone();
        let mode = layout_mode(ui.ctx(), ui.available_width());
        // Controls default to ~40% of the height (clamped), leaving the rest to
        // the graph; the splitter lets the user trade one for the other.
        let controls_h = (ui.available_height() * 0.4).clamp(150.0, 340.0);
        egui::Panel::bottom("controls_panel")
            .resizable(true)
            .default_size(controls_h)
            .min_size(72.0)
            .show_inside(ui, |ui| match mode {
                LayoutMode::Wide => self.lower_columns(ui, &state),
                LayoutMode::Narrow => {
                    egui::ScrollArea::vertical()
                        .id_salt("controls_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.accordion_stack(ui, &state));
                }
            });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            if let Some(s) = &state {
                self.eq_curve(ui, s);
            }
        });
    }

    /// Wide layout: three columns — Effects | EQ bands (widest) | Devices/Profiles
    /// — capped to a max width and centred so an ultra-wide window leaves neutral
    /// surface margins (libadwaita `AdwClamp` idiom) instead of stretching cards
    /// across voids. Manual columns (not side panels) so the cluster can centre.
    fn lower_columns(&mut self, ui: &mut egui::Ui, state: &Option<DaemonState>) {
        const CAP: f32 = 1200.0;
        const GAP: f32 = 10.0;
        let avail = ui.available_width();
        let w = avail.min(CAP);
        let side_pad = ((avail - w) / 2.0).max(0.0);
        let bands_w = (w - EFFECTS_W - DEVICES_W - 2.0 * GAP).max(240.0);
        let h = ui.available_height();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.add_space(side_pad);
            ui.allocate_ui(egui::vec2(EFFECTS_W, h), |ui| {
                if let Some(s) = state {
                    padded_scroll(ui, "effects_scroll", |ui| {
                        section(ui, "Effects", |ui| self.effects_section(ui, s))
                    });
                }
            });
            ui.separator();
            ui.allocate_ui(egui::vec2(bands_w, h), |ui| {
                if let Some(s) = state {
                    padded_scroll(ui, "bands_scroll", |ui| {
                        section(ui, "EQ bands", |ui| self.bands_section(ui, s))
                    });
                }
            });
            ui.separator();
            ui.allocate_ui(egui::vec2(DEVICES_W, h), |ui| {
                padded_scroll(ui, "side", |ui| self.devices_profiles(ui));
            });
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
                        accordion(ui, "acc_fx_v2", "Effects", true, |ui| {
                            if let Some(s) = state {
                                self.effects_section(ui, s);
                            }
                        });
                        accordion(ui, "acc_bands_v2", "EQ bands", true, |ui| {
                            if let Some(s) = state {
                                self.bands_section(ui, s);
                            }
                        });
                        accordion(ui, "acc_map_v2", "Device Mapping", false, |ui| {
                            self.device_mapping_section(ui)
                        });
                        accordion(ui, "acc_prof_v2", "Profiles", false, |ui| {
                            self.profiles_panel(ui)
                        });
                    });
            });
    }
}
