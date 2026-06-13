//! egui/eframe front-end for the Resonance daemon.
//!
//! All daemon mutations are expressed as `Command`s collected into `pending`
//! during the frame, then dispatched synchronously after the UI is built. The
//! authoritative `DaemonState` is re-fetched immediately afterwards (and on a
//! periodic poll) so widgets always reflect the daemon.

use crate::browser::{Browser, Item};
use crate::curve;
use crate::ipc::IpcClient;
use crate::theme::{Palette, Theme};
use eframe::egui;
use resonance_ipc::{
    BandState, BandType, Command, DaemonState, EffectsState, FxEffectId, Response, service,
    transport::TransportError,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

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
/// Matugen colour-file mtime poll: snappy so the GUI recolours nearly as fast
/// as other matugen-aware apps when the wallpaper/theme changes.
const MATUGEN_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Width below which the toolbar drops from one row to the designed two-row
/// layout. Above this both control groups fit comfortably side by side.
const TOOLBAR_ONE_ROW_MIN: f32 = 1320.0;

/// Two-row toolbar column grid. Each column is a fixed width so its contents
/// centre inside it and the separators between columns form continuous
/// full-height dividers. `TB_ON_W` spans both rows; the rest stack two cells of
/// `TB_ROW_H`.
const TB_ON_W: f32 = 80.0; // power button (spans both rows)
const TB_UNDO_W: f32 = 60.0; // undo / redo stacked
const TB_MID_W: f32 = 290.0; // preamp / daemon+theme+settings (max, elastic)
const TB_MID_MIN: f32 = 150.0; // mid column floor before meters drop
const TB_AUX_W: f32 = 132.0; // load+export / reset
const TB_TAIL_W: f32 = 285.0; // output / meters (right-pushed)
const TB_EDGE_PAD: f32 = 8.0; // gap between window edge and first toolbar cell
const TB_ROW_H: f32 = 26.0;
const TB_FULL_H: f32 = TB_ROW_H * 2.0;

/// Default widths of the Effects / Devices side columns; EQ bands (central)
/// takes whatever's left so its 8-column table is never the one that squishes.
/// Default / fallback widths of the Effects & Devices side panels (used as
/// `default_size` and when no resized width is stored yet). The tab-vs-columns
/// decision is measured at runtime, not derived from these.
const EFFECTS_W: f32 = 300.0;
const DEVICES_W: f32 = 420.0;
/// Fallback natural width of the EQ bands table before it's been measured —
/// keeps the first frame in column layout at the default window size.
const DEFAULT_BANDS_W: f32 = 500.0;

/// Spectrum envelope time constants: bars snap up, glide down.
const SPECTRUM_ATTACK_TAU: f32 = 0.020;
const SPECTRUM_DECAY_TAU: f32 = 0.20;

/// Editable per-band limits. Generous so extreme cuts/boosts and very narrow
/// notches are possible; the daemon/DSP impose no limit of their own.
const GAIN_LIMIT: f64 = 40.0;
const Q_LIMIT: f64 = 100.0;

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
    /// Export the current chain: a directory navigator plus a filename field.
    ExportProfile(SaveDialog),
}

/// State for the Export (save-as) dialog: where to write and under what name.
struct SaveDialog {
    browser: Browser,
    /// Filename stem the user is typing (the `.toml` suffix is implicit).
    filename: String,
}

impl SaveDialog {
    /// Full destination path: `<cwd>/<filename>.toml`.
    fn target(&self) -> std::path::PathBuf {
        let name = self.filename.trim();
        let name = name.strip_suffix(".toml").unwrap_or(name);
        self.browser.cwd.join(format!("{name}.toml"))
    }
}

/// A pending destructive/overwriting profile action awaiting confirmation.
#[derive(Clone)]
enum Confirm {
    /// Overwrite an existing profile of this name with the current chain.
    SaveProfile(String),
    /// Delete this profile.
    DeleteProfile(String),
}

/// A zero-arg systemd service action (start/stop/restart/…).
type ServiceFn = fn() -> std::io::Result<()>;

/// One unit of work for the service worker thread.
enum ServiceAction {
    /// Re-read installed/active/enabled status from the platform manager.
    RefreshStatus,
    /// Run a lifecycle op (start/stop/restart/enable/disable). The static
    /// `label` is shown in the toolbar status when the result comes back.
    Run { label: &'static str, f: ServiceFn },
}

/// Worker → UI message. Carries an updated Status snapshot (always) plus
/// optional toolbar feedback (only for Run results).
struct ServiceWorkerResult {
    status: service::Status,
    feedback: Option<String>,
}

/// Spawn the service worker thread. It serialises `service::start/stop/
/// status/...` calls so the UI thread never blocks on launchctl.
fn spawn_service_worker(
    rx: std::sync::mpsc::Receiver<ServiceAction>,
    tx: std::sync::mpsc::Sender<ServiceWorkerResult>,
    ctx: egui::Context,
) {
    std::thread::Builder::new()
        .name("resonance-service".into())
        .spawn(move || {
            while let Ok(action) = rx.recv() {
                let feedback = match action {
                    ServiceAction::RefreshStatus => None,
                    ServiceAction::Run { label, f } => Some(match f() {
                        Ok(()) => format!("{label} ok"),
                        Err(e) => format!("{label} failed: {e}"),
                    }),
                };
                let status = service::status();
                if tx.send(ServiceWorkerResult { status, feedback }).is_err() {
                    break;
                }
                // Wake the UI so it consumes the result on the next frame
                // (egui sleeps between frames when idle; without this the
                // updated daemon status would only appear on the next mouse
                // move / keystroke).
                ctx.request_repaint();
            }
        })
        .expect("spawn service worker");
}

/// Transient toolbar status feedback (e.g. "layout reset", "undo") clears after
/// this long so it stops reading like a permanent label.
const STATUS_TTL: Duration = Duration::from_secs(4);

/// How the window is decorated.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TitlebarMode {
    /// Custom client-side titlebar on stacking WMs; native decorations on
    /// tiling WMs (where window buttons / a drag bar are pointless).
    Auto,
    /// Always our custom client-side titlebar with window controls.
    Custom,
    /// Always the OS window decorations (no custom bar).
    Native,
}

impl TitlebarMode {
    const ALL: [TitlebarMode; 3] = [
        TitlebarMode::Auto,
        TitlebarMode::Custom,
        TitlebarMode::Native,
    ];

    fn label(self) -> &'static str {
        match self {
            TitlebarMode::Auto => "Auto",
            TitlebarMode::Custom => "Custom",
            TitlebarMode::Native => "Native",
        }
    }

    fn from_label(s: &str) -> TitlebarMode {
        Self::ALL
            .into_iter()
            .find(|m| m.label() == s)
            .unwrap_or(TitlebarMode::Auto)
    }

    /// Whether to draw our custom titlebar (and hide native decorations).
    fn use_csd(self) -> bool {
        match self {
            TitlebarMode::Custom => true,
            TitlebarMode::Native => false,
            TitlebarMode::Auto => !is_tiling_wm(),
        }
    }
}

/// Heuristic: are we on a tiling WM where a client-side title bar and window
/// buttons add nothing (the compositor owns geometry)? Checks well-known tiling
/// compositors via their signature env vars and the desktop name.
fn is_tiling_wm() -> bool {
    use std::env::var;
    if var("HYPRLAND_INSTANCE_SIGNATURE").is_ok()
        || var("SWAYSOCK").is_ok()
        || var("I3SOCK").is_ok()
    {
        return true;
    }
    const TILING: [&str; 10] = [
        "i3",
        "sway",
        "hyprland",
        "river",
        "bspwm",
        "dwm",
        "qtile",
        "awesome",
        "xmonad",
        "herbstluft",
    ];
    [
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "DESKTOP_SESSION",
    ]
    .iter()
    .filter_map(|k| var(k).ok())
    .any(|v| {
        let v = v.to_ascii_lowercase();
        TILING.iter().any(|n| v.contains(n))
    })
}

/// Which of the three lower sections is visible when the window is too narrow
/// for side-by-side columns and they collapse into a single tabbed pane.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum LowerTab {
    Effects,
    #[default]
    Bands,
    Mapping,
    Profiles,
}

pub struct GuiApp {
    /// Channel to the IPC worker thread — the UI thread never does IPC itself,
    /// so a stopped/restarting daemon can't block or freeze the window.
    cmd_tx: std::sync::mpsc::Sender<WorkerCmd>,
    /// Latest snapshot the IPC worker published (copied into fields each frame).
    shared: Arc<Mutex<GuiShared>>,
    state: Option<DaemonState>,
    profiles: Vec<String>,
    mappings: Vec<(String, String)>,
    status: String,
    needs_meta: bool,
    dialog: Dialog,
    /// Pending profile save-overwrite / delete awaiting a yes/no modal.
    confirm: Option<Confirm>,
    selected_band: usize,
    drag_band: Option<usize>,
    /// Optimistic (freq, gain) of the band being dragged, so its marker tracks
    /// the cursor exactly instead of the IPC-lagged echoed state.
    drag_value: Option<(f64, f64)>,
    /// True while the active curve drag edits Q (right button) vs freq+gain.
    drag_q: bool,
    profile_name: String,
    /// Inline profile rename in progress: (original name, edit buffer).
    rename: Option<(String, String)>,
    /// Smoothed spectrum bar heights + last animation tick.
    spectrum_display: Vec<f32>,
    last_anim: Instant,
    /// Animated FR dB-axis half-range — eased toward the target so the axis
    /// grows/shrinks smoothly instead of snapping between stops (no flicker).
    db_axis: f64,
    /// Hysteretic target half-range (the chosen ± dB stop) + its grid step. Held
    /// across frames with a deadband so the stop choice doesn't chatter at a
    /// boundary; `db_axis` eases toward `db_target`.
    db_target: f64,
    db_step: f64,
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
    /// Band pinned to gain (freq+Q only, gain locked) via shift+double-right-
    /// click. Mutually exclusive with `vlock`.
    hlock: Option<usize>,
    /// Visible x-axis range as (log10 freq lo, hi). Full span when not zoomed.
    view_log: (f64, f64),
    /// Start of an in-progress shift-drag zoom selection (log10 freq).
    zoom_sel: Option<f64>,
    /// Keybind / gesture help overlay visible.
    show_help: bool,
    /// Cached systemd user-service status; refreshed on a slow timer.
    daemon_status: service::Status,
    last_service_poll: Instant,
    /// Channel into the service worker. We send `ServiceAction` requests
    /// off the UI thread because `launchctl` calls + the daemon's
    /// CoreAudio teardown can take 200–800 ms — synchronous on the UI
    /// thread froze egui visibly when the user clicked Start/Stop.
    service_tx: std::sync::mpsc::Sender<ServiceAction>,
    /// Worker result channel. Each tick we drain it and apply updates
    /// (status text + cached Status).
    service_rx: std::sync::mpsc::Receiver<ServiceWorkerResult>,
    /// True while a Start/Stop/Restart is in flight — we grey-out the
    /// menu so the user can't fire overlapping ops.
    service_busy: bool,
    /// When the current `status` text should auto-clear (transient feedback).
    status_until: Option<Instant>,
    /// Window-decoration mode + the last decoration value pushed to the
    /// viewport (so we only send the command when it changes).
    titlebar_mode: TitlebarMode,
    decorations_applied: Option<bool>,
    /// Matugen colour-file mtime + last poll, for live theme reload.
    matugen_mtime: Option<SystemTime>,
    last_matugen_check: Instant,
    /// Selected section in narrow (tabbed) layout.
    lower_tab: LowerTab,
    /// Measured width the two-row toolbar actually needs this frame (left
    /// columns + trailing group). Drives a dynamic `MinInnerSize` so the window
    /// can't shrink to where the toolbar clips — no hardcoded min.
    tb_required_w: f32,
    /// Last min-inner width pushed to the viewport (only resend on change).
    min_applied: Option<f32>,
}

