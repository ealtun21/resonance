//! EQ bands table: per-band type/freq/gain/Q editors drawn with the bespoke kit
//! (custom toggle, dropdown, drag-or-type number fields, icon buttons). Manual
//! rows with fixed columns + a flexible Graph column, so the table fills its pane
//! at any width and the columns stay aligned. Columns collapse as it narrows.

use crate::app::GuiApp;
use crate::state::{BAND_TYPES, GAIN_LIMIT, Q_LIMIT};
use crate::ui::curve_view::channel_color;
use crate::ui::icons::Icon;
use crate::ui::kit;
use crate::ui::widgets::{freq_color, gain_bar, gain_color};
use eframe::egui;
use resonance_ipc::{BandType, ChannelMask, Command, DaemonState};

const IDX_W: f32 = 26.0;
const ON_W: f32 = 36.0;
const FREQ_W: f32 = 58.0;
const GAIN_W: f32 = 54.0;
const Q_W: f32 = 50.0;
const CH_W: f32 = 64.0;
const X_W: f32 = 24.0;

/// Short label for a band's channel target, e.g. `all` / `FL` / `FL FR` /
/// `FL +2` / `none`. Used in the per-band channel column (multichannel only).
pub(crate) fn channel_tag(mask: ChannelMask, layout: &[String], channels: usize) -> String {
    if mask.is_global(channels) {
        return "all".to_string();
    }
    let names: Vec<&str> = (0..channels)
        .filter(|&c| mask.contains(c))
        .map(|c| layout.get(c).map(String::as_str).unwrap_or("?"))
        .collect();
    match names.len() {
        0 => "none".to_string(),
        1 | 2 => names.join(" "),
        _ => format!("{} +{}", names[0], names.len() - 1),
    }
}

