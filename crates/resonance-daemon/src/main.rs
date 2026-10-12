// On Windows, run as a GUI-subsystem process so launching the daemon never
// flashes a console window (it's a background service driving the APO).
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
// On Windows the daemon is control-plane only: it drives the in-graph APO and
// does no audio itself, so the RT audio path (chain application, meters,
// routing) and assorted PipeWire-only / virtual-cable helpers are compiled but
// never run. Allow the resulting dead code on Windows instead of cfg-gating
// every audio-path item individually.
#![cfg_attr(windows, allow(dead_code))]

mod audio;
mod config;
mod ipc_server;
mod ir;
mod meters;
mod shutdown;
mod spectrum;
mod state;

use anyhow::Result;
use config::{KnownSinks, Mappings, Profile};
use resonance_dsp::chain::ProcessorChain;
use rtrb::RingBuffer;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// Set up tracing. Defaults to `info` when `RUST_LOG` is unset so an
/// autostarted daemon still logs. On Windows the process is GUI-subsystem (no
/// console), so logs also go to a file — otherwise a crash/restart loop leaves
/// no trace at all. A panic hook routes panics into the same log.
fn init_logging() {
    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    #[cfg(windows)]
    let to_file = {
        let path = resonance_ipc::paths::daemon_log_path();
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(file) => {
                tracing_subscriber::fmt()
                    .with_env_filter(filter())
                    .with_ansi(false)
                    .with_writer(move || file.try_clone().expect("clone daemon log file"))
                    .init();
                true
            }
            Err(_) => false,
        }
    };
    #[cfg(not(windows))]
    let to_file = false;

    if !to_file {
        tracing_subscriber::fmt().with_env_filter(filter()).init();
    }

    // Route panics into the log too (the default hook writes to stderr, which is
    // discarded under the Windows GUI subsystem).
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        prev(info);
    }));
}

/// Measurement mode: `resonanced --measure-loopback <dev-substr> <out.raw> <secs>`
/// loopback-captures an output endpoint to raw f32le for spectral analysis.
/// Returns `Ok(true)` when the mode ran (the caller should exit), `Ok(false)`
/// when the flag was absent (continue normal startup).
#[cfg(target_os = "windows")]
fn handle_measure_loopback() -> Result<bool> {
    let args: Vec<String> = std::env::args().collect();
    let Some(p) = args.iter().position(|a| a == "--measure-loopback") else {
        return Ok(false);
    };
    let dev = args.get(p + 1).cloned().unwrap_or_default();
    let out = args.get(p + 2).cloned().unwrap_or_default();
    let secs = args.get(p + 3).and_then(|s| s.parse().ok()).unwrap_or(6);
    audio::measure_loopback(&dev, &out, secs)?;
    Ok(true)
}

/// Debug helper: `resonanced --list-apps` prints the per-application stream list
/// and exits, without starting the audio backend — on macOS this enumerates
/// Core Audio process objects with no Process Tap (so no TCC prompt and no audio
/// reroute). Linux enumerates in-graph, so use `resonance apps` against a live
/// daemon there. Returns `true` when the mode ran (caller should exit).
fn handle_list_apps() -> bool {
    if !std::env::args().any(|a| a == "--list-apps") {
        return false;
    }
    #[cfg(target_os = "macos")]
    let apps = audio::mac_apps::enumerate();
    #[cfg(target_os = "windows")]
    let apps = audio::win_apps::enumerate();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let apps: Vec<resonance_ipc::AppStream> = {
        eprintln!(
            "--list-apps: Linux enumerates in-graph; use `resonance apps` on a running daemon"
        );
        Vec::new()
    };
    for a in &apps {
        println!(
            "{} | {} | pid={:?} | vol={:.2} | muted={} | active={}",
            a.key, a.display_name, a.pid, a.volume, a.muted, a.active
        );
    }
    println!("({} apps)", apps.len());
    true
}

