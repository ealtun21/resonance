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
use resonance_ipc::{BandScope, BandType, ChannelMask, Command, DaemonState};

const IDX_W: f32 = 26.0;
const ON_W: f32 = 36.0;
const FREQ_W: f32 = 58.0;
const GAIN_W: f32 = 54.0;
const Q_W: f32 = 50.0;
const CH_W: f32 = 64.0;
const X_W: f32 = 24.0;
/// Width of the abbreviated coloured Type badge (PK/LS/HS…). Compact + scannable;
/// the full names live in its dropdown menu.
const TYPE_W: f32 = 50.0;
/// Width of the filter-slope selector (12/24/48 dB/oct). Only meaningful for
/// shelves + HP/LP; other band types show a dim placeholder to keep alignment.
const SLOPE_W: f32 = 48.0;
/// The slopes offered by the per-band slope selector, in cycle order.
const SLOPES: [u8; 3] = [12, 24, 48];
/// Width of the stereo-scope selector (St/M/S). Applies to every band type;
/// only audible on >= 2-channel streams.
const SCOPE_W: f32 = 44.0;
/// The stereo scopes offered by the per-band scope selector, in menu order.
const SCOPES: [BandScope; 3] = [BandScope::Stereo, BandScope::Mid, BandScope::Side];
/// Cells get an 8px gutter of their own (mockup table `td` padding) while the
/// row tint/rule still span the full card width.
const GUTTER: f32 = 8.0;

/// Resolved column layout for the bands table at a given available width and
/// channel state. Computed once per frame so the header captions and every row
/// agree on which columns show and how wide each is. Columns collapse as the
/// table narrows (the gain graph drops first, then the Type combo).
#[derive(Debug, Clone, Copy, PartialEq)]
struct BandColumns {
    /// Per-band channel-target column (multichannel / per-channel-EQ only).
    show_ch: bool,
    /// Flexible gain-graph column (the first to drop when tight).
    show_graph: bool,
    /// Coloured Type-badge dropdown column.
    show_type: bool,
    /// Filter-slope selector column (rides alongside the Type column).
    show_slope: bool,
    /// Stereo-scope selector column (St/M/S, applies to every band).
    show_scope: bool,
    /// Inter-column spacing.
    gap: f32,
    /// Width of the flexible Graph column (only meaningful when `show_graph`).
    graph_w: f32,
}

impl BandColumns {
    /// Derive the column layout from the table's available width (full pane width
    /// minus the two side gutters), the inter-column `gap`, and whether the
    /// channel column should appear. `avail` is already clamped to a sane minimum.
    // `show_slope` and `show_scope` are deliberately parallel column flags.
    #[allow(clippy::similar_names)]
    fn resolve(avail: f32, gap: f32, show_ch: bool) -> Self {
        // Collapse columns as the table narrows: drop the gain graph first, then
        // the Slope selector, then the Scope selector, then the Type combo.
        let show_graph = avail >= 560.0;
        let show_slope = avail >= 464.0;
        let show_scope = avail >= 410.0;
        let show_type = avail >= 360.0;
        let n_cols = 6
            + usize::from(show_type)
            + usize::from(show_slope)
            + usize::from(show_scope)
            + usize::from(show_graph)
            + usize::from(show_ch);
        let fixed = IDX_W
            + ON_W
            + if show_type { TYPE_W } else { 0.0 }
            + if show_slope { SLOPE_W } else { 0.0 }
            + if show_scope { SCOPE_W } else { 0.0 }
            + FREQ_W
            + GAIN_W
            + Q_W
            + if show_ch { CH_W } else { 0.0 }
            + X_W;
        let graph_w = (avail - fixed - gap * (n_cols as f32 - 1.0)).max(60.0);
        Self {
            show_ch,
            show_graph,
            show_type,
            show_slope,
            show_scope,
            gap,
            graph_w,
        }
    }
}

