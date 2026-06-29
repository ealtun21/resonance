//! `PipeWire` backend — mirrors `FxSound` `AudioPassthruPipeWire` architecture:
//!   1. null-audio-sink "Resonance EQ" — routable device; apps play into it.
//!   2. `pw_filter` "Resonance EQ Processor" — N-in/N-out (channel count from
//!      `target_channels()`, default stereo; override `RESONANCE_CHANNELS`),
//!      driven by the same clock as the sink (no ring-buffer choppy artefacts).
//!   3. Registry listener creates links: sink-monitor → filter-in,
//!      filter-out → real device, paired by channel label (positional fallback).
//!   4. `WirePlumber` metadata sets "Resonance EQ" as system default sink.

use super::{SAMPLE_RATE, apply_command, target_channels};
use crate::meters::{AtomicMeters, Sample, peak_rms, peak_rms_f32};
use crate::state::AudioCommand;
use anyhow::Result;
use pipewire as pw;
use pipewire::properties::properties;
use pipewire_sys as pw_sys;
use pw::spa;
use resonance_dsp::chain::ProcessorChain;
use spa_sys::{SPA_DIRECTION_INPUT, SPA_DIRECTION_OUTPUT, spa_hook, spa_io_position};
use std::{
    collections::HashMap,
    os::raw::c_void,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tracing::info;

// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct PortMeta {
    id: u32,
    node_id: u32,
    is_output: bool,
    is_monitor: bool,
    channel: String,
}

struct NodeMeta {
    media_class: String,
    name: String,
    description: String,
}

struct GraphState {
    raw_core: usize, // *mut pw_sys::pw_core stored as usize for Send
    sink_node_id: u32,
    filter_node_id: u32,
    nodes: HashMap<u32, NodeMeta>,
    ports: HashMap<u32, PortMeta>,
    out_links: Vec<pw::link::Link>,
    monitor_links: Vec<pw::link::Link>,
    metadata_obj: Option<pw::metadata::Metadata>,
    /// Listener that captures the pre-existing default sink from metadata.
    metadata_listener: Option<pw::metadata::MetadataListener>,
    /// `PipeWire`'s default sink name *before* Resonance took over — the fallback
    /// downstream target. Written once by the metadata listener, read in `reroute`.
    original_default: Arc<Mutex<Option<String>>>,
    /// The latest *real* default sink the user/WirePlumber selected (≠ our own
    /// "resonance"). Updated live by the metadata listener; when not pinned, this
    /// is the device Resonance follows, so picking an output in the system
    /// switcher (or a BT/headset hot-plug) retargets us. See `find_target_sink`.
    live_default: Arc<Mutex<Option<String>>>,
    /// Set by the metadata listener when the real system default changed, so the
    /// loop timer re-evaluates the target (reroute) and re-claims default for
    /// "resonance" (keeping apps feeding the EQ). Self-claims are filtered out.
    default_dirty: Arc<AtomicBool>,
    default_set: bool,
    /// Reports the node.name of the real sink Resonance currently feeds, on change.
    output_tx: tokio::sync::mpsc::UnboundedSender<String>,
    /// Last output name reported (dedupe).
    last_output: Option<String>,
    /// Preferred output node name; set via IPC command `SetOutputTarget`.
    preferred_output: Option<String>,
    /// Receives preferred-output updates from the IPC thread.
    route_rx: std::sync::mpsc::Receiver<String>,
    /// Sends the current set of available sink names to the daemon state.
    sinks_tx: tokio::sync::mpsc::UnboundedSender<Vec<(String, String)>>,
    /// Channel count the live filter is built for (= the `FilterData` port count).
    /// Set at filter setup; compared against the output device's channel count
    /// each timer tick to drive live channel-count following.
    active_channels: usize,
    /// When the output device's channel count changes, the timer records the new
    /// count here and quits the loop; the reconnect path rebuilds the filter +
    /// chain at this width, then clears it.
    pending_channels: Option<usize>,
    /// Debounce for device-channel detection: the count last seen + how many
    /// consecutive 50 ms ticks it has held. A rebuild only triggers once the
    /// count is stable, so transient counts during device enumeration (ports
    /// appearing one-by-one) don't cause spurious rebuilds.
    device_seen_count: usize,
    device_seen_ticks: u8,
}

// SAFETY: only touched from the pw main-loop thread.
unsafe impl Send for GraphState {}

struct FilterData {
    /// One DSP buffer pointer per channel (length = processing channel count).
    in_ports: Vec<*mut c_void>,
    out_ports: Vec<*mut c_void>,
    chain: ProcessorChain,
    cmd_rx: rtrb::Consumer<AudioCommand>,
    spectrum_tx: rtrb::Producer<f32>,
    /// Reusable interleaved f64 scratch buffer — avoids allocating every RT callback.
    scratch: Vec<f64>,
    /// Second interleaved scratch for the routing matrix output (square remap).
    routed: Vec<f64>,
    /// Live meters published to the IPC thread.
    meters: Arc<AtomicMeters>,
    /// Last sample rate logged (so the *actual* negotiated graph rate is logged
    /// once when it settles/changes, not the requested constant).
    logged_rate: f64,
}

/// Pre-allocated scratch capacity: a generous upper bound on the `PipeWire` quantum
/// (frames × channels). Growth in the RT callback past this is rare and handled.
const MAX_QUANTUM: usize = 8192;
/// Hard ceiling on channels the RT callback gathers onto the stack (matches
/// `resonance_dsp::channel::MAX_CHANNELS`); `target_channels()` already clamps to it.
const MAX_CH: usize = 64;

// SAFETY: only touched from pw_filter process callback (RT thread).
unsafe impl Send for FilterData {}

// ─────────────────────────────────────────────────────────────────────────────