impl GuiApp {
    /// The EQ bands card body for the wide layout (the caller frames it as a card
    /// that fills the column). Structured as nested panels — head on top, footer
    /// pinned at the bottom, the table scrolling in the centre — so it fills the
    /// column height *without* reading `available_height` (which would feed back
    /// into the resizable controls panel and let the table eat the graph). The
    /// full-height fill puts the accent "Add band" footer flush with the bottom,
    /// aligned with the neighbour columns (mockup `.bandscard`).
    pub(crate) fn bands_card(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        let off = state.bands.iter().filter(|b| !b.enabled).count();
        let hint = if off > 0 {
            format!("{} bands · {} off", state.bands.len(), off)
        } else {
            format!("{} bands", state.bands.len())
        };
        let t = kit::tokens(ui);

        // Head bar (caption + hint over a full-width rule).
        egui::Panel::top("bands_head")
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .show_inside(ui, |ui| {
                let full_w = ui.available_width();
                let (head, _) = ui.allocate_exact_size(
                    egui::vec2(full_w, kit::CARD_HEAD_H),
                    egui::Sense::hover(),
                );
                kit::caption(
                    ui.painter(),
                    egui::pos2(head.left() + kit::CARD_PAD_X, head.center().y),
                    "EQ Bands",
                    t.dim,
                );
                ui.painter().text(
                    egui::pos2(head.right() - kit::CARD_PAD_X, head.center().y),
                    egui::Align2::RIGHT_CENTER,
                    &hint,
                    egui::FontId::proportional(kit::T_CAPTION),
                    t.faint,
                );
                ui.painter().hline(
                    head.x_range(),
                    head.bottom() - 0.5,
                    egui::Stroke::new(1.0, t.line),
                );
            });

        // Footer pinned at the bottom.
        egui::Panel::bottom("bands_foot")
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .show_inside(ui, |ui| self.bands_footer(ui));

        // Table fills the centre and scrolls when it overflows.
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("bands_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Frame::default()
                            .inner_margin(egui::Margin {
                                left: 0,
                                right: 0,
                                top: kit::CARD_PAD_Y as i8,
                                bottom: kit::CARD_PAD_Y as i8,
                            })
                            .show(ui, |ui| self.bands_section(ui, state));
                    });
            });
    }

    pub(crate) fn bands_section(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        // The card body is full-bleed (no horizontal padding) so row rules and the
        // selection wash run to the card edge; cells get an 8px gutter of their own
        // (mockup table td padding) while the tint spans the full width.
        const GUTTER: f32 = 8.0;
        let full_w = ui.available_width();
        let avail = (full_w - GUTTER * 2.0).max(60.0);
        // Per-channel EQ: the channel-target column appears on >2-channel
        // devices automatically (progressive disclosure), or when a ≥2ch user
        // opts in via the Channels section's "Per-channel EQ" toggle (lets a
        // stereo user do L/R-specific EQ).
        let show_ch = state.channels > 2 || (self.per_channel_eq && state.channels >= 2);
        // Collapse columns as the table narrows: drop the gain graph first, then
        // the Type combo (abbreviated when tight).
        let show_graph = avail >= 480.0;
        let show_type = avail >= 360.0;
        // Always the short coloured type badge (PK/LS/HS…) — compact + scannable;
        // the full name lives in its dropdown menu.
        let type_w = 50.0;
        let gap = kit::SP_S;
        let n_cols = 6 + show_type as usize + show_graph as usize + show_ch as usize;
        let fixed = IDX_W
            + ON_W
            + if show_type { type_w } else { 0.0 }
            + FREQ_W
            + GAIN_W
            + Q_W
            + if show_ch { CH_W } else { 0.0 }
            + X_W;
        let graph_w = (avail - fixed - gap * (n_cols as f32 - 1.0)).max(60.0);

        // Header captions, aligned to the same column widths as the rows.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            ui.add_space(GUTTER);
            let dim = kit::tokens(ui).dim;
            let cap = |ui: &mut egui::Ui, w: f32, s: &str| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(w, 16.0), egui::Sense::hover());
                ui.painter().text(
                    egui::pos2(r.left(), r.center().y),
                    egui::Align2::LEFT_CENTER,
                    s,
                    egui::FontId::proportional(kit::T_CAPTION),
                    dim,
                );
            };
            cap(ui, IDX_W, "#");
            cap(ui, ON_W, "On");
            if show_type {
                cap(ui, type_w, "Type");
            }
            cap(ui, FREQ_W, "Freq");
            cap(ui, GAIN_W, "Gain");
            cap(ui, Q_W, "Q");
            if show_ch {
                cap(ui, CH_W, "Ch");
            }
            if show_graph {
                cap(ui, graph_w, "Graph");
            }
        });

        let nbands = state.bands.len();
        for (i, b) in state.bands.iter().enumerate() {
            // Selected row: a faint accent wash + a 2px accent bar down its left
            // edge (mockup `tr.sel`); every row but the last gets a hairline rule
            // under it so the table reads as ruled rows, not floating text.
            let row_selected = i == self.selected_band;
            let tint = if row_selected {
                kit::tokens(ui).accent.gamma_multiply(0.10)
            } else {
                egui::Color32::TRANSPARENT
            };
            // Each row gets a stable id namespace keyed by band index. Without it,
            // adding/removing the per-channel "Ch" column reflows the row and egui's
            // positional auto-ids momentarily collide → it flags the shifted widgets
            // (the ✕ buttons) with a red ID-clash border for a frame.
            let row = ui
                .push_id(i, |ui| {
                    egui::Frame::default()
                        .fill(tint)
                        .inner_margin(egui::Margin {
                            left: 0,
                            right: 0,
                            top: 2,
                            bottom: 2,
                        })
                        .show(ui, |ui| {
                            // Span the full card width so the wash/rule reach both edges;
                            // the cell content is inset by the gutter.
                            ui.set_min_width(full_w);
                            ui.horizontal(|ui| {
                                ui.set_min_height(26.0);
                                ui.spacing_mut().item_spacing.x = gap;
                                ui.add_space(GUTTER);
                                let t = kit::tokens(ui);

                                // Index chip doubles as the row selector.
                                let selected = i == self.selected_band;
                                let (r, rr) = ui.allocate_exact_size(
                                    egui::vec2(IDX_W, 22.0),
                                    egui::Sense::click(),
                                );
                                if selected {
                                    ui.painter().rect_filled(r, 4.0, t.accent);
                                }
                                ui.painter().text(
                                    r.center(),
                                    egui::Align2::CENTER_CENTER,
                                    format!("{}", i + 1),
                                    egui::FontId::monospace(kit::T_VALUE),
                                    if selected {
                                        egui::Color32::WHITE
                                    } else {
                                        t.dim
                                    },
                                );
                                if rr.clicked() {
                                    self.selected_band = i;
                                }

                                let mut on = b.enabled;
                                if kit::toggle(ui, &mut on) {
                                    self.queue_edit(Command::SetBandEnabled {
                                        index: i,
                                        enabled: on,
                                    });
                                }

                                if show_type {
                                    // Coloured abbreviated badge (PK/LS/…); the menu lists full names.
                                    let labels: Vec<&str> =
                                        BAND_TYPES.iter().map(|bt| bt.full()).collect();
                                    if let Some(sel) = kit::tag_dropdown(
                                        ui,
                                        type_w,
                                        22.0,
                                        egui::Id::new(("bt", i)),
                                        b.band_type.abbrev(),
                                        t.accent,
                                        &labels,
                                    ) {
                                        self.queue_edit(Command::SetBandType {
                                            index: i,
                                            band_type: BAND_TYPES[sel],
                                        });
                                    }
                                }

                                let mut freq = b.freq;
                                let mut gain = b.gain_db;
                                let mut q = b.q;
                                // Tint the freq value across the visible-light spectrum so
                                // low/high bands read at a glance (red bass → violet treble).
                                let fcol = freq_color(freq);
                                let fc = kit::num_field_colored(
                                    ui,
                                    FREQ_W,
                                    egui::Id::new(("f", i)),
                                    &mut freq,
                                    20.0..=20000.0,
                                    0,
                                    2.0,
                                    fcol,
                                );
                                let gcol = gain_color(gain, &self.palette);
                                let gc = kit::num_field_colored(
                                    ui,
                                    GAIN_W,
                                    egui::Id::new(("g", i)),
                                    &mut gain,
                                    -GAIN_LIMIT..=GAIN_LIMIT,
                                    1,
                                    0.1,
                                    gcol,
                                );
                                let qc = kit::num_field(
                                    ui,
                                    Q_W,
                                    egui::Id::new(("q", i)),
                                    &mut q,
                                    0.1..=Q_LIMIT,
                                    2,
                                    0.02,
                                );
                                if fc || gc || qc {
                                    self.queue_edit(Command::SetBand {
                                        index: i,
                                        freq,
                                        gain_db: gain,
                                        q,
                                    });
                                }

                                if show_ch {
                                    // Channel-target chip: coloured by its target (neutral "all",
                                    // else the first targeted channel's curve colour) + a checkbox
                                    // popup. Edits collected after the closure so it never borrows
                                    // `self`.
                                    let mut new_mask: Option<ChannelMask> = None;
                                    let tag = channel_tag(
                                        b.channels,
                                        &state.channel_layout,
                                        state.channels,
                                    );
                                    let col = if b.channels.is_global(state.channels) {
                                        t.dim
                                    } else {
                                        (0..state.channels)
                                            .find(|&c| b.channels.contains(c))
                                            .map(channel_color)
                                            .unwrap_or(t.dim)
                                    };
                                    let resp = kit::tag_chip(ui, CH_W, 22.0, &tag, col);
                                    egui::Popup::menu(&resp)
                                        .id(egui::Id::new(("ch", i)))
                                        .close_behavior(
                                            egui::PopupCloseBehavior::CloseOnClickOutside,
                                        )
                                        .show(|ui| {
                                            let mut mask = b.channels;
                                            for c in 0..state.channels {
                                                let label = state
                                                    .channel_layout
                                                    .get(c)
                                                    .cloned()
                                                    .unwrap_or_else(|| format!("ch{c}"));
                                                let mut on = mask.contains(c);
                                                if kit::checkbox(ui, &mut on, &label) {
                                                    mask = if on {
                                                        mask.with(c)
                                                    } else {
                                                        mask.without(c)
                                                    };
                                                    new_mask = Some(mask);
                                                }
                                            }
                                        });
                                    if let Some(m) = new_mask {
                                        // Collapse "every channel" back to the canonical ALL.
                                        let m = if m.is_global(state.channels) {
                                            ChannelMask::ALL
                                        } else {
                                            m
                                        };
                                        self.queue_edit(Command::SetBandChannels {
                                            index: i,
                                            channels: m,
                                        });
                                    }
                                }

                                if show_graph {
                                    gain_bar(ui, graph_w, b.gain_db, &self.palette);
                                }

                                if kit::icon_btn(ui, Icon::Close, 24.0, "Remove this band") {
                                    self.queue_edit(Command::RemoveBand { index: i });
                                    // Keep the lock pins on the same band after the list shifts.
                                    remap_pin_on_remove(&mut self.vlock, i);
                                    remap_pin_on_remove(&mut self.hlock, i);
                                }
                            });
                        })
                })
                .inner;
            let rr = row.response.rect;
            let tk = kit::tokens(ui);
            if i + 1 < nbands {
                ui.painter()
                    .hline(rr.x_range(), rr.bottom(), egui::Stroke::new(1.0, tk.line));
            }
            if row_selected {
                let bar = egui::Rect::from_min_max(
                    egui::pos2(rr.left(), rr.top() + 1.0),
                    egui::pos2(rr.left() + 2.0, rr.bottom() - 1.0),
                );
                ui.painter().rect_filled(bar, 0.0, tk.accent);
            }
        }

        if self.selected_band >= state.bands.len() {
            self.selected_band = state.bands.len().saturating_sub(1);
        }
    }

    /// The bands card footer (mockup `.bandsfoot`): a top rule, then an accent
    /// "Add band" button + a hint, pinned at the card bottom. (No "Flatten" — it
    /// too easily wipes a careful EQ by accident.)
    pub(crate) fn bands_footer(&mut self, ui: &mut egui::Ui) {
        let t = kit::tokens(ui);
        // Full-bleed top rule, then the button row in a padded frame so the accent
        // "Add band" sits 12px in from the card edge and is vertically centred.
        let (line_r, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
        ui.painter().hline(
            line_r.x_range(),
            line_r.top(),
            egui::Stroke::new(1.0, t.line),
        );
        egui::Frame::default()
            .inner_margin(egui::Margin::symmetric(
                kit::CARD_PAD_X as i8,
                kit::SP_S as i8,
            ))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_min_height(kit::CTRL_H);
                    ui.spacing_mut().item_spacing.x = kit::SP_S;
                    if kit::icon_text_btn(
                        ui,
                        Icon::Plus,
                        "Add band",
                        true,
                        true,
                        "Add a new peaking band",
                    ) {
                        self.queue_edit(Command::AddBand {
                            band_type: BandType::Peaking,
                            freq: 1000.0,
                            gain_db: 0.0,
                            q: 1.4,
                        });
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new("double-click the graph to add a band")
                                .size(kit::T_CAPTION)
                                .color(t.faint),
                        );
                    });
                });
            });
    }
}

/// Adjust a band-index lock pin after the band at `removed` is deleted: drop the
/// pin if it was that band, decrement it if it sat above the removed index.
pub(crate) fn remap_pin_on_remove(pin: &mut Option<usize>, removed: usize) {
    match *pin {
        Some(i) if i == removed => *pin = None,
        Some(i) if i > removed => *pin = Some(i - 1),
        _ => {}
    }
}