/// Debug helper for the per-output-sink volume control plane, without starting
/// the audio backend (on macOS/Windows this is a pure control-plane operation —
/// no TCC prompt, no audio reroute). Modes:
///   `--list-sinks`                    print output sinks (id | name | vol | muted)
///   `--set-sink <id> <percent>`       set one sink's volume (0–100)
///   `--mute-sink <id> <on|off>`       mute/unmute one sink
/// On Linux, sink volume is driven in-graph — use `resonance sinks` against a
/// live daemon. Returns `true` when a mode ran (caller should exit).
fn handle_sink_debug() -> bool {
    let args: Vec<String> = std::env::args().collect();
    let arg_after = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1..i + 3))
    };

    if args.iter().any(|a| a == "--list-sinks") {
        for s in &sink_debug_enumerate() {
            println!(
                "{} | {} | vol={:.2} | muted={}",
                s.name, s.description, s.volume, s.muted
            );
        }
        return true;
    }
    if let Some([id, pct]) = arg_after("--set-sink") {
        let vol = pct.parse::<f64>().unwrap_or(f64::NAN) / 100.0;
        if vol.is_finite() {
            println!(
                "set-sink {id} -> {:.0}%: {}",
                vol * 100.0,
                sink_debug_set_volume(id, vol)
            );
        } else {
            eprintln!("--set-sink: percent must be a number 0..=100");
        }
        return true;
    }
    if let Some([id, state]) = arg_after("--mute-sink") {
        let muted = matches!(state.as_str(), "on" | "1" | "true" | "yes");
        println!(
            "mute-sink {id} -> {muted}: {}",
            sink_debug_set_mute(id, muted)
        );
        return true;
    }
    false
}

/// Enumerate output sinks for the `--list-sinks` debug mode (control-plane only).
#[cfg(target_os = "macos")]
fn sink_debug_enumerate() -> Vec<resonance_ipc::SinkVolume> {
    audio::mac_sinks::enumerate_output_sinks()
}
#[cfg(target_os = "windows")]
fn sink_debug_enumerate() -> Vec<resonance_ipc::SinkVolume> {
    audio::win_sinks::enumerate()
}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn sink_debug_enumerate() -> Vec<resonance_ipc::SinkVolume> {
    eprintln!(
        "--list-sinks: Linux drives sink volume in-graph; use `resonance sinks` on a live daemon"
    );
    Vec::new()
}

/// Set a sink's volume by id for the `--set-sink` debug mode.
#[cfg(target_os = "macos")]
fn sink_debug_set_volume(id: &str, volume: f64) -> bool {
    audio::mac_sinks::set_volume_by_uid(id, volume)
}
#[cfg(target_os = "windows")]
fn sink_debug_set_volume(id: &str, volume: f64) -> bool {
    audio::win_sinks::set_volume(id, volume)
}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn sink_debug_set_volume(_id: &str, _volume: f64) -> bool {
    eprintln!("--set-sink: unsupported on Linux (use `resonance sink … volume`)");
    false
}

/// Mute/unmute a sink by id for the `--mute-sink` debug mode.
#[cfg(target_os = "macos")]
fn sink_debug_set_mute(id: &str, muted: bool) -> bool {
    audio::mac_sinks::set_mute_by_uid(id, muted)
}
#[cfg(target_os = "windows")]
fn sink_debug_set_mute(id: &str, muted: bool) -> bool {
    audio::win_sinks::set_mute(id, muted)
}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn sink_debug_set_mute(_id: &str, _muted: bool) -> bool {
    eprintln!("--mute-sink: unsupported on Linux (use `resonance sink … mute`)");
    false
}

/// Acquire the single-instance lock. Returns `Ok(true)` when this process owns
/// it (continue startup), `Ok(false)` when another live daemon already holds it.
///
/// A duplicate launch (e.g. launchd racing a manual start) exits *cleanly*
/// (status 0) and does NOT touch the other daemon's socket/pidfile — a crash
/// here would be relaunched in a throttled loop by `KeepAlive { SuccessfulExit
/// = false }`.
fn acquire_singleton_or_exit() -> Result<bool> {
    match shutdown::acquire_singleton() {
        Ok(shutdown::Singleton::Acquired) => Ok(true),
        Ok(shutdown::Singleton::AlreadyRunning) => {
            info!("another resonanced already holds the single-instance lock; exiting");
            Ok(false)
        }
        Err(e) => anyhow::bail!("single-instance lock: {e}"),
    }
}