/// Short label for a band's channel target, e.g. `all` / `FL` / `FL FR` /
/// `FL +2` / `none`. Used in the per-band channel column (multichannel only).
pub(crate) fn channel_tag(mask: ChannelMask, layout: &[String], channels: usize) -> String {
    if mask.is_global(channels) {
        return "all".to_string();
    }
    let names: Vec<&str> = (0..channels)
        .filter(|&c| mask.contains(c))
        .map(|c| layout.get(c).map_or("?", String::as_str))
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

    /// The scrolling EQ-bands table: a header caption row followed by one ruled
    /// row per band. The card body is full-bleed (no horizontal padding) so row
    /// rules and the selection wash run to the card edge, while cell content is
    /// inset by [`GUTTER`].
    pub(crate) fn bands_section(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        let full_w = ui.available_width();
        let avail = (full_w - GUTTER * 2.0).max(60.0);
        // Per-channel EQ: the channel-target column appears on >2-channel
        // devices automatically (progressive disclosure), or when a ≥2ch user
        // opts in via the Channels section's "Per-channel EQ" toggle (lets a
        // stereo user do L/R-specific EQ).
        let show_ch = state.channels > 2 || (self.per_channel_eq && state.channels >= 2);
        let cols = BandColumns::resolve(avail, kit::SP_S, show_ch);

        bands_header(ui, &cols);

        let nbands = state.bands.len();
        for i in 0..nbands {
            self.band_row(ui, state, &cols, i, nbands);
        }

        if self.selected_band >= state.bands.len() {
            self.selected_band = state.bands.len().saturating_sub(1);
        }
    }

    /// Render one band row at index `i` and paint its under-rule / selection bar.
    /// `nbands` is the total count (so the last row skips its bottom rule).
    fn band_row(
        &mut self,
        ui: &mut egui::Ui,
        state: &DaemonState,
        cols: &BandColumns,
        i: usize,
        nbands: usize,
    ) {
        let full_w = ui.available_width();
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
                            ui.spacing_mut().item_spacing.x = cols.gap;
                            ui.add_space(GUTTER);
                            self.band_row_cells(ui, state, cols, i);
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

    /// Lay out one band row's cells left-to-right (index chip, On toggle, optional
    /// Type badge, Freq/Gain/Q number fields, optional channel chip + gain graph,
    /// remove button), queueing the matching command when the user edits a cell.
    /// Runs inside the row's `horizontal` so it shares its spacing + id scope.
    fn band_row_cells(
        &mut self,
        ui: &mut egui::Ui,
        state: &DaemonState,
        cols: &BandColumns,
        i: usize,
    ) {
        let b = &state.bands[i];
        let t = kit::tokens(ui);

        // Index chip doubles as the row selector.
        if self.band_index_chip(ui, i, &t) {
            self.selected_band = i;
        }

        let mut on = b.enabled;
        if kit::toggle(ui, &mut on) {
            self.queue_edit(Command::SetBandEnabled {
                index: i,
                enabled: on,
            });
        }

        if cols.show_type {
            // Coloured abbreviated badge (PK/LS/…); the menu lists full names.
            let labels: Vec<&str> = BAND_TYPES.iter().map(|bt| bt.full()).collect();
            if let Some(sel) = kit::tag_dropdown(
                ui,
                TYPE_W,
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

        if cols.show_slope {
            self.band_slope_cell(ui, b, i, &t);
        }

        if cols.show_scope {
            self.band_scope_cell(ui, b, i, &t);
        }

        let mut freq = b.freq;
        let mut gain = b.gain_db;
        let mut q = b.q;
        // Tint the freq value across the visible-light spectrum so low/high bands
        // read at a glance (red bass → violet treble).
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

        if cols.show_ch {
            self.band_channel_chip(ui, state, i, &t);
        }

        if cols.show_graph {
            gain_bar(ui, cols.graph_w, b.gain_db, &self.palette);
        }

        if kit::icon_btn(ui, Icon::Close, 24.0, "Remove this band") {
            self.queue_edit(Command::RemoveBand { index: i });
            // Keep the lock pins on the same band after the list shifts.
            remap_pin_on_remove(&mut self.vlock, i);
            remap_pin_on_remove(&mut self.hlock, i);
        }
    }

    /// The filter-slope cell for band `i`. Shelves + HP/LP get a 12/24/48 dB/oct
    /// dropdown (reusing the Type badge widget); every other band type is
    /// single-biquad and ignores slope, so it shows a dim "—" placeholder that
    /// keeps the column aligned without offering a no-op control.
    fn band_slope_cell(
        &mut self,
        ui: &mut egui::Ui,
        b: &resonance_ipc::BandState,
        i: usize,
        t: &kit::Tokens,
    ) {
        if b.band_type.uses_slope() {
            let labels: [String; 3] = SLOPES.map(|s| format!("{s} dB/oct"));
            let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            if let Some(sel) = kit::tag_dropdown(
                ui,
                SLOPE_W,
                22.0,
                egui::Id::new(("bs", i)),
                &format!("{}", b.slope_db_oct),
                t.accent,
                &label_refs,
            ) {
                self.queue_edit(Command::SetBandSlope {
                    index: i,
                    slope_db_oct: SLOPES[sel],
                });
            }
        } else {
            // Single-biquad type: no slope. Dim placeholder keeps alignment.
            let (r, _) = ui.allocate_exact_size(egui::vec2(SLOPE_W, 22.0), egui::Sense::hover());
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                "—",
                egui::FontId::monospace(kit::T_VALUE),
                t.faint,
            );
        }
    }

    /// The stereo-scope cell for band `i`. Every band type gets a Stereo/Mid/Side
    /// dropdown (reusing the Type badge widget): the badge shows the abbrev
    /// (St/M/S) and the menu lists the full names. Only audible on >= 2-channel
    /// streams, but always shown so the choice is discoverable.
    fn band_scope_cell(
        &mut self,
        ui: &mut egui::Ui,
        b: &resonance_ipc::BandState,
        i: usize,
        t: &kit::Tokens,
    ) {
        let labels: Vec<&str> = SCOPES.iter().map(|s| s.full()).collect();
        if let Some(sel) = kit::tag_dropdown(
            ui,
            SCOPE_W,
            22.0,
            egui::Id::new(("bsc", i)),
            b.scope.abbrev(),
            t.accent,
            &labels,
        ) {
            self.queue_edit(Command::SetBandScope {
                index: i,
                scope: SCOPES[sel],
            });
        }
    }

    /// The leading index chip (1-based) that doubles as the row selector: filled
    /// with the accent when selected. Returns true when clicked so the caller can
    /// own the `&mut self` selection write.
    fn band_index_chip(&self, ui: &mut egui::Ui, i: usize, t: &kit::Tokens) -> bool {
        let selected = i == self.selected_band;
        let (r, rr) = ui.allocate_exact_size(egui::vec2(IDX_W, 22.0), egui::Sense::click());
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
        rr.clicked()
    }

    /// The per-band channel-target chip + checkbox popup. The chip is coloured by
    /// its target (neutral "all", else the first targeted channel's curve colour).
    /// The new mask is collected out of the popup closure so it never borrows
    /// `self`, then a single edit is queued.
    fn band_channel_chip(
        &mut self,
        ui: &mut egui::Ui,
        state: &DaemonState,
        i: usize,
        t: &kit::Tokens,
    ) {
        let b = &state.bands[i];
        let tag = channel_tag(b.channels, &state.channel_layout, state.channels);
        let col = if b.channels.is_global(state.channels) {
            t.dim
        } else {
            (0..state.channels)
                .find(|&c| b.channels.contains(c))
                .map_or(t.dim, channel_color)
        };
        let resp = kit::tag_chip(ui, CH_W, 22.0, &tag, col);
        let mut new_mask: Option<ChannelMask> = None;
        egui::Popup::menu(&resp)
            .id(egui::Id::new(("ch", i)))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
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
                        mask = if on { mask.with(c) } else { mask.without(c) };
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

/// The table header caption row, aligned to the same column widths as the data
/// rows (optional columns appear only when `cols` enables them).
fn bands_header(ui: &mut egui::Ui, cols: &BandColumns) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = cols.gap;
        ui.add_space(GUTTER);
        let dim = kit::tokens(ui).dim;
        // One left-aligned caption sized to a column's width, so headings sit
        // over their cells.
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
        if cols.show_type {
            cap(ui, TYPE_W, "Type");
        }
        if cols.show_slope {
            cap(ui, SLOPE_W, "Slope");
        }
        if cols.show_scope {
            cap(ui, SCOPE_W, "Scope");
        }
        cap(ui, FREQ_W, "Freq");
        cap(ui, GAIN_W, "Gain");
        cap(ui, Q_W, "Q");
        if cols.show_ch {
            cap(ui, CH_W, "Ch");
        }
        if cols.show_graph {
            cap(ui, cols.graph_w, "Graph");
        }
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_tag_all_and_none() {
        let layout = vec!["FL".into(), "FR".into(), "FC".into()];
        assert_eq!(channel_tag(ChannelMask::ALL, &layout, 3), "all");
        // A mask covering every channel is also "all" (global), not a name list.
        assert_eq!(
            channel_tag(ChannelMask::from_indices(0..3), &layout, 3),
            "all"
        );
        assert_eq!(channel_tag(ChannelMask::NONE, &layout, 3), "none");
    }

    #[test]
    fn channel_tag_one_two_and_overflow() {
        let layout = vec!["FL".into(), "FR".into(), "FC".into(), "LFE".into()];
        assert_eq!(channel_tag(ChannelMask::single(0), &layout, 4), "FL");
        assert_eq!(
            channel_tag(ChannelMask::from_indices([0, 1]), &layout, 4),
            "FL FR"
        );
        // Three-plus collapse to "first +N".
        assert_eq!(
            channel_tag(ChannelMask::from_indices([0, 2, 3]), &layout, 4),
            "FL +2"
        );
    }

    #[test]
    fn channel_tag_missing_layout_name_falls_back() {
        // A channel index past the supplied layout names renders "?".
        assert_eq!(
            channel_tag(ChannelMask::single(1), &["FL".to_string()], 2),
            "?"
        );
    }

    #[test]
    fn columns_collapse_as_width_shrinks() {
        let gap = 6.0;
        // Wide: every optional column shows.
        let wide = BandColumns::resolve(700.0, gap, true);
        assert!(wide.show_graph && wide.show_slope && wide.show_scope && wide.show_type);
        // The graph drops first below 560; slope + scope + type still show.
        let mid = BandColumns::resolve(500.0, gap, true);
        assert!(!mid.show_graph && mid.show_slope && mid.show_scope && mid.show_type);
        // The slope selector drops below 464; scope + type still show.
        let tight = BandColumns::resolve(440.0, gap, true);
        assert!(!tight.show_graph && !tight.show_slope && tight.show_scope && tight.show_type);
        // The scope selector drops below 410; type still shows.
        let tighter = BandColumns::resolve(390.0, gap, true);
        assert!(!tighter.show_slope && !tighter.show_scope && tighter.show_type);
        // The Type combo drops below 360.
        let narrow = BandColumns::resolve(300.0, gap, true);
        assert!(
            !narrow.show_graph && !narrow.show_slope && !narrow.show_scope && !narrow.show_type
        );
    }

    #[test]
    fn graph_width_clamps_to_minimum() {
        // Even at an absurd width the flexible graph never collapses past 60px.
        let cols = BandColumns::resolve(480.0, 6.0, true);
        assert!(cols.graph_w >= 60.0);
    }

    #[test]
    fn show_ch_passes_through_to_layout() {
        assert!(!BandColumns::resolve(700.0, 6.0, false).show_ch);
        assert!(BandColumns::resolve(700.0, 6.0, true).show_ch);
    }

    #[test]
    fn remap_pin_on_remove_cases() {
        // Removing the pinned band drops the pin.
        let mut pin = Some(2);
        remap_pin_on_remove(&mut pin, 2);
        assert_eq!(pin, None);
        // Removing below the pin shifts it down by one.
        let mut pin = Some(3);
        remap_pin_on_remove(&mut pin, 1);
        assert_eq!(pin, Some(2));
        // Removing above the pin leaves it unchanged.
        let mut pin = Some(1);
        remap_pin_on_remove(&mut pin, 3);
        assert_eq!(pin, Some(1));
        // No pin stays no pin.
        let mut pin = None;
        remap_pin_on_remove(&mut pin, 0);
        assert_eq!(pin, None);
    }
}
