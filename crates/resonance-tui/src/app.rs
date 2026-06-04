use anyhow::{Result, anyhow};
use resonance_ipc::{
    Command, DaemonState, FxEffectId, Response,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    LoadPreset { input: String },
}

pub struct App {
    pub state: Option<DaemonState>,
    pub running: bool,
    pub focus: Panel,
    pub effect_cursor: usize,
    pub band_cursor: usize,
    pub mode: InputMode,
    pub status: String,
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
            mode: InputMode::Normal,
            status: String::new(),
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
        self.mode = InputMode::LoadPreset {
            input: String::new(),
        };
    }

    pub fn confirm_load_preset(&mut self) {
        if let InputMode::LoadPreset { input } = &self.mode.clone() {
            let path = input.clone();
            self.mode = InputMode::Normal;
            if !path.is_empty() {
                self.send(Command::LoadPreset { path });
                self.refresh_state();
            }
        }
    }

    pub fn cancel_input(&mut self) {
        self.mode = InputMode::Normal;
    }

    pub fn push_input_char(&mut self, c: char) {
        if let InputMode::LoadPreset { input } = &mut self.mode {
            input.push(c);
        }
    }

    pub fn pop_input_char(&mut self) {
        if let InputMode::LoadPreset { input } = &mut self.mode {
            input.pop();
        }
    }

    // ── Navigation ────────────────────────────────────────────────────────

    pub fn next_panel(&mut self) {
        self.focus = match self.focus {
            Panel::Effects => Panel::Bands,
            Panel::Bands => Panel::Effects,
        };
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
                let new_val = (current + delta).clamp(0.0, 1.0);
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
                let new_gain = (band.gain_db + delta).clamp(-20.0, 20.0);
                let new_gain = (new_gain * 10.0).round() / 10.0;
                self.send(Command::SetBand {
                    index: idx,
                    freq: band.freq,
                    gain_db: new_gain,
                    q: band.q,
                });
                self.refresh_state();
            }
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
            Response::Ok | Response::State(_) | Response::PresetList(_) => Ok(()),
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
