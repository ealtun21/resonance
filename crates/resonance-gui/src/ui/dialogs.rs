//! Modal dialogs: the help overlay, the preset-load browser, the export
//! save-as dialog, the confirm modal, and their shared navigation header.

use crate::app::GuiApp;
use crate::browser::{Browser, Item};
use crate::state::{Confirm, Dialog};
use crate::theme::Theme;
use crate::ui::icons::Icon;
use crate::ui::kit;
use crate::ui::widgets::dialog_window;
use eframe::egui;
use resonance_ipc::Command;

impl GuiApp {
    /// Modal listing every FR-graph gesture and keyboard shortcut, for users
    /// who don't know the (mostly mouse-driven) controls exist.
    pub(crate) fn help_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_help {
            return;
        }
        let mut open = true;
        dialog_window(ctx, "Controls & shortcuts")
            .open(&mut open)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // (gesture, what it does) rows under section headers.
                    let section = |ui: &mut egui::Ui, title: &str, rows: &[(&str, &str)]| {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(title).strong());
                        egui::Grid::new(title)
                            .num_columns(2)
                            .spacing(egui::vec2(16.0, 4.0))
                            .striped(true)
                            .show(ui, |ui| {
                                for (k, v) in rows {
                                    ui.label(egui::RichText::new(*k).monospace());
                                    ui.label(*v);
                                    ui.end_row();
                                }
                            });
                    };
                    section(
                        ui,
                        "EQ graph — nodes",
                        &[
                            ("Left-drag node", "move band (frequency + gain)"),
                            ("Right-drag node", "adjust Q (drag up = narrower)"),
                            ("Double-left-click", "add a peaking band there"),
                            ("Double-right-click node", "pin frequency; again to release"),
                            (
                                "Shift+double-right-click node",
                                "pin gain; again to release",
                            ),
                        ],
                    );
                    section(
                        ui,
                        "EQ graph — zoom",
                        &[
                            ("Scroll wheel", "zoom x-axis around the pointer"),
                            ("Shift + left-drag", "box-select a frequency range to zoom"),
                            ("Click \"reset ⟲\"", "restore the full 20 Hz–20 kHz view"),
                        ],
                    );
                    section(
                        ui,
                        "Reference curves",
                        &[
                            (
                                "Reference toggle",
                                "overlay a target + a headphone measurement on the graph",
                            ),
                            (
                                "Target",
                                "pick a target to EQ toward (Diffuse Field / Harman / PEQdB); load a measurement via Browse to compare against it",
                            ),
                            (
                                "raw meas / normalize",
                                "toggles (after a measurement loads): raw meas shows the un-EQ'd curve; normalize flattens the target to 0 so you EQ the result straight",
                            ),
                            (
                                "Auto-EQ",
                                "fit EQ bands (peqdb AutoEQ) so the measurement matches the target",
                            ),
                            (
                                "Bounds",
                                "shade the listener-preference tolerance band around the target (tight mids, wider bass/treble); keep the result inside it",
                            ),
                            (
                                "Customize",
                                "tilt + bass + ear-gain + treble on top of any target; Save stores it",
                            ),
                            (
                                "Browse",
                                "download a headphone/IEM measurement from squig.link",
                            ),
                        ],
                    );
                    section(
                        ui,
                        "Keyboard",
                        &[
                            ("Ctrl+Z", "undo"),
                            ("Ctrl+Y / Ctrl+Shift+Z", "redo"),
                            ("F1 / ?", "toggle this help"),
                            ("Esc", "close this help"),
                        ],
                    );
                });
            });
        if !open {
            self.show_help = false;
        }
    }

    // ── Settings dialog ─────────────────────────────────────────────────────

    /// App settings modal: advanced-feature visibility toggles, the relocated
    /// channel controls, and the theme picker (moved out of the overflow menu).
    pub(crate) fn settings_dialog(&mut self, ctx: &egui::Context) {
        if !matches!(self.dialog, Dialog::Settings) {
            return;
        }
        let mut open = true;
        let state = self.state.clone();
        dialog_window(ctx, "Settings")
            .open(&mut open)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Advanced features").strong());
                    ui.weak("Hidden by default to keep the main view clean.");
                    ui.add_space(4.0);
                    ui.checkbox(
                        &mut self.show_slope,
                        "Filter slope column (12/24/48 dB/oct)",
                    );
                    ui.checkbox(&mut self.show_scope, "Stereo scope column (Mid/Side)");
                    ui.checkbox(
                        &mut self.show_dynamics,
                        "Dynamic EQ column (level-driven bands)",
                    );
                    ui.checkbox(&mut self.show_dither, "Output dither section");
                    ui.checkbox(
                        &mut self.show_ir,
                        "Convolution section (WAV impulse response)",
                    );

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("EQ phase").strong());
                    if let Some(s) = &state {
                        // Daemon state, not a preference: mirrors the live mode
                        // and sends the toggle. Not routed through `queue_edit` —
                        // the undo snapshot doesn't cover the phase mode (it is
                        // preserved across `ApplyState`, like dither and the IR).
                        ui.horizontal(|ui| {
                            let mut linear = s.phase_mode_linear;
                            if ui
                                .checkbox(&mut linear, "Linear phase")
                                .on_hover_text(
                                    "Render the static EQ bands to an FIR — no phase \
                                     rotation, but adds latency (~171 ms at 48 kHz). \
                                     Mid/Side-scoped and dynamic bands stay \
                                     minimum-phase.",
                                )
                                .changed()
                            {
                                self.queue(Command::SetPhaseMode { linear });
                            }
                            if s.phase_mode_linear && s.eq_fir_latency_frames > 0 {
                                let ms = s.eq_fir_latency_frames as f64 / s.sample_rate.max(1.0)
                                    * 1000.0;
                                ui.weak(format!("(+{ms:.1} ms)"));
                            }
                        });
                    } else {
                        ui.weak("Connect the daemon to change the EQ phase mode.");
                    }

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Channels").strong());
                    if let Some(s) = &state {
                        if s.channels >= 2 {
                            self.channels_section(ui, s);
                        } else {
                            ui.weak("Stereo or multichannel output required.");
                        }
                    } else {
                        ui.weak("Connect the daemon to configure channels.");
                    }

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Theme").strong());
                    let cctx = ui.ctx().clone();
                    for t in Theme::ALL {
                        if ui.selectable_label(self.theme == t, t.label()).clicked() {
                            self.set_theme(&cctx, t);
                        }
                    }
                });
            });
        if !open {
            self.dialog = Dialog::None;
        }
    }

    // ── Preset load dialog ──────────────────────────────────────────────────

    pub(crate) fn preset_dialog(&mut self, ctx: &egui::Context) {
        let Dialog::LoadPreset(browser) = &mut self.dialog else {
            return;
        };
        let mut open = true;
        let mut close = false;
        let mut to_load: Option<String> = None;

        let pal = self.palette;
        // Scale the file/preview lists to the window so the dialog fits a small
        // window without overflowing. From the viewport (stable per window size),
        // not the dialog's own content, so it can't oscillate.
        let vh = ctx.content_rect().height();
        let list_h = (vh * 0.42).clamp(120.0, 320.0);
        let prev_h = (vh * 0.24).clamp(72.0, 150.0);
        dialog_window(ctx, "Load preset")
            // Fresh id: egui keys collapse/geometry state off the window title; a
            // dedicated id avoids inheriting a stale collapsed state.
            .id(egui::Id::new("resonance_load_preset_dialog"))
            .open(&mut open)
            .show(ctx, |ui| {
                if let Some(p) = nav_bar(ui, browser) {
                    to_load = Some(p);
                }
                ui.add_space(4.0);

                // Keyboard navigation when no text field holds focus.
                let kbd = ctx.memory(|m| m.focused().is_none());
                let mut activate: Option<usize> = None;
                if kbd {
                    ui.input(|i| {
                        if i.key_pressed(egui::Key::ArrowDown) {
                            browser.move_cursor(1);
                        }
                        if i.key_pressed(egui::Key::ArrowUp) {
                            browser.move_cursor(-1);
                        }
                        if i.key_pressed(egui::Key::Backspace) {
                            browser.parent();
                        }
                        if i.key_pressed(egui::Key::Enter) {
                            activate = Some(browser.cursor);
                        }
                    });
                }

                // Stacked, fixed-height body (constants, not `available_height`,
                // so the auto-sizing window can't oscillate). File list on top,
                // parsed preview below — mirrors the Export dialog's layout.
                let mut select: Option<usize> = None;
                kit::well_frame(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("files")
                        .auto_shrink([false, false])
                        .min_scrolled_height(list_h)
                        .max_height(list_h)
                        .show(ui, |ui| {
                            for (i, it) in browser.entries.iter().enumerate() {
                                let label = format!("{}  {}", entry_icon(it), it.name);
                                let resp = kit::list_row(ui, i == browser.cursor, &label)
                                    .on_hover_text(it.path.display().to_string());
                                if resp.clicked() {
                                    select = Some(i);
                                }
                                if resp.double_clicked() {
                                    activate = Some(i);
                                }
                            }
                        });
                });
                if let Some(i) = select {
                    browser.select(i);
                }
                if let Some(i) = activate {
                    if let Some(path) = browser.activate(i) {
                        to_load = Some(path);
                    }
                }

                // Parsed preview of the highlighted entry.
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Preview").color(pal.neutral));
                kit::well_frame(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("preview")
                        .auto_shrink([false, false])
                        .min_scrolled_height(prev_h)
                        .max_height(prev_h)
                        .show(ui, |ui| {
                            if browser.preview.is_empty() {
                                ui.weak("select a file to preview");
                            }
                            for line in &browser.preview {
                                ui.monospace(line);
                            }
                        });
                });

                // Footer: Load (only for a loadable file) + Cancel.
                ui.separator();
                let loadable = browser
                    .selected()
                    .is_some_and(|it| !it.is_dir && it.is_preset);
                ui.horizontal(|ui| {
                    if kit::button(ui, "Load", true, loadable) {
                        to_load = browser.activate(browser.cursor);
                    }
                    if kit::button(ui, "Cancel", false, true) {
                        close = true;
                    }
                });
            });

        if let Some(path) = to_load {
            self.import_and_load(path);
            close = true;
        }
        if !open || close {
            self.dialog = Dialog::None;
        }
    }

    // ── Impulse-response (convolution) picker ───────────────────────────────

    /// `.wav` picker for the convolution stage — the Load-preset navigator with
    /// a WAV-header preview, sending `SetConvolutionIr` on pick.
    pub(crate) fn ir_dialog(&mut self, ctx: &egui::Context) {
        let Dialog::LoadIr(browser) = &mut self.dialog else {
            return;
        };
        let mut open = true;
        let mut close = false;
        let mut to_load: Option<String> = None;

        let pal = self.palette;
        let vh = ctx.content_rect().height();
        let list_h = (vh * 0.42).clamp(120.0, 320.0);
        let prev_h = (vh * 0.16).clamp(48.0, 90.0);
        dialog_window(ctx, "Load impulse response")
            .id(egui::Id::new("resonance_load_ir_dialog"))
            .open(&mut open)
            .show(ctx, |ui| {
                if let Some(p) = nav_bar(ui, browser) {
                    to_load = Some(p);
                }
                ui.add_space(4.0);

                let kbd = ctx.memory(|m| m.focused().is_none());
                let mut activate: Option<usize> = None;
                if kbd {
                    ui.input(|i| {
                        if i.key_pressed(egui::Key::ArrowDown) {
                            browser.move_cursor(1);
                        }
                        if i.key_pressed(egui::Key::ArrowUp) {
                            browser.move_cursor(-1);
                        }
                        if i.key_pressed(egui::Key::Backspace) {
                            browser.parent();
                        }
                        if i.key_pressed(egui::Key::Enter) {
                            activate = Some(browser.cursor);
                        }
                    });
                }

                let mut select: Option<usize> = None;
                kit::well_frame(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("ir_files")
                        .auto_shrink([false, false])
                        .min_scrolled_height(list_h)
                        .max_height(list_h)
                        .show(ui, |ui| {
                            for (i, it) in browser.entries.iter().enumerate() {
                                let label = format!("{}  {}", entry_icon(it), it.name);
                                let resp = kit::list_row(ui, i == browser.cursor, &label)
                                    .on_hover_text(it.path.display().to_string());
                                if resp.clicked() {
                                    select = Some(i);
                                }
                                if resp.double_clicked() {
                                    activate = Some(i);
                                }
                            }
                        });
                });
                if let Some(i) = select {
                    browser.select(i);
                }
                if let Some(i) = activate {
                    if let Some(path) = browser.activate(i) {
                        to_load = Some(path);
                    }
                }

                ui.add_space(6.0);
                ui.label(egui::RichText::new("Preview").color(pal.neutral));
                kit::well_frame(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("ir_preview")
                        .auto_shrink([false, false])
                        .min_scrolled_height(prev_h)
                        .max_height(prev_h)
                        .show(ui, |ui| {
                            if browser.preview.is_empty() {
                                ui.weak("select a .wav to preview");
                            }
                            for line in &browser.preview {
                                ui.monospace(line);
                            }
                        });
                });

                ui.separator();
                let loadable = browser
                    .selected()
                    .is_some_and(|it| !it.is_dir && it.is_preset);
                ui.horizontal(|ui| {
                    if kit::button(ui, "Load", true, loadable) {
                        to_load = browser.activate(browser.cursor);
                    }
                    if kit::button(ui, "Cancel", false, true) {
                        close = true;
                    }
                });
            });

        if let Some(path) = to_load {
            self.queue(Command::SetConvolutionIr { path });
            self.set_status("loading impulse response…");
            close = true;
        }
        if !open || close {
            self.dialog = Dialog::None;
        }
    }

    /// Save-as dialog: navigate to a folder, type a name, write the current
    /// chain as a Resonance `.toml` profile.
    pub(crate) fn export_dialog(&mut self, ctx: &egui::Context) {
        let Dialog::ExportProfile(save) = &mut self.dialog else {
            return;
        };
        let source = save.source.clone();
        let pal = self.palette;
        let mut open = true;
        let mut close = false;
        let mut do_export: Option<String> = None;

        let body_h = (ctx.content_rect().height() * 0.4).clamp(120.0, 280.0);
        dialog_window(ctx, "Export profile")
            .open(&mut open)
            .show(ctx, |ui| {
                // Typing a full file path into the location bar pre-fills the
                // folder + name instead of loading anything.
                if let Some(p) = nav_bar(ui, &mut save.browser) {
                    if let Some(stem) = std::path::Path::new(&p)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                    {
                        save.filename = stem;
                    }
                }
                ui.add_space(4.0);

                // Folder + existing-file picker. Clicking a file copies its name
                // into the field (a quick "overwrite this one" gesture). Reserve
                // the footer (name field, path readout, buttons) and let the list
                // fill the rest of the resizable window.
                let mut select: Option<usize> = None;
                let mut activate: Option<usize> = None;
                let mut pick_name: Option<String> = None;
                kit::well_frame(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("save_files")
                        .auto_shrink([false, false])
                        .min_scrolled_height(body_h)
                        .max_height(body_h)
                        .show(ui, |ui| {
                            for (i, it) in save.browser.entries.iter().enumerate() {
                                let label = format!("{}  {}", entry_icon(it), it.name);
                                let resp = kit::list_row(ui, i == save.browser.cursor, &label);
                                if resp.clicked() {
                                    select = Some(i);
                                    if !it.is_dir {
                                        pick_name = std::path::Path::new(&it.name)
                                            .file_stem()
                                            .map(|s| s.to_string_lossy().into_owned());
                                    }
                                }
                                if resp.double_clicked() {
                                    activate = Some(i);
                                }
                            }
                        });
                });
                if let Some(i) = select {
                    save.browser.select(i);
                }
                if let Some(n) = pick_name {
                    save.filename = n;
                }
                if let Some(i) = activate {
                    save.browser.activate(i); // dirs navigate; files are ignored here
                }

                // Filename + implicit .toml suffix.
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Name");
                    kit::text_field(
                        ui,
                        260.0,
                        egui::Id::new("export_name_field"),
                        &mut save.filename,
                        "profile name",
                        false,
                    );
                    ui.label(egui::RichText::new(".toml").color(pal.neutral));
                });

                // Destination readout + overwrite warning.
                let target = save.target();
                let exists = target.is_file();
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("→ ").color(pal.neutral));
                    ui.label(egui::RichText::new(target.display().to_string()).monospace());
                });
                if exists {
                    ui.colored_label(
                        pal.cut,
                        "! a file with this name exists — it will be overwritten",
                    );
                }

                ui.separator();
                ui.horizontal(|ui| {
                    let ok = !save.filename.trim().is_empty();
                    if exists {
                        if kit::button_filled(ui, "Overwrite", pal.cut, ok) {
                            do_export = Some(target.to_string_lossy().into_owned());
                        }
                    } else if kit::button(ui, "Save", true, ok) {
                        do_export = Some(target.to_string_lossy().into_owned());
                    }
                    if kit::button(ui, "Cancel", false, true) {
                        close = true;
                    }
                });
            });

        if let Some(p) = do_export {
            match source {
                Some(name) => self.export_profile_named(name, p),
                None => self.export_profile(p),
            }
            close = true;
        }
        if !open || close {
            self.dialog = Dialog::None;
        }
    }

    /// Yes/no modal for overwriting or deleting a profile (destructive actions
    /// confirm before running).
    pub(crate) fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(action) = self.confirm.clone() else {
            return;
        };
        let (title, body, ok_label) = match &action {
            Confirm::SaveProfile(name) => (
                "Overwrite profile",
                format!(
                    "Profile '{name}' already exists.\nOverwrite it with the current settings?"
                ),
                "Overwrite",
            ),
            Confirm::DeleteProfile(name) => {
                let mapped = self.mappings.iter().filter(|(_, p)| p == name).count();
                let extra = if mapped == 0 {
                    String::new()
                } else {
                    format!(
                        "\n\nIt's mapped to {mapped} output device{}; that mapping will be removed too.",
                        if mapped == 1 { "" } else { "s" }
                    )
                };
                (
                    "Delete profile",
                    format!("Really delete profile '{name}'?\nThis cannot be undone.{extra}"),
                    "Delete",
                )
            }
        };
        let mut decision: Option<bool> = None;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(body);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if kit::button_filled(ui, ok_label, self.palette.cut, true) {
                        decision = Some(true);
                    }
                    if kit::button(ui, "Cancel", false, true) {
                        decision = Some(false);
                    }
                });
            });
        match decision {
            Some(true) => {
                match action {
                    Confirm::SaveProfile(name) => {
                        // Bundle the current measurement with the (overwritten) profile.
                        self.reference.store_measurement_for(&name);
                        self.queue(Command::SaveProfile { name });
                        self.dirty = false;
                    }
                    Confirm::DeleteProfile(name) => {
                        // Drop any device→profile mappings pointing at it first,
                        // so no device is left mapped to a profile that's gone.
                        for (node, profile) in self.mappings.clone() {
                            if profile == name {
                                self.queue(Command::UnmapOutputFor { node_name: node });
                            }
                        }
                        self.reference.remove_profile_meas(&name);
                        self.queue(Command::DeleteProfile { name });
                    }
                }
                self.needs_meta = true;
                self.confirm = None;
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }
}