/// Messages from the UI thread to the IPC worker.
enum WorkerCmd {
    Cmd(Command),
    RefreshMeta,
    Import(String),
    Export(String),
}

/// Snapshot the IPC worker publishes for the UI thread to read each frame.
#[derive(Default)]
struct GuiShared {
    state: Option<DaemonState>,
    profiles: Vec<String>,
    mappings: Vec<(String, String)>,
    /// Transient feedback from worker-side actions (import/export/errors).
    status: Option<String>,
}

fn worker_status(shared: &Arc<Mutex<GuiShared>>, msg: String) {
    if let Ok(mut s) = shared.lock() {
        s.status = Some(msg);
    }
}

/// IPC worker thread. Owns the daemon connection and performs ALL IPC —
/// connect, poll, commands, meta — so the egui UI thread never blocks. A
/// stopped or restarting daemon therefore can't freeze the window: blocking
/// `connect()`/reads happen here, off the UI thread.
fn spawn_ipc_worker(
    rx: std::sync::mpsc::Receiver<WorkerCmd>,
    shared: Arc<Mutex<GuiShared>>,
    ctx: egui::Context,
) {
    std::thread::Builder::new()
        .name("resonance-ipc".into())
        .spawn(move || {
            let mut ipc: Option<IpcClient> = None;
            let mut last_meta = Instant::now() - META_INTERVAL;
            let mut refresh_meta_now = true;
            loop {
                if ipc.is_none() {
                    match crate::ipc::connect() {
                        Ok(c) => {
                            ipc = Some(c);
                            refresh_meta_now = true;
                        }
                        Err(_) => {
                            if let Ok(mut s) = shared.lock() {
                                s.state = None;
                            }
                            ctx.request_repaint();
                            std::thread::sleep(RECONNECT_INTERVAL);
                            continue;
                        }
                    }
                }

                // Apply queued UI commands.
                loop {
                    let msg = match rx.try_recv() {
                        Ok(m) => m,
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                    };
                    let Some(c) = ipc.as_mut() else { break };
                    match msg {
                        WorkerCmd::Cmd(cmd) => {
                            // A daemon-level rejection (Response::Error) is not a
                            // dead connection — surface it and keep the socket.
                            // Only a transport failure tears it down to reconnect.
                            match c.send(cmd) {
                                Ok(()) => {}
                                Err(TransportError::Daemon(msg)) => worker_status(&shared, msg),
                                Err(_) => {
                                    ipc = None;
                                    break;
                                }
                            }
                        }
                        WorkerCmd::RefreshMeta => refresh_meta_now = true,
                        WorkerCmd::Import(path) => {
                            match c.send_recv(Command::ImportPreset { path, name: None }) {
                                Ok(Response::Imported(name)) => {
                                    let _ = c.send(Command::LoadProfile { name: name.clone() });
                                    refresh_meta_now = true;
                                    worker_status(&shared, format!("imported + loaded '{name}'"));
                                }
                                Ok(Response::Error(e)) => {
                                    worker_status(&shared, format!("import failed: {e}"))
                                }
                                Ok(_) => worker_status(&shared, "import failed".into()),
                                Err(_) => {
                                    ipc = None;
                                    break;
                                }
                            }
                        }
                        WorkerCmd::Export(path) => {
                            match c.send_recv(Command::ExportProfile { path: path.clone() }) {
                                Ok(Response::Ok) => {
                                    worker_status(&shared, format!("exported → {path}"))
                                }
                                Ok(Response::Error(e)) => {
                                    worker_status(&shared, format!("export failed: {e}"))
                                }
                                Ok(_) => worker_status(&shared, "export failed".into()),
                                Err(_) => {
                                    ipc = None;
                                    break;
                                }
                            }
                        }
                    }
                }

                // Poll state.
                if let Some(c) = ipc.as_mut() {
                    match c.get_state() {
                        Ok(st) => {
                            if let Ok(mut s) = shared.lock() {
                                s.state = Some(st);
                            }
                            ctx.request_repaint();
                        }
                        Err(_) => {
                            ipc = None;
                            if let Ok(mut s) = shared.lock() {
                                s.state = None;
                            }
                            ctx.request_repaint();
                        }
                    }
                }

                // Meta (profiles + mappings) on a slow timer / on request.
                if let Some(c) = ipc.as_mut()
                    && (refresh_meta_now || last_meta.elapsed() >= META_INTERVAL)
                {
                    if let Ok(Response::PresetList(p)) = c.send_recv(Command::ListProfiles)
                        && let Ok(mut s) = shared.lock()
                    {
                        s.profiles = p;
                    }
                    if let Ok(Response::Mappings(m)) = c.send_recv(Command::ListMappings)
                        && let Ok(mut s) = shared.lock()
                    {
                        s.mappings = m;
                    }
                    last_meta = Instant::now();
                    refresh_meta_now = false;
                }

                std::thread::sleep(STATE_INTERVAL);
            }
        })
        .expect("spawn ipc worker");
}

impl GuiApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_symbol_fonts(&cc.egui_ctx);
        let (service_tx, service_worker_rx) = std::sync::mpsc::channel::<ServiceAction>();
        let (service_worker_tx, service_rx) = std::sync::mpsc::channel::<ServiceWorkerResult>();
        spawn_service_worker(service_worker_rx, service_worker_tx, cc.egui_ctx.clone());
        // Kick off an initial status fetch so the toolbar doesn't show
        // "stopped" for the first 1.5 s while the poll timer warms up.
        let _ = service_tx.send(ServiceAction::RefreshStatus);
        // Restore the persisted theme (panel sizes restore automatically via
        // eframe's egui-memory persistence).
        let theme = cc
            .storage
            .and_then(|s| s.get_string("theme"))
            .map(|s| Theme::from_label(&s))
            .unwrap_or(Theme::System);
        cc.egui_ctx.set_visuals(theme.visuals());
        // Compact, consistent button sizing so controls stay usable when the
        // window is narrow (set once; set_visuals doesn't touch spacing).
        cc.egui_ctx.global_style_mut(|s| {
            s.spacing.button_padding = egui::vec2(8.0, 3.0);
            s.spacing.interact_size.y = 22.0;
        });
        let titlebar_mode = cc
            .storage
            .and_then(|s| s.get_string("titlebar"))
            .map(|s| TitlebarMode::from_label(&s))
            .unwrap_or(TitlebarMode::Auto);
        let (cmd_tx, ipc_rx) = std::sync::mpsc::channel::<WorkerCmd>();
        let shared = Arc::new(Mutex::new(GuiShared::default()));
        spawn_ipc_worker(ipc_rx, shared.clone(), cc.egui_ctx.clone());

        Self {
            cmd_tx,
            shared,
            state: None,
            profiles: Vec::new(),
            mappings: Vec::new(),
            status: String::new(),
            needs_meta: false,
            dialog: Dialog::None,
            confirm: None,
            selected_band: 0,
            drag_band: None,
            drag_value: None,
            drag_q: false,
            profile_name: String::new(),
            rename: None,
            spectrum_display: Vec::new(),
            last_anim: Instant::now(),
            db_axis: curve::DB_RANGE,
            db_target: curve::DB_RANGE,
            db_step: 6.0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: None,
            clip_until: None,
            theme,
            palette: theme.palette(),
            vlock: None,
            hlock: None,
            view_log: (curve::LOG_MIN, curve::LOG_MAX),
            zoom_sel: None,
            show_help: false,
            daemon_status: service::Status::default(),
            last_service_poll: Instant::now(),
            service_tx,
            service_rx,
            service_busy: false,
            status_until: None,
            titlebar_mode,
            decorations_applied: None,
            matugen_mtime: crate::theme::matugen_source_mtime(),
            last_matugen_check: Instant::now(),
            lower_tab: LowerTab::default(),
            tb_required_w: 0.0,
            min_applied: None,
        }
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
            self.set_status("undo");
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo_stack.pop() {
            if let Some(cur) = self.snapshot() {
                self.undo_stack.push(cur);
            }
            self.apply_snapshot(&next);
            self.last_edit = None;
            self.set_status("redo");
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

    /// Copy the IPC worker's latest snapshot into our fields (called once per
    /// frame). The UI thread never does IPC, so this never blocks.
    fn pull_shared(&mut self) {
        let (state, profiles, mappings, status) = {
            let mut s = self.shared.lock().unwrap();
            (
                s.state.clone(),
                s.profiles.clone(),
                s.mappings.clone(),
                s.status.take(),
            )
        };
        if let Some(st) = &state
            && st.meters.clip
        {
            self.clip_until = Some(Instant::now() + Duration::from_millis(250));
        }
        self.state = state;
        self.profiles = profiles;
        self.mappings = mappings;
        // Drop lock pins that no longer name a valid band — a profile load or
        // another client can shrink the band list, after which a stale pin would
        // silently apply to a different band.
        let bands = self.state.as_ref().map(|s| s.bands.len()).unwrap_or(0);
        self.vlock = self.vlock.filter(|&i| i < bands);
        self.hlock = self.hlock.filter(|&i| i < bands);
        if let Some(msg) = status {
            self.set_status(msg);
        }
        if self.needs_meta {
            self.needs_meta = false;
            let _ = self.cmd_tx.send(WorkerCmd::RefreshMeta);
        }
    }

    fn queue(&mut self, cmd: Command) {
        let _ = self.cmd_tx.send(WorkerCmd::Cmd(cmd));
    }

    /// Set the toolbar status text with an auto-clear timer so transient action
    /// feedback doesn't linger like a permanent label.
    fn set_status(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_until = Some(Instant::now() + STATUS_TTL);
    }

    /// Import a preset file as a profile (our own format), then load that
    /// profile — mirrors the TUI flow so presets are always captured, not just
    /// applied transiently.
    fn import_and_load(&mut self, path: String) {
        let _ = self.cmd_tx.send(WorkerCmd::Import(path));
    }
}

impl eframe::App for GuiApp {
    /// Persist the chosen theme. Panel sizes are saved by eframe's egui-memory
    /// persistence (enabled by the `persistence` feature + default
    /// `persist_egui_memory`).
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("theme", self.theme.label().to_string());
        storage.set_string("titlebar", self.titlebar_mode.label().to_string());
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Read the latest snapshot the IPC worker published (never blocks).
        self.pull_shared();

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

        // F1 or `?` toggles the keybind/gesture help overlay; Esc closes it.
        ui.ctx().input(|i| {
            if i.key_pressed(egui::Key::F1)
                || (i.key_pressed(egui::Key::Questionmark) && !i.modifiers.command)
            {
                self.show_help = !self.show_help;
            }
            if i.key_pressed(egui::Key::Escape) {
                self.show_help = false;
            }
        });

        // Drain any results the service worker thread has produced. Each
        // result carries a fresh Status snapshot and (for Run requests)
        // a feedback string for the toolbar status line.
        while let Ok(res) = self.service_rx.try_recv() {
            self.daemon_status = res.status;
            if let Some(msg) = res.feedback {
                self.set_status(msg);
            }
            // Receiving a result always clears the "busy" gate; further
            // clicks can fire again.
            self.service_busy = false;
        }
        // Service status drives the toolbar daemon controls; poll it on a
        // slow timer via the worker (off the UI thread so launchctl
        // latency never freezes egui).
        if self.last_service_poll.elapsed() >= Duration::from_millis(1500) {
            self.last_service_poll = Instant::now();
            let _ = self.service_tx.send(ServiceAction::RefreshStatus);
        }

