//! Central-area layout: the disconnected start screen and the responsive shell
//! (FR graph + spectrum, with a width-driven 3-column / single-column accordion
//! lower area).

use crate::app::{GuiApp, ServiceAction, ServiceFn};
use crate::ui::kit;
use crate::ui::widgets::{padded_scroll, section, section_hint};
use eframe::egui;
use resonance_ipc::{DaemonState, service};

/// Default / fallback widths of the Effects & Devices side panels in the wide
/// 3-column layout; EQ bands (central) takes whatever's left so its table is
/// never the one that squishes.
pub(crate) const EFFECTS_W: f32 = 320.0;
pub(crate) const DEVICES_W: f32 = 384.0;
/// Minimum comfortable width for the central EQ-bands column in the 3-column
/// layout — enough for the bands table's core columns plus a usable gain graph.
pub(crate) const BANDS_MIN: f32 = 440.0;

/// Width at/above which the lower area uses the 3-column layout; below it the
/// sections stack into a single-column accordion. Derived from the actual column
/// widths (the two side panels + a comfortable centre) rather than a standalone
/// magic number, so changing a column width moves the breakpoint with it instead
/// of leaving a stale threshold. A 24px hysteresis band (kept in temp memory)
/// stops a window parked near the threshold from flip-flopping.
pub(crate) const WIDE_MIN: f32 = EFFECTS_W + DEVICES_W + BANDS_MIN;

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
                    let label = if busy {
                        "starting…"
                    } else {
                        "▶  Start daemon"
                    };
                    if kit::button_sized(ui, label, true, !busy, egui::vec2(180.0, 40.0), 18.0) {
                        self.service_busy = true;
                        let _ = self.service_tx.send(ServiceAction::Run {
                            label: "Start",
                            f: service::start,
                        });
                    }
                    ui.add_space(8.0);
                    let mut autostart = self.daemon_status.enabled;
                    if kit::checkbox(ui, &mut autostart, "Start automatically at login") && !busy {
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
        match mode {
            // Wide: graph is the elastic CentralPanel (full-bleed hero); the three
            // control columns sit in a resizable bottom strip.
            LayoutMode::Wide => {
                // 60/40 split: the graph hero keeps ~60% of the height, the
                // controls strip ~40%, with only a usability floor — no upper cap,
                // so a tall/maximised window honours the ratio instead of pinning
                // the controls at a fixed height (and `reset_layout` lands here).
                let controls_h = (ui.available_height() * 0.4).max(150.0);
                egui::Panel::bottom("controls_panel")
                    .resizable(true)
                    .default_size(controls_h)
                    .min_size(80.0)
                    .show_separator_line(false)
                    .show_inside(ui, |ui| self.lower_columns(ui, state.as_ref()));
                // The hero card is the CentralPanel itself (so it fills): a card
                // frame holds the head, plot, readout AND the reference bar as
                // nested panels (mockup — the reference bar lives inside the graph
                // card, not as a separate strip below it).
                let t = kit::tokens(ui);
                let hero_frame = egui::Frame::default()
                    .fill(ui.visuals().extreme_bg_color)
                    .stroke(egui::Stroke::new(1.0, t.line))
                    .corner_radius(egui::CornerRadius::same(kit::R_CARD as u8))
                    .outer_margin(egui::Margin {
                        left: 8,
                        right: 8,
                        top: 8,
                        bottom: 4,
                    });
                egui::CentralPanel::default()
                    .frame(hero_frame)
                    .show_inside(ui, |ui| {
                        if let Some(s) = &state {
                            self.hero(ui, s);
                        }
                    });
            }
            // Narrow: graph on top (resizable, with a floor so it stays usable),
            // the accordion of sections scrolls in the central area below — open
            // sections fill it; the splitter trades graph height for controls.
            LayoutMode::Narrow => {
                let gh = (ui.available_height() * 0.5).max(180.0);
                egui::Panel::top("graph_narrow")
                    .resizable(true)
                    .default_size(gh)
                    .min_size(150.0)
                    .show_separator_line(false)
                    .show_inside(ui, |ui| {
                        if let Some(s) = &state {
                            self.eq_curve(ui, s);
                        }
                    });
                egui::Panel::top("reference_bar_narrow")
                    .resizable(false)
                    .show_separator_line(false)
                    .show_inside(ui, |ui| self.reference_bar(ui));
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("controls_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.accordion_stack(ui, state.as_ref()));
                });
            }
        }
    }

    /// Wide layout: three columns — Effects | EQ bands (flexible centre) |
    /// Devices/Profiles — that FILL the width like a native desktop app's panes
    /// (thin splitter rules between them). EQ bands takes all the slack so its
    /// table grows into the space rather than leaving a centred island.
    fn lower_columns(&mut self, ui: &mut egui::Ui, state: Option<&DaemonState>) {
        // Frame::NONE on every column so they share one top inset (the panels'
        // default frames differ — that's why EQ bands sat lower than its
        // neighbours) and no separator lines, so the three cards float on the body
        // background with plain gaps between them (mockup `.controls`), instead of
        // egui's panel-boundary grid lines.
        egui::Panel::left("effects_col")
            .resizable(false)
            .exact_size(EFFECTS_W)
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .show_inside(ui, |ui| {
                if let Some(s) = state {
                    padded_scroll(ui, "effects_scroll", |ui| {
                        section_hint(ui, "Effects", "DSP sound effects", |ui| {
                            self.effects_section(ui, s);
                        });
                        // Channels sits under Effects (matches the design mock).
                        // Multi-channel-only — stereo users still get the L/R swap.
                        if s.channels >= 2 {
                            ui.add_space(12.0);
                            section(ui, "Channels", |ui| self.channels_section(ui, s));
                        }
                    });
                }
            });
        egui::Panel::right("devices_col")
            .resizable(false)
            .exact_size(DEVICES_W)
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .show_inside(ui, |ui| {
                padded_scroll(ui, "side", |ui| {
                    // Applications (per-app volume) sits at the top of the right
                    // column when the backend reports app streams — adjusted more
                    // often than the device→profile mapping below it.
                    if let Some(s) = state {
                        if !s.apps.is_empty() {
                            section_hint(ui, "Applications", "per-app volume", |ui| {
                                self.apps_section(ui, s);
                            });
                            ui.add_space(12.0);
                        }
                    }
                    self.devices_profiles(ui);
                });
            });
        // The centre column IS the bands card: a card-styled CentralPanel (so it
        // fills the column height), with the gutter as its outer margin. Its body
        // (head/table/footer nested panels) lives in `bands_card`.
        let t = kit::tokens(ui);
        let card_frame = egui::Frame::default()
            .fill(ui.visuals().faint_bg_color)
            .stroke(egui::Stroke::new(1.0, t.line))
            .corner_radius(egui::CornerRadius::same(kit::R_CARD as u8))
            .outer_margin(egui::Margin::symmetric(8, 10));
        egui::CentralPanel::default()
            .frame(card_frame)
            .show_inside(ui, |ui| {
                if let Some(s) = state {
                    self.bands_card(ui, s);
                }
            });
    }

    /// Narrow layout: the lower sections stacked as the same collapsible cards
    /// the wide layout uses (the titlebar view) rather than plain-text accordion
    /// headers. Same titles ⇒ collapse state is shared with the wide layout. No
    /// scroll area of its own — the caller wraps it in one.
    fn accordion_stack(&mut self, ui: &mut egui::Ui, state: Option<&DaemonState>) {
        const GAP: f32 = 8.0;
        egui::Frame::default()
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                if let Some(s) = state {
                    section_hint(ui, "Effects", "DSP sound effects", |ui| {
                        self.effects_section(ui, s);
                    });
                    if !s.apps.is_empty() {
                        ui.add_space(GAP);
                        section_hint(ui, "Applications", "per-app volume", |ui| {
                            self.apps_section(ui, s);
                        });
                    }
                    ui.add_space(GAP);
                    section(ui, "EQ bands", |ui| self.bands_section(ui, s));
                    if s.channels >= 2 {
                        ui.add_space(GAP);
                        section(ui, "Channels", |ui| self.channels_section(ui, s));
                    }
                }
                ui.add_space(GAP);
                section_hint(ui, "Device → Profile", "auto-switch", |ui| {
                    self.device_mapping_section(ui);
                });
                ui.add_space(GAP);
                section(ui, "Profiles", |ui| self.profiles_panel(ui));
            });
    }
}
