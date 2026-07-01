//! Effects section: the `FxSound` effect toggles + intensity sliders, drawn with
//! the bespoke kit so every row aligns (toggle · name · slider · value).

use crate::app::GuiApp;
use crate::ui::kit;
use eframe::egui;
use resonance_ipc::{Command, DaemonState, FxEffectId};

impl GuiApp {
    /// Demoted `FxSound` effects (mockup `.fx`): each effect is a two-line block —
    /// `[toggle]  Name ………… +NN%` over a full-width thin slider — so the EQ stays
    /// the visual lead and the effects read as a quiet secondary rack.
    pub(crate) fn effects_section(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        const PCT_W: f32 = 42.0;
        let n = FxEffectId::ALL.len();
        for (idx, id) in FxEffectId::ALL.into_iter().enumerate() {
            let (mut intensity, mut on) = state.effects.get(id);
            let min = id.min();

            // Line 1: toggle · name (dim when off) · right-aligned percentage.
            ui.horizontal(|ui| {
                ui.set_min_height(22.0);
                ui.spacing_mut().item_spacing.x = kit::SP_S;
                if kit::toggle(ui, &mut on) {
                    self.queue_edit(Command::SetEffectEnabled {
                        effect: id,
                        enabled: on,
                    });
                }
                let t = kit::tokens(ui);
                let pct = format!("{:+.0}%", intensity * 100.0);
                let name_w = (ui.available_width() - PCT_W - kit::SP_S).max(40.0);
                let (nr, _) =
                    ui.allocate_exact_size(egui::vec2(name_w, 22.0), egui::Sense::hover());
                ui.painter().text(
                    egui::pos2(nr.left(), nr.center().y),
                    egui::Align2::LEFT_CENTER,
                    id.label(),
                    egui::FontId::proportional(kit::T_BODY),
                    if on { t.text } else { t.faint },
                );
                let (pr, _) = ui.allocate_exact_size(egui::vec2(PCT_W, 22.0), egui::Sense::hover());
                ui.painter().text(
                    egui::pos2(pr.right(), pr.center().y),
                    egui::Align2::RIGHT_CENTER,
                    &pct,
                    egui::FontId::monospace(kit::T_CAPTION),
                    if on { t.dim } else { t.faint },
                );
            });

            // Line 2: a thin full-width slider. Dragging also enables the effect,
            // so a value can be dialled in without ticking the toggle first.
            ui.add_space(2.0);
            if kit::slider_h(ui, ui.available_width(), 12.0, &mut intensity, min..=1.0) {
                if !on {
                    self.queue_edit(Command::SetEffectEnabled {
                        effect: id,
                        enabled: true,
                    });
                }
                self.queue_edit(Command::SetEffectIntensity {
                    effect: id,
                    value: intensity,
                });
            }
            if idx + 1 < n {
                ui.add_space(kit::SP_S);
            }
        }
    }

    /// Output-stage settings: currently the TPDF dither selector (Off / 16 / 20 /
    /// 24-bit). A segmented row of pills — `active` marks the live `dither_bits`.
    pub(crate) fn output_section(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        // (label, bits) — `None` = dither off.
        const CHOICES: [(&str, Option<u32>); 4] = [
            ("Off", None),
            ("16", Some(16)),
            ("20", Some(20)),
            ("24", Some(24)),
        ];
        ui.horizontal(|ui| {
            ui.set_min_height(kit::CTRL_H);
            ui.spacing_mut().item_spacing.x = kit::SP_XS;
            let t = kit::tokens(ui);
            ui.painter().text(
                egui::pos2(ui.cursor().left(), ui.cursor().center().y),
                egui::Align2::LEFT_CENTER,
                "Dither",
                egui::FontId::proportional(kit::T_BODY),
                t.dim,
            );
            ui.add_space(kit::text_width(ui, kit::T_BODY, "Dither") + kit::SP_S);
            for (label, bits) in CHOICES {
                let active = state.dither_bits == bits;
                if kit::pill_icon(ui, None, label, active, false, true, "output dither depth")
                    && !active
                {
                    self.queue_edit(Command::SetDither { bits });
                }
            }
        });
    }
}
