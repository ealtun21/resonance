//! Central-area layout: the disconnected start screen and the responsive shell
//! (FR graph + spectrum, with a width-driven 3-column / single-column accordion
//! lower area).

use crate::app::{GuiApp, ServiceAction, ServiceFn};
use crate::card_layout::{CardCol, CardId, CardLayout};
use crate::panes::{BandsOffLayout, PaneAction, PaneId};
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
                // Drop the whole controls strip when every lower pane is hidden
                // (and we're not arranging), so the hero graph fills the window.
                if self.layout_edit || self.lower_has_content() {
                    let controls_h = (ui.available_height() * 0.4).max(150.0);
                    egui::Panel::bottom("controls_panel")
                        .resizable(true)
                        .default_size(controls_h)
                        .min_size(80.0)
                        .show_separator_line(false)
                        .show_inside(ui, |ui| self.lower_columns(ui, state.as_ref()));
                }
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
                // Apply any arrange-mode action queued this frame — from the
                // controls strip (tray / columns / bands) or the hero's reference
                // row — now that both panels have rendered.
                if let Some(action) = self.pending_pane_action.take() {
                    self.apply_pane_action(action);
                }
            }
            // Narrow: graph on top (resizable, with a floor so it stays usable),
            // the accordion of sections scrolls in the central area below — open
            // sections fill it; the splitter trades graph height for controls.
            LayoutMode::Narrow => {
                let ref_visible = self.pane_visible(PaneId::ReferenceBar);
                if self.lower_has_content() {
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
                    if ref_visible {
                        egui::Panel::top("reference_bar_narrow")
                            .resizable(false)
                            .show_separator_line(false)
                            .show_inside(ui, |ui| self.reference_bar(ui));
                    }
                    egui::CentralPanel::default().show_inside(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("controls_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.accordion_stack(ui, state.as_ref()));
                    });
                } else {
                    // Nothing below the graph: let it fill, with the reference bar
                    // (if shown) pinned under it.
                    if ref_visible {
                        egui::Panel::bottom("reference_bar_narrow")
                            .resizable(false)
                            .show_separator_line(false)
                            .show_inside(ui, |ui| self.reference_bar(ui));
                    }
                    egui::CentralPanel::default().show_inside(ui, |ui| {
                        if let Some(s) = &state {
                            self.eq_curve(ui, s);
                        }
                    });
                }
            }
        }
    }

    /// Render one control card by id, wrapped in its section frame. Returns true
    /// if it drew anything — absent Applications/Outputs cards draw nothing and
    /// return false, so the caller can skip their inter-card spacing.
    fn render_card(&mut self, ui: &mut egui::Ui, s: &DaemonState, id: CardId) -> bool {
        if !self.pane_visible(PaneId::from_card(id)) {
            return false;
        }
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
    /// mode draws the live cards (skipping absent ones); edit mode draws compact
    /// draggable tiles with drop zones. Both honour the hidden-panes set, so the
    /// arranger shows only the cards the live layout will show.
    fn render_lower_column(&mut self, ui: &mut egui::Ui, s: &DaemonState, col: CardCol) {
        let ids = self.layout.column(col).to_vec();
        if self.layout_edit {
            // Only show the drop gaps once a drag is in flight, so an idle edit
            // mode stays uncluttered. Hidden cards are omitted (WYSIWYG); drop
            // indices stay ABSOLUTE (into the full column) so `move_card` places
            // dropped cards correctly even with hidden cards interleaved.
            let dragging = egui::DragAndDrop::has_payload_of_type::<PaneId>(ui.ctx());
            let mut shown = 0;
            for (idx, id) in ids.iter().enumerate() {
                if !self.pane_visible(PaneId::from_card(*id)) {
                    continue;
                }
                if dragging {
                    self.drop_gap(ui, col, idx);
                }
                self.card_tile(ui, *id);
                ui.add_space(6.0);
                shown += 1;
            }
            if dragging {
                self.drop_gap(ui, col, ids.len());
            } else if shown == 0 {
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

    /// A compact arrange-mode tile for a card: the grip + name is the drag source
    /// (carrying its `PaneId`, so it can be dropped into a column or the Hidden
    /// tray), and a trailing × removes it to the tray.
    fn card_tile(&mut self, ui: &mut egui::Ui, id: CardId) {
        let t = kit::tokens(ui);
        egui::Frame::default()
            .fill(ui.visuals().faint_bg_color)
            .stroke(egui::Stroke::new(1.0, t.line))
            .corner_radius(egui::CornerRadius::same(kit::R_CARD as u8))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_width(ui.available_width());
                    ui.dnd_drag_source(
                        egui::Id::new(("card_tile", id)),
                        PaneId::from_card(id),
                        |ui| {
                            let (r, _) = ui
                                .allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                            crate::ui::icons::draw(
                                ui.painter(),
                                crate::ui::icons::Icon::Grip,
                                r,
                                t.dim,
                            );
                            ui.add_space(6.0);
                            ui.label(id.title());
                        },
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if kit::icon_btn(
                            ui,
                            crate::ui::icons::Icon::Close,
                            kit::CTRL_H,
                            "Hide this pane",
                        ) {
                            self.pending_pane_action =
                                Some(PaneAction::Hide(PaneId::from_card(id)));
                        }
                    });
                });
            });
    }

    /// A tile in the Hidden tray: the grip + name is a drag source (drop it into
    /// a column / centre / reference row to restore), and a trailing ＋ restores
    /// it to its home.
    fn tray_tile(&mut self, ui: &mut egui::Ui, pane: PaneId) {
        let t = kit::tokens(ui);
        egui::Frame::default()
            .fill(ui.visuals().faint_bg_color)
            .stroke(egui::Stroke::new(1.0, t.line))
            .corner_radius(egui::CornerRadius::same(kit::R_CARD as u8))
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.dnd_drag_source(egui::Id::new(("tray_tile", pane)), pane, |ui| {
                        let (r, _) =
                            ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                        crate::ui::icons::draw(
                            ui.painter(),
                            crate::ui::icons::Icon::Grip,
                            r,
                            t.dim,
                        );
                        ui.add_space(6.0);
                        ui.label(pane.title());
                    });
                    if kit::icon_btn(
                        ui,
                        crate::ui::icons::Icon::Plus,
                        kit::CTRL_H,
                        "Show this pane",
                    ) {
                        self.pending_pane_action = Some(PaneAction::Show(pane));
                    }
                });
            });
    }

    /// The Hidden tray: a labelled strip of every hidden pane, and itself a drop
    /// zone — drop a pane here to remove it.
    fn hidden_tray(&mut self, ui: &mut egui::Ui) {
        let hidden: Vec<PaneId> = PaneId::ALL
            .iter()
            .copied()
            .filter(|p| self.hidden_panes.contains(p))
            .collect();
        let frame = egui::Frame::default().inner_margin(egui::Margin::symmetric(8, 6));
        let (_, payload) = ui.dnd_drop_zone::<PaneId, _>(frame, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("Hidden:").strong());
                if hidden.is_empty() {
                    ui.weak("nothing hidden — drag a pane here to remove it");
                }
                for pane in hidden {
                    self.tray_tile(ui, pane);
                }
            });
        });
        if let Some(p) = payload {
            self.pending_pane_action = Some(PaneAction::Hide(*p));
        }
    }

    /// A thin full-width drop target between/around card tiles in edit mode. When
    /// a card is released over it, records the pending move to `(col, idx)`.
    fn drop_gap(&mut self, ui: &mut egui::Ui, col: CardCol, idx: usize) {
        let frame = egui::Frame::default().inner_margin(egui::Margin::symmetric(0, 3));
        let (_, payload) = ui.dnd_drop_zone::<PaneId, _>(frame, |ui| {
            ui.allocate_exact_size(
                egui::vec2(ui.available_width().max(1.0), 8.0),
                egui::Sense::hover(),
            );
        });
        if let Some(p) = payload {
            // Only cards live in columns; a bands/reference payload here is a no-op.
            if let Some(card) = p.card() {
                self.pending_pane_action = Some(PaneAction::PlaceCard { card, col, idx });
            }
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
                    ui.label(
                        "Arranging layout — drag panes to Hidden to remove; drag back (or +) to add.",
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Done").clicked() {
                            self.layout_edit = false;
                        }
                        if ui.button("Reset").clicked() {
                            self.layout = CardLayout::default();
                            self.hidden_panes.clear();
                            self.bands_off_layout = BandsOffLayout::default();
                        }
                        ui.separator();
                        // Bands-off layout preference (applies to the live view).
                        // Right-to-left layout: add the combo first so the "EQ
                        // bands off:" label sits to its left.
                        let mut pref = self.bands_off_layout;
                        egui::ComboBox::from_id_salt("bands_off_layout")
                            .selected_text(match pref {
                                BandsOffLayout::Columns => "Columns",
                                BandsOffLayout::Stacked => "Stack",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut pref, BandsOffLayout::Columns, "Columns");
                                ui.selectable_value(&mut pref, BandsOffLayout::Stacked, "Stack");
                            });
                        self.bands_off_layout = pref;
                        ui.label("EQ bands off:");
                    });
                });
            });
    }

    /// Visible cards in a column, honouring the hidden-panes set. Used to decide
    /// whether a side panel has anything to show (so an all-hidden column drops
    /// its panel entirely) and to drive the stacked fallback when bands is hidden.
    fn visible_cards(&self, col: CardCol) -> Vec<CardId> {
        self.layout
            .column(col)
            .iter()
            .copied()
            .filter(|&id| self.pane_visible(PaneId::from_card(id)))
            .collect()
    }

    /// True when at least one lower pane (any control card or the EQ bands) is
    /// visible. When false the wide layout drops the whole controls strip and the
    /// narrow layout lets the graph fill, so the FR graph is the entire UI.
    fn lower_has_content(&self) -> bool {
        self.pane_visible(PaneId::Bands)
            || CardId::ALL
                .iter()
                .any(|&id| self.pane_visible(PaneId::from_card(id)))
    }

    /// Wide layout controls strip. Arrange (edit) mode and live mode diverge, so
    /// each has its own renderer; both honour the hidden-panes set.
    fn lower_columns(&mut self, ui: &mut egui::Ui, state: Option<&DaemonState>) {
        if self.layout_edit {
            self.lower_columns_arrange(ui, state);
        } else {
            self.lower_columns_live(ui, state);
        }
    }

    /// Arrange (edit-layout) mode: the fixed 3-column arranger with draggable
    /// tiles and drop zones. Respects the hidden-panes set — hidden cards are
    /// omitted from the columns and a hidden EQ-bands centre shows a hint instead
    /// of the table — so the arranger matches what the live layout will show.
    /// Both side columns always render so an empty column stays a drop target;
    /// the banner's Reset unhides every pane, so hiding everything is never a
    /// dead-end.
    fn lower_columns_arrange(&mut self, ui: &mut egui::Ui, state: Option<&DaemonState>) {
        egui::Panel::top("layout_edit_banner")
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .show_inside(ui, |ui| self.layout_edit_banner(ui));
        // Hidden tray pinned to the bottom of the controls strip (under the columns).
        egui::Panel::bottom("hidden_tray")
            .frame(egui::Frame::NONE)
            .show_separator_line(true)
            .show_inside(ui, |ui| self.hidden_tray(ui));
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
        let _ = state; // arrange centre shows a compact bands tile, not the table
        egui::CentralPanel::default()
            .frame(bands_card_frame(ui))
            .show_inside(ui, |ui| {
                if self.pane_visible(PaneId::Bands) {
                    // Compact draggable "EQ bands" tile with an × to remove.
                    ui.horizontal(|ui| {
                        ui.set_width(ui.available_width());
                        let t = kit::tokens(ui);
                        ui.dnd_drag_source(egui::Id::new("bands_tile"), PaneId::Bands, |ui| {
                            let (r, _) = ui
                                .allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                            crate::ui::icons::draw(
                                ui.painter(),
                                crate::ui::icons::Icon::Grip,
                                r,
                                t.dim,
                            );
                            ui.add_space(6.0);
                            ui.label(PaneId::Bands.title());
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if kit::icon_btn(
                                ui,
                                crate::ui::icons::Icon::Close,
                                kit::CTRL_H,
                                "Hide EQ bands",
                            ) {
                                self.pending_pane_action = Some(PaneAction::Hide(PaneId::Bands));
                            }
                        });
                    });
                } else {
                    // Drop zone filling the centre to restore the bands table.
                    let (_, payload) = ui.dnd_drop_zone::<PaneId, _>(egui::Frame::NONE, |ui| {
                        ui.set_min_size(ui.available_size());
                        ui.centered_and_justified(|ui| {
                            ui.weak("drop EQ bands here to show it");
                        });
                    });
                    if let Some(p) = payload {
                        if *p == PaneId::Bands {
                            self.pending_pane_action = Some(PaneAction::Show(PaneId::Bands));
                        }
                    }
                }
            });
        // The pending action set here (or in `hero`'s reference row) is applied at
        // the end of `shell`'s wide branch, after the hero renders.
    }

    /// Live (non-edit) layout: three columns — Effects | EQ bands (flexible
    /// centre) | Devices/Profiles — that FILL the width like a native desktop
    /// app's panes. A side panel drops when the user has hidden every card in it;
    /// when EQ bands is hidden the remaining visible cards stack full-width
    /// instead of the 3-column split.
    fn lower_columns_live(&mut self, ui: &mut egui::Ui, state: Option<&DaemonState>) {
        let left_cards = self.visible_cards(CardCol::Left);
        let right_cards = self.visible_cards(CardCol::Right);

        if self.pane_visible(PaneId::Bands) {
            // Frame::NONE on every column so they share one top inset (the panels'
            // default frames differ — that's why EQ bands sat lower than its
            // neighbours) and no separator lines, so the cards float on the body
            // background with plain gaps between them (mockup `.controls`), instead
            // of egui's panel-boundary grid lines.
            if !left_cards.is_empty() {
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
            }
            if !right_cards.is_empty() {
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
            }
            // The centre column IS the bands card: a card-styled CentralPanel (so
            // it fills whatever the side panels leave — full width if both are
            // gone). Its body (head/table/footer nested panels) lives in
            // `bands_card`.
            egui::CentralPanel::default()
                .frame(bands_card_frame(ui))
                .show_inside(ui, |ui| {
                    if let Some(s) = state {
                        self.bands_card(ui, s);
                    }
                });
        } else {
            // Bands hidden: layout per the user's preference.
            let Some(s) = state else { return };
            match self.bands_off_layout {
                // Stacked: all visible cards in one full-width scrolled column.
                BandsOffLayout::Stacked => {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show_inside(ui, |ui| {
                            padded_scroll(ui, "stacked_cards", |ui| {
                                for id in left_cards.iter().chain(&right_cards) {
                                    if self.render_card(ui, s, *id) {
                                        ui.add_space(12.0);
                                    }
                                }
                            });
                        });
                }
                // Columns: two equal side-by-side columns. If only one side has
                // visible cards it fills the width; both populated → 50/50.
                BandsOffLayout::Columns => {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show_inside(ui, |ui| {
                            match (left_cards.is_empty(), right_cards.is_empty()) {
                                (false, false) => {
                                    ui.columns(2, |cols| {
                                        padded_scroll(&mut cols[0], "dual_left", |ui| {
                                            for id in &left_cards {
                                                if self.render_card(ui, s, *id) {
                                                    ui.add_space(12.0);
                                                }
                                            }
                                        });
                                        padded_scroll(&mut cols[1], "dual_right", |ui| {
                                            for id in &right_cards {
                                                if self.render_card(ui, s, *id) {
                                                    ui.add_space(12.0);
                                                }
                                            }
                                        });
                                    });
                                }
                                (false, true) => {
                                    padded_scroll(ui, "dual_left", |ui| {
                                        for id in &left_cards {
                                            if self.render_card(ui, s, *id) {
                                                ui.add_space(12.0);
                                            }
                                        }
                                    });
                                }
                                (true, false) => {
                                    padded_scroll(ui, "dual_right", |ui| {
                                        for id in &right_cards {
                                            if self.render_card(ui, s, *id) {
                                                ui.add_space(12.0);
                                            }
                                        }
                                    });
                                }
                                // Both empty + bands hidden ⇒ lower_has_content() is
                                // false, so this branch isn't reached (graph fills).
                                (true, true) => {}
                            }
                        });
                }
            }
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
                    if self.pane_visible(PaneId::Bands) {
                        section(ui, "EQ bands", |ui| self.bands_section(ui, s));
                        ui.add_space(GAP);
                    }
                    for id in self.layout.right.clone() {
                        if self.render_card(ui, s, id) {
                            ui.add_space(GAP);
                        }
                    }
                }
            });
    }
}

/// The card frame for the centre EQ-bands panel (and its hidden-in-arrange
/// placeholder), shared by the arrange and live renderers so the two can't drift.
fn bands_card_frame(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::default()
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(1.0, kit::tokens(ui).line))
        .corner_radius(egui::CornerRadius::same(kit::R_CARD as u8))
        .outer_margin(egui::Margin::symmetric(8, 10))
}