        // Expire transient status feedback so it stops reading like a label.
        if let Some(t) = self.status_until {
            if Instant::now() >= t {
                self.status.clear();
                self.status_until = None;
            }
        }

        // Live-reload the Matugen theme when its colour file changes on disk.
        if self.theme == Theme::Matugen
            && self.last_matugen_check.elapsed() >= MATUGEN_POLL_INTERVAL
        {
            self.last_matugen_check = Instant::now();
            let mtime = crate::theme::matugen_source_mtime();
            if mtime != self.matugen_mtime {
                self.matugen_mtime = mtime;
                let ctx = ui.ctx().clone();
                self.set_theme(&ctx, Theme::Matugen);
            }
        }

        // Apply the window-decoration mode (custom titlebar ⇒ hide native
        // decorations). Only send the viewport command when the value changes.
        let csd = self.titlebar_mode.use_csd();
        if self.decorations_applied != Some(!csd) {
            self.decorations_applied = Some(!csd);
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Decorations(!csd));
        }
        if csd {
            egui::Panel::top("titlebar").show_inside(ui, |ui| self.titlebar(ui));
        }

        egui::Panel::top("toolbar").show_inside(ui, |ui| self.toolbar(ui));

        if self.state.is_none() {
            egui::CentralPanel::default().show_inside(ui, |ui| self.disconnected(ui));
        } else {
            // Layout: FR graph (resizable top) + spectrum (resizable bottom)
            // span the full width; below them three resizable columns —
            // Effects │ EQ bands │ Devices/Profiles.
            let state = self.state.clone();
            // Default sizes are proportional to the current window so first
            // launch / Reset layout gives the same shape at any size: FR ~40%
            // height, spectrum ~18%, and the three columns split the width into
            // equal thirds. (default_size only applies when no size is stored.)
            let fr_h = (ui.available_height() * 0.50).max(70.0);
            let spec_h = (ui.available_height() * 0.18).max(28.0);
            egui::Panel::top("fr")
                .resizable(true)
                .default_size(fr_h)
                .min_size(70.0)
                .show_inside(ui, |ui| {
                    if let Some(s) = &state {
                        self.eq_curve(ui, s);
                    }
                });
            egui::Panel::bottom("spectrum")
                .resizable(true)
                .default_size(spec_h)
                .min_size(28.0)
                .show_inside(ui, |ui| {
                    if let Some(s) = &state {
                        self.spectrum(ui, s);
                    }
                });

            // The three lower sections share the band between FR and spectrum.
            // The side panels (Effects, Devices/Profiles) are *fixed* at their
            // content width and not manually resizable; EQ bands (central) takes
            // all the remaining width. We stay in this 3-column layout only while
            // the bands table fits at its natural width — the instant it would be
            // clipped we collapse into the tabbed pane. Re-evaluated every frame,
            // so it reflows live as the window is resized.
            //
            // `bands_w` is the table's natural width. `centered` stores it as a
            // per-frame *temp* value (0 on a fresh launch); persist it so the
            // column/tabbed decision is correct immediately, with a seeded
            // default for the very first frame.
            let bands_persist = egui::Id::new("bands_natural_w");
            let bands_temp = ui
                .ctx()
                .data(|d| d.get_temp::<f32>(egui::Id::new(("centered", "bands_body"))));
            if let Some(w) = bands_temp.filter(|w| *w > 1.0) {
                ui.ctx().data_mut(|d| d.insert_persisted(bands_persist, w));
            }
            let bands_w = bands_temp
                .filter(|w| *w > 1.0)
                .or_else(|| ui.ctx().data_mut(|d| d.get_persisted::<f32>(bands_persist)))
                .unwrap_or(DEFAULT_BANDS_W);
            // +36 covers the two panel separators and scroll inner margins.
            let cols_fit = ui.available_width() >= EFFECTS_W + DEVICES_W + bands_w + 36.0;
            if cols_fit {
                // Fixed-width side panels (no manual resize); EQ bands central
                // takes the rest so it never squishes below its 8-column table.
                egui::Panel::left("fx_pane")
                    .resizable(false)
                    .default_size(EFFECTS_W)
                    .show_inside(ui, |ui| {
                        if let Some(s) = &state {
                            padded_scroll(ui, "effects_scroll", |ui| self.effects_section(ui, s));
                        }
                    });
                egui::Panel::right("dev_pane")
                    .resizable(false)
                    .default_size(DEVICES_W)
                    .show_inside(ui, |ui| {
                        padded_scroll(ui, "side", |ui| self.devices_profiles(ui));
                    });
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    if let Some(s) = &state {
                        padded_scroll(ui, "bands_scroll", |ui| self.bands_section(ui, s));
                    }
                });
            } else {
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    self.lower_tabs(ui, &state);
                });
            }
        }

        let ctx = ui.ctx().clone();

        // Dynamic minimum window width: the two-row toolbar's measured required
        // width (+ panel frame margin). The window then physically can't shrink
        // to where the toolbar clips — adapts to font, scale and device-name
        // length instead of a hardcoded constant. Floor until first measured.
        let min_w = if self.tb_required_w > 1.0 {
            // + panel frame inner margins + a small safety gutter at the edge.
            (self.tb_required_w + 44.0).ceil()
        } else {
            600.0
        };
        if self
            .min_applied
            .map(|w| (w - min_w).abs() > 1.0)
            .unwrap_or(true)
        {
            self.min_applied = Some(min_w);
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(
                min_w, 460.0,
            )));
        }

        self.preset_dialog(&ctx);
        self.export_dialog(&ctx);
        self.confirm_dialog(&ctx);
        self.help_dialog(&ctx);

        // With native decorations off, the OS resize borders are gone — add our
        // own edge/corner grips so the window can still be resized.
        if csd {
            self.csd_resize_grips(&ctx);
        }

        // Drive ~144 fps repaint so spectrum/curve stay smooth.
        ctx.request_repaint_after(FRAME_INTERVAL);
    }
}

impl GuiApp {
    /// Invisible resize grips around the window border (custom-titlebar mode):
    /// a drag on an edge/corner asks the backend to resize, replacing the native
    /// resize borders that `Decorations(false)` removes.
    fn csd_resize_grips(&self, ctx: &egui::Context) {
        use egui::{CursorIcon as Cur, ResizeDirection as Dir, Sense, ViewportCommand};
        // No resizing while maximized.
        if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
            return;
        }
        let r = ctx.content_rect();
        let b = 6.0; // grip thickness (px)
        let p = egui::pos2;
        let grips: [(&str, egui::Rect, Dir, Cur); 8] = [
            (
                "n",
                egui::Rect::from_min_max(p(r.left() + b, r.top()), p(r.right() - b, r.top() + b)),
                Dir::North,
                Cur::ResizeNorth,
            ),
            (
                "s",
                egui::Rect::from_min_max(
                    p(r.left() + b, r.bottom() - b),
                    p(r.right() - b, r.bottom()),
                ),
                Dir::South,
                Cur::ResizeSouth,
            ),
            (
                "w",
                egui::Rect::from_min_max(p(r.left(), r.top() + b), p(r.left() + b, r.bottom() - b)),
                Dir::West,
                Cur::ResizeWest,
            ),
            (
                "e",
                egui::Rect::from_min_max(
                    p(r.right() - b, r.top() + b),
                    p(r.right(), r.bottom() - b),
                ),
                Dir::East,
                Cur::ResizeEast,
            ),
            (
                "nw",
                egui::Rect::from_min_max(r.left_top(), p(r.left() + b, r.top() + b)),
                Dir::NorthWest,
                Cur::ResizeNorthWest,
            ),
            (
                "ne",
                egui::Rect::from_min_max(p(r.right() - b, r.top()), p(r.right(), r.top() + b)),
                Dir::NorthEast,
                Cur::ResizeNorthEast,
            ),
            (
                "sw",
                egui::Rect::from_min_max(p(r.left(), r.bottom() - b), p(r.left() + b, r.bottom())),
                Dir::SouthWest,
                Cur::ResizeSouthWest,
            ),
            (
                "se",
                egui::Rect::from_min_max(p(r.right() - b, r.bottom() - b), r.right_bottom()),
                Dir::SouthEast,
                Cur::ResizeSouthEast,
            ),
        ];
        for (name, rect, dir, cursor) in grips {
            egui::Area::new(egui::Id::new(("csd_grip", name)))
                .order(egui::Order::Foreground)
                .fixed_pos(rect.min)
                .interactable(true)
                .show(ctx, |ui| {
                    let resp = ui.allocate_rect(rect, Sense::drag());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(cursor);
                    }
                    if resp.drag_started() {
                        ui.ctx()
                            .send_viewport_cmd(ViewportCommand::BeginResize(dir));
                    }
                });
        }
    }
}

// ── UI sections ─────────────────────────────────────────────────────────────

impl GuiApp {
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let state = self.state.clone();
        let avail = ui.available_width();
        // One row when everything fits side by side; otherwise the designed
        // two-row column grid. The two-row grid is *elastic*: the preamp slider
        // and its column shrink, and the meters drop, as the window narrows, so
        // it keeps fitting (no clipping, no wrapping) down to a small floor — all
        // driven by measured widths, not hardcoded breakpoints.
        if avail >= TOOLBAR_ONE_ROW_MIN {
            ui.horizontal(|ui| {
                ui.add_space(TB_EDGE_PAD);
                self.tb_power(ui, &state);
                ui.separator();
                self.tb_preamp(ui, &state, 170.0);
                ui.separator();
                self.tb_load_export(ui, &state);
                ui.separator();
                self.tb_output(ui, &state);
                ui.separator();
                self.tb_history(ui);
                ui.separator();
                self.daemon_menu(ui);
                ui.separator();
                self.theme_menu(ui);
                self.settings_menu(ui);
                self.tb_reset(ui);
                ui.separator();
                if let Some(s) = &state {
                    self.meters_widget(ui, s);
                }
                self.tb_status(ui);
            });
            return;
        }

        // Last frame's measured tail widths: full = output+meters, out = output
        // only (used to decide whether meters still fit, and to size the spacer).
        let tail_full = ui
            .ctx()
            .data(|d| d.get_temp::<f32>(egui::Id::new("tb_tail_full")))
            .unwrap_or(TB_TAIL_W);
        let tail_out = ui
            .ctx()
            .data(|d| d.get_temp::<f32>(egui::Id::new("tb_tail_out")))
            .unwrap_or(120.0);
        // Non-elastic left width: ON + Undo/Redo + Load/Reset columns + the four
        // separators (+ a little slack). The mid (preamp/menus) column absorbs
        // the rest down to a floor; below that, the meters drop.
        let fixed_left = TB_EDGE_PAD + TB_ON_W + TB_UNDO_W + TB_AUX_W + 70.0;
        let show_meters = avail >= fixed_left + TB_MID_MIN + tail_full + 8.0;
        let tail_target = if show_meters { tail_full } else { tail_out };
        let mid_w = (avail - fixed_left - tail_target - 8.0).clamp(TB_MID_MIN, TB_MID_W);
        // Preamp slider fills the mid column minus its label + value box.
        let slider_w = (mid_w - 116.0).clamp(56.0, 170.0);

