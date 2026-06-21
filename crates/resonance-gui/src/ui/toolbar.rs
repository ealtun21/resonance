//! The top toolbar: a single wrapping row of power, preamp, output picker,
//! undo/redo, the daemon menu, an overflow (☰) menu, and the level/DSP meters.
//! `horizontal_wrapped` lets controls reflow onto a second line instead of
//! clipping or jittering when the window is narrow — no measured column grid.

use crate::app::{GuiApp, ServiceAction, ServiceFn};
use crate::browser::Browser;
use crate::state::{Dialog, SaveDialog};
use crate::theme::Theme;
use crate::ui::widgets::ellipsize;
use eframe::egui;
use resonance_ipc::{Command, DaemonState, service};
use std::time::Instant;

impl GuiApp {
    pub(crate) fn toolbar(&mut self, ui: &mut egui::Ui) {
        let state = self.state.clone();
        // One wrapping row: groups reflow onto a second line as the window
        // narrows. Power and the output picker stay one-click; everything that
        // doesn't need to be immediate lives behind the ☰ overflow menu.
        ui.horizontal_wrapped(|ui| {
            ui.add_space(4.0);
            self.tb_power(ui, &state);
            ui.separator();
            self.tb_preamp(ui, &state);
            ui.separator();
            self.tb_output(ui, &state);
            ui.separator();
            self.tb_history(ui);
            ui.separator();
            self.daemon_menu(ui);
            self.overflow_menu(ui);
            if let Some(s) = &state {
                ui.separator();
                self.meters_widget(ui, s);
            }
            self.tb_status(ui);
        });
    }

    /// Prominent power toggle: a large filled green/red button with a status dot,
    /// not a tiny checkbox.
    fn tb_power(&mut self, ui: &mut egui::Ui, state: &Option<DaemonState>) {
        let enabled = state.as_ref().map(|s| s.enabled).unwrap_or(false);
        let (txt, fill) = if enabled {
            ("ON", self.palette.boost)
        } else {
            ("OFF", self.palette.cut)
        };
        // A transparent default-font label sizes the button like the other
        // toolbar buttons; the visible status dot + label are painted as one
        // centred group on top (the font's ● glyph sits off-centre, so we draw
        // our own dot).
        let font = egui::TextStyle::Button.resolve(ui.style());
        let power_btn = egui::Button::new(
            egui::RichText::new(format!("   {txt}"))
                .font(font.clone())
                .color(egui::Color32::TRANSPARENT),
        )
        .fill(fill)
        .min_size(egui::vec2(66.0, 0.0));
        let resp = ui
            .add_enabled(state.is_some(), power_btn)
            .on_hover_text("toggle DSP power");
        let r = resp.rect;
        let galley =
            ui.painter()
                .layout_no_wrap(txt.to_string(), font.clone(), egui::Color32::WHITE);
        const DOT_D: f32 = 10.0;
        const GAP: f32 = 6.0;
        let block_w = DOT_D + GAP + galley.size().x;
        let start_x = r.center().x - block_w * 0.5;
        let cy = r.center().y;
        let dot_c = egui::pos2(start_x + DOT_D * 0.5, cy);
        if enabled {
            ui.painter().circle_filled(dot_c, 5.0, egui::Color32::WHITE);
        } else {
            ui.painter()
                .circle_stroke(dot_c, 4.5, egui::Stroke::new(1.6, egui::Color32::WHITE));
        }
        ui.painter().text(
            egui::pos2(start_x + DOT_D + GAP, cy),
            egui::Align2::LEFT_CENTER,
            txt,
            font,
            egui::Color32::WHITE,
        );
        if resp.clicked() {
            self.queue_edit(Command::SetPower { enabled: !enabled });
        }
    }

    fn tb_preamp(&mut self, ui: &mut egui::Ui, state: &Option<DaemonState>) {
        ui.label("Preamp");
        // Fixed rail width: deriving it from the row's remaining space inside a
        // wrapped layout changes the wrap point and oscillates.
        ui.spacing_mut().slider_width = 140.0;
        if let Some(s) = state {
            let mut db = s.preamp_db;
            if ui
                .add(
                    egui::Slider::new(&mut db, -20.0..=20.0)
                        .suffix(" dB")
                        .fixed_decimals(1),
                )
                .changed()
            {
                self.queue_edit(Command::SetPreamp { db });
            }
        } else {
            ui.add_enabled(false, egui::Slider::new(&mut 0.0, -20.0..=20.0));
        }
    }

