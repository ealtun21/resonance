use ratatui::layout::Rect;
use resonance_autoeq::{BandKind, Smoothing};
use resonance_ipc::{
    BandDynamics, BandState, BandType, ChannelMask, Command, DaemonState, EffectsState, FxEffectId,
    Response, RoutingMatrix,
    transport::{SyncClient as IpcClient, TransportError},
};
use resonance_reference::download::{self, Catalog, DlCmd, DlEvent, ModelEntry, TargetEntry};
use resonance_reference::reference::{PersistedReference, ReferenceState};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

/// A finished background Auto-EQ fit, applied on the UI thread.
struct AutoEqDone {
    preamp_db: f64,
    bands: Vec<BandState>,
}

/// Which squig.link list the online browser is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SquigTab {
    /// Headphone/IEM measurements (loaded as the active measurement).
    Models,
    /// Target curves (added to the target library).
    Targets,
}

/// Spectrum envelope time constants: bars snap up, glide down.
const SPECTRUM_ATTACK_TAU: f32 = 0.020;
const SPECTRUM_DECAY_TAU: f32 = 0.200;
/// How long a status message stays visible after it's set.
const STATUS_TTL: Duration = Duration::from_secs(4);

/// Whether per-channel controls (the `Ch` column, the `c`/`w` keys) should be
/// visible: always on genuinely multichannel devices (`>2`), and opt-in on
/// stereo via the `show_channels` pref. Mono (`<2`) never shows them.
pub(crate) fn channels_visible(show_channels: bool, channels: usize) -> bool {
    channels > 2 || (show_channels && channels >= 2)
}

/// Build the compact status-bar hint naming hidden-but-active advanced
/// features, or `None` when nothing hidden is doing anything.
// `slope`/`scope` are deliberately parallel feature names.
#[allow(clippy::similar_names)]
pub(crate) fn advanced_hint_label(
    dither: bool,
    ir: bool,
    slope: bool,
    scope: bool,
    dynamics: bool,
    channels: bool,
) -> Option<String> {
    let parts: Vec<&str> = [
        ("dither", dither),
        ("ir", ir),
        ("slope", slope),
        ("scope", scope),
        ("dyn", dynamics),
        ("channels", channels),
    ]
    .into_iter()
    .filter_map(|(name, on)| on.then_some(name))
    .collect();
    (!parts.is_empty()).then(|| format!("adv: {}", parts.join(" ")))
}

/// Rows of the per-band dynamics editor, in display order: threshold / range /
/// attack / release.
pub(crate) const DYN_FIELDS: usize = 4;

