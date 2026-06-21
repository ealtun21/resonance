//! egui/eframe front-end for the Resonance daemon.
//!
//! All daemon mutations are expressed as `Command`s collected into `pending`
//! during the frame, then dispatched synchronously after the UI is built. The
//! authoritative `DaemonState` is re-fetched immediately afterwards (and on a
//! periodic poll) so widgets always reflect the daemon.

use crate::curve;
use crate::ipc::IpcClient;
use crate::state::*;
use crate::theme::{Palette, Theme};
use crate::ui::widgets::*;
use eframe::egui;
use resonance_ipc::{Command, DaemonState, Response, service, transport::TransportError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// Consecutive edits within this window coalesce into one undo entry (a drag
/// gesture becomes a single undo step).
const UNDO_COALESCE: Duration = Duration::from_millis(400);

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

/// A zero-arg systemd service action (start/stop/restart/…).
pub(crate) type ServiceFn = fn() -> std::io::Result<()>;

/// One unit of work for the service worker thread.
pub(crate) enum ServiceAction {
    /// Re-read installed/active/enabled status from the platform manager.
    RefreshStatus,
    /// Run a lifecycle op (start/stop/restart/enable/disable). The static
    /// `label` is shown in the toolbar status when the result comes back.
    Run { label: &'static str, f: ServiceFn },
}

/// Worker → UI message. Carries an updated Status snapshot (always) plus
/// optional toolbar feedback (only for Run results).
pub(crate) struct ServiceWorkerResult {
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

/// Theme the native Windows title bar via DWM so it matches the app instead of
/// the default light/grey OS bar. Immersive dark mode works on Windows 10+; the
/// exact caption/text colours apply on Windows 11 (22000+) and are harmless
/// no-ops on Windows 10 (it stays dark).
#[cfg(target_os = "windows")]
mod native_titlebar {
    use std::ffi::c_void;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn FindWindowW(class: *const u16, name: *const u16) -> isize;
    }
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attr: u32, pv: *const c_void, cb: u32) -> i32;
    }
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_CAPTION_COLOR: u32 = 35; // Win11 22000+
    const DWMWA_TEXT_COLOR: u32 = 36; // Win11 22000+

    /// 0x00BBGGRR COLORREF from an egui colour.
    fn colorref(c: egui::Color32) -> u32 {
        (c.r() as u32) | ((c.g() as u32) << 8) | ((c.b() as u32) << 16)
    }

    fn hwnd() -> isize {
        let title: Vec<u16> = "Resonance\0".encode_utf16().collect();
        // SAFETY: null class + valid NUL-terminated wide title; FindWindow reads
        // only the title and returns the matching top-level window (ours).
        unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) }
    }

    /// Returns false if the window wasn't found yet (caller should retry).
    pub fn apply(dark: bool, caption: egui::Color32, text: egui::Color32) -> bool {
        let h = hwnd();
        if h == 0 {
            return false;
        }
        let dark_i: i32 = dark as i32;
        let cap = colorref(caption);
        let txt = colorref(text);
        // SAFETY: each call passes a pointer to a 4-byte value of the stated
        // size; unsupported attributes (Win10) just return an error we ignore.
        unsafe {
            DwmSetWindowAttribute(
                h,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                (&dark_i as *const i32).cast(),
                4,
            );
            DwmSetWindowAttribute(h, DWMWA_CAPTION_COLOR, (&cap as *const u32).cast(), 4);
            DwmSetWindowAttribute(h, DWMWA_TEXT_COLOR, (&txt as *const u32).cast(), 4);
        }
        true
    }
}

