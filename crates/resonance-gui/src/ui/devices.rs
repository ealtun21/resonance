//! Right column: device → profile mapping table and the saved-profiles list.

use crate::app::GuiApp;
use crate::state::Confirm;
use crate::ui::icons::Icon;
use crate::ui::kit;
use crate::ui::widgets::section;
use eframe::egui;
use resonance_ipc::{Command, DaemonState, RoutingMatrix};

impl GuiApp {
    // ── Right column: devices → profiles + profile list ─────────────────────

    pub(crate) fn devices_profiles(&mut self, ui: &mut egui::Ui) {
        section(ui, "Device mapping", |ui| self.device_mapping_section(ui));
        ui.add_space(12.0);
        section(ui, "Profiles", |ui| self.profiles_panel(ui));
        // Channels sits below Profiles. Channel routing surfaces only on
        // multi-channel-capable devices (progressive disclosure — stereo users
        // don't see it for >2ch features, but the L/R swap is useful from 2ch up).
        if let Some(s) = self.state.clone() {
            if s.channels >= 2 {
                ui.add_space(12.0);
                section(ui, "Channels", |ui| self.channels_section(ui, &s));
            }
        }
    }

    /// Channel layout + routing controls: shows the in→out channel counts +
    /// position labels, an L/R swap toggle, and a clear-routing button. The full
    /// N×N matrix editor is intentionally omitted (the in-place backends only do
    /// square remaps; swap covers the common case) — CLI `channel route` remains
    /// for power users.
    pub(crate) fn channels_section(&mut self, ui: &mut egui::Ui, s: &DaemonState) {
        let line = if s.out_channels != 0 && s.out_channels != s.channels {
            format!("in {} → out {}", s.channels, s.out_channels)
        } else {
            format!("{} ch", s.channels)
        };
        let layout = s.channel_layout.join(" ");
        ui.horizontal_wrapped(|ui| {
            ui.weak(line);
            if !layout.is_empty() {
                ui.weak("·");
                ui.weak(layout);
            }
        });
        if s.channels < 2 {
            return;
        }
        ui.add_space(kit::SP_XS);
        // The L/R swap is exactly the swap(channels, 0, 1) routing matrix.
        let swap = RoutingMatrix::swap(s.channels, 0, 1);
        let is_swapped = s.routing.as_ref() == Some(&swap);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = kit::SP_S;
            if ui
                .selectable_label(is_swapped, "Swap L/R")
                .on_hover_text("Swap the front-left and front-right channels")
                .clicked()
            {
                if is_swapped {
                    self.queue(Command::ClearRouting);
                } else {
                    self.queue(Command::SwapChannels { a: 0, b: 1 });
                }
            }
            if s.routing.is_some()
                && kit::icon_text_btn(
                    ui,
                    Icon::Close,
                    "Clear",
                    false,
                    true,
                    "Remove channel routing (straight passthrough)",
                )
            {
                self.queue(Command::ClearRouting);
            }
        });
        // Opt-in per-channel EQ: reveals a per-band channel-target column so a
        // band can be aimed at specific channels (e.g. L-only / R-only). On
        // >2ch the column shows automatically, so the toggle only matters at 2ch.
        ui.add_space(kit::SP_XS);
        let mut per_ch = self.per_channel_eq;
        if ui
            .checkbox(&mut per_ch, "Per-channel EQ")
            .on_hover_text(
                "Show a per-band channel column so a band can target specific \
                 channels (e.g. left- or right-only). Always on for >2 channels.",
            )
            .changed()
        {
            self.per_channel_eq = per_ch;
        }
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
        let save_name_w = (full_w - 96.0).max(110.0);
        let row_name_w = (full_w - 80.0).max(100.0);

        ui.horizontal(|ui| {
            ui.set_min_height(26.0);
            ui.spacing_mut().item_spacing.x = kit::SP_S;
            let resp = kit::text_field(
                ui,
                save_name_w,
                egui::Id::new("new_profile_name"),
                &mut self.profile_name,
                "new profile name…",
                false,
            );
            let save_clicked = kit::icon_text_btn(
                ui,
                Icon::Save,
                "Save",
                true,
                true,
                "Save the current EQ as a profile with this name",
            );
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
                    self.dirty = false;
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
                ui.set_min_height(26.0);
                ui.spacing_mut().item_spacing.x = kit::SP_S;
                let active = current.as_deref() == Some(name.as_str());
                let editing = matches!(&self.rename, Some((from, _)) if from == name);

                // Selection affordance: the active profile shows an OPEN folder
                // ("this one's loaded"); every other row is a CLOSED folder. Both
                // are clickable — loading a profile, including re-loading the
                // active one, restores it (discarding any in-progress edits).
                let load = if active {
                    kit::icon_btn(
                        ui,
                        Icon::FolderOpen,
                        kit::CTRL_H,
                        "Loaded — click to restore (discard edits)",
                    )
                } else {
                    kit::icon_btn(ui, Icon::Folder, kit::CTRL_H, "Load this profile")
                };
                if load {
                    self.queue(Command::LoadProfile { name: name.clone() });
                    self.dirty = false; // loaded profile is the new baseline
                }

                // Inline-editable, fixed-width name. Type + Enter to rename;
                // click away abandons the edit.
                let mut buf = match &self.rename {
                    Some((from, b)) if from == name => b.clone(),
                    _ => name.clone(),
                };
                let resp = kit::text_field(
                    ui,
                    row_name_w,
                    egui::Id::new(("pname", name)),
                    &mut buf,
                    "name",
                    active && !editing,
                )
                .on_hover_text("rename: edit + Enter");

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

                if kit::icon_btn(ui, Icon::Close, kit::CTRL_H, "Delete this profile") {
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
                // Flexible device name — soft-faded + hover-scrolled to its cell
                // so a long device name never overflows into the dropdown.
                let text = kit::tokens(ui).text;
                let bg = ui.visuals().panel_fill;
                let (lr, _) =
                    ui.allocate_exact_size(egui::vec2(name_w, 22.0), egui::Sense::hover());
                kit::fade_text(
                    ui,
                    lr,
                    &s.sink_label(node),
                    egui::FontId::proportional(kit::T_BODY),
                    text,
                    bg,
                );
                // Profile picker: index 0 = "—" (unmapped), the rest map 1:1.
                let cur_text = map.get(node).cloned().unwrap_or_else(|| "—".to_string());
                let mut opts = vec!["—".to_string()];
                opts.extend(profiles.iter().cloned());
                let refs: Vec<&str> = opts.iter().map(String::as_str).collect();
                if let Some(idx) = kit::dropdown(
                    ui,
                    DD_W,
                    kit::CTRL_H,
                    egui::Id::new(("devmap", node)),
                    &cur_text,
                    &refs,
                ) {
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
                if kit::icon_btn(ui, Icon::Close, kit::CTRL_H, "Forget this device") {
                    self.queue(Command::ForgetSink {
                        node_name: node.clone(),
                    });
                    self.needs_meta = true;
                }
            });
        }
    }
}
