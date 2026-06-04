/// PipeWire backend — mirrors FxSound AudioPassthruPipeWire architecture:
///   1. null-audio-sink "Resonance EQ" — routable device; apps play into it.
///   2. pw_filter "Resonance EQ Processor" — 2-in/2-out, driven by same clock
///      as the sink (no ring-buffer choppy artefacts).
///   3. Registry listener creates links: sink-monitor → filter-in,
///      filter-out → real device.
///   4. WirePlumber metadata sets "Resonance EQ" as system default sink.
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
    default_set: bool,
}

// SAFETY: only touched from the pw main-loop thread.
unsafe impl Send for GraphState {}

struct FilterData {
    in_ports: [*mut c_void; 2],
    out_ports: [*mut c_void; 2],
    chain: ProcessorChain,
    cmd_rx: rtrb::Consumer<AudioCommand>,
    spectrum_tx: rtrb::Producer<f32>,
}

// SAFETY: only touched from pw_filter process callback (RT thread).
unsafe impl Send for FilterData {}

// ─────────────────────────────────────────────────────────────────────────────

pub fn spawn(
    cmd_rx: rtrb::Consumer<AudioCommand>,
    spectrum_tx: rtrb::Producer<f32>,
    initial_chain: ProcessorChain,
) -> Result<JoinHandle<()>> {
    Ok(thread::Builder::new()
        .name("resonance-pw".into())
        .spawn(move || {
            if let Err(e) = run(cmd_rx, spectrum_tx, initial_chain) {
                tracing::error!("PipeWire thread: {e:#}");
            }
        })?)
}

fn run(
    cmd_rx: rtrb::Consumer<AudioCommand>,
    spectrum_tx: rtrb::Producer<f32>,
    chain: ProcessorChain,
) -> Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopBox::new(None)?;
    let context = pw::context::ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect(None)?;

    // ── pw_filter (raw FFI) ───────────────────────────────────────────────
    let raw_core = core.as_raw_ptr();

    let fd = Box::new(FilterData {
        in_ports: [std::ptr::null_mut(); 2],
        out_ports: [std::ptr::null_mut(); 2],
        chain,
        cmd_rx,
        spectrum_tx,
    });
    let fd_ptr = Box::into_raw(fd);

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

    let gs = Arc::new(Mutex::new(GraphState {
        raw_core: raw_core as usize,
        sink_node_id: u32::MAX,
        filter_node_id: u32::MAX,
        nodes: HashMap::new(),
        ports: HashMap::new(),
        out_links: Vec::new(),
        monitor_links: Vec::new(),
        metadata_obj: None,
        default_set: false,
    }));

    // SAFETY: `core` and `registry` outlive the listener (same scope, dropped after mainloop).
    // The transmute extends the RegistryBox lifetime to 'static for the closure — safe because
    // the listener is dropped before registry (Rust drops in reverse declaration order).
    let registry_static: &'static pw::registry::Registry = unsafe {
        std::mem::transmute::<&pw::registry::Registry, &'static pw::registry::Registry>(&*registry)
    };

    let gs_global = Arc::clone(&gs);
    let gs_remove = Arc::clone(&gs);

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
    {
        let gs_timer = Arc::clone(&gs);
        let timer = mainloop.loop_().add_timer(move |_| {
            let node_id = unsafe { pw_sys::pw_filter_get_node_id(filter) };
            if node_id != u32::MAX {
                let mut g = gs_timer.lock().unwrap();
                if g.filter_node_id == u32::MAX {
                    g.filter_node_id = node_id;
                    reroute(&mut g);
                }
            }
        });
        timer
            .update_timer(
                Some(std::time::Duration::from_millis(50)),
                Some(std::time::Duration::from_millis(50)),
            )
            .into_result()?;
        // Keep timer alive for the duration of the loop.
        std::mem::forget(timer);
    }

    info!(
        "PipeWire ready — 'Resonance EQ' is now the default output ({}ch @ {} Hz)",
        CHANNELS, SAMPLE_RATE
    );

    mainloop.run();

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
            g.nodes.insert(
                obj.id,
                NodeMeta {
                    media_class: mc.clone(),
                    name: name.clone(),
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

    // filter-out → real device
    if g.filter_node_id != u32::MAX {
        let real_sink_id = g
            .nodes
            .iter()
            .find(|(id, n)| {
                n.media_class == "Audio/Sink" && n.name != "resonance" && **id != g.sink_node_id
            })
            .map(|(id, _)| *id);
        if let Some(tid) = real_sink_id {
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
            } else {
                std::ptr::write_bytes(out0, 0, n * std::mem::size_of::<f32>());
                std::ptr::write_bytes(out1, 0, n * std::mem::size_of::<f32>());
            }
            return;
        }

        let mut buf: Vec<f64> = Vec::with_capacity(n * 2);
        for i in 0..n {
            buf.push(*in0.add(i) as f64);
            buf.push(*in1.add(i) as f64);
        }
        fd.chain.process(&mut buf);
        for i in 0..n {
            *out0.add(i) = buf[i * 2] as f32;
            *out1.add(i) = buf[i * 2 + 1] as f32;
        }

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
    }
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