/// Spawn the spectrum-computation task: drains the post-DSP sample ring and
/// publishes analyzer bins into shared state.
fn spawn_spectrum_task(spectrum_rx: rtrb::Consumer<f32>, shared: &state::SharedState) {
    let spectrum_state = shared.clone();
    tokio::spawn(async move {
        spectrum::run(spectrum_rx, spectrum_state).await;
    });
}

/// Spawn the output-device change task: when the backend reports a new real sink,
/// record it as the active output and auto-load the profile mapped to it (if any).
fn spawn_output_mapping_task(
    mut output_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    shared: &state::SharedState,
) {
    let output_state = shared.clone();
    tokio::spawn(async move {
        while let Some(output) = output_rx.recv().await {
            info!("active output changed: {output}");
            let mapped = Mappings::load().get(&output).map(str::to_owned);
            {
                let mut inner = output_state.0.lock().unwrap();
                inner.active_output = Some(output.clone());
                inner.mapped_profile.clone_from(&mapped);
            }
            if let Some(name) = mapped {
                match apply_profile(&name, &output_state) {
                    Ok(()) => {
                        info!("auto-loaded profile '{name}' for output '{output}'");
                        output_state.0.lock().unwrap().current_preset = Some(name);
                    }
                    Err(e) => warn!("auto-load profile '{name}' failed: {e}"),
                }
            }
        }
    });
}

/// Spawn the available-sinks update task: keep shared state in sync with the
/// backend's view of the graph, folding freshly-seen sinks into the persistent
/// known-device registry so descriptions survive a device being unplugged.
fn spawn_sinks_task(
    mut sinks_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<(String, String)>>,
    shared: &state::SharedState,
) {
    let sinks_state = shared.clone();
    tokio::spawn(async move {
        while let Some(sinks) = sinks_rx.recv().await {
            let mut known = KnownSinks::load();
            let mut changed = false;
            for (name, desc) in &sinks {
                changed |= known.remember(name.clone(), desc.clone());
            }
            if changed {
                if let Err(e) = known.save() {
                    warn!("could not persist known sinks: {e}");
                }
            }
            // Descriptions = present sinks first, then any remembered device not
            // currently present, so mappings for absent devices still get a name.
            let mut descriptions = sinks.clone();
            for (name, desc) in known.list() {
                if !descriptions.iter().any(|(n, _)| n == &name) {
                    descriptions.push((name, desc));
                }
            }
            let mut inner = sinks_state.0.lock().unwrap();
            inner.available_sinks = sinks.iter().map(|(name, _)| name.clone()).collect();
            inner.sink_descriptions = descriptions;
            // Keep mapped_profile in sync with the active output's mapping; with
            // several devices present the active sink can be resolved here before
            // an output-change event fires, so reconcile it from disk.
            if let Some(active) = inner.active_output.clone() {
                inner.mapped_profile = Mappings::load().get(&active).map(str::to_owned);
            }
        }
    });
}

/// Drain the live per-application stream list pushed by the backend (or a
/// platform enumeration task) and mirror it into shared state for clients.
fn spawn_apps_task(
    mut apps_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<resonance_ipc::AppStream>>,
    shared: &state::SharedState,
) {
    let apps_state = shared.clone();
    tokio::spawn(async move {
        while let Some(apps) = apps_rx.recv().await {
            apps_state.set_apps(apps);
        }
    });
}

/// Drain the live output-sink volume list pushed by the backend into shared state.
fn spawn_sinks_vol_task(
    mut sinks_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<resonance_ipc::SinkVolume>>,
    shared: &state::SharedState,
) {
    let sinks_state = shared.clone();
    tokio::spawn(async move {
        while let Some(sinks) = sinks_rx.recv().await {
            sinks_state.set_sinks(sinks);
        }
    });
}

