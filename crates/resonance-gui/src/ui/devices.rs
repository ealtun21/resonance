//! Right column: device → profile mapping table and the saved-profiles list.

use crate::app::GuiApp;
use crate::state::Confirm;
use crate::ui::kit;
use crate::ui::widgets::{ellipsize, section};
use eframe::egui;
use resonance_ipc::{Command, DaemonState};

impl GuiApp {
    // ── Right column: devices → profiles + profile list ─────────────────────

    pub(crate) fn devices_profiles(&mut self, ui: &mut egui::Ui) {
        section(ui, "Device Profile Mapping", |ui| {
            self.device_mapping_section(ui)
        });
        ui.add_space(12.0);
        section(ui, "Profiles", |ui| self.profiles_panel(ui));
    }

    /// Device → profile mapping table: every output device we've ever seen, each
    /// with a profile dropdown. The active device auto-loads its mapped one.
    pub(crate) fn device_mapping_section(&mut self, ui: &mut egui::Ui) {
        let state = self.state.clone();
        if let Some(s) = &state {
            self.device_table(ui, s);
        } else {
            ui.weak("(no daemon)");
        }
    }

    /// Saved profiles list (the save/load/rename rows).
    pub(crate) fn profiles_panel(&mut self, ui: &mut egui::Ui) {
        self.profiles_section(ui);
    }

