//! Modal dialogs: the help overlay, the preset-load browser, the export
//! save-as dialog, the confirm modal, and their shared navigation header.

use crate::app::GuiApp;
use crate::browser::{Browser, Item};
use crate::state::{Confirm, Dialog};
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
        let list_h = (vh * 0.42).clamp(120.0, 260.0);
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
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("files")
                        .auto_shrink([false, false])
                        .min_scrolled_height(list_h)
                        .max_height(list_h)
                        .show(ui, |ui| {
                            for (i, it) in browser.entries.iter().enumerate() {
                                let label = format!("{}  {}", entry_icon(it), it.name);
                                let resp = ui
                                    .selectable_label(i == browser.cursor, label)
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
                egui::Frame::group(ui.style()).show(ui, |ui| {
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
                    .map(|it| !it.is_dir && it.is_preset)
                    .unwrap_or(false);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(loadable, egui::Button::new("Load"))
                        .clicked()
                    {
                        to_load = browser.activate(browser.cursor);
                    }
                    if ui.button("Cancel").clicked() {
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

    /// Save-as dialog: navigate to a folder, type a name, write the current
    /// chain as a Resonance `.toml` profile.
    pub(crate) fn export_dialog(&mut self, ctx: &egui::Context) {
        let Dialog::ExportProfile(save) = &mut self.dialog else {
            return;
        };
        let pal = self.palette;
        let mut open = true;
        let mut close = false;
        let mut do_export: Option<String> = None;

        let body_h = (ctx.content_rect().height() * 0.4).clamp(120.0, 240.0);
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
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("save_files")
                        .auto_shrink([false, false])
                        .min_scrolled_height(body_h)
                        .max_height(body_h)
                        .show(ui, |ui| {
                            for (i, it) in save.browser.entries.iter().enumerate() {
                                let label = format!("{}  {}", entry_icon(it), it.name);
                                let resp = ui.selectable_label(i == save.browser.cursor, label);
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
                    ui.add(
                        egui::TextEdit::singleline(&mut save.filename)
                            .desired_width(260.0)
                            .hint_text("profile name"),
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
                    let label = if exists { "Overwrite" } else { "Save" };
                    if ui.add_enabled(ok, egui::Button::new(label)).clicked() {
                        do_export = Some(target.to_string_lossy().into_owned());
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });

        if let Some(p) = do_export {
            self.export_profile(p);
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
            Confirm::DeleteProfile(name) => (
                "Delete profile",
                format!("Really delete profile '{name}'?\nThis cannot be undone."),
                "Delete",
            ),
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
                    let ok = egui::Button::new(
                        egui::RichText::new(ok_label).color(egui::Color32::WHITE),
                    )
                    .fill(self.palette.cut);
                    if ui.add(ok).clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
        match decision {
            Some(true) => {
                match action {
                    Confirm::SaveProfile(name) => self.queue(Command::SaveProfile { name }),
                    Confirm::DeleteProfile(name) => self.queue(Command::DeleteProfile { name }),
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
        if ui
            .button("↑ Up")
            .on_hover_text("parent directory")
            .clicked()
        {
            browser.parent();
        }
        if ui.button("Home").on_hover_text("home directory").clicked() {
            browser.navigate(crate::browser::home_dir());
        }
        if ui
            .button("Library")
            .on_hover_text("Resonance preset library")
            .clicked()
        {
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