        ui.horizontal(|ui| {
            ui.set_min_height(TB_FULL_H);
            let x0 = ui.min_rect().min.x;
            ui.add_space(TB_EDGE_PAD);

            // ON — one tall cell spanning both rows.
            tb_cell(ui, "on", TB_ON_W, TB_FULL_H, |ui| self.tb_power(ui, &state));
            ui.separator();

            // Undo (top) / Redo (bottom).
            tb_column(ui, |ui| {
                tb_cell(ui, "undo", TB_UNDO_W, TB_ROW_H, |ui| {
                    if ui
                        .add_enabled(!self.undo_stack.is_empty(), egui::Button::new("Undo"))
                        .clicked()
                    {
                        self.undo();
                    }
                });
                tb_cell(ui, "redo", TB_UNDO_W, TB_ROW_H, |ui| {
                    if ui
                        .add_enabled(!self.redo_stack.is_empty(), egui::Button::new("Redo"))
                        .clicked()
                    {
                        self.redo();
                    }
                });
            });
            ui.separator();

            // Preamp (top) / daemon + theme + settings menus (bottom). Elastic.
            tb_column(ui, |ui| {
                tb_cell(ui, "preamp", mid_w, TB_ROW_H, |ui| {
                    self.tb_preamp(ui, &state, slider_w)
                });
                tb_cell(ui, "menus", mid_w, TB_ROW_H, |ui| {
                    self.daemon_menu(ui);
                    self.theme_menu(ui);
                    self.settings_menu(ui);
                });
            });
            ui.separator();

            // Load/Export (top) / Reset layout (bottom).
            tb_column(ui, |ui| {
                tb_cell(ui, "loadexp", TB_AUX_W, TB_ROW_H, |ui| {
                    self.tb_load_export(ui, &state)
                });
                tb_cell(ui, "reset", TB_AUX_W, TB_ROW_H, |ui| self.tb_reset(ui));
            });
            ui.separator();

            let left_used = ui.cursor().min.x - x0;

            // Trailing column: output (top) / meters+status (bottom). Meters are
            // shown only when they fit; pushed right by a measured spacer.
            let space = (ui.available_width() - tail_target).max(0.0);
            ui.add_space(space);
            let (out_w, full_w) = ui
                .vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let w1 = ui
                        .horizontal(|ui| {
                            ui.set_min_height(TB_ROW_H);
                            self.tb_output(ui, &state);
                        })
                        .response
                        .rect
                        .width();
                    let w2 = ui
                        .horizontal(|ui| {
                            ui.set_min_height(TB_ROW_H);
                            if show_meters {
                                if let Some(s) = &state {
                                    self.meters_widget(ui, s);
                                }
                                if !self.status.is_empty() {
                                    ui.separator();
                                    ui.label(&self.status);
                                }
                            }
                        })
                        .response
                        .rect
                        .width();
                    (w1, w1.max(w2))
                })
                .inner;
            ui.ctx()
                .data_mut(|d| d.insert_temp(egui::Id::new("tb_tail_out"), out_w));
            // Only record the full (with-meters) width while meters are shown,
            // else the stored value collapses and the decision oscillates.
            if show_meters {
                ui.ctx()
                    .data_mut(|d| d.insert_temp(egui::Id::new("tb_tail_full"), full_w));
            }
            // Drives the dynamic min width (honoured by floating WMs).
            self.tb_required_w = left_used + tail_target;
        });
    }

    /// Prominent power toggle: a large filled green/red button, not a tiny
    /// checkbox. The app title lives in the custom titlebar, not here.
    fn tb_power(&mut self, ui: &mut egui::Ui, state: &Option<DaemonState>) {
        let enabled = state.as_ref().map(|s| s.enabled).unwrap_or(false);
        let (txt, fill) = if enabled {
            ("ON", self.palette.boost)
        } else {
            ("OFF", self.palette.cut)
        };
        // A transparent default-font label sizes the button exactly like the
        // other toolbar buttons (same text style + button padding → same
        // height); we don't force a min height. The visible status dot and
        // label are painted as one centred group on top (the font's ● glyph
        // sits off the vertical centre, so we draw our own dot). Vertical
        // centring within the double-row cell is handled by `tb_cell`.
        let font = egui::TextStyle::Button.resolve(ui.style());
        // Squat button: trim vertical padding so it's shorter than a default
        // button, but keep a width floor so it stays wide.
        ui.spacing_mut().button_padding.y = 1.0;
        let power_btn = egui::Button::new(
            egui::RichText::new(format!("   {txt}"))
                .font(font.clone())
                .color(egui::Color32::TRANSPARENT),
        )
        .fill(fill)
        .min_size(egui::vec2(66.0, 0.0));
        let resp = ui
            .add_enabled(state.is_some(), power_btn)
            .on_hover_text("toggle DSP power");
        let r = resp.rect;
        let galley =
            ui.painter()
                .layout_no_wrap(txt.to_string(), font.clone(), egui::Color32::WHITE);
        const DOT_D: f32 = 10.0;
        const GAP: f32 = 6.0;
        let block_w = DOT_D + GAP + galley.size().x;
        let start_x = r.center().x - block_w * 0.5;
        let cy = r.center().y;
        let dot_c = egui::pos2(start_x + DOT_D * 0.5, cy);
        if enabled {
            ui.painter().circle_filled(dot_c, 5.0, egui::Color32::WHITE);
        } else {
            ui.painter()
                .circle_stroke(dot_c, 4.5, egui::Stroke::new(1.6, egui::Color32::WHITE));
        }
        ui.painter().text(
            egui::pos2(start_x + DOT_D + GAP, cy),
            egui::Align2::LEFT_CENTER,
            txt,
            font,
            egui::Color32::WHITE,
        );
        if resp.clicked() {
            self.queue_edit(Command::SetPower { enabled: !enabled });
        }
    }

    fn tb_preamp(&mut self, ui: &mut egui::Ui, state: &Option<DaemonState>, slider_w: f32) {
        ui.label("Preamp");
        // Slider rail width is supplied so the two-row layout can shrink it as
        // the window narrows (one row passes its full 170).
        ui.spacing_mut().slider_width = slider_w;
        if let Some(s) = state {
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
    }

    fn tb_load_export(&mut self, ui: &mut egui::Ui, state: &Option<DaemonState>) {
        if ui
            .button("Load…")
            .on_hover_text("import a .fac / APO .txt / Resonance .toml file")
            .clicked()
        {
            let lib = resonance_ipc::paths::user_preset_dir();
            let _ = std::fs::create_dir_all(&lib);
            let start = if lib.is_dir() {
                lib
            } else {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
            };
            self.dialog = Dialog::LoadPreset(Browser::new(start, false));
        }
        if ui
            .button("Export…")
            .on_hover_text("save the current chain as a Resonance .toml profile")
            .clicked()
        {
            let lib = resonance_ipc::paths::user_preset_dir();
            let _ = std::fs::create_dir_all(&lib);
            let stem = state
                .as_ref()
                .and_then(|s| s.current_preset.clone())
                .unwrap_or_else(|| "resonance".to_string());
            self.dialog = Dialog::ExportProfile(SaveDialog {
                browser: Browser::new(lib, true),
                filename: stem,
            });
        }
    }

    /// Output device picker (left-to-right: 🔊 then the combo).
    fn tb_output(&mut self, ui: &mut egui::Ui, state: &Option<DaemonState>) {
        if let Some(s) = state {
            ui.label("🔊");
            self.output_combo(ui, s);
        }
    }

    fn output_combo(&mut self, ui: &mut egui::Ui, s: &DaemonState) {
        // Sentinel for the "follow the OS default output" choice.
        const AUTO: &str = "\u{0}auto";
        let following = s.preferred_output.is_none();
        let current = if following {
            AUTO.to_string()
        } else {
            s.preferred_output.clone().unwrap_or_default()
        };
        let mut sel = current.clone();
        // When following the system, show the device it's currently on.
        let selected_text = if following {
            match &s.active_output {
                Some(d) => format!("Auto · {}", ellipsize(&s.sink_label(d), 16)),
                None => "Automatic".to_string(),
            }
        } else {
            ellipsize(&s.sink_label(&sel), 24)
        };
        egui::ComboBox::from_id_salt("toolbar_sink")
            .selected_text(selected_text)
            .width(180.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut sel, AUTO.to_string(), "Automatic (follow system)");
                ui.separator();
                for sink in &s.available_sinks {
                    let label = s.sink_label(sink);
                    ui.selectable_value(&mut sel, sink.clone(), label);
                }
            });
        if sel != current {
            if sel == AUTO {
                self.queue(Command::FollowSystemOutput);
            } else if !sel.is_empty() {
                self.queue(Command::SetOutputTarget { node_name: sel });
            }
        }
    }

    fn tb_history(&mut self, ui: &mut egui::Ui) {
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
    }

    fn tb_reset(&mut self, ui: &mut egui::Ui) {
        if ui
            .button("Reset layout")
            .on_hover_text("restore default panel sizes")
            .clicked()
        {
            self.reset_layout(ui.ctx());
        }
    }

    /// Modal listing every FR-graph gesture and keyboard shortcut, for users
    /// who don't know the (mostly mouse-driven) controls exist.
    fn help_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_help {
            return;
        }
        let mut open = true;
        egui::Window::new("Controls & shortcuts")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                // (gesture, what it does) rows under section headers.
                let section = |ui: &mut egui::Ui, title: &str, rows: &[(&str, &str)]| {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(title).strong());
                    egui::Grid::new(title)
                        .num_columns(2)
                        .spacing(egui::vec2(16.0, 4.0))
                        .striped(true)
                        .show(ui, |ui| {
                            for (k, v) in rows {
                                ui.label(egui::RichText::new(*k).monospace());
                                ui.label(*v);
                                ui.end_row();
                            }
                        });
                };
                section(
                    ui,
                    "EQ graph — nodes",
                    &[
                        ("Left-drag node", "move band (frequency + gain)"),
                        ("Right-drag node", "adjust Q (drag up = narrower)"),
                        ("Double-left-click", "add a peaking band there"),
                        ("Double-right-click node", "pin frequency; again to release"),
                        (
                            "Shift+double-right-click node",
                            "pin gain; again to release",
                        ),
                    ],
                );
                section(
                    ui,
                    "EQ graph — zoom",
                    &[
                        ("Scroll wheel", "zoom x-axis around the pointer"),
                        ("Shift + left-drag", "box-select a frequency range to zoom"),
                        ("Click \"reset ⟲\"", "restore the full 20 Hz–20 kHz view"),
                    ],
                );
                section(
                    ui,
                    "Keyboard",
                    &[
                        ("Ctrl+Z", "undo"),
                        ("Ctrl+Y / Ctrl+Shift+Z", "redo"),
                        ("F1 / ?", "toggle this help"),
                        ("Esc", "close this help"),
                    ],
                );
            });
        if !open {
            self.show_help = false;
        }
    }

    fn tb_status(&mut self, ui: &mut egui::Ui) {
        if !self.status.is_empty() {
            ui.separator();
            ui.label(&self.status);
        }
    }

    /// Daemon lifecycle controls (systemd user service) as a compact menu so
    /// users never type a `systemctl` line. All ops dispatch to a worker
    /// thread so the UI never blocks on launchctl / systemctl latency.
    fn daemon_menu(&mut self, ui: &mut egui::Ui) {
        if !service::manager_available() {
            return;
        }
        let st = self.daemon_status;
        let busy = self.service_busy;
        let (dot, color) = if busy {
            ("…", self.palette.boost)
        } else if st.active {
            ("●", self.palette.boost)
        } else {
            ("○", self.palette.cut)
        };
        ui.menu_button(
            egui::RichText::new(format!("{dot} Daemon")).color(color),
            |ui| {
                ui.label(format!(
                    "{}  ·  autostart {}",
                    if busy {
                        "…"
                    } else if st.active {
                        "running"
                    } else {
                        "stopped"
                    },
                    if st.enabled { "on" } else { "off" },
                ));
                ui.separator();
                let actions: [(&str, ServiceFn); 3] = [
                    ("Start", service::start),
                    ("Stop", service::stop),
                    ("Restart", service::restart),
                ];
                for (label, f) in actions {
                    let btn = ui.add_enabled(!busy, egui::Button::new(label));
                    if btn.clicked() {
                        self.service_busy = true;
                        let _ = self.service_tx.send(ServiceAction::Run { label, f });
                    }
                }
                ui.separator();
                let mut autostart = st.enabled;
                let auto = ui.add_enabled(
                    !busy,
                    egui::Checkbox::new(&mut autostart, "Autostart at login"),
                );
                if auto.changed() {
                    self.service_busy = true;
                    let f: ServiceFn = if autostart {
                        service::enable
                    } else {
                        service::disable
                    };
                    let _ = self.service_tx.send(ServiceAction::Run {
                        label: "autostart",
                        f,
                    });
                }
            },
        );
    }

    /// Clear the persisted panel sizes so the resizable panels fall back to
    /// their defaults next frame.
    fn reset_layout(&mut self, ctx: &egui::Context) {
        use egui::containers::panel::PanelState;
        for id in ["fr", "spectrum", "fx_pane", "dev_pane"] {
            ctx.data_mut(|d| d.remove::<PanelState>(egui::Id::new(id)));
        }
        self.set_status("layout reset");
    }

    /// Theme picker combo box.
    fn theme_menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut sel = self.theme;
        egui::ComboBox::from_id_salt("theme")
            .selected_text(ellipsize(self.theme.label(), 16))
            .width(120.0)
            .show_ui(ui, |ui| {
                for t in Theme::ALL {
                    ui.selectable_value(&mut sel, t, t.label());
                }
            });
        if sel != self.theme {
            self.set_theme(&ctx, sel);
        }
    }

    /// Settings menu: window-decoration (titlebar) mode.
    fn settings_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("⚙", |ui| {
            ui.label("Titlebar");
            for m in TitlebarMode::ALL {
                let hint = match m {
                    TitlebarMode::Auto => "custom bar, native on tiling WMs",
                    TitlebarMode::Custom => "always our titlebar",
                    TitlebarMode::Native => "always OS decorations",
                };
                if ui
                    .selectable_label(self.titlebar_mode == m, m.label())
                    .on_hover_text(hint)
                    .clicked()
                {
                    self.titlebar_mode = m;
                    ui.close();
                }
            }
        });
    }

    /// Custom client-side titlebar: app logo + title + drag-to-move + window
    /// controls. Shown only when `use_csd()` (native decorations are off).
    fn titlebar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let pal = self.palette;
        // Reserve a fixed-height strip and interact with its whole area for
        // drag-to-move / double-click-maximize.
        let h = 30.0;
        let full = egui::vec2(ui.available_width(), h);
        let (rect, resp) = ui.allocate_exact_size(full, egui::Sense::click_and_drag());
        if resp.drag_started_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if resp.double_clicked_by(egui::PointerButton::Primary) {
            let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
        }

        // Brand logo + title at the left (the title lives here, not the toolbar).
        // The brand doubles as the help button: click it to open the controls &
        // shortcuts overlay.
        let pad = 8.0;
        let logo = egui::Rect::from_min_size(
            egui::pos2(rect.left() + pad, rect.center().y - 9.0),
            egui::vec2(18.0, 18.0),
        );
        let title_pos = egui::pos2(logo.right() + 8.0, rect.center().y);
        let galley = ui.painter().layout_no_wrap(
            "Resonance".to_owned(),
            egui::FontId::proportional(14.0),
            pal.neutral,
        );
        // Clickable hit-area spanning the logo + the title text.
        let brand_rect = egui::Rect::from_min_max(
            logo.left_top(),
            egui::pos2(title_pos.x + galley.size().x, rect.bottom()),
        );
        let brand = ui.interact(
            brand_rect,
            egui::Id::new("titlebar_brand_help"),
            egui::Sense::click(),
        );
        if brand.clicked() {
            self.show_help = !self.show_help;
        }
        if brand.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let title_col = if brand.hovered() {
            pal.highlight
        } else {
            pal.neutral
        };
        crate::icon::paint(ui.painter(), logo);
        ui.painter().text(
            title_pos,
            egui::Align2::LEFT_CENTER,
            "Resonance",
            egui::FontId::proportional(14.0),
            title_col,
        );
        brand.on_hover_text("controls & shortcuts (F1)");

        // Window-control buttons at the right edge: close, maximize, minimize.
        let bw = 30.0;
        let btn_rect = |slot: usize| {
            let right = rect.right() - slot as f32 * bw;
            egui::Rect::from_min_max(
                egui::pos2(right - bw, rect.top()),
                egui::pos2(right, rect.bottom()),
            )
        };
        let maxed = ctx.input(|i| i.viewport().maximized.unwrap_or(false));

        // Close (slot 0).
        let close = ui.interact(
            btn_rect(0),
            egui::Id::new("titlebtn_close"),
            egui::Sense::click(),
        );
        if close.hovered() {
            ui.painter().rect_filled(close.rect, 0.0, pal.cut);
        }
        paint_glyph(ui.painter(), close.rect, "close", egui::Color32::WHITE);
        if close.clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // Maximize / restore (slot 1).
        let max = ui.interact(
            btn_rect(1),
            egui::Id::new("titlebtn_max"),
            egui::Sense::click(),
        );
        if max.hovered() {
            ui.painter()
                .rect_filled(max.rect, 0.0, pal.grid.gamma_multiply(0.8));
        }
        paint_glyph(
            ui.painter(),
            max.rect,
            if maxed { "restore" } else { "maximize" },
            pal.neutral,
        );
        if max.clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maxed));
        }

        // Minimize (slot 2).
        let min = ui.interact(
            btn_rect(2),
            egui::Id::new("titlebtn_min"),
            egui::Sense::click(),
        );
        if min.hovered() {
            ui.painter()
                .rect_filled(min.rect, 0.0, pal.grid.gamma_multiply(0.8));
        }
        paint_glyph(ui.painter(), min.rect, "minimize", pal.neutral);
        if min.clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
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

        // Levels only — the output device is shown by the toolbar selector.
        // Reading order: I │ O │ DSP │ CLIP.
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
                if service::manager_available() {
                    let busy = self.service_busy;
                    let btn = egui::Button::new(
                        egui::RichText::new(if busy {
                            "starting…"
                        } else {
                            "▶  Start daemon"
                        })
                        .size(18.0)
                        .color(egui::Color32::WHITE),
                    )
                    .fill(self.palette.boost)
                    .min_size(egui::vec2(180.0, 40.0));
                    if ui.add_enabled(!busy, btn).clicked() {
                        self.service_busy = true;
                        let _ = self.service_tx.send(ServiceAction::Run {
                            label: "Start",
                            f: service::start,
                        });
                    }
                    ui.add_space(6.0);
                    let mut autostart = self.daemon_status.enabled;
                    let auto = ui.add_enabled(
                        !busy,
                        egui::Checkbox::new(&mut autostart, "Start automatically at login"),
                    );
                    if auto.changed() {
                        self.service_busy = true;
                        let f: ServiceFn = if autostart {
                            service::enable
                        } else {
                            service::disable
                        };
                        let _ = self.service_tx.send(ServiceAction::Run {
                            label: "autostart",
                            f,
                        });
                    }
                } else {
                    ui.label(service::manager_unavailable_message());
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

        // Inset the plotting area so nodes and the curve never sit flush against
        // the panel edge — a few px of breathing room on every side.
        let plot = rect.shrink2(egui::vec2(8.0, 8.0));

        // Response curve + auto-scaled dB axis: the axis grows to fit the
        // loudest point (curve peak or any band's gain) plus 5 dB headroom, so
        // big boosts/cuts stay on-screen with margin instead of clipping.
        let (vlo, vhi) = self.view_log;
        let zoomed = vlo > curve::LOG_MIN + 1e-6 || vhi < curve::LOG_MAX - 1e-6;
        // Draw the curve from an optimistic copy: while dragging, the daemon's
        // echoed `state` only refreshes at the worker's ~30 Hz poll, so the line
        // would visibly lag the node (which uses the immediate `drag_value`).
        // Patch the dragged band's live freq/gain in so the line and node move
        // together at the display's frame rate.
        let mut bands = state.bands.clone();
        if let (Some(i), Some((f, g))) = (self.drag_band, self.drag_value) {
            if let Some(b) = bands.get_mut(i) {
                b.freq = f;
                b.gain_db = g;
            }
        }
        let pts = curve::curve_points_range(&bands, state.sample_rate, 240, vlo, vhi);
        // Loudest point the axis must show (any band gain or curve peak) + 5 dB
        // headroom. Includes the dragged band, so the axis expands live as you
        // drag a node up and contracts as you bring it down.
        let peak = pts
            .iter()
            .map(|&(_, g)| g.abs())
            .chain(bands.iter().map(|b| b.gain_db.abs()))
            .fold(0.0_f64, f64::max);
        let needed = peak + 5.0;
        // Pick the ± dB stop with HYSTERESIS so it doesn't chatter (jiggle) when
        // `needed` sits right on a stop boundary: grow once it exceeds 98% of the
        // current stop, shrink only once it drops below 65%. The wide deadband
        // also breaks the gain↔axis feedback — dragging the node up to ~mid-graph
        // settles on a stop instead of running away (only slamming the node to
        // the very top edge takes it to the max stop, which is what you'd want).
        if needed > self.db_target * 0.98 || needed < self.db_target * 0.65 {
            let (t, s) = curve::display_range(needed);
            self.db_target = t;
            self.db_step = s;
        }
        let target_db = self.db_target;
        let db_step = self.db_step;
        // Ease the axis toward the chosen stop so the curve + markers glide
        // instead of snapping. Same easing whether or not a drag is active, so
        // expand and contract feel identical.
        self.db_axis += (target_db - self.db_axis) * 0.20;
        if (self.db_axis - target_db).abs() < 0.05 {
            self.db_axis = target_db;
        }
        let db = self.db_axis;
        let axis_animating = (self.db_axis - target_db).abs() > 1e-3;
        let x_of =
            |logf: f64| -> f32 { plot.left() + ((logf - vlo) / (vhi - vlo)) as f32 * plot.width() };
        let y_of = |gain: f64| -> f32 {
            plot.top() + (1.0 - ((gain + db) / (2.0 * db)) as f32) * plot.height()
        };
        let logf_of =
            |x: f32| -> f64 { vlo + ((x - plot.left()) / plot.width()) as f64 * (vhi - vlo) };
        let db_of =
            |y: f32| -> f64 { ((1.0 - (y - plot.top()) / plot.height()) as f64) * 2.0 * db - db };

        // Frequency-region background bands (sub/bass/lo-mid/hi-mid/treble/air).
        // Alternating faint fills make each tonal region lightly noticeable
        // without competing with the curve; a dim label sits along the top.
        for (i, (lo, hi, label)) in curve::freq_bands().into_iter().enumerate() {
            let xl = x_of(lo.log10()).max(plot.left());
            let xr = x_of(hi.log10()).min(plot.right());
            if xr <= xl {
                continue;
            }
            let band =
                egui::Rect::from_min_max(egui::pos2(xl, plot.top()), egui::pos2(xr, plot.bottom()));
            let alpha = if i % 2 == 0 { 10 } else { 22 };
            let [r, g, b, _] = pal.neutral.to_array();
            painter.rect_filled(
                band,
                0.0,
                egui::Color32::from_rgba_unmultiplied(r, g, b, alpha),
            );
            painter.text(
                egui::pos2((xl + xr) * 0.5, plot.top() + 1.0),
                egui::Align2::CENTER_TOP,
                label,
                egui::FontId::monospace(8.0),
                egui::Color32::from_rgba_unmultiplied(r, g, b, 150),
            );
        }

        // Horizontal dB grid lines at multiples of the step within ±range.
        let label_col = pal.neutral;
        let grid = egui::Stroke::new(1.0, pal.grid.gamma_multiply(0.6));
        let n_lines = (db / db_step) as i32;
        for k in -n_lines..=n_lines {
            let g = k as f64 * db_step;
            let y = y_of(g);
            let stroke = if g == 0.0 {
                // Emphasised 0 dB reference line.
                egui::Stroke::new(1.6, pal.neutral)
            } else {
                grid
            };
            painter.line_segment(
                [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
                stroke,
            );
            painter.text(
                egui::pos2(plot.left() + 2.0, y),
                egui::Align2::LEFT_CENTER,
                format!("{g:+.0}"),
                egui::FontId::monospace(9.0),
                label_col,
            );
        }
        // Vertical frequency grid + labels.
        for (logf, label) in curve::x_axis_ticks_range(vlo, vhi) {
            let x = x_of(logf);
            painter.line_segment(
                [egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())],
                grid,
            );
            painter.text(
                egui::pos2(x, plot.bottom() - 2.0),
                egui::Align2::CENTER_BOTTOM,
                label,
                egui::FontId::monospace(9.0),
                label_col,
            );
        }

        // Response curve — colour-coded by gain: each segment is tinted toward
        // boost (green) or cut (red), neutral near 0 dB.
        for w in pts.windows(2) {
            let (lf0, g0) = w[0];
            let (lf1, g1) = w[1];
            let a = egui::pos2(x_of(lf0), y_of(g0));
            let b = egui::pos2(x_of(lf1), y_of(g1));
            let color = gain_color((g0 + g1) * 0.5, &pal);
            painter.line_segment([a, b], egui::Stroke::new(2.0, color));
        }

        use egui::PointerButton::{Primary, Secondary};

        // ── Zoom ────────────────────────────────────────────────────────────
        // Scroll wheel over the graph zooms the x-axis around the pointer;
        // Shift+left-drag box-selects a frequency range to zoom into. The
        // span is the daemon-wide LOG_MIN..LOG_MAX; we never zoom out past it.
        let shift = ui.input(|i| i.modifiers.shift);
        let full = (curve::LOG_MIN, curve::LOG_MAX);
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                if let Some(p) = response.hover_pos() {
                    let center = logf_of(p.x).clamp(vlo, vhi);
                    // Positive scroll = zoom in; shrink the span toward center.
                    let factor = (-scroll as f64 * 0.0015).exp();
                    let new_span =
                        ((vhi - vlo) * factor).clamp(0.15, curve::LOG_MAX - curve::LOG_MIN);
                    let t = (center - vlo) / (vhi - vlo);
                    let mut lo = center - t * new_span;
                    let mut hi = lo + new_span;
                    // Slide back inside the full span instead of clipping.
                    if lo < full.0 {
                        hi += full.0 - lo;
                        lo = full.0;
                    }
                    if hi > full.1 {
                        lo -= hi - full.1;
                        hi = full.1;
                    }
                    self.view_log = (lo.max(full.0), hi.min(full.1));
                }
            }
        }
        // Shift+left-drag: select an x-range, zoom to it on release.
        if shift && response.drag_started_by(Primary) {
            if let Some(p) = response.interact_pointer_pos() {
                self.zoom_sel = Some(logf_of(p.x));
            }
        }
        if let Some(start) = self.zoom_sel {
            if let Some(p) = response.interact_pointer_pos() {
                let (a, b) = (x_of(start).min(p.x), x_of(start).max(p.x));
                let band = egui::Rect::from_min_max(
                    egui::pos2(a.max(plot.left()), plot.top()),
                    egui::pos2(b.min(plot.right()), plot.bottom()),
                );
                let [r, g, bl, _] = pal.highlight.to_array();
                painter.rect_filled(
                    band,
                    0.0,
                    egui::Color32::from_rgba_unmultiplied(r, g, bl, 40),
                );
                painter.rect_stroke(
                    band,
                    0.0,
                    egui::Stroke::new(1.0, pal.highlight),
                    egui::StrokeKind::Inside,
                );
            }
            if response.drag_stopped() {
                if let Some(p) = response.interact_pointer_pos() {
                    let (lo, hi) = (start.min(logf_of(p.x)), start.max(logf_of(p.x)));
                    // Ignore a stray click; require a meaningful selection width.
                    if hi - lo > 0.05 {
                        self.view_log = (lo.max(full.0), hi.min(full.1));
                    }
                }
                self.zoom_sel = None;
            }
        }
        // "Reset zoom" affordance + current range readout (only when zoomed).
        if zoomed && self.zoom_sel.is_none() {
            let lo_hz = 10f64.powf(vlo).round();
            let hi_hz = 10f64.powf(vhi).round();
            let txt = format!("{lo_hz:.0}–{hi_hz:.0} Hz · reset ⟲");
            let btn = painter.text(
                egui::pos2(plot.right() - 4.0, plot.top() + 2.0),
                egui::Align2::RIGHT_TOP,
                txt,
                egui::FontId::monospace(10.0),
                pal.highlight,
            );
            let hit = btn.expand(3.0);
            if response.clicked() {
                if let Some(p) = response.interact_pointer_pos() {
                    if hit.contains(p) {
                        self.view_log = full;
                    }
                }
            }
        }

        // Double-right-click a node → toggle vertical-lock (gain-only) movement.
        // Shift+double-right-click → toggle gain-lock (freq+Q only, gain pinned).
        // The two locks are mutually exclusive.
        if response.double_clicked_by(Secondary) {
            if let Some(p) = response.interact_pointer_pos() {
                if let Some(i) = nearest_band(state, p, &x_of, &y_of) {
                    if ui.input(|inp| inp.modifiers.shift) {
                        self.hlock = if self.hlock == Some(i) { None } else { Some(i) };
                        self.vlock = None;
                    } else {
                        self.vlock = if self.vlock == Some(i) { None } else { Some(i) };
                        self.hlock = None;
                    }
                    self.selected_band = i;
                }
            }
        }

        // Drag handling: left button moves a node (freq+gain), right button
        // tunes its Q (drag up = narrower). A vertical-locked node moves on the
        // gain axis only. Pick the nearest node on press.
        let started_primary = response.drag_started_by(Primary);
        let started_secondary = response.drag_started_by(Secondary);
        if (started_primary || started_secondary) && self.zoom_sel.is_none() {
            if let Some(p) = response.interact_pointer_pos() {
                if let Some(i) = nearest_band(state, p, &x_of, &y_of) {
                    self.drag_band = Some(i);
                    self.selected_band = i;
                    // Right button always tunes Q — the vertical lock pins only
                    // frequency, never Q.
                    self.drag_q = started_secondary;
                }
            }
        }
        if let Some(i) = self.drag_band {
            let locked = self.vlock == Some(i);
            let gain_locked = self.hlock == Some(i);
            if self.drag_q && response.dragged_by(Secondary) {
                let dy = response.drag_delta().y as f64;
                if dy != 0.0 {
                    if let Some(b) = state.bands.get(i) {
                        // Exponential so Q scales smoothly across its range.
                        let q = (b.q * (-dy * 0.015).exp()).clamp(0.1, Q_LIMIT);
                        self.queue_edit(Command::SetBand {
                            index: i,
                            freq: b.freq,
                            gain_db: b.gain_db,
                            q,
                        });
                    }
                }
            } else if !self.drag_q && response.dragged_by(Primary) {
                if let Some(p) = response.interact_pointer_pos() {
                    if let Some(b) = state.bands.get(i) {
                        // vlock: keep freq, move gain only.
                        // hlock: keep gain, move freq only.
                        let freq = if locked {
                            b.freq
                        } else {
                            10f64.powf(logf_of(p.x)).clamp(20.0, 20000.0)
                        };
                        let gain = if gain_locked {
                            b.gain_db
                        } else {
                            // Map the cursor through the current axis. Because the
                            // node is rendered with the same `db` (y_of ∘ db_of =
                            // identity), it stays exactly under the cursor; the
                            // axis growth is decoupled (see the peak calc above),
                            // so there's no feedback wobble.
                            db_of(p.y).clamp(-GAIN_LIMIT, GAIN_LIMIT)
                        };
                        // Remember the cursor-derived value so the node renders
                        // there immediately (not at the IPC-lagged echo).
                        self.drag_value = Some((freq, gain));
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
            self.drag_value = None;
        }

        // Drive continuous frames (not just on input events) while dragging a
        // node or while the dB axis is still easing, so motion is smooth at the
        // display's refresh rate rather than the OS pointer-event cadence.
        if self.drag_band.is_some() || axis_animating {
            ui.ctx().request_repaint();
        }
        // Double-left-click empty area → add a peaking band there.
        if response.double_clicked_by(Primary) {
            if let Some(p) = response.interact_pointer_pos() {
                let freq = 10f64.powf(logf_of(p.x)).clamp(20.0, 20000.0);
                let gain = db_of(p.y).clamp(-GAIN_LIMIT, GAIN_LIMIT);
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
            // While this band is being dragged, render it at the cursor-derived
            // value (not the IPC-lagged echo) so the node tracks the mouse.
            let (bf, bg) = match self.drag_value {
                Some(v) if self.drag_band == Some(i) => v,
                _ => (b.freq, b.gain_db),
            };
            // Clamp the node inside the plot so it can never draw off-screen
            // even if the response momentarily exceeds the axis.
            let center = egui::pos2(
                x_of(curve::clampf_log(bf)).clamp(plot.left(), plot.right()),
                y_of(bg).clamp(plot.top(), plot.bottom()),
            );
            let selected = i == self.selected_band;
            let locked = self.vlock == Some(i);
            let gain_locked = self.hlock == Some(i);
            // High-contrast guide derived from the graph background (not the
            // palette accent/highlight, which on some themes — e.g. matugen —
            // matches the curve and nodes). Dashed + thick so it stands out
            // against the grid and the response curve.
            let guide = contrast_color(pal.graph_bg);
            let stroke = egui::Stroke::new(2.0, guide);
            // vlock: vertical guide with end caps ("moves up/down only").
            if locked {
                let x = center.x;
                painter.add(egui::Shape::dashed_line(
                    &[
                        egui::pos2(x, plot.top() + 2.0),
                        egui::pos2(x, plot.bottom() - 2.0),
                    ],
                    stroke,
                    6.0,
                    4.0,
                ));
                for cap_y in [plot.top() + 2.0, plot.bottom() - 2.0] {
                    painter.line_segment(
                        [egui::pos2(x - 5.0, cap_y), egui::pos2(x + 5.0, cap_y)],
                        stroke,
                    );
                }
            }
            // hlock: horizontal guide with end caps ("moves left/right only").
            if gain_locked {
                let y = center.y;
                painter.add(egui::Shape::dashed_line(
                    &[
                        egui::pos2(plot.left() + 2.0, y),
                        egui::pos2(plot.right() - 2.0, y),
                    ],
                    stroke,
                    6.0,
                    4.0,
                ));
                for cap_x in [plot.left() + 2.0, plot.right() - 2.0] {
                    painter.line_segment(
                        [egui::pos2(cap_x, y - 5.0), egui::pos2(cap_x, y + 5.0)],
                        stroke,
                    );
                }
            }
            // Selected node pops; the rest recede hard toward the background so
            // the active band is unmistakable on every theme. The selected node
            // also gets a high-contrast ring (white on dark, black on light).
            let color = if selected {
                pal.highlight
            } else {
                pal.neutral.gamma_multiply(0.45)
            };
            let r = if selected || locked || gain_locked {
                7.0
            } else {
                4.0
            };
            painter.circle_filled(center, r, color);
            let ring = if selected {
                egui::Stroke::new(2.0, contrast_color(pal.graph_bg))
            } else {
                egui::Stroke::new(1.0, pal.graph_bg)
            };
            painter.circle_stroke(center, r, ring);
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
        let pal = self.palette;
        for (i, &v) in self.spectrum_display.iter().enumerate() {
            let h = (v.clamp(0.0, 1.0)) * (rect.height() - 4.0);
            let x0 = rect.left() + gap + i as f32 * (bw + gap);
            let bar = egui::Rect::from_min_max(
                egui::pos2(x0, rect.bottom() - 2.0 - h),
                egui::pos2(x0 + bw, rect.bottom() - 2.0),
            );
            // Theme-aware gradient: low energy = accent, peaks toward highlight.
            let t = v.clamp(0.0, 1.0);
            let color = lerp_color(pal.accent, pal.highlight, t);
            painter.rect_filled(bar, 1.0, color);
        }
    }

    // ── Effects ─────────────────────────────────────────────────────────────

    /// Narrow-window fallback: the lower sections as one tabbed pane. A centred
    /// tab bar picks Effects / EQ bands / Device Profile Mapping / Profiles; the
    /// chosen section fills the full width below.
    fn lower_tabs(&mut self, ui: &mut egui::Ui, state: &Option<DaemonState>) {
        ui.add_space(4.0);
        let tabs = [
            (LowerTab::Effects, "Effects"),
            (LowerTab::Bands, "EQ bands"),
            (LowerTab::Mapping, "Device Profile Mapping"),
            (LowerTab::Profiles, "Profiles"),
        ];
        // `centered` pads from the row's measured width so the (variable-length)
        // tab bar sits centred.
        centered(ui, "lower_tabs", |ui| {
            ui.horizontal(|ui| {
                for (tab, label) in tabs {
                    let sel = self.lower_tab == tab;
                    if ui.add(egui::Button::selectable(sel, label)).clicked() {
                        self.lower_tab = tab;
                    }
                }
            });
        });
        ui.separator();
        match self.lower_tab {
            LowerTab::Effects => {
                padded_scroll(ui, "tab_effects", |ui| {
                    if let Some(s) = state {
                        self.effects_section(ui, s);
                    }
                });
            }
            LowerTab::Bands => {
                padded_scroll(ui, "tab_bands", |ui| {
                    if let Some(s) = state {
                        self.bands_section(ui, s);
                    }
                });
            }
            LowerTab::Mapping => {
                padded_scroll(ui, "tab_mapping", |ui| self.device_mapping_section(ui));
            }
            LowerTab::Profiles => {
                padded_scroll(ui, "tab_profiles", |ui| self.profiles_panel(ui));
            }
        }
    }

    fn effects_section(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        ui.vertical_centered(|ui| ui.heading("Effects"));
        ui.add_space(4.0);
        centered(ui, "effects_body", |ui| {
            egui::Grid::new("effects_grid")
                .num_columns(3)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    for id in FxEffectId::ALL {
                        let name = id.label();
                        let (mut intensity, mut on) = state.effects.get(id);
                        let min = id.min();

                        if ui.checkbox(&mut on, "").changed() {
                            self.queue_edit(Command::SetEffectEnabled {
                                effect: id,
                                enabled: on,
                            });
                        }
                        ui.label(name);
                        // Slider is always interactive — dragging it auto-enables the
                        // effect so you can set a value without first ticking the box.
                        if ui
                            .add(
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
                            if !on {
                                self.queue_edit(Command::SetEffectEnabled {
                                    effect: id,
                                    enabled: true,
                                });
                            }
                            self.queue_edit(Command::SetEffectIntensity {
                                effect: id,
                                value: intensity,
                            });
                        }
                        ui.end_row();
                    }
                });
        });
    }

    // ── EQ bands table ──────────────────────────────────────────────────────

    fn bands_section(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        ui.vertical_centered(|ui| ui.heading("EQ bands"));
        ui.add_space(4.0);

        centered(ui, "bands_body", |ui| {
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
                    ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                        ui.label("Q")
                    });
                    ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                        ui.label("Gain Graph")
                    });
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
                                    .range(-GAIN_LIMIT..=GAIN_LIMIT)
                                    .fixed_decimals(1),
                            )
                            .changed();
                        let q_changed = ui
                            .add(
                                egui::DragValue::new(&mut q)
                                    .speed(0.02)
                                    .range(0.1..=Q_LIMIT)
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
                            // Keep the lock pins pointing at the same band after
                            // the list shifts (or drop them if the pinned band
                            // was the one removed).
                            remap_pin_on_remove(&mut self.vlock, i);
                            remap_pin_on_remove(&mut self.hlock, i);
                        }
                        ui.end_row();
                    }
                });
        });

        // "Add band" sits under the table, reading as "append a new row below".
        // Subtle (weak text, default chrome) so it matches the rest of the UI.
        ui.add_space(6.0);
        centered(ui, "bands_add", |ui| {
            let btn = egui::Button::new("✚  Add band").min_size(egui::vec2(160.0, 24.0));
            if ui.add(btn).on_hover_text("append a new EQ band").clicked() {
                self.queue_edit(Command::AddBand {
                    band_type: BandType::Peaking,
                    freq: 1000.0,
                    gain_db: 0.0,
                    q: 1.4,
                });
            }
        });

        if self.selected_band >= state.bands.len() {
            self.selected_band = state.bands.len().saturating_sub(1);
        }
    }

    // ── Right column: devices → profiles + profile list ─────────────────────

    fn devices_profiles(&mut self, ui: &mut egui::Ui) {
        self.device_mapping_section(ui);
        ui.add_space(8.0);
        ui.separator();
        self.profiles_panel(ui);
    }

    /// Device → profile mapping table: every output device we've ever seen, each
    /// with a profile dropdown. The active device auto-loads its mapped one.
    fn device_mapping_section(&mut self, ui: &mut egui::Ui) {
        let state = self.state.clone();
        ui.vertical_centered(|ui| ui.heading("Device Profile Mapping"));
        if let Some(s) = &state {
            centered(ui, "dev_body", |ui| self.device_table(ui, s));
        } else {
            ui.weak("(no daemon)");
        }
    }

    /// Saved profiles list (heading + the save/load/rename rows).
    fn profiles_panel(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| ui.heading("Profiles"));
        centered(ui, "profiles_body", |ui| self.profiles_section(ui));
    }

    /// Profiles list: a fixed-width save row plus one row per profile — Load
    /// (A/B), an inline-editable name (type + Enter to rename), and delete.
    /// Widths are fixed (not `available_width`) so the block centres cleanly.
    fn profiles_section(&mut self, ui: &mut egui::Ui) {
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
    fn device_table(&mut self, ui: &mut egui::Ui, s: &DaemonState) {
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

    // ── Preset load dialog ──────────────────────────────────────────────────

    fn preset_dialog(&mut self, ctx: &egui::Context) {
        let Dialog::LoadPreset(browser) = &mut self.dialog else {
            return;
        };
        let mut open = true;
        let mut close = false;
        let mut to_load: Option<String> = None;

        let pal = self.palette;
        egui::Window::new("Load preset")
            // Fresh id: egui keys collapse/geometry state off the window title,
            // and `collapsible(false)` hides the toggle without force-expanding.
            // An older build let this window collapse, so the persisted collapsed
            // state under the title-derived id left it stuck as a title-only
            // pill. A dedicated id starts from `default_open` (expanded).
            .id(egui::Id::new("resonance_load_preset_dialog"))
            .open(&mut open)
            .resizable(true)
            .default_size([720.0, 480.0])
            .min_width(520.0)
            .collapsible(false)
            .show(ctx, |ui| {
                if let Some(p) = nav_bar(ui, browser) {
                    to_load = Some(p);
                }
                ui.add_space(4.0);

                // Keyboard navigation when no text field holds focus.
                let kbd = ctx.memory(|m| m.focused().is_none());
                let mut activate: Option<usize> = None;
                if kbd {
                    ui.input(|i| {
                        if i.key_pressed(egui::Key::ArrowDown) {
                            browser.move_cursor(1);
                        }
                        if i.key_pressed(egui::Key::ArrowUp) {
                            browser.move_cursor(-1);
                        }
                        if i.key_pressed(egui::Key::Backspace) {
                            browser.parent();
                        }
                        if i.key_pressed(egui::Key::Enter) {
                            activate = Some(browser.cursor);
                        }
                    });
                }

                // Stacked, fixed-height body (constants, not `available_height`,
                // so the auto-sizing window can't oscillate). File list on top,
                // parsed preview below — mirrors the Export dialog's layout.
                let mut select: Option<usize> = None;
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("files")
                        .auto_shrink([false, false])
                        .min_scrolled_height(260.0)
                        .max_height(260.0)
                        .show(ui, |ui| {
                            for (i, it) in browser.entries.iter().enumerate() {
                                let label = format!("{}  {}", entry_icon(it), it.name);
                                let resp = ui
                                    .selectable_label(i == browser.cursor, label)
                                    .on_hover_text(it.path.display().to_string());
                                if resp.clicked() {
                                    select = Some(i);
                                }
                                if resp.double_clicked() {
                                    activate = Some(i);
                                }
                            }
                        });
                });
                if let Some(i) = select {
                    browser.select(i);
                }
                if let Some(i) = activate {
                    if let Some(path) = browser.activate(i) {
                        to_load = Some(path);
                    }
                }

                // Parsed preview of the highlighted entry.
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Preview").color(pal.neutral));
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("preview")
                        .auto_shrink([false, false])
                        .min_scrolled_height(150.0)
                        .max_height(150.0)
                        .show(ui, |ui| {
                            if browser.preview.is_empty() {
                                ui.weak("select a file to preview");
                            }
                            for line in &browser.preview {
                                ui.monospace(line);
                            }
                        });
                });

                // Footer: Load (only for a loadable file) + Cancel.
                ui.separator();
                let loadable = browser
                    .selected()
                    .map(|it| !it.is_dir && it.is_preset)
                    .unwrap_or(false);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(loadable, egui::Button::new("Load"))
                        .clicked()
                    {
                        to_load = browser.activate(browser.cursor);
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });

        if let Some(path) = to_load {
            self.import_and_load(path);
            close = true;
        }
        if !open || close {
            self.dialog = Dialog::None;
        }
    }

    /// Save-as dialog: navigate to a folder, type a name, write the current
    /// chain as a Resonance `.toml` profile.
    fn export_dialog(&mut self, ctx: &egui::Context) {
        let Dialog::ExportProfile(save) = &mut self.dialog else {
            return;
        };
        let pal = self.palette;
        let mut open = true;
        let mut close = false;
        let mut do_export: Option<String> = None;

        egui::Window::new("Export profile")
            .open(&mut open)
            .resizable(true)
            .default_size([640.0, 500.0])
            .min_width(480.0)
            .collapsible(false)
            .show(ctx, |ui| {
                // Typing a full file path into the location bar pre-fills the
                // folder + name instead of loading anything.
                if let Some(p) = nav_bar(ui, &mut save.browser) {
                    if let Some(stem) = std::path::Path::new(&p)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                    {
                        save.filename = stem;
                    }
                }
                ui.add_space(4.0);

                // Folder + existing-file picker. Clicking a file copies its name
                // into the field (a quick "overwrite this one" gesture). Reserve
                // the footer (name field, path readout, buttons) and let the list
                // fill the rest of the resizable window.
                let mut select: Option<usize> = None;
                let mut activate: Option<usize> = None;
                let mut pick_name: Option<String> = None;
                // Constant height (see Load dialog) — avoids window-size jitter.
                let body_h = 240.0;
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("save_files")
                        .auto_shrink([false, false])
                        .min_scrolled_height(body_h)
                        .max_height(body_h)
                        .show(ui, |ui| {
                            for (i, it) in save.browser.entries.iter().enumerate() {
                                let label = format!("{}  {}", entry_icon(it), it.name);
                                let resp = ui.selectable_label(i == save.browser.cursor, label);
                                if resp.clicked() {
                                    select = Some(i);
                                    if !it.is_dir {
                                        pick_name = std::path::Path::new(&it.name)
                                            .file_stem()
                                            .map(|s| s.to_string_lossy().into_owned());
                                    }
                                }
                                if resp.double_clicked() {
                                    activate = Some(i);
                                }
                            }
                        });
                });
                if let Some(i) = select {
                    save.browser.select(i);
                }
                if let Some(n) = pick_name {
                    save.filename = n;
                }
                if let Some(i) = activate {
                    save.browser.activate(i); // dirs navigate; files are ignored here
                }

                // Filename + implicit .toml suffix.
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.add(
                        egui::TextEdit::singleline(&mut save.filename)
                            .desired_width(260.0)
                            .hint_text("profile name"),
                    );
                    ui.label(egui::RichText::new(".toml").color(pal.neutral));
                });

                // Destination readout + overwrite warning.
                let target = save.target();
                let exists = target.is_file();
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("→ ").color(pal.neutral));
                    ui.label(egui::RichText::new(target.display().to_string()).monospace());
                });
                if exists {
                    ui.colored_label(
                        pal.cut,
                        "! a file with this name exists — it will be overwritten",
                    );
                }

                ui.separator();
                ui.horizontal(|ui| {
                    let ok = !save.filename.trim().is_empty();
                    let label = if exists { "Overwrite" } else { "Save" };
                    if ui.add_enabled(ok, egui::Button::new(label)).clicked() {
                        do_export = Some(target.to_string_lossy().into_owned());
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });

        if let Some(p) = do_export {
            self.export_profile(p);
            close = true;
        }
        if !open || close {
            self.dialog = Dialog::None;
        }
    }

    /// Write the current chain to `path` as a `.toml` profile (round-trips via
    /// the daemon so it captures the authoritative state).
    fn export_profile(&mut self, path: String) {
        let _ = self.cmd_tx.send(WorkerCmd::Export(path));
    }

    /// Yes/no modal for overwriting or deleting a profile (destructive actions
    /// confirm before running).
    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(action) = self.confirm.clone() else {
            return;
        };
        let (title, body, ok_label) = match &action {
            Confirm::SaveProfile(name) => (
                "Overwrite profile",
                format!(
                    "Profile '{name}' already exists.\nOverwrite it with the current settings?"
                ),
                "Overwrite",
            ),
            Confirm::DeleteProfile(name) => (
                "Delete profile",
                format!("Really delete profile '{name}'?\nThis cannot be undone."),
                "Delete",
            ),
        };
        let mut decision: Option<bool> = None;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(body);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let ok = egui::Button::new(
                        egui::RichText::new(ok_label).color(egui::Color32::WHITE),
                    )
                    .fill(self.palette.cut);
                    if ui.add(ok).clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
        match decision {
            Some(true) => {
                match action {
                    Confirm::SaveProfile(name) => self.queue(Command::SaveProfile { name }),
                    Confirm::DeleteProfile(name) => self.queue(Command::DeleteProfile { name }),
                }
                self.needs_meta = true;
                self.confirm = None;
            }
            Some(false) => self.confirm = None,
            None => {}
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

/// Leading glyph for a directory entry. Restricted to glyphs present in the
/// embedded `icons.ttf` subset (`▸ ↑ ·`) so nothing renders as a tofu box —
/// egui can't rasterise colour-emoji fonts, so file-type emoji are out.
fn entry_icon(it: &Item) -> &'static str {
    if it.is_dir {
        if it.name == ".." { "↑" } else { "▸" }
    } else {
        "·"
    }
}

