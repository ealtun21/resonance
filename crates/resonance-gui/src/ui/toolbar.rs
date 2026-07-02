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
use crate::ui::icons::{self, Icon};
use crate::ui::kit;
use crate::ui::widgets::{ellipsize, lerp_color};
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
        // Build the list of present groups, then draw them joined by exactly
        // one separator between consecutive groups (never leading, trailing or
        // doubled — the structural fix for the adjacent-separator bug). Power
        // and Preamp are always present; ⚙/?/☰ live in a right-aligned cluster
        // drawn separately below.
        #[derive(Clone, Copy)]
        enum Grp {
            Power,
            Preamp,
            Output,
            History,
            Daemon,
        }
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
        let w = ui.available_width() - kit::SP_XS;
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
        let w_output = 18.0 + gap + 200.0; // speaker icon + 2-line dropdown
        let w_daemon = text_width(ui, kf.clone(), "● Daemon") + 22.0; // menu button
        let w_history = kit::CTRL_H + gap + kit::CTRL_H; // two icon buttons
        let w_settings = 28.0; // ⚙ settings icon button
        let w_help = 28.0; // ? help icon button
        let w_overflow = 28.0; // ☰ icon menu button

        // Cumulative widths required, in widen order: output → preamp-full →
        // daemon → history. Power, the compact preamp, ? and ☰ are always present.
        // The ⚙/?/☰ cluster is anchored to the right edge, so a tier that merely
        // *fits* can leave zero daylight between the clusters (the estimates
        // above under-measure the painter-drawn controls by a few px, which used
        // to spill harmlessly past the trailing margin when the icons were
        // left-packed). `min_flex` demands that daylight on top of the summed
        // widths, so every tier collapses while the clusters are still apart.
        let min_flex = 2.0 * unit;
        let base = w_power + w_pre_min + w_settings + w_help + w_overflow + 5.0 * unit + min_flex;
        let req_output = base + w_output + unit;
        let req_preamp_full = req_output + (w_pre_full - w_pre_min);
        let req_daemon = req_preamp_full + w_daemon + unit;
        let req_history = req_daemon + w_history + unit;

        let out_inline = w >= req_output;
        let preamp_full = w >= req_preamp_full;
        let daemon_inline = w >= req_daemon && service::manager_available();
        let history_inline = w >= req_history;

        // ── Single controls row ────────────────────────────────────────────
        // The brand/meters row was removed: the native OS title bar already names
        // the window (no CSD), so a second app-drawn "title bar" was redundant —
        // it read as two stacked title bars. Identity lives in the OS chrome;
        // live meters moved to the bottom status strip (see `status_bar`). The
        // macOS traffic-light inset (`lead`) now sits on this row so the controls
        // clear the floating window buttons of the unified title bar.
        ui.add_space(kit::SP_XS);
        ui.horizontal(|ui| {
            ui.set_min_height(36.0);
            ui.spacing_mut().item_spacing.x = kit::SP_S;
            ui.add_space(lead);

            // Preamp + Output draw nothing without a connected daemon — omit them
            // (not just their content) so no stranded separators are left behind
            // on the disconnected toolbar.
            let connected = state.is_some();
            let mut groups = vec![Grp::Power];
            if connected {
                groups.push(Grp::Preamp);
            }
            if connected && out_inline {
                groups.push(Grp::Output);
            }
            if history_inline {
                groups.push(Grp::History);
            }
            if daemon_inline {
                groups.push(Grp::Daemon);
            }

            for (i, g) in groups.iter().enumerate() {
                if i > 0 {
                    Self::tb_sep(ui);
                }
                match g {
                    Grp::Power => self.tb_power(ui, state.as_ref()),
                    Grp::Preamp => self.tb_preamp(ui, state.as_ref(), !preamp_full),
                    Grp::Output => self.tb_output(ui, state.as_ref()),
                    Grp::History => self.tb_history(ui),
                    Grp::Daemon => self.daemon_menu(ui),
                }
            }

            // ⚙ / ? / ☰ anchor to the right edge, separated from the left
            // cluster by the flexible gap. Right-to-left layout, so they're
            // drawn outermost-first: ☰ hugs the edge, then ?, then ⚙.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(kit::SP_XS);
                self.overflow_menu(
                    ui,
                    state.as_ref(),
                    Overflow {
                        preamp: false,
                        output: !out_inline,
                        history: !history_inline,
                        daemon: !daemon_inline && service::manager_available(),
                    },
                );
                Self::tb_sep(ui);
                self.tb_help(ui);
                Self::tb_sep(ui);
                self.tb_settings(ui);
            });
        });
        // Bottom hairline so the toolbar reads as a distinct header band over the
        // body (mockup `.toolbar` border-bottom), instead of bleeding into the
        // graph card below it.
        ui.add_space(kit::SP_XS);
        let line = kit::tokens(ui).line;
        let (r, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
        ui.painter()
            .hline(r.x_range(), r.center().y, egui::Stroke::new(1.0, line));
    }

    /// A thin vertical hairline between toolbar groups, matching the kit's rule
    /// colour (egui's default separator is heavier and theme-mismatched). Also
    /// reused by the reference bar to separate its sections.
    pub(crate) fn tb_sep(ui: &mut egui::Ui) {
        let line = kit::tokens(ui).line;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, 22.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 0.0, line);
    }

    /// Prominent power toggle (mockup `.power`): a translucent, accent-bordered
    /// pill with a status dot, a coloured ON/OFF, and a dim "POWER" sub-label —
    /// fully painter-drawn, not an `egui::Button`. Green when on, red when off.
    fn tb_power(&mut self, ui: &mut egui::Ui, state: Option<&DaemonState>) {
        const PAD_L: f32 = 12.0;
        const PAD_R: f32 = 14.0;
        const DOT_D: f32 = 9.0;
        const GAP: f32 = 8.0;
        const SUB_GAP: f32 = 7.0;
        let connected = state.is_some();
        let on = state.is_some_and(|s| s.enabled);
        let t = kit::tokens(ui);
        let color = if !connected {
            t.dim
        } else if on {
            self.palette.boost
        } else {
            self.palette.cut
        };
        let label = if on { "ON" } else { "OFF" };

        let lab_font = egui::FontId::proportional(13.5);
        let sub_font = egui::FontId::proportional(kit::T_CAPTION);
        let lab_w = text_width(ui, lab_font.clone(), label);
        let sub_w = text_width(ui, sub_font.clone(), "POWER");
        let w = PAD_L + DOT_D + GAP + lab_w + SUB_GAP + sub_w + PAD_R;
        let h = 30.0;
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());

        let hover = resp.hovered() && connected;
        let bg = lerp_color(t.well, color, if hover { 0.24 } else { 0.13 });
        let border = lerp_color(t.well, color, 0.45);
        let p = ui.painter();
        let radius = kit::R_CTRL + 2.0;
        p.rect_filled(rect, radius, bg);
        p.rect_stroke(
            rect,
            radius,
            egui::Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        );
        let cy = rect.center().y;
        let dot_c = egui::pos2(rect.left() + PAD_L + DOT_D * 0.5, cy);
        if on {
            p.circle_filled(dot_c, DOT_D * 0.5, color);
        } else {
            p.circle_stroke(dot_c, DOT_D * 0.5 - 0.5, egui::Stroke::new(1.5, color));
        }
        let lx = rect.left() + PAD_L + DOT_D + GAP;
        p.text(
            egui::pos2(lx, cy),
            egui::Align2::LEFT_CENTER,
            label,
            lab_font,
            color,
        );
        p.text(
            egui::pos2(lx + lab_w + SUB_GAP, cy),
            egui::Align2::LEFT_CENTER,
            "POWER",
            sub_font,
            t.dim,
        );
        let resp = resp.on_hover_text("toggle DSP power");
        if resp.clicked() && connected {
            self.queue_edit(Command::SetPower { enabled: !on });
        }
    }

    /// Preamp gain. `compact` shows just a draggable value chip (label "Pre");
    /// otherwise the full labelled slider + dB readout.
    fn tb_preamp(&mut self, ui: &mut egui::Ui, state: Option<&DaemonState>, compact: bool) {
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

    /// Output device picker (left-to-right: speaker icon then a 2-line combo
    /// showing the friendly device name over its node id, like the mockup
    /// `.device` chip).
    fn tb_output(&mut self, ui: &mut egui::Ui, state: Option<&DaemonState>) {
        if let Some(s) = state {
            let (r, resp) = ui.allocate_exact_size(egui::vec2(18.0, 32.0), egui::Sense::hover());
            let g = egui::Rect::from_center_size(r.center(), egui::Vec2::splat(16.0));
            icons::draw(ui.painter(), Icon::Speaker, g, kit::tokens(ui).dim);
            resp.on_hover_text("Output device");
            self.output_combo(ui, s);
        }
    }

    fn output_combo(&mut self, ui: &mut egui::Ui, s: &DaemonState) {
        // Following the system shows the device it's currently on (with an "auto"
        // tag on the node line); a pinned device shows itself. The chip soft-fades
        // + hover-scrolls long names itself, so no pre-truncation here.
        let following = s.preferred_output.is_none();
        let node = if following {
            s.active_output.clone()
        } else {
            s.preferred_output.clone()
        };
        let (line1, line2) = match &node {
            Some(n) => {
                let id = if following {
                    format!("auto · {n}")
                } else {
                    n.clone()
                };
                (s.sink_label(n), id)
            }
            None => ("Automatic".to_string(), "follow system".to_string()),
        };
        // Index 0 = follow the OS default; the rest map 1:1 to available_sinks.
        let mut opts = vec!["Automatic (follow system)".to_string()];
        opts.extend(s.available_sinks.iter().map(|sink| s.sink_label(sink)));
        let opt_refs: Vec<&str> = opts.iter().map(String::as_str).collect();
        if let Some(sel) = kit::dropdown_2line(
            ui,
            200.0,
            32.0,
            egui::Id::new("toolbar_sink"),
            &line1,
            &line2,
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

    fn tb_settings(&mut self, ui: &mut egui::Ui) {
        if kit::icon_btn(ui, Icon::Gear, kit::CTRL_H, "Settings") {
            self.dialog = Dialog::Settings;
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
    fn overflow_menu(&mut self, ui: &mut egui::Ui, state: Option<&DaemonState>, of: Overflow) {
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
                let editing = self.layout_edit;
                if kit::menu_item(ui, "Edit layout", editing) {
                    self.layout_edit = !editing;
                }
                // Theme moved to the Settings dialog (gear icon).
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
            source: None,
        });
    }

    /// Export a *specific* stored profile (the per-row Export action), rather than
    /// the current chain. Seeds the filename + source with the profile name.
    pub(crate) fn open_export_dialog_for(&mut self, name: String) {
        let lib = resonance_ipc::paths::user_preset_dir();
        let _ = std::fs::create_dir_all(&lib);
        self.dialog = Dialog::ExportProfile(SaveDialog {
            browser: Browser::new(lib, true),
            filename: name.clone(),
            source: Some(name),
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

    /// Bottom status strip: backend/format on the left, live level + DSP meters,
    /// and an OK/CLIP lamp pinned to the right. Replaces the old brand-row meters
    /// — a status strip reads as chrome, not a second title bar. Painter-drawn so
    /// the segment separators match the toolbar's hairline rule.
    pub(crate) fn status_bar(&mut self, ui: &mut egui::Ui) {
        let Some(s) = self.state.clone() else {
            return;
        };
        let t = kit::tokens(ui);
        // Top hairline so the strip reads as a distinct footer surface.
        let (line_r, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
        ui.painter().hline(
            line_r.x_range(),
            line_r.top(),
            egui::Stroke::new(1.0, t.line),
        );

        let mono = egui::FontId::monospace(kit::T_CAPTION);
        let prop = egui::FontId::proportional(kit::T_CAPTION);
        ui.horizontal(|ui| {
            ui.set_min_height(22.0);
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(kit::CARD_PAD_X);

            // One left-anchored segment: a dim label then a bright value, with a
            // hairline divider trailing it (skipped on the last).
            let seg = |ui: &mut egui::Ui, label: &str, value: &str, vcol: egui::Color32| {
                let lab_w = if label.is_empty() {
                    0.0
                } else {
                    kit::text_width(ui, kit::T_CAPTION, label) + 5.0
                };
                // Measure the value in the SAME monospace font it's drawn in, so a
                // fixed-width (padded) value yields a constant segment width — the
                // meters then update in place instead of nudging the segments after
                // them as the digits change.
                let val_w = ui
                    .painter()
                    .layout_no_wrap(value.to_owned(), mono.clone(), egui::Color32::WHITE)
                    .rect
                    .width();
                let w = lab_w + val_w;
                let (r, _) = ui.allocate_exact_size(egui::vec2(w, 22.0), egui::Sense::hover());
                let p = ui.painter();
                let dim = kit::tokens(ui).dim;
                if !label.is_empty() {
                    p.text(
                        egui::pos2(r.left(), r.center().y),
                        egui::Align2::LEFT_CENTER,
                        label,
                        prop.clone(),
                        dim,
                    );
                }
                p.text(
                    egui::pos2(r.left() + lab_w, r.center().y),
                    egui::Align2::LEFT_CENTER,
                    value,
                    mono.clone(),
                    vcol,
                );
            };

            let m = &s.meters;
            let db = |lin: f32| {
                if lin <= 1e-6 {
                    "-inf".to_string()
                } else {
                    format!("{:+.0}", 20.0 * lin.log10())
                }
            };
            let khz = |hz: f64| format!("{:.1}k", hz / 1000.0);
            let clip_active = self.clip_until.is_some_and(|t| Instant::now() < t);
            let lvl_col = if clip_active {
                self.palette.cut
            } else {
                t.text
            };
            let resampling = (s.capture_rate - s.sample_rate).abs() > 1.0;
            let rate = if resampling {
                format!("{}→{}", khz(s.capture_rate), khz(s.sample_rate))
            } else {
                khz(s.sample_rate)
            };

            let mut segs: Vec<(String, String, egui::Color32)> = vec![
                (format!("{} · ", backend_label()), rate, t.text),
                (String::new(), format!("{} ch", s.channels.max(1)), t.dim),
                ("in ".into(), format!("{:>4} dB", db(m.in_peak)), lvl_col),
                ("out ".into(), format!("{:>4} dB", db(m.out_peak)), lvl_col),
                (
                    "dsp ".into(),
                    format!("{:>3.0}%", m.dsp_load * 100.0),
                    if m.dsp_load > 0.8 {
                        self.palette.cut
                    } else if m.dsp_load > 0.5 {
                        self.palette.boost
                    } else {
                        t.dim
                    },
                ),
            ];
            // Compact hint when a hidden advanced feature holds a non-default
            // value (e.g. dither on while its section is hidden).
            if let Some(hint) = self.advanced_active_hint() {
                segs.push((String::new(), hint, self.palette.boost));
            }
            let n = segs.len();
            for (i, (label, value, vcol)) in segs.iter().enumerate() {
                seg(ui, label, value, *vcol);
                if i + 1 < n {
                    ui.add_space(kit::SP_M);
                    Self::tb_sep(ui);
                    ui.add_space(kit::SP_M);
                }
            }

            // Right-pinned OK/CLIP lamp.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(kit::CARD_PAD_X);
                let (txt, col) = if clip_active {
                    ("CLIP", self.palette.cut)
                } else {
                    ("OK", self.palette.boost)
                };
                let w = kit::text_width(ui, kit::T_CAPTION, txt);
                let (r, _) = ui.allocate_exact_size(egui::vec2(w, 22.0), egui::Sense::hover());
                ui.painter().text(
                    egui::pos2(r.right(), r.center().y),
                    egui::Align2::RIGHT_CENTER,
                    txt,
                    mono.clone(),
                    col,
                );
            });
        });
    }
}

/// Audio backend name for the status strip (compile-time per platform).
fn backend_label() -> &'static str {
    if cfg!(target_os = "windows") {
        "WASAPI"
    } else if cfg!(target_os = "macos") {
        "CoreAudio"
    } else {
        "PipeWire"
    }
}
