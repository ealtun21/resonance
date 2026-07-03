//! egui/eframe front-end for the Resonance daemon.
//!
//! All daemon mutations are expressed as `Command`s collected into `pending`
//! during the frame, then dispatched synchronously after the UI is built. The
//! authoritative `DaemonState` is re-fetched immediately afterwards (and on a
//! periodic poll) so widgets always reflect the daemon.

use crate::card_layout::{CardCol, CardId, CardLayout};
use crate::curve;
use crate::ipc::IpcClient;
use crate::state::{Confirm, Dialog, Snapshot};
use crate::theme::{Palette, Theme};
use crate::ui::kit;
use crate::ui::widgets::{dialog_window, install_symbol_fonts};
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
const META_INTERVAL: Duration = Duration::from_secs(1);
/// How often the worker retries `connect()` while the daemon is unreachable.
/// Snappy so a reconnect lands well inside `CONN_GRACE` after a daemon restart.
const RECONNECT_INTERVAL: Duration = Duration::from_millis(500);
/// Grace window: after the worker loses the daemon, the UI keeps showing the
/// last-known state (with a subtle "reconnecting…" hint) for this long before
/// falling back to the "No daemon connected" start screen. A daemon restart or
/// a momentary stall reconnects well within this window, so the UI no longer
/// flickers between connected and disconnected on every blip.
const CONN_GRACE: Duration = Duration::from_secs(3);
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

/// Result of a background Auto-EQ fit, applied on the UI thread once it lands.
pub(crate) struct AutoEqOutcome {
    /// Pre-fit chain state (for the undo step).
    pub snapshot: Snapshot,
    pub preamp_db: f64,
    pub bands: Vec<resonance_ipc::BandState>,
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
        u32::from(c.r()) | (u32::from(c.g()) << 8) | (u32::from(c.b()) << 16)
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
        let dark_i: i32 = i32::from(dark);
        let caption_rgb = colorref(caption);
        let text_rgb = colorref(text);
        // SAFETY: each call passes a pointer to a 4-byte value of the stated
        // size; unsupported attributes (Win10) just return an error we ignore.
        unsafe {
            DwmSetWindowAttribute(
                h,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                (&raw const dark_i).cast(),
                4,
            );
            DwmSetWindowAttribute(h, DWMWA_CAPTION_COLOR, (&raw const caption_rgb).cast(), 4);
            DwmSetWindowAttribute(h, DWMWA_TEXT_COLOR, (&raw const text_rgb).cast(), 4);
        }
        true
    }
}

/// macOS: vertically centre the window's traffic-light buttons within our
/// toolbar. With the unified (transparent) title bar the toolbar is taller than
/// the standard title bar `AppKit` lays the buttons out for, so they sit high and
/// look top-aligned; we nudge them to the toolbar's vertical centre. Re-applied
/// each frame because `AppKit` re-lays them out on resize. Best-effort: any missing
/// piece just leaves the buttons where `AppKit` put them.
#[cfg(target_os = "macos")]
mod traffic_lights {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSWindowButton};

    pub fn center(toolbar_h: f64) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        // Single-window process; the key window is ours (fall back to main when
        // unfocused — AppKit doesn't re-lay-out then, so doing nothing is fine).
        let Some(window) = app.keyWindow().or_else(|| app.mainWindow()) else {
            return;
        };
        for kind in [
            NSWindowButton::CloseButton,
            NSWindowButton::MiniaturizeButton,
            NSWindowButton::ZoomButton,
        ] {
            let Some(btn) = window.standardWindowButton(kind) else {
                continue;
            };
            // SAFETY: read-only access to the button's superview on the main thread.
            let Some(sv) = (unsafe { btn.superview() }) else {
                continue;
            };
            let frame = btn.frame();
            // The superview's top edge is pinned to the window top (whether it's
            // the full theme-frame or a short title-bar container), so placing the
            // button's centre `toolbar_h/2` down from that top centres it in the
            // toolbar regardless of which view hosts it.
            let svh = sv.frame().size.height;
            let mut nf = frame;
            nf.origin.y = svh - toolbar_h / 2.0 - frame.size.height / 2.0;
            btn.setFrame(nf);
        }
    }
}