pub fn spawn(ctx: super::BackendCtx) -> Result<JoinHandle<()>> {
    let super::BackendCtx {
        cmd_rx,
        spectrum_tx,
        initial_chain,
        output_tx,
        route_rx,
        sinks_tx,
        meters,
    } = ctx;
    // Channel count is fixed for the daemon's lifetime (env override or stereo).
    // The chain may arrive built at a different width; force it to match the ports.
    let channels = target_channels();
    let mut initial_chain = initial_chain;
    initial_chain.set_channels(channels);

    // FilterData and GraphState persist across reconnects — the audio chain (EQ
    // state) and the daemon-facing channels must survive a PipeWire restart,
    // since they're paired with producers/receivers the daemon already holds.
    let mut fd = Box::new(FilterData {
        in_ports: vec![std::ptr::null_mut(); channels],
        out_ports: vec![std::ptr::null_mut(); channels],
        chain: initial_chain,
        cmd_rx,
        spectrum_tx,
        scratch: vec![0.0; MAX_QUANTUM * channels],
        routed: vec![0.0; MAX_QUANTUM * channels],
        meters,
        logged_rate: 0.0,
    });
    let gs = Arc::new(Mutex::new(GraphState {
        raw_core: 0,
        sink_node_id: u32::MAX,
        filter_node_id: u32::MAX,
        nodes: HashMap::new(),
        ports: HashMap::new(),
        out_links: Vec::new(),
        monitor_links: Vec::new(),
        metadata_obj: None,
        metadata_listener: None,
        original_default: Arc::new(Mutex::new(None)),
        live_default: Arc::new(Mutex::new(None)),
        default_dirty: Arc::new(AtomicBool::new(false)),
        default_set: false,
        output_tx,
        last_output: None,
        preferred_output: None,
        route_rx,
        sinks_tx,
        active_channels: channels,
        pending_channels: None,
        device_seen_count: 0,
        device_seen_ticks: 0,
    }));

    Ok(thread::Builder::new()
        .name("resonance-pw".into())
        .spawn(move || {
            pw::init();
            let mut backoff = Duration::from_millis(200);
            loop {
                // Live channel-count following: if the output device's channel
                // count changed, the timer recorded it and quit the loop — rebuild
                // the FilterData (ports, scratch) + chain at the new width here,
                // before the next connection attempt re-creates the filter from
                // `fd.in_ports.len()`. `set_channels` clamps per-band channel masks.
                if let Some(n) = gs.lock().unwrap().pending_channels.take() {
                    if n != fd.in_ports.len() && (1..=MAX_CH).contains(&n) {
                        fd.in_ports = vec![std::ptr::null_mut(); n];
                        fd.out_ports = vec![std::ptr::null_mut(); n];
                        fd.scratch = vec![0.0; MAX_QUANTUM * n];
                        fd.routed = vec![0.0; MAX_QUANTUM * n];
                        fd.chain.set_channels(n);
                        fd.logged_rate = 0.0;
                        tracing::info!("output device is now {n}ch — rebuilt the DSP chain");
                    }
                }
                // Reset the per-attempt graph view (the persistent channels +
                // user preferences + the captured original default are kept).
                {
                    let mut g = gs.lock().unwrap();
                    g.sink_node_id = u32::MAX;
                    g.filter_node_id = u32::MAX;
                    g.nodes.clear();
                    g.ports.clear();
                    g.out_links.clear();
                    g.monitor_links.clear();
                    g.metadata_obj = None;
                    g.metadata_listener = None;
                    g.default_set = false;
                    g.last_output = None;
                }
                fd.in_ports
                    .iter_mut()
                    .for_each(|p| *p = std::ptr::null_mut());
                fd.out_ports
                    .iter_mut()
                    .for_each(|p| *p = std::ptr::null_mut());
                let fd_ptr: *mut FilterData = &raw mut *fd;

                let started = Instant::now();
                match build_and_run(fd_ptr, &gs) {
                    Ok(()) => tracing::warn!("PipeWire connection lost; reconnecting…"),
                    Err(e) => tracing::warn!("PipeWire setup failed: {e:#}; retrying…"),
                }

                // A session that lived a while is a real reconnect, not a flap:
                // reset the backoff so the next outage retries promptly.
                if started.elapsed() > Duration::from_secs(10) {
                    backoff = Duration::from_millis(200);
                }
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        })?)
}

/// Build the `PipeWire` graph for one connection attempt and run its main loop
/// until the connection drops (core error → quit). Returns `Ok` on a clean
/// disconnect so the caller can reconnect; `Err` if setup failed.
fn build_and_run(fd_ptr: *mut FilterData, gs: &Arc<Mutex<GraphState>>) -> Result<()> {
    let mainloop = pw::main_loop::MainLoopBox::new(None)?;
    let context = pw::context::ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect(None)?;

    let raw_core = core.as_raw_ptr();
    gs.lock().unwrap().raw_core = raw_core as usize;

    let quit_ptr = mainloop.as_raw_ptr() as usize;
    let _core_listener = register_core_error_listener(&core, quit_ptr);

    // ── pw_filter (raw FFI) + its DSP ports ───────────────────────────────
    let filter = create_filter(raw_core, fd_ptr)?;

    // Channel count + layout for this connection — fixed at the FilterData's port
    // count (set in `spawn` from `target_channels()`). Reused for the sink's
    // `audio.position` and the ready log below.
    //
    // SAFETY: take one `&mut` to the FilterData for port setup. Nothing else
    // aliases it here — the RT process callback is not invoked until the filter is
    // connected (below) and the main loop runs. The borrow is dropped before then.
    let fd = unsafe { &mut *fd_ptr };
    let channels = fd.in_ports.len();
    gs.lock().unwrap().active_channels = channels;
    let names = pw_channel_names(channels);
    let position = names.join(",");

    add_filter_ports(filter, fd, &names);
    connect_filter(filter)?;

    create_null_sink(&core, &position)?;

    // ── Registry listener (high-level, 'static closure via Arc) ──────────
    let registry = core.get_registry()?;
    // SAFETY: `core` and `registry` outlive the listener (same scope, dropped after mainloop).
    // The transmute extends the RegistryBox lifetime to 'static for the closure — safe because
    // the listener is dropped before registry (Rust drops in reverse declaration order).
    let registry_static: &'static pw::registry::Registry = unsafe {
        std::mem::transmute::<&pw::registry::Registry, &'static pw::registry::Registry>(&*registry)
    };
    let _listener = register_graph_listener(registry_static, gs);

    // ── Poll filter node-id, then run main loop ───────────────────────────
    let quit_timer = mainloop.as_raw_ptr() as usize;
    let timer = spawn_graph_timer(&mainloop, gs, filter, quit_timer)?;

    info!(
        "PipeWire ready — 'Resonance EQ' is now the default output ({}ch [{}], requested {} Hz; \
         actual graph rate logged once negotiated)",
        channels, position, SAMPLE_RATE
    );

    // Blocks until the connection drops (core error → pw_main_loop_quit).
    mainloop.run();

    // Detach the RT callback from `fd` before this attempt's objects are dropped
    // and `fd` is reused by the next attempt.
    unsafe {
        pw_sys::pw_filter_destroy(filter);
    }
    drop(timer);
    Ok(())
}

