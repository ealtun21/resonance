use anyhow::{Result, anyhow};
use ratatui::layout::Rect;
use resonance_ipc::{
    BandType, Command, DaemonState, FxEffectId, Response,
    transport::{read_response, write_command},
};
use std::{
    env,
    io::{BufReader, BufWriter, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

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
    ipc: Option<IpcClient>,
}

impl App {
    pub fn new() -> Self {
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
            ipc: None,
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

    // ── Normal-mode actions ────────────────────────────────────────────────

    pub fn toggle_power(&mut self) {
        let enabled = self.state.as_ref().map(|s| !s.enabled).unwrap_or(true);
        self.send(Command::SetPower { enabled });
        self.refresh_state();
    }

    pub fn begin_load_preset(&mut self) {
        let start = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        self.mode = InputMode::Browse(crate::browser::Browser::new(start));
    }

    pub fn cancel_input(&mut self) {
        self.mode = InputMode::Normal;
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

    /// Activate the selected entry: enter a directory, or load a preset file.
    pub fn browse_enter(&mut self) {
        let action = match &mut self.mode {
            InputMode::Browse(b) => b.enter(),
            _ => return,
        };
        if let Some(path) = action {
            self.mode = InputMode::Normal;
            self.send(Command::LoadPreset { path });
            self.refresh_state();
        }
    }

    // ── Navigation ────────────────────────────────────────────────────────

    /// Tab cycles: Effects → Bands(Gain) → Bands(Freq) → Bands(Q) → Effects
    pub fn next_panel(&mut self) {
        match self.focus {
            Panel::Effects => {
                self.focus = Panel::Bands;
                self.band_field = BandField::Gain;
            }
            Panel::Bands => match self.band_field {
                BandField::Gain => self.band_field = BandField::Freq,
                BandField::Freq => self.band_field = BandField::Q,
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
                        // log-scale: ±0.05 → ±1 semitone, ±0.10 → ±2 semitones
                        let semitones = delta * 20.0;
                        let f = (band.freq * 2.0_f64.powf(semitones / 12.0)).clamp(20.0, 20000.0);
                        let f = (f * 10.0).round() / 10.0;
                        (f, band.gain_db, band.q)
                    }
                    BandField::Gain => {
                        // 0.5 dB per tick (10× delta) so it always clears the
                        // 0.1 dB rounding grid — a smaller step rounds back to
                        // the same value and appears "stuck".
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
    /// The field is `Some` when a Freq/Gain/Q cell was hit; the Type cell
    /// returns `Some(None)` semantics via a separate flag handled by callers.
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
            Some(5) => BandHit::Toggle,
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
            // Scroll over a specific cell adjusts that field; elsewhere uses the
            // currently-selected field.
            if let BandHit::Field(f) = hit {
                self.band_field = f;
            }
            self.adjust(delta);
        }
    }

    pub fn add_band(&mut self) {
        self.send(Command::AddBand {
            band_type: BandType::Peaking,
            freq: 1000.0,
            gain_db: 0.0,
            q: 1.4,
        });
        self.refresh_state();
        // Move cursor to new band
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
                self.send(Command::SetEffectEnabled { effect, enabled });
                self.refresh_state();
            }
            Panel::Bands => {
                let Some(state) = &self.state else { return };
                let idx = self.band_cursor;
                let Some(band) = state.bands.get(idx) else {
                    return;
                };
                self.send(Command::SetBandEnabled {
                    index: idx,
                    enabled: !band.enabled,
                });
                self.refresh_state();
            }
        }
    }

    pub fn preamp_adjust(&mut self, delta: f64) {
        let current = self.state.as_ref().map(|s| s.preamp_db).unwrap_or(0.0);
        let new_db = ((current + delta) * 10.0).round() / 10.0;
        let new_db = new_db.clamp(-20.0, 20.0);
        self.send(Command::SetPreamp { db: new_db });
        self.refresh_state();
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
            | Response::Mappings(_) => Ok(()),
            Response::Error(e) => Err(anyhow!("{e}")),
            Response::StateChanged(_) => Ok(()),
        }
    }
}

fn socket_path() -> PathBuf {
    if let Ok(p) = env::var(resonance_ipc::SOCKET_PATH_ENV) {
        return PathBuf::from(p);
    }
    let runtime = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime).join(resonance_ipc::DEFAULT_SOCKET_FILENAME)
}
