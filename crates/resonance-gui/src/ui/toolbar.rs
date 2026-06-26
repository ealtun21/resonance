//! The top toolbar: a single non-wrapping row that adapts to width with a
//! priority+ ("collapse into the ☰ menu") strategy instead of reflowing. As the
//! window narrows, secondary controls drop into the overflow menu in a fixed
//! order (meters → undo/redo → daemon → preamp detail → output), so the bar
//! always reads as one clean row at every width — never a jittering wrapped pile.
//!
//! Breakpoints are NOT fixed pixels: each tier's threshold is the summed,
//! *measured* width of the controls that must fit at that tier (text laid out at
//! the live font), so the bar collapses at the right point regardless of the
//! machine's font size or zoom — fixed pixels overlapped/clipped when the font
//! differed. Collapse order (widest-hungriest first to drop):
//!   meters → undo/redo → daemon → preamp detail → output
//! The far-right meters go a step further: they're shown only when the space the
//! left controls actually leave fits their measured width (see `toolbar`), so the
//! right-aligned block can never draw on top of the left controls.

use crate::app::{GuiApp, ServiceAction, ServiceFn};
use crate::browser::Browser;
use crate::state::{Dialog, SaveDialog};
use crate::theme::Theme;
use crate::ui::icons::{self, Icon};
use crate::ui::kit;
use crate::ui::widgets::ellipsize;
use eframe::egui;
use resonance_ipc::{Command, DaemonState, service};
use std::time::Instant;

/// Which secondary controls have collapsed off the bar and must be reachable
/// from the ☰ overflow menu at the current width.
#[derive(Clone, Copy)]
struct Overflow {
    preamp: bool,
    output: bool,
    history: bool,
    daemon: bool,
}

/// Width a string occupies at `font`, measured against the live fonts — so the
/// toolbar's collapse points track the real text size on any machine (different
/// default font, zoom) instead of assuming fixed pixel widths.
fn text_width(ui: &egui::Ui, font: egui::FontId, s: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(s.to_owned(), font, egui::Color32::WHITE)
        .rect
        .width()
}