/// Shared navigation header for the Load / Export dialogs: bookmark buttons, an
/// editable location bar, and a name filter. Returns `Some(path)` when the typed
/// location resolves to a file (load dialogs act on it; save dialogs pre-fill).
fn nav_bar(ui: &mut egui::Ui, browser: &mut Browser) -> Option<String> {
    let mut typed: Option<String> = None;
    ui.horizontal(|ui| {
        if ui
            .button("↑ Up")
            .on_hover_text("parent directory")
            .clicked()
        {
            browser.parent();
        }
        if ui.button("Home").on_hover_text("home directory").clicked() {
            browser.navigate(crate::browser::home_dir());
        }
        if ui
            .button("Library")
            .on_hover_text("Resonance preset library")
            .clicked()
        {
            let lib = resonance_ipc::paths::user_preset_dir();
            let _ = std::fs::create_dir_all(&lib);
            browser.navigate(lib);
        }
    });
    ui.add_space(2.0);
    egui::Grid::new("nav_fields")
        .num_columns(2)
        .spacing([6.0, 4.0])
        .show(ui, |ui| {
            ui.label("Path");
            let edit = ui.add(
                egui::TextEdit::singleline(&mut browser.path_edit)
                    .desired_width(f32::INFINITY)
                    .hint_text("type a path and press Enter"),
            );
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                typed = browser.go_to_typed();
            }
            ui.end_row();

            ui.label("Filter");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut browser.filter)
                        .desired_width(f32::INFINITY)
                        .hint_text("filter by name"),
                )
                .changed()
            {
                browser.refilter();
            }
            ui.end_row();
        });
    typed
}

