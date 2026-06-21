//! The top toolbar: power, preamp, load/export, output picker, undo/redo,
//! daemon + theme menus, reset, and the level/DSP meters. Reflows from one row
//! to a measured two-row column grid as the window narrows.

use crate::app::{GuiApp, ServiceAction, ServiceFn};
use crate::browser::Browser;
use crate::state::{Dialog, SaveDialog};
use crate::theme::Theme;
use crate::ui::widgets::ellipsize;
use eframe::egui;
use resonance_ipc::{Command, DaemonState, service};
use std::time::Instant;

/// Width below which the toolbar drops from one row to the designed two-row
/// layout. Above this both control groups fit comfortably side by side.
const TOOLBAR_ONE_ROW_MIN: f32 = 1320.0;

/// Two-row toolbar column grid. Each column is a fixed width so its contents
/// centre inside it and the separators between columns form continuous
/// full-height dividers. `TB_ON_W` spans both rows; the rest stack two cells of
/// `TB_ROW_H`.
const TB_ON_W: f32 = 80.0; // power button (spans both rows)
const TB_UNDO_W: f32 = 60.0; // undo / redo stacked
const TB_MID_W: f32 = 290.0; // preamp / daemon+theme+settings (max, elastic)
const TB_MID_MIN: f32 = 150.0; // mid column floor before meters drop
const TB_AUX_W: f32 = 132.0; // load+export / reset
const TB_TAIL_W: f32 = 285.0; // output / meters (right-pushed)
const TB_EDGE_PAD: f32 = 8.0; // gap between window edge and first toolbar cell
const TB_ROW_H: f32 = 26.0;
const TB_FULL_H: f32 = TB_ROW_H * 2.0;