    /// Profiles list: a save row plus one row per profile — Load (A/B), an
    /// inline-editable name (type + Enter to rename), and delete. The name fields
    /// flex to fill the column width so the block isn't a squished fixed island.
    pub(crate) fn profiles_section(&mut self, ui: &mut egui::Ui) {
        let full_w = ui.available_width();
        let save_name_w = (full_w - 72.0).max(120.0);
        let row_name_w = (full_w - 96.0).max(100.0);

        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.profile_name)
                    .hint_text("new profile name…")
                    .desired_width(save_name_w),
            );
            let save_clicked = kit::button(ui, "Save", true, true);
            let go = save_clicked
                || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            if go && !self.profile_name.trim().is_empty() {
                let name = self.profile_name.trim().to_string();
                // Overwriting an existing profile asks first; a new name saves now.
                if self.profiles.iter().any(|p| p == &name) {
                    self.confirm = Some(Confirm::SaveProfile(name));
                } else {
                    self.queue(Command::SaveProfile { name });
                    self.needs_meta = true;
                }
                self.profile_name.clear();
            }
        });

        ui.add_space(6.0);

        let profiles = self.profiles.clone();
        if profiles.is_empty() {
            ui.weak("(no profiles yet)");
            return;
        }
        let current = self.state.as_ref().and_then(|s| s.current_preset.clone());
        for name in &profiles {
            ui.horizontal(|ui| {
                let active = current.as_deref() == Some(name.as_str());
                let editing = matches!(&self.rename, Some((from, _)) if from == name);

                if kit::button(ui, "Load", false, true) {
                    self.queue(Command::LoadProfile { name: name.clone() });
                }

                // Inline-editable, fixed-width name. Type + Enter to rename;
                // click away abandons the edit.
                let mut buf = match &self.rename {
                    Some((from, b)) if from == name => b.clone(),
                    _ => name.clone(),
                };
                let mut edit = egui::TextEdit::singleline(&mut buf)
                    .id_salt(("pname", name))
                    .desired_width(row_name_w)
                    .hint_text("name");
                if active && !editing {
                    edit = edit.text_color(self.palette.accent);
                }
                let resp = ui.add(edit).on_hover_text("rename: edit + Enter");

                if resp.gained_focus() || resp.changed() {
                    self.rename = Some((name.clone(), buf.clone()));
                }
                if resp.lost_focus() {
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let to = buf.trim().to_string();
                    if enter && !to.is_empty() && to != *name {
                        self.queue(Command::RenameProfile {
                            from: name.clone(),
                            to,
                        });
                        self.needs_meta = true;
                    }
                    self.rename = None;
                }

                if kit::icon_button(ui, "✕") {
                    self.confirm = Some(Confirm::DeleteProfile(name.clone()));
                }
            });
        }
    }

    /// The device→profile mapping table. Lists every known output device
    /// (`sink_descriptions` already merges present + remembered ones); each gets
    /// a profile dropdown and a "forget" button. Forgetting only drops it until
    /// PipeWire next reports it (plug in / select as output).
    pub(crate) fn device_table(&mut self, ui: &mut egui::Ui, s: &DaemonState) {
        use std::collections::HashSet;
        let present: HashSet<&str> = s.available_sinks.iter().map(String::as_str).collect();
        let active = s.active_output.as_deref();
        // Own the map so the `&self.mappings` borrow ends before the closure
        // below mutably borrows `self` (to queue commands).
        let map: std::collections::HashMap<String, String> =
            self.mappings.iter().cloned().collect();
        let profiles = self.profiles.clone();

        if s.sink_descriptions.is_empty() {
            ui.label("(no devices seen yet)");
            return;
        }

        // Manual rows so the device-name column FILLS the width (a Grid would
        // hug content and strand empty space to the right). Each row: status dot,
        // flexible name, profile dropdown, forget ✕.
        const DD_W: f32 = 132.0;
        let full_w = ui.available_width();
        let name_w = (full_w - 16.0 - DD_W - 26.0 - 3.0 * kit::SP_S).max(70.0);
        for (node, _desc) in &s.sink_descriptions {
            let here = present.contains(node.as_str());
            let is_active = active == Some(node.as_str());
            let col = if is_active {
                self.palette.boost
            } else if here {
                self.palette.neutral
            } else {
                self.palette.grid
            };
            ui.horizontal(|ui| {
                ui.set_min_height(26.0);
                ui.spacing_mut().item_spacing.x = kit::SP_S;
                // Status dot: filled when present/active, hollow when remembered.
                let (dr, _) = ui.allocate_exact_size(egui::vec2(12.0, 22.0), egui::Sense::hover());
                if is_active || here {
                    ui.painter().circle_filled(dr.center(), 4.0, col);
                } else {
                    ui.painter()
                        .circle_stroke(dr.center(), 3.5, egui::Stroke::new(1.3, col));
                }
                // Flexible device name (ellipsised to its cell).
                let text = kit::tokens(ui).text;
                let (lr, _) =
                    ui.allocate_exact_size(egui::vec2(name_w, 22.0), egui::Sense::hover());
                ui.painter().text(
                    egui::pos2(lr.left(), lr.center().y),
                    egui::Align2::LEFT_CENTER,
                    ellipsize(&s.sink_label(node), (name_w / 7.0) as usize),
                    egui::FontId::proportional(kit::T_BODY),
                    text,
                );
                // Profile picker: index 0 = "—" (unmapped), the rest map 1:1.
                let cur_text = map.get(node).cloned().unwrap_or_else(|| "—".to_string());
                let mut opts = vec!["—".to_string()];
                opts.extend(profiles.iter().cloned());
                let refs: Vec<&str> = opts.iter().map(String::as_str).collect();
                if let Some(idx) =
                    kit::dropdown(ui, DD_W, egui::Id::new(("devmap", node)), &cur_text, &refs)
                {
                    if idx == 0 {
                        self.queue(Command::UnmapOutputFor {
                            node_name: node.clone(),
                        });
                    } else {
                        self.queue(Command::MapOutputFor {
                            node_name: node.clone(),
                            profile: profiles[idx - 1].clone(),
                        });
                    }
                    self.needs_meta = true;
                }
                if kit::icon_button(ui, "✕") {
                    self.queue(Command::ForgetSink {
                        node_name: node.clone(),
                    });
                    self.needs_meta = true;
                }
            });
        }
    }
}