/// Adjust a band-index lock pin after the band at `removed` is deleted: drop the
/// pin if it was that band, decrement it if it sat above the removed index.
fn remap_pin_on_remove(pin: &mut Option<usize>, removed: usize) {
    match *pin {
        Some(i) if i == removed => *pin = None,
        Some(i) if i > removed => *pin = Some(i - 1),
        _ => {}
    }
}

fn nearest_band(
    state: &DaemonState,
    p: egui::Pos2,
    x_of: &dyn Fn(f64) -> f32,
    y_of: &dyn Fn(f64) -> f32,
) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, b) in state.bands.iter().enumerate() {
        // Disabled bands aren't drawn, so they must not be grabbable either —
        // otherwise a drag/double-click lands on an invisible node.
        if !b.enabled {
            continue;
        }
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
    let t = (db.abs() / GAIN_LIMIT).clamp(0.0, 1.0) as f32;
    if db.abs() < 0.3 {
        return pal.accent;
    }
    let target = if db > 0.0 { pal.boost } else { pal.cut };
    lerp_color(pal.accent, target, t)
}

/// Truncate `s` to at most `max` chars, appending an ellipsis when cut. Keeps
/// the toolbar output combo from expanding past its slot on long device names.
fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let keep = max.saturating_sub(1);
        format!("{}…", s.chars().take(keep).collect::<String>())
    }
}

