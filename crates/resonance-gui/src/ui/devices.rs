//! Right column: device → profile mapping table and the saved-profiles list.

use crate::app::GuiApp;
use crate::state::Confirm;
use crate::ui::icons::Icon;
use crate::ui::kit;
use crate::ui::widgets::{gain_color, section_hint};
use eframe::egui;
use resonance_ipc::{Command, DaemonState, RoutingMatrix};

/// Trim `text` with a trailing ellipsis so it fits `max_w` at `size` — measured
/// against the live fonts. Used by the profile rows so long names clip cleanly
/// instead of overrunning the column.
fn elide(ui: &egui::Ui, text: &str, size: f32, max_w: f32) -> String {
    if kit::text_width(ui, size, text) <= max_w {
        return text.to_string();
    }
    let mut s = text.to_string();
    while !s.is_empty() && kit::text_width(ui, size, &format!("{s}…")) > max_w {
        s.pop();
    }
    format!("{}…", s.trim_end())
}

impl GuiApp {
    // ── Right column: devices → profiles + profile list ─────────────────────

    pub(crate) fn devices_profiles(&mut self, ui: &mut egui::Ui) {
        section_hint(ui, "Device → Profile", "auto-switch", |ui| {
            self.device_mapping_section(ui)
        });
        ui.add_space(12.0);
        let n = self.profiles.len();
        let filt = self.profile_filter.trim().to_lowercase();
        let saved = if filt.is_empty() {
            format!("{n} saved")
        } else {
            let shown = self
                .profiles
                .iter()
                .filter(|p| p.to_lowercase().contains(&filt))
                .count();
            format!("{shown}/{n} saved")
        };
        section_hint(ui, "Profiles", &saved, |ui| self.profiles_panel(ui));
        // Channels now lives under Effects (left column, matching the mock).
    }

