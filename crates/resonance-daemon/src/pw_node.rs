/// PipeWire backend — mirrors FxSound AudioPassthruPipeWire architecture:
///   1. null-audio-sink "Resonance EQ" — routable device; apps play into it.
///   2. pw_filter "Resonance EQ Processor" — 2-in/2-out, driven by same clock
///      as the sink (no ring-buffer choppy artefacts).
///   3. Registry listener creates links: sink-monitor → filter-in,
///      filter-out → real device.
///   4. WirePlumber metadata sets "Resonance EQ" as system default sink.
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
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tracing::info;

pub const CHANNELS: usize = 2;
pub const SAMPLE_RATE: u32 = 48000;
pub const SPECTRUM_BUF: usize = 8192;

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
    /// PipeWire's default sink name *before* Resonance took over — our first-choice
    /// downstream target. Written by the metadata listener, read in `reroute`.
    original_default: Arc<Mutex<Option<String>>>,
    default_set: bool,
    /// Reports the node.name of the real sink Resonance currently feeds, on change.
    output_tx: tokio::sync::mpsc::UnboundedSender<String>,
    /// Last output name reported (dedupe).
    last_output: Option<String>,
    /// Preferred output node name; set via IPC command SetOutputTarget.
    preferred_output: Option<String>,
    /// Receives preferred-output updates from the IPC thread.
    route_rx: std::sync::mpsc::Receiver<String>,
    /// Sends the current set of available sink names to the daemon state.
    sinks_tx: tokio::sync::mpsc::UnboundedSender<Vec<(String, String)>>,
}

// SAFETY: only touched from the pw main-loop thread.
unsafe impl Send for GraphState {}

struct FilterData {
    in_ports: [*mut c_void; 2],
    out_ports: [*mut c_void; 2],
    chain: ProcessorChain,
    cmd_rx: rtrb::Consumer<AudioCommand>,
    spectrum_tx: rtrb::Producer<f32>,
    /// Reusable interleaved f64 scratch buffer — avoids allocating every RT callback.
    scratch: Vec<f64>,
    /// Live meters published to the IPC thread.
    meters: Arc<AtomicMeters>,
}

/// Pre-allocated scratch capacity: a generous upper bound on the PipeWire quantum
/// (frames × CHANNELS). Growth in the RT callback past this is rare and handled.
const MAX_QUANTUM: usize = 8192;

// SAFETY: only touched from pw_filter process callback (RT thread).
unsafe impl Send for FilterData {}