/// Wire a listener that quits the main loop on a fatal core error (server gone /
/// disconnect) so the reconnect loop can take over. `quit_ptr` is the
/// `pw_main_loop` raw pointer as `usize` (to keep the closure `Send`).
fn register_core_error_listener(core: &pw::core::Core, quit_ptr: usize) -> pw::core::Listener {
    core.add_listener_local()
        .error(move |id, _seq, res, message| {
            // id 0 == the core proxy itself; an error there means the connection
            // is broken.
            if id == 0 {
                tracing::warn!("PipeWire core error (res={res}): {message}");
                unsafe {
                    pw_sys::pw_main_loop_quit(quit_ptr as *mut pw_sys::pw_main_loop);
                }
            }
        })
        .register()
}

/// Create the `pw_filter` node and attach the RT process callback, pointing it at
/// `fd_ptr`. The events struct and hook are intentionally leaked: they must
/// outlive the filter, which is destroyed (not dropped) at the end of the
/// connection attempt. Returns the raw filter pointer.
fn create_filter(
    raw_core: *mut pw_sys::pw_core,
    fd_ptr: *mut FilterData,
) -> Result<*mut pw_sys::pw_filter> {
    let fev: &'static _ = Box::leak(Box::new(unsafe {
        let mut e: pw_sys::pw_filter_events = std::mem::zeroed();
        e.version = pw_sys::PW_VERSION_FILTER_EVENTS;
        e.process = Some(filter_process_cb);
        e
    }));

    let filter = unsafe {
        let props = pw_props_raw(&[
            ("media.type", "Audio"),
            ("media.category", "Filter"),
            ("media.role", "DSP"),
            ("node.name", "resonance-processor"),
            ("node.description", "Resonance EQ Processor"),
            ("node.autoconnect", "false"),
            ("node.virtual", "true"),
        ]);
        let f = pw_sys::pw_filter_new(raw_core, c"resonance-dsp".as_ptr(), props);
        anyhow::ensure!(!f.is_null(), "pw_filter_new");
        let hook = Box::leak(Box::new(std::mem::zeroed::<spa_hook>()));
        pw_sys::pw_filter_add_listener(f, hook, fev, fd_ptr.cast::<c_void>());
        f
    };
    Ok(filter)
}

/// Add one mono-float input + output DSP port per channel to `filter`, storing
/// the port handles in `fd.in_ports`/`fd.out_ports`. Ports carry the channel's
/// SPA position label (FL/FR/…) so the link layer can pair them positionally.
fn add_filter_ports(filter: *mut pw_sys::pw_filter, fd: &mut FilterData, names: &[String]) {
    for (ch, chname) in names.iter().enumerate() {
        let chname = chname.as_str();
        let inname = format!("input_{chname}");
        let outname = format!("output_{chname}");
        fd.in_ports[ch] = unsafe {
            pw_sys::pw_filter_add_port(
                filter,
                SPA_DIRECTION_INPUT,
                pw_sys::pw_filter_port_flags_PW_FILTER_PORT_FLAG_MAP_BUFFERS,
                std::mem::size_of::<u64>(),
                pw_props_raw(&[
                    ("format.dsp", "32 bit float mono audio"),
                    ("port.name", inname.as_str()),
                    ("audio.channel", chname),
                ]),
                std::ptr::null_mut(),
                0,
            )
        };
        fd.out_ports[ch] = unsafe {
            pw_sys::pw_filter_add_port(
                filter,
                SPA_DIRECTION_OUTPUT,
                pw_sys::pw_filter_port_flags_PW_FILTER_PORT_FLAG_MAP_BUFFERS,
                std::mem::size_of::<u64>(),
                pw_props_raw(&[
                    ("format.dsp", "32 bit float mono audio"),
                    ("port.name", outname.as_str()),
                    ("audio.channel", chname),
                ]),
                std::ptr::null_mut(),
                0,
            )
        };
    }
}

/// Connect the filter into the graph as an RT-process node.
fn connect_filter(filter: *mut pw_sys::pw_filter) -> Result<()> {
    anyhow::ensure!(
        unsafe {
            pw_sys::pw_filter_connect(
                filter,
                pw_sys::pw_filter_flags_PW_FILTER_FLAG_RT_PROCESS,
                std::ptr::null_mut(),
                0,
            )
        } >= 0,
        "pw_filter_connect"
    );
    Ok(())
}

/// Create the routable "Resonance EQ" null-audio-sink that apps play into.
/// `position` is the comma-joined SPA channel layout (e.g. `FL,FR`).
fn create_null_sink(core: &pw::core::Core, position: &str) -> Result<()> {
    let _sink: pw::node::Node = core.create_object(
        "adapter",
        &properties! {
            "factory.name"           => "support.null-audio-sink",
            "node.name"              => "resonance",
            "node.description"       => "Resonance EQ",
            "media.class"            => "Audio/Sink",
            "audio.position"         => position,
            "monitor.channel-volumes" => "false",
            "monitor.passthrough"    => "true",
            "node.virtual"           => "true",
        },
    )?;
    Ok(())
}

/// Register the registry global / global-remove listener that drives node + port
/// discovery and rerouting. The `'static` registry reference must outlive the
/// returned listener (the caller drops the listener before the registry).
fn register_graph_listener(
    registry_static: &'static pw::registry::Registry,
    gs: &Arc<Mutex<GraphState>>,
) -> pw::registry::Listener {
    let gs_global = Arc::clone(gs);
    let gs_remove = Arc::clone(gs);
    registry_static
        .add_listener_local()
        .global(move |obj| {
            let mut g = gs_global.lock().unwrap();
            on_global(&mut g, registry_static, obj);
        })
        .global_remove(move |id| {
            let mut g = gs_remove.lock().unwrap();
            on_global_remove(&mut g, id);
        })
        .register()
}