/// A two-row toolbar column: stack its cells with no vertical gap so the column
/// is exactly `TB_FULL_H` tall and the separators on either side stay flush.
fn tb_column(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        add(ui);
    });
}

/// One fixed-size toolbar cell with its contents centred both ways. A top-down
/// `Align::Center` layout centres the content row horizontally for free; the
/// vertical centring pads from *last frame's* measured content height (kept in
/// egui memory, like `centered`) to avoid a layout feedback loop. Identical cell
/// sizes across both rows keep every column — and the separators between them —
/// aligned.
fn tb_cell(ui: &mut egui::Ui, id: &str, w: f32, h: f32, add: impl FnOnce(&mut egui::Ui)) {
    let key = egui::Id::new(("tb_cell", id));
    let prev_h = ui.ctx().data(|d| d.get_temp::<f32>(key)).unwrap_or(0.0);
    let pad = ((h - prev_h) * 0.5).max(0.0);
    let out = ui.allocate_ui_with_layout(
        egui::vec2(w, h),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            ui.set_min_size(egui::vec2(w, h));
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(pad);
            ui.horizontal(|ui| add(ui)).response.rect.height()
        },
    );
    ui.ctx().data_mut(|d| d.insert_temp(key, out.inner));
}

/// Wrap a column's content in uniform inner padding and a vertical scroll area,
/// so the three central columns breathe instead of hugging the panel edge.
fn padded_scroll(ui: &mut egui::Ui, salt: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::default()
        .inner_margin(egui::Margin::symmetric(8, 10))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(salt)
                .auto_shrink([false, false])
                .show(ui, add);
        });
}