pub struct GuiApp {
    /// Channel to the IPC worker thread — the UI thread never does IPC itself,
    /// so a stopped/restarting daemon can't block or freeze the window.
    pub(crate) cmd_tx: std::sync::mpsc::Sender<WorkerCmd>,
    /// Latest snapshot the IPC worker published (copied into fields each frame).
    pub(crate) shared: Arc<Mutex<GuiShared>>,
    pub(crate) state: Option<DaemonState>,
    /// When the worker first lost the daemon while we still held a snapshot.
    /// Drives the reconnect grace window (`CONN_GRACE`): we keep showing the
    /// held-over state until it elapses, so a transient drop doesn't blank the
    /// UI (the connect/disconnect flicker).
    pub(crate) conn_lost_since: Option<Instant>,
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
    /// Live filter text for the profiles list (shown only past a few profiles).
    pub(crate) profile_filter: String,
    /// Set when the dirty banner is clicked: focus the save-name field next frame.
    pub(crate) focus_profile_name: bool,
    /// Last observed loaded-profile name, for detecting profile-load transitions
    /// (to restore a profile's bundled measurement). `preset_seen` gates the very
    /// first observation so startup doesn't clobber the persisted live measurement.
    pub(crate) last_preset: Option<String>,
    pub(crate) preset_seen: bool,
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
    /// `CoreAudio` teardown can take 200–800 ms — synchronous on the UI
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
    /// Frame counter for the one-time window-state migration: counts up after the
    /// window is forced out of a restored fullscreen so we clear the stale panel
    /// sizes only once it has resized. `None` once the migration is done.
    pub(crate) migrate_settle: Option<u8>,
    /// Reference / measurement overlay state (target curve, headphone
    /// measurement, customizer, overlay-vs-deviation view).
    pub(crate) reference: resonance_reference::reference::ReferenceState,
    /// squig.link measurement downloader: command channel, event channel, and
    /// the last catalog snapshot + status it published.
    pub(crate) dl_tx: std::sync::mpsc::Sender<resonance_reference::download::DlCmd>,
    pub(crate) dl_rx: std::sync::mpsc::Receiver<resonance_reference::download::DlEvent>,
    pub(crate) catalog: Option<resonance_reference::download::Catalog>,
    pub(crate) dl_status: String,
    pub(crate) dl_busy: bool,
    /// Background Auto-EQ fit: result channel + in-flight flag (the fit runs off
    /// the UI thread so a 3000-step optimize never freezes the window).
    pub(crate) autoeq_tx: std::sync::mpsc::Sender<AutoEqOutcome>,
    pub(crate) autoeq_rx: std::sync::mpsc::Receiver<AutoEqOutcome>,
    pub(crate) autoeq_busy: bool,
    /// EQ has unsaved edits (set on any edit, cleared on profile save/load).
    /// Drives the save-before-quit prompt.
    pub(crate) dirty: bool,
    /// The "unsaved changes" close prompt is showing; its name buffer.
    pub(crate) pending_quit: bool,
    pub(crate) quit_save_name: String,
    /// Set once the user has resolved the prompt, so the window may close.
    pub(crate) allow_close: bool,
    /// Brief grace window to let a queued "Save & Quit" reach the daemon before
    /// the window actually closes.
    pub(crate) quit_deadline: Option<Instant>,
    /// Opt-in: reveal per-band channel targeting even on ≤2-channel devices
    /// (the per-band `Ch` column is otherwise hidden until >2ch, progressive
    /// disclosure). Lets stereo users do per-channel (L/R) EQ. Persisted.
    pub(crate) per_channel_eq: bool,
    /// Advanced-feature visibility toggles (persisted; default off for a clean
    /// UI). `show_slope`/`show_scope`/`show_dynamics` gate the bands-table
    /// Slope/Scope/Dyn columns; `show_dither` gates the Output section.
    /// Channels controls are relocated into the Settings dialog; the per-band
    /// `Ch` column stays gated by `per_channel_eq` (auto-on for >2ch).
    pub(crate) show_slope: bool,
    pub(crate) show_scope: bool,
    pub(crate) show_dynamics: bool,
    pub(crate) show_dither: bool,
    /// Gates the Convolution (impulse response) section under Effects.
    pub(crate) show_ir: bool,
    /// User's arrangement of the movable control cards (persisted).
    pub(crate) layout: CardLayout,
    /// Session-only "arrange the layout" mode: shows draggable card tiles + drop
    /// zones instead of the live cards. Never persisted.
    pub(crate) layout_edit: bool,
    /// A card move requested this frame by a drop, applied after the columns
    /// finish rendering (so the lists aren't mutated mid-iteration).
    pub(crate) pending_card_move: Option<(CardId, CardCol, usize)>,
    /// Graph series the user has hidden via the legend's eye toggles (keyed by
    /// the legend label, e.g. "FL"/"Result FR"/"Target"). Session-only.
    pub(crate) hidden_curves: std::collections::HashSet<String>,
    /// Dev/screenshot hook (`RESONANCE_DEMO=1`): hold an injected demo state and
    /// skip all IPC state updates so the full UI renders without a daemon.
    pub(crate) demo: bool,
    /// Dev/screenshot hook: hold the reference Customize popup open
    /// (`RESONANCE_OPEN=customize`). No effect in normal use.
    pub(crate) open_customizer: bool,
}