/// Step one dynamics-editor field by `steps` (signed; Shift = ×5 upstream),
/// clamped to the ranges the daemon accepts. Each field gets a step size that
/// suits its scale — fine values are the CLI's job.
pub(crate) fn dyn_field_adjust(d: &mut BandDynamics, field: usize, steps: f64) {
    match field {
        0 => d.threshold_db = (d.threshold_db + steps).clamp(-80.0, 0.0),
        1 => d.range_db = (d.range_db + 0.5 * steps).clamp(-24.0, 24.0),
        2 => d.attack_ms = (d.attack_ms + steps).clamp(0.1, 500.0),
        _ => d.release_ms = (d.release_ms + 10.0 * steps).clamp(1.0, 5000.0),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Effects,
    Bands,
    /// The interactive FR graph — nodes editable by keyboard (arrows) and mouse
    /// (click-select + drag).
    Graph,
    /// Per-application volume/mute list (in the Tab cycle only when visible:
    /// `show_apps` pref on and the daemon reports application streams).
    Apps,
    /// Per-output-sink (device) volume/mute list (in the Tab cycle only when
    /// visible: `show_sinks` pref on and the daemon reports output sinks).
    Sinks,
}

/// An in-progress mouse drag of a band node on the FR graph.
#[derive(Debug, Clone, Copy)]
struct Drag {
    band: usize,
    /// Right-button drag tunes Q (vs left = freq+gain).
    q_mode: bool,
    /// Last row seen (for relative Q drag).
    last_row: u16,
    /// Whether an undo snapshot has been pushed for this gesture yet (pushed on
    /// the first move so a click-without-drag doesn't add an undo entry).
    pushed: bool,
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
    /// The per-band channel-target cell (multichannel only).
    Channels,
}

pub enum InputMode {
    Normal,
    Browse(crate::browser::Browser),
    SelectOutput {
        sinks: Vec<String>,
        cursor: usize,
    },
    Settings(crate::settings::SettingsState),
    /// Per-band channel-target picker (multichannel only): toggle which channels
    /// the selected band's filter applies to. `mask` is edited live; `cursor`
    /// indexes `0..channels`.
    SelectBandChannels {
        index: usize,
        mask: ChannelMask,
        cursor: usize,
    },
    /// Per-band dynamic-EQ editor (peaking bands only): tune the detector's
    /// threshold / range / attack / release. `dynamics` is edited locally;
    /// Enter applies (enabling dynamics if they were off), Esc cancels.
    EditBandDynamics {
        index: usize,
        dynamics: BandDynamics,
        cursor: usize,
    },
    /// squig.link online browser: search + pick a measurement (or target) from
    /// the federated catalog. `cursor` indexes into the query-filtered list.
    SquigBrowse {
        tab: SquigTab,
        query: String,
        cursor: usize,
    },
    Help,
}

impl InputMode {
    pub fn is_normal(&self) -> bool {
        matches!(self, InputMode::Normal)
    }
}

// `app_cursor` mirrors the sibling `effect_cursor`/`band_cursor` naming; the
// `_cursor` convention is clearer than renaming to dodge the struct-name lint.
#[allow(clippy::struct_field_names)]
pub struct App {
    pub state: Option<DaemonState>,
    pub running: bool,
    pub focus: Panel,
    pub effect_cursor: usize,
    pub app_cursor: usize,
    pub sink_cursor: usize,
    pub band_cursor: usize,
    pub band_field: BandField,
    pub mode: InputMode,
    pub status: String,
    /// When the current status message stops being shown. Action feedback would
    /// otherwise be wiped by the next state poll (every few ms) before the user
    /// can read it; it now lingers for `STATUS_TTL`.
    status_until: Instant,
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
    /// Reference/measurement target-curve overlay state (shared with the GUI).
    pub reference: ReferenceState,
    /// Background Auto-EQ fit result channel + in-flight flag.
    autoeq_tx: Sender<AutoEqDone>,
    autoeq_rx: Receiver<AutoEqDone>,
    pub autoeq_busy: bool,
    /// squig.link downloader: command sender + event receiver, plus the latest
    /// catalog snapshot and busy/status (drained by `pump_downloads`).
    dl_tx: Sender<DlCmd>,
    dl_rx: Receiver<DlEvent>,
    pub catalog: Option<Catalog>,
    pub dl_busy: bool,
    pub dl_status: String,
    /// Whether `DlCmd::Init` has been sent (catalog warmed lazily on first open).
    dl_inited: bool,
    /// In-progress FR-graph node drag (None when not dragging).
    graph_drag: Option<Drag>,
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
        // Restore the reference overlay (loaded measurement, target, customizer)
        // from the last session, like the GUI does.
        let mut reference = ReferenceState::default();
        if let Some(p) = Self::load_reference() {
            reference.restore(p);
        }
        let (autoeq_tx, autoeq_rx) = std::sync::mpsc::channel();
        // Spawn the squig.link downloader worker idle (no Init yet — the catalog
        // is warmed lazily the first time the online browser opens, so a plain
        // TUI launch never touches the network). The TUI polls, so the wake
        // callback is a no-op.
        let (dl_tx, dl_rx) = download::spawn(std::sync::Arc::new(|| {}));
        Self {
            state: None,
            running: true,
            focus: Panel::Effects,
            effect_cursor: 0,
            app_cursor: 0,
            sink_cursor: 0,
            band_cursor: 0,
            band_field: BandField::Gain,
            mode: InputMode::Normal,
            status: String::new(),
            status_until: Instant::now(),
            last_frame: Rect::default(),
            prefs,
            spectrum_display: Vec::new(),
            last_anim: Instant::now(),
            ipc: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            clip_until: None,
            daemon_status: resonance_ipc::service::Status::default(),
            reference,
            autoeq_tx,
            autoeq_rx,
            autoeq_busy: false,
            dl_tx,
            dl_rx,
            catalog: None,
            dl_busy: false,
            dl_status: String::new(),
            dl_inited: false,
            graph_drag: None,
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
        let msg = match r {
            Ok(()) => format!("daemon: {label} ok"),
            Err(e) => format!("daemon: {label} failed: {e}"),
        };
        self.set_status(msg);
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
                self.set_status("undo");
            }
            None => self.set_status("nothing to undo"),
        }
    }

    pub fn redo(&mut self) {
        match self.redo_stack.pop() {
            Some(next) => {
                if let Some(cur) = self.snapshot() {
                    self.undo_stack.push(cur);
                }
                self.apply_snapshot(&next);
                self.set_status("redo");
            }
            None => self.set_status("nothing to redo"),
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
            // Disconnected: decay the bars to zero instead of freezing the last
            // frame, so a dead daemon doesn't look like it's still playing.
            let coeff = 1.0 - (-dt / SPECTRUM_DECAY_TAU).exp();
            for disp in &mut self.spectrum_display {
                *disp -= *disp * coeff;
            }
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

    /// Set the transient status message and (re)start its visibility window.
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_until = Instant::now() + STATUS_TTL;
    }

    /// The status message, blanked once its TTL has elapsed.
    pub fn status_text(&self) -> &str {
        if Instant::now() < self.status_until {
            &self.status
        } else {
            ""
        }
    }

    pub fn connect(&mut self) {
        // The TUI polls less aggressively than the GUI, so a slightly longer
        // (read+write) timeout than the GUI's is fine.
        match IpcClient::connect_with_timeout(Duration::from_millis(500)) {
            Ok(c) => {
                self.ipc = Some(c);
                self.set_status("connected");
            }
            Err(e) => {
                self.set_status(format!("not connected: {e}"));
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
                // Keep the band cursor in range: a profile load or another
                // client can shrink the band list out from under it, after which
                // band edits/deletes would target the wrong (or no) band.
                if s.bands.is_empty() {
                    self.band_cursor = 0;
                } else {
                    self.band_cursor = self.band_cursor.min(s.bands.len() - 1);
                }
                self.state = Some(s);
                // Don't wipe the status here — it expires on its own TTL so
                // action feedback survives the next poll a few ms later.
            }
            Err(e) => {
                self.set_status(format!("error: {e}"));
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
            // A daemon-level rejection (e.g. "profile not found") is not a
            // broken connection — show it but keep the socket. Only a real
            // transport error drops the connection so we reconnect.
            if let TransportError::Daemon(msg) = e {
                self.set_status(msg);
            } else {
                self.set_status(format!("error: {e}"));
                self.ipc = None;
            }
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
                self.set_status(format!("error: {e}"));
                self.ipc = None;
                None
            }
        }
    }

    // ── Normal-mode actions ────────────────────────────────────────────────

    pub fn toggle_power(&mut self) {
        let enabled = self.state.as_ref().is_none_or(|s| !s.enabled);
        // Snapshot first: `enabled` is part of the undo state, so toggling power
        // must be undoable like every other edit.
        self.push_undo();
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
        let (action, purpose) = match &mut self.mode {
            InputMode::Browse(b) => (b.enter(), b.purpose),
            _ => return,
        };
        if let Some(path) = action {
            self.mode = InputMode::Normal;
            match purpose {
                crate::browser::BrowsePurpose::LoadPreset => {
                    self.import_and_load(path);
                    self.refresh_state();
                }
                crate::browser::BrowsePurpose::LoadMeasurement => self.load_measurement(&path),
                crate::browser::BrowsePurpose::LoadIr => {
                    match self.query(Command::SetConvolutionIr { path }) {
                        Some(Response::Ok) => self.set_status("impulse response loaded"),
                        Some(Response::Error(e)) => self.set_status(format!("IR load failed: {e}")),
                        _ => self.set_status("IR load failed"),
                    }
                    self.refresh_state();
                }
            }
        }
    }

    fn import_and_load(&mut self, path: String) {
        match self.query(Command::ImportPreset { path, name: None }) {
            Some(Response::Imported(name)) => {
                self.send(Command::LoadProfile { name: name.clone() });
                self.set_status(format!("imported + loaded '{name}'"));
            }
            Some(Response::Error(e)) => self.set_status(format!("import failed: {e}")),
            _ => self.set_status("import failed"),
        }
    }

    // ── Navigation ────────────────────────────────────────────────────────

    /// Tab cycles left-to-right across columns, then wraps:
    /// Effects → Bands(Freq) → Bands(Gain) → Bands(Q) → Graph → Effects
    pub fn next_panel(&mut self) {
        match self.focus {
            Panel::Effects => {
                self.focus = Panel::Bands;
                self.band_field = BandField::Freq;
            }
            Panel::Bands => match self.band_field {
                BandField::Freq => self.band_field = BandField::Gain,
                BandField::Gain => self.band_field = BandField::Q,
                BandField::Q => self.focus = Panel::Graph,
            },
            // After the graph, visit the first visible extra panel (Applications
            // then Outputs), else wrap straight back to Effects.
            Panel::Graph => {
                self.focus = if self.apps_visible() {
                    Panel::Apps
                } else if self.sinks_visible() {
                    Panel::Sinks
                } else {
                    Panel::Effects
                };
            }
            Panel::Apps => {
                self.focus = if self.sinks_visible() {
                    Panel::Sinks
                } else {
                    Panel::Effects
                };
            }
            Panel::Sinks => self.focus = Panel::Effects,
        }
    }

    /// Whether the daemon currently reports any application streams.
    pub fn has_apps(&self) -> bool {
        self.state.as_ref().is_some_and(|s| !s.apps.is_empty())
    }

    /// Whether the daemon currently reports any output sinks.
    pub fn has_sinks(&self) -> bool {
        self.state.as_ref().is_some_and(|s| !s.sinks.is_empty())
    }

    /// Whether the Applications panel is visible: the `show_apps` toggle is on
    /// AND the daemon reports streams (progressive disclosure). Drives layout,
    /// rendering and Tab inclusion.
    pub fn apps_visible(&self) -> bool {
        self.prefs.show_apps && self.has_apps()
    }

    /// Whether the Outputs panel is visible (`show_sinks` toggle on + sinks
    /// reported).
    pub fn sinks_visible(&self) -> bool {
        self.prefs.show_sinks && self.has_sinks()
    }

    /// Toggle the Applications panel; if it was focused and is now hidden, move
    /// focus back to Effects so the cursor never lands on an invisible panel.
    pub fn toggle_apps_panel(&mut self) {
        self.prefs.show_apps = !self.prefs.show_apps;
        self.prefs.save();
        if !self.apps_visible() && self.focus == Panel::Apps {
            self.focus = Panel::Effects;
        }
        self.set_status(if self.prefs.show_apps {
            "applications panel: on"
        } else {
            "applications panel: off"
        });
    }

    /// Toggle the Outputs panel (see [`Self::toggle_apps_panel`]).
    pub fn toggle_sinks_panel(&mut self) {
        self.prefs.show_sinks = !self.prefs.show_sinks;
        self.prefs.save();
        if !self.sinks_visible() && self.focus == Panel::Sinks {
            self.focus = Panel::Effects;
        }
        self.set_status(if self.prefs.show_sinks {
            "outputs panel: on"
        } else {
            "outputs panel: off"
        });
    }

    pub fn cursor_up(&mut self) {
        match self.focus {
            Panel::Effects => {
                if self.effect_cursor > 0 {
                    self.effect_cursor -= 1;
                }
            }
            Panel::Apps => {
                if self.app_cursor > 0 {
                    self.app_cursor -= 1;
                }
            }
            Panel::Sinks => {
                if self.sink_cursor > 0 {
                    self.sink_cursor -= 1;
                }
            }
            Panel::Bands => {
                if self.band_cursor > 0 {
                    self.band_cursor -= 1;
                }
            }
            // Graph: ↑/↓ move the node's gain, handled in the key dispatcher.
            Panel::Graph => {}
        }
    }

    pub fn cursor_down(&mut self) {
        match self.focus {
            Panel::Effects => {
                let max = EFFECT_NAMES.len() - 1;
                if self.effect_cursor < max {
                    self.effect_cursor += 1;
                }
            }
            Panel::Apps => {
                let max = self
                    .state
                    .as_ref()
                    .map_or(0, |s| s.apps.len().saturating_sub(1));
                if self.app_cursor < max {
                    self.app_cursor += 1;
                }
            }
            Panel::Sinks => {
                let max = self
                    .state
                    .as_ref()
                    .map_or(0, |s| s.sinks.len().saturating_sub(1));
                if self.sink_cursor < max {
                    self.sink_cursor += 1;
                }
            }
            Panel::Bands => {
                let max = self
                    .state
                    .as_ref()
                    .map_or(0, |s| s.bands.len().saturating_sub(1));
                if self.band_cursor < max {
                    self.band_cursor += 1;
                }
            }
            Panel::Graph => {}
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
            Panel::Apps => {
                // Per-app volume nudge: capture key + current value first so the
                // immutable `state` borrow ends before `send`/`refresh` (&mut self).
                let Some((key, cur)) = self
                    .state
                    .as_ref()
                    .and_then(|s| s.apps.get(self.app_cursor))
                    .map(|a| (a.key.clone(), a.volume))
                else {
                    return;
                };
                let new_vol = ((cur + delta).clamp(0.0, 1.0) * 100.0).round() / 100.0;
                if (new_vol - cur).abs() > 0.001 {
                    // Per-app volume isn't part of the EQ profile — no undo snapshot.
                    self.send(Command::SetAppVolume {
                        key,
                        volume: new_vol,
                    });
                    self.refresh_state();
                }
            }
            Panel::Sinks => {
                // Per-output-sink volume nudge (mirrors the Apps arm). Not part
                // of the EQ profile — no undo snapshot.
                let Some((name, cur)) = self
                    .state
                    .as_ref()
                    .and_then(|s| s.sinks.get(self.sink_cursor))
                    .map(|s| (s.name.clone(), s.volume))
                else {
                    return;
                };
                let new_vol = ((cur + delta).clamp(0.0, 1.0) * 100.0).round() / 100.0;
                if (new_vol - cur).abs() > 0.001 {
                    self.send(Command::SetSinkVolume {
                        name,
                        volume: new_vol,
                    });
                    self.refresh_state();
                }
            }
            // Graph uses dedicated 2-axis nudges (gain/freq), not the single
            // active-field `adjust`; handled in the key dispatcher.
            Panel::Graph => {}
        }
    }

    // ── Mouse hit-testing ─────────────────────────────────────────────────

    /// Resolve a click on the effects column to an effect index.
    fn hit_effect(&self, col: u16, row: u16) -> Option<usize> {
        let p = crate::layout::panes(
            self.last_frame,
            self.prefs.show_spectrum,
            self.apps_visible(),
            self.sinks_visible(),
        );
        if !crate::layout::hit(p.effects, col, row) {
            return None;
        }
        let inner = crate::layout::block_inner(p.effects);
        let rows = crate::layout::effect_rows(inner, EFFECT_NAMES.len());
        rows.iter().position(|r| crate::layout::hit(*r, col, row))
    }

    /// Resolve a click on the bands panel to (band index, optional field).
    fn hit_band(&self, col: u16, row: u16) -> Option<(usize, BandHit)> {
        let p = crate::layout::panes(
            self.last_frame,
            self.prefs.show_spectrum,
            self.apps_visible(),
            self.sinks_visible(),
        );
        if !crate::layout::hit(p.bands, col, row) {
            return None;
        }
        let inner = crate::layout::block_inner(p.bands);
        if inner.height < 2 || row < inner.y + 1 {
            return None; // border or header
        }
        let n = self.state.as_ref().map_or(0, |s| s.bands.len());
        let visible = (inner.height - 1) as usize;
        let offset = crate::layout::band_scroll_offset(self.band_cursor, n, visible);
        let line = (row - (inner.y + 1)) as usize;
        let idx = offset + line;
        if idx >= n {
            return None;
        }
        let row_rect = ratatui::layout::Rect::new(inner.x, row, inner.width, 1);
        let show_ch = self.show_ch();
        let cols = crate::layout::band_columns(row_rect, show_ch);
        let hit = match cols.iter().position(|c| crate::layout::hit(*c, col, row)) {
            Some(1) => BandHit::Type,
            Some(2) => BandHit::Field(BandField::Freq),
            Some(3) => BandHit::Field(BandField::Gain),
            Some(4) => BandHit::Field(BandField::Q),
            Some(6) => BandHit::Toggle, // 5 is the spacer column
            // The Ch column is rect 7 only when shown; otherwise rect 7 is the
            // gain bar (→ Row). The fixed columns 0–6 are unaffected.
            Some(7) if show_ch => BandHit::Channels,
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
                // Click the Ch cell → open the picker (band_cursor already set).
                BandHit::Channels => self.begin_select_band_channels(),
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
        if !matches!(self.focus, Panel::Bands | Panel::Graph) {
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

    /// Cycle the filter slope of the selected band (`S` key): 12 → 24 → 48 → 12
    /// dB/oct. Only shelves + HP/LP have a slope; other (single-biquad) band
    /// types show a status hint instead of a no-op edit.
    pub fn cycle_band_slope(&mut self) {
        if !matches!(self.focus, Panel::Bands | Panel::Graph) {
            return;
        }
        let Some(state) = &self.state else { return };
        let idx = self.band_cursor;
        let Some(band) = state.bands.get(idx) else {
            return;
        };
        if !band.band_type.uses_slope() {
            self.set_status("slope applies to shelves + HP/LP only");
            return;
        }
        let next = resonance_ipc::next_slope_db_oct(band.slope_db_oct);
        self.push_undo();
        self.send(Command::SetBandSlope {
            index: idx,
            slope_db_oct: next,
        });
        self.refresh_state();
    }

    /// Cycle the stereo scope of the selected band (`M` key): Stereo → Mid →
    /// Side → Stereo. Applies to every band type but is only audible on
    /// >= 2-channel streams.
    pub fn cycle_band_scope(&mut self) {
        if !matches!(self.focus, Panel::Bands | Panel::Graph) {
            return;
        }
        let Some(state) = &self.state else { return };
        let idx = self.band_cursor;
        let Some(band) = state.bands.get(idx) else {
            return;
        };
        let next = band.scope.next();
        self.push_undo();
        self.send(Command::SetBandScope {
            index: idx,
            scope: next,
        });
        self.refresh_state();
    }

    /// Toggle dynamic EQ on the selected band (`y` key): on enables with the
    /// default parameters (tune them with `Y`), off reverts to a static band.
    /// Peaking bands only; other types show a status hint instead of a no-op
    /// edit.
    pub fn toggle_band_dynamics(&mut self) {
        if !matches!(self.focus, Panel::Bands | Panel::Graph) {
            return;
        }
        let Some(state) = &self.state else { return };
        let idx = self.band_cursor;
        let Some(band) = state.bands.get(idx) else {
            return;
        };
        if !band.band_type.uses_dynamics() {
            self.set_status("dynamics applies to peaking bands only");
            return;
        }
        let dynamics = match band.dynamics {
            Some(_) => None,
            None => Some(BandDynamics::DEFAULT),
        };
        self.push_undo();
        self.send(Command::SetBandDynamics {
            index: idx,
            dynamics,
        });
        self.refresh_state();
    }

    /// Cycle the selected band's audition (`L` key): Off → Solo → Listen → Off.
    /// Solo bypasses other bands; Listen band-passes this band's region.
    /// Transient — no undo entry, never saved; suspends linear-phase while
    /// active. The daemon auto-clears it on any band-table edit.
    pub fn cycle_band_audition(&mut self) {
        if !matches!(self.focus, Panel::Bands | Panel::Graph) {
            return;
        }
        let Some(state) = &self.state else { return };
        let idx = self.band_cursor;
        if idx >= state.bands.len() {
            return;
        }
        let cur = state.audition.filter(|a| a.band == idx).map(|a| a.mode);
        let next = match cur {
            None => Some(resonance_ipc::AuditionMode::Solo),
            Some(resonance_ipc::AuditionMode::Solo) => Some(resonance_ipc::AuditionMode::Listen),
            Some(resonance_ipc::AuditionMode::Listen) => None,
        };
        self.send(Command::SetBandAudition {
            index: next.map(|_| idx),
            mode: next.unwrap_or(resonance_ipc::AuditionMode::Solo),
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
            // Both Bands and Graph toggle the selected band's enable.
            Panel::Bands | Panel::Graph => {
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
            // Toggle the selected application's mute.
            Panel::Apps => {
                let Some((key, muted)) = self
                    .state
                    .as_ref()
                    .and_then(|s| s.apps.get(self.app_cursor))
                    .map(|a| (a.key.clone(), a.muted))
                else {
                    return;
                };
                // Per-app mute isn't part of the EQ profile — no undo snapshot.
                self.send(Command::SetAppMute { key, muted: !muted });
                self.refresh_state();
            }
            // Toggle the selected output sink's mute.
            Panel::Sinks => {
                let Some((name, muted)) = self
                    .state
                    .as_ref()
                    .and_then(|s| s.sinks.get(self.sink_cursor))
                    .map(|s| (s.name.clone(), s.muted))
                else {
                    return;
                };
                self.send(Command::SetSinkMute {
                    name,
                    muted: !muted,
                });
                self.refresh_state();
            }
        }
    }

    // ── FR-graph node editing (keyboard + mouse) ────────────────────────────

    /// The interactive plot rectangle of the EQ curve (None if too small).
    fn eq_plot(&self) -> Option<Rect> {
        let p = crate::layout::panes(
            self.last_frame,
            self.prefs.show_spectrum,
            self.apps_visible(),
            self.sinks_visible(),
        );
        let plot = crate::layout::eq_plot_area(p.eq);
        (plot.width >= 2 && plot.height >= 2).then_some(plot)
    }

    /// True if (col,row) is inside the EQ-curve panel.
    pub fn in_eq_panel(&self, col: u16, row: u16) -> bool {
        let p = crate::layout::panes(
            self.last_frame,
            self.prefs.show_spectrum,
            self.apps_visible(),
            self.sinks_visible(),
        );
        crate::layout::hit(p.eq, col, row)
    }

    /// The enabled band whose node is nearest (in cells) to (col,row).
    fn nearest_band(&self, col: u16, row: u16, plot: Rect) -> Option<usize> {
        let s = self.state.as_ref()?;
        let mut best: Option<usize> = None;
        let mut best_d = i64::MAX;
        for (i, b) in s.bands.iter().enumerate() {
            if !b.enabled {
                continue;
            }
            let nc = i64::from(crate::layout::graph_node_col(plot, b.freq));
            let nr = i64::from(crate::layout::graph_node_row(plot, b.gain_db));
            let d = (nc - i64::from(col)).pow(2) + (nr - i64::from(row)).pow(2);
            if d < best_d {
                best_d = d;
                best = Some(i);
            }
        }
        best
    }

    pub fn is_graph_dragging(&self) -> bool {
        self.graph_drag.is_some()
    }

    /// Select the previous/next band node (Graph-panel `[` / `]`).
    pub fn graph_select(&mut self, delta: i32) {
        let n = self.state.as_ref().map_or(0, |s| s.bands.len());
        if n == 0 {
            return;
        }
        let max = n as i32 - 1;
        self.band_cursor = (self.band_cursor as i32 + delta).clamp(0, max) as usize;
    }

    /// Nudge the selected node by `d_gain` dB and/or `d_semitones` (keyboard
    /// arrows on the Graph panel). One undo step per press.
    pub fn graph_nudge(&mut self, d_gain: f64, d_semitones: f64) {
        let Some((i, mut freq, mut gain, q)) = self.state.as_ref().and_then(|s| {
            let i = self.band_cursor;
            s.bands.get(i).map(|b| (i, b.freq, b.gain_db, b.q))
        }) else {
            return;
        };
        if d_semitones != 0.0 {
            freq = (freq * 2f64.powf(d_semitones / 12.0)).clamp(20.0, 20000.0);
            freq = (freq * 10.0).round() / 10.0;
        }
        if d_gain != 0.0 {
            let lim = crate::layout::GRAPH_DB_RANGE;
            gain = ((gain + d_gain).clamp(-lim, lim) * 10.0).round() / 10.0;
        }
        self.push_undo();
        self.send(Command::SetBand {
            index: i,
            freq,
            gain_db: gain,
            q,
        });
        self.refresh_state();
    }

    /// Mouse press on the graph: grab the nearest node for dragging (left =
    /// freq+gain, right = Q). The undo snapshot is deferred to the first move so
    /// a click that only selects doesn't add an undo entry.
    pub fn graph_press(&mut self, col: u16, row: u16, right: bool) {
        let Some(plot) = self.eq_plot() else { return };
        let Some(i) = self.nearest_band(col, row, plot) else {
            return;
        };
        self.focus = Panel::Graph;
        self.band_cursor = i;
        self.graph_drag = Some(Drag {
            band: i,
            q_mode: right,
            last_row: row,
            pushed: false,
        });
    }

    /// Mouse drag: move the grabbed node to the cursor (freq+gain), or tune its
    /// Q relative to the last row (right-drag). No-op if not dragging.
    pub fn graph_drag_to(&mut self, col: u16, row: u16) {
        let Some(plot) = self.eq_plot() else { return };
        let Some(drag) = self.graph_drag else { return };
        let i = drag.band;
        let Some((bf, bg, bq)) = self
            .state
            .as_ref()
            .and_then(|s| s.bands.get(i))
            .map(|b| (b.freq, b.gain_db, b.q))
        else {
            return;
        };
        if !drag.pushed {
            self.push_undo();
            if let Some(d) = &mut self.graph_drag {
                d.pushed = true;
            }
        }
        let (freq, gain, q) = if drag.q_mode {
            // Relative drag: up = narrower (higher Q), exponential like the GUI.
            let dy = f64::from(drag.last_row) - f64::from(row);
            let q = (bq * (dy * 0.06).exp()).clamp(0.1, 20.0);
            if let Some(d) = &mut self.graph_drag {
                d.last_row = row;
            }
            (bf, bg, (q * 100.0).round() / 100.0)
        } else {
            let (f, g) = crate::layout::graph_pixel_to_data(plot, col, row);
            ((f * 10.0).round() / 10.0, (g * 10.0).round() / 10.0, bq)
        };
        self.send(Command::SetBand {
            index: i,
            freq,
            gain_db: gain,
            q,
        });
        self.refresh_state();
    }

    pub fn graph_release(&mut self) {
        self.graph_drag = None;
    }

    /// Mouse wheel on the graph: select the nearest node and nudge its gain.
    pub fn graph_scroll(&mut self, col: u16, row: u16, delta: f64) {
        let Some(plot) = self.eq_plot() else { return };
        if let Some(i) = self.nearest_band(col, row, plot) {
            self.focus = Panel::Graph;
            self.band_cursor = i;
            self.graph_nudge(delta, 0.0);
        }
    }

    // ── Channel targeting / routing (multichannel) ──────────────────────────

    /// Whether per-channel controls should surface. Progressive disclosure:
    /// stereo/mono users never see the channel column or the picker — only
    /// Reveal per-band channel targeting on devices with more than 2 channels;
    /// stereo reveals it only when the user opts in via the `show_channels` pref.
    pub(crate) fn show_ch(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(|s| channels_visible(self.prefs.show_channels, s.channels))
    }

    /// Compact hint for the status bar: names advanced features that are hidden
    /// (their toggle off) yet hold a non-default value, so nothing runs
    /// invisibly. `None` when every hidden feature is at its default.
    // `slope`/`scope` are deliberately parallel feature names.
    #[allow(clippy::similar_names)]
    pub(crate) fn advanced_active_hint(&self) -> Option<String> {
        let s = self.state.as_ref()?;
        let dither = !self.prefs.show_dither && s.dither_bits.is_some();
        let ir = !self.prefs.show_ir && s.convolution.as_ref().is_some_and(|c| c.enabled);
        let slope = !self.prefs.show_slope
            && s.bands
                .iter()
                .any(|b| b.band_type.uses_slope() && b.slope_db_oct != 12);
        let scope = !self.prefs.show_scope
            && s.bands
                .iter()
                .any(|b| b.scope != resonance_ipc::BandScope::Stereo);
        let dynamics = !self.prefs.show_dynamics && s.bands.iter().any(|b| b.dynamics.is_some());
        let channels = !channels_visible(self.prefs.show_channels, s.channels)
            && (s.routing.is_some() || s.bands.iter().any(|b| !b.channels.is_global(s.channels)));
        advanced_hint_label(dither, ir, slope, scope, dynamics, channels)
    }

    /// Open the per-band channel-target picker (`c`) for the selected band.
    /// No-op on ≤2-channel devices (feature hidden) or with no bands.
    pub fn begin_select_band_channels(&mut self) {
        if !matches!(self.focus, Panel::Bands | Panel::Graph) || !self.show_ch() {
            return;
        }
        let idx = self.band_cursor;
        let mask = match self.state.as_ref().and_then(|s| s.bands.get(idx)) {
            Some(b) => b.channels,
            None => return,
        };
        self.mode = InputMode::SelectBandChannels {
            index: idx,
            mask,
            cursor: 0,
        };
    }

    pub fn band_channels_move(&mut self, delta: i32) {
        let channels = self.state.as_ref().map_or(0, |s| s.channels);
        if let InputMode::SelectBandChannels { cursor, .. } = &mut self.mode {
            if channels == 0 {
                return;
            }
            let max = channels as i32 - 1;
            *cursor = (*cursor as i32 + delta).clamp(0, max) as usize;
        }
    }

    /// Toggle the channel under the cursor in the live mask.
    pub fn band_channels_toggle(&mut self) {
        let channels = self.state.as_ref().map_or(0, |s| s.channels);
        if let InputMode::SelectBandChannels { mask, cursor, .. } = &mut self.mode {
            let c = *cursor;
            if c >= channels {
                return;
            }
            // The default ALL mask sets every bit (count-independent); normalise
            // it to the concrete in-range set before clearing one channel, so the
            // result is an exact "all but c" rather than still-global.
            let mut m = if mask.is_global(channels) {
                ChannelMask::from_indices(0..channels)
            } else {
                *mask
            };
            m = if m.contains(c) {
                m.without(c)
            } else {
                m.with(c)
            };
            *mask = m;
        }
    }

    pub fn band_channels_set_all(&mut self) {
        if let InputMode::SelectBandChannels { mask, .. } = &mut self.mode {
            *mask = ChannelMask::ALL;
        }
    }

    pub fn band_channels_set_none(&mut self) {
        if let InputMode::SelectBandChannels { mask, .. } = &mut self.mode {
            *mask = ChannelMask::NONE;
        }
    }

    /// Apply the edited mask to the band and close the picker (Enter).
    pub fn band_channels_apply(&mut self) {
        let channels = self.state.as_ref().map_or(0, |s| s.channels);
        let (index, mask) = match &self.mode {
            InputMode::SelectBandChannels { index, mask, .. } => (*index, *mask),
            _ => return,
        };
        // Collapse "every channel selected" back to the canonical ALL.
        let mask = if mask.is_global(channels) {
            ChannelMask::ALL
        } else {
            mask
        };
        self.mode = InputMode::Normal;
        self.push_undo();
        self.send(Command::SetBandChannels {
            index,
            channels: mask,
        });
        self.refresh_state();
    }

    // ── Dynamic EQ editor (peaking bands) ────────────────────────────────────

    /// Open the per-band dynamics editor (`Y`) for the selected band. Starts
    /// from the band's current parameters, or the defaults when dynamics are
    /// off — Enter then enables them too. Peaking bands only, like the toggle.
    pub fn begin_edit_band_dynamics(&mut self) {
        if !matches!(self.focus, Panel::Bands | Panel::Graph) {
            return;
        }
        let idx = self.band_cursor;
        let Some(band) = self.state.as_ref().and_then(|s| s.bands.get(idx)) else {
            return;
        };
        if !band.band_type.uses_dynamics() {
            self.set_status("dynamics applies to peaking bands only");
            return;
        }
        self.mode = InputMode::EditBandDynamics {
            index: idx,
            dynamics: band.dynamics.unwrap_or(BandDynamics::DEFAULT),
            cursor: 0,
        };
    }

    pub fn band_dynamics_move(&mut self, delta: i32) {
        if let InputMode::EditBandDynamics { cursor, .. } = &mut self.mode {
            let max = DYN_FIELDS as i32 - 1;
            *cursor = (*cursor as i32 + delta).clamp(0, max) as usize;
        }
    }

    /// Step the parameter under the cursor in the local edit copy.
    pub fn band_dynamics_adjust(&mut self, steps: f64) {
        if let InputMode::EditBandDynamics {
            dynamics, cursor, ..
        } = &mut self.mode
        {
            dyn_field_adjust(dynamics, *cursor, steps);
        }
    }

    /// Apply the edited parameters to the band and close the editor (Enter).
    pub fn band_dynamics_apply(&mut self) {
        let (index, dynamics) = match &self.mode {
            InputMode::EditBandDynamics {
                index, dynamics, ..
            } => (*index, *dynamics),
            _ => return,
        };
        self.mode = InputMode::Normal;
        self.push_undo();
        self.send(Command::SetBandDynamics {
            index,
            dynamics: Some(dynamics),
        });
        self.refresh_state();
    }

    /// True when the current routing is exactly the front-L/R swap matrix.
    pub(crate) fn is_swapped_lr(&self) -> bool {
        self.state.as_ref().is_some_and(|s| {
            s.channels >= 2 && s.routing.as_ref() == Some(&RoutingMatrix::swap(s.channels, 0, 1))
        })
    }

    /// Toggle a front L/R swap (`w`). If the routing is already exactly the L/R
    /// swap, clear it (back to straight passthrough); otherwise install it.
    /// Mirrors the GUI's "Swap L/R" control; available from 2 channels up.
    pub fn toggle_swap_lr(&mut self) {
        // Resolve channel count + current swap state up front so the immutable
        // borrow of `self.state` ends before we mutate via `send`. Only build
        // the swap matrix at ≥2 channels (it indexes channels 0 and 1).
        let (channels, is_swapped) = match &self.state {
            Some(s) if s.channels >= 2 => {
                let swap = RoutingMatrix::swap(s.channels, 0, 1);
                (s.channels, s.routing.as_ref() == Some(&swap))
            }
            Some(s) => (s.channels, false),
            None => return,
        };
        if channels < 2 {
            self.set_status("swap needs ≥2 channels");
            return;
        }
        if is_swapped {
            self.send(Command::ClearRouting);
            self.set_status("routing cleared");
        } else {
            self.send(Command::SwapChannels { a: 0, b: 1 });
            self.set_status("swapped L/R");
        }
        self.refresh_state();
    }

    // ── Convolution / impulse response ────────────────────────────────────

    /// Open the `.wav` impulse-response picker (`I`). Enter loads/replaces the
    /// IR; while the picker is open, `t` toggles bypass and `x` removes it.
    pub fn open_ir_browser(&mut self) {
        let start = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_dir())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        self.mode = InputMode::Browse(crate::browser::Browser::new_ir(start));
    }

    /// Toggle the loaded IR between enabled and bypassed (`t` in the picker).
    pub fn browse_ir_toggle(&mut self) {
        if !self.browse_is_ir() {
            return;
        }
        let Some(enabled) = self
            .state
            .as_ref()
            .and_then(|s| s.convolution.as_ref())
            .map(|c| c.enabled)
        else {
            self.set_status("no impulse response loaded");
            return;
        };
        self.send(Command::SetConvolutionEnabled { enabled: !enabled });
        self.set_status(if enabled {
            "impulse response bypassed"
        } else {
            "impulse response enabled"
        });
        self.refresh_state();
    }

    /// Remove the loaded IR entirely (`x` in the picker) and close the picker.
    pub fn browse_ir_clear(&mut self) {
        if !self.browse_is_ir() {
            return;
        }
        self.mode = InputMode::Normal;
        self.send(Command::ClearConvolutionIr);
        self.set_status("impulse response removed");
        self.refresh_state();
    }

    fn browse_is_ir(&self) -> bool {
        matches!(&self.mode, InputMode::Browse(b)
            if b.purpose == crate::browser::BrowsePurpose::LoadIr)
    }

    // ── Reference overlay / Auto-EQ ──────────────────────────────────────────

    /// Open the file picker to load a measurement curve (freq/dB `.txt`).
    pub fn begin_browse_measurement(&mut self) {
        let start = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_dir())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        self.mode = InputMode::Browse(crate::browser::Browser::new_measurement(start));
    }

    /// Load a measurement file into the reference overlay (enabling it so the
    /// overlay shows immediately).
    fn load_measurement(&mut self, path: &str) {
        if self
            .reference
            .load_measurement_file(std::path::Path::new(path))
        {
            // load_measurement_file already enables the overlay.
            let name = self.reference.measurement_name.clone();
            self.set_status(format!("loaded measurement: {name} (overlay on)"));
            // Persist now so a loaded measurement survives even a non-graceful
            // exit (the exit-time save still runs on a normal quit).
            self.save_reference();
        } else {
            self.set_status("failed to load measurement (expected a freq/dB curve)");
        }
    }

    /// Cycle the active reference target through the available options.
    pub fn cycle_reference_target(&mut self) {
        let opts = self.reference.target_options();
        if opts.is_empty() {
            return;
        }
        let idx = opts
            .iter()
            .position(|(_, sel)| *sel == self.reference.target_sel)
            .unwrap_or(0);
        let sel = opts[(idx + 1) % opts.len()].1.clone();
        self.reference.set_target(sel);
    }

    /// Kick off a background Auto-EQ fit (measurement → target). Needs both a
    /// target and a loaded measurement; the result lands via [`Self::pump_autoeq`].
    pub fn run_autoeq(&mut self) {
        if self.autoeq_busy {
            return;
        }
        let (Some(meas), Some(tgt)) = (
            self.reference.measurement.clone(),
            self.reference.target.clone(),
        ) else {
            self.set_status("Auto-EQ needs a target and a measurement");
            return;
        };
        // Sample both curves onto AutoEQ's fixed log grid (dB).
        let f = resonance_autoeq::log_freqs();
        let target: Vec<f32> = f
            .iter()
            .map(|&hz| tgt.interp(f64::from(hz)) as f32)
            .collect();
        let measured: Vec<f32> = f
            .iter()
            .map(|&hz| meas.interp(f64::from(hz)) as f32)
            .collect();
        let smoothing = if self.reference.measurement_iem {
            Smoothing::InEar
        } else {
            Smoothing::OverEar
        };
        let tx = self.autoeq_tx.clone();
        self.autoeq_busy = true;
        self.set_status("Auto-EQ: fitting…");
        std::thread::Builder::new()
            .name("resonance-autoeq".into())
            .spawn(move || {
                // Always send exactly one result, even if the fit panics, so
                // pump_autoeq() can't leave autoeq_busy stuck on forever.
                let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let res = resonance_autoeq::run(&target, &measured, 10, smoothing, 3000);
                    let bands: Vec<BandState> = res
                        .filters
                        .iter()
                        .map(|fl| BandState {
                            band_type: match fl.kind {
                                BandKind::Peak => BandType::Peaking,
                                BandKind::LowShelf => BandType::LowShelf,
                                BandKind::HighShelf => BandType::HighShelf,
                            },
                            freq: fl.freq,
                            gain_db: fl.gain_db,
                            q: fl.q,
                            enabled: true,
                            channels: ChannelMask::ALL,
                            slope_db_oct: resonance_ipc::default_slope_db_oct(),
                            scope: resonance_ipc::BandScope::Stereo,
                            dynamics: None,
                        })
                        .collect();
                    AutoEqDone {
                        preamp_db: res.preamp_db,
                        bands,
                    }
                }))
                .unwrap_or(AutoEqDone {
                    preamp_db: 0.0,
                    bands: Vec::new(),
                });
                let _ = tx.send(done);
            })
            .ok();
    }

    /// Apply any finished Auto-EQ fit (undo snapshot + `ApplyState`). Called each
    /// loop iteration, like the spectrum pump.
    pub fn pump_autoeq(&mut self) {
        while let Ok(o) = self.autoeq_rx.try_recv() {
            self.autoeq_busy = false;
            if o.bands.is_empty() {
                self.set_status("Auto-EQ: nothing to correct");
                continue;
            }
            let count = o.bands.len();
            // The chain is untouched during the fit, so snapshotting now captures
            // the correct pre-fit state for undo.
            self.push_undo();
            let effects = self
                .state
                .as_ref()
                .map(|s| s.effects.clone())
                .unwrap_or_default();
            self.send(Command::ApplyState {
                preamp_db: o.preamp_db,
                enabled: true,
                bands: o.bands,
                effects,
            });
            self.refresh_state();
            self.set_status(format!("Auto-EQ: fitted {count} bands"));
        }
    }

    // ── squig.link online browser ────────────────────────────────────────────

    /// Open the online browser, warming the catalog from cache on first open
    /// (the network warm-up happens on the downloader's background thread).
    pub fn begin_squig_browse(&mut self) {
        if !self.dl_inited {
            let _ = self.dl_tx.send(DlCmd::Init);
            self.dl_inited = true;
            self.dl_busy = true;
        }
        self.mode = InputMode::SquigBrowse {
            tab: SquigTab::Models,
            query: String::new(),
            cursor: 0,
        };
    }

    /// Drain downloader events each loop iteration: catalog/status/busy updates,
    /// install a fetched measurement (and close the browser), or add a fetched
    /// target to the library.
    pub fn pump_downloads(&mut self) {
        while let Ok(ev) = self.dl_rx.try_recv() {
            match ev {
                DlEvent::Catalog(c) => self.catalog = Some(c),
                DlEvent::Status(s) => self.dl_status = s,
                DlEvent::Busy(b) => self.dl_busy = b,
                DlEvent::Fetched(f) => {
                    let name = f.name.clone();
                    self.reference.enabled = true;
                    self.reference
                        .set_measurement(f.name, f.iem, f.left, f.right);
                    self.set_status(format!("loaded measurement: {name}"));
                    if matches!(self.mode, InputMode::SquigBrowse { .. }) {
                        self.mode = InputMode::Normal;
                    }
                    // Persist the freshly-downloaded measurement immediately.
                    self.save_reference();
                }
                DlEvent::FetchedTarget { name, curve } => {
                    self.reference.write_target(&name, &curve);
                    self.set_status(format!("added target: {name}"));
                }
            }
        }
    }

    /// Length of the currently-filtered squig list (for cursor clamping).
    fn squig_len(&self) -> usize {
        let (tab, query) = match &self.mode {
            InputMode::SquigBrowse { tab, query, .. } => (*tab, query.as_str()),
            _ => return 0,
        };
        let Some(cat) = &self.catalog else {
            return 0;
        };
        match tab {
            SquigTab::Models => squig_filter_models(cat, query).len(),
            SquigTab::Targets => squig_filter_targets(cat, query).len(),
        }
    }

    pub fn squig_move(&mut self, delta: i32) {
        let len = self.squig_len();
        if let InputMode::SquigBrowse { cursor, .. } = &mut self.mode {
            if len == 0 {
                *cursor = 0;
                return;
            }
            let max = len as i32 - 1;
            *cursor = (*cursor as i32 + delta).clamp(0, max) as usize;
        }
    }

    pub fn squig_switch_tab(&mut self) {
        if let InputMode::SquigBrowse { tab, cursor, .. } = &mut self.mode {
            *tab = match *tab {
                SquigTab::Models => SquigTab::Targets,
                SquigTab::Targets => SquigTab::Models,
            };
            *cursor = 0;
        }
    }

    pub fn squig_query_char(&mut self, c: char) {
        if let InputMode::SquigBrowse { query, cursor, .. } = &mut self.mode {
            query.push(c);
            *cursor = 0;
        }
    }

    pub fn squig_backspace(&mut self) {
        if let InputMode::SquigBrowse { query, cursor, .. } = &mut self.mode {
            query.pop();
            *cursor = 0;
        }
    }

    pub fn squig_refresh(&mut self) {
        let _ = self.dl_tx.send(DlCmd::Refresh);
        self.dl_busy = true;
        self.set_status("refreshing squig.link catalog…");
    }

    /// Fetch the selected entry: a measurement (→ active measurement) or a
    /// target curve (→ target library). The fetch runs on the worker thread.
    pub fn squig_enter(&mut self) {
        let (tab, query, cursor) = match &self.mode {
            InputMode::SquigBrowse { tab, query, cursor } => (*tab, query.clone(), *cursor),
            _ => return,
        };
        // Resolve the command in a scope so the catalog borrow ends before we
        // mutate `self` (dl_busy / status) below.
        let cmd = {
            let Some(cat) = &self.catalog else {
                return;
            };
            match tab {
                SquigTab::Models => squig_filter_models(cat, &query)
                    .get(cursor)
                    .map(|m| DlCmd::Fetch((*m).clone())),
                SquigTab::Targets => squig_filter_targets(cat, &query)
                    .get(cursor)
                    .map(|t| DlCmd::FetchTarget((*t).clone())),
            }
        };
        if let Some(cmd) = cmd {
            let fetching_target = matches!(cmd, DlCmd::FetchTarget(_));
            let _ = self.dl_tx.send(cmd);
            self.dl_busy = true;
            self.set_status(if fetching_target {
                "fetching target…"
            } else {
                "fetching measurement…"
            });
        }
    }

    // ── Reference-overlay persistence ────────────────────────────────────────

    /// Where the reference overlay snapshot is stored. JSON (not TOML): the
    /// `PersistedReference` has scalar fields after table-valued ones, which
    /// TOML can't serialise.
    fn reference_path() -> std::path::PathBuf {
        crate::prefs::Prefs::config_dir().join("tui-reference.json")
    }

    fn load_reference() -> Option<PersistedReference> {
        std::fs::read_to_string(Self::reference_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    /// Persist the reference overlay (called on exit) so a loaded measurement +
    /// target + customizer survive a restart.
    pub fn save_reference(&self) {
        let path = Self::reference_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string(&self.reference.to_persisted()) {
            let _ = std::fs::write(path, s);
        }
    }

    pub fn preamp_adjust(&mut self, delta: f64) {
        let current = self.state.as_ref().map_or(0.0, |s| s.preamp_db);
        let new_db = ((current + delta) * 10.0).round() / 10.0;
        let new_db = new_db.clamp(-20.0, 20.0);
        if (new_db - current).abs() > 1e-6 {
            self.push_undo();
            self.send(Command::SetPreamp { db: new_db });
            self.refresh_state();
        }
    }

    /// Cycle the output TPDF dither depth: Off → 16 → 20 → 24 → Off.
    pub fn cycle_dither(&mut self) {
        let current = self.state.as_ref().and_then(|s| s.dither_bits);
        let next = match current {
            None => Some(16),
            Some(16) => Some(20),
            Some(20) => Some(24),
            _ => None,
        };
        self.push_undo();
        self.send(Command::SetDither { bits: next });
        self.refresh_state();
    }

    /// Toggle the chain-level EQ phase mode: minimum (biquads, zero latency) ↔
    /// linear (static bands rendered to one FIR — no phase rotation, adds
    /// latency; Mid/Side + dynamic bands stay minimum phase).
    pub fn toggle_phase_mode(&mut self) {
        let linear = self.state.as_ref().is_some_and(|s| s.phase_mode_linear);
        self.send(Command::SetPhaseMode { linear: !linear });
        self.set_status(if linear {
            "phase: minimum"
        } else {
            "phase: linear"
        });
        self.refresh_state();
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
                        Some(Response::Ok) => self.set_status(format!("exported → {path_str}")),
                        Some(Response::Error(e)) => self.set_status(format!("export failed: {e}")),
                        _ => self.set_status("export failed"),
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
                        Some(Response::Ok) => self.set_status(format!("renamed to '{to}'")),
                        Some(Response::Error(e)) => self.set_status(format!("rename failed: {e}")),
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
            TextPurpose::TrayPoll => {
                if let Ok(n) = buf.trim().parse::<u64>() {
                    let mut cfg = resonance_ipc::tray::TrayConfig::load();
                    cfg.poll_secs = n.clamp(1, 30);
                    let _ = cfg.save();
                }
                if let InputMode::Settings(s) = &mut self.mode {
                    s.text_input = None;
                }
            }
            TextPurpose::TrayRecent => {
                if let Ok(n) = buf.trim().parse::<usize>() {
                    let mut cfg = resonance_ipc::tray::TrayConfig::load();
                    cfg.recent_count = n.clamp(0, 20);
                    let _ = cfg.save();
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
            2 => self.settings_route_output(),
            3 => self.settings_pref_activate(),
            4 => self.settings_daemon_activate(),
            5 => self.settings_reference_activate(),
            6 => self.settings_tray_activate(),
            // Tab 1 has no enter action; the wildcard covers it (and any other tab).
            _ => {}
        }
    }

    /// Reference tab actions (by cursor row): toggle on/off, cycle target, load a
    /// measurement, run Auto-EQ, toggle show-measurement / normalize.
    fn settings_reference_activate(&mut self) {
        let cursor = match &self.mode {
            InputMode::Settings(s) => s.cursor,
            _ => return,
        };
        match cursor {
            0 => self.reference.enabled = !self.reference.enabled,
            1 => self.cycle_reference_target(),
            2 => self.begin_browse_measurement(),
            3 => self.begin_squig_browse(),
            4 => self.run_autoeq(),
            5 => self.reference.show_measurement = !self.reference.show_measurement,
            6 => self.reference.normalized = !self.reference.normalized,
            7 => self.reference.show_bounds = !self.reference.show_bounds,
            12 => {
                // Reset the customizer to a flat (no-adjustment) target.
                self.reference.adj_tilt = 0.0;
                self.reference.adj_bass = 0.0;
                self.reference.adj_ear = 0.0;
                self.reference.adj_treble = 0.0;
                self.reference.rebuild_target();
            }
            _ => {}
        }
    }

    /// Adjust the value under the settings cursor by a direction (`±1`). Only the
    /// Reference tab's customizer rows respond; `+`/`-` are no-ops elsewhere.
    pub fn settings_adjust(&mut self, dir: f64) {
        let (tab, cursor) = match &self.mode {
            InputMode::Settings(s) => (s.tab, s.cursor),
            _ => return,
        };
        if tab != 5 {
            return;
        }
        let r = &mut self.reference;
        match cursor {
            // Tilt is gentle (dB/oct), the gain bands coarser (dB). Ranges mirror
            // the GUI customizer sliders. (Rows 8–11; 3=Browse online shifted them.)
            8 => r.adj_tilt = (r.adj_tilt + dir * 0.1).clamp(-2.0, 1.0),
            9 => r.adj_bass = (r.adj_bass + dir * 0.5).clamp(-12.0, 18.0),
            10 => r.adj_ear = (r.adj_ear + dir * 0.5).clamp(-12.0, 12.0),
            11 => r.adj_treble = (r.adj_treble + dir * 0.5).clamp(-12.0, 12.0),
            _ => return,
        }
        self.reference.rebuild_target();
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

    /// Tray tab actions: start/stop, toggle autostart, toggle close-to-tray,
    /// cycle left-click, edit poll/recent (numeric — opens a `TextInput`).
    /// Full parity with the GUI settings dialog's Tray section and the CLI's
    /// `resonance tray` / `resonance tray config` subcommands.
    fn settings_tray_activate(&mut self) {
        use crate::settings::{TextInput, TextPurpose};
        use resonance_ipc::tray::{LeftClick, TrayConfig, autostart, control};
        let cursor = match &self.mode {
            InputMode::Settings(s) => s.cursor,
            _ => return,
        };
        match cursor {
            0 => {
                let r = if control::is_running() {
                    control::stop().map(|_| ())
                } else {
                    control::start()
                };
                self.tray_action("tray", r);
            }
            1 => {
                let r = if autostart::is_enabled() {
                    autostart::disable()
                } else {
                    autostart::enable()
                };
                self.tray_action("tray autostart", r);
            }
            2 => {
                let mut cfg = TrayConfig::load();
                cfg.close_gui_to_tray = !cfg.close_gui_to_tray;
                let r = cfg.save();
                self.tray_action("close-to-tray", r);
            }
            3 => {
                let mut cfg = TrayConfig::load();
                cfg.left_click = match cfg.left_click {
                    LeftClick::ToggleUi => LeftClick::Menu,
                    LeftClick::Menu => LeftClick::ToggleUi,
                };
                let r = cfg.save();
                self.tray_action("left-click", r);
            }
            4 => {
                let v = TrayConfig::load().poll_secs.to_string();
                if let InputMode::Settings(s) = &mut self.mode {
                    s.text_input = Some(TextInput::new(
                        v,
                        TextPurpose::TrayPoll,
                        "Poll seconds (1-30)",
                    ));
                }
            }
            5 => {
                let v = TrayConfig::load().recent_count.to_string();
                if let InputMode::Settings(s) = &mut self.mode {
                    s.text_input = Some(TextInput::new(
                        v,
                        TextPurpose::TrayRecent,
                        "Recent presets (0-20)",
                    ));
                }
            }
            _ => {}
        }
    }

    /// Run a tray control/config action and surface the result in the status
    /// line (mirrors `daemon_action`, minus the daemon-connect follow-up which
    /// doesn't apply to the tray process).
    fn tray_action(&mut self, label: &str, r: std::io::Result<()>) {
        let msg = match r {
            Ok(()) => format!("{label}: ok"),
            Err(e) => format!("{label}: failed: {e}"),
        };
        self.set_status(msg);
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
            5 => {
                self.prefs.show_spectrum = !self.prefs.show_spectrum;
                self.prefs.save();
            }
            6 => {
                self.prefs.show_slope = !self.prefs.show_slope;
                self.prefs.save();
            }
            7 => {
                self.prefs.show_scope = !self.prefs.show_scope;
                self.prefs.save();
            }
            8 => {
                self.prefs.show_dynamics = !self.prefs.show_dynamics;
                self.prefs.save();
            }
            9 => {
                self.prefs.show_dither = !self.prefs.show_dither;
                self.prefs.save();
            }
            10 => {
                self.prefs.show_ir = !self.prefs.show_ir;
                self.prefs.save();
            }
            11 => {
                self.prefs.show_channels = !self.prefs.show_channels;
                self.prefs.save();
            }
            // Swap L/R lives here too (parity with the GUI's relocated channel
            // controls) so it's reachable even when the channels column is hidden.
            12 => self.toggle_swap_lr(),
            // Chain-level EQ phase mode (like power/dither — not an advanced pref).
            13 => self.toggle_phase_mode(),
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
                    (InputMode::Settings(s), Some(state)) => {
                        s.mappings.get(s.cursor).is_some_and(|(out, _)| {
                            state.active_output.as_deref() == Some(out.as_str())
                        })
                    }
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
                    self.set_status("can only unmap the active output");
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
        let Some(Response::PresetList(profiles)) = self.query(Command::ListProfiles) else {
            return;
        };
        if profiles.is_empty() {
            self.set_status("no profiles saved");
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

/// Narrow labels for the TUI effects column (kept local: "Dyn Boost" fits where
/// the shared `FxEffectId::label()` "Dynamic Boost" would not). One entry per
/// `FxEffectId::ALL` variant, in chain order — the array length drives the
/// effects column's row count, so a new effect appears here and renders.
pub const EFFECT_NAMES: [&str; FxEffectId::ALL.len()] = [
    "Fidelity",
    "Ambience",
    "Surround",
    "Dyn Boost",
    "Bass",
    "Loudness",
    "Crossfeed",
];

pub fn fx_effect_at(idx: usize) -> FxEffectId {
    FxEffectId::ALL[idx.min(FxEffectId::ALL.len() - 1)]
}

/// Minimum intensity for an effect: Surround and Bass are bipolar (−1), others 0.
pub fn fx_min(idx: usize) -> f64 {
    fx_effect_at(idx).min()
}

pub fn fx_intensity(state: &DaemonState, idx: usize) -> f64 {
    state.effects.get(fx_effect_at(idx)).0
}

pub fn fx_enabled(state: &DaemonState, idx: usize) -> bool {
    state.effects.get(fx_effect_at(idx)).1
}

// ── squig.link catalog filtering (shared by the browser actions + renderer) ──

/// Models whose display name or source contains the (case-insensitive) query.
pub(crate) fn squig_filter_models<'a>(catalog: &'a Catalog, query: &str) -> Vec<&'a ModelEntry> {
    let q = query.to_lowercase();
    catalog
        .models
        .iter()
        .filter(|m| {
            q.is_empty()
                || m.display.to_lowercase().contains(&q)
                || m.source.to_lowercase().contains(&q)
        })
        .collect()
}

/// Target curves whose name or source contains the (case-insensitive) query.
pub(crate) fn squig_filter_targets<'a>(catalog: &'a Catalog, query: &str) -> Vec<&'a TargetEntry> {
    let q = query.to_lowercase();
    catalog
        .targets
        .iter()
        .filter(|t| {
            q.is_empty()
                || t.name.to_lowercase().contains(&q)
                || t.source.to_lowercase().contains(&q)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn channels_visible_rules() {
        // Mono never shows channel controls.
        assert!(!super::channels_visible(true, 1));
        // Stereo: only when opted in.
        assert!(!super::channels_visible(false, 2));
        assert!(super::channels_visible(true, 2));
        // >2ch always shows (auto-disclosure), regardless of the pref.
        assert!(super::channels_visible(false, 6));
    }

    #[test]
    fn advanced_hint_label_lists_active() {
        assert_eq!(
            super::advanced_hint_label(false, false, false, false, false, false),
            None
        );
        assert_eq!(
            super::advanced_hint_label(true, false, false, true, false, false).as_deref(),
            Some("adv: dither scope")
        );
        assert_eq!(
            super::advanced_hint_label(false, false, false, false, true, false).as_deref(),
            Some("adv: dyn")
        );
        assert_eq!(
            super::advanced_hint_label(true, true, true, true, true, true).as_deref(),
            Some("adv: dither ir slope scope dyn channels")
        );
    }

    #[test]
    fn dyn_field_adjust_steps_and_clamps() {
        use resonance_ipc::BandDynamics;
        let mut d = BandDynamics::DEFAULT;
        // Each row steps at its own scale…
        super::dyn_field_adjust(&mut d, 0, -5.0);
        assert!((d.threshold_db - -35.0).abs() < 1e-9);
        super::dyn_field_adjust(&mut d, 1, 2.0);
        assert!((d.range_db - -5.0).abs() < 1e-9);
        super::dyn_field_adjust(&mut d, 2, 1.0);
        assert!((d.attack_ms - 6.0).abs() < 1e-9);
        super::dyn_field_adjust(&mut d, 3, 5.0);
        assert!((d.release_ms - 200.0).abs() < 1e-9);
        // …and clamps to the ranges the daemon accepts.
        super::dyn_field_adjust(&mut d, 0, -1000.0);
        assert!((d.threshold_db - -80.0).abs() < 1e-9);
        super::dyn_field_adjust(&mut d, 1, 1000.0);
        assert!((d.range_db - 24.0).abs() < 1e-9);
        super::dyn_field_adjust(&mut d, 2, -1000.0);
        assert!((d.attack_ms - 0.1).abs() < 1e-9);
        super::dyn_field_adjust(&mut d, 3, 1000.0);
        assert!((d.release_ms - 5000.0).abs() < 1e-9);
    }
}
