//! Central-area layout: the disconnected start screen and the responsive shell
//! (FR graph + spectrum, with a width-driven 3-column / single-column accordion
//! lower area).

use crate::app::{GuiApp, ServiceAction, ServiceFn};
use crate::card_layout::{CardCol, CardId, CardLayout};
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

    /// Render one control card by id, wrapped in its section frame. Returns true
    /// if it drew anything — absent Applications/Outputs cards draw nothing and
    /// return false, so the caller can skip their inter-card spacing.
    fn render_card(&mut self, ui: &mut egui::Ui, s: &DaemonState, id: CardId) -> bool {
        match id {
            CardId::Effects => {
                section_hint(ui, "Effects", "DSP sound effects", |ui| {
                    self.effects_section(ui, s);
                });
                // Output stage (dither) rides under Effects when enabled.
                if self.show_dither {
                    ui.add_space(12.0);
                    section_hint(ui, "Output", "dither", |ui| {
                        self.output_section(ui, s);
                    });
                }
                // Convolution (impulse response) likewise rides under Effects.
                if self.show_ir {
                    ui.add_space(12.0);
                    section_hint(ui, "Convolution", "impulse response", |ui| {
                        self.convolution_section(ui, s);
                    });
                }
                true
            }
            CardId::Applications => {
                if s.apps.is_empty() {
                    return false;
                }
                section_hint(ui, "Applications", "per-app volume", |ui| {
                    self.apps_section(ui, s);
                });
                true
            }
            CardId::Outputs => {
                if s.sinks.is_empty() {
                    return false;
                }
                section_hint(ui, "Outputs", "device volume", |ui| {
                    self.sinks_section(ui, s);
                });
                true
            }
            CardId::DeviceMap => {
                section_hint(ui, "Device → Profile", "auto-switch", |ui| {
                    self.device_mapping_section(ui);
                });
                true
            }
            CardId::Profiles => {
                let saved = self.profiles_saved_hint();
                section_hint(ui, "Profiles", &saved, |ui| self.profiles_panel(ui));
                true
            }
        }
    }

    /// Render one wide-layout side column from the persisted card order. Normal
    /// mode draws the live cards (skipping absent ones); edit mode (Task 5) draws
    /// compact draggable tiles with drop zones.
    fn render_lower_column(&mut self, ui: &mut egui::Ui, s: &DaemonState, col: CardCol) {
        let ids = self.layout.column(col).to_vec();
        if self.layout_edit {
            // Only show the drop gaps once a drag is in flight, so an idle edit
            // mode stays uncluttered.
            let dragging = egui::DragAndDrop::has_payload_of_type::<CardId>(ui.ctx());
            for (idx, id) in ids.iter().enumerate() {
                if dragging {
                    self.drop_gap(ui, col, idx);
                }
                self.card_tile(ui, *id);
                ui.add_space(6.0);
            }
            if dragging {
                self.drop_gap(ui, col, ids.len());
            } else if ids.is_empty() {
                ui.weak("(empty — drag a card here)");
            }
        } else {
            for id in &ids {
                if self.render_card(ui, s, *id) {
                    ui.add_space(12.0);
                }
            }
        }
    }

    /// A compact draggable tile (grip + card name) shown in edit mode. The whole
    /// tile is the drag source carrying the card's `CardId`.
    // `&mut self` matches its sibling edit-mode helpers (`drop_gap`,
    // `layout_edit_banner`) that DO mutate state; kept as a method for a
    // consistent `self.card_tile(...)` call shape at the render_lower_column
    // call site rather than a one-off associated function.
    #[allow(clippy::unused_self)]
    fn card_tile(&mut self, ui: &mut egui::Ui, id: CardId) {
        ui.dnd_drag_source(egui::Id::new(("card_tile", id)), id, |ui| {
            let t = kit::tokens(ui);
            egui::Frame::default()
                .fill(ui.visuals().faint_bg_color)
                .stroke(egui::Stroke::new(1.0, t.line))
                .corner_radius(egui::CornerRadius::same(kit::R_CARD as u8))
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.set_width(ui.available_width());
                        let (r, _) =
                            ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                        crate::ui::icons::draw(
                            ui.painter(),
                            crate::ui::icons::Icon::Grip,
                            r,
                            t.dim,
                        );
                        ui.add_space(6.0);
                        ui.label(id.title());
                    });
                });
        });
    }

    /// A thin full-width drop target between/around card tiles in edit mode. When
    /// a card is released over it, records the pending move to `(col, idx)`.
    fn drop_gap(&mut self, ui: &mut egui::Ui, col: CardCol, idx: usize) {
        let frame = egui::Frame::default().inner_margin(egui::Margin::symmetric(0, 3));
        let (_, payload) = ui.dnd_drop_zone::<CardId, _>(frame, |ui| {
            ui.allocate_exact_size(
                egui::vec2(ui.available_width().max(1.0), 8.0),
                egui::Sense::hover(),
            );
        });
        if let Some(p) = payload {
            self.pending_card_move = Some((*p, col, idx));
        }
    }

    /// The banner shown across the top of the controls strip while arranging.
    fn layout_edit_banner(&mut self, ui: &mut egui::Ui) {
        let t = kit::tokens(ui);
        egui::Frame::default()
            .fill(t.accent.gamma_multiply(0.18))
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Arranging layout — drag cards between the side columns.");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Done").clicked() {
                            self.layout_edit = false;
                        }
                        if ui.button("Reset").clicked() {
                            self.layout = CardLayout::default();
                        }
                    });
                });
            });
    }

    /// Wide layout: three columns — Effects | EQ bands (flexible centre) |
    /// Devices/Profiles — that FILL the width like a native desktop app's panes
    /// (thin splitter rules between them). EQ bands takes all the slack so its
    /// table grows into the space rather than leaving a centred island.
    fn lower_columns(&mut self, ui: &mut egui::Ui, state: Option<&DaemonState>) {
        // Edit-mode banner spans the top of the controls strip.
        if self.layout_edit {
            egui::Panel::top("layout_edit_banner")
                .frame(egui::Frame::NONE)
                .show_separator_line(false)
                .show_inside(ui, |ui| self.layout_edit_banner(ui));
        }
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
                        self.render_lower_column(ui, s, CardCol::Left);
                    });
                }
            });
        egui::Panel::right("devices_col")
            .resizable(false)
            .exact_size(DEVICES_W)
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .show_inside(ui, |ui| {
                if let Some(s) = state {
                    padded_scroll(ui, "side", |ui| {
                        self.render_lower_column(ui, s, CardCol::Right);
                    });
                }
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
        // Apply a card move requested by a drop this frame, now that both columns
        // have finished rendering (never mutate the lists mid-iteration).
        if let Some((id, col, idx)) = self.pending_card_move.take() {
            self.layout.move_card(id, col, idx);
        }
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
                    // Left-column cards, then the fixed EQ Bands anchor, then the
                    // right-column cards — reflecting the wide-layout arrangement.
                    for id in self.layout.left.clone() {
                        if self.render_card(ui, s, id) {
                            ui.add_space(GAP);
                        }
                    }
                    section(ui, "EQ bands", |ui| self.bands_section(ui, s));
                    ui.add_space(GAP);
                    for id in self.layout.right.clone() {
                        if self.render_card(ui, s, id) {
                            ui.add_space(GAP);
                        }
                    }
                }
            });
    }
}