/// Add the 50 ms loop timer that polls the filter node-id, applies pending
/// route/default changes, and follows the output device's channel count.
/// `quit_ptr` is the `pw_main_loop` raw pointer as `usize`. The returned
/// `TimerSource` borrows `mainloop` and must be dropped before it.
fn spawn_graph_timer<'l>(
    mainloop: &'l pw::main_loop::MainLoopBox,
    gs: &Arc<Mutex<GraphState>>,
    filter: *mut pw_sys::pw_filter,
    quit_ptr: usize,
) -> Result<pw::loop_::TimerSource<'l>> {
    let gs_timer = Arc::clone(gs);
    let filter_ptr = filter as usize;
    let timer = mainloop.loop_().add_timer(move |_| {
        let node_id =
            unsafe { pw_sys::pw_filter_get_node_id(filter_ptr as *mut pw_sys::pw_filter) };
        let mut g = gs_timer.lock().unwrap();
        on_graph_timer_tick(&mut g, node_id, quit_ptr);
    });
    timer
        .update_timer(
            Some(std::time::Duration::from_millis(50)),
            Some(std::time::Duration::from_millis(50)),
        )
        .into_result()?;
    Ok(timer)
}

/// One tick of the graph timer: latch the filter node-id, drain pending
/// preferred-output changes, react to a system-default change, and follow the
/// output device's channel count (quitting the loop on a stable change so the
/// reconnect path rebuilds at the new width). `quit_ptr` is the `pw_main_loop`
/// raw pointer as `usize`.
fn on_graph_timer_tick(g: &mut GraphState, node_id: u32, quit_ptr: usize) {
    if node_id != u32::MAX && g.filter_node_id == u32::MAX {
        g.filter_node_id = node_id;
        reroute(g);
    }
    // Apply any pending preferred-output changes from the IPC thread.
    let mut reroute_needed = false;
    while let Ok(name) = g.route_rx.try_recv() {
        // Empty = follow the OS default sink (clear the pin).
        g.preferred_output = if name.is_empty() { None } else { Some(name) };
        reroute_needed = true;
    }
    // The system default sink changed (user picked an output in the system
    // switcher, or a device hot-plugged). When unpinned, `find_target_sink`
    // now follows it; either way re-claim the default for "resonance" so apps
    // keep playing into the EQ instead of moving to the device the user picked.
    let default_changed = g.default_dirty.swap(false, Ordering::Relaxed);
    if default_changed {
        reroute_needed = true;
    }
    if reroute_needed {
        reroute(g);
    }
    if default_changed {
        g.default_set = false;
        try_set_default(g);
    }

    // Live channel-count following: when the output device's channel count
    // differs from the filter's, record it and quit the loop so the reconnect
    // path rebuilds the filter + chain at the new width.
    let dev = device_channels(g);
    let action = channel_follow_step(
        dev,
        g.device_seen_count,
        g.device_seen_ticks,
        g.active_channels,
        g.pending_channels.is_some(),
    );
    g.device_seen_count = action.seen_count;
    g.device_seen_ticks = action.seen_ticks;
    if action.rebuild {
        tracing::info!(
            "output device is {dev}ch (filter is {}ch) — rebuilding the filter",
            g.active_channels
        );
        g.pending_channels = Some(dev);
        unsafe {
            pw_sys::pw_main_loop_quit(quit_ptr as *mut pw_sys::pw_main_loop);
        }
    }
}

/// Number of consecutive stable ticks the device channel count must hold (~200 ms
/// at the 50 ms timer) before a rebuild fires, so transient counts during device
/// enumeration (ports appearing one-by-one) don't cause a spurious rebuild.
const CHANNEL_FOLLOW_STABLE_TICKS: u8 = 4;

/// Outcome of one channel-follow debounce step: the updated debounce state and
/// whether a rebuild should be triggered this tick.
#[derive(Debug, PartialEq, Eq)]
struct ChannelFollowAction {
    seen_count: usize,
    seen_ticks: u8,
    rebuild: bool,
}