pub struct GuiApp {
    /// Channel to the IPC worker thread — the UI thread never does IPC itself,
    /// so a stopped/restarting daemon can't block or freeze the window.
    pub(crate) cmd_tx: std::sync::mpsc::Sender<WorkerCmd>,
    /// Latest snapshot the IPC worker published (copied into fields each frame).
    pub(crate) shared: Arc<Mutex<GuiShared>>,
    pub(crate) state: Option<DaemonState>,
    pub(crate) profiles: Vec<String>,
    pub(crate) mappings: Vec<(String, String)>,
    pub(crate) status: String,
    pub(crate) needs_meta: bool,
    pub(crate) dialog: Dialog,
    /// Pending profile save-overwrite / delete awaiting a yes/no modal.
    pub(crate) confirm: Option<Confirm>,
    pub(crate) selected_band: usize,
    pub(crate) drag_band: Option<usize>,
    /// Optimistic (freq, gain) of the band being dragged, so its marker tracks
    /// the cursor exactly instead of the IPC-lagged echoed state.
    pub(crate) drag_value: Option<(f64, f64)>,
    /// True while the active curve drag edits Q (right button) vs freq+gain.
    pub(crate) drag_q: bool,
    pub(crate) profile_name: String,
    /// Inline profile rename in progress: (original name, edit buffer).
    pub(crate) rename: Option<(String, String)>,
    /// Smoothed spectrum bar heights + last animation tick.
    pub(crate) spectrum_display: Vec<f32>,
    pub(crate) last_anim: Instant,
    /// Animated FR dB-axis half-range — eased toward the target so the axis
    /// grows/shrinks smoothly instead of snapping between stops (no flicker).
    pub(crate) db_axis: f64,
    /// Hysteretic target half-range (the chosen ± dB stop) + its grid step. Held
    /// across frames with a deadband so the stop choice doesn't chatter at a
    /// boundary; `db_axis` eases toward `db_target`.
    pub(crate) db_target: f64,
    pub(crate) db_step: f64,
    pub(crate) undo_stack: Vec<Snapshot>,
    pub(crate) redo_stack: Vec<Snapshot>,
    /// Start of the current edit burst (for undo coalescing).
    pub(crate) last_edit: Option<Instant>,
    /// While `Some` and in the future, the clip indicator flashes.
    pub(crate) clip_until: Option<Instant>,
    /// Active colour theme + its semantic palette (kept in sync).
    pub(crate) theme: Theme,
    pub(crate) palette: Palette,
    /// Band pinned to vertical (gain-only) movement via double-right-click.
    pub(crate) vlock: Option<usize>,
    /// Band pinned to gain (freq+Q only, gain locked) via shift+double-right-
    /// click. Mutually exclusive with `vlock`.
    pub(crate) hlock: Option<usize>,
    /// Visible x-axis range as (log10 freq lo, hi). Full span when not zoomed.
    pub(crate) view_log: (f64, f64),
    /// Start of an in-progress shift-drag zoom selection (log10 freq).
    pub(crate) zoom_sel: Option<f64>,
    /// Keybind / gesture help overlay visible.
    pub(crate) show_help: bool,
    /// Cached systemd user-service status; refreshed on a slow timer.
    pub(crate) daemon_status: service::Status,
    pub(crate) last_service_poll: Instant,
    /// Channel into the service worker. We send `ServiceAction` requests
    /// off the UI thread because `launchctl` calls + the daemon's
    /// CoreAudio teardown can take 200–800 ms — synchronous on the UI
    /// thread froze egui visibly when the user clicked Start/Stop.
    pub(crate) service_tx: std::sync::mpsc::Sender<ServiceAction>,
    /// Worker result channel. Each tick we drain it and apply updates
    /// (status text + cached Status).
    pub(crate) service_rx: std::sync::mpsc::Receiver<ServiceWorkerResult>,
    /// True while a Start/Stop/Restart is in flight — we grey-out the
    /// menu so the user can't fire overlapping ops.
    pub(crate) service_busy: bool,
    /// When the current `status` text should auto-clear (transient feedback).
    pub(crate) status_until: Option<Instant>,
    /// Theme the native Windows title bar (via DWM) was last styled for; re-apply
    /// only when the theme changes. `None` until applied.
    #[cfg(target_os = "windows")]
    pub(crate) native_titlebar_theme: Option<Theme>,
    /// Matugen colour-file mtime + last poll, for live theme reload.
    pub(crate) matugen_mtime: Option<SystemTime>,
    pub(crate) last_matugen_check: Instant,
}