/// Leading glyph for a directory entry. Restricted to glyphs present in the
/// embedded `icons.ttf` subset (`▸ ↑ ·`) so nothing renders as a tofu box —
/// egui can't rasterise colour-emoji fonts, so file-type emoji are out.
fn entry_icon(it: &Item) -> &'static str {
    if it.is_dir {
        if it.name == ".." { "↑" } else { "▸" }
    } else {
        "·"
    }
}

/// Shared navigation header for the Load / Export dialogs: bookmark buttons, an
/// editable location bar, and a name filter. Returns `Some(path)` when the typed
/// location resolves to a file (load dialogs act on it; save dialogs pre-fill).
fn nav_bar(ui: &mut egui::Ui, browser: &mut Browser) -> Option<String> {
    let mut typed: Option<String> = None;
    ui.horizontal(|ui| {
        if kit::icon_btn(ui, Icon::Up, kit::CTRL_H, "Up to parent folder") {
            browser.parent();
        }
        if kit::icon_btn(ui, Icon::Home, kit::CTRL_H, "Home folder") {
            browser.navigate(crate::browser::home_dir());
        }
        if kit::button_tip(ui, "Library", false, true, "Resonance preset library") {
            let lib = resonance_ipc::paths::user_preset_dir();
            let _ = std::fs::create_dir_all(&lib);
            browser.navigate(lib);
        }
    });
    ui.add_space(2.0);
    egui::Grid::new("nav_fields")
        .num_columns(2)
        .spacing([6.0, 4.0])
        .show(ui, |ui| {
            ui.label("Path");
            let edit = ui.add(
                egui::TextEdit::singleline(&mut browser.path_edit)
                    .desired_width(f32::INFINITY)
                    .hint_text("type a path and press Enter"),
            );
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                typed = browser.go_to_typed();
            }
            ui.end_row();

            ui.label("Filter");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut browser.filter)
                        .desired_width(f32::INFINITY)
                        .hint_text("filter by name"),
                )
                .changed()
            {
                browser.refilter();
            }
            ui.end_row();
        });
    typed
}