    /// Output device picker (left-to-right: 🔊 then the combo).
    fn tb_output(&mut self, ui: &mut egui::Ui, state: &Option<DaemonState>) {
        if let Some(s) = state {
            ui.label("🔊");
            self.output_combo(ui, s);
        }
    }

    fn output_combo(&mut self, ui: &mut egui::Ui, s: &DaemonState) {
        // Sentinel for the "follow the OS default output" choice.
        const AUTO: &str = "\u{0}auto";
        let following = s.preferred_output.is_none();
        let current = if following {
            AUTO.to_string()
        } else {
            s.preferred_output.clone().unwrap_or_default()
        };
        let mut sel = current.clone();
        // When following the system, show the device it's currently on.
        let selected_text = if following {
            match &s.active_output {
                Some(d) => format!("Auto · {}", ellipsize(&s.sink_label(d), 16)),
                None => "Automatic".to_string(),
            }
        } else {
            ellipsize(&s.sink_label(&sel), 24)
        };
        egui::ComboBox::from_id_salt("toolbar_sink")
            .selected_text(selected_text)
            .width(180.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut sel, AUTO.to_string(), "Automatic (follow system)");
                ui.separator();
                for sink in &s.available_sinks {
                    let label = s.sink_label(sink);
                    ui.selectable_value(&mut sel, sink.clone(), label);
                }
            });
        if sel != current {
            if sel == AUTO {
                self.queue(Command::FollowSystemOutput);
            } else if !sel.is_empty() {
                self.queue(Command::SetOutputTarget { node_name: sel });
            }
        }
    }

    fn tb_history(&mut self, ui: &mut egui::Ui) {
        if ui
            .add_enabled(!self.undo_stack.is_empty(), egui::Button::new("Undo"))
            .clicked()
        {
            self.undo();
        }
        if ui
            .add_enabled(!self.redo_stack.is_empty(), egui::Button::new("Redo"))
            .clicked()
        {
            self.redo();
        }
    }

    fn tb_status(&mut self, ui: &mut egui::Ui) {
        if !self.status.is_empty() {
            ui.separator();
            ui.label(&self.status);
        }
    }

    /// Overflow menu (☰): theme picker, load/export, and reset-layout — controls
    /// that don't need to be one-click, kept out of the main row to stay tidy on
    /// small windows.
    fn overflow_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("☰", |ui| {
            ui.menu_button("Theme", |ui| {
                let ctx = ui.ctx().clone();
                for t in Theme::ALL {
                    if ui.selectable_label(self.theme == t, t.label()).clicked() {
                        self.set_theme(&ctx, t);
                        ui.close();
                    }
                }
            });
            ui.separator();
            if ui
                .button("Load…")
                .on_hover_text("import a .fac / APO .txt / Resonance .toml file")
                .clicked()
            {
                self.open_load_dialog();
                ui.close();
            }
            if ui
                .button("Export…")
                .on_hover_text("save the current chain as a Resonance .toml profile")
                .clicked()
            {
                self.open_export_dialog();
                ui.close();
            }
            ui.separator();
            if ui
                .button("Reset layout")
                .on_hover_text("restore default panel sizes")
                .clicked()
            {
                self.reset_layout(ui.ctx());
                ui.close();
            }
        });
    }

    fn open_load_dialog(&mut self) {
        let lib = resonance_ipc::paths::user_preset_dir();
        let _ = std::fs::create_dir_all(&lib);
        let start = if lib.is_dir() {
            lib
        } else {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        };
        self.dialog = Dialog::LoadPreset(Browser::new(start, false));
    }

    fn open_export_dialog(&mut self) {
        let lib = resonance_ipc::paths::user_preset_dir();
        let _ = std::fs::create_dir_all(&lib);
        let stem = self
            .state
            .as_ref()
            .and_then(|s| s.current_preset.clone())
            .unwrap_or_else(|| "resonance".to_string());
        self.dialog = Dialog::ExportProfile(SaveDialog {
            browser: Browser::new(lib, true),
            filename: stem,
        });
    }

    /// Daemon lifecycle controls (systemd/launchd/Windows service) as a compact
    /// menu with a status dot, so users never type a `systemctl` line. All ops
    /// dispatch to a worker thread so the UI never blocks on launchctl/systemctl.
    fn daemon_menu(&mut self, ui: &mut egui::Ui) {
        if !service::manager_available() {
            return;
        }
        let st = self.daemon_status;
        let busy = self.service_busy;
        let (dot, color) = if busy {
            ("…", self.palette.boost)
        } else if st.active {
            ("●", self.palette.boost)
        } else {
            ("○", self.palette.cut)
        };
        ui.menu_button(
            egui::RichText::new(format!("{dot} Daemon")).color(color),
            |ui| {
                ui.label(format!(
                    "{}  ·  autostart {}",
                    if busy {
                        "…"
                    } else if st.active {
                        "running"
                    } else {
                        "stopped"
                    },
                    if st.enabled { "on" } else { "off" },
                ));
                ui.separator();
                let actions: [(&str, ServiceFn); 3] = [
                    ("Start", service::start),
                    ("Stop", service::stop),
                    ("Restart", service::restart),
                ];
                for (label, f) in actions {
                    let btn = ui.add_enabled(!busy, egui::Button::new(label));
                    if btn.clicked() {
                        self.service_busy = true;
                        let _ = self.service_tx.send(ServiceAction::Run { label, f });
                    }
                }
                ui.separator();
                let mut autostart = st.enabled;
                let auto = ui.add_enabled(
                    !busy,
                    egui::Checkbox::new(&mut autostart, "Autostart at login"),
                );
                if auto.changed() {
                    self.service_busy = true;
                    let f: ServiceFn = if autostart {
                        service::enable
                    } else {
                        service::disable
                    };
                    let _ = self.service_tx.send(ServiceAction::Run {
                        label: "autostart",
                        f,
                    });
                }
            },
        );
    }

    /// In/out levels, DSP load, and a clip flash, drawn with separators matching
    /// the rest of the toolbar. The output device is shown by the picker, so this
    /// is levels only. Reading order: I │ O │ DSP │ CLIP.
    fn meters_widget(&self, ui: &mut egui::Ui, s: &DaemonState) {
        let m = &s.meters;
        // Fixed-width, monospace readouts so values change without nudging the layout.
        let db = |lin: f32| {
            let s = if lin <= 1e-6 {
                "-inf".to_string()
            } else {
                format!("{:+.0}", 20.0 * lin.log10())
            };
            format!("{s:>4}")
        };
        let clip_active = self.clip_until.map(|t| Instant::now() < t).unwrap_or(false);
        let lvl_color = if clip_active {
            egui::Color32::from_rgb(230, 60, 60)
        } else {
            egui::Color32::from_rgb(90, 200, 120)
        };
        let dsp_color = if m.dsp_load > 0.8 {
            egui::Color32::from_rgb(230, 60, 60)
        } else if m.dsp_load > 0.5 {
            egui::Color32::from_rgb(220, 200, 80)
        } else {
            egui::Color32::GRAY
        };

        ui.colored_label(
            lvl_color,
            egui::RichText::new(format!("I {} dB", db(m.in_peak))).monospace(),
        );
        ui.separator();
        ui.colored_label(
            lvl_color,
            egui::RichText::new(format!("O {} dB", db(m.out_peak))).monospace(),
        );
        ui.separator();
        ui.colored_label(
            dsp_color,
            egui::RichText::new(format!("DSP {:>3.0}%", m.dsp_load * 100.0)).monospace(),
        );
        ui.separator();
        // Always draw the clip slot so it flips OK→CLIP in place without shifting.
        if clip_active {
            ui.colored_label(
                egui::Color32::from_rgb(230, 60, 60),
                egui::RichText::new("CLIP").monospace(),
            );
        } else {
            ui.colored_label(egui::Color32::GRAY, egui::RichText::new(" OK ").monospace());
        }
    }
}
