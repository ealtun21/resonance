//! EQ bands table: per-band type/freq/gain/Q editors drawn with the bespoke kit
//! (custom toggle, dropdown, drag-or-type number fields, icon buttons). Manual
//! rows with fixed columns + a flexible Graph column, so the table fills its pane
//! at any width and the columns stay aligned. Columns collapse as it narrows.

use crate::app::GuiApp;
use crate::state::{BAND_TYPES, GAIN_LIMIT, Q_LIMIT};
use crate::ui::kit;
use crate::ui::widgets::gain_bar;
use eframe::egui;
use resonance_ipc::{BandType, Command, DaemonState};

const IDX_W: f32 = 26.0;
const ON_W: f32 = 36.0;
const FREQ_W: f32 = 58.0;
const GAIN_W: f32 = 54.0;
const Q_W: f32 = 50.0;
const X_W: f32 = 24.0;

impl GuiApp {
    pub(crate) fn bands_section(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        let avail = ui.available_width();
        // Collapse columns as the table narrows: drop the gain graph first, then
        // the Type combo (abbreviated when tight).
        let show_graph = avail >= 480.0;
        let show_type = avail >= 360.0;
        let abbrev_type = avail < 560.0;
        let type_w = if abbrev_type { 56.0 } else { 96.0 };
        let gap = kit::SP_S;
        let n_cols = 6 + show_type as usize + show_graph as usize;
        let fixed =
            IDX_W + ON_W + if show_type { type_w } else { 0.0 } + FREQ_W + GAIN_W + Q_W + X_W;
        let graph_w = (avail - fixed - gap * (n_cols as f32 - 1.0)).max(60.0);

        // Header captions, aligned to the same column widths as the rows.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
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
            if show_graph {
                cap(ui, graph_w, "Graph");
            }
        });

        for (i, b) in state.bands.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.set_min_height(26.0);
                ui.spacing_mut().item_spacing.x = gap;
                let t = kit::tokens(ui);

                // Index chip doubles as the row selector.
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
                    let labels: Vec<&str> = BAND_TYPES
                        .iter()
                        .map(|bt| if abbrev_type { bt.abbrev() } else { bt.full() })
                        .collect();
                    let cur = if abbrev_type {
                        b.band_type.abbrev()
                    } else {
                        b.band_type.full()
                    };
                    if let Some(sel) =
                        kit::dropdown(ui, type_w, egui::Id::new(("bt", i)), cur, &labels)
                    {
                        self.queue_edit(Command::SetBandType {
                            index: i,
                            band_type: BAND_TYPES[sel],
                        });
                    }
                }

                let mut freq = b.freq;
                let mut gain = b.gain_db;
                let mut q = b.q;
                let fc = kit::num_field(
                    ui,
                    FREQ_W,
                    egui::Id::new(("f", i)),
                    &mut freq,
                    20.0..=20000.0,
                    0,
                    2.0,
                );
                let gc = kit::num_field(
                    ui,
                    GAIN_W,
                    egui::Id::new(("g", i)),
                    &mut gain,
                    -GAIN_LIMIT..=GAIN_LIMIT,
                    1,
                    0.1,
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

                if show_graph {
                    gain_bar(ui, graph_w, b.gain_db, &self.palette);
                }

                if kit::icon_button(ui, "✕") {
                    self.queue_edit(Command::RemoveBand { index: i });
                    // Keep the lock pins on the same band after the list shifts.
                    remap_pin_on_remove(&mut self.vlock, i);
                    remap_pin_on_remove(&mut self.hlock, i);
                }
            });
        }

        ui.add_space(kit::SP_S);
        if kit::button(ui, "✚  Add band", false, true) {
            self.queue_edit(Command::AddBand {
                band_type: BandType::Peaking,
                freq: 1000.0,
                gain_db: 0.0,
                q: 1.4,
            });
        }

        if self.selected_band >= state.bands.len() {
            self.selected_band = state.bands.len().saturating_sub(1);
        }
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