/// Messages from the UI thread to the IPC worker.
pub(crate) enum WorkerCmd {
    Cmd(Command),
    RefreshMeta,
    Import(String),
    Export(String),
    /// Export a *named* stored profile (per-row export): (profile, path).
    ExportNamed(String, String),
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

/// Outcome of draining the queued UI commands in the IPC worker.
enum DrainOutcome {
    /// The command channel is empty — carry on with the rest of the frame.
    Empty,
    /// A transport failure occurred; the caller must drop the connection and
    /// reconnect on the next iteration.
    ConnectionLost,
    /// The UI-thread sender hung up — the worker thread should exit.
    Shutdown,
}

/// Apply all currently-queued UI commands to the daemon connection `c`.
///
/// A daemon-level rejection (`Response::Error`) is surfaced as worker status but
/// keeps the socket — only a transport failure tears the connection down. Sets
/// `refresh_meta_now` when a command (or its follow-up) changed the profile set.
fn drain_commands(
    rx: &std::sync::mpsc::Receiver<WorkerCmd>,
    shared: &Arc<Mutex<GuiShared>>,
    c: &mut IpcClient,
    refresh_meta_now: &mut bool,
) -> DrainOutcome {
    loop {
        let msg = match rx.try_recv() {
            Ok(m) => m,
            Err(std::sync::mpsc::TryRecvError::Empty) => return DrainOutcome::Empty,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return DrainOutcome::Shutdown,
        };
        match msg {
            WorkerCmd::Cmd(cmd) => match c.send(cmd) {
                Ok(()) => {}
                Err(TransportError::Daemon(msg)) => worker_status(shared, msg),
                Err(_) => return DrainOutcome::ConnectionLost,
            },
            WorkerCmd::RefreshMeta => *refresh_meta_now = true,
            WorkerCmd::Import(path) => {
                match c.send_recv(Command::ImportPreset { path, name: None }) {
                    Ok(Response::Imported(name)) => {
                        let _ = c.send(Command::LoadProfile { name: name.clone() });
                        *refresh_meta_now = true;
                        worker_status(shared, format!("imported + loaded '{name}'"));
                    }
                    Ok(Response::Error(e)) => {
                        worker_status(shared, format!("import failed: {e}"));
                    }
                    Ok(_) => worker_status(shared, "import failed".into()),
                    Err(_) => return DrainOutcome::ConnectionLost,
                }
            }
            WorkerCmd::Export(path) => {
                match c.send_recv(Command::ExportProfile { path: path.clone() }) {
                    Ok(Response::Ok) => worker_status(shared, format!("exported → {path}")),
                    Ok(Response::Error(e)) => {
                        worker_status(shared, format!("export failed: {e}"));
                    }
                    Ok(_) => worker_status(shared, "export failed".into()),
                    Err(_) => return DrainOutcome::ConnectionLost,
                }
            }
            WorkerCmd::ExportNamed(name, path) => {
                match c.send_recv(Command::ExportProfileNamed {
                    name: name.clone(),
                    path: path.clone(),
                }) {
                    Ok(Response::Ok) => {
                        worker_status(shared, format!("exported '{name}' → {path}"));
                    }
                    Ok(Response::Error(e)) => {
                        worker_status(shared, format!("export failed: {e}"));
                    }
                    Ok(_) => worker_status(shared, "export failed".into()),
                    Err(_) => return DrainOutcome::ConnectionLost,
                }
            }
        }
    }
}

/// Poll the live daemon state and publish it for the UI thread. Returns `false`
/// on a transport error (caller drops the connection); the published state is
/// set to `None` on failure so the UI sees the loss.
fn poll_state(shared: &Arc<Mutex<GuiShared>>, c: &mut IpcClient, ctx: &egui::Context) -> bool {
    if let Ok(st) = c.get_state() {
        if let Ok(mut s) = shared.lock() {
            s.state = Some(st);
        }
        ctx.request_repaint();
        true
    } else {
        if let Ok(mut s) = shared.lock() {
            s.state = None;
        }
        ctx.request_repaint();
        false
    }
}

/// Refresh the slow-changing meta (profile list + device→profile mappings) and
/// publish it. Returns `false` on a transport error.
///
/// A transport error here must tear the connection down — leaving a half-read
/// framed reply on the socket would desync the next `GetState` and surface as a
/// spurious disconnect.
fn refresh_meta(shared: &Arc<Mutex<GuiShared>>, c: &mut IpcClient) -> bool {
    match c.send_recv(Command::ListProfiles) {
        Ok(Response::PresetList(p)) => {
            if let Ok(mut s) = shared.lock() {
                s.profiles = p;
            }
        }
        Ok(_) => {}
        Err(_) => return false,
    }
    match c.send_recv(Command::ListMappings) {
        Ok(Response::Mappings(m)) => {
            if let Ok(mut s) = shared.lock() {
                s.mappings = m;
            }
        }
        Ok(_) => {}
        Err(_) => return false,
    }
    true
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
            let mut last_meta = Instant::now().checked_sub(META_INTERVAL).unwrap();
            let mut refresh_meta_now = true;
            loop {
                if ipc.is_none() {
                    if let Ok(c) = crate::ipc::connect() {
                        ipc = Some(c);
                        refresh_meta_now = true;
                    } else {
                        if let Ok(mut s) = shared.lock() {
                            s.state = None;
                        }
                        ctx.request_repaint();
                        std::thread::sleep(RECONNECT_INTERVAL);
                        continue;
                    }
                }

                // Apply queued UI commands.
                if let Some(c) = ipc.as_mut() {
                    match drain_commands(&rx, &shared, c, &mut refresh_meta_now) {
                        DrainOutcome::Empty => {}
                        DrainOutcome::ConnectionLost => ipc = None,
                        DrainOutcome::Shutdown => return,
                    }
                }

                // Poll state.
                if let Some(c) = ipc.as_mut() {
                    if !poll_state(&shared, c, &ctx) {
                        ipc = None;
                    }
                }

                // Meta (profiles + mappings) on a slow timer / on request.
                if let Some(c) = ipc.as_mut() {
                    if refresh_meta_now || last_meta.elapsed() >= META_INTERVAL {
                        if refresh_meta(&shared, c) {
                            last_meta = Instant::now();
                            refresh_meta_now = false;
                        } else {
                            ipc = None;
                        }
                    }
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
            .map_or(Theme::System, |s| Theme::from_label(&s));
        cc.egui_ctx.set_visuals(theme.visuals());
        // Restore the reference overlay (measurement + target) from a previous
        // session so a loaded measurement persists across restarts.
        let persisted_ref = cc.storage.and_then(|s| s.get_string("reference"));
        // Compact, consistent button sizing so controls stay usable when the
        // window is narrow (set once; set_visuals doesn't touch spacing).
        // Denser, desktop-native metrics (tighter than egui's airy defaults).
        cc.egui_ctx.global_style_mut(|s| {
            s.spacing.button_padding = egui::vec2(8.0, 4.0);
            s.spacing.interact_size.y = 22.0;
            s.spacing.item_spacing = egui::vec2(7.0, 5.0);
            s.spacing.menu_margin = egui::Margin::same(6);
        });
        let (cmd_tx, ipc_rx) = std::sync::mpsc::channel::<WorkerCmd>();
        let shared = Arc::new(Mutex::new(GuiShared::default()));
        spawn_ipc_worker(ipc_rx, shared.clone(), cc.egui_ctx.clone());
        // Wake the egui event loop whenever the downloader emits an event (it
        // runs on a background thread). The shared crate is UI-agnostic, so we
        // hand it a repaint closure rather than the egui Context itself.
        let dl_ctx = cc.egui_ctx.clone();
        let wake: resonance_reference::download::Wake = Arc::new(move || dl_ctx.request_repaint());
        let (dl_tx, dl_rx) = resonance_reference::download::spawn(wake);
        // Warm the target/measurement catalog at startup from the on-disk cache
        // (instant) so the Manage/Browse dialogs open already populated; the
        // worker's `IfStale` policy silently re-fetches anything older than the
        // TTL in the background. Manual "Refresh" still forces a full re-fetch.
        let _ = dl_tx.send(resonance_reference::download::DlCmd::Init);
        let (autoeq_tx, autoeq_rx) = std::sync::mpsc::channel::<AutoEqOutcome>();

        let mut app = Self {
            cmd_tx,
            shared,
            state: None,
            conn_lost_since: None,
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
            profile_filter: String::new(),
            focus_profile_name: false,
            last_preset: None,
            preset_seen: false,
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
            migrate_settle: None,
            reference: resonance_reference::reference::ReferenceState::default(),
            dl_tx,
            dl_rx,
            catalog: None,
            dl_status: String::new(),
            dl_busy: false,
            autoeq_tx,
            autoeq_rx,
            autoeq_busy: false,
            dirty: false,
            pending_quit: false,
            quit_save_name: String::new(),
            allow_close: false,
            quit_deadline: None,
            per_channel_eq: cc
                .storage
                .and_then(|s| s.get_string("per_channel_eq"))
                .is_some_and(|v| v == "true"),
            show_slope: cc
                .storage
                .and_then(|s| s.get_string("show_slope"))
                .is_some_and(|v| v == "true"),
            show_scope: cc
                .storage
                .and_then(|s| s.get_string("show_scope"))
                .is_some_and(|v| v == "true"),
            show_dynamics: cc
                .storage
                .and_then(|s| s.get_string("show_dynamics"))
                .is_some_and(|v| v == "true"),
            show_dither: cc
                .storage
                .and_then(|s| s.get_string("show_dither"))
                .is_some_and(|v| v == "true"),
            show_ir: cc
                .storage
                .and_then(|s| s.get_string("show_ir"))
                .is_some_and(|v| v == "true"),
            layout: cc
                .storage
                .and_then(|s| s.get_string("card_layout"))
                .map(|s| CardLayout::from_json_or_default(&s))
                .unwrap_or_default(),
            layout_edit: std::env::var("RESONANCE_EDIT_LAYOUT").is_ok(),
            pending_card_move: None,
            hidden_curves: std::collections::HashSet::new(),
            demo: std::env::var("RESONANCE_DEMO").is_ok(),
            open_customizer: std::env::var("RESONANCE_OPEN").as_deref() == Ok("customize"),
        };
        if let Some(p) = persisted_ref.and_then(|j| serde_json::from_str(&j).ok()) {
            app.reference.restore(p);
        }
        app.apply_dev_hooks(&cc.egui_ctx);
        app
    }

    /// Apply the startup dev/screenshot hooks (`RESONANCE_DEMO`,
    /// `RESONANCE_OPEN`, `RESONANCE_DEMO_REF`). No effect in normal use — they
    /// only populate state / open dialogs for the screenshot harness.
    fn apply_dev_hooks(&mut self, ctx: &egui::Context) {
        if self.demo {
            self.populate_demo_state();
        }
        // `RESONANCE_OPEN=manage|browse` opens that dialog at startup so the
        // screenshot harness can capture it.
        match std::env::var("RESONANCE_OPEN").as_deref() {
            Ok("manage") => self.reference.show_manage = true,
            Ok("browse") => self.reference.show_browser = true,
            _ => {}
        }
        if let Ok(mode) = std::env::var("RESONANCE_DEMO_REF") {
            self.apply_demo_ref(ctx, &mode);
        }
    }

    /// `RESONANCE_DEMO=1`: inject a representative populated state so the full UI
    /// renders without a daemon — for the screenshot harness / design work.
    /// `pull_shared` early-returns in demo mode so IPC never clobbers it.
    fn populate_demo_state(&mut self) {
        self.state = Some(demo_state());
        self.profiles = vec![
            "Reference".into(),
            "Harman 660S".into(),
            "Late Night".into(),
            "Vocal Forward".into(),
            "Bass Heavy".into(),
        ];
        self.mappings = vec![
            ("alsa_output.usb-D1".into(), "Reference".into()),
            ("bluez.hd660s".into(), "Harman 660S".into()),
            ("bluez.wh1000xm5".into(), "Commute".into()),
        ];
        self.per_channel_eq = true;
        // Reference overlay on (target + synthetic measurement + bounds) so the
        // demo shows the full reference workflow like the design mock.
        self.reference.set_measurement(
            "Sennheiser HD 660S".into(),
            false,
            resonance_ipc::curve::RefCurve {
                points: demo_measurement(),
            },
            None,
        );
        self.reference.enabled = true;
        self.reference.show_bounds = true;
        if let Some((_, sel)) = self
            .reference
            .target_options()
            .into_iter()
            .find(|(n, _)| n != "None")
        {
            self.reference.set_target(sel);
        }
    }

    /// `RESONANCE_DEMO_REF=norm|raw`: load the first local curve as a stand-in
    /// measurement and turn the reference overlay on (normalised unless `raw`),
    /// so the harness can screenshot the overlay/legend/normalise morph without a
    /// live measurement. Also clears persisted panel sizes so the graph is
    /// full-height.
    fn apply_demo_ref(&mut self, ctx: &egui::Context, mode: &str) {
        use egui::containers::panel::PanelState;
        let dir = resonance_ipc::paths::user_curve_dir();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            if let Some(p) = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("txt"))
                .min()
                && self.reference.load_measurement_file(&p)
            {
                self.reference.enabled = true;
                self.reference.normalized = mode != "raw";
                self.reference.show_bounds = true;
            }
        }
        ctx.data_mut(|d| {
            d.remove::<PanelState>(egui::Id::new("controls_panel"));
            d.remove::<PanelState>(egui::Id::new("graph_narrow"));
        });
    }

