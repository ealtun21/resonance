//! egui/eframe front-end for the Resonance daemon.
//!
//! All daemon mutations are expressed as `Command`s collected into `pending`
//! during the frame, then dispatched synchronously after the UI is built. The
//! authoritative `DaemonState` is re-fetched immediately afterwards (and on a
//! periodic poll) so widgets always reflect the daemon.

use crate::browser::Browser;
use crate::curve;
use crate::ipc::IpcClient;
use crate::theme::{Palette, Theme};
use eframe::egui;
use resonance_ipc::{
    BandState, BandType, Command, DaemonState, EffectsState, FxEffectId, Response, service,
};
use std::time::{Duration, Instant};

/// Consecutive edits within this window coalesce into one undo entry (a drag
/// gesture becomes a single undo step).
const UNDO_COALESCE: Duration = Duration::from_millis(400);

/// A restorable snapshot of the editable chain state (undo/redo).
#[derive(Clone)]
struct Snapshot {
    preamp_db: f64,
    enabled: bool,
    bands: Vec<BandState>,
    effects: EffectsState,
}

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

/// A zero-arg systemd service action (start/stop/restart/…).
type ServiceFn = fn() -> std::io::Result<()>;

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
    /// True while the active curve drag edits Q (right button) vs freq+gain.
    drag_q: bool,
    profile_name: String,
    /// Smoothed spectrum bar heights + last animation tick.
    spectrum_display: Vec<f32>,
    last_anim: Instant,
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    /// Start of the current edit burst (for undo coalescing).
    last_edit: Option<Instant>,
    /// While `Some` and in the future, the clip indicator flashes.
    clip_until: Option<Instant>,
    /// Active colour theme + its semantic palette (kept in sync).
    theme: Theme,
    palette: Palette,
    /// Band pinned to vertical (gain-only) movement via double-right-click.
    vlock: Option<usize>,
    /// Cached systemd user-service status; refreshed on a slow timer.
    daemon_status: service::Status,
    last_service_poll: Instant,
}