impl GuiApp {
    pub(crate) fn toolbar(&mut self, ui: &mut egui::Ui) {
        let state = self.state.clone();
        let avail = ui.available_width();
        // One row when everything fits side by side; otherwise the designed
        // two-row column grid. The two-row grid is *elastic*: the preamp slider
        // and its column shrink, and the meters drop, as the window narrows, so
        // it keeps fitting (no clipping, no wrapping) down to a small floor — all
        // driven by measured widths, not hardcoded breakpoints.
        if avail >= TOOLBAR_ONE_ROW_MIN {
            ui.horizontal(|ui| {
                ui.add_space(TB_EDGE_PAD);
                self.tb_power(ui, &state);
                ui.separator();
                self.tb_preamp(ui, &state, 170.0);
                ui.separator();
                self.tb_load_export(ui, &state);
                ui.separator();
                self.tb_output(ui, &state);
                ui.separator();
                self.tb_history(ui);
                ui.separator();
                self.daemon_menu(ui);
                ui.separator();
                self.theme_menu(ui);
                self.tb_reset(ui);
                ui.separator();
                if let Some(s) = &state {
                    self.meters_widget(ui, s);
                }
                self.tb_status(ui);
            });
            return;
        }

        // Last frame's measured tail widths: full = output+meters, out = output
        // only (used to decide whether meters still fit, and to size the spacer).
        let tail_full = ui
            .ctx()
            .data(|d| d.get_temp::<f32>(egui::Id::new("tb_tail_full")))
            .unwrap_or(TB_TAIL_W);
        let tail_out = ui
            .ctx()
            .data(|d| d.get_temp::<f32>(egui::Id::new("tb_tail_out")))
            .unwrap_or(120.0);
        // Non-elastic left width: ON + Undo/Redo + Load/Reset columns + the four
        // separators (+ a little slack). The mid (preamp/menus) column absorbs
        // the rest down to a floor; below that, the meters drop.
        let fixed_left = TB_EDGE_PAD + TB_ON_W + TB_UNDO_W + TB_AUX_W + 70.0;
        let show_meters = avail >= fixed_left + TB_MID_MIN + tail_full + 8.0;
        let tail_target = if show_meters { tail_full } else { tail_out };
        let mid_w = (avail - fixed_left - tail_target - 8.0).clamp(TB_MID_MIN, TB_MID_W);
        // Preamp slider fills the mid column minus its label + value box.
        let slider_w = (mid_w - 116.0).clamp(56.0, 170.0);

        ui.horizontal(|ui| {
            ui.set_min_height(TB_FULL_H);
            let x0 = ui.min_rect().min.x;
            ui.add_space(TB_EDGE_PAD);

            // ON — one tall cell spanning both rows.
            tb_cell(ui, "on", TB_ON_W, TB_FULL_H, |ui| self.tb_power(ui, &state));
            ui.separator();

            // Undo (top) / Redo (bottom).
            tb_column(ui, |ui| {
                tb_cell(ui, "undo", TB_UNDO_W, TB_ROW_H, |ui| {
                    if ui
                        .add_enabled(!self.undo_stack.is_empty(), egui::Button::new("Undo"))
                        .clicked()
                    {
                        self.undo();
                    }
                });
                tb_cell(ui, "redo", TB_UNDO_W, TB_ROW_H, |ui| {
                    if ui
                        .add_enabled(!self.redo_stack.is_empty(), egui::Button::new("Redo"))
                        .clicked()
                    {
                        self.redo();
                    }
                });
            });
            ui.separator();

            // Preamp (top) / daemon + theme + settings menus (bottom). Elastic.
            tb_column(ui, |ui| {
                tb_cell(ui, "preamp", mid_w, TB_ROW_H, |ui| {
                    self.tb_preamp(ui, &state, slider_w)
                });
                tb_cell(ui, "menus", mid_w, TB_ROW_H, |ui| {
                    self.daemon_menu(ui);
                    self.theme_menu(ui);
                });
            });
            ui.separator();

            // Load/Export (top) / Reset layout (bottom).
            tb_column(ui, |ui| {
                tb_cell(ui, "loadexp", TB_AUX_W, TB_ROW_H, |ui| {
                    self.tb_load_export(ui, &state)
                });
                tb_cell(ui, "reset", TB_AUX_W, TB_ROW_H, |ui| self.tb_reset(ui));
            });
            ui.separator();

            let left_used = ui.cursor().min.x - x0;

            // Trailing column: output (top) / meters+status (bottom). Meters are
            // shown only when they fit; pushed right by a measured spacer.
            let space = (ui.available_width() - tail_target).max(0.0);
            ui.add_space(space);
            let (out_w, full_w) = ui
                .vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let w1 = ui
                        .horizontal(|ui| {
                            ui.set_min_height(TB_ROW_H);
                            self.tb_output(ui, &state);
                        })
                        .response
                        .rect
                        .width();
                    let w2 = ui
                        .horizontal(|ui| {
                            ui.set_min_height(TB_ROW_H);
                            if show_meters {
                                if let Some(s) = &state {
                                    self.meters_widget(ui, s);
                                }
                                if !self.status.is_empty() {
                                    ui.separator();
                                    ui.label(&self.status);
                                }
                            }
                        })
                        .response
                        .rect
                        .width();
                    (w1, w1.max(w2))
                })
                .inner;
            ui.ctx()
                .data_mut(|d| d.insert_temp(egui::Id::new("tb_tail_out"), out_w));
            // Only record the full (with-meters) width while meters are shown,
            // else the stored value collapses and the decision oscillates.
            if show_meters {
                ui.ctx()
                    .data_mut(|d| d.insert_temp(egui::Id::new("tb_tail_full"), full_w));
            }
            // Drives the dynamic min width (honoured by floating WMs).
            self.tb_required_w = left_used + tail_target;
        });
    }

    /// Prominent power toggle: a large filled green/red button, not a tiny
    /// checkbox. The app title lives in the custom titlebar, not here.
    fn tb_power(&mut self, ui: &mut egui::Ui, state: &Option<DaemonState>) {
        let enabled = state.as_ref().map(|s| s.enabled).unwrap_or(false);
        let (txt, fill) = if enabled {
            ("ON", self.palette.boost)
        } else {
            ("OFF", self.palette.cut)
        };
        // A transparent default-font label sizes the button exactly like the
        // other toolbar buttons (same text style + button padding → same
        // height); we don't force a min height. The visible status dot and
        // label are painted as one centred group on top (the font's ● glyph
        // sits off the vertical centre, so we draw our own dot). Vertical
        // centring within the double-row cell is handled by `tb_cell`.
        let font = egui::TextStyle::Button.resolve(ui.style());
        // Squat button: trim vertical padding so it's shorter than a default
        // button, but keep a width floor so it stays wide.
        ui.spacing_mut().button_padding.y = 1.0;
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

    fn tb_preamp(&mut self, ui: &mut egui::Ui, state: &Option<DaemonState>, slider_w: f32) {
        ui.label("Preamp");
        // Slider rail width is supplied so the two-row layout can shrink it as
        // the window narrows (one row passes its full 170).
        ui.spacing_mut().slider_width = slider_w;
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

    fn tb_load_export(&mut self, ui: &mut egui::Ui, state: &Option<DaemonState>) {
        if ui
            .button("Load…")
            .on_hover_text("import a .fac / APO .txt / Resonance .toml file")
            .clicked()
        {
            let lib = resonance_ipc::paths::user_preset_dir();
            let _ = std::fs::create_dir_all(&lib);
            let start = if lib.is_dir() {
                lib
            } else {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
            };
            self.dialog = Dialog::LoadPreset(Browser::new(start, false));
        }
        if ui
            .button("Export…")
            .on_hover_text("save the current chain as a Resonance .toml profile")
            .clicked()
        {
            let lib = resonance_ipc::paths::user_preset_dir();
            let _ = std::fs::create_dir_all(&lib);
            let stem = state
                .as_ref()
                .and_then(|s| s.current_preset.clone())
                .unwrap_or_else(|| "resonance".to_string());
            self.dialog = Dialog::ExportProfile(SaveDialog {
                browser: Browser::new(lib, true),
                filename: stem,
            });
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

    fn tb_reset(&mut self, ui: &mut egui::Ui) {
        if ui
            .button("Reset layout")
            .on_hover_text("restore default panel sizes")
            .clicked()
        {
            self.reset_layout(ui.ctx());
        }
    }

    fn tb_status(&mut self, ui: &mut egui::Ui) {
        if !self.status.is_empty() {
            ui.separator();
            ui.label(&self.status);
        }
    }

    /// Daemon lifecycle controls (systemd user service) as a compact menu so
    /// users never type a `systemctl` line. All ops dispatch to a worker
    /// thread so the UI never blocks on launchctl / systemctl latency.
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

    /// Theme picker combo box.
    fn theme_menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut sel = self.theme;
        egui::ComboBox::from_id_salt("theme")
            .selected_text(ellipsize(self.theme.label(), 16))
            .width(120.0)
            .show_ui(ui, |ui| {
                for t in Theme::ALL {
                    ui.selectable_value(&mut sel, t, t.label());
                }
            });
        if sel != self.theme {
            self.set_theme(&ctx, sel);
        }
    }

    /// Output device, in/out levels, DSP load, and a clip flash, drawn
    /// right-aligned in the bar with separators matching the rest of the toolbar.
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

        // Levels only — the output device is shown by the toolbar selector.
        // Reading order: I │ O │ DSP │ CLIP.
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

/// A two-row toolbar column: stack its cells with no vertical gap so the column
/// is exactly `TB_FULL_H` tall and the separators on either side stay flush.
fn tb_column(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        add(ui);
    });
}

/// One fixed-size toolbar cell with its contents centred both ways. A top-down
/// `Align::Center` layout centres the content row horizontally for free; the
/// vertical centring pads from *last frame's* measured content height (kept in
/// egui memory, like `centered`) to avoid a layout feedback loop. Identical cell
/// sizes across both rows keep every column — and the separators between them —
/// aligned.
fn tb_cell(ui: &mut egui::Ui, id: &str, w: f32, h: f32, add: impl FnOnce(&mut egui::Ui)) {
    let key = egui::Id::new(("tb_cell", id));
    let prev_h = ui.ctx().data(|d| d.get_temp::<f32>(key)).unwrap_or(0.0);
    let pad = ((h - prev_h) * 0.5).max(0.0);
    let out = ui.allocate_ui_with_layout(
        egui::vec2(w, h),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            ui.set_min_size(egui::vec2(w, h));
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(pad);
            ui.horizontal(|ui| add(ui)).response.rect.height()
        },
    );
    ui.ctx().data_mut(|d| d.insert_temp(key, out.inner));
}
