//! Effects section: the FxSound effect toggles + intensity sliders, drawn with
//! the bespoke kit so every row aligns (toggle · name · slider · value).

use crate::app::GuiApp;
use crate::ui::kit;
use eframe::egui;
use resonance_ipc::{Command, DaemonState, FxEffectId};

impl GuiApp {
    pub(crate) fn effects_section(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        const NAME_W: f32 = 100.0;
        const VALUE_W: f32 = 52.0;
        for id in FxEffectId::ALL {
            let (mut intensity, mut on) = state.effects.get(id);
            let min = id.min();
            ui.horizontal(|ui| {
                ui.set_min_height(kit::ROW_H);
                ui.spacing_mut().item_spacing.x = kit::SP_S;
                ui.add_space(kit::SP_XS);

                if kit::toggle(ui, &mut on) {
                    self.queue_edit(Command::SetEffectEnabled {
                        effect: id,
                        enabled: on,
                    });
                }

                // Name (dimmed while the effect is off) in a fixed column so the
                // sliders below it all start at the same x.
                let t = kit::tokens(ui);
                let (lr, _) =
                    ui.allocate_exact_size(egui::vec2(NAME_W, kit::ROW_H), egui::Sense::hover());
                ui.painter().text(
                    egui::pos2(lr.left(), lr.center().y),
                    egui::Align2::LEFT_CENTER,
                    id.label(),
                    egui::FontId::proportional(kit::T_BODY),
                    if on { t.text } else { t.dim },
                );

                // Slider fills the gap to the value chip. Dragging it also enables
                // the effect, so you can set a value without ticking first.
                let slider_w = (ui.available_width() - VALUE_W - kit::SP_S).max(60.0);
                if kit::slider(ui, slider_w, &mut intensity, min..=1.0) {
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
                kit::value_chip(ui, VALUE_W, &format!("{:+.0}%", intensity * 100.0));
            });
        }
    }
}
