//! EQ bands table: per-band type/freq/gain/Q editors, gain bars, add/remove.

use crate::app::GuiApp;
use crate::state::{BAND_TYPES, GAIN_LIMIT, Q_LIMIT};
use crate::ui::widgets::{centered, gain_bar};
use eframe::egui;
use resonance_ipc::{BandType, Command, DaemonState};

impl GuiApp {
    pub(crate) fn bands_section(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        // Collapse columns as the table narrows so it never needs a horizontal
        // scrollbar: drop the decorative gain bar first, then the Type combo (the
        // widest cell), abbreviating the type label when it's tight. Header and
        // body branch on the SAME flags so the grid's column count always matches
        // the cells emitted (a mismatch would misalign the whole table).
        let avail = ui.available_width();
        let show_graph = avail >= 520.0;
        let show_type = avail >= 380.0;
        let abbrev_type = avail < 540.0;
        let num_columns = 6 + show_type as usize + show_graph as usize;

        centered(ui, "bands_body", |ui| {
            egui::Grid::new("bands_grid")
                .num_columns(num_columns)
                .striped(true)
                .spacing([10.0, 4.0])
                .show(ui, |ui| {
                    ui.label("#");
                    ui.label("On");
                    if show_type {
                        ui.label("Type");
                    }
                    ui.label("Freq (Hz)");
                    ui.label("Gain (dB)");
                    ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                        ui.label("Q")
                    });
                    if show_graph {
                        ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                            ui.label("Gain Graph")
                        });
                    }
                    ui.label("");
                    ui.end_row();

                    for (i, b) in state.bands.iter().enumerate() {
                        let selected = i == self.selected_band;

                        // Band number doubles as the row selector (replaces the old ● dot).
                        if ui
                            .selectable_label(selected, format!("{:>2}", i + 1))
                            .on_hover_text("select this band")
                            .clicked()
                        {
                            self.selected_band = i;
                        }

                        let mut on = b.enabled;
                        if ui.checkbox(&mut on, "").changed() {
                            self.queue_edit(Command::SetBandEnabled {
                                index: i,
                                enabled: on,
                            });
                        }

                        // Type combo (hidden on very narrow tables; abbreviated when tight).
                        if show_type {
                            let mut bt = b.band_type;
                            let (label, w) = if abbrev_type {
                                (bt.abbrev(), 52.0)
                            } else {
                                (bt.full(), 92.0)
                            };
                            egui::ComboBox::from_id_salt(("bt", i))
                                .selected_text(label)
                                .width(w)
                                .show_ui(ui, |ui| {
                                    for cand in BAND_TYPES {
                                        if ui.selectable_value(&mut bt, cand, cand.full()).clicked()
                                        {
                                        }
                                    }
                                });
                            if bt != b.band_type {
                                self.queue_edit(Command::SetBandType {
                                    index: i,
                                    band_type: bt,
                                });
                            }
                        }

                        // Freq / gain / Q drag values.
                        let mut freq = b.freq;
                        let mut gain = b.gain_db;
                        let mut q = b.q;
                        let f_changed = ui
                            .add(
                                egui::DragValue::new(&mut freq)
                                    .speed(2.0)
                                    .range(20.0..=20000.0)
                                    .fixed_decimals(0),
                            )
                            .changed();
                        let g_changed = ui
                            .add(
                                egui::DragValue::new(&mut gain)
                                    .speed(0.1)
                                    .range(-GAIN_LIMIT..=GAIN_LIMIT)
                                    .fixed_decimals(1),
                            )
                            .changed();
                        let q_changed = ui
                            .add(
                                egui::DragValue::new(&mut q)
                                    .speed(0.02)
                                    .range(0.1..=Q_LIMIT)
                                    .fixed_decimals(2),
                            )
                            .changed();
                        if f_changed || g_changed || q_changed {
                            self.queue_edit(Command::SetBand {
                                index: i,
                                freq,
                                gain_db: gain,
                                q,
                            });
                        }

                        // Centre-out gain bar (the TUI's gain graph): fills right for
                        // boosts, left for cuts, tinted by gain colour.
                        if show_graph {
                            gain_bar(ui, b.gain_db, &self.palette);
                        }

                        if ui.button("✕").on_hover_text("remove").clicked() {
                            self.queue_edit(Command::RemoveBand { index: i });
                            // Keep the lock pins pointing at the same band after
                            // the list shifts (or drop them if the pinned band
                            // was the one removed).
                            remap_pin_on_remove(&mut self.vlock, i);
                            remap_pin_on_remove(&mut self.hlock, i);
                        }
                        ui.end_row();
                    }
                });
        });

        // "Add band" sits under the table, reading as "append a new row below".
        // Subtle (weak text, default chrome) so it matches the rest of the UI.
        ui.add_space(6.0);
        centered(ui, "bands_add", |ui| {
            let btn = egui::Button::new("✚  Add band").min_size(egui::vec2(160.0, 24.0));
            if ui.add(btn).on_hover_text("append a new EQ band").clicked() {
                self.queue_edit(Command::AddBand {
                    band_type: BandType::Peaking,
                    freq: 1000.0,
                    gain_db: 0.0,
                    q: 1.4,
                });
            }
        });

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