    /// Channel layout + routing controls: shows the in→out channel counts +
    /// position labels, an L/R swap toggle, and a clear-routing button. The full
    /// N×N matrix editor is intentionally omitted (the in-place backends only do
    /// square remaps; swap covers the common case) — CLI `channel route` remains
    /// for power users.
    pub(crate) fn channels_section(&mut self, ui: &mut egui::Ui, s: &DaemonState) {
        // Layout summary: channel count (with in→out when a routing matrix
        // up/downmixes) and the position labels.
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = kit::SP_S;
            let count = if s.out_channels != 0 && s.out_channels != s.channels {
                format!("{} → {} ch", s.channels, s.out_channels)
            } else {
                format!("{} ch", s.channels)
            };
            ui.strong(count);
            let layout = s.channel_layout.join("  ");
            if !layout.is_empty() {
                ui.weak(layout);
            }
        });
        if s.channels < 2 {
            return;
        }

        ui.add_space(kit::SP_S);
        // L/R swap row (the swap(channels, 0, 1) routing matrix).
        let swap = RoutingMatrix::swap(s.channels, 0, 1);
        let is_swapped = s.routing.as_ref() == Some(&swap);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = kit::SP_S;
            let mut swapped = is_swapped;
            if kit::toggle(ui, &mut swapped) {
                if is_swapped {
                    self.queue(Command::ClearRouting);
                } else {
                    self.queue(Command::SwapChannels { a: 0, b: 1 });
                }
            }
            ui.label("Swap L / R")
                .on_hover_text("Swap the front-left and front-right channels");
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

        ui.add_space(kit::SP_S);
        // Per-channel EQ: reveals the per-band channel-target column + per-channel
        // FR curves. Always on for >2 channels; an opt-in toggle at 2ch (a styled
        // switch matching the rest of the UI, not a bare checkbox).
        if s.channels > 2 {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = kit::SP_S;
                let mut on = true;
                ui.add_enabled_ui(false, |ui| {
                    kit::toggle(ui, &mut on);
                });
                ui.label("Per-channel EQ");
                ui.weak("(multichannel)");
            });
        } else {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = kit::SP_S;
                let mut per_ch = self.per_channel_eq;
                if kit::toggle(ui, &mut per_ch) {
                    self.per_channel_eq = per_ch;
                }
                ui.label("Per-channel EQ").on_hover_text(
                    "Aim individual bands at specific channels (e.g. left- or right-only).",
                );
            });
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

    /// Profiles list (mockup port): a save row, an optional unsaved-changes
    /// banner, a filter field once the list grows, then one flat 2-line row per
    /// profile — folder icon · name + meta · hover-revealed Duplicate / Export /
    /// Delete. Click a row to load; double-click the name to rename inline.
    pub(crate) fn profiles_section(&mut self, ui: &mut egui::Ui) {
        const ROW_H: f32 = 38.0;
        // Reserved width for the hover action cluster (3 × CTRL_H + gaps) so the
        // name column never re-truncates when the icons appear on hover.
        const ACTION_W: f32 = 96.0;
        // Amber unsaved indicator. The palette has no `warn` slot; a fixed amber
        // reads as "attention" on every theme and is the only warn use here.
        let warn = egui::Color32::from_rgb(0xd8, 0xa2, 0x3f);
        let pal = self.palette;
        let accent_wash = {
            let a = pal.accent;
            egui::Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), 18)
        };
        let hover_wash = egui::Color32::from_white_alpha(8);

        // ── save row: Save pinned right, the name field fills the rest ─────────
        ui.horizontal(|ui| {
            ui.set_min_height(26.0);
            ui.spacing_mut().item_spacing.x = kit::SP_S;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let save_clicked = kit::icon_text_btn(
                    ui,
                    Icon::Save,
                    "Save",
                    true,
                    true,
                    "Save the current EQ as a profile with this name",
                );
                let resp = kit::text_field(
                    ui,
                    ui.available_width().max(110.0),
                    egui::Id::new("new_profile_name"),
                    &mut self.profile_name,
                    "new profile name…",
                    false,
                );
                // The dirty banner prefills + focuses this field on click.
                if self.focus_profile_name {
                    resp.request_focus();
                    self.focus_profile_name = false;
                }
                let go = save_clicked
                    || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                if go && !self.profile_name.trim().is_empty() {
                    let name = self.profile_name.trim().to_string();
                    // Overwriting an existing profile asks first; a new name saves now.
                    if self.profiles.iter().any(|p| p == &name) {
                        self.confirm = Some(Confirm::SaveProfile(name));
                    } else {
                        // Bundle the current measurement with the new profile.
                        self.reference.store_measurement_for(&name);
                        self.queue(Command::SaveProfile { name });
                        self.needs_meta = true;
                        self.dirty = false;
                    }
                    self.profile_name.clear();
                }
            });
        });

        let profiles = self.profiles.clone();
        if profiles.is_empty() {
            ui.add_space(6.0);
            ui.weak("(no profiles yet)");
            return;
        }
        let current = self.state.as_ref().and_then(|s| s.current_preset.clone());
        let mappings = self.mappings.clone();
        let (loaded_bands, loaded_preamp) = self
            .state
            .as_ref()
            .map(|s| (s.bands.len(), s.preamp_db))
            .unwrap_or((0, 0.0));

        // ── unsaved-changes banner (only when the live chain is dirty) ─────────
        if self.dirty {
            if let Some(active) = current.clone() {
                ui.add_space(8.0);
                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 32.0),
                    egui::Sense::click(),
                );
                let resp = resp.on_hover_text("Re-save the active profile with the current edits");
                let t = kit::tokens(ui);
                let cy = rect.center().y;
                let prefix = "Unsaved changes to ";
                let pre_w = kit::text_width(ui, kit::T_CAPTION, prefix);
                let save_w = kit::text_width(ui, kit::T_CAPTION, "Save →");
                let x0 = rect.left() + 24.0;
                let name_max = (rect.right() - 12.0 - save_w - 6.0 - (x0 + pre_w)).max(30.0);
                let nm = elide(ui, &active, kit::T_CAPTION, name_max);
                let p = ui.painter();
                p.rect_filled(rect, kit::R_CTRL, t.well);
                p.rect_filled(
                    egui::Rect::from_min_size(rect.min, egui::vec2(2.0, rect.height())),
                    0.0,
                    warn,
                );
                p.circle_filled(egui::pos2(rect.left() + 14.0, cy), 3.0, warn);
                let cap = egui::FontId::proportional(kit::T_CAPTION);
                p.text(
                    egui::pos2(x0, cy),
                    egui::Align2::LEFT_CENTER,
                    prefix,
                    cap.clone(),
                    t.dim,
                );
                p.text(
                    egui::pos2(x0 + pre_w, cy),
                    egui::Align2::LEFT_CENTER,
                    &nm,
                    cap.clone(),
                    pal.accent,
                );
                p.text(
                    egui::pos2(rect.right() - 12.0, cy),
                    egui::Align2::RIGHT_CENTER,
                    "Save →",
                    cap,
                    t.faint,
                );
                if resp.clicked() {
                    self.profile_name = active;
                    self.focus_profile_name = true;
                }
            }
        }

        // ── filter (only once the list is worth filtering) ─────────────────────
        if profiles.len() > 6 {
            ui.add_space(8.0);
            kit::text_field(
                ui,
                ui.available_width(),
                egui::Id::new("profile_filter"),
                &mut self.profile_filter,
                "filter profiles…",
                false,
            );
        }

        ui.add_space(6.0);

        let filt = self.profile_filter.trim().to_lowercase();
        let filtered: Vec<String> = if filt.is_empty() {
            profiles.clone()
        } else {
            profiles
                .iter()
                .filter(|p| p.to_lowercase().contains(&filt))
                .cloned()
                .collect()
        };
        if filtered.is_empty() {
            ui.weak("(no profiles match)");
            return;
        }
        let n = filtered.len();
        // Contiguous rows: no inter-row gap, so `ui.cursor()` is the true row top
        // (egui adds item_spacing at placement) and the wash/rail/hairline align.
        ui.spacing_mut().item_spacing.y = 0.0;
        for (idx, name) in filtered.iter().enumerate() {
            let active = current.as_deref() == Some(name.as_str());
            let mapped = mappings.iter().filter(|(_, p)| p == name).count();
            let editing = matches!(&self.rename, Some((from, _)) if from == name);

            // Row geometry + hover, computed up front so the cluster reveals and
            // the background wash can paint behind the row content. The accent rail
            // is painted *after* the content (below) so the folder button's hover
            // background can't punch a gap through its middle.
            let row_top = ui.cursor().min;
            let row_w = ui.available_width();
            let row_rect = egui::Rect::from_min_size(row_top, egui::vec2(row_w, ROW_H));
            let hovered = !editing && ui.rect_contains_pointer(row_rect);
            {
                let p = ui.painter();
                if active {
                    p.rect_filled(row_rect, 0.0, accent_wash);
                } else if hovered {
                    p.rect_filled(row_rect, 0.0, hover_wash);
                }
            }

            ui.horizontal(|ui| {
                ui.set_min_height(ROW_H);
                ui.spacing_mut().item_spacing.x = kit::SP_S;
                let t = kit::tokens(ui);
                // Left gutter so the accent rail doesn't crowd the folder icon.
                ui.add_space(kit::SP_XS);

                // Folder icon: open when loaded, closed otherwise; clicking loads.
                let icon = if active {
                    Icon::FolderOpen
                } else {
                    Icon::Folder
                };
                if kit::icon_btn(ui, icon, kit::CTRL_H, "Load this profile") {
                    self.queue(Command::LoadProfile { name: name.clone() });
                    self.dirty = false;
                }

                let region_w = if editing {
                    ui.available_width().max(60.0)
                } else {
                    (ui.available_width() - ACTION_W).max(60.0)
                };
                if editing {
                    // Inline rename: a transient text field only while editing.
                    let mut buf = match &self.rename {
                        Some((from, b)) if from == name => b.clone(),
                        _ => name.clone(),
                    };
                    let field_id = egui::Id::new(("pname", name));
                    let resp = kit::text_field(ui, region_w, field_id, &mut buf, "name", false);
                    // Focus once on entering edit (NOT every frame — that would
                    // re-grab focus so `lost_focus` never fires and the box never
                    // closes). A temp flag per field tracks "already focused".
                    let foc_key = field_id.with("foc");
                    let focused: bool = ui.data(|d| d.get_temp(foc_key).unwrap_or(false));
                    if !focused {
                        resp.request_focus();
                        ui.data_mut(|d| d.insert_temp(foc_key, true));
                    }
                    if resp.changed() {
                        self.rename = Some((name.clone(), buf.clone()));
                    }
                    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if enter {
                        let to = buf.trim().to_string();
                        if !to.is_empty() && to != *name {
                            self.reference.rename_profile_meas(name, &to);
                            self.queue(Command::RenameProfile {
                                from: name.clone(),
                                to,
                            });
                            self.needs_meta = true;
                        }
                        self.rename = None;
                        ui.data_mut(|d| d.remove::<bool>(foc_key));
                    } else if esc || resp.lost_focus() {
                        // Esc or click-away cancels the edit and dismisses the box.
                        self.rename = None;
                        ui.data_mut(|d| d.remove::<bool>(foc_key));
                    }
                } else {
                    let (r, rr) =
                        ui.allocate_exact_size(egui::vec2(region_w, ROW_H), egui::Sense::click());
                    let name_col = if active { t.accent } else { t.text };
                    let nm = elide(ui, name, kit::T_BODY, region_w - 4.0);
                    if active || mapped > 0 {
                        ui.painter().text(
                            egui::pos2(r.left(), r.top() + ROW_H * 0.34),
                            egui::Align2::LEFT_CENTER,
                            &nm,
                            egui::FontId::proportional(kit::T_BODY),
                            name_col,
                        );
                        let my = r.top() + ROW_H * 0.70;
                        if active {
                            // Honest meta — only the loaded profile's band count +
                            // preamp are known (live DaemonState); preamp colour-signed.
                            let plural = if loaded_bands == 1 { "" } else { "s" };
                            let pre = format!("loaded · {loaded_bands} band{plural} · ");
                            let pw = kit::text_width(ui, kit::T_CAPTION, &pre);
                            ui.painter().text(
                                egui::pos2(r.left(), my),
                                egui::Align2::LEFT_CENTER,
                                &pre,
                                egui::FontId::proportional(kit::T_CAPTION),
                                t.faint,
                            );
                            ui.painter().text(
                                egui::pos2(r.left() + pw, my),
                                egui::Align2::LEFT_CENTER,
                                format!("{loaded_preamp:+.1} dB"),
                                egui::FontId::proportional(kit::T_CAPTION),
                                gain_color(loaded_preamp, &pal),
                            );
                        } else {
                            let meta = format!(
                                "auto-loads on {mapped} device{}",
                                if mapped == 1 { "" } else { "s" }
                            );
                            ui.painter().text(
                                egui::pos2(r.left(), my),
                                egui::Align2::LEFT_CENTER,
                                &meta,
                                egui::FontId::proportional(kit::T_CAPTION),
                                t.faint,
                            );
                        }
                    } else {
                        ui.painter().text(
                            egui::pos2(r.left(), r.center().y),
                            egui::Align2::LEFT_CENTER,
                            &nm,
                            egui::FontId::proportional(kit::T_BODY),
                            name_col,
                        );
                    }
                    let rr = rr.on_hover_text("click = load · double-click = rename");
                    if rr.clicked() {
                        self.queue(Command::LoadProfile { name: name.clone() });
                        self.dirty = false;
                    }
                    if rr.double_clicked() {
                        self.rename = Some((name.clone(), name.clone()));
                    }

                    // Hover-revealed action cluster (width reserved above so the
                    // name never re-truncates). Delete lives here, not always-on.
                    if hovered {
                        ui.spacing_mut().item_spacing.x = kit::SP_XS;
                        if kit::icon_btn(ui, Icon::Copy, kit::CTRL_H, "Duplicate this profile") {
                            let mut to = format!("{name} copy");
                            let mut i = 2;
                            while self.profiles.iter().any(|p| p == &to) {
                                to = format!("{name} copy {i}");
                                i += 1;
                            }
                            self.reference.duplicate_profile_meas(name, &to);
                            self.queue(Command::DuplicateProfile {
                                from: name.clone(),
                                to,
                            });
                            self.queue(Command::ListProfiles);
                        }
                        if kit::icon_btn(ui, Icon::Up, kit::CTRL_H, "Export this profile to a file")
                        {
                            self.open_export_dialog_for(name.clone());
                        }
                        if kit::icon_btn(ui, Icon::Trash, kit::CTRL_H, "Delete this profile") {
                            self.confirm = Some(Confirm::DeleteProfile(name.clone()));
                        }
                    }
                }
            });
            // Accent rail on top of the content so the folder button's hover bg
            // can't break it (full row height, left edge).
            if active {
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(row_rect.min, egui::vec2(2.0, ROW_H)),
                    0.0,
                    pal.accent,
                );
            }
            if idx + 1 < n {
                let line = kit::tokens(ui).line;
                ui.painter().hline(
                    row_rect.x_range(),
                    row_rect.bottom(),
                    egui::Stroke::new(1.0, line),
                );
            }
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

        // Manual two-line rows (mockup `.drow`): a status dot, the device name over
        // a transport/rate sub-line, a profile picker, then a LIVE tag (active) or
        // a forget ✕ (others). A hairline rules each row but the last.
        const DD_W: f32 = 116.0;
        const TAIL_W: f32 = 44.0;
        const ROW_H: f32 = 40.0;
        let full_w = ui.available_width();
        let name_w = (full_w - 14.0 - DD_W - TAIL_W - 3.0 * kit::SP_S).max(80.0);
        let n = s.sink_descriptions.len();
        for (idx, (node, _desc)) in s.sink_descriptions.iter().enumerate() {
            let here = present.contains(node.as_str());
            let is_active = active == Some(node.as_str());
            let dot_col = if is_active {
                self.palette.boost
            } else if here {
                self.palette.neutral
            } else {
                self.palette.grid
            };
            let row = ui.horizontal(|ui| {
                ui.set_min_height(ROW_H);
                ui.spacing_mut().item_spacing.x = kit::SP_S;
                let t = kit::tokens(ui);
                // Status dot: filled when present/active, hollow when remembered.
                let (dr, _) = ui.allocate_exact_size(egui::vec2(12.0, ROW_H), egui::Sense::hover());
                if is_active || here {
                    ui.painter().circle_filled(dr.center(), 4.0, dot_col);
                } else {
                    ui.painter()
                        .circle_stroke(dr.center(), 3.5, egui::Stroke::new(1.3, dot_col));
                }
                // Name (faded if long) over a transport · rate sub-line. A
                // disconnected device is dimmed so present ones read first.
                let bg = ui.visuals().faint_bg_color;
                let (rg, _) =
                    ui.allocate_exact_size(egui::vec2(name_w, ROW_H), egui::Sense::hover());
                let name_col = if is_active || here { t.text } else { t.dim };
                let name_cy = rg.top() + ROW_H * 0.34;
                kit::fade_text(
                    ui,
                    egui::Rect::from_min_max(
                        egui::pos2(rg.left(), name_cy - 9.0),
                        egui::pos2(rg.right(), name_cy + 9.0),
                    ),
                    &s.sink_label(node),
                    egui::FontId::proportional(kit::T_BODY),
                    name_col,
                    bg,
                );
                let sub = if is_active {
                    format!(
                        "{} · {:.1} kHz",
                        device_transport(node),
                        s.sample_rate / 1000.0
                    )
                } else if here {
                    device_transport(node).to_string()
                } else {
                    format!("{} · disconnected", device_transport(node))
                };
                let sub_cy = rg.top() + ROW_H * 0.70;
                kit::fade_text(
                    ui,
                    egui::Rect::from_min_max(
                        egui::pos2(rg.left(), sub_cy - 8.0),
                        egui::pos2(rg.right(), sub_cy + 8.0),
                    ),
                    &sub,
                    egui::FontId::monospace(kit::T_CAPTION - 0.5),
                    t.faint,
                    bg,
                );
                // Profile picker: index 0 = "—" (unmapped), the rest map 1:1.
                let cur_text = map.get(node).cloned().unwrap_or_else(|| "—".to_string());
                let mut opts = vec!["—".to_string()];
                opts.extend(profiles.iter().cloned());
                let refs: Vec<&str> = opts.iter().map(String::as_str).collect();
                if let Some(i) = kit::dropdown(
                    ui,
                    DD_W,
                    kit::CTRL_H,
                    egui::Id::new(("devmap", node)),
                    &cur_text,
                    &refs,
                ) {
                    if i == 0 {
                        self.queue(Command::UnmapOutputFor {
                            node_name: node.clone(),
                        });
                    } else {
                        self.queue(Command::MapOutputFor {
                            node_name: node.clone(),
                            profile: profiles[i - 1].clone(),
                        });
                    }
                    self.needs_meta = true;
                }
                // Tail: a LIVE tag on the active device (can't forget what's
                // playing), else a forget ✕.
                if is_active {
                    let (tr, _) =
                        ui.allocate_exact_size(egui::vec2(TAIL_W, ROW_H), egui::Sense::hover());
                    let tag = egui::Rect::from_center_size(tr.center(), egui::vec2(38.0, 16.0));
                    ui.painter().rect_stroke(
                        tag,
                        3.0,
                        egui::Stroke::new(1.0, self.palette.boost.gamma_multiply(0.7)),
                        egui::StrokeKind::Inside,
                    );
                    ui.painter().text(
                        tag.center(),
                        egui::Align2::CENTER_CENTER,
                        "LIVE",
                        egui::FontId::proportional(kit::T_CAPTION - 1.5),
                        self.palette.boost,
                    );
                } else if kit::icon_btn(ui, Icon::Close, kit::CTRL_H, "Forget this device") {
                    self.queue(Command::ForgetSink {
                        node_name: node.clone(),
                    });
                    self.needs_meta = true;
                }
            });
            if idx + 1 < n {
                let line = kit::tokens(ui).line;
                let rr = row.response.rect;
                ui.painter()
                    .hline(rr.x_range(), rr.bottom(), egui::Stroke::new(1.0, line));
            }
        }
    }
}

/// Best-effort transport label from a sink node name (for the device sub-line).
fn device_transport(node: &str) -> &'static str {
    let n = node.to_ascii_lowercase();
    if n.contains("bluez") || n.contains("blue") || n.contains("a2dp") {
        "Bluetooth"
    } else if n.contains("usb") {
        "USB"
    } else if n.contains("hdmi") {
        "HDMI"
    } else if n.contains("analog") || n.contains("pci") || n.contains("built") {
        "Analog"
    } else {
        "Output"
    }
}
