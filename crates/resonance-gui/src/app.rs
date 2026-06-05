//! egui/eframe front-end for the Resonance daemon.
//!
//! All daemon mutations are expressed as `Command`s collected into `pending`
//! during the frame, then dispatched synchronously after the UI is built. The
//! authoritative `DaemonState` is re-fetched immediately afterwards (and on a
//! periodic poll) so widgets always reflect the daemon.

use crate::browser::Browser;
use crate::curve;
use crate::ipc::IpcClient;
use eframe::egui;
use resonance_ipc::{BandType, Command, DaemonState, FxEffectId, Response};
use std::time::{Duration, Instant};

/// Repaint cadence: ~144 fps. Rendering reads the *smoothed* spectrum, so it
/// stays buttery even though the underlying data arrives far slower.
const FRAME_INTERVAL: Duration = Duration::from_micros(6_944);
/// Daemon state poll: ~30 Hz. Decoupled from the draw rate — polling every
/// frame both hammered the socket and made the bars jitter.
const STATE_INTERVAL: Duration = Duration::from_millis(33);
/// Profiles/mappings rarely change — poll them far less often than state.
const META_INTERVAL: Duration = Duration::from_millis(1000);
const RECONNECT_INTERVAL: Duration = Duration::from_millis(1000);

/// Spectrum envelope time constants: bars snap up, glide down.
const SPECTRUM_ATTACK_TAU: f32 = 0.020;
const SPECTRUM_DECAY_TAU: f32 = 0.20;

const EFFECTS: [(FxEffectId, &str); 5] = [
    (FxEffectId::Fidelity, "Fidelity"),
    (FxEffectId::Ambience, "Ambience"),
    (FxEffectId::Surround, "Surround"),
    (FxEffectId::DynamicBoost, "Dynamic Boost"),
    (FxEffectId::Bass, "Bass"),
];

const BAND_TYPES: [BandType; 8] = [
    BandType::Peaking,
    BandType::LowShelf,
    BandType::HighShelf,
    BandType::LowPass,
    BandType::HighPass,
    BandType::BandPass,
    BandType::Notch,
    BandType::AllPass,
];

enum Dialog {
    None,
    LoadPreset(Browser),
}

pub struct GuiApp {
    ipc: Option<IpcClient>,
    state: Option<DaemonState>,
    profiles: Vec<String>,
    mappings: Vec<(String, String)>,
    status: String,
    last_poll: Instant,
    last_meta: Instant,
    last_reconnect: Instant,
    pending: Vec<Command>,
    needs_meta: bool,
    dialog: Dialog,
    selected_band: usize,
    drag_band: Option<usize>,
    profile_name: String,
    /// Smoothed spectrum bar heights + last animation tick.
    spectrum_display: Vec<f32>,
    last_anim: Instant,
}

impl GuiApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self {
            ipc: None,
            state: None,
            profiles: Vec::new(),
            mappings: Vec::new(),
            status: String::new(),
            last_poll: Instant::now(),
            last_meta: Instant::now() - META_INTERVAL,
            last_reconnect: Instant::now() - RECONNECT_INTERVAL,
            pending: Vec::new(),
            needs_meta: false,
            dialog: Dialog::None,
            selected_band: 0,
            drag_band: None,
            profile_name: String::new(),
            spectrum_display: Vec::new(),
            last_anim: Instant::now(),
        };
        app.try_connect();
        app
    }

    // ── Connection / polling ────────────────────────────────────────────────

    fn try_connect(&mut self) {
        match IpcClient::connect() {
            Ok(c) => {
                self.ipc = Some(c);
                self.status = "connected".into();
                self.refresh_state();
                self.refresh_meta();
            }
            Err(e) => {
                self.status = format!("not connected: {e}");
                self.ipc = None;
            }
        }
        self.last_reconnect = Instant::now();
    }

    /// Pull a fresh `DaemonState` snapshot (cheap; runs every frame).
    fn refresh_state(&mut self) {
        let Some(ipc) = self.ipc.as_mut() else {
            return;
        };
        match ipc.get_state() {
            Ok(s) => {
                self.state = Some(s);
                if self.status.starts_with("error") {
                    self.status.clear();
                }
            }
            Err(e) => {
                self.status = format!("error: {e}");
                self.state = None;
                self.ipc = None;
            }
        }
    }

    /// Pull profiles + output mappings (runs on a slow timer / on demand).
    fn refresh_meta(&mut self) {
        let Some(ipc) = self.ipc.as_mut() else {
            return;
        };
        if let Ok(Response::PresetList(p)) = ipc.send_recv(Command::ListProfiles) {
            self.profiles = p;
        }
        if let Ok(Response::Mappings(m)) = ipc.send_recv(Command::ListMappings) {
            self.mappings = m;
        }
        self.last_meta = Instant::now();
    }

    fn dispatch(&mut self) {
        if !self.pending.is_empty() {
            let cmds = std::mem::take(&mut self.pending);
            if let Some(ipc) = self.ipc.as_mut() {
                for cmd in cmds {
                    if let Err(e) = ipc.send(cmd) {
                        self.status = format!("error: {e}");
                        self.ipc = None;
                        break;
                    }
                }
            }
            self.refresh_state();
        }
        if self.needs_meta {
            self.needs_meta = false;
            self.refresh_meta();
        }
    }

    fn queue(&mut self, cmd: Command) {
        self.pending.push(cmd);
    }
}