    /// Drain download-worker events each frame: update the catalog snapshot /
    /// status, and install a fetched measurement onto the reference overlay.
    pub(crate) fn pump_downloads(&mut self) {
        while let Ok(ev) = self.dl_rx.try_recv() {
            match ev {
                resonance_reference::download::DlEvent::Catalog(c) => self.catalog = Some(c),
                resonance_reference::download::DlEvent::Status(s) => self.dl_status = s,
                resonance_reference::download::DlEvent::Busy(b) => self.dl_busy = b,
                resonance_reference::download::DlEvent::Fetched(f) => {
                    self.reference.enabled = true;
                    self.reference
                        .set_measurement(f.name, f.iem, f.left, f.right);
                    self.reference.show_browser = false;
                    self.touch_measurement();
                }
                resonance_reference::download::DlEvent::FetchedTarget { name, curve } => {
                    // Added from the Manage-targets dialog; keep the dialog open
                    // so the user can add several in a row.
                    self.reference.write_target(&name, &curve);
                    self.set_status(format!("added target: {name}"));
                }
            }
        }
    }

    /// Apply a finished background Auto-EQ fit (undo snapshot + `ApplyState`).
    pub(crate) fn pump_autoeq(&mut self) {
        while let Ok(o) = self.autoeq_rx.try_recv() {
            self.autoeq_busy = false;
            if o.bands.is_empty() {
                self.set_status("Auto-EQ: nothing to correct");
                continue;
            }
            let count = o.bands.len();
            let effects = o.snapshot.effects.clone();
            self.undo_stack.push(o.snapshot);
            if self.undo_stack.len() > 100 {
                self.undo_stack.remove(0);
            }
            self.redo_stack.clear();
            self.last_edit = None;
            self.dirty = true;
            self.queue(Command::ApplyState {
                preamp_db: o.preamp_db,
                enabled: true,
                bands: o.bands,
                effects,
            });
            self.set_status(format!("Auto-EQ: fitted {count} bands"));
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
            .is_some_and(|t| now.duration_since(t) < UNDO_COALESCE);
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
        self.dirty = true;
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
        // Demo/screenshot mode: keep the injected state; never let IPC clobber it.
        if self.demo {
            return;
        }
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
        // Connection hysteresis: a transient loss (daemon restart, a momentary
        // stall) must NOT instantly blank the UI to the "No daemon connected"
        // screen — that flip-flop is the connect/disconnect flicker. When the
        // worker has a fresh snapshot, adopt it and clear the grace timer. When
        // it doesn't, keep showing the last-known state for `CONN_GRACE`; only
        // after sustained loss do we drop to the start screen.
        match state {
            Some(st) => {
                let preset = st.current_preset.clone();
                self.state = Some(st);
                self.conn_lost_since = None;
                self.sync_profile_measurement(preset);
            }
            None => {
                // Hold the last snapshot for the grace window; only drop to the
                // start screen once the loss is sustained.
                if self.state.is_some() {
                    let since = *self.conn_lost_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= CONN_GRACE {
                        self.state = None;
                    }
                }
            }
        }
        self.profiles = profiles;
        self.mappings = mappings;
        // Drop lock pins that no longer name a valid band — a profile load or
        // another client can shrink the band list, after which a stale pin would
        // silently apply to a different band.
        let bands = self.state.as_ref().map_or(0, |s| s.bands.len());
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

    /// React to a change in the loaded profile (manual load or device auto-load):
    /// restore the measurement that profile was saved with, so profiles can be
    /// A/B-compared visually. The first observation after startup is a silent
    /// baseline — it must not clobber the persisted live measurement.
    fn sync_profile_measurement(&mut self, preset: Option<String>) {
        if !self.preset_seen {
            self.preset_seen = true;
            self.last_preset = preset;
            return;
        }
        if preset == self.last_preset {
            return;
        }
        self.last_preset = preset.clone();
        if let Some(name) = preset {
            self.reference.restore_measurement_for(&name);
        }
    }

    /// Mark the chain dirty when the user changes the measurement while a profile
    /// is loaded, so the unsaved banner prompts a re-save (which re-bundles the
    /// new measurement with the profile).
    pub(crate) fn touch_measurement(&mut self) {
        if self
            .state
            .as_ref()
            .and_then(|s| s.current_preset.as_ref())
            .is_some()
        {
            self.dirty = true;
        }
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
        self.dirty = false; // loading a preset replaces the current tuning
        let _ = self.cmd_tx.send(WorkerCmd::Import(path));
    }
}

/// A synthetic headphone measurement for `RESONANCE_DEMO=1`: 96 points log-spaced
/// 20 Hz → 20 kHz around an 84 dB baseline, with a low-frequency lift, a presence
/// hump near 2.7 kHz, and a sub-bass roll-off — a plausible-looking IEM curve for
/// the demo overlay.
fn demo_measurement() -> Vec<(f64, f64)> {
    (0..96)
        .map(|i| {
            let f = 20.0 * 1000f64.powf(f64::from(i) / 95.0); // 20 Hz → 20 kHz
            let l = f.log10();
            let db = 84.0
                + 4.0 / (1.0 + (f / 110.0).powi(2))
                + 3.0 * (-((l - 2700f64.log10()) / 0.22).powi(2)).exp()
                - 4.0 / (1.0 + (1500.0 / f).powi(3));
            (f, db)
        })
        .collect()
}

/// A representative populated `DaemonState` for `RESONANCE_DEMO=1` — lets the full
/// UI render (graph, bands, effects, devices, per-channel curves) with no daemon,
/// for the screenshot harness and design iteration. Values mirror the design mock.
fn demo_state() -> DaemonState {
    use resonance_ipc::{
        AppStream, BandState, BandType, ChannelMask, EffectsState, Meters, SinkVolume,
    };
    let band = |band_type, freq, gain_db, q, enabled, channels| BandState {
        band_type,
        freq,
        gain_db,
        q,
        enabled,
        channels,
        slope_db_oct: 12,
        scope: resonance_ipc::BandScope::Stereo,
        dynamics: None,
    };
    DaemonState {
        enabled: true,
        preamp_db: -10.2,
        eq_enabled: true,
        bands: vec![
            band(BandType::LowShelf, 60.0, 4.5, 0.71, true, ChannelMask::ALL),
            band(BandType::Peaking, 120.0, -2.1, 1.20, true, ChannelMask::ALL),
            band(BandType::Peaking, 280.0, -7.4, 1.40, true, ChannelMask::ALL),
            band(
                BandType::Peaking,
                1000.0,
                3.2,
                1.41,
                true,
                ChannelMask::single(0),
            ),
            band(
                BandType::Peaking,
                3500.0,
                -1.8,
                2.00,
                false,
                ChannelMask::single(1),
            ),
            band(
                BandType::HighShelf,
                9000.0,
                5.8,
                0.71,
                true,
                ChannelMask::ALL,
            ),
        ],
        effects: EffectsState {
            fidelity_intensity: 0.62,
            fidelity_enabled: true,
            ambience_intensity: 0.34,
            ambience_enabled: true,
            surround_intensity: 0.18,
            surround_enabled: false,
            dynamic_boost_intensity: 0.48,
            dynamic_boost_enabled: true,
            bass_intensity: 0.71,
            bass_enabled: true,
            loudness_intensity: 0.4,
            loudness_enabled: true,
            crossfeed_intensity: 0.25,
            crossfeed_enabled: true,
        },
        current_preset: Some("Reference".into()),
        sample_rate: 48000.0,
        capture_rate: 48000.0,
        channels: 2,
        out_channels: 2,
        channel_layout: resonance_ipc::default_channel_layout(2),
        routing: None,
        spectrum: (0..16)
            .map(|i| {
                let t = i as f32 / 15.0;
                (0.82 * (1.0 - t).powf(1.25)).max(0.05)
            })
            .collect(),
        active_output: Some("alsa_output.usb-D1".into()),
        mapped_profile: Some("Reference".into()),
        available_sinks: vec![
            "alsa_output.usb-D1".into(),
            "bluez.hd660s".into(),
            "bluez.wh1000xm5".into(),
        ],
        sink_descriptions: vec![
            ("alsa_output.usb-D1".into(), "D1 24-bit DAC".into()),
            ("bluez.hd660s".into(), "Sennheiser HD 660S".into()),
            ("bluez.wh1000xm5".into(), "WH-1000XM5".into()),
        ],
        preferred_output: None,
        meters: Meters {
            in_peak: 0.25,
            out_peak: 0.36,
            in_rms: 0.12,
            out_rms: 0.18,
            clip: false,
            dsp_load: 0.14,
            dsp_frame_us: 140,
        },
        apps: vec![
            AppStream {
                key: "firefox.41".into(),
                display_name: "Firefox".into(),
                pid: Some(4141),
                volume: 1.0,
                muted: false,
                active: true,
            },
            AppStream {
                key: "spotify.88".into(),
                display_name: "Spotify".into(),
                pid: Some(8800),
                volume: 0.7,
                muted: false,
                active: true,
            },
            AppStream {
                key: "discord.12".into(),
                display_name: "Discord".into(),
                pid: Some(1200),
                volume: 1.0,
                muted: true,
                active: false,
            },
        ],
        sinks: vec![
            SinkVolume {
                name: "alsa_output.pci-0000_00_1f.3.analog-stereo".into(),
                description: "Built-in Speakers".into(),
                volume: 0.85,
                muted: false,
            },
            SinkVolume {
                name: "bluez_output.AC_12_2F.1".into(),
                description: "WH-1000XM4".into(),
                volume: 0.6,
                muted: false,
            },
            SinkVolume {
                name: "alsa_output.pci-0000_5c_00.1.hdmi-surround71".into(),
                description:
                    "Radeon High Definition Audio Controller Digital Surround 7.1 (HDMI 3)".into(),
                volume: 1.0,
                muted: false,
            },
        ],
        dither_bits: None,
        convolution: None,
        solo_band: None,
        phase_mode_linear: false,
        eq_fir_latency_frames: 0,
    }
}

impl eframe::App for GuiApp {
    /// The GL clear colour shows through any gap the panels don't fill — notably
    /// the hero card's outer margin. eframe's default is near-black, which framed
    /// the deep FR graph in an odd black band; use the theme's window/ground tier
    /// so that gap matches the body background (as the control-card gaps already
    /// do) instead of reading as a black border around the graph.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.window_fill.to_normalized_gamma_f32()
    }

    /// Persist the chosen theme. Panel sizes are saved by eframe's egui-memory
    /// persistence (enabled by the `persistence` feature + default
    /// `persist_egui_memory`).
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("theme", self.theme.label().to_string());
        // Persist the reference overlay (measurement + target + customizer) so a
        // loaded measurement survives a restart, tied to the EQ it was built for.
        if let Ok(j) = serde_json::to_string(&self.reference.to_persisted()) {
            storage.set_string("reference", j);
        }
        storage.set_string("per_channel_eq", self.per_channel_eq.to_string());
        storage.set_string("show_slope", self.show_slope.to_string());
        storage.set_string("show_scope", self.show_scope.to_string());
        storage.set_string("show_dynamics", self.show_dynamics.to_string());
        storage.set_string("show_dither", self.show_dither.to_string());
        storage.set_string("show_ir", self.show_ir.to_string());
        if let Ok(j) = serde_json::to_string(&self.layout) {
            storage.set_string("card_layout", j);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Read the latest snapshot the IPC worker published (never blocks).
        self.pull_shared();

        self.handle_quit_guard(ui.ctx());
        self.run_window_migration(ui.ctx());
        #[cfg(target_os = "windows")]
        self.apply_native_titlebar();
        self.handle_keyboard(ui.ctx());
        self.pump_workers(ui.ctx());

        self.render_panels(ui);

        let ctx = ui.ctx().clone();
        self.render_dialogs(&ctx);

        // Drive ~144 fps repaint so spectrum/curve stay smooth.
        ctx.request_repaint_after(FRAME_INTERVAL);
    }
}

impl GuiApp {
    /// Save-before-quit guard plus the deferred close. If the EQ has unsaved
    /// edits, intercept the window close and offer to save them as a profile
    /// first; once a "Save & Quit" has had a moment to flush to the daemon,
    /// actually close the window.
    fn handle_quit_guard(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && self.dirty {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.pending_quit {
                self.pending_quit = true;
                self.quit_save_name = self
                    .state
                    .as_ref()
                    .and_then(|s| s.current_preset.clone())
                    .unwrap_or_default();
            }
        }
        if let Some(dl) = self.quit_deadline {
            if Instant::now() >= dl {
                self.quit_deadline = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// One-time window-state migration: older (CSD/borderless) builds persisted
    /// `fullscreen:true`/`maximized:true` plus panel sizes scaled to the full
    /// screen, which eframe restores into this decorated build — opening
    /// full-screen with no title bar and squashed panels. Force a normal window
    /// once; then, after it has resized (a few frames later), clear the stale
    /// panel sizes so they re-derive from the windowed height. Runs once, then
    /// the user's later window/panel choices are respected and re-persisted.
    fn run_window_migration(&mut self, ctx: &egui::Context) {
        use egui::containers::panel::PanelState;
        let migrated = ctx.data_mut(|d| {
            let id = egui::Id::new("window_state_migrated_v07");
            let done = d.get_persisted::<bool>(id).unwrap_or(false);
            d.insert_persisted(id, true);
            done
        });
        if !migrated {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
            self.migrate_settle = Some(0);
        }
        if let Some(n) = self.migrate_settle {
            if n >= 15 {
                // Window has settled to its windowed size; drop the stale panel
                // sizes so the proportional defaults re-apply to that size.
                self.migrate_settle = None;
                ctx.data_mut(|d| d.remove::<PanelState>(egui::Id::new("controls_panel")));
            } else {
                self.migrate_settle = Some(n + 1);
            }
        }
    }

    /// Tint the native Windows title bar to match the theme (re-apply on theme
    /// change; retry until the window handle exists).
    #[cfg(target_os = "windows")]
    fn apply_native_titlebar(&mut self) {
        if self.native_titlebar_theme != Some(self.theme) {
            let (caption, text) = self.theme.native_caption_colors();
            if native_titlebar::apply(!self.theme.is_light(), caption, text) {
                self.native_titlebar_theme = Some(self.theme);
            }
        }
    }

    /// Global keyboard shortcuts: Ctrl-Z undo, Ctrl-Y / Ctrl-Shift-Z redo, and
    /// F1 / `?` to toggle the help overlay (Esc closes it).
    fn handle_keyboard(&mut self, ctx: &egui::Context) {
        let (undo, redo) = ctx.input(|i| {
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
        ctx.input(|i| {
            if i.key_pressed(egui::Key::F1)
                || (i.key_pressed(egui::Key::Questionmark) && !i.modifiers.command)
            {
                self.show_help = !self.show_help;
            }
            if i.key_pressed(egui::Key::Escape) {
                self.show_help = false;
            }
        });
    }

    /// Per-frame background-worker housekeeping: drain service-worker results,
    /// pump download + Auto-EQ events, poll the daemon status on a slow timer,
    /// expire transient status feedback, and live-reload the Matugen theme.
    fn pump_workers(&mut self, ctx: &egui::Context) {
        // Drain service-worker results: each carries a fresh Status snapshot and
        // (for Run requests) a toolbar feedback string. Any result clears the
        // "busy" gate so further clicks can fire again.
        while let Ok(res) = self.service_rx.try_recv() {
            self.daemon_status = res.status;
            if let Some(msg) = res.feedback {
                self.set_status(msg);
            }
            self.service_busy = false;
        }

        // Drain measurement-downloader events (catalog/status/fetched curves).
        self.pump_downloads();
        // Apply any finished background Auto-EQ fit.
        self.pump_autoeq();

        // Service status drives the toolbar daemon controls; poll it on a slow
        // timer via the worker (off the UI thread so launchctl latency never
        // freezes egui).
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
                self.set_theme(ctx, Theme::Matugen);
            }
        }
    }

    /// Lay out the frame's panels: top toolbar, bottom status strip (only when
    /// connected), then the central shell.
    fn render_panels(&mut self, ui: &mut egui::Ui) {
        let toolbar = egui::Panel::top("toolbar").show_inside(ui, |ui| self.toolbar(ui));
        // macOS: keep the traffic lights centred in the toolbar (see module docs).
        #[cfg(target_os = "macos")]
        traffic_lights::center(f64::from(toolbar.response.rect.height()));
        #[cfg(not(target_os = "macos"))]
        let _ = toolbar;

        // The bottom status strip claims the window's bottom edge before `shell`
        // so the controls cluster stacks above it. Only when connected.
        if self.state.is_some() {
            egui::Panel::bottom("statusbar")
                .resizable(false)
                .show_inside(ui, |ui| self.status_bar(ui));
        }

        self.shell(ui);
    }

    /// Render all top-level dialogs / overlays for the frame (order preserved so
    /// later ones draw on top).
    fn render_dialogs(&mut self, ctx: &egui::Context) {
        self.preset_dialog(ctx);
        self.ir_dialog(ctx);
        self.export_dialog(ctx);
        self.confirm_dialog(ctx);
        self.help_dialog(ctx);
        self.settings_dialog(ctx);
        self.browse_dialog(ctx);
        self.manage_dialog(ctx);
        self.curve_picker_dialog(ctx);
        self.quit_dialog(ctx);
        self.status_toast(ctx);
    }
}

// ── UI sections ─────────────────────────────────────────────────────────────

impl GuiApp {
    /// Clear the persisted panel sizes so the resizable panels fall back to
    /// their defaults next frame.
    pub(crate) fn reset_layout(&mut self, ctx: &egui::Context) {
        use egui::containers::panel::PanelState;
        // Clear both resizable splitters (wide bottom strip + narrow top graph) so
        // the panels fall back to their 60/40 defaults regardless of which layout
        // is active when reset is pressed.
        ctx.data_mut(|d| {
            d.remove::<PanelState>(egui::Id::new("controls_panel"));
            d.remove::<PanelState>(egui::Id::new("graph_narrow"));
        });
        self.layout = CardLayout::default();
        self.set_status("layout reset");
    }

    /// A transient status toast floated at the bottom-centre of the window (e.g.
    /// "layout reset", "undo"), instead of cluttering the toolbar with a status
    /// label. Auto-hides once `status` is cleared by its TTL (see `ui`).
    fn status_toast(&self, ctx: &egui::Context) {
        if self.status.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("status_toast"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -18.0))
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    // Extend (don't wrap): the anchored area starts near-zero width,
                    // which otherwise breaks "layout reset" into one glyph per line.
                    ui.add(egui::Label::new(&self.status).wrap_mode(egui::TextWrapMode::Extend));
                });
            });
    }

    /// "Unsaved changes" prompt shown when the window is closed with dirty EQ
    /// edits: save them as a profile, discard them, or cancel and stay.
    fn quit_dialog(&mut self, ctx: &egui::Context) {
        if !self.pending_quit {
            return;
        }
        let mut open = true;
        dialog_window(ctx, "Unsaved changes")
            .id(egui::Id::new("quit_confirm"))
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("You have unsaved EQ changes. Save them as a profile before quitting?");
                ui.add_space(kit::SP_S);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("name").size(kit::T_CAPTION).weak());
                    kit::text_field(
                        ui,
                        220.0,
                        egui::Id::new("quit_save_name"),
                        &mut self.quit_save_name,
                        "profile name…",
                        false,
                    );
                });
                ui.add_space(kit::SP_S);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = kit::SP_S;
                    let name = self.quit_save_name.trim().to_string();
                    if kit::button_tip(
                        ui,
                        "Save & Quit",
                        true,
                        !name.is_empty(),
                        "Save the current EQ as this profile, then close",
                    ) {
                        self.queue(Command::SaveProfile { name });
                        self.needs_meta = true;
                        self.dirty = false;
                        self.pending_quit = false;
                        self.allow_close = true;
                        self.quit_deadline = Some(Instant::now() + Duration::from_millis(400));
                    }
                    if kit::button_tip(
                        ui,
                        "Discard & Quit",
                        false,
                        true,
                        "Close without saving — the unsaved tuning is lost",
                    ) {
                        self.dirty = false;
                        self.pending_quit = false;
                        self.allow_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if kit::button_tip(ui, "Cancel", false, true, "Keep the window open") {
                        self.pending_quit = false;
                    }
                });
            });
        // The window's own close button (X) acts as Cancel.
        if !open {
            self.pending_quit = false;
        }
    }

    /// Write the current chain to `path` as a `.toml` profile (round-trips via
    /// the daemon so it captures the authoritative state).
    pub(crate) fn export_profile(&mut self, path: String) {
        let _ = self.cmd_tx.send(WorkerCmd::Export(path));
    }

    /// Export a *named* stored profile (per-row export) rather than the live chain.
    pub(crate) fn export_profile_named(&mut self, name: String, path: String) {
        let _ = self.cmd_tx.send(WorkerCmd::ExportNamed(name, path));
    }

    /// Names advanced features that are hidden yet non-default, so nothing runs
    /// invisibly. `None` when every hidden feature is at its default.
    // `slope`/`scope` are deliberately parallel feature names.
    #[allow(clippy::similar_names)]
    pub(crate) fn advanced_active_hint(&self) -> Option<String> {
        let s = self.state.as_ref()?;
        // The per-band Ch column is visible on >2ch or when per-channel EQ is on.
        let ch_visible = s.channels > 2 || (self.per_channel_eq && s.channels >= 2);
        let dither = !self.show_dither && s.dither_bits.is_some();
        let ir = !self.show_ir && s.convolution.as_ref().is_some_and(|c| c.enabled);
        let slope = !self.show_slope
            && s.bands
                .iter()
                .any(|b| b.band_type.uses_slope() && b.slope_db_oct != 12);
        let scope = !self.show_scope
            && s.bands
                .iter()
                .any(|b| b.scope != resonance_ipc::BandScope::Stereo);
        let dynamics = !self.show_dynamics && s.bands.iter().any(|b| b.dynamics.is_some());
        let channels = !ch_visible && s.bands.iter().any(|b| !b.channels.is_global(s.channels));
        advanced_hint_label(dither, ir, slope, scope, dynamics, channels)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advanced_hint_label_lists_active_features() {
        assert_eq!(
            advanced_hint_label(false, false, false, false, false, false),
            None
        );
        assert_eq!(
            advanced_hint_label(true, false, false, true, false, false).as_deref(),
            Some("adv: dither scope")
        );
        assert_eq!(
            advanced_hint_label(false, false, false, false, true, false).as_deref(),
            Some("adv: dyn")
        );
        assert_eq!(
            advanced_hint_label(true, true, true, true, true, true).as_deref(),
            Some("adv: dither ir slope scope dyn channels")
        );
    }

    /// The demo measurement spans 20 Hz → 20 kHz over exactly 96 log-spaced
    /// points, strictly increasing in frequency, with finite dB values.
    #[test]
    #[allow(clippy::float_cmp)]
    fn demo_measurement_span_and_monotonic() {
        let m = demo_measurement();
        assert_eq!(m.len(), 96);
        assert_eq!(m.first().unwrap().0, 20.0);
        // Last point lands on 20 kHz (20 · 1000^1 = 20000) within float error.
        assert!((m.last().unwrap().0 - 20_000.0).abs() < 1e-6);
        for w in m.windows(2) {
            assert!(w[1].0 > w[0].0, "frequency must increase");
        }
        assert!(m.iter().all(|&(f, db)| f.is_finite() && db.is_finite()));
    }
}