/// Messages from the UI thread to the IPC worker.
pub(crate) enum WorkerCmd {
    Cmd(Command),
    RefreshMeta,
    Import(String),
    Export(String),
}

/// Snapshot the IPC worker publishes for the UI thread to read each frame.
#[derive(Default)]
pub(crate) struct GuiShared {
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
            #[cfg(target_os = "windows")]
            native_titlebar_theme: None,
            matugen_mtime: crate::theme::matugen_source_mtime(),
            last_matugen_check: Instant::now(),
        }
    }

    /// Switch theme: store it, refresh the cached palette, and push new visuals.
    pub(crate) fn set_theme(&mut self, ctx: &egui::Context, theme: Theme) {
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
    pub(crate) fn queue_edit(&mut self, cmd: Command) {
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

    pub(crate) fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop() {
            if let Some(cur) = self.snapshot() {
                self.redo_stack.push(cur);
            }
            self.apply_snapshot(&prev);
            self.last_edit = None;
            self.set_status("undo");
        }
    }

    pub(crate) fn redo(&mut self) {
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

    pub(crate) fn queue(&mut self, cmd: Command) {
        let _ = self.cmd_tx.send(WorkerCmd::Cmd(cmd));
    }

    /// Set the toolbar status text with an auto-clear timer so transient action
    /// feedback doesn't linger like a permanent label.
    pub(crate) fn set_status(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_until = Some(Instant::now() + STATUS_TTL);
    }

    /// Import a preset file as a profile (our own format), then load that
    /// profile — mirrors the TUI flow so presets are always captured, not just
    /// applied transiently.
    pub(crate) fn import_and_load(&mut self, path: String) {
        let _ = self.cmd_tx.send(WorkerCmd::Import(path));
    }
}

impl eframe::App for GuiApp {
    /// Persist the chosen theme. Panel sizes are saved by eframe's egui-memory
    /// persistence (enabled by the `persistence` feature + default
    /// `persist_egui_memory`).
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("theme", self.theme.label().to_string());
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Read the latest snapshot the IPC worker published (never blocks).
        self.pull_shared();

        // Tint the native Windows title bar to match the theme (re-apply on theme
        // change; retry until the window handle exists).
        #[cfg(target_os = "windows")]
        if self.native_titlebar_theme != Some(self.theme) {
            let (caption, text) = self.theme.native_caption_colors();
            if native_titlebar::apply(!self.theme.is_light(), caption, text) {
                self.native_titlebar_theme = Some(self.theme);
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

        egui::Panel::top("toolbar").show_inside(ui, |ui| self.toolbar(ui));

        self.shell(ui);

        let ctx = ui.ctx().clone();

        self.preset_dialog(&ctx);
        self.export_dialog(&ctx);
        self.confirm_dialog(&ctx);
        self.help_dialog(&ctx);

        // Drive ~144 fps repaint so spectrum/curve stay smooth.
        ctx.request_repaint_after(FRAME_INTERVAL);
    }
}

// ── UI sections ─────────────────────────────────────────────────────────────

impl GuiApp {
    /// Clear the persisted panel sizes so the resizable panels fall back to
    /// their defaults next frame.
    pub(crate) fn reset_layout(&mut self, ctx: &egui::Context) {
        use egui::containers::panel::PanelState;
        for id in ["fr", "spectrum", "fx_pane", "dev_pane"] {
            ctx.data_mut(|d| d.remove::<PanelState>(egui::Id::new(id)));
        }
        self.set_status("layout reset");
    }

    /// Write the current chain to `path` as a `.toml` profile (round-trips via
    /// the daemon so it captures the authoritative state).
    pub(crate) fn export_profile(&mut self, path: String) {
        let _ = self.cmd_tx.send(WorkerCmd::Export(path));
    }
}
