use anyhow::{Result, anyhow};
use ratatui::layout::Rect;
use resonance_ipc::{
    BandState, Command, DaemonState, EffectsState, FxEffectId, Response,
    transport::{read_response, write_command},
};
use std::{
    io::{BufReader, BufWriter, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::{Duration, Instant},
};

/// Spectrum envelope time constants: bars snap up, glide down.
const SPECTRUM_ATTACK_TAU: f32 = 0.020;
const SPECTRUM_DECAY_TAU: f32 = 0.200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Effects,
    Bands,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandField {
    Freq,
    Gain,
    Q,
}

/// What part of a band row a mouse hit landed on.
#[derive(Debug, Clone, Copy)]
enum BandHit {
    Row,
    Type,
    Field(BandField),
    Toggle,
}

pub enum InputMode {
    Normal,
    Browse(crate::browser::Browser),
    SelectOutput { sinks: Vec<String>, cursor: usize },
    Settings(crate::settings::SettingsState),
    Help,
}

impl InputMode {
    pub fn is_normal(&self) -> bool {
        matches!(self, InputMode::Normal)
    }
}

pub struct App {
    pub state: Option<DaemonState>,
    pub running: bool,
    pub focus: Panel,
    pub effect_cursor: usize,
    pub band_cursor: usize,
    pub band_field: BandField,
    pub mode: InputMode,
    pub status: String,
    pub last_frame: Rect,
    pub prefs: crate::prefs::Prefs,
    /// Smoothed spectrum bar heights (drawn instead of raw bins to stop flicker).
    pub spectrum_display: Vec<f32>,
    last_anim: Instant,
    ipc: Option<IpcClient>,
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    /// While `Some` and in the future, the output clip indicator flashes.
    pub clip_until: Option<Instant>,
    /// Cached systemd user-service status (refreshed when the Daemon tab is used).
    pub daemon_status: resonance_ipc::service::Status,
}

/// A restorable snapshot of the editable chain state (for undo/redo).
#[derive(Clone)]
struct Snapshot {
    preamp_db: f64,
    enabled: bool,
    bands: Vec<BandState>,
    effects: EffectsState,
}

impl App {
    pub fn new() -> Self {
        let prefs = crate::prefs::Prefs::load();
        Self {
            state: None,
            running: true,
            focus: Panel::Effects,
            effect_cursor: 0,
            band_cursor: 0,
            band_field: BandField::Gain,
            mode: InputMode::Normal,
            status: String::new(),
            last_frame: Rect::default(),
            prefs,
            spectrum_display: Vec::new(),
            last_anim: Instant::now(),
            ipc: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            clip_until: None,
            daemon_status: resonance_ipc::service::Status::default(),
        }
    }

    // ── Daemon (systemd user service) control ────────────────────────────────

    /// Refresh the cached service status snapshot.
    pub fn refresh_daemon_status(&mut self) {
        self.daemon_status = resonance_ipc::service::status();
    }

    /// Run a service action, update status text + cached snapshot, and try to
    /// (re)connect afterward so the UI reflects the change immediately.
    pub fn daemon_action(&mut self, label: &str, r: std::io::Result<()>) {
        self.status = match r {
            Ok(()) => format!("daemon: {label} ok"),
            Err(e) => format!("daemon: {label} failed: {e}"),
        };
        self.refresh_daemon_status();
        if self.ipc.is_none() {
            self.connect();
            self.refresh_state();
        }
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

    /// Record the pre-edit state. Call before any mutating action.
    fn push_undo(&mut self) {
        if let Some(s) = self.snapshot() {
            self.undo_stack.push(s);
            if self.undo_stack.len() > 100 {
                self.undo_stack.remove(0);
            }
            self.redo_stack.clear();
        }
    }

    pub fn undo(&mut self) {
        match self.undo_stack.pop() {
            Some(prev) => {
                if let Some(cur) = self.snapshot() {
                    self.redo_stack.push(cur);
                }
                self.apply_snapshot(&prev);
                self.status = "undo".into();
            }
            None => self.status = "nothing to undo".into(),
        }
    }

    pub fn redo(&mut self) {
        match self.redo_stack.pop() {
            Some(next) => {
                if let Some(cur) = self.snapshot() {
                    self.undo_stack.push(cur);
                }
                self.apply_snapshot(&next);
                self.status = "redo".into();
            }
            None => self.status = "nothing to redo".into(),
        }
    }

    fn apply_snapshot(&mut self, s: &Snapshot) {
        self.send(Command::ApplyState {
            preamp_db: s.preamp_db,
            enabled: s.enabled,
            bands: s.bands.clone(),
            effects: s.effects.clone(),
        });
        self.refresh_state();
    }

    /// Advance the spectrum envelope toward the latest daemon bins. Fast attack,
    /// slow decay — driven each frame so it's smooth regardless of the data rate.
    pub fn animate_spectrum(&mut self) {
        let dt = self.last_anim.elapsed().as_secs_f32().min(0.1);
        self.last_anim = Instant::now();
        let Some(bins) = self.state.as_ref().map(|s| s.spectrum.as_slice()) else {
            return;
        };
        if self.spectrum_display.len() != bins.len() {
            self.spectrum_display = vec![0.0; bins.len()];
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
    }

    pub fn connect(&mut self) {
        match IpcClient::connect() {
            Ok(c) => {
                self.ipc = Some(c);
                self.status = "connected".into();
            }
            Err(e) => {
                self.status = format!("not connected: {e}");
            }
        }
    }

    pub fn refresh_state(&mut self) {
        let Some(ipc) = self.ipc.as_mut() else {
            self.connect();
            return;
        };
        match ipc.get_state() {
            Ok(s) => {
                // Latch the clip flash: the daemon reports "clipped since last poll".
                if s.meters.clip {
                    self.clip_until = Some(Instant::now() + Duration::from_millis(250));
                }
                self.state = Some(s);
                self.status.clear();
            }
            Err(e) => {
                self.status = format!("error: {e}");
                self.state = None;
                self.ipc = None;
            }
        }
    }

    fn send(&mut self, cmd: Command) {
        let Some(ipc) = self.ipc.as_mut() else {
            self.connect();
            return;
        };
        if let Err(e) = ipc.send(cmd) {
            self.status = format!("error: {e}");
            self.ipc = None;
        }
    }

    fn query(&mut self, cmd: Command) -> Option<Response> {
        let Some(ipc) = self.ipc.as_mut() else {
            self.connect();
            return None;
        };
        match ipc.send_recv(cmd) {
            Ok(r) => Some(r),
            Err(e) => {
                self.status = format!("error: {e}");
                self.ipc = None;
                None
            }
        }
    }

    // ── Normal-mode actions ────────────────────────────────────────────────

    pub fn toggle_power(&mut self) {
        let enabled = self.state.as_ref().map(|s| !s.enabled).unwrap_or(true);
        self.send(Command::SetPower { enabled });
        self.refresh_state();
    }

    pub fn begin_load_preset(&mut self) {
        // Default to the XDG preset library (create it on first use) so imported
        // and AutoEq-downloaded presets are right there.
        let lib = resonance_ipc::paths::user_preset_dir();
        let _ = std::fs::create_dir_all(&lib);
        let start = if lib.is_dir() {
            lib
        } else {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        };
        self.mode = InputMode::Browse(crate::browser::Browser::new(start));
    }

    pub fn cancel_input(&mut self) {
        self.mode = InputMode::Normal;
    }

    pub fn show_help(&mut self) {
        self.mode = InputMode::Help;
    }

    pub fn begin_select_output(&mut self) {
        let sinks = self
            .state
            .as_ref()
            .map(|s| s.available_sinks.clone())
            .unwrap_or_default();
        let active = self
            .state
            .as_ref()
            .and_then(|s| s.preferred_output.as_deref().or(s.active_output.as_deref()));
        let cursor = active
            .and_then(|a| sinks.iter().position(|s| s == a))
            .unwrap_or(0);
        self.mode = InputMode::SelectOutput { sinks, cursor };
    }

    pub fn output_move(&mut self, delta: i32) {
        if let InputMode::SelectOutput { sinks, cursor } = &mut self.mode {
            if sinks.is_empty() {
                return;
            }
            let max = sinks.len() as i32 - 1;
            *cursor = ((*cursor as i32 + delta).clamp(0, max)) as usize;
        }
    }

    pub fn output_enter(&mut self) {
        let node_name = match &self.mode {
            InputMode::SelectOutput { sinks, cursor } => sinks.get(*cursor).cloned(),
            _ => return,
        };
        self.mode = InputMode::Normal;
        if let Some(name) = node_name {
            self.send(Command::SetOutputTarget { node_name: name });
            self.refresh_state();
        }
    }

    /// Move the file-picker cursor.
    pub fn browse_move(&mut self, delta: i32) {
        if let InputMode::Browse(b) = &mut self.mode {
            b.move_cursor(delta);
        }
    }

    /// Go to the parent directory in the picker.
    pub fn browse_parent(&mut self) {
        if let InputMode::Browse(b) = &mut self.mode {
            b.parent();
        }
    }

    /// Activate the selected entry: enter a directory, or import+load a preset.
    ///
    /// Picking a file imports it as a profile (our own format) and then loads
    /// that profile — so every preset that enters the app is captured and can be
    /// renamed/managed from Settings, rather than loaded transiently.
    pub fn browse_enter(&mut self) {
        let action = match &mut self.mode {
            InputMode::Browse(b) => b.enter(),
            _ => return,
        };
        if let Some(path) = action {
            self.mode = InputMode::Normal;
            self.import_and_load(path);
            self.refresh_state();
        }
    }

    fn import_and_load(&mut self, path: String) {
        match self.query(Command::ImportPreset { path, name: None }) {
            Some(Response::Imported(name)) => {
                self.send(Command::LoadProfile { name: name.clone() });
                self.status = format!("imported + loaded '{name}'");
            }
            Some(Response::Error(e)) => self.status = format!("import failed: {e}"),
            _ => self.status = "import failed".into(),
        }
    }

    // ── Navigation ────────────────────────────────────────────────────────

    /// Tab cycles left-to-right across columns, then wraps:
    /// Effects → Bands(Freq) → Bands(Gain) → Bands(Q) → Effects
    pub fn next_panel(&mut self) {
        match self.focus {
            Panel::Effects => {
                self.focus = Panel::Bands;
                self.band_field = BandField::Freq;
            }
            Panel::Bands => match self.band_field {
                BandField::Freq => self.band_field = BandField::Gain,
                BandField::Gain => self.band_field = BandField::Q,
                BandField::Q => self.focus = Panel::Effects,
            },
        }
    }

    pub fn cursor_up(&mut self) {
        match self.focus {
            Panel::Effects => {
                if self.effect_cursor > 0 {
                    self.effect_cursor -= 1;
                }
            }
            Panel::Bands => {
                if self.band_cursor > 0 {
                    self.band_cursor -= 1;
                }
            }
        }
    }

    pub fn cursor_down(&mut self) {
        match self.focus {
            Panel::Effects => {
                let max = 4; // 5 effects
                if self.effect_cursor < max {
                    self.effect_cursor += 1;
                }
            }
            Panel::Bands => {
                let max = self
                    .state
                    .as_ref()
                    .map(|s| s.bands.len().saturating_sub(1))
                    .unwrap_or(0);
                if self.band_cursor < max {
                    self.band_cursor += 1;
                }
            }
        }
    }

    pub fn adjust(&mut self, delta: f64) {
        match self.focus {
            Panel::Effects => {
                let Some(state) = &self.state else { return };
                let effect = fx_effect_at(self.effect_cursor);
                let current = fx_intensity(state, self.effect_cursor);
                let new_val = (current + delta).clamp(fx_min(self.effect_cursor), 1.0);
                let new_pct = (new_val * 100.0).round();
                let new_val = new_pct / 100.0;
                if (new_val - current).abs() > 0.001 {
                    self.push_undo();
                    self.send(Command::SetEffectIntensity {
                        effect,
                        value: new_val,
                    });
                    self.refresh_state();
                }
            }
            Panel::Bands => {
                let Some(state) = &self.state else { return };
                let idx = self.band_cursor;
                let Some(band) = state.bands.get(idx) else {
                    return;
                };
                let (new_freq, new_gain, new_q) = match self.band_field {
                    BandField::Freq => {
                        let semitones = delta * 20.0;
                        let f = (band.freq * 2.0_f64.powf(semitones / 12.0)).clamp(20.0, 20000.0);
                        let f = (f * 10.0).round() / 10.0;
                        (f, band.gain_db, band.q)
                    }
                    BandField::Gain => {
                        let g = (band.gain_db + delta * 10.0).clamp(-20.0, 20.0);
                        let g = (g * 10.0).round() / 10.0;
                        (band.freq, g, band.q)
                    }
                    BandField::Q => {
                        let q = (band.q + delta * 2.0).clamp(0.1, 20.0);
                        let q = (q * 100.0).round() / 100.0;
                        (band.freq, band.gain_db, q)
                    }
                };
                self.push_undo();
                self.send(Command::SetBand {
                    index: idx,
                    freq: new_freq,
                    gain_db: new_gain,
                    q: new_q,
                });
                self.refresh_state();
            }
        }
    }

    // ── Mouse hit-testing ─────────────────────────────────────────────────

    /// Resolve a click on the effects column to an effect index.
    fn hit_effect(&self, col: u16, row: u16) -> Option<usize> {
        let p = crate::layout::panes(self.last_frame);
        if !crate::layout::hit(p.effects, col, row) {
            return None;
        }
        let inner = crate::layout::block_inner(p.effects);
        let rows = crate::layout::effect_rows(inner, EFFECT_NAMES.len());
        rows.iter().position(|r| crate::layout::hit(*r, col, row))
    }

    /// Resolve a click on the bands panel to (band index, optional field).
    fn hit_band(&self, col: u16, row: u16) -> Option<(usize, BandHit)> {
        let p = crate::layout::panes(self.last_frame);
        if !crate::layout::hit(p.bands, col, row) {
            return None;
        }
        let inner = crate::layout::block_inner(p.bands);
        if inner.height < 2 || row < inner.y + 1 {
            return None; // border or header
        }
        let n = self.state.as_ref().map(|s| s.bands.len()).unwrap_or(0);
        let visible = (inner.height - 1) as usize;
        let offset = crate::layout::band_scroll_offset(self.band_cursor, n, visible);
        let line = (row - (inner.y + 1)) as usize;
        let idx = offset + line;
        if idx >= n {
            return None;
        }
        let row_rect = ratatui::layout::Rect::new(inner.x, row, inner.width, 1);
        let cols = crate::layout::band_columns(row_rect);
        let hit = match cols.iter().position(|c| crate::layout::hit(*c, col, row)) {
            Some(1) => BandHit::Type,
            Some(2) => BandHit::Field(BandField::Freq),
            Some(3) => BandHit::Field(BandField::Gain),
            Some(4) => BandHit::Field(BandField::Q),
            Some(6) => BandHit::Toggle, // 5 is the spacer column
            _ => BandHit::Row,
        };
        Some((idx, hit))
    }

    /// Handle left-click: select effect/band, focus the field, or toggle/cycle.
    pub fn mouse_click(&mut self, col: u16, row: u16) {
        if let Some(idx) = self.hit_effect(col, row) {
            self.focus = Panel::Effects;
            self.effect_cursor = idx;
            return;
        }
        if let Some((idx, hit)) = self.hit_band(col, row) {
            self.focus = Panel::Bands;
            self.band_cursor = idx;
            match hit {
                BandHit::Field(f) => self.band_field = f,
                BandHit::Type => self.cycle_band_type(),
                BandHit::Toggle => self.toggle_selected(),
                BandHit::Row => {}
            }
        }
    }

    /// Handle scroll: adjust the effect or band cell under the cursor.
    pub fn mouse_scroll(&mut self, col: u16, row: u16, delta: f64) {
        if let Some(idx) = self.hit_effect(col, row) {
            self.focus = Panel::Effects;
            self.effect_cursor = idx;
            self.adjust(delta);
            return;
        }
        if let Some((idx, hit)) = self.hit_band(col, row) {
            self.focus = Panel::Bands;
            self.band_cursor = idx;
            if let BandHit::Field(f) = hit {
                self.band_field = f;
            }
            self.adjust(delta);
        }
    }

    pub fn add_band(&mut self) {
        self.push_undo();
        self.send(Command::AddBand {
            band_type: self.prefs.default_band_type,
            freq: 1000.0,
            gain_db: 0.0,
            q: self.prefs.default_band_q,
        });
        self.refresh_state();
        if let Some(s) = &self.state {
            self.band_cursor = s.bands.len().saturating_sub(1);
        }
    }

    /// Cycle the filter type of the selected band (`t` key).
    pub fn cycle_band_type(&mut self) {
        if self.focus != Panel::Bands {
            return;
        }
        let Some(state) = &self.state else { return };
        let idx = self.band_cursor;
        let Some(band) = state.bands.get(idx) else {
            return;
        };
        let next = band.band_type.next();
        self.push_undo();
        self.send(Command::SetBandType {
            index: idx,
            band_type: next,
        });
        self.refresh_state();
    }

    pub fn remove_band(&mut self) {
        let Some(state) = &self.state else { return };
        if state.bands.is_empty() {
            return;
        }
        let idx = self.band_cursor.min(state.bands.len() - 1);
        self.push_undo();
        self.send(Command::RemoveBand { index: idx });
        self.refresh_state();
        if let Some(s) = &self.state {
            self.band_cursor = self.band_cursor.min(s.bands.len().saturating_sub(1));
        }
    }

    pub fn toggle_selected(&mut self) {
        match self.focus {
            Panel::Effects => {
                let Some(state) = &self.state else { return };
                let effect = fx_effect_at(self.effect_cursor);
                let enabled = !fx_enabled(state, self.effect_cursor);
                self.push_undo();
                self.send(Command::SetEffectEnabled { effect, enabled });
                self.refresh_state();
            }
            Panel::Bands => {
                let Some(state) = &self.state else { return };
                let idx = self.band_cursor;
                let Some(band) = state.bands.get(idx) else {
                    return;
                };
                let enabled = !band.enabled;
                self.push_undo();
                self.send(Command::SetBandEnabled {
                    index: idx,
                    enabled,
                });
                self.refresh_state();
            }
        }
    }

    pub fn preamp_adjust(&mut self, delta: f64) {
        let current = self.state.as_ref().map(|s| s.preamp_db).unwrap_or(0.0);
        let new_db = ((current + delta) * 10.0).round() / 10.0;
        let new_db = new_db.clamp(-20.0, 20.0);
        if (new_db - current).abs() > 1e-6 {
            self.push_undo();
            self.send(Command::SetPreamp { db: new_db });
            self.refresh_state();
        }
    }

    // ── Settings popup ─────────────────────────────────────────────────────

    pub fn begin_settings(&mut self) {
        let profiles = match self.query(Command::ListProfiles) {
            Some(Response::PresetList(v)) => v,
            _ => vec![],
        };
        let mappings = match self.query(Command::ListMappings) {
            Some(Response::Mappings(v)) => v,
            _ => vec![],
        };
        let sinks = self
            .state
            .as_ref()
            .map(|s| s.available_sinks.clone())
            .unwrap_or_default();
        self.refresh_daemon_status();
        self.mode = InputMode::Settings(crate::settings::SettingsState::new(
            profiles, mappings, sinks,
        ));
    }

    pub fn settings_close(&mut self) {
        self.mode = InputMode::Normal;
    }

    pub fn settings_set_tab(&mut self, tab: usize) {
        if let InputMode::Settings(s) = &mut self.mode {
            s.tab = tab;
            s.cursor = 0;
            s.text_input = None;
            s.confirm = None;
            s.sub_picker = None;
        }
    }

    pub fn settings_tab_shift(&mut self, delta: i32) {
        if let InputMode::Settings(s) = &mut self.mode {
            let n = crate::settings::TABS.len() as i32;
            s.tab = ((s.tab as i32 + delta).rem_euclid(n)) as usize;
            s.cursor = 0;
            s.text_input = None;
            s.confirm = None;
            s.sub_picker = None;
        }
    }

    pub fn settings_move(&mut self, delta: i32) {
        if let InputMode::Settings(s) = &mut self.mode {
            if let Some(sp) = &mut s.sub_picker {
                let max = sp.profiles.len().saturating_sub(1) as i32;
                sp.cursor = (sp.cursor as i32 + delta).clamp(0, max) as usize;
                return;
            }
            let max = s.max_cursor() as i32;
            s.cursor = (s.cursor as i32 + delta).clamp(0, max) as usize;
        }
    }

    pub fn settings_has_text_input(&self) -> bool {
        matches!(&self.mode, InputMode::Settings(s) if s.text_input.is_some())
    }

    pub fn settings_has_confirm(&self) -> bool {
        matches!(&self.mode, InputMode::Settings(s) if s.confirm.is_some())
    }

    pub fn settings_has_sub_picker(&self) -> bool {
        matches!(&self.mode, InputMode::Settings(s) if s.sub_picker.is_some())
    }

    pub fn settings_text_char(&mut self, c: char) {
        if let InputMode::Settings(s) = &mut self.mode {
            if let Some(ti) = &mut s.text_input {
                ti.insert(c);
            }
        }
    }

    pub fn settings_backspace(&mut self) {
        if let InputMode::Settings(s) = &mut self.mode {
            if let Some(ti) = &mut s.text_input {
                ti.backspace();
            }
        }
    }

    pub fn settings_cursor_left(&mut self) {
        if let InputMode::Settings(s) = &mut self.mode {
            if let Some(ti) = &mut s.text_input {
                ti.cursor_left();
            }
        }
    }

    pub fn settings_cursor_right(&mut self) {
        if let InputMode::Settings(s) = &mut self.mode {
            if let Some(ti) = &mut s.text_input {
                ti.cursor_right();
            }
        }
    }

    pub fn settings_cancel_text(&mut self) {
        if let InputMode::Settings(s) = &mut self.mode {
            s.text_input = None;
        }
    }

    pub fn settings_confirm_text(&mut self) {
        use crate::settings::TextPurpose;
        let (purpose, buf) = match &self.mode {
            InputMode::Settings(s) => match &s.text_input {
                Some(ti) => (ti.purpose.clone(), ti.buf.clone()),
                None => return,
            },
            _ => return,
        };

        match purpose {
            TextPurpose::SaveProfile => {
                let name = buf.trim().to_string();
                if !name.is_empty() {
                    self.send(Command::SaveProfile { name });
                    let profiles = match self.query(Command::ListProfiles) {
                        Some(Response::PresetList(v)) => v,
                        _ => vec![],
                    };
                    if let InputMode::Settings(s) = &mut self.mode {
                        s.profiles = profiles;
                        s.text_input = None;
                    }
                }
            }
            TextPurpose::ExportProfile => {
                let name = buf.trim().trim_end_matches(".toml").to_string();
                if !name.is_empty() {
                    let dir = resonance_ipc::paths::user_preset_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    let path = dir.join(format!("{name}.toml"));
                    let path_str = path.to_string_lossy().to_string();
                    match self.query(Command::ExportProfile {
                        path: path_str.clone(),
                    }) {
                        Some(Response::Ok) => self.status = format!("exported → {path_str}"),
                        Some(Response::Error(e)) => self.status = format!("export failed: {e}"),
                        _ => self.status = "export failed".into(),
                    }
                }
                if let InputMode::Settings(s) = &mut self.mode {
                    s.text_input = None;
                }
            }
            TextPurpose::RenameProfile(from) => {
                let to = buf.trim().to_string();
                if !to.is_empty() && to != from {
                    match self.query(Command::RenameProfile {
                        from: from.clone(),
                        to: to.clone(),
                    }) {
                        Some(Response::Ok) => self.status = format!("renamed to '{to}'"),
                        Some(Response::Error(e)) => self.status = format!("rename failed: {e}"),
                        _ => {}
                    }
                }
                let profiles = match self.query(Command::ListProfiles) {
                    Some(Response::PresetList(v)) => v,
                    _ => vec![],
                };
                if let InputMode::Settings(s) = &mut self.mode {
                    s.profiles = profiles;
                    s.cursor = s.cursor.min(s.profiles.len().saturating_sub(1));
                    s.text_input = None;
                }
            }
            TextPurpose::PrefFps => {
                if let Ok(n) = buf.trim().parse::<u64>() {
                    self.prefs.fps = n.clamp(5, 240);
                    self.prefs.save();
                }
                if let InputMode::Settings(s) = &mut self.mode {
                    s.text_input = None;
                }
            }
            TextPurpose::PrefRefreshMs => {
                if let Ok(n) = buf.trim().parse::<u64>() {
                    self.prefs.refresh_ms = n.clamp(100, 5000);
                    self.prefs.save();
                }
                if let InputMode::Settings(s) = &mut self.mode {
                    s.text_input = None;
                }
            }
            TextPurpose::PrefBandQ => {
                if let Ok(q) = buf.trim().parse::<f64>() {
                    self.prefs.default_band_q = q.clamp(0.1, 20.0);
                    self.prefs.save();
                }
                if let InputMode::Settings(s) = &mut self.mode {
                    s.text_input = None;
                }
            }
        }
    }

    pub fn settings_confirm_yes(&mut self) {
        let action = match &self.mode {
            InputMode::Settings(s) => s.confirm.clone(),
            _ => return,
        };
        match action {
            Some(crate::settings::ConfirmAction::DeleteProfile(name)) => {
                self.do_delete_profile(name);
            }
            Some(crate::settings::ConfirmAction::UnmapOutput) => {
                self.do_unmap_output();
            }
            None => {}
        }
    }

    pub fn settings_confirm_no(&mut self) {
        if let InputMode::Settings(s) = &mut self.mode {
            s.confirm = None;
        }
    }

    pub fn settings_close_sub_picker(&mut self) {
        if let InputMode::Settings(s) = &mut self.mode {
            s.sub_picker = None;
        }
    }

    pub fn settings_sub_picker_confirm(&mut self) {
        let (profile, for_sink) = match &self.mode {
            InputMode::Settings(s) => match &s.sub_picker {
                Some(sp) => (sp.profiles.get(sp.cursor).cloned(), sp.for_sink.clone()),
                None => return,
            },
            _ => return,
        };
        let Some(profile) = profile else { return };
        if let Some(sink) = for_sink {
            self.send(Command::SetOutputTarget { node_name: sink });
            self.refresh_state();
        }
        self.send(Command::MapOutput { profile });
        let mappings = match self.query(Command::ListMappings) {
            Some(Response::Mappings(v)) => v,
            _ => vec![],
        };
        self.refresh_state();
        if let InputMode::Settings(s) = &mut self.mode {
            s.sub_picker = None;
            s.mappings = mappings;
        }
    }

    pub fn settings_enter(&mut self) {
        let tab = match &self.mode {
            InputMode::Settings(s) => s.tab,
            _ => return,
        };
        match tab {
            0 => self.settings_load_profile(),
            1 => {}
            2 => self.settings_route_output(),
            3 => self.settings_pref_activate(),
            4 => self.settings_daemon_activate(),
            _ => {}
        }
    }

    /// Daemon tab actions: Start / Stop / Restart / toggle Autostart.
    fn settings_daemon_activate(&mut self) {
        use resonance_ipc::service;
        let cursor = match &self.mode {
            InputMode::Settings(s) => s.cursor,
            _ => return,
        };
        match cursor {
            0 => self.daemon_action("start", service::start()),
            1 => self.daemon_action("stop", service::stop()),
            2 => self.daemon_action("restart", service::restart()),
            3 => {
                let r = if self.daemon_status.enabled {
                    service::disable()
                } else {
                    service::enable()
                };
                self.daemon_action("autostart", r);
            }
            _ => {}
        }
    }

    fn settings_load_profile(&mut self) {
        let name = match &self.mode {
            InputMode::Settings(s) => s.profiles.get(s.cursor).cloned(),
            _ => return,
        };
        if let Some(name) = name {
            self.send(Command::LoadProfile { name });
            self.refresh_state();
        }
    }

    fn settings_route_output(&mut self) {
        let sink = match &self.mode {
            InputMode::Settings(s) => s.sinks.get(s.cursor).cloned(),
            _ => return,
        };
        if let Some(node_name) = sink {
            self.send(Command::SetOutputTarget { node_name });
            self.refresh_state();
        }
    }

    fn settings_pref_activate(&mut self) {
        use crate::settings::{TextInput, TextPurpose};
        let cursor = match &self.mode {
            InputMode::Settings(s) => s.cursor,
            _ => return,
        };
        match cursor {
            0 => {
                let v = self.prefs.fps.to_string();
                if let InputMode::Settings(s) = &mut self.mode {
                    s.text_input = Some(TextInput::new(v, TextPurpose::PrefFps, "FPS (5–144)"));
                }
            }
            1 => {
                let v = self.prefs.refresh_ms.to_string();
                if let InputMode::Settings(s) = &mut self.mode {
                    s.text_input = Some(TextInput::new(
                        v,
                        TextPurpose::PrefRefreshMs,
                        "Refresh ms (100–5000)",
                    ));
                }
            }
            2 => {
                self.prefs.confirm_on_delete = !self.prefs.confirm_on_delete;
                self.prefs.save();
            }
            3 => {
                let v = format!("{:.1}", self.prefs.default_band_q);
                if let InputMode::Settings(s) = &mut self.mode {
                    s.text_input = Some(TextInput::new(
                        v,
                        TextPurpose::PrefBandQ,
                        "Default Q (0.1–20.0)",
                    ));
                }
            }
            4 => {
                self.prefs.default_band_type = self.prefs.default_band_type.next();
                self.prefs.save();
            }
            _ => {}
        }
    }

    pub fn settings_key_n(&mut self) {
        use crate::settings::{TextInput, TextPurpose};
        if let InputMode::Settings(s) = &mut self.mode {
            if s.tab == 0 {
                s.text_input = Some(TextInput::new("", TextPurpose::SaveProfile, "Profile name"));
            }
        }
    }

    /// Export the current chain to a `.toml` file in the preset library.
    pub fn settings_key_e(&mut self) {
        use crate::settings::{TextInput, TextPurpose};
        if let InputMode::Settings(s) = &mut self.mode {
            if s.tab == 0 {
                s.text_input = Some(TextInput::new(
                    "",
                    TextPurpose::ExportProfile,
                    "Export filename (.toml)",
                ));
            }
        }
    }

    pub fn settings_key_r(&mut self) {
        use crate::settings::{TextInput, TextPurpose};
        let name = match &self.mode {
            InputMode::Settings(s) if s.tab == 0 => s.profiles.get(s.cursor).cloned(),
            _ => None,
        };
        if let Some(name) = name {
            if let InputMode::Settings(s) = &mut self.mode {
                s.text_input = Some(TextInput::new(
                    name.clone(),
                    TextPurpose::RenameProfile(name),
                    "Rename profile",
                ));
            }
        }
    }

    pub fn settings_key_d(&mut self) {
        let tab = match &self.mode {
            InputMode::Settings(s) => s.tab,
            _ => return,
        };
        match tab {
            0 => {
                let name = match &self.mode {
                    InputMode::Settings(s) => s.profiles.get(s.cursor).cloned(),
                    _ => return,
                };
                if let Some(name) = name {
                    if self.prefs.confirm_on_delete {
                        if let InputMode::Settings(s) = &mut self.mode {
                            s.confirm = Some(crate::settings::ConfirmAction::DeleteProfile(name));
                        }
                    } else {
                        self.do_delete_profile(name);
                    }
                }
            }
            1 => {
                let can_unmap = match (&self.mode, &self.state) {
                    (InputMode::Settings(s), Some(state)) => s
                        .mappings
                        .get(s.cursor)
                        .map(|(out, _)| state.active_output.as_deref() == Some(out.as_str()))
                        .unwrap_or(false),
                    _ => false,
                };
                if can_unmap {
                    if self.prefs.confirm_on_delete {
                        if let InputMode::Settings(s) = &mut self.mode {
                            s.confirm = Some(crate::settings::ConfirmAction::UnmapOutput);
                        }
                    } else {
                        self.do_unmap_output();
                    }
                } else {
                    self.status = "can only unmap the active output".into();
                }
            }
            _ => {}
        }
    }

    pub fn settings_key_m(&mut self) {
        let tab = match &self.mode {
            InputMode::Settings(s) => s.tab,
            _ => return,
        };
        if tab != 1 && tab != 2 {
            return;
        }
        let profiles = match self.query(Command::ListProfiles) {
            Some(Response::PresetList(v)) => v,
            _ => return,
        };
        if profiles.is_empty() {
            self.status = "no profiles saved".into();
            return;
        }
        let for_sink = if tab == 2 {
            match &self.mode {
                InputMode::Settings(s) => s.sinks.get(s.cursor).cloned(),
                _ => None,
            }
        } else {
            None
        };
        if let InputMode::Settings(s) = &mut self.mode {
            s.sub_picker = Some(crate::settings::SubPicker {
                profiles,
                cursor: 0,
                for_sink,
            });
        }
    }

    fn do_delete_profile(&mut self, name: String) {
        self.send(Command::DeleteProfile { name });
        let profiles = match self.query(Command::ListProfiles) {
            Some(Response::PresetList(v)) => v,
            _ => vec![],
        };
        if let InputMode::Settings(s) = &mut self.mode {
            s.profiles = profiles;
            s.cursor = s.cursor.min(s.profiles.len().saturating_sub(1));
            s.confirm = None;
        }
    }

    fn do_unmap_output(&mut self) {
        self.send(Command::UnmapOutput);
        let mappings = match self.query(Command::ListMappings) {
            Some(Response::Mappings(v)) => v,
            _ => vec![],
        };
        self.refresh_state();
        if let InputMode::Settings(s) = &mut self.mode {
            s.mappings = mappings;
            s.cursor = s.cursor.min(s.mappings.len().saturating_sub(1));
            s.confirm = None;
        }
    }
}

// ── FxSound effect helpers ─────────────────────────────────────────────────

const EFFECT_ORDER: [FxEffectId; 5] = [
    FxEffectId::Fidelity,
    FxEffectId::Ambience,
    FxEffectId::Surround,
    FxEffectId::DynamicBoost,
    FxEffectId::Bass,
];

pub const EFFECT_NAMES: [&str; 5] = ["Fidelity", "Ambience", "Surround", "Dyn Boost", "Bass"];

pub fn fx_effect_at(idx: usize) -> FxEffectId {
    EFFECT_ORDER[idx.min(4)]
}

/// Minimum intensity for an effect: Surround and Bass are bipolar (−1), others 0.
pub fn fx_min(idx: usize) -> f64 {
    match idx {
        2 | 4 => -1.0, // Surround, Bass
        _ => 0.0,
    }
}

pub fn fx_intensity(state: &DaemonState, idx: usize) -> f64 {
    let e = &state.effects;
    match idx {
        0 => e.fidelity_intensity,
        1 => e.ambience_intensity,
        2 => e.surround_intensity,
        3 => e.dynamic_boost_intensity,
        _ => e.bass_intensity,
    }
}

pub fn fx_enabled(state: &DaemonState, idx: usize) -> bool {
    let e = &state.effects;
    match idx {
        0 => e.fidelity_enabled,
        1 => e.ambience_enabled,
        2 => e.surround_enabled,
        3 => e.dynamic_boost_enabled,
        _ => e.bass_enabled,
    }
}

// ── Sync IPC client ────────────────────────────────────────────────────────

struct IpcClient {
    reader: BufReader<UnixStream>,
    writer: BufWriter<UnixStream>,
}

impl IpcClient {
    fn connect() -> Result<Self> {
        let path = socket_path();
        let stream =
            UnixStream::connect(&path).map_err(|e| anyhow!("connect {}: {e}", path.display()))?;
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        let writer = BufWriter::new(stream.try_clone()?);
        let reader = BufReader::new(stream);
        Ok(Self { reader, writer })
    }

    fn get_state(&mut self) -> Result<DaemonState> {
        write_command(&mut self.writer, &Command::GetState)?;
        self.writer.flush()?;
        match read_response(&mut self.reader)? {
            Response::State(s) => Ok(s),
            Response::Error(e) => Err(anyhow!("{e}")),
            _ => Err(anyhow!("unexpected response")),
        }
    }

    fn send(&mut self, cmd: Command) -> Result<()> {
        write_command(&mut self.writer, &cmd)?;
        self.writer.flush()?;
        match read_response(&mut self.reader)? {
            Response::Ok
            | Response::State(_)
            | Response::PresetList(_)
            | Response::Imported(_)
            | Response::Mappings(_) => Ok(()),
            Response::Error(e) => Err(anyhow!("{e}")),
            Response::StateChanged(_) => Ok(()),
        }
    }

    fn send_recv(&mut self, cmd: Command) -> Result<Response> {
        write_command(&mut self.writer, &cmd)?;
        self.writer.flush()?;
        Ok(read_response(&mut self.reader)?)
    }
}

fn socket_path() -> PathBuf {
    resonance_ipc::paths::default_socket_path()
}