/// Centre `add`'s content horizontally by its own measured width, so the
/// content keeps its natural size and only the side padding grows/shrinks as
/// the column resizes. Pads from *last frame's* measured width (kept in egui
/// memory) to avoid a layout feedback loop — so nothing inside `add` may size
/// itself to `ui.available_width()`, or the width would never settle.
fn centered<R>(ui: &mut egui::Ui, id_src: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let id = egui::Id::new(("centered", id_src));
    let avail = ui.available_width();
    let prev = ui.ctx().data(|d| d.get_temp::<f32>(id)).unwrap_or(0.0);
    let pad = ((avail - prev) * 0.5).max(0.0);
    let outer = ui.horizontal(|ui| {
        ui.add_space(pad);
        ui.vertical(add)
    });
    let inner = outer.inner;
    ui.ctx()
        .data_mut(|d| d.insert_temp(id, inner.response.rect.width()));
    inner.inner
}

/// Paint a window-control glyph centred in `rect`. The bundled icon font lacks
/// minimize/maximize/restore glyphs, so draw them as line art.
fn paint_glyph(painter: &egui::Painter, rect: egui::Rect, kind: &str, color: egui::Color32) {
    let c = rect.center();
    let s = 5.0;
    let stroke = egui::Stroke::new(1.4, color);
    match kind {
        "minimize" => {
            painter.line_segment(
                [
                    egui::pos2(c.x - s, c.y + 0.5),
                    egui::pos2(c.x + s, c.y + 0.5),
                ],
                stroke,
            );
        }
        "maximize" => {
            painter.rect_stroke(
                egui::Rect::from_center_size(c, egui::vec2(2.0 * s, 2.0 * s)),
                0.0,
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        "restore" => {
            let sq = egui::Rect::from_center_size(c, egui::vec2(1.8 * s, 1.8 * s));
            painter.rect_stroke(
                sq.translate(egui::vec2(-1.5, 1.5)),
                0.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.rect_stroke(
                sq.translate(egui::vec2(1.5, -1.5)),
                0.0,
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        "close" => {
            painter.line_segment(
                [egui::pos2(c.x - s, c.y - s), egui::pos2(c.x + s, c.y + s)],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(c.x - s, c.y + s), egui::pos2(c.x + s, c.y - s)],
                stroke,
            );
        }
        _ => {}
    }
}

/// A colour that contrasts strongly with `bg`: near-white on dark backgrounds,
/// near-black on light ones. Used for UI guides that must read on any theme.
fn contrast_color(bg: egui::Color32) -> egui::Color32 {
    let lum = 0.299 * bg.r() as f32 + 0.587 * bg.g() as f32 + 0.114 * bg.b() as f32;
    if lum < 128.0 {
        egui::Color32::from_rgb(245, 245, 250)
    } else {
        egui::Color32::from_rgb(15, 15, 20)
    }
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
    // Scale to the FR graph's ±DB_RANGE so the bar length matches the curve's
    // vertical extent (and the TUI's bar), not the much larger ±GAIN_LIMIT edit
    // clamp — which made a typical ±6 dB edit read as a sliver.
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
