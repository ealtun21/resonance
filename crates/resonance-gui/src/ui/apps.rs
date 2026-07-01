//! Per-application volume/mute panel.
//!
//! One row per application stream, styled to match the Effects rows: a mute
//! pill-toggle, the app name (dim when muted or idle), a right-aligned volume
//! percentage (tinted boost-green above 100%), and a thin full-width volume
//! slider beneath. Progressive disclosure — the section is only shown when the
//! backend reports application streams (`DaemonState::apps`).

use crate::app::GuiApp;
use crate::ui::kit;
use eframe::egui;
use resonance_ipc::{AppStream, Command, DaemonState};

/// Volume slider range: 0–100 % (the conventional full-at-unity look). Boost
/// (>100 %, supported on Linux/macOS) is set via the CLI for now and still shows
/// here as a boost-green percentage with the slider pegged full.
const VOL_MAX: f64 = 1.0;

impl GuiApp {
    /// Render the per-application rows for the current state.
    pub(crate) fn apps_section(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        // Clone the list so the rows can borrow it while `self` is mutated by the
        // control callbacks (queue_edit takes `&mut self`).
        let apps = state.apps.clone();
        for (i, app) in apps.iter().enumerate() {
            if i > 0 {
                ui.add_space(kit::SP_S);
            }
            self.app_row(ui, app);
        }
    }

    fn app_row(&mut self, ui: &mut egui::Ui, app: &AppStream) {
        const PCT_W: f32 = 46.0;
        let boost_col = self.palette.boost;

        // Line 1: mute toggle · name · right-aligned percentage.
        ui.horizontal(|ui| {
            ui.set_min_height(22.0);
            ui.spacing_mut().item_spacing.x = kit::SP_S;

            // The toggle reads as "audible": on = not muted.
            let mut audible = !app.muted;
            if kit::toggle(ui, &mut audible) {
                // `queue`, not `queue_edit`: per-app volume/mute isn't part of the
                // EQ profile, so it must not mark the profile dirty or push undo.
                self.queue(Command::SetAppMute {
                    key: app.key.clone(),
                    muted: !audible,
                });
            }

            let t = kit::tokens(ui);
            let name_w = (ui.available_width() - PCT_W - kit::SP_S).max(40.0);
            let (nr, _) = ui.allocate_exact_size(egui::vec2(name_w, 22.0), egui::Sense::hover());
            let name_col = if app.muted {
                t.faint
            } else if app.active {
                t.text
            } else {
                t.dim
            };
            let font = egui::FontId::proportional(kit::T_BODY);
            let shown = crate::ui::widgets::ellipsize_to_width(ui, &app.display_name, &font, nr.width());
            ui.painter().text(
                egui::pos2(nr.left(), nr.center().y),
                egui::Align2::LEFT_CENTER,
                shown,
                font,
                name_col,
            );

            let (pr, _) = ui.allocate_exact_size(egui::vec2(PCT_W, 22.0), egui::Sense::hover());
            let pct_col = if app.muted {
                t.faint
            } else if app.volume > 1.0001 {
                boost_col
            } else {
                t.dim
            };
            ui.painter().text(
                egui::pos2(pr.right(), pr.center().y),
                egui::Align2::RIGHT_CENTER,
                format!("{:.0}%", app.volume * 100.0),
                egui::FontId::monospace(kit::T_CAPTION),
                pct_col,
            );
        });

        // Line 2: thin full-width volume slider.
        ui.add_space(2.0);
        let mut vol = app.volume.min(VOL_MAX);
        if kit::slider_h(ui, ui.available_width(), 12.0, &mut vol, 0.0..=VOL_MAX) {
            self.queue(Command::SetAppVolume {
                key: app.key.clone(),
                volume: vol,
            });
        }
    }
}