impl GuiApp {
    pub(crate) fn toolbar(&mut self, ui: &mut egui::Ui) {
        let state = self.state.clone();
        // macOS reserves the far-left of the toolbar for the traffic-light buttons
        // (unified titlebar); elsewhere just a small inset. That space isn't usable
        // by the controls, so discount it from the width the collapse thresholds
        // reason about — otherwise the bar thinks it has the full width and
        // collapses too late (the macOS-only "late collapse" bug). Same value is
        // used for the actual leading space below, so the two can't drift.
        let lead = if cfg!(target_os = "macos") {
            72.0
        } else {
            kit::SP_XS
        };
        let w = ui.available_width() - lead;
        let gap = kit::SP_S;

        // Collapse points are the *measured* widths of the actual controls, summed
        // in the order they reappear as the bar widens — not fixed pixel
        // breakpoints — so a machine with larger text (or zoom) collapses the bar
        // at proportionally larger widths instead of overlapping or clipping.
        let body = egui::TextStyle::Body.resolve(ui.style());
        let kf = egui::FontId::proportional(kit::T_BODY);
        // One separator + its surrounding spacing, kept generous so a tier
        // collapses a hair early rather than letting a control spill past the edge.
        let unit = 1.0 + 2.0 * gap;
        let w_power = 66.0; // power button min width
        let w_pre_min = text_width(ui, body.clone(), "Pre") + gap + 58.0; // label + num field
        let w_pre_full = text_width(ui, body.clone(), "Preamp") + gap + 150.0 + gap + 72.0;
        let w_output = 18.0 + gap + 190.0; // speaker icon + dropdown
        let w_daemon = text_width(ui, kf.clone(), "● Daemon") + 22.0; // menu button
        let w_history = kit::CTRL_H + gap + kit::CTRL_H; // two icon buttons
        let w_help = 28.0; // ? help icon button
        let w_overflow = 28.0; // ☰ icon menu button

        // Cumulative widths required, in widen order: output → preamp-full →
        // daemon → history. Power, the compact preamp, ? and ☰ are always present.
        let base = w_power + w_pre_min + w_help + w_overflow + 4.0 * unit;
        let req_output = base + w_output + unit;
        let req_preamp_full = req_output + (w_pre_full - w_pre_min);
        let req_daemon = req_preamp_full + w_daemon + unit;
        let req_history = req_daemon + w_history + unit;

        let out_inline = w >= req_output;
        let preamp_full = w >= req_preamp_full;
        let daemon_inline = w >= req_daemon && service::manager_available();
        let history_inline = w >= req_history;

        ui.horizontal(|ui| {
            ui.set_min_height(36.0);
            ui.spacing_mut().item_spacing.x = kit::SP_S;
            // Leading inset (macOS: clears the traffic-light buttons under the
            // transparent titlebar; elsewhere a small inset). Matches the `lead`
            // discounted from the collapse-threshold width above.
            ui.add_space(lead);

            // Build the list of present groups, then draw them joined by exactly
            // one separator between consecutive groups (never leading, trailing or
            // doubled — the structural fix for the adjacent-separator bug). Power
            // and Preamp are always present; Overflow always closes the row.
            #[derive(Clone, Copy)]
            enum Grp {
                Power,
                Preamp,
                Output,
                History,
                Daemon,
                Help,
                Overflow,
            }
            let mut groups = vec![Grp::Power, Grp::Preamp];
            if out_inline {
                groups.push(Grp::Output);
            }
            if history_inline {
                groups.push(Grp::History);
            }
            if daemon_inline {
                groups.push(Grp::Daemon);
            }
            groups.push(Grp::Help);
            groups.push(Grp::Overflow);

            for (i, g) in groups.iter().enumerate() {
                if i > 0 {
                    self.tb_sep(ui);
                }
                match g {
                    Grp::Power => self.tb_power(ui, &state),
                    Grp::Preamp => self.tb_preamp(ui, &state, !preamp_full),
                    Grp::Output => self.tb_output(ui, &state),
                    Grp::History => self.tb_history(ui),
                    Grp::Daemon => self.daemon_menu(ui),
                    Grp::Help => self.tb_help(ui),
                    Grp::Overflow => self.overflow_menu(
                        ui,
                        &state,
                        Overflow {
                            preamp: false,
                            output: !out_inline,
                            history: !history_inline,
                            daemon: !daemon_inline && service::manager_available(),
                        },
                    ),
                }
            }

            // Meters pinned to the far right (informational; first to drop). Shown
            // only when the space the left controls actually left over fits them —
            // measured per-frame — so the right-aligned block never draws on top of
            // those controls (the fixed-breakpoint bug). `available_width()` here is
            // exactly the gap between the last left control and the right edge.
            if let Some(s) = &state {
                if ui.available_width() >= self.meters_min_width(ui, s) {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(kit::SP_S);
                        ui.horizontal(|ui| self.meters_widget(ui, s));
                    });
                }
            }
        });
    }

    /// A thin vertical hairline between toolbar groups, matching the kit's rule
    /// colour (egui's default separator is heavier and theme-mismatched). Also
    /// reused by the reference bar to separate its sections.
    pub(crate) fn tb_sep(&self, ui: &mut egui::Ui) {
        let line = kit::tokens(ui).line;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, 22.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 0.0, line);
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

    /// Preamp gain. `compact` shows just a draggable value chip (label "Pre");
    /// otherwise the full labelled slider + dB readout.
    fn tb_preamp(&mut self, ui: &mut egui::Ui, state: &Option<DaemonState>, compact: bool) {
        let Some(s) = state else {
            return;
        };
        let mut db = s.preamp_db;
        if compact {
            ui.label("Pre");
            if kit::num_field(
                ui,
                58.0,
                egui::Id::new("preamp_field"),
                &mut db,
                -20.0..=20.0,
                1,
                0.1,
            ) {
                self.queue_edit(Command::SetPreamp { db });
            }
        } else {
            ui.label("Preamp");
            if kit::slider(ui, 150.0, &mut db, -20.0..=20.0) {
                self.queue_edit(Command::SetPreamp { db });
            }
            kit::value_chip(ui, 72.0, &format!("{db:+.1} dB"));
        }
    }

    /// Output device picker (left-to-right: speaker icon then the combo).
    fn tb_output(&mut self, ui: &mut egui::Ui, state: &Option<DaemonState>) {
        if let Some(s) = state {
            let (r, resp) =
                ui.allocate_exact_size(egui::vec2(18.0, kit::CTRL_H), egui::Sense::hover());
            let g = egui::Rect::from_center_size(r.center(), egui::Vec2::splat(16.0));
            icons::draw(ui.painter(), Icon::Speaker, g, kit::tokens(ui).dim);
            resp.on_hover_text("Output device");
            self.output_combo(ui, s);
        }
    }

    fn output_combo(&mut self, ui: &mut egui::Ui, s: &DaemonState) {
        // When following the system, show the device it's currently on.
        // Full labels — the combo's value box soft-fades + hover-scrolls long
        // device names itself (see `kit::dropdown`), so no pre-truncation here.
        let current_label = if s.preferred_output.is_none() {
            match &s.active_output {
                Some(d) => format!("Auto · {}", s.sink_label(d)),
                None => "Automatic".to_string(),
            }
        } else {
            s.sink_label(s.preferred_output.as_deref().unwrap_or(""))
        };
        // Index 0 = follow the OS default; the rest map 1:1 to available_sinks.
        let mut opts = vec!["Automatic (follow system)".to_string()];
        opts.extend(s.available_sinks.iter().map(|sink| s.sink_label(sink)));
        let opt_refs: Vec<&str> = opts.iter().map(String::as_str).collect();
        if let Some(sel) = kit::dropdown(
            ui,
            190.0,
            kit::CTRL_H,
            egui::Id::new("toolbar_sink"),
            &current_label,
            &opt_refs,
        ) {
            if sel == 0 {
                self.queue(Command::FollowSystemOutput);
            } else {
                let node = s.available_sinks[sel - 1].clone();
                self.queue(Command::SetOutputTarget { node_name: node });
            }
        }
    }

    /// Output device list as menu rows (for the ☰ overflow when the inline combo
    /// has collapsed). The active choice is marked.
    fn output_menu_items(&mut self, ui: &mut egui::Ui, s: &DaemonState) {
        if kit::menu_item(
            ui,
            "Automatic (follow system)",
            s.preferred_output.is_none(),
        ) {
            self.queue(Command::FollowSystemOutput);
        }
        for sink in &s.available_sinks {
            let checked = s.preferred_output.as_deref() == Some(sink.as_str());
            let label = ellipsize(&s.sink_label(sink), 30);
            if kit::menu_item(ui, &label, checked) {
                self.queue(Command::SetOutputTarget {
                    node_name: sink.clone(),
                });
            }
        }
    }

    /// Help button — opens the controls & shortcuts overlay (same as F1 / ?).
    fn tb_help(&mut self, ui: &mut egui::Ui) {
        if kit::icon_btn(ui, Icon::Help, kit::CTRL_H, "Controls & shortcuts (F1)") {
            self.show_help = true;
        }
    }

    fn tb_history(&mut self, ui: &mut egui::Ui) {
        if kit::icon_btn_enabled(
            ui,
            Icon::Undo,
            kit::CTRL_H,
            !self.undo_stack.is_empty(),
            "Undo (Ctrl+Z)",
        ) {
            self.undo();
        }
        if kit::icon_btn_enabled(
            ui,
            Icon::Redo,
            kit::CTRL_H,
            !self.redo_stack.is_empty(),
            "Redo (Ctrl+Y)",
        ) {
            self.redo();
        }
    }

    /// Overflow menu (☰): hosts whatever has collapsed off the bar at the current
    /// width, plus the always-present preset/view/theme actions — so nothing is
    /// ever unreachable on a small window.
    fn overflow_menu(&mut self, ui: &mut egui::Ui, state: &Option<DaemonState>, of: Overflow) {
        kit::icon_menu_button(
            ui,
            Icon::Menu,
            egui::Id::new("overflow_pop"),
            true,
            "Menu",
            |ui| {
                ui.set_min_width(220.0);
                ui.spacing_mut().item_spacing.y = 2.0;

                // Collapsed controls first (only those hidden from the bar).
                if of.output {
                    if let Some(s) = state {
                        kit::menu_caption(ui, "Output device");
                        self.output_menu_items(ui, s);
                    }
                }
                if of.history {
                    kit::menu_caption(ui, "Edit");
                    if kit::menu_item(ui, "Undo", false) {
                        self.undo();
                    }
                    if kit::menu_item(ui, "Redo", false) {
                        self.redo();
                    }
                }
                if of.daemon {
                    kit::menu_caption(ui, "Daemon");
                    self.daemon_controls(ui);
                }
                let _ = of.preamp; // preamp never collapses fully (compact stays inline)

                kit::menu_caption(ui, "Presets");
                if kit::menu_item(ui, "Load preset…", false) {
                    self.open_load_dialog();
                }
                if kit::menu_item(ui, "Export profile…", false) {
                    self.open_export_dialog();
                }

                kit::menu_caption(ui, "View");
                if kit::menu_item(ui, "Reset layout", false) {
                    self.reset_layout(ui.ctx());
                }

                kit::menu_caption(ui, "Theme");
                let ctx = ui.ctx().clone();
                for t in Theme::ALL {
                    if kit::menu_item(ui, t.label(), self.theme == t) {
                        self.set_theme(&ctx, t);
                    }
                }
            },
        );
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
    /// menu with a status dot, so users never type a `systemctl` line.
    fn daemon_menu(&mut self, ui: &mut egui::Ui) {
        let st = self.daemon_status;
        let busy = self.service_busy;
        let (dot, color) = if busy {
            ("…", self.palette.boost)
        } else if st.active {
            ("●", self.palette.boost)
        } else {
            ("○", self.palette.cut)
        };
        kit::menu_button(
            ui,
            &format!("{dot} Daemon"),
            color,
            egui::Id::new("daemon_pop"),
            |ui| {
                ui.set_min_width(190.0);
                self.daemon_controls(ui);
            },
        );
    }

    /// The shared daemon-control body (status line, Start/Stop/Restart, autostart
    /// toggle), drawn with the kit so it looks identical whether it appears in the
    /// inline daemon menu or the ☰ overflow. All ops dispatch to a worker thread
    /// so the UI never blocks on launchctl/systemctl.
    fn daemon_controls(&mut self, ui: &mut egui::Ui) {
        let st = self.daemon_status;
        let busy = self.service_busy;
        let dim = kit::tokens(ui).dim;
        let status = if busy {
            "working…".to_string()
        } else {
            format!(
                "{} · autostart {}",
                if st.active { "running" } else { "stopped" },
                if st.enabled { "on" } else { "off" },
            )
        };
        ui.label(egui::RichText::new(status).size(kit::T_CAPTION).color(dim));
        ui.add_space(kit::SP_XS);

        let actions: [(&str, ServiceFn); 3] = [
            ("Start", service::start),
            ("Stop", service::stop),
            ("Restart", service::restart),
        ];
        for (label, f) in actions {
            if kit::menu_item(ui, label, false) && !busy {
                self.service_busy = true;
                let _ = self.service_tx.send(ServiceAction::Run { label, f });
            }
        }

        ui.add_space(kit::SP_XS);
        ui.horizontal(|ui| {
            let mut autostart = st.enabled;
            if kit::toggle(ui, &mut autostart) && !busy {
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
            let text = kit::tokens(ui).text;
            ui.label(
                egui::RichText::new("Autostart at login")
                    .size(kit::T_BODY)
                    .color(text),
            );
        });
    }

    /// In/out levels, DSP load, and a clip flash. Drawn inside the toolbar's
    /// right-to-left (right-pinned) sub-layout, so the readouts are added in
    /// REVERSE and the separators trail each — giving a left-to-right reading
    /// order of I │ O │ DSP │ CLIP against the right edge.
    /// The four meter segments `(colour, text)`. Each text is fixed-width
    /// (monospace, padded) so values change in place without nudging the layout —
    /// and so [`meters_min_width`](Self::meters_min_width) can measure the block
    /// regardless of the current levels. Shared by the renderer and the fit check.
    fn meters_items(&self, s: &DaemonState) -> Vec<(egui::Color32, String)> {
        let m = &s.meters;
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
        // Clip slot is always drawn so it flips OK→CLIP in place without shifting.
        let (clip_col, clip_txt) = if clip_active {
            (egui::Color32::from_rgb(230, 60, 60), "CLIP")
        } else {
            (egui::Color32::GRAY, " OK ")
        };
        // Sample-rate segment (leftmost). Shows the live DSP/playback rate; when a
        // backend is resampling (capture rate ≠ DSP rate) it reads "in→out" in
        // amber so the conversion is visible at a glance.
        let khz = |hz: f64| {
            if ((hz / 1000.0).fract()).abs() < 0.05 {
                format!("{:.0}k", hz / 1000.0)
            } else {
                format!("{:.1}k", hz / 1000.0)
            }
        };
        let resampling = (s.capture_rate - s.sample_rate).abs() > 1.0;
        let (rate_col, rate_txt) = if resampling {
            (
                egui::Color32::from_rgb(220, 200, 80),
                format!("{}→{}", khz(s.capture_rate), khz(s.sample_rate)),
            )
        } else {
            (egui::Color32::GRAY, khz(s.sample_rate))
        };
        vec![
            (rate_col, rate_txt),
            (lvl_color, format!("I {} dB", db(m.in_peak))),
            (lvl_color, format!("O {} dB", db(m.out_peak))),
            (dsp_color, format!("DSP {:>3.0}%", m.dsp_load * 100.0)),
            (clip_col, clip_txt.to_string()),
        ]
    }

    /// Width the inline meters need. We only draw them when the left controls
    /// leave at least this much room (see `toolbar`), so the right-aligned block
    /// can never overlap them — fixing the old fixed-breakpoint collision. The
    /// galley widths are exact; the separator/spacing terms over-estimate so the
    /// meters collapse a hair *before* they'd touch, never after.
    fn meters_min_width(&self, ui: &egui::Ui, s: &DaemonState) -> f32 {
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let text: f32 = self
            .meters_items(s)
            .iter()
            .map(|(_, t)| {
                ui.painter()
                    .layout_no_wrap(t.clone(), font.clone(), egui::Color32::WHITE)
                    .rect
                    .width()
            })
            .sum();
        let gap = ui.spacing().item_spacing.x;
        // N labels + (N−1) separators ⇒ (2N−1) widgets / (2N−2) gaps, plus the
        // leading RTL pad. Derived from the item count so adding a segment (e.g.
        // the sample-rate readout) keeps the fit check correct.
        let n = self.meters_items(s).len() as f32;
        text + (n - 1.0) * 8.0 + (2.0 * n - 1.0) * gap + kit::SP_M
    }

    fn meters_widget(&self, ui: &mut egui::Ui, s: &DaemonState) {
        let items = self.meters_items(s);
        // RTL: add reversed (CLIP first → rightmost), a hairline between each —
        // the same rule the toolbar groups use, so the bar has one separator style.
        for (i, (col, txt)) in items.iter().enumerate().rev() {
            ui.colored_label(*col, egui::RichText::new(txt).monospace());
            if i != 0 {
                self.tb_sep(ui);
            }
        }
    }
}
