//! EQ bands table: per-band type/freq/gain/Q editors drawn with the bespoke kit
//! (custom toggle, dropdown, drag-or-type number fields, icon buttons) so it
//! matches the rest of the UI. Columns collapse as the table narrows.

use crate::app::GuiApp;
use crate::state::{BAND_TYPES, GAIN_LIMIT, Q_LIMIT};
use crate::ui::kit;
use crate::ui::widgets::gain_bar;
use eframe::egui;
use resonance_ipc::{BandType, Command, DaemonState};

impl GuiApp {
    pub(crate) fn bands_section(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        // Collapse columns as the table narrows: drop the decorative gain bar
        // first, then the Type combo, abbreviating its label when tight. Header
        // and body branch on the SAME flags so the grid column count matches.
        let avail = ui.available_width();
        let show_graph = avail >= 520.0;
        let show_type = avail >= 380.0;
        let abbrev_type = avail < 560.0;
        let num_columns = 6 + show_type as usize + show_graph as usize;

        egui::Grid::new("bands_grid")
            .num_columns(num_columns)
            .spacing([kit::SP_S, 6.0])
            .show(ui, |ui| {
                let t = kit::tokens(ui);
                let cap = |ui: &mut egui::Ui, s: &str| {
                    ui.label(
                        egui::RichText::new(s)
                            .size(kit::T_CAPTION)
                            .strong()
                            .color(t.dim),
                    );
                };
                cap(ui, "#");
                cap(ui, "On");
                if show_type {
                    cap(ui, "Type");
                }
                cap(ui, "Freq");
                cap(ui, "Gain");
                cap(ui, "Q");
                if show_graph {
                    cap(ui, "Graph");
                }
                cap(ui, "");
                ui.end_row();

                for (i, b) in state.bands.iter().enumerate() {
                    let selected = i == self.selected_band;

                    // Index chip doubles as the row selector.
                    let (r, rr) =
                        ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
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
                        let w = if abbrev_type { 56.0 } else { 96.0 };
                        if let Some(sel) =
                            kit::dropdown(ui, w, egui::Id::new(("bt", i)), cur, &labels)
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
                        58.0,
                        egui::Id::new(("f", i)),
                        &mut freq,
                        20.0..=20000.0,
                        0,
                        2.0,
                    );
                    let gc = kit::num_field(
                        ui,
                        54.0,
                        egui::Id::new(("g", i)),
                        &mut gain,
                        -GAIN_LIMIT..=GAIN_LIMIT,
                        1,
                        0.1,
                    );
                    let qc = kit::num_field(
                        ui,
                        50.0,
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
                        gain_bar(ui, b.gain_db, &self.palette);
                    }

                    if kit::icon_button(ui, "✕") {
                        self.queue_edit(Command::RemoveBand { index: i });
                        // Keep the lock pins pointing at the same band after the
                        // list shifts (or drop them if the pinned band was removed).
                        remap_pin_on_remove(&mut self.vlock, i);
                        remap_pin_on_remove(&mut self.hlock, i);
                    }
                    ui.end_row();
                }
            });

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