impl GuiApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_symbol_fonts(&cc.egui_ctx);
        let theme = Theme::System;
        cc.egui_ctx.set_visuals(theme.visuals());
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
            drag_q: false,
            profile_name: String::new(),
            spectrum_display: Vec::new(),
            last_anim: Instant::now(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: None,
            clip_until: None,
            theme,
            palette: theme.palette(),
            vlock: None,
            daemon_status: service::status(),
            last_service_poll: Instant::now(),
        };
        app.try_connect();
        app
    }

    /// Switch theme: store it, refresh the cached palette, and push new visuals.
    fn set_theme(&mut self, ctx: &egui::Context, theme: Theme) {
        self.theme = theme;
        self.palette = theme.palette();
        ctx.set_visuals(theme.visuals());
    }

    // ── Undo / redo ─────────────────────────────────────────────────────────

    fn snapshot(&self) -> Option<Snapshot> {
        let s = self.state.as_ref()?;
        Some(Snapshot {
            preamp_db: s.preamp_db,
            enabled: s.enabled,
            bands: s.bands.clone(),
            effects: s.effects.clone(),
        })
    }

    /// Queue an edit command, recording an undo snapshot at the start of each
    /// edit burst (consecutive edits within `UNDO_COALESCE` coalesce).
    fn queue_edit(&mut self, cmd: Command) {
        let now = Instant::now();
        let coalesce = self
            .last_edit
            .map(|t| now.duration_since(t) < UNDO_COALESCE)
            .unwrap_or(false);
        if !coalesce {
            if let Some(s) = self.snapshot() {
                self.undo_stack.push(s);
                if self.undo_stack.len() > 100 {
                    self.undo_stack.remove(0);
                }
                self.redo_stack.clear();
            }
        }
        self.last_edit = Some(now);
        self.queue(cmd);
    }

    fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop() {
            if let Some(cur) = self.snapshot() {
                self.redo_stack.push(cur);
            }
            self.apply_snapshot(&prev);
            self.last_edit = None;
            self.status = "undo".into();
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo_stack.pop() {
            if let Some(cur) = self.snapshot() {
                self.undo_stack.push(cur);
            }
            self.apply_snapshot(&next);
            self.last_edit = None;
            self.status = "redo".into();
        }
    }

    fn apply_snapshot(&mut self, s: &Snapshot) {
        self.queue(Command::ApplyState {
            preamp_db: s.preamp_db,
            enabled: s.enabled,
            bands: s.bands.clone(),
            effects: s.effects.clone(),
        });
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
                if s.meters.clip {
                    self.clip_until = Some(Instant::now() + Duration::from_millis(250));
                }
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

    /// Import a preset file as a profile (our own format), then load that
    /// profile — mirrors the TUI flow so presets are always captured, not just
    /// applied transiently.
    fn import_and_load(&mut self, path: String) {
        let Some(ipc) = self.ipc.as_mut() else {
            return;
        };
        match ipc.send_recv(Command::ImportPreset { path, name: None }) {
            Ok(Response::Imported(name)) => {
                self.queue(Command::LoadProfile { name: name.clone() });
                self.status = format!("imported + loaded '{name}'");
                self.needs_meta = true;
            }
            Ok(Response::Error(e)) => self.status = format!("import failed: {e}"),
            Ok(_) => self.status = "import failed".into(),
            Err(e) => {
                self.status = format!("error: {e}");
                self.ipc = None;
            }
        }
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

        // Keyboard: Ctrl-Z undo, Ctrl-Y / Ctrl-Shift-Z redo.
        let (undo, redo) = ui.ctx().input(|i| {
            let ctrl = i.modifiers.command || i.modifiers.ctrl;
            let undo = ctrl && i.key_pressed(egui::Key::Z) && !i.modifiers.shift;
            let redo = ctrl
                && (i.key_pressed(egui::Key::Y)
                    || (i.key_pressed(egui::Key::Z) && i.modifiers.shift));
            (undo, redo)
        });
        if undo {
            self.undo();
        } else if redo {
            self.redo();
        }

        // Service status drives the toolbar daemon controls; poll it slowly.
        if self.last_service_poll.elapsed() >= Duration::from_millis(1500) {
            self.last_service_poll = Instant::now();
            self.daemon_status = service::status();
        }

        egui::Panel::top("toolbar").show_inside(ui, |ui| self.toolbar(ui));

        if self.state.is_none() {
            egui::CentralPanel::default().show_inside(ui, |ui| self.disconnected(ui));
        } else {
            // FR graph: a resizable top panel — drag its bottom edge to size it
            // directly. Spectrum is a resizable bottom panel; effects + bands
            // fill the central area.
            egui::Panel::top("fr")
                .resizable(true)
                .default_size(220.0)
                .min_size(70.0)
                .show_inside(ui, |ui| {
                    let state = self.state.clone();
                    if let Some(s) = &state {
                        self.eq_curve(ui, s);
                    }
                });
            // Spectrum first so it spans the full window width along the bottom;
            // the side panel then reserves the right column above it.
            egui::Panel::bottom("spectrum")
                .resizable(true)
                .default_size(90.0)
                .min_size(28.0)
                .show_inside(ui, |ui| {
                    let state = self.state.clone();
                    if let Some(s) = &state {
                        self.spectrum(ui, s);
                    }
                });
            egui::Panel::right("side")
                .resizable(true)
                .default_size(280.0)
                .show_inside(ui, |ui| self.side_panel(ui));
            egui::CentralPanel::default().show_inside(ui, |ui| {
                let state = self.state.clone();
                if let Some(s) = &state {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        self.effects_section(ui, s);
                        ui.add_space(8.0);
                        ui.separator();
                        self.bands_section(ui, s);
                    });
                }
            });
        }

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
        // Wrapped so narrow windows reflow controls onto a second row instead of
        // overlapping (the bar used to clip on small widths).
        ui.horizontal_wrapped(|ui| {
            ui.heading("Resonance");
            ui.separator();

            // Prominent power toggle: a large filled green/red button, not a
            // tiny checkbox.
            let enabled = state.as_ref().map(|s| s.enabled).unwrap_or(false);
            let (txt, fill) = if enabled {
                ("●  ON", self.palette.boost)
            } else {
                ("○  OFF", self.palette.cut)
            };
            let power_btn = egui::Button::new(
                egui::RichText::new(txt)
                    .strong()
                    .size(15.0)
                    .color(egui::Color32::WHITE),
            )
            .fill(fill)
            .min_size(egui::vec2(74.0, 26.0));
            if ui
                .add_enabled(state.is_some(), power_btn)
                .on_hover_text("toggle DSP power")
                .clicked()
            {
                self.queue_edit(Command::SetPower { enabled: !enabled });
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
                    self.queue_edit(Command::SetPreamp { db });
                }
            } else {
                ui.add_enabled(false, egui::Slider::new(&mut 0.0, -20.0..=20.0));
            }

            ui.separator();
            if ui.button("Load preset…").clicked() {
                let lib = resonance_ipc::paths::user_preset_dir();
                let _ = std::fs::create_dir_all(&lib);
                let start = if lib.is_dir() {
                    lib
                } else {
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                };
                self.dialog = Dialog::LoadPreset(Browser::new(start));
            }
            if let Some(p) = state.as_ref().and_then(|s| s.current_preset.as_ref()) {
                ui.label(format!("▸ {p}"));
            }

            ui.separator();
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

            ui.separator();
            self.daemon_menu(ui);

            ui.separator();
            self.theme_menu(ui);

            ui.separator();
            if let Some(s) = &state {
                self.meters_widget(ui, s);
            }
            if !self.status.is_empty() {
                ui.separator();
                ui.label(&self.status);
            }
        });
    }

    /// Daemon lifecycle controls (systemd user service) as a compact menu so
    /// users never type a `systemctl` line.
    fn daemon_menu(&mut self, ui: &mut egui::Ui) {
        if !service::systemd_available() {
            return;
        }
        let st = self.daemon_status;
        let (dot, color) = if st.active {
            ("●", self.palette.boost)
        } else {
            ("○", self.palette.cut)
        };
        ui.menu_button(
            egui::RichText::new(format!("{dot} Daemon")).color(color),
            |ui| {
                ui.label(format!(
                    "{}  ·  autostart {}",
                    if st.active { "running" } else { "stopped" },
                    if st.enabled { "on" } else { "off" },
                ));
                ui.separator();
                let actions: [(&str, ServiceFn); 3] = [
                    ("Start", service::start),
                    ("Stop", service::stop),
                    ("Restart", service::restart),
                ];
                for (label, f) in actions {
                    if ui.button(label).clicked() {
                        self.status = match f() {
                            Ok(()) => format!("{label} ok"),
                            Err(e) => format!("{label} failed: {e}"),
                        };
                        self.daemon_status = service::status();
                    }
                }
                ui.separator();
                let mut autostart = st.enabled;
                if ui.checkbox(&mut autostart, "Autostart at login").changed() {
                    let r = if autostart {
                        service::enable()
                    } else {
                        service::disable()
                    };
                    self.status = match r {
                        Ok(()) => "autostart updated".into(),
                        Err(e) => format!("autostart failed: {e}"),
                    };
                    self.daemon_status = service::status();
                }
            },
        );
    }

    /// Theme picker combo box.
    fn theme_menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut sel = self.theme;
        egui::ComboBox::from_id_salt("theme")
            .selected_text(self.theme.label())
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

        // Natural reading order: 🔊 output │ I │ O │ DSP │ CLIP.
        if let Some(out) = s.active_output.as_deref() {
            ui.label(format!("🔊 {}", s.sink_label(out)));
            ui.separator();
        }
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

    /// Centre screen shown while no daemon is connected: a one-click start
    /// button instead of asking the user to type `resonanced`.
    fn disconnected(&mut self, ui: &mut egui::Ui) {
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.heading("No daemon connected");
                ui.add_space(8.0);
                ui.label(&self.status);
                ui.add_space(16.0);
                if service::systemd_available() {
                    let btn = egui::Button::new(
                        egui::RichText::new("▶  Start daemon")
                            .size(18.0)
                            .color(egui::Color32::WHITE),
                    )
                    .fill(self.palette.boost)
                    .min_size(egui::vec2(180.0, 40.0));
                    if ui.add(btn).clicked() {
                        match service::start() {
                            Ok(()) => self.status = "starting daemon…".into(),
                            Err(e) => self.status = format!("start failed: {e}"),
                        }
                        self.daemon_status = service::status();
                        self.try_connect();
                    }
                    ui.add_space(6.0);
                    let mut autostart = self.daemon_status.enabled;
                    if ui
                        .checkbox(&mut autostart, "Start automatically at login")
                        .changed()
                    {
                        let r = if autostart {
                            service::enable()
                        } else {
                            service::disable()
                        };
                        if let Err(e) = r {
                            self.status = format!("autostart: {e}");
                        }
                        self.daemon_status = service::status();
                    }
                } else {
                    ui.label("systemctl --user unavailable — run `resonanced` manually.");
                }
            });
        });
    }

    // ── EQ response curve (draggable nodes) ─────────────────────────────────

    fn eq_curve(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        // Fill the FR panel so dragging its bottom edge resizes the graph.
        let height = ui.available_height().max(50.0);
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::click_and_drag(),
        );
        let painter = ui.painter().with_clip_rect(rect);
        let pal = self.palette;

        painter.rect_filled(rect, 4.0, pal.graph_bg);

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
        let label_col = pal.neutral;
        let grid = egui::Stroke::new(1.0, pal.grid.gamma_multiply(0.6));
        for g in [-12.0, -6.0, 0.0, 6.0, 12.0] {
            let y = y_of(g);
            let stroke = if g == 0.0 {
                // Emphasised 0 dB reference line.
                egui::Stroke::new(1.6, pal.neutral)
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
                label_col,
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
                label_col,
            );
        }

        // Response curve — colour-coded by gain: each segment is tinted toward
        // boost (green) or cut (red), neutral near 0 dB.
        let pts = curve::curve_points(&state.bands, state.sample_rate, 240);
        for w in pts.windows(2) {
            let (lf0, g0) = w[0];
            let (lf1, g1) = w[1];
            let a = egui::pos2(x_of(lf0), y_of(g0));
            let b = egui::pos2(x_of(lf1), y_of(g1));
            let color = gain_color((g0 + g1) * 0.5, &pal);
            painter.line_segment([a, b], egui::Stroke::new(2.0, color));
        }

        // Double-right-click a node → toggle vertical-lock (gain-only) movement.
        use egui::PointerButton::{Primary, Secondary};
        if response.double_clicked_by(Secondary) {
            if let Some(p) = response.interact_pointer_pos() {
                if let Some(i) = nearest_band(state, p, &x_of, &y_of) {
                    self.vlock = if self.vlock == Some(i) { None } else { Some(i) };
                    self.selected_band = i;
                }
            }
        }

        // Drag handling: left button moves a node (freq+gain), right button
        // tunes its Q (drag up = narrower). A vertical-locked node moves on the
        // gain axis only. Pick the nearest node on press.
        let started_primary = response.drag_started_by(Primary);
        let started_secondary = response.drag_started_by(Secondary);
        if started_primary || started_secondary {
            if let Some(p) = response.interact_pointer_pos() {
                if let Some(i) = nearest_band(state, p, &x_of, &y_of) {
                    self.drag_band = Some(i);
                    self.selected_band = i;
                    // Locked nodes ignore the Q gesture entirely.
                    self.drag_q = started_secondary && self.vlock != Some(i);
                }
            }
        }
        if let Some(i) = self.drag_band {
            let locked = self.vlock == Some(i);
            if self.drag_q && response.dragged_by(Secondary) {
                let dy = response.drag_delta().y as f64;
                if dy != 0.0 {
                    if let Some(b) = state.bands.get(i) {
                        // Exponential so Q scales smoothly across its range.
                        let q = (b.q * (-dy * 0.015).exp()).clamp(0.1, 20.0);
                        self.queue_edit(Command::SetBand {
                            index: i,
                            freq: b.freq,
                            gain_db: b.gain_db,
                            q,
                        });
                    }
                }
            } else if !self.drag_q
                && (response.dragged_by(Primary) || (locked && response.dragged_by(Secondary)))
            {
                if let Some(p) = response.interact_pointer_pos() {
                    if let Some(b) = state.bands.get(i) {
                        // Locked: keep freq, move gain only.
                        let freq = if locked {
                            b.freq
                        } else {
                            10f64.powf(logf_of(p.x)).clamp(20.0, 20000.0)
                        };
                        let gain = db_of(p.y).clamp(-20.0, 20.0);
                        self.queue_edit(Command::SetBand {
                            index: i,
                            freq,
                            gain_db: gain,
                            q: b.q,
                        });
                    }
                }
            }
        }
        if response.drag_stopped_by(Primary) || response.drag_stopped_by(Secondary) {
            self.drag_band = None;
            self.drag_q = false;
        }
        // Double-left-click empty area → add a peaking band there.
        if response.double_clicked_by(Primary) {
            if let Some(p) = response.interact_pointer_pos() {
                let freq = 10f64.powf(logf_of(p.x)).clamp(20.0, 20000.0);
                let gain = db_of(p.y).clamp(-20.0, 20.0);
                self.queue_edit(Command::AddBand {
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
            let locked = self.vlock == Some(i);
            // Locked node: a vertical guide spanning the plot with end caps, so
            // it reads as "this node only moves up/down".
            if locked {
                let x = center.x;
                let stroke = egui::Stroke::new(1.0, pal.highlight);
                painter.line_segment(
                    [
                        egui::pos2(x, rect.top() + 2.0),
                        egui::pos2(x, rect.bottom() - 2.0),
                    ],
                    stroke,
                );
                for cap_y in [rect.top() + 2.0, rect.bottom() - 2.0] {
                    painter.line_segment(
                        [egui::pos2(x - 4.0, cap_y), egui::pos2(x + 4.0, cap_y)],
                        stroke,
                    );
                }
            }
            let color = if selected { pal.highlight } else { pal.accent };
            let r = if selected || locked { 6.0 } else { 4.0 };
            painter.circle_filled(center, r, color);
            painter.circle_stroke(center, r, egui::Stroke::new(1.0, pal.graph_bg));
        }
    }

    // ── Spectrum bars ───────────────────────────────────────────────────────

    fn spectrum(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        // Fill the resizable spectrum panel rather than a fixed height (a fixed
        // height taller than the panel makes the splitter bounce back).
        let height = ui.available_height().max(16.0);
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
                        self.queue_edit(Command::SetEffectEnabled {
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
                        self.queue_edit(Command::SetEffectIntensity {
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
            if ui.button("✚ Add band").clicked() {
                self.queue_edit(Command::AddBand {
                    band_type: BandType::Peaking,
                    freq: 1000.0,
                    gain_db: 0.0,
                    q: 1.4,
                });
            }
        });

        egui::Grid::new("bands_grid")
            .num_columns(8)
            .striped(true)
            .spacing([10.0, 4.0])
            .show(ui, |ui| {
                ui.label("#");
                ui.label("On");
                ui.label("Type");
                ui.label("Freq (Hz)");
                ui.label("Gain (dB)");
                ui.label("Q");
                ui.label("Level");
                ui.label("");
                ui.end_row();

                for (i, b) in state.bands.iter().enumerate() {
                    let selected = i == self.selected_band;

                    // Band number doubles as the row selector (replaces the old ● dot).
                    if ui
                        .selectable_label(selected, format!("{:>2}", i + 1))
                        .on_hover_text("select this band")
                        .clicked()
                    {
                        self.selected_band = i;
                    }

                    let mut on = b.enabled;
                    if ui.checkbox(&mut on, "").changed() {
                        self.queue_edit(Command::SetBandEnabled {
                            index: i,
                            enabled: on,
                        });
                    }

                    // Type combo.
                    let mut bt = b.band_type;
                    egui::ComboBox::from_id_salt(("bt", i))
                        .selected_text(bt.full())
                        .width(92.0)
                        .show_ui(ui, |ui| {
                            for cand in BAND_TYPES {
                                if ui.selectable_value(&mut bt, cand, cand.full()).clicked() {}
                            }
                        });
                    if bt != b.band_type {
                        self.queue_edit(Command::SetBandType {
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
                        self.queue_edit(Command::SetBand {
                            index: i,
                            freq,
                            gain_db: gain,
                            q,
                        });
                    }

                    // Centre-out gain bar (the TUI's gain graph): fills right for
                    // boosts, left for cuts, tinted by gain colour.
                    gain_bar(ui, b.gain_db, &self.palette);

                    if ui.button("✕").on_hover_text("remove").clicked() {
                        self.queue_edit(Command::RemoveBand { index: i });
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
                .selected_text(s.sink_label(&sel))
                .width(ui.available_width() - 10.0)
                .show_ui(ui, |ui| {
                    for sink in &s.available_sinks {
                        let label = s.sink_label(sink);
                        ui.selectable_value(&mut sel, sink.clone(), label);
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
                        if ui.button("✕").clicked() {
                            self.queue(Command::DeleteProfile { name: name.clone() });
                            self.needs_meta = true;
                        }
                        if ui
                            .button("Rename")
                            .on_hover_text("rename to the name in the text box above")
                            .clicked()
                            && !self.profile_name.trim().is_empty()
                        {
                            self.queue(Command::RenameProfile {
                                from: name.clone(),
                                to: self.profile_name.trim().to_string(),
                            });
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
                let label = state
                    .as_ref()
                    .map(|s| s.sink_label(out))
                    .unwrap_or_else(|| short_name(out));
                ui.label(format!("{label} → {prof}"));
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
                            if ui.button("↑ parent").clicked() {
                                go_parent = true;
                            }
                            for (i, it) in browser.entries.iter().enumerate() {
                                let label = if it.is_dir {
                                    format!("{}/", it.name)
                                } else {
                                    it.name.clone()
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
                    if ui.button("Import & load").clicked() {
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
            self.import_and_load(path);
            close = true;
        }
        if !open || close {
            self.dialog = Dialog::None;
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Bundled icon font: a ~2 KB subset of DejaVu Sans containing only the eight
/// glyphs the UI draws (●▸↑✕✚→·…), which egui's built-in fonts lack. Embedded
/// so icons render identically everywhere with negligible binary cost.
/// DejaVu license; see `assets/DejaVuSans-LICENSE.txt`.
const SYMBOL_FONT: &[u8] = include_bytes!("../assets/icons.ttf");

/// Register the bundled symbol font as a fallback so the geometric glyphs used
/// in the UI (●, ▸, ↑, ✕, ✚, →, …) render instead of tofu boxes — egui's
/// built-in fonts cover only a small symbol subset. Appended last so normal
/// text keeps the default typeface.
fn install_symbol_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "noto-symbols".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(SYMBOL_FONT)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("noto-symbols".to_owned());
    }
    ctx.set_fonts(fonts);
}

/// Index of the band whose node is nearest `p` (within a small radius).
fn nearest_band(
    state: &DaemonState,
    p: egui::Pos2,
    x_of: &dyn Fn(f64) -> f32,
    y_of: &dyn Fn(f64) -> f32,
) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, b) in state.bands.iter().enumerate() {
        let node = egui::pos2(x_of(curve::clampf_log(b.freq)), y_of(b.gain_db));
        let d = node.distance(p);
        if d < 14.0 && best.map(|(_, bd)| d < bd).unwrap_or(true) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

/// Colour for a gain value: neutral accent near 0, tinting toward boost (green)
/// or cut (red) as magnitude grows — the FR curve's colour coding.
fn gain_color(db: f64, pal: &Palette) -> egui::Color32 {
    let t = (db.abs() / curve::DB_RANGE).clamp(0.0, 1.0) as f32;
    if db.abs() < 0.3 {
        return pal.accent;
    }
    let target = if db > 0.0 { pal.boost } else { pal.cut };
    lerp_color(pal.accent, target, t)
}

fn lerp_color(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    egui::Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Paint a centre-out gain bar in a fixed-size cell: a centre tick with the bar
/// growing right for boosts and left for cuts, scaled to ±`DB_RANGE`.
fn gain_bar(ui: &mut egui::Ui, db: f64, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(96.0, 14.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let cx = rect.center().x;
    // Centre tick.
    painter.line_segment(
        [
            egui::pos2(cx, rect.top() + 1.0),
            egui::pos2(cx, rect.bottom() - 1.0),
        ],
        egui::Stroke::new(1.0, pal.grid),
    );
    let t = (db / curve::DB_RANGE).clamp(-1.0, 1.0) as f32;
    let half = rect.width() * 0.5 - 2.0;
    let w = t.abs() * half;
    if w >= 1.0 {
        let bar = if db >= 0.0 {
            egui::Rect::from_min_max(
                egui::pos2(cx, rect.top() + 2.0),
                egui::pos2(cx + w, rect.bottom() - 2.0),
            )
        } else {
            egui::Rect::from_min_max(
                egui::pos2(cx - w, rect.top() + 2.0),
                egui::pos2(cx, rect.bottom() - 2.0),
            )
        };
        painter.rect_filled(bar, 1.0, gain_color(db, pal));
    }
}

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
