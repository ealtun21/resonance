//! Right column: device → profile mapping table and the saved-profiles list.

use crate::app::GuiApp;
use crate::state::Confirm;
use crate::ui::widgets::{centered, section};
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
            centered(ui, "dev_body", |ui| self.device_table(ui, s));
        } else {
            ui.weak("(no daemon)");
        }
    }

    /// Saved profiles list (the save/load/rename rows).
    pub(crate) fn profiles_panel(&mut self, ui: &mut egui::Ui) {
        centered(ui, "profiles_body", |ui| self.profiles_section(ui));
    }

    /// Profiles list: a fixed-width save row plus one row per profile — Load
    /// (A/B), an inline-editable name (type + Enter to rename), and delete.
    /// Widths are fixed (not `available_width`) so the block centres cleanly.
    pub(crate) fn profiles_section(&mut self, ui: &mut egui::Ui) {
        const NAME_W: f32 = 200.0;

        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.profile_name)
                    .hint_text("new profile name…")
                    .desired_width(NAME_W),
            );
            let save = ui
                .button("Save")
                .on_hover_text("save current chain as a new profile");
            let go = save.clicked()
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

                if ui
                    .add(egui::Button::new("Load").small())
                    .on_hover_text("load this profile (A/B)")
                    .clicked()
                {
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
                    .desired_width(NAME_W)
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

                if ui
                    .add(egui::Button::new("✕").small())
                    .on_hover_text("delete profile")
                    .clicked()
                {
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

        egui::Grid::new("device_map_grid")
            .num_columns(3)
            .striped(true)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                for (node, _desc) in &s.sink_descriptions {
                    let here = present.contains(node.as_str());
                    let is_active = active == Some(node.as_str());
                    // Status dot: green = active, dim green = present, grey = absent.
                    let (dot, col) = if is_active {
                        ("●", self.palette.boost)
                    } else if here {
                        ("●", self.palette.neutral)
                    } else {
                        ("○", self.palette.grid)
                    };
                    ui.colored_label(col, dot).on_hover_text(if here {
                        "connected"
                    } else {
                        "remembered (absent)"
                    });
                    ui.label(s.sink_label(node)).on_hover_text(node.as_str());

                    let cur: Option<&str> = map.get(node).map(String::as_str);
                    let mut sel: Option<String> = cur.map(str::to_owned);
                    let cur_text = sel.clone().unwrap_or_else(|| "—".to_string());
                    egui::ComboBox::from_id_salt(("devmap", node))
                        .selected_text(cur_text)
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut sel, None, "—");
                            for p in &profiles {
                                ui.selectable_value(&mut sel, Some(p.clone()), p);
                            }
                        });
                    let new = sel.as_deref();
                    if new != cur {
                        match new {
                            Some(p) => self.queue(Command::MapOutputFor {
                                node_name: node.clone(),
                                profile: p.to_string(),
                            }),
                            None => self.queue(Command::UnmapOutputFor {
                                node_name: node.clone(),
                            }),
                        }
                        self.needs_meta = true;
                    }

                    if ui
                        .button("✕")
                        .on_hover_text("forget device (re-adds when next connected)")
                        .clicked()
                    {
                        self.queue(Command::ForgetSink {
                            node_name: node.clone(),
                        });
                        self.needs_meta = true;
                    }
                    ui.end_row();
                }
            });
    }
}
