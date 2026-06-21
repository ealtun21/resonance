//! Effects section: the FxSound effect toggles + intensity sliders.

use crate::app::GuiApp;
use crate::ui::widgets::centered;
use eframe::egui;
use resonance_ipc::{Command, DaemonState, FxEffectId};

impl GuiApp {
    pub(crate) fn effects_section(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        ui.vertical_centered(|ui| ui.heading("Effects"));
        ui.add_space(4.0);
        centered(ui, "effects_body", |ui| {
            egui::Grid::new("effects_grid")
                .num_columns(3)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    for id in FxEffectId::ALL {
                        let name = id.label();
                        let (mut intensity, mut on) = state.effects.get(id);
                        let min = id.min();

                        if ui.checkbox(&mut on, "").changed() {
                            self.queue_edit(Command::SetEffectEnabled {
                                effect: id,
                                enabled: on,
                            });
                        }
                        ui.label(name);
                        // Slider is always interactive — dragging it auto-enables the
                        // effect so you can set a value without first ticking the box.
                        if ui
                            .add(
                                egui::Slider::new(&mut intensity, min..=1.0)
                                    .custom_formatter(|v, _| format!("{:+.0}%", v * 100.0))
                                    .custom_parser(|s| {
                                        s.trim_end_matches('%')
                                            .parse::<f64>()
                                            .ok()
                                            .map(|v| v / 100.0)
                                    }),
                            )
                            .changed()
                        {
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
                        ui.end_row();
                    }
                });
        });
    }
}
