//! Per-output-sink volume/mute panel.
//!
//! One row per real output device (the daemon's `DaemonState::sinks`), styled
//! identically to the Applications rows: a mute pill-toggle, the device name
//! (dim when muted), a right-aligned volume percentage (boost-green above
//! 100%), and a thin full-width volume slider beneath. Progressive disclosure —
//! the section is only shown when the backend reports output sinks.

use crate::app::GuiApp;
use crate::ui::kit;
use eframe::egui;
use resonance_ipc::{Command, DaemonState, SinkVolume};

/// Volume slider range: 0–100 % (matches the Applications panel). Boost
/// (>100 %) is set via the CLI and still shows here as a boost-green percentage
/// with the slider pegged full.
const VOL_MAX: f64 = 1.0;

impl GuiApp {
    /// Render the per-output-sink rows for the current state.
    pub(crate) fn sinks_section(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        // Clone the list so the rows can borrow it while `self` is mutated by the
        // control callbacks (`queue` takes `&mut self`).
        let sinks = state.sinks.clone();
        for (i, sink) in sinks.iter().enumerate() {
            if i > 0 {
                ui.add_space(kit::SP_S);
            }
            self.sink_row(ui, sink);
        }
    }

    fn sink_row(&mut self, ui: &mut egui::Ui, sink: &SinkVolume) {
        const PCT_W: f32 = 46.0;
        let boost_col = self.palette.boost;
        let label = if sink.description.is_empty() {
            &sink.name
        } else {
            &sink.description
        };

        // Line 1: mute toggle · name · right-aligned percentage.
        ui.horizontal(|ui| {
            ui.set_min_height(22.0);
            ui.spacing_mut().item_spacing.x = kit::SP_S;

            // The toggle reads as "audible": on = not muted.
            let mut audible = !sink.muted;
            if kit::toggle(ui, &mut audible) {
                // `queue`, not `queue_edit`: output-sink volume/mute isn't part of
                // the EQ profile, so it must not mark the profile dirty or push undo.
                self.queue(Command::SetSinkMute {
                    name: sink.name.clone(),
                    muted: !audible,
                });
            }

            let t = kit::tokens(ui);
            let name_w = (ui.available_width() - PCT_W - kit::SP_S).max(40.0);
            let (nr, _) = ui.allocate_exact_size(egui::vec2(name_w, 22.0), egui::Sense::hover());
            let name_col = if sink.muted { t.faint } else { t.text };
            let font = egui::FontId::proportional(kit::T_BODY);
            let shown = crate::ui::widgets::ellipsize_to_width(ui, label, &font, nr.width());
            ui.painter().text(
                egui::pos2(nr.left(), nr.center().y),
                egui::Align2::LEFT_CENTER,
                shown,
                font,
                name_col,
            );

            let (pr, _) = ui.allocate_exact_size(egui::vec2(PCT_W, 22.0), egui::Sense::hover());
            let pct_col = if sink.muted {
                t.faint
            } else if sink.volume > 1.0001 {
                boost_col
            } else {
                t.dim
            };
            ui.painter().text(
                egui::pos2(pr.right(), pr.center().y),
                egui::Align2::RIGHT_CENTER,
                format!("{:.0}%", sink.volume * 100.0),
                egui::FontId::monospace(kit::T_CAPTION),
                pct_col,
            );
        });

        // Line 2: thin full-width volume slider.
        ui.add_space(2.0);
        let mut vol = sink.volume.min(VOL_MAX);
        if kit::slider_h(ui, ui.available_width(), 12.0, &mut vol, 0.0..=VOL_MAX) {
            self.queue(Command::SetSinkVolume {
                name: sink.name.clone(),
                volume: vol,
            });
        }
    }
}