impl eframe::App for GuiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Reconnect / poll.
        if self.ipc.is_none() {
            if self.last_reconnect.elapsed() >= RECONNECT_INTERVAL {
                self.try_connect();
            }
        } else {
            if self.last_poll.elapsed() >= STATE_INTERVAL {
                self.last_poll = Instant::now();
                self.refresh_state();
            }
            if self.last_meta.elapsed() >= META_INTERVAL {
                self.refresh_meta();
            }
        }

        egui::Panel::top("toolbar").show_inside(ui, |ui| self.toolbar(ui));
        egui::Panel::right("side")
            .resizable(true)
            .default_size(280.0)
            .show_inside(ui, |ui| self.side_panel(ui));
        egui::CentralPanel::default().show_inside(ui, |ui| self.central(ui));

        let ctx = ui.ctx().clone();
        self.preset_dialog(&ctx);

        self.dispatch();

        // Drive ~144 fps repaint so spectrum/curve stay smooth.
        ctx.request_repaint_after(FRAME_INTERVAL);
    }
}

// ── UI sections ─────────────────────────────────────────────────────────────

impl GuiApp {
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let state = self.state.clone();
        ui.horizontal(|ui| {
            ui.heading("Resonance");
            ui.separator();

            let enabled = state.as_ref().map(|s| s.enabled).unwrap_or(false);
            let mut power = enabled;
            if ui
                .add_enabled(state.is_some(), egui::Checkbox::new(&mut power, "Power"))
                .changed()
            {
                self.queue(Command::SetPower { enabled: power });
            }

            ui.separator();
            ui.label("Preamp");
            if let Some(s) = &state {
                let mut db = s.preamp_db;
                if ui
                    .add(
                        egui::Slider::new(&mut db, -20.0..=20.0)
                            .suffix(" dB")
                            .fixed_decimals(1),
                    )
                    .changed()
                {
                    self.queue(Command::SetPreamp { db });
                }
            } else {
                ui.add_enabled(false, egui::Slider::new(&mut 0.0, -20.0..=20.0));
            }

            ui.separator();
            if ui.button("Load preset…").clicked() {
                let start =
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                self.dialog = Dialog::LoadPreset(Browser::new(start));
            }
            if let Some(p) = state.as_ref().and_then(|s| s.current_preset.as_ref()) {
                ui.label(format!("▸ {p}"));
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(&self.status);
            });
        });
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        let Some(state) = self.state.clone() else {
            ui.centered_and_justified(|ui| {
                ui.label("Waiting for daemon… start it with `resonanced`.");
            });
            return;
        };

        self.eq_curve(ui, &state);
        ui.add_space(6.0);
        self.spectrum(ui, &state);
        ui.add_space(6.0);
        ui.separator();

        egui::ScrollArea::vertical().show(ui, |ui| {
            self.effects_section(ui, &state);
            ui.add_space(8.0);
            ui.separator();
            self.bands_section(ui, &state);
        });
    }

    // ── EQ response curve (draggable nodes) ─────────────────────────────────

    fn eq_curve(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        let height = 220.0;
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::click_and_drag(),
        );
        let painter = ui.painter().with_clip_rect(rect);
        let visuals = ui.visuals();

        painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);

        let db = curve::DB_RANGE;
        let x_of = |logf: f64| -> f32 {
            rect.left()
                + ((logf - curve::LOG_MIN) / (curve::LOG_MAX - curve::LOG_MIN)) as f32
                    * rect.width()
        };
        let y_of = |gain: f64| -> f32 {
            rect.top() + (1.0 - ((gain + db) / (2.0 * db)) as f32) * rect.height()
        };
        let logf_of = |x: f32| -> f64 {
            curve::LOG_MIN
                + ((x - rect.left()) / rect.width()) as f64 * (curve::LOG_MAX - curve::LOG_MIN)
        };
        let db_of =
            |y: f32| -> f64 { ((1.0 - (y - rect.top()) / rect.height()) as f64) * 2.0 * db - db };

        // Horizontal dB grid lines.
        let grid = egui::Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.3));
        for g in [-12.0, -6.0, 0.0, 6.0, 12.0] {
            let y = y_of(g);
            let stroke = if g == 0.0 {
                egui::Stroke::new(1.0, visuals.weak_text_color())
            } else {
                grid
            };
            painter.line_segment(
                [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                stroke,
            );
            painter.text(
                egui::pos2(rect.left() + 2.0, y),
                egui::Align2::LEFT_CENTER,
                format!("{g:+.0}"),
                egui::FontId::monospace(9.0),
                visuals.weak_text_color(),
            );
        }
        // Vertical frequency grid + labels.
        for (logf, label) in curve::x_axis_ticks() {
            let x = x_of(logf);
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                grid,
            );
            painter.text(
                egui::pos2(x, rect.bottom() - 2.0),
                egui::Align2::CENTER_BOTTOM,
                label,
                egui::FontId::monospace(9.0),
                visuals.weak_text_color(),
            );
        }

        // Response polyline.
        let pts = curve::curve_points(&state.bands, state.sample_rate, 240);
        let line: Vec<egui::Pos2> = pts
            .iter()
            .map(|&(lf, g)| egui::pos2(x_of(lf), y_of(g)))
            .collect();
        painter.add(egui::Shape::line(
            line,
            egui::Stroke::new(2.0, egui::Color32::from_rgb(80, 200, 255)),
        ));

        // Drag handling: pick / move the nearest node.
        if response.drag_started() {
            if let Some(p) = response.interact_pointer_pos() {
                let mut best: Option<(usize, f32)> = None;
                for (i, b) in state.bands.iter().enumerate() {
                    let node = egui::pos2(x_of(curve::clampf_log(b.freq)), y_of(b.gain_db));
                    let d = node.distance(p);
                    if d < 14.0 && best.map(|(_, bd)| d < bd).unwrap_or(true) {
                        best = Some((i, d));
                    }
                }
                if let Some((i, _)) = best {
                    self.drag_band = Some(i);
                    self.selected_band = i;
                }
            }
        }
        if response.dragged() {
            if let (Some(i), Some(p)) = (self.drag_band, response.interact_pointer_pos()) {
                if let Some(b) = state.bands.get(i) {
                    let freq = 10f64.powf(logf_of(p.x)).clamp(20.0, 20000.0);
                    let gain = db_of(p.y).clamp(-20.0, 20.0);
                    self.queue(Command::SetBand {
                        index: i,
                        freq,
                        gain_db: gain,
                        q: b.q,
                    });
                }
            }
        }
        if response.drag_stopped() {
            self.drag_band = None;
        }
        // Double-click empty area → add a peaking band there.
        if response.double_clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                let freq = 10f64.powf(logf_of(p.x)).clamp(20.0, 20000.0);
                let gain = db_of(p.y).clamp(-20.0, 20.0);
                self.queue(Command::AddBand {
                    band_type: BandType::Peaking,
                    freq,
                    gain_db: gain,
                    q: 1.4,
                });
            }
        }

        // Band node markers.
        for (i, b) in state.bands.iter().enumerate() {
            if !b.enabled {
                continue;
            }
            let center = egui::pos2(x_of(curve::clampf_log(b.freq)), y_of(b.gain_db));
            let selected = i == self.selected_band;
            let color = if selected {
                egui::Color32::YELLOW
            } else {
                egui::Color32::from_rgb(80, 200, 255)
            };
            painter.circle_filled(center, if selected { 6.0 } else { 4.0 }, color);
            painter.circle_stroke(
                center,
                if selected { 6.0 } else { 4.0 },
                egui::Stroke::new(1.0, visuals.extreme_bg_color),
            );
        }
    }

    // ── Spectrum bars ───────────────────────────────────────────────────────

    fn spectrum(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        let height = 70.0;
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::hover(),
        );
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);

        let bins = &state.spectrum;
        if bins.is_empty() {
            return;
        }
        let n = bins.len();

        // Smooth each bar toward the latest value: fast rise, slow fall. This
        // is what kills the flicker — the data jumps, the bars don't.
        let dt = self.last_anim.elapsed().as_secs_f32().min(0.1);
        self.last_anim = Instant::now();
        if self.spectrum_display.len() != n {
            self.spectrum_display = vec![0.0; n];
        }
        for (disp, &raw) in self.spectrum_display.iter_mut().zip(bins.iter()) {
            let target = raw.clamp(0.0, 1.0);
            let tau = if target > *disp {
                SPECTRUM_ATTACK_TAU
            } else {
                SPECTRUM_DECAY_TAU
            };
            let coeff = 1.0 - (-dt / tau).exp();
            *disp += (target - *disp) * coeff;
        }

        let gap = 2.0;
        let bw = (rect.width() - gap * (n as f32 + 1.0)) / n as f32;
        for (i, &v) in self.spectrum_display.iter().enumerate() {
            let h = (v.clamp(0.0, 1.0)) * (rect.height() - 4.0);
            let x0 = rect.left() + gap + i as f32 * (bw + gap);
            let bar = egui::Rect::from_min_max(
                egui::pos2(x0, rect.bottom() - 2.0 - h),
                egui::pos2(x0 + bw, rect.bottom() - 2.0),
            );
            let t = v.clamp(0.0, 1.0);
            let color = egui::Color32::from_rgb(
                (60.0 + 195.0 * t) as u8,
                (200.0 - 80.0 * t) as u8,
                255 - (155.0 * t) as u8,
            );
            painter.rect_filled(bar, 1.0, color);
        }
    }

    // ── Effects ─────────────────────────────────────────────────────────────

    fn effects_section(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        ui.heading("Effects");
        egui::Grid::new("effects_grid")
            .num_columns(3)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                for (id, name) in EFFECTS {
                    let (mut intensity, mut on) = effect_values(state, id);
                    let bipolar = matches!(id, FxEffectId::Surround | FxEffectId::Bass);
                    let min = if bipolar { -1.0 } else { 0.0 };

                    if ui.checkbox(&mut on, "").changed() {
                        self.queue(Command::SetEffectEnabled {
                            effect: id,
                            enabled: on,
                        });
                    }
                    ui.label(name);
                    if ui
                        .add_enabled(
                            on,
                            egui::Slider::new(&mut intensity, min..=1.0)
                                .custom_formatter(|v, _| format!("{:+.0}%", v * 100.0))
                                .custom_parser(|s| {
                                    s.trim_end_matches('%')
                                        .parse::<f64>()
                                        .ok()
                                        .map(|v| v / 100.0)
                                }),
                        )
                        .changed()
                    {
                        self.queue(Command::SetEffectIntensity {
                            effect: id,
                            value: intensity,
                        });
                    }
                    ui.end_row();
                }
            });
    }

    // ── EQ bands table ──────────────────────────────────────────────────────

    fn bands_section(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        ui.horizontal(|ui| {
            ui.heading("EQ bands");
            if ui.button("➕ Add band").clicked() {
                self.queue(Command::AddBand {
                    band_type: BandType::Peaking,
                    freq: 1000.0,
                    gain_db: 0.0,
                    q: 1.4,
                });
            }
        });

        egui::Grid::new("bands_grid")
            .num_columns(7)
            .striped(true)
            .spacing([10.0, 4.0])
            .show(ui, |ui| {
                ui.label("On");
                ui.label("Type");
                ui.label("Freq (Hz)");
                ui.label("Gain (dB)");
                ui.label("Q");
                ui.label("");
                ui.label("");
                ui.end_row();

                for (i, b) in state.bands.iter().enumerate() {
                    let selected = i == self.selected_band;

                    let mut on = b.enabled;
                    if ui.checkbox(&mut on, "").changed() {
                        self.queue(Command::SetBandEnabled {
                            index: i,
                            enabled: on,
                        });
                    }

                    // Type combo.
                    let mut bt = b.band_type;
                    egui::ComboBox::from_id_salt(("bt", i))
                        .selected_text(bt.abbrev())
                        .width(56.0)
                        .show_ui(ui, |ui| {
                            for cand in BAND_TYPES {
                                if ui.selectable_value(&mut bt, cand, cand.full()).clicked() {}
                            }
                        });
                    if bt != b.band_type {
                        self.queue(Command::SetBandType {
                            index: i,
                            band_type: bt,
                        });
                    }

                    // Freq / gain / Q drag values.
                    let mut freq = b.freq;
                    let mut gain = b.gain_db;
                    let mut q = b.q;
                    let f_changed = ui
                        .add(
                            egui::DragValue::new(&mut freq)
                                .speed(2.0)
                                .range(20.0..=20000.0)
                                .fixed_decimals(0),
                        )
                        .changed();
                    let g_changed = ui
                        .add(
                            egui::DragValue::new(&mut gain)
                                .speed(0.1)
                                .range(-20.0..=20.0)
                                .fixed_decimals(1),
                        )
                        .changed();
                    let q_changed = ui
                        .add(
                            egui::DragValue::new(&mut q)
                                .speed(0.02)
                                .range(0.1..=20.0)
                                .fixed_decimals(2),
                        )
                        .changed();
                    if f_changed || g_changed || q_changed {
                        self.queue(Command::SetBand {
                            index: i,
                            freq,
                            gain_db: gain,
                            q,
                        });
                    }

                    if ui
                        .selectable_label(selected, "◉")
                        .on_hover_text("select")
                        .clicked()
                    {
                        self.selected_band = i;
                    }
                    if ui.button("🗑").on_hover_text("remove").clicked() {
                        self.queue(Command::RemoveBand { index: i });
                    }
                    ui.end_row();
                }
            });

        if self.selected_band >= state.bands.len() {
            self.selected_band = state.bands.len().saturating_sub(1);
        }
    }

    // ── Side panel: output, profiles, mappings, info ────────────────────────

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        let state = self.state.clone();

        ui.heading("Output device");
        if let Some(s) = &state {
            let current = s
                .preferred_output
                .clone()
                .or_else(|| s.active_output.clone())
                .unwrap_or_default();
            let mut sel = current.clone();
            egui::ComboBox::from_id_salt("sink")
                .selected_text(short_name(&sel))
                .width(ui.available_width() - 10.0)
                .show_ui(ui, |ui| {
                    for sink in &s.available_sinks {
                        ui.selectable_value(&mut sel, sink.clone(), short_name(sink));
                    }
                });
            if sel != current && !sel.is_empty() {
                self.queue(Command::SetOutputTarget { node_name: sel });
            }
            if let Some(mp) = &s.mapped_profile {
                ui.label(format!("mapped profile: {mp}"));
            }
            ui.horizontal(|ui| {
                if ui.button("Map active→profile").clicked() {
                    if let Some(name) = self.profiles.first().cloned() {
                        // Map to currently selected profile name field if set.
                        let target = if self.profile_name.is_empty() {
                            name
                        } else {
                            self.profile_name.clone()
                        };
                        self.queue(Command::MapOutput { profile: target });
                        self.needs_meta = true;
                    }
                }
                if ui.button("Unmap").clicked() {
                    self.queue(Command::UnmapOutput);
                    self.needs_meta = true;
                }
            });
        } else {
            ui.label("(no daemon)");
        }

        ui.separator();
        ui.heading("Profiles");
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.profile_name);
            if ui.button("Save").clicked() && !self.profile_name.trim().is_empty() {
                self.queue(Command::SaveProfile {
                    name: self.profile_name.trim().to_string(),
                });
                self.needs_meta = true;
            }
        });
        let profiles = self.profiles.clone();
        egui::ScrollArea::vertical()
            .max_height(160.0)
            .show(ui, |ui| {
                for name in &profiles {
                    ui.horizontal(|ui| {
                        if ui.button("Load").clicked() {
                            self.queue(Command::LoadProfile { name: name.clone() });
                        }
                        if ui.button("🗑").clicked() {
                            self.queue(Command::DeleteProfile { name: name.clone() });
                            self.needs_meta = true;
                        }
                        if ui
                            .button("Map")
                            .on_hover_text("map active output")
                            .clicked()
                        {
                            self.queue(Command::MapOutput {
                                profile: name.clone(),
                            });
                            self.needs_meta = true;
                        }
                        ui.label(name);
                    });
                }
            });

        if !self.mappings.is_empty() {
            ui.separator();
            ui.heading("Output mappings");
            for (out, prof) in &self.mappings {
                ui.label(format!("{} → {}", short_name(out), prof));
            }
        }

        ui.separator();
        if let Some(s) = &state {
            ui.label(format!("{} Hz · {} ch", s.sample_rate as u32, s.channels));
        }
    }

    // ── Preset load dialog ──────────────────────────────────────────────────

    fn preset_dialog(&mut self, ctx: &egui::Context) {
        let Dialog::LoadPreset(browser) = &mut self.dialog else {
            return;
        };
        let mut open = true;
        let mut close = false;
        let mut to_load: Option<String> = None;

        egui::Window::new("Load preset")
            .open(&mut open)
            .resizable(true)
            .default_size([640.0, 420.0])
            .show(ctx, |ui| {
                ui.label(browser.cwd.display().to_string());
                ui.separator();
                let mut go_parent = false;
                let mut select: Option<usize> = None;
                let mut activate: Option<usize> = None;
                ui.columns(2, |cols| {
                    // File list.
                    egui::ScrollArea::vertical()
                        .id_salt("files")
                        .show(&mut cols[0], |ui| {
                            if ui.button("⬆ parent").clicked() {
                                go_parent = true;
                            }
                            for (i, it) in browser.entries.iter().enumerate() {
                                let label = if it.is_dir {
                                    format!("📁 {}", it.name)
                                } else {
                                    format!("📄 {}", it.name)
                                };
                                let resp = ui.selectable_label(i == browser.cursor, label);
                                if resp.clicked() {
                                    select = Some(i);
                                }
                                if resp.double_clicked() {
                                    activate = Some(i);
                                }
                            }
                        });
                    // Preview.
                    egui::ScrollArea::vertical()
                        .id_salt("preview")
                        .show(&mut cols[1], |ui| {
                            for line in &browser.preview {
                                ui.monospace(line);
                            }
                        });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Load selected").clicked() {
                        activate = Some(browser.cursor);
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });

                if go_parent {
                    browser.parent();
                } else if let Some(i) = select {
                    browser.select(i);
                }
                if let Some(i) = activate {
                    if let Some(path) = browser.activate(i) {
                        to_load = Some(path);
                    }
                }
            });

        if let Some(path) = to_load {
            self.queue(Command::LoadPreset { path });
            close = true;
        }
        if !open || close {
            self.dialog = Dialog::None;
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn effect_values(state: &DaemonState, id: FxEffectId) -> (f64, bool) {
    let e = &state.effects;
    match id {
        FxEffectId::Fidelity => (e.fidelity_intensity, e.fidelity_enabled),
        FxEffectId::Ambience => (e.ambience_intensity, e.ambience_enabled),
        FxEffectId::Surround => (e.surround_intensity, e.surround_enabled),
        FxEffectId::DynamicBoost => (e.dynamic_boost_intensity, e.dynamic_boost_enabled),
        FxEffectId::Bass => (e.bass_intensity, e.bass_enabled),
    }
}

/// Last path segment of a PipeWire node name, for compact display.
fn short_name(node: &str) -> String {
    if node.is_empty() {
        return "(default)".to_string();
    }
    node.rsplit('.').next().unwrap_or(node).to_string()
}