/// Initialise the Windows control plane: open the APO state bridge, start the
/// telemetry pump (mirrors APO meters/spectrum into shared state for clients),
/// and start the device-watch thread that auto-attaches the APO to every
/// appearing render endpoint and tracks the system default.
#[cfg(target_os = "windows")]
fn init_windows_control_plane(
    shared: &state::SharedState,
    output_tx: tokio::sync::mpsc::UnboundedSender<String>,
) {
    let path = resonance_apo::state::default_state_path();
    match resonance_apo::state::ApoStateWriter::create(&path) {
        Ok(writer) => {
            shared.set_apo_writer(writer);
            info!("APO control bridge ready at {}", path.display());
        }
        Err(e) => warn!("APO control bridge unavailable ({}): {e}", path.display()),
    }

    let tele_state = shared.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(30));
        loop {
            tick.tick().await;
            tele_state.pump_telemetry();
        }
    });

    // Device list: enumerate render endpoints so clients can pick an output;
    // also track the current system default. Runs on a dedicated thread — the
    // COM calls block, and must never stall the async IPC runtime.
    let dev_state = shared.clone();
    std::thread::spawn(move || {
        // Render endpoints we've already auto-attached the APO to this run.
        // Poll-and-diff (not IMMNotificationClient OnDeviceAdded, which only
        // fires for never-before-seen devices — re-connected DACs/BT slip
        // through it) so EVERY newly-appearing endpoint gets the APO: a
        // hot-plugged DAC or a Bluetooth headset is attached on the spot,
        // without re-running the installer.
        let mut attached: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut last_default: Option<String> = None;
        loop {
            let endpoints = audio::win_devices::enumerate_render_endpoints();
            let default = audio::win_devices::default_render_id();
            for (id, _name) in &endpoints {
                if let Some(guid) = audio::win_devices::endpoint_guid(id) {
                    if attached.insert(guid.to_string()) {
                        let r = audio::win_devices::attach_apo_endpoint(guid);
                        info!("APO auto-attach {guid}: {r}");
                    }
                }
            }
            // Feed the (cross-platform) output-mapping task: first sight at
            // startup counts as a change, which is what auto-applies the
            // device's mapped profile after a daemon restart.
            if default.is_some() && default != last_default {
                last_default.clone_from(&default);
                if let Some(id) = default.clone() {
                    let _ = output_tx.send(id);
                }
            }
            {
                let mut inner = dev_state.0.lock().unwrap();
                inner.available_sinks = endpoints.iter().map(|(id, _)| id.clone()).collect();
                inner.sink_descriptions = endpoints;
                inner.active_output = default;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });
}

// A single-threaded runtime is plenty: the daemon's async side is all low-rate
// IPC and event handling, and the only real-time work (the audio callback) runs
// on its own dedicated OS thread, not on tokio. The default multi-thread runtime
// would otherwise spawn one worker per core — wasted stacks and idle scheduler
// wakeups for no latency benefit. Blocking work (preset parsing) still uses
// spawn_blocking's separate pool.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    init_logging();

    info!("resonanced starting");

    // Measurement mode short-circuits before the pidfile/IPC so it can run
    // alongside a live daemon.
    #[cfg(target_os = "windows")]
    if handle_measure_loopback()? {
        return Ok(());
    }

    // Debug: print the per-app list without starting the backend (no tap/TCC).
    if handle_list_apps() {
        return Ok(());
    }

    // Debug: inspect/set output-sink volume without starting the backend
    // (control-plane only — no tap/TCC/reroute).
    if handle_sink_debug() {
        return Ok(());
    }

    // Single-instance guard: a duplicate launch is a clean no-op, not a crash.
    if !acquire_singleton_or_exit()? {
        return Ok(());
    }

    let (cmd_tx, cmd_rx) = RingBuffer::<state::AudioCommand>::new(256);
    let (spectrum_tx, spectrum_rx) = RingBuffer::<f32>::new(audio::SPECTRUM_BUF);
    let (route_tx, route_rx) = std::sync::mpsc::channel::<String>();
    let (sinks_tx, sinks_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<(String, String)>>();
    let (output_tx, output_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    // Per-app control (IPC → backend/control task) + the live app list
    // (backend/enumeration task → IPC), mirroring route_tx/sinks_tx.
    let (app_ctl_tx, app_ctl_rx) = std::sync::mpsc::channel::<state::AppControl>();
    let (apps_tx, apps_rx) =
        tokio::sync::mpsc::unbounded_channel::<Vec<resonance_ipc::AppStream>>();
    // Per-output-sink control + the live sink-volume list (same shape as apps).
    let (sink_ctl_tx, sink_ctl_rx) = std::sync::mpsc::channel::<state::SinkCtl>();
    let (sinks_vol_tx, sinks_vol_rx) =
        tokio::sync::mpsc::unbounded_channel::<Vec<resonance_ipc::SinkVolume>>();

    let initial_chain = ProcessorChain::builder()
        .channels(audio::target_channels())
        .sample_rate(48000.0)
        .build();

    let meters = std::sync::Arc::new(meters::AtomicMeters::default());
    let shared = state::SharedState::new(cmd_tx, route_tx, meters.clone(), app_ctl_tx, sink_ctl_tx);
    shutdown::install_signal_handlers(&shared);

    spawn_spectrum_task(spectrum_rx, &shared);
    spawn_output_mapping_task(output_rx, &shared);
    spawn_sinks_task(sinks_rx, &shared);
    spawn_apps_task(apps_rx, &shared);
    spawn_sinks_vol_task(sinks_vol_rx, &shared);

    // Audio backend on a dedicated RT thread (Linux/PipeWire, macOS/CoreAudio).
    // On Windows the daemon does no audio: the in-graph APO owns the DSP and the
    // daemon is the control plane — it publishes chain state to the shared file
    // the APO reads inside audiodg.exe.
    #[cfg(not(target_os = "windows"))]
    let pw_handle = audio::spawn(audio::BackendCtx {
        cmd_rx,
        spectrum_tx,
        initial_chain,
        output_tx,
        route_rx,
        sinks_tx,
        meters,
        apps_tx,
        app_ctl_rx,
        sinks_vol_tx,
        sink_ctl_rx,
    })?;

    #[cfg(target_os = "windows")]
    {
        // Only the (skipped) audio backend consumes these on Windows.
        let _ = (
            cmd_rx,
            spectrum_tx,
            initial_chain,
            route_rx,
            sinks_tx,
            meters,
        );
        // Per-output-sink volume: enumerate WASAPI render endpoints + apply
        // volume/mute on dedicated COM threads, mirroring the per-app plane.
        // The enumeration thread feeds `sinks_vol_tx` → `spawn_sinks_vol_task`
        // → shared state, like the audio backends do on Linux/macOS.
        audio::win_sinks::spawn_sink_tasks(sinks_vol_tx, sink_ctl_rx);
        // Per-app control plane: enumerate WASAPI sessions + apply volume/mute
        // on dedicated COM threads. The enumeration thread feeds `apps_tx` →
        // `spawn_apps_task` → shared state, like the audio backends do.
        audio::win_apps::spawn_app_tasks(apps_tx, app_ctl_rx);
        init_windows_control_plane(&shared, output_tx);
    }

    // IPC server (blocks until shutdown)
    ipc_server::run(shared).await?;

    #[cfg(not(target_os = "windows"))]
    pw_handle.join().ok();

    shutdown::cleanup();
    info!("resonanced stopped");
    Ok(())
}

/// Load a named profile and swap it onto the chain (used by the output-mapping
/// task). Goes through the shared profile apply so the convolution IR (if the
/// profile carries one) is restored too.
fn apply_profile(name: &str, state: &state::SharedState) -> Result<(), String> {
    let profile = Profile::load(name)?;
    ipc_server::apply_profile_chain(&profile, state);
    Ok(())
}