// ─────────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn spawn(
    cmd_rx: rtrb::Consumer<AudioCommand>,
    spectrum_tx: rtrb::Producer<f32>,
    initial_chain: ProcessorChain,
    output_tx: tokio::sync::mpsc::UnboundedSender<String>,
    route_rx: std::sync::mpsc::Receiver<String>,
    sinks_tx: tokio::sync::mpsc::UnboundedSender<Vec<(String, String)>>,
    meters: Arc<AtomicMeters>,
) -> Result<JoinHandle<()>> {
    // FilterData and GraphState persist across reconnects — the audio chain (EQ
    // state) and the daemon-facing channels must survive a PipeWire restart,
    // since they're paired with producers/receivers the daemon already holds.
    let mut fd = Box::new(FilterData {
        in_ports: [std::ptr::null_mut(); 2],
        out_ports: [std::ptr::null_mut(); 2],
        chain: initial_chain,
        cmd_rx,
        spectrum_tx,
        scratch: vec![0.0; MAX_QUANTUM * CHANNELS],
        meters,
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
        default_set: false,
        output_tx,
        last_output: None,
        preferred_output: None,
        route_rx,
        sinks_tx,
    }));

    Ok(thread::Builder::new()
        .name("resonance-pw".into())
        .spawn(move || {
            pw::init();
            let mut backoff = Duration::from_millis(200);
            loop {
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
                fd.in_ports = [std::ptr::null_mut(); 2];
                fd.out_ports = [std::ptr::null_mut(); 2];
                let fd_ptr: *mut FilterData = &mut *fd;

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

/// Build the PipeWire graph for one connection attempt and run its main loop
/// until the connection drops (core error → quit). Returns `Ok` on a clean
/// disconnect so the caller can reconnect; `Err` if setup failed.
fn build_and_run(fd_ptr: *mut FilterData, gs: &Arc<Mutex<GraphState>>) -> Result<()> {
    let mainloop = pw::main_loop::MainLoopBox::new(None)?;
    let context = pw::context::ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect(None)?;

    // ── pw_filter (raw FFI) ───────────────────────────────────────────────
    let raw_core = core.as_raw_ptr();
    gs.lock().unwrap().raw_core = raw_core as usize;

    // Quit the main loop on a fatal core error (server gone / disconnect) so
    // the reconnect loop can take over.
    let quit_ptr = mainloop.as_raw_ptr() as usize;
    let _core_listener = core
        .add_listener_local()
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
        .register();

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
        pw_sys::pw_filter_add_listener(f, hook, fev, fd_ptr as *mut c_void);
        f
    };

    for ch in 0..CHANNELS {
        let (chname, inname, outname) = if ch == 0 {
            ("FL", "input_FL", "output_FL")
        } else {
            ("FR", "input_FR", "output_FR")
        };
        unsafe {
            (*fd_ptr).in_ports[ch] = pw_sys::pw_filter_add_port(
                filter,
                SPA_DIRECTION_INPUT,
                pw_sys::pw_filter_port_flags_PW_FILTER_PORT_FLAG_MAP_BUFFERS,
                std::mem::size_of::<u64>(),
                pw_props_raw(&[
                    ("format.dsp", "32 bit float mono audio"),
                    ("port.name", inname),
                    ("audio.channel", chname),
                ]),
                std::ptr::null_mut(),
                0,
            );
            (*fd_ptr).out_ports[ch] = pw_sys::pw_filter_add_port(
                filter,
                SPA_DIRECTION_OUTPUT,
                pw_sys::pw_filter_port_flags_PW_FILTER_PORT_FLAG_MAP_BUFFERS,
                std::mem::size_of::<u64>(),
                pw_props_raw(&[
                    ("format.dsp", "32 bit float mono audio"),
                    ("port.name", outname),
                    ("audio.channel", chname),
                ]),
                std::ptr::null_mut(),
                0,
            );
        }
    }

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

    // ── null-audio-sink (high-level) ──────────────────────────────────────
    let _sink: pw::node::Node = core.create_object(
        "adapter",
        &properties! {
            "factory.name"           => "support.null-audio-sink",
            "node.name"              => "resonance",
            "node.description"       => "Resonance EQ",
            "media.class"            => "Audio/Sink",
            "audio.position"         => "FL,FR",
            "monitor.channel-volumes" => "false",
            "monitor.passthrough"    => "true",
            "node.virtual"           => "true",
        },
    )?;

    // ── Registry listener (high-level, 'static closure via Arc) ──────────
    let registry = core.get_registry()?;

    // SAFETY: `core` and `registry` outlive the listener (same scope, dropped after mainloop).
    // The transmute extends the RegistryBox lifetime to 'static for the closure — safe because
    // the listener is dropped before registry (Rust drops in reverse declaration order).
    let registry_static: &'static pw::registry::Registry = unsafe {
        std::mem::transmute::<&pw::registry::Registry, &'static pw::registry::Registry>(&*registry)
    };

    let gs_global = Arc::clone(gs);
    let gs_remove = Arc::clone(gs);

    let _listener = registry_static
        .add_listener_local()
        .global(move |obj| {
            let mut g = gs_global.lock().unwrap();
            on_global(&mut g, registry_static, obj);
        })
        .global_remove(move |id| {
            let mut g = gs_remove.lock().unwrap();
            on_global_remove(&mut g, id);
        })
        .register();

    // ── Poll filter node-id, then run main loop ───────────────────────────
    // Use a timer on the loop to poll pw_filter_get_node_id.
    let gs_timer = Arc::clone(gs);
    let timer = mainloop.loop_().add_timer(move |_| {
        let node_id = unsafe { pw_sys::pw_filter_get_node_id(filter) };
        let mut g = gs_timer.lock().unwrap();
        if node_id != u32::MAX && g.filter_node_id == u32::MAX {
            g.filter_node_id = node_id;
            reroute(&mut g);
        }
        // Apply any pending preferred-output changes from the IPC thread.
        let mut reroute_needed = false;
        while let Ok(name) = g.route_rx.try_recv() {
            g.preferred_output = Some(name);
            reroute_needed = true;
        }
        if reroute_needed {
            reroute(&mut g);
        }
    });
    timer
        .update_timer(
            Some(std::time::Duration::from_millis(50)),
            Some(std::time::Duration::from_millis(50)),
        )
        .into_result()?;

    info!(
        "PipeWire ready — 'Resonance EQ' is now the default output ({}ch @ {} Hz)",
        CHANNELS, SAMPLE_RATE
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
            let props = match obj.props {
                Some(p) => p,
                None => return,
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
            let props = match obj.props {
                Some(p) => p,
                None => return,
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
            let props = match obj.props {
                Some(p) => p,
                None => return,
            };
            if props.get("metadata.name") == Some("default") && g.metadata_obj.is_none() {
                if let Ok(meta) = registry.bind::<pw::metadata::Metadata, _>(obj) {
                    // Capture the existing default sink before we override it, so
                    // our first downstream target is whatever PipeWire used before.
                    let orig = Arc::clone(&g.original_default);
                    let listener = meta
                        .add_listener_local()
                        .property(move |_subject, key, _type, value| {
                            if key == Some("default.audio.sink") {
                                if let Some(name) = value.and_then(parse_metadata_name) {
                                    let mut o = orig.lock().unwrap();
                                    if o.is_none() {
                                        *o = Some(name);
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
        .map(|n| n.media_class == "Audio/Sink" && n.name != "resonance")
        .unwrap_or(false);
    g.ports.remove(&id);
    if was_real_sink {
        reroute(g);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Link / routing helpers
// ─────────────────────────────────────────────────────────────────────────────

fn reroute(g: &mut GraphState) {
    let core_ptr = g.raw_core as *mut pw_sys::pw_core;
    // SAFETY: raw_core is valid for daemon lifetime; we reconstruct a temporary Core ref.
    let core: &pw::core::Core = unsafe { &*(core_ptr as *mut pw::core::Core) };

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
        // Target priority (scoped so the shared borrows end before we relink):
        //   1. user-selected sink (SetOutputTarget)
        //   2. PipeWire's default sink from before Resonance took over
        //   3. any available real sink
        let real_sink_id = {
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
                    g.original_default
                        .lock()
                        .unwrap()
                        .as_deref()
                        .and_then(&find_sink)
                })
                .or_else(|| {
                    g.nodes
                        .iter()
                        .find(|(id, n)| {
                            n.media_class == "Audio/Sink"
                                && n.name != "resonance"
                                && **id != g.sink_node_id
                        })
                        .map(|(id, _)| *id)
                })
        };
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

/// Extract the sink name from a PipeWire metadata default value, e.g.
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

fn create_links(
    core: &pw::core::Core,
    srcs: &[PortMeta],
    dsts: &[PortMeta],
) -> Vec<pw::link::Link> {
    let find = |ports: &[PortMeta], ch: &str| -> Option<u32> {
        ports
            .iter()
            .find(|p| p.channel == ch)
            .or_else(|| ports.first())
            .map(|p| p.id)
    };
    let mut out = Vec::new();
    for ch in ["FL", "FR"] {
        let Some(sid) = find(srcs, ch) else { continue };
        let Some(did) = find(dsts, ch) else { continue };
        let props = properties! {
            "link.output.port" => sid.to_string(),
            "link.input.port"  => did.to_string(),
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

/// Peak/RMS across two mono `f32` channel buffers (RT-thread passthrough metering).
///
/// # Safety
/// `a` and `b` must point to at least `n` valid `f32` samples.
unsafe fn stereo_peak_rms(a: *const f32, b: *const f32, n: usize) -> (f32, f32) {
    let (pa, ra) = peak_rms_f32(unsafe { std::slice::from_raw_parts(a, n) });
    let (pb, rb) = peak_rms_f32(unsafe { std::slice::from_raw_parts(b, n) });
    (pa.max(pb), ((ra * ra + rb * rb) / 2.0).sqrt())
}

unsafe extern "C" fn filter_process_cb(data: *mut c_void, position: *mut spa_io_position) {
    unsafe {
        let fd = &mut *(data as *mut FilterData);
        if position.is_null() {
            return;
        }
        let n = (*position).clock.duration as usize;
        if n == 0 {
            return;
        }

        let in0 = pw_sys::pw_filter_get_dsp_buffer(fd.in_ports[0], n as u32) as *mut f32;
        let in1 = pw_sys::pw_filter_get_dsp_buffer(fd.in_ports[1], n as u32) as *mut f32;
        let out0 = pw_sys::pw_filter_get_dsp_buffer(fd.out_ports[0], n as u32) as *mut f32;
        let out1 = pw_sys::pw_filter_get_dsp_buffer(fd.out_ports[1], n as u32) as *mut f32;
        if out0.is_null() || out1.is_null() {
            return;
        }

        while let Ok(cmd) = fd.cmd_rx.pop() {
            apply_command(&mut fd.chain, cmd);
        }

        let have_in = !in0.is_null() && !in1.is_null();
        if !have_in || !fd.chain.enabled {
            if have_in {
                std::ptr::copy_nonoverlapping(in0, out0, n);
                std::ptr::copy_nonoverlapping(in1, out1, n);
                // Passthrough: in == out, no DSP cost.
                let (p, r) = stereo_peak_rms(in0, in1, n);
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
                std::ptr::write_bytes(out0, 0, n * std::mem::size_of::<f32>());
                std::ptr::write_bytes(out1, 0, n * std::mem::size_of::<f32>());
                fd.meters.store(Sample::default());
            }
            return;
        }

        // Reuse the pre-allocated scratch buffer (grows only if the quantum exceeds
        // MAX_QUANTUM, which is rare); no per-callback heap allocation in steady state.
        let need = n * 2;
        if fd.scratch.len() < need {
            fd.scratch.resize(need, 0.0);
        }
        let buf = &mut fd.scratch[..need];
        for i in 0..n {
            buf[i * 2] = *in0.add(i) as f64;
            buf[i * 2 + 1] = *in1.add(i) as f64;
        }
        let (in_peak, in_rms) = peak_rms(buf);
        let t0 = Instant::now();
        fd.chain.process(buf);
        let dt = t0.elapsed();
        let (out_peak, out_rms) = peak_rms(buf);
        for i in 0..n {
            *out0.add(i) = buf[i * 2] as f32;
            *out1.add(i) = buf[i * 2 + 1] as f32;
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

        let sn = n.min(fd.spectrum_tx.slots());
        for i in 0..sn {
            let _ = fd
                .spectrum_tx
                .push((buf[i * 2] + buf[i * 2 + 1]) as f32 * 0.5);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// AudioCommand dispatch
// ─────────────────────────────────────────────────────────────────────────────

fn apply_command(chain: &mut ProcessorChain, cmd: AudioCommand) {
    match cmd {
        AudioCommand::SetPower(on) => chain.enabled = on,
        AudioCommand::SetPreamp(db) => chain.preamp_db = db,
        AudioCommand::SetEffectIntensity { effect, value } => {
            chain.set_effect_intensity(effect, value)
        }
        AudioCommand::SetEffectEnabled { effect, on } => chain.set_effect_enabled(effect, on),
        AudioCommand::ReplaceChain(c) => *chain = *c,
        AudioCommand::Reset => chain.reset(),
        AudioCommand::SetBand {
            index,
            freq,
            gain_db,
            q,
        } => {
            let sr = chain.sample_rate;
            if let Some(f) = chain.filters.get_mut(index) {
                // Update coefficients in place — preserves filter state so rapid
                // live edits don't reset history and crackle.
                let _ = f.update(f.filter_type, freq, gain_db, q, sr);
            }
        }
        AudioCommand::SetBandEnabled { index, enabled } => {
            if let Some(f) = chain.filters.get_mut(index) {
                f.enabled = enabled;
            }
        }
        AudioCommand::AddBand {
            band_type,
            freq,
            gain_db,
            q,
        } => {
            if let Ok(f) = build_band(chain, band_type.into(), freq, gain_db, q, true) {
                chain.filters.push(f);
            }
        }
        AudioCommand::RemoveBand { index } => {
            if index < chain.filters.len() {
                chain.filters.remove(index);
            }
        }
        AudioCommand::SetBandType { index, band_type } => {
            let sr = chain.sample_rate;
            if let Some(f) = chain.filters.get_mut(index) {
                let _ = f.update(band_type.into(), f.freq, f.gain_db, f.q, sr);
            }
        }
    }
}

/// Build an `ApoFilter` matching the chain's sample rate / channel count.
fn build_band(
    chain: &ProcessorChain,
    filter_type: resonance_dsp::filter::FilterType,
    freq: f64,
    gain_db: f64,
    q: f64,
    enabled: bool,
) -> Result<resonance_dsp::filter::ApoFilter, resonance_dsp::filter::FilterError> {
    resonance_dsp::filter::ApoFilter::builder()
        .filter_type(filter_type)
        .freq(freq)
        .gain_db(gain_db)
        .q(q)
        .enabled(enabled)
        .channels(chain.channels)
        .sample_rate(chain.sample_rate)
        .build()
}

// ─────────────────────────────────────────────────────────────────────────────
// Raw pw_properties helper (for pw_filter FFI only)
// ─────────────────────────────────────────────────────────────────────────────

fn pw_props_raw(pairs: &[(&str, &str)]) -> *mut pw_sys::pw_properties {
    use std::ffi::CString;
    unsafe {
        let p = pw_sys::pw_properties_new(std::ptr::null());
        for (k, v) in pairs {
            let kc = CString::new(*k).unwrap();
            let vc = CString::new(*v).unwrap();
            pw_sys::pw_properties_set(p, kc.as_ptr(), vc.as_ptr());
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use resonance_dsp::filter::FilterType;
    use resonance_ipc::BandType;

    fn chain() -> ProcessorChain {
        ProcessorChain::builder()
            .channels(2)
            .sample_rate(48000.0)
            .build()
    }

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

    #[test]
    fn add_band_uses_requested_type() {
        let mut c = chain();
        apply_command(
            &mut c,
            AudioCommand::AddBand {
                band_type: BandType::HighShelf,
                freq: 8000.0,
                gain_db: 4.0,
                q: 0.7,
            },
        );
        assert_eq!(c.filters.len(), 1);
        assert_eq!(c.filters[0].filter_type, FilterType::HighShelf);
    }

    #[test]
    fn set_band_type_preserves_freq_gain_q() {
        let mut c = chain();
        apply_command(
            &mut c,
            AudioCommand::AddBand {
                band_type: BandType::Peaking,
                freq: 1000.0,
                gain_db: 6.0,
                q: 2.0,
            },
        );
        apply_command(
            &mut c,
            AudioCommand::SetBandType {
                index: 0,
                band_type: BandType::LowPass,
            },
        );
        let f = &c.filters[0];
        assert_eq!(f.filter_type, FilterType::LowPassQ); // BandType::LowPass → LowPassQ
        assert!((f.freq - 1000.0).abs() < 1e-9);
        assert!((f.gain_db - 6.0).abs() < 1e-9);
        assert!((f.q - 2.0).abs() < 1e-9);
    }

    #[test]
    fn remove_band_out_of_range_is_noop() {
        let mut c = chain();
        apply_command(
            &mut c,
            AudioCommand::AddBand {
                band_type: BandType::Peaking,
                freq: 1000.0,
                gain_db: 0.0,
                q: 1.0,
            },
        );
        apply_command(&mut c, AudioCommand::RemoveBand { index: 5 });
        assert_eq!(c.filters.len(), 1);
        apply_command(&mut c, AudioCommand::RemoveBand { index: 0 });
        assert_eq!(c.filters.len(), 0);
    }

    #[test]
    fn preamp_and_power_commands_apply() {
        let mut c = chain();
        apply_command(&mut c, AudioCommand::SetPreamp(-6.0));
        apply_command(&mut c, AudioCommand::SetPower(false));
        assert!((c.preamp_db + 6.0).abs() < 1e-9);
        assert!(!c.enabled);
    }
}