/// Pure debounce decision for live channel-count following.
///
/// Given the device's current input-port count (`dev`), the previously-seen
/// count + how many consecutive ticks it has held, the filter's active channel
/// count, and whether a rebuild is already pending, return the new debounce state
/// and whether to trigger a rebuild now.
///
/// A rebuild only fires once `dev` has held steady for [`CHANNEL_FOLLOW_STABLE_TICKS`]
/// ticks, differs from the active count, lies in the actionable `1..=MAX_CH` range
/// (an out-of-range count the rebuild path can't act on would otherwise livelock
/// the reconnect loop), and no rebuild is already pending (so it can't re-fire
/// every tick before the quit lands).
fn channel_follow_step(
    dev: usize,
    seen_count: usize,
    seen_ticks: u8,
    active_channels: usize,
    rebuild_pending: bool,
) -> ChannelFollowAction {
    let (seen_count, seen_ticks) = if dev == seen_count {
        (seen_count, seen_ticks.saturating_add(1))
    } else {
        (dev, 1)
    };
    let rebuild = (1..=MAX_CH).contains(&dev)
        && dev != active_channels
        && seen_ticks >= CHANNEL_FOLLOW_STABLE_TICKS
        && !rebuild_pending;
    ChannelFollowAction {
        seen_count,
        seen_ticks,
        rebuild,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Registry event handlers
// ─────────────────────────────────────────────────────────────────────────────

fn on_global(
    g: &mut GraphState,
    registry: &pw::registry::Registry,
    obj: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
) {
    use pw::types::ObjectType;

    match &obj.type_ {
        ObjectType::Node => {
            let Some(props) = obj.props else {
                return;
            };
            let mc = props.get("media.class").unwrap_or("").to_owned();
            let name = props.get("node.name").unwrap_or("").to_owned();
            let description = props.get("node.description").unwrap_or("").to_owned();
            g.nodes.insert(
                obj.id,
                NodeMeta {
                    media_class: mc.clone(),
                    name: name.clone(),
                    description,
                },
            );
            if mc == "Audio/Sink" && name == "resonance" {
                g.sink_node_id = obj.id;
                reroute(g);
                try_set_default(g);
            } else if mc == "Audio/Sink" {
                reroute(g);
            }
        }
        ObjectType::Port => {
            let Some(props) = obj.props else {
                return;
            };
            let node_id: u32 = props
                .get("node.id")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let is_output = props.get("port.direction") == Some("out");
            let channel = props.get("audio.channel").unwrap_or("").to_owned();
            let is_monitor = props.get("port.monitor") == Some("true");
            g.ports.insert(
                obj.id,
                PortMeta {
                    id: obj.id,
                    node_id,
                    is_output,
                    is_monitor,
                    channel,
                },
            );
            reroute(g);
        }
        ObjectType::Metadata => {
            let Some(props) = obj.props else {
                return;
            };
            if props.get("metadata.name") == Some("default") && g.metadata_obj.is_none() {
                if let Ok(meta) = registry.bind::<pw::metadata::Metadata, _>(obj) {
                    // Track the system default sink. `original_default` latches the
                    // first one (our fallback); `live_default` follows every change
                    // to a real sink so the system output switcher (and BT/headset
                    // hot-plugs) retarget Resonance. Our own re-claim of the default
                    // (name == "resonance") is filtered so it can't feed back.
                    let orig = Arc::clone(&g.original_default);
                    let live = Arc::clone(&g.live_default);
                    let dirty = Arc::clone(&g.default_dirty);
                    let listener = meta
                        .add_listener_local()
                        .property(move |_subject, key, _type, value| {
                            if key == Some("default.audio.sink") {
                                if let Some(name) = value.and_then(parse_metadata_name) {
                                    if name != "resonance" {
                                        let mut o = orig.lock().unwrap();
                                        if o.is_none() {
                                            *o = Some(name.clone());
                                        }
                                        *live.lock().unwrap() = Some(name);
                                        dirty.store(true, Ordering::Relaxed);
                                    }
                                }
                            }
                            0
                        })
                        .register();
                    g.metadata_listener = Some(listener);
                    g.metadata_obj = Some(meta);
                    try_set_default(g);
                }
            }
        }
        _ => {}
    }
}

fn on_global_remove(g: &mut GraphState, id: u32) {
    let was_real_sink = g
        .nodes
        .remove(&id)
        .is_some_and(|n| n.media_class == "Audio/Sink" && n.name != "resonance");
    g.ports.remove(&id);
    if was_real_sink {
        reroute(g);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Link / routing helpers
// ─────────────────────────────────────────────────────────────────────────────

/// The real output sink Resonance feeds, by priority: the explicit pin
/// (`SetOutputTarget`) — which always wins; else the live system default sink
/// (so the system output switcher / BT hot-plug retargets us when unpinned);
/// else `PipeWire`'s default from before Resonance took over; else the lowest-id
/// real sink (deterministic). `None` if none is known yet.
fn find_target_sink(g: &GraphState) -> Option<u32> {
    let find_sink = |name: &str| -> Option<u32> {
        g.nodes
            .iter()
            .find(|(id, n)| {
                n.media_class == "Audio/Sink" && n.name == name && **id != g.sink_node_id
            })
            .map(|(id, _)| *id)
    };
    g.preferred_output
        .as_deref()
        .and_then(&find_sink)
        .or_else(|| {
            g.live_default
                .lock()
                .unwrap()
                .as_deref()
                .and_then(&find_sink)
        })
        .or_else(|| {
            g.original_default
                .lock()
                .unwrap()
                .as_deref()
                .and_then(&find_sink)
        })
        .or_else(|| {
            g.nodes
                .iter()
                .filter(|(id, n)| {
                    n.media_class == "Audio/Sink" && n.name != "resonance" && **id != g.sink_node_id
                })
                .min_by_key(|(id, _)| **id)
                .map(|(id, _)| *id)
        })
}

/// The channel count of the current output device (its input-port count), or 0
/// when no target sink is known yet. Used to drive live channel-count following.
fn device_channels(g: &GraphState) -> usize {
    match find_target_sink(g) {
        Some(tid) => g
            .ports
            .values()
            .filter(|p| p.node_id == tid && !p.is_output)
            .count(),
        None => 0,
    }
}

fn reroute(g: &mut GraphState) {
    let core_ptr = g.raw_core as *mut pw_sys::pw_core;
    // SAFETY: raw_core is valid for daemon lifetime; we reconstruct a temporary Core ref.
    let core: &pw::core::Core = unsafe { &*core_ptr.cast::<pw::core::Core>() };

    // sink-monitor → filter-in
    if g.sink_node_id != u32::MAX && g.filter_node_id != u32::MAX {
        let srcs: Vec<_> = g
            .ports
            .values()
            .filter(|p| p.node_id == g.sink_node_id && p.is_output && p.is_monitor)
            .cloned()
            .collect();
        let dsts: Vec<_> = g
            .ports
            .values()
            .filter(|p| p.node_id == g.filter_node_id && !p.is_output)
            .cloned()
            .collect();
        if !srcs.is_empty() && !dsts.is_empty() {
            g.monitor_links.clear();
            g.monitor_links = create_links(core, &srcs, &dsts);
        }
    }

    // Report available sinks (name + friendly description) for the selection UI.
    let mut available: Vec<(String, String)> = g
        .nodes
        .values()
        .filter(|n| n.media_class == "Audio/Sink" && n.name != "resonance")
        .map(|n| (n.name.clone(), n.description.clone()))
        .collect();
    available.sort();
    let _ = g.sinks_tx.send(available);

    // filter-out → real device
    if g.filter_node_id != u32::MAX {
        let real_sink_id = find_target_sink(g);
        if let Some(tid) = real_sink_id {
            // Report the device name to the daemon when it changes (for output→profile mapping).
            if let Some(name) = g.nodes.get(&tid).map(|n| n.name.clone()) {
                if g.last_output.as_deref() != Some(name.as_str()) {
                    g.last_output = Some(name.clone());
                    let _ = g.output_tx.send(name);
                }
            }
            let srcs: Vec<_> = g
                .ports
                .values()
                .filter(|p| p.node_id == g.filter_node_id && p.is_output)
                .cloned()
                .collect();
            let dsts: Vec<_> = g
                .ports
                .values()
                .filter(|p| p.node_id == tid && !p.is_output)
                .cloned()
                .collect();
            if !srcs.is_empty() && !dsts.is_empty() {
                g.out_links.clear();
                g.out_links = create_links(core, &srcs, &dsts);
            }
        }
    }
}

/// Extract the sink name from a `PipeWire` metadata default value, e.g.
/// `{ "name": "alsa_output.pci-0000_00_1b.0.analog-stereo" }` → the name.
fn parse_metadata_name(value: &str) -> Option<String> {
    let key = value.find("\"name\"")?;
    let after = &value[key + 6..];
    let colon = after.find(':')?;
    let rest = &after[colon + 1..];
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    Some(rest[start..end].to_string())
}

/// Pair source ports to destination ports as `(src_id, dst_id)`: each source (in
/// port-id order) prefers a free destination carrying the *same* `audio.channel`
/// label (FL→FL, FC→FC, …), else falls back to the next free destination by
/// position (handles unlabeled ports and mismatched layouts). Stops when
/// destinations run out. Pure (no `PipeWire` calls) so the pairing is unit-testable.
fn pair_ports(srcs: &[PortMeta], dsts: &[PortMeta]) -> Vec<(u32, u32)> {
    let mut srcs_sorted = srcs.to_vec();
    srcs_sorted.sort_by_key(|p| p.id);
    let mut dst_used = vec![false; dsts.len()];

    let mut pairs = Vec::new();
    for s in &srcs_sorted {
        // Prefer a free same-label destination; else the first free one (positional).
        let pick = dsts
            .iter()
            .enumerate()
            .find(|(i, d)| !dst_used[*i] && !d.channel.is_empty() && d.channel == s.channel)
            .map(|(i, _)| i)
            .or_else(|| dst_used.iter().position(|&used| !used));
        let Some(di) = pick else { break };
        dst_used[di] = true;
        pairs.push((s.id, dsts[di].id));
    }
    pairs
}

fn create_links(
    core: &pw::core::Core,
    srcs: &[PortMeta],
    dsts: &[PortMeta],
) -> Vec<pw::link::Link> {
    let mut out = Vec::new();
    for (src_id, dst_id) in pair_ports(srcs, dsts) {
        let props = properties! {
            "link.output.port" => src_id.to_string(),
            "link.input.port"  => dst_id.to_string(),
            "object.linger"    => "false",
        };
        if let Ok(link) = core.create_object::<pw::link::Link>("link-factory", &props) {
            out.push(link);
        }
    }
    out
}

fn try_set_default(g: &mut GraphState) {
    if g.metadata_obj.is_none() || g.sink_node_id == u32::MAX || g.default_set {
        return;
    }
    if let Some(meta) = &g.metadata_obj {
        meta.set_property(
            0,
            "default.configured.audio.sink",
            Some("Spa:String:JSON"),
            Some("{ \"name\": \"resonance\" }"),
        );
        g.default_set = true;
        info!("Set 'Resonance EQ' as default audio output");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// RT process callback
// ─────────────────────────────────────────────────────────────────────────────

/// Peak/RMS across `channels` mono `f32` buffers (RT-thread passthrough metering).
/// Peak is the max over channels; RMS is the root-mean of per-channel mean-squares
/// — the N-channel generalisation of the old stereo helper.
///
/// # Safety
/// Each of `ptrs[..channels]` must point to at least `n` valid `f32` samples.
unsafe fn ptrs_peak_rms(ptrs: &[*mut f32], channels: usize, n: usize) -> (f32, f32) {
    let mut peak = 0.0f32;
    let mut sumsq = 0.0f64;
    for &p in &ptrs[..channels] {
        let (pk, rms) = peak_rms_f32(unsafe { std::slice::from_raw_parts(p, n) });
        peak = peak.max(pk);
        sumsq += f64::from(rms) * f64::from(rms);
    }
    let rms = if channels > 0 {
        (sumsq / channels as f64).sqrt() as f32
    } else {
        0.0
    };
    (peak, rms)
}

unsafe extern "C" fn filter_process_cb(data: *mut c_void, position: *mut spa_io_position) {
    unsafe {
        let fd = &mut *data.cast::<FilterData>();
        if position.is_null() {
            return;
        }
        let n = (*position).clock.duration as usize;
        if n == 0 {
            return;
        }

        // Follow the graph's negotiated sample rate. `clock.rate` is the seconds-
        // per-tick fraction {num, denom}, so the rate in Hz is denom/num (the
        // common case is {1, 48000}). If the graph runs off-48k — a 44.1 kHz DAC,
        // a `default.clock.rate` override, a device that forces its own rate — the
        // biquad + effect coefficients were computed for the wrong rate, landing
        // every EQ band at the wrong frequency. Rebind to the live rate.
        //
        // `rebind_sample_rate` is a no-op when unchanged, so the steady state pays
        // nothing. It reallocates effect state on an actual change (rare: startup
        // off-48k, or a graph rate renegotiation), which can cost one block —
        // acceptable versus a whole session of wrong-rate audio.
        let rate = (*position).clock.rate;
        if rate.num > 0 {
            let sr = f64::from(rate.denom) / f64::from(rate.num);
            if sr > 0.0 {
                fd.chain.rebind_sample_rate(sr);
            }
        }
        // Publish the live rate for `status` (cheap relaxed store every block).
        // The in-graph filter captures and plays at the same graph rate, so no
        // resampling happens here — capture == DSP.
        fd.meters.set_sample_rate(fd.chain.sample_rate);
        fd.meters.set_capture_rate(fd.chain.sample_rate);
        // Publish the live channel width too: the backend follows the output
        // device's channel count (see the reconnect-loop rebuild), but the
        // daemon's mirror chain stays frozen at its startup width, so `status`
        // and `rebuild_chain` must read this rather than the stale mirror.
        fd.meters.set_channels(fd.in_ports.len());
        // Log the ACTUAL negotiated graph rate once it settles/changes (the
        // "ready" line above only knew the requested rate). Guarded so it fires
        // on startup + rare renegotiations, never per block.
        if (fd.chain.sample_rate - fd.logged_rate).abs() > 0.5 {
            fd.logged_rate = fd.chain.sample_rate;
            info!(
                "PipeWire graph rate: {:.0} Hz ({} ch)",
                fd.chain.sample_rate,
                fd.in_ports.len()
            );
        }

        // Gather the per-channel DSP buffers onto the stack (no heap on the RT
        // path). `channels` is fixed at the FilterData's port count.
        let channels = fd.in_ports.len();
        let mut ins: [*mut f32; MAX_CH] = [std::ptr::null_mut(); MAX_CH];
        let mut outs: [*mut f32; MAX_CH] = [std::ptr::null_mut(); MAX_CH];
        for (ch, (&inp, &outp)) in fd.in_ports.iter().zip(fd.out_ports.iter()).enumerate() {
            ins[ch] = pw_sys::pw_filter_get_dsp_buffer(inp, n as u32).cast::<f32>();
            outs[ch] = pw_sys::pw_filter_get_dsp_buffer(outp, n as u32).cast::<f32>();
        }
        // No output buffers this cycle → nothing to write.
        if outs[..channels].iter().any(|p| p.is_null()) {
            return;
        }

        while let Ok(cmd) = fd.cmd_rx.pop() {
            apply_command(&mut fd.chain, cmd);
        }

        let have_in = ins[..channels].iter().all(|p| !p.is_null());
        if !have_in || !fd.chain.enabled {
            if have_in {
                for (&inp, &outp) in ins[..channels].iter().zip(&outs[..channels]) {
                    std::ptr::copy_nonoverlapping(inp, outp, n);
                }
                // Passthrough: in == out, no DSP cost.
                let (p, r) = ptrs_peak_rms(&outs, channels, n);
                fd.meters.store(Sample {
                    in_peak: p,
                    out_peak: p,
                    in_rms: r,
                    out_rms: r,
                    clip: p >= 0.999,
                    dsp_load: 0.0,
                    dsp_frame_us: 0,
                });
            } else {
                for &outp in &outs[..channels] {
                    std::ptr::write_bytes(outp, 0, n * std::mem::size_of::<f32>());
                }
                fd.meters.store(Sample::default());
            }
            // Keep the spectrum ring fed even while bypassed (power off) or with no
            // input: feed the mono mix of the output we just wrote. Without this
            // the ring starves and the analyzer freezes on its last full buffer.
            let sn = n.min(fd.spectrum_tx.slots());
            for i in 0..sn {
                let mut acc = 0.0f32;
                for &outp in &outs[..channels] {
                    acc += *outp.add(i);
                }
                let _ = fd.spectrum_tx.push(acc / channels as f32);
            }
            return;
        }

        // Reuse the pre-allocated scratch buffers (grow only if the quantum exceeds
        // MAX_QUANTUM, which is rare); no per-callback heap allocation in steady state.
        let need = n * channels;
        if fd.scratch.len() < need {
            fd.scratch.resize(need, 0.0);
        }
        if fd.routed.len() < need {
            fd.routed.resize(need, 0.0);
        }
        for i in 0..n {
            for (ch, &inp) in ins[..channels].iter().enumerate() {
                fd.scratch[i * channels + ch] = f64::from(*inp.add(i));
            }
        }
        let (in_peak, in_rms) = peak_rms(&fd.scratch[..need]);
        let t0 = Instant::now();
        fd.chain.process(&mut fd.scratch[..need]);
        // Output routing: a square remap (swap / per-channel gain) maps the
        // processed channels onto the same number of ports. A non-square matrix
        // can't change the fixed port count here, so it's skipped — full
        // up/downmix is the daemon-path backends' job (macOS), not the in-graph
        // filter's. `route` copies when there's no matrix or it's identity.
        // Only a true square remap at the live width is applied in-graph (the
        // filter has a fixed `channels` ports in and out). Requiring BOTH dims to
        // equal `channels` rejects a mismatched matrix that would misframe the
        // buffer; the daemon also validates this at install time.
        let route_applies =
            matches!(&fd.chain.routing, Some(m) if m.in_ch() == channels && m.out_ch() == channels);
        if route_applies {
            fd.chain.route(&fd.scratch[..need], &mut fd.routed[..need]);
        }
        let out_buf: &[f64] = if route_applies {
            &fd.routed[..need]
        } else {
            &fd.scratch[..need]
        };
        let dt = t0.elapsed();
        let (out_peak, out_rms) = peak_rms(out_buf);
        for i in 0..n {
            for (ch, &outp) in outs[..channels].iter().enumerate() {
                *outp.add(i) = out_buf[i * channels + ch] as f32;
            }
        }

        // DSP load = process time / the block's real-time budget (n / sample_rate).
        let budget = n as f64 / fd.chain.sample_rate;
        let load = if budget > 0.0 {
            (dt.as_secs_f64() / budget) as f32
        } else {
            0.0
        };
        fd.meters.store(Sample {
            in_peak,
            out_peak,
            in_rms,
            out_rms,
            clip: out_peak >= 0.999,
            dsp_load: load,
            dsp_frame_us: dt.as_micros() as u32,
        });

        // Spectrum: mono mix of the final output, per frame.
        let sn = n.min(fd.spectrum_tx.slots());
        for i in 0..sn {
            let mut acc = 0.0f64;
            for ch in 0..channels {
                acc += out_buf[i * channels + ch];
            }
            let _ = fd.spectrum_tx.push((acc / channels as f64) as f32);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Raw pw_properties helper (for pw_filter FFI only)
// ─────────────────────────────────────────────────────────────────────────────

/// SPA channel-position names for `channels` ports. Uses the standard WAVE-order
/// names for 1..=8 (matching `resonance_ipc::default_channel_layout`, all valid
/// SPA positions); beyond 8 falls back to `AUX0..` (also valid SPA names) since
/// there are no further standard positions.
fn pw_channel_names(channels: usize) -> Vec<String> {
    if (1..=8).contains(&channels) {
        resonance_ipc::default_channel_layout(channels)
    } else {
        (0..channels).map(|i| format!("AUX{i}")).collect()
    }
}

fn pw_props_raw(pairs: &[(&str, &str)]) -> *mut pw_sys::pw_properties {
    use std::ffi::CString;
    unsafe {
        let p = pw_sys::pw_properties_new(std::ptr::null());
        for (k, v) in pairs {
            // Skip a pair with an interior NUL rather than panic. Current callers
            // pass only static literals (none contain NUL), so this never fires —
            // but a future dynamic caller must not be able to crash the daemon.
            let (Ok(kc), Ok(vc)) = (CString::new(*k), CString::new(*v)) else {
                continue;
            };
            pw_sys::pw_properties_set(p, kc.as_ptr(), vc.as_ptr());
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_metadata_name_extracts_sink() {
        assert_eq!(
            parse_metadata_name("{ \"name\": \"alsa_output.pci-0000_00_1b.0.analog-stereo\" }")
                .as_deref(),
            Some("alsa_output.pci-0000_00_1b.0.analog-stereo")
        );
        assert_eq!(
            parse_metadata_name("{\"name\":\"bluez_output.AA_BB\"}").as_deref(),
            Some("bluez_output.AA_BB")
        );
        assert_eq!(parse_metadata_name("null"), None);
        assert_eq!(parse_metadata_name("{}"), None);
    }

    fn port(id: u32, channel: &str) -> PortMeta {
        PortMeta {
            id,
            node_id: 0,
            is_output: false,
            is_monitor: false,
            channel: channel.to_string(),
        }
    }

    #[test]
    fn pair_ports_matches_by_channel_label() {
        // Destinations deliberately out of label order; pairing is by label.
        let srcs = [port(1, "FL"), port(2, "FR")];
        let dsts = [port(20, "FR"), port(21, "FL")];
        assert_eq!(pair_ports(&srcs, &dsts), vec![(1, 21), (2, 20)]);
    }

    #[test]
    fn pair_ports_falls_back_positional_when_unlabeled() {
        // Unlabeled destinations → positional (src id order ↔ dst index order).
        let srcs = [port(2, "FR"), port(1, "FL")];
        let dsts = [port(30, ""), port(31, "")];
        // srcs sorted by id → [1, 2]; positional → 1→30, 2→31.
        assert_eq!(pair_ports(&srcs, &dsts), vec![(1, 30), (2, 31)]);
    }

    #[test]
    fn pair_ports_stops_when_destinations_exhausted() {
        let srcs = [port(1, "FL"), port(2, "FR"), port(3, "FC")];
        let dsts = [port(10, "FL"), port(11, "FR")];
        assert_eq!(pair_ports(&srcs, &dsts), vec![(1, 10), (2, 11)]);
    }

    #[test]
    fn pair_ports_surround_all_matched_by_label() {
        let labels = ["FL", "FR", "FC", "LFE", "RL", "RR"];
        let srcs: Vec<_> = labels
            .iter()
            .enumerate()
            .map(|(i, c)| port(i as u32, c))
            .collect();
        // Destinations shuffled, ids offset.
        let dsts: Vec<_> = labels
            .iter()
            .rev()
            .enumerate()
            .map(|(i, c)| port(100 + i as u32, c))
            .collect();
        let pairs = pair_ports(&srcs, &dsts);
        assert_eq!(pairs.len(), 6);
        // Every source linked to the dst with the same label.
        for (sid, did) in pairs {
            let s_label = &srcs[sid as usize].channel;
            let d_label = &dsts.iter().find(|d| d.id == did).unwrap().channel;
            assert_eq!(s_label, d_label, "src {sid} → dst {did} same label");
        }
    }

    #[test]
    fn channel_follow_no_rebuild_until_stable() {
        // A fresh count seen for the first time: ticks reset to 1, no rebuild yet.
        let a = channel_follow_step(6, 2, 9, 2, false);
        assert_eq!(
            a,
            ChannelFollowAction {
                seen_count: 6,
                seen_ticks: 1,
                rebuild: false
            }
        );
        // Same count held but not yet stable (3 < 4 ticks): still no rebuild.
        let a = channel_follow_step(6, 6, 2, 2, false);
        assert_eq!(
            a,
            ChannelFollowAction {
                seen_count: 6,
                seen_ticks: 3,
                rebuild: false
            }
        );
    }

    #[test]
    fn channel_follow_rebuilds_once_stable() {
        // Held to the stable threshold and differs from active → rebuild.
        let a = channel_follow_step(6, 6, CHANNEL_FOLLOW_STABLE_TICKS - 1, 2, false);
        assert_eq!(
            a,
            ChannelFollowAction {
                seen_count: 6,
                seen_ticks: CHANNEL_FOLLOW_STABLE_TICKS,
                rebuild: true
            }
        );
    }

    #[test]
    fn channel_follow_no_rebuild_when_matching_or_pending_or_out_of_range() {
        // Equals the active count → no rebuild even when stable.
        assert!(!channel_follow_step(2, 2, 99, 2, false).rebuild);
        // A rebuild is already pending → don't re-fire.
        assert!(!channel_follow_step(6, 6, 99, 2, true).rebuild);
        // Out-of-range device count (> MAX_CH) the rebuild path can't act on.
        assert!(!channel_follow_step(MAX_CH + 1, MAX_CH + 1, 99, 2, false).rebuild);
        // Zero (no target sink known yet) is also out of the actionable range.
        assert!(!channel_follow_step(0, 0, 99, 2, false).rebuild);
    }

    #[test]
    fn channel_follow_ticks_saturate() {
        // seen_ticks must not wrap when the count holds for a very long time.
        let a = channel_follow_step(6, 6, u8::MAX, 6, false);
        assert_eq!(a.seen_ticks, u8::MAX);
    }

    #[test]
    fn pw_channel_names_standard_layouts() {
        assert_eq!(pw_channel_names(1), vec!["MONO"]);
        assert_eq!(pw_channel_names(2), vec!["FL", "FR"]);
        assert_eq!(
            pw_channel_names(6),
            vec!["FL", "FR", "FC", "LFE", "RL", "RR"]
        );
        // Beyond 8 → AUX fallback (valid SPA names).
        let n9 = pw_channel_names(9);
        assert_eq!(n9.len(), 9);
        assert_eq!(n9[0], "AUX0");
        assert_eq!(n9[8], "AUX8");
    }
}
