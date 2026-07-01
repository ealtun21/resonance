mod autoeq;

use anyhow::{Result, bail};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use resonance_ipc::{ChannelMask, Command, FxEffectId, Response, transport::SyncClient};
use std::io::{self, IsTerminal};

#[derive(Parser)]
#[command(name = "resonance", about = "Control the Resonance EQ daemon", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Sub>,
}

#[derive(Subcommand)]
enum Sub {
    /// Show daemon status (default when no subcommand given)
    Status,
    /// Load a preset file (.fac or APO .txt)
    Load {
        /// Path to preset file
        path: String,
    },
    /// List preset files (defaults to the XDG preset library if no dir given)
    List {
        /// Directory to scan (optional)
        dir: Option<String>,
    },
    /// Set an `FxSound` effect intensity (0–100)
    Set {
        /// Effect: fidelity / ambience / surround / `dynamic_boost` / bass / loudness / crossfeed
        effect: String,
        /// Intensity 0–100
        value: u8,
    },
    /// Toggle or set daemon power
    Power {
        /// on | off
        state: String,
    },
    /// Set preamp gain in dB
    Preamp {
        /// Gain in dB (e.g. -3.5)
        // Negative values are the primary use case; without this clap reads the
        // leading '-' as an unknown flag and rejects `resonance preamp -3.5`.
        #[arg(allow_hyphen_values = true)]
        db: f64,
    },
    /// Set output dither: off | 16 | 20 | 24 (TPDF to that bit depth)
    Dither {
        /// off | 16 | 20 | 24
        depth: String,
    },
    /// Save the current settings as a named profile
    Save {
        /// Profile name
        name: String,
    },
    /// Load a saved profile by name
    Profile {
        /// Profile name
        name: String,
    },
    /// List saved profiles
    Profiles,
    /// Delete a saved profile
    RmProfile {
        /// Profile name
        name: String,
    },
    /// Map the current output device to a profile (auto-loads on output change)
    Map {
        /// Profile name
        profile: String,
    },
    /// Remove the mapping for the current output device
    Unmap,
    /// List output→profile mappings
    Maps,
    /// Set an EQ band's filter slope in dB/oct (shelves + HP/LP only)
    BandSlope {
        /// Band index (1-based, as shown in `status`)
        index: usize,
        /// Slope: 12 | 24 | 48 dB/oct
        slope: u8,
    },
    /// Reset to defaults: flat EQ, all effects off, 0 dB preamp
    Reset,
    /// Export the current EQ to an `EqualizerAPO` .txt file
    Export {
        /// Output file path (e.g. ./my-eq.txt)
        path: String,
    },
    /// List available output devices and the active one
    Devices,
    /// Set the active output device by name, or "auto" to follow the OS default
    Output {
        /// Device name (from `devices`), or "auto" to follow the system default
        name: String,
    },
    /// Store the current state into an A/B comparison slot
    Store {
        /// Slot: a | b
        slot: String,
    },
    /// Recall a previously stored A/B slot
    Recall {
        /// Slot: a | b
        slot: String,
    },
    /// Import a preset file (.fac / APO .txt) as a saved profile (does not load it)
    Import {
        /// Path to preset file
        path: String,
        /// Profile name (defaults to the file name)
        name: Option<String>,
    },
    /// Rename a saved profile
    Rename {
        /// Current profile name
        from: String,
        /// New profile name
        to: String,
    },
    /// Download an `AutoEq` headphone correction and import it as a profile
    Autoeq {
        /// Headphone name (e.g. "HD 600"); multiple words allowed
        query: Vec<String>,
    },
    /// Channel routing + per-channel EQ (N-channel support)
    Channel {
        // Optional so a bare `resonance channel` runs `info` (None → Info).
        #[command(subcommand)]
        action: Option<ChannelAction>,
    },
    /// List per-application audio streams the daemon can control
    Apps,
    /// Control one application's audio, e.g. `resonance app firefox.42 volume 150`
    App {
        /// Application key as shown by `resonance apps`
        key: String,
        #[command(subcommand)]
        action: AppAction,
    },
    /// List the output sinks (devices) the daemon can control
    Sinks,
    /// Control one output sink's volume, e.g. `resonance sink <name> volume 80`
    Sink {
        /// Sink node name as shown by `resonance sinks`
        name: String,
        #[command(subcommand)]
        action: SinkAction,
    },
    /// Send a raw shutdown signal to the daemon
    Shutdown,
    /// Manage the resonanced user service (start/stop/autostart). Backed by
    /// systemd (with an xdg-autostart fallback) on Linux, launchd on macOS, and
    /// the Run registry key on Windows.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Print shell completions
    Completions {
        /// Shell: bash | zsh | fish | elvish | powershell
        shell: Shell,
    },
}

#[derive(Subcommand)]
enum ChannelAction {
    /// Show the channel layout + active routing (default)
    Info,
    /// Swap two channels, e.g. `channel swap 0 1` (L/R swap)
    Swap { a: usize, b: usize },
    /// Clear routing — straight passthrough at the processing channel count
    Clear,
    /// Target an EQ band to a channel subset. `index` is 1-based (as shown in
    /// `status`); `channels` is a comma list of indices or names (e.g. `0,1`,
    /// `FL,FR`), or `all`.
    Band { index: usize, channels: String },
}

#[derive(Subcommand)]
enum AppAction {
    /// Set volume as a percentage, 0–400 (100 = unity; >100 boosts where supported)
    Volume { percent: u16 },
    /// Mute or unmute: on | off
    Mute { state: String },
}

#[derive(Subcommand)]
enum SinkAction {
    /// Set volume as a percentage, 0–400 (100 = unity; >100 boosts where supported)
    Volume { percent: u16 },
    /// Mute or unmute: on | off
    Mute { state: String },
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Start the daemon now (installs the user service if needed)
    Start,
    /// Stop the running daemon
    Stop,
    /// Restart the daemon
    Restart,
    /// Enable autostart at login and start now
    Enable,
    /// Disable autostart and stop now
    Disable,
    /// Write/refresh the user service unit (systemd unit or launchd plist)
    Install,
    /// Remove the user service unit (systemd unit or launchd plist)
    Uninstall,
    /// Show service install/active/enabled status (default)
    Status,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let sub = cli.cmd.unwrap_or(Sub::Status);

    // Handle completions without connecting to daemon
    if let Sub::Completions { shell } = sub {
        let mut cmd = Cli::command();
        let bin = cmd.get_name().to_string();
        generate(shell, &mut cmd, bin, &mut io::stdout());
        return Ok(());
    }

    // `autoeq` downloads + imports client-side, then asks the daemon to import.
    if let Sub::Autoeq { query } = &sub {
        let q = query.join(" ");
        if q.trim().is_empty() {
            bail!("usage: resonance autoeq <headphone name>");
        }
        let path = autoeq::run(&q)?;
        let p = Paint::auto();
        println!(
            "{} {}",
            p.dim("downloaded"),
            p.bold(&path.display().to_string())
        );
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned());
        let resp = send(Command::ImportPreset {
            path: path.to_string_lossy().into_owned(),
            name,
        })?;
        print_response(resp);
        return Ok(());
    }

    // `daemon` controls the user service (systemd on Linux, launchd on
    // macOS); it never touches the socket.
    if let Sub::Daemon { action } = &sub {
        return run_daemon(action);
    }

    // `devices` reuses GetState but renders a sink list instead of full status.
    if let Sub::Devices = sub {
        let resp = send(Command::GetState)?;
        if let Response::State(s) = resp {
            print_devices(&Paint::auto(), &s);
            return Ok(());
        }
        print_response(resp);
        return Ok(());
    }

    // All `channel` subcommands need either a state fetch (info / layout-aware
    // band targeting) or a direct IPC send; handle them together.
    if let Sub::Channel { action } = &sub {
        return run_channel(action.as_ref());
    }

    // `apps` reuses GetState but renders the per-app list; `app …` maps to a
    // direct per-app control command.
    if let Sub::Apps = sub {
        return run_apps();
    }
    if let Sub::App { key, action } = &sub {
        return run_app(key, action);
    }
    if let Sub::Sinks = sub {
        return run_sinks();
    }
    if let Sub::Sink { name, action } = &sub {
        return run_sink(name, action);
    }

    let cmd = to_ipc_command(sub)?;
    let response = send(cmd)?;
    print_response(response);
    Ok(())
}

fn to_ipc_command(sub: Sub) -> Result<Command> {
    match sub {
        Sub::Status => Ok(Command::GetState),
        Sub::Load { path } => Ok(Command::LoadPreset {
            path: absolutize(path),
        }),
        Sub::List { dir } => Ok(Command::ListPresets {
            dir: dir.map(absolutize),
        }),
        Sub::Autoeq { .. } => unreachable!(),
        Sub::Power { state } => Ok(Command::SetPower {
            enabled: parse_bool(&state)?,
        }),
        Sub::Preamp { db } => {
            if !db.is_finite() {
                bail!("preamp must be a finite number, got '{db}'");
            }
            Ok(Command::SetPreamp { db })
        }
        Sub::Dither { depth } => Ok(Command::SetDither {
            bits: parse_dither(&depth)?,
        }),
        Sub::Set { effect, value } => {
            if value > 100 {
                bail!("intensity must be 0–100, got {value}");
            }
            Ok(Command::SetEffectIntensity {
                effect: parse_effect(&effect)?,
                value: f64::from(value) / 100.0,
            })
        }
        Sub::Save { name } => Ok(Command::SaveProfile { name }),
        Sub::Profile { name } => Ok(Command::LoadProfile { name }),
        Sub::Profiles => Ok(Command::ListProfiles),
        Sub::RmProfile { name } => Ok(Command::DeleteProfile { name }),
        Sub::Map { profile } => Ok(Command::MapOutput { profile }),
        Sub::Unmap => Ok(Command::UnmapOutput),
        Sub::Maps => Ok(Command::ListMappings),
        Sub::Output { name } => {
            // `output auto` (or follow/default/system) clears the pin and tracks
            // the OS default device.
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "auto" | "follow" | "default" | "system"
            ) {
                Ok(Command::FollowSystemOutput)
            } else {
                Ok(Command::SetOutputTarget { node_name: name })
            }
        }
        Sub::BandSlope { index, slope } => {
            if index == 0 {
                bail!("band index is 1-based (see `status`)");
            }
            if !matches!(slope, 12 | 24 | 48) {
                bail!("slope must be 12, 24 or 48 dB/oct, got {slope}");
            }
            Ok(Command::SetBandSlope {
                index: index - 1,
                slope_db_oct: slope,
            })
        }
        Sub::Reset => Ok(Command::Reset),
        Sub::Export { path } => Ok(Command::ExportApo {
            path: absolutize(path),
        }),
        Sub::Store { slot } => Ok(Command::StoreSlot {
            slot: parse_slot(&slot)?,
        }),
        Sub::Recall { slot } => Ok(Command::RecallSlot {
            slot: parse_slot(&slot)?,
        }),
        Sub::Import { path, name } => Ok(Command::ImportPreset {
            path: absolutize(path),
            name,
        }),
        Sub::Rename { from, to } => Ok(Command::RenameProfile { from, to }),
        Sub::Shutdown => Ok(Command::Shutdown),
        // `Channel` is handled in `run_channel` (needs a state fetch); the others
        // are handled in `main` before this point.
        Sub::Channel { .. }
        | Sub::Daemon { .. }
        | Sub::Devices
        | Sub::Apps
        | Sub::App { .. }
        | Sub::Sinks
        | Sub::Sink { .. }
        | Sub::Completions { .. } => {
            unreachable!()
        }
    }
}

/// Handle the `channel` command group. `info` (or a bare `channel`) renders the
/// layout; `swap`/`clear` map straight to IPC; `band` fetches state first so
/// channel names resolve against the live layout and indices are range-checked.
fn run_channel(action: Option<&ChannelAction>) -> Result<()> {
    match action {
        None | Some(ChannelAction::Info) => {
            let resp = send(Command::GetState)?;
            if let Response::State(s) = resp {
                print_channels(&Paint::auto(), &s);
            } else {
                print_response(resp);
            }
            Ok(())
        }
        Some(ChannelAction::Swap { a, b }) => {
            print_response(send(Command::SwapChannels { a: *a, b: *b })?);
            Ok(())
        }
        Some(ChannelAction::Clear) => {
            print_response(send(Command::ClearRouting)?);
            Ok(())
        }
        Some(ChannelAction::Band { index, channels }) => {
            if *index == 0 {
                bail!("band index is 1-based (see `status`)");
            }
            let st = match send(Command::GetState)? {
                Response::State(s) => s,
                other => {
                    print_response(other);
                    return Ok(());
                }
            };
            let mask = parse_channel_mask(channels, &st)?;
            print_response(send(Command::SetBandChannels {
                index: index - 1,
                channels: mask,
            })?);
            Ok(())
        }
    }
}

/// `resonance apps`: fetch state and render the per-application stream list.
fn run_apps() -> Result<()> {
    match send(Command::GetState)? {
        Response::State(s) => {
            print_apps(&Paint::auto(), &s.apps);
            Ok(())
        }
        other => {
            print_response(other);
            Ok(())
        }
    }
}

/// `resonance app <key> volume|mute …`: map to a per-app control command.
fn run_app(key: &str, action: &AppAction) -> Result<()> {
    let cmd = match action {
        AppAction::Volume { percent } => {
            if *percent > 400 {
                bail!("volume must be 0–400, got {percent}");
            }
            Command::SetAppVolume {
                key: key.to_string(),
                volume: f64::from(*percent) / 100.0,
            }
        }
        AppAction::Mute { state } => Command::SetAppMute {
            key: key.to_string(),
            muted: parse_bool(state)?,
        },
    };
    print_response(send(cmd)?);
    Ok(())
}

fn print_apps(p: &Paint, apps: &[resonance_ipc::AppStream]) {
    println!("{}", p.bold("application streams"));
    if apps.is_empty() {
        println!(
            "  {}",
            p.dim("(no per-app streams reported — backend may not support it yet)")
        );
        return;
    }
    for app in apps {
        let marker = if app.active {
            p.green("●")
        } else {
            p.dim("○")
        };
        let vol = format!("{:>4.0}%", app.volume * 100.0);
        let vol = if app.muted {
            p.red("muted")
        } else {
            p.cyan(&vol)
        };
        // Friendly name first; key dimmed so it's usable in `resonance app <key>`.
        println!(
            "  {marker} {}  {}  {}",
            p.bold(&app.display_name),
            vol,
            p.dim(&app.key)
        );
    }
}

fn run_sinks() -> Result<()> {
    match send(Command::GetState)? {
        Response::State(s) => {
            print_sinks(&Paint::auto(), &s.sinks);
            Ok(())
        }
        other => {
            print_response(other);
            Ok(())
        }
    }
}

/// `resonance sink <name> volume|mute …`: map to a per-sink control command.
fn run_sink(name: &str, action: &SinkAction) -> Result<()> {
    let cmd = match action {
        SinkAction::Volume { percent } => {
            if *percent > 400 {
                bail!("volume must be 0–400, got {percent}");
            }
            Command::SetSinkVolume {
                name: name.to_string(),
                volume: f64::from(*percent) / 100.0,
            }
        }
        SinkAction::Mute { state } => Command::SetSinkMute {
            name: name.to_string(),
            muted: parse_bool(state)?,
        },
    };
    print_response(send(cmd)?);
    Ok(())
}

fn print_sinks(p: &Paint, sinks: &[resonance_ipc::SinkVolume]) {
    println!("{}", p.bold("output sinks"));
    if sinks.is_empty() {
        println!(
            "  {}",
            p.dim("(no output sinks reported — backend may not support it yet)")
        );
        return;
    }
    for sink in sinks {
        let vol = format!("{:>4.0}%", sink.volume * 100.0);
        let vol = if sink.muted {
            p.red("muted")
        } else {
            p.cyan(&vol)
        };
        let label = if sink.description.is_empty() {
            &sink.name
        } else {
            &sink.description
        };
        // Friendly description first; node name dimmed for `resonance sink <name>`.
        println!("  {}  {}  {}", p.bold(label), vol, p.dim(&sink.name));
    }
}

fn run_daemon(action: &DaemonAction) -> Result<()> {
    use resonance_ipc::service;
    let p = Paint::auto();
    if !service::manager_available() {
        bail!("{}", service::manager_unavailable_message());
    }
    match action {
        DaemonAction::Start => service::start()?,
        DaemonAction::Stop => service::stop()?,
        DaemonAction::Restart => service::restart()?,
        DaemonAction::Enable => service::enable()?,
        DaemonAction::Disable => service::disable()?,
        DaemonAction::Install => service::install()?,
        DaemonAction::Uninstall => service::uninstall()?,
        DaemonAction::Status => {}
    }
    let s = service::status();
    let yn = |b: bool, yes: &str, no: &str| {
        if b { p.green(yes) } else { p.dim(no) }
    };
    println!(
        "{}  {}  {}  {}",
        p.magenta_bold("♪ resonanced"),
        yn(s.active, "● running", "○ stopped"),
        yn(s.enabled, "autostart on", "autostart off"),
        yn(s.installed, "installed", "not installed"),
    );
    Ok(())
}

fn send(cmd: Command) -> Result<Response> {
    // No timeout: a CLI command may legitimately wait on slower daemon work
    // (preset import, AutoEq download-and-apply).
    let mut client =
        SyncClient::connect().map_err(|e| anyhow::anyhow!("cannot connect to daemon: {e}"))?;
    Ok(client.send_recv(cmd)?)
}

fn print_response(resp: Response) {
    let p = Paint::auto();
    match resp {
        Response::Ok => {}
        Response::State(s) => print_state(&p, &s),
        Response::PresetList(list) => {
            if list.is_empty() {
                println!("{}", p.dim("(none)"));
            }
            for name in list {
                println!("{name}");
            }
        }
        Response::Mappings(maps) => {
            if maps.is_empty() {
                println!("{}", p.dim("no output mappings"));
            }
            for (output, profile) in maps {
                println!("{}  {}  {}", p.cyan(&output), p.dim("→"), p.bold(&profile));
            }
        }
        Response::Imported(name) => {
            println!("{} {}", p.dim("imported as profile"), p.bold(&name));
        }
        Response::Error(e) => {
            eprintln!("{} {e}", p.red("error:"));
            std::process::exit(1);
        }
    }
}

fn print_state(p: &Paint, s: &resonance_ipc::DaemonState) {
    // Header
    let power = if s.enabled {
        p.green("● on")
    } else {
        p.red("○ off")
    };
    println!("{}  {power}", p.magenta_bold("♪ Resonance"));
    println!();

    let label = |k: &str| p.dim(&format!("{k:<8}"));

    if let Some(out) = &s.active_output {
        let tail = match &s.mapped_profile {
            Some(prof) => format!("  {} {}", p.dim("→ profile"), p.bold(prof)),
            None => String::new(),
        };
        println!("{}{}{tail}", label("output"), p.cyan(&s.sink_label(out)));
    }
    println!(
        "{}{}",
        label("preset"),
        s.current_preset.as_deref().unwrap_or("none")
    );
    println!("{}{:+.1} dB", label("preamp"), s.preamp_db);
    let resampling = (s.capture_rate - s.sample_rate).abs() > 1.0;
    let rate = if resampling {
        format!(
            "{:.0} Hz · {}ch  (resampling {:.0}→{:.0} Hz)",
            s.sample_rate, s.channels, s.capture_rate, s.sample_rate
        )
    } else {
        format!("{:.0} Hz · {}ch", s.sample_rate, s.channels)
    };
    println!("{}{}", label("format"), rate);
    // Routing: only shown when non-trivial (a remap or a changed output width).
    if s.routing.is_some() || (s.out_channels != 0 && s.out_channels != s.channels) {
        let out = if s.out_channels == 0 {
            s.channels
        } else {
            s.out_channels
        };
        println!(
            "{}{} → {} ch (custom remap)",
            label("route"),
            s.channels,
            out
        );
    }

    // Live meters.
    let m = &s.meters;
    let dbfs = |lin: f32| {
        if lin <= 1e-6 {
            "-inf".to_string()
        } else {
            format!("{:+.1}", 20.0 * lin.log10())
        }
    };
    let clip = if m.clip {
        p.red(" CLIP")
    } else {
        String::new()
    };
    println!(
        "{}in {} dB  out {} dB{}",
        label("levels"),
        dbfs(m.in_peak),
        dbfs(m.out_peak),
        clip
    );
    println!(
        "{}{:.0}% ({} µs/block)",
        label("dsp"),
        m.dsp_load * 100.0,
        m.dsp_frame_us
    );

    println!("{}{}", label("dither"), dither_label(s.dither_bits));

    // Effects with intensity bars. Iterate `FxEffectId::ALL` so new effects
    // (Loudness, Crossfeed, …) show up automatically and stay in chain order.
    println!();
    println!("{}", p.bold("effects"));
    for id in FxEffectId::ALL {
        let (int, on) = s.effects.get(id);
        let pct = (int * 100.0).round() as i32;
        let state = if on { p.green("on ") } else { p.dim("off") };
        println!(
            "  {:<14} {} {:>4}%  {state}",
            effect_cli_name(id),
            p.cyan(&bar(int)),
            pct
        );
    }

    // EQ bands
    if !s.bands.is_empty() {
        println!();
        println!(
            "{} {}",
            p.bold("bands"),
            p.dim(&format!("({})", s.bands.len()))
        );
        for (i, b) in s.bands.iter().enumerate() {
            let state = if b.enabled {
                p.green("on ")
            } else {
                p.dim("off")
            };
            let chlabel = mask_label(b.channels, &s.channel_layout, s.channels);
            // Slope only applies to shelves + HP/LP; hide it for single-biquad types.
            let slope_tail = if b.band_type.uses_slope() {
                format!("  {}", p.dim(&format!("{} dB/oct", b.slope_db_oct)))
            } else {
                String::new()
            };
            let ch_tail = if chlabel.is_empty() {
                String::new()
            } else {
                format!("  {}", p.dim(&chlabel))
            };
            let tail = format!("{slope_tail}{ch_tail}");
            println!(
                "  {:>2}  {}  {:>8.1} Hz  {:+5.1} dB  Q {:>4.2}  {state}{tail}",
                i + 1,
                p.cyan(b.band_type.abbrev()),
                b.freq,
                b.gain_db,
                b.q,
            );
        }
    }
}

/// Render the channel layout + active routing (the `channel info` view).
fn print_channels(p: &Paint, s: &resonance_ipc::DaemonState) {
    let out = if s.out_channels == 0 {
        s.channels
    } else {
        s.out_channels
    };
    println!(
        "{} {}",
        p.bold("channels"),
        p.dim(&format!("(in {} → out {out})", s.channels))
    );
    for (i, name) in s.channel_layout.iter().enumerate() {
        println!("  {:>2}  {}", i, p.cyan(name));
    }
    println!();
    match &s.routing {
        Some(rm) => println!(
            "{} {}",
            p.bold("routing"),
            p.dim(&format!("{}→{} matrix (custom remap)", rm.in_ch, rm.out_ch))
        ),
        None => println!("{} {}", p.bold("routing"), p.dim("passthrough")),
    }
    // Per-channel band targets, if any.
    let targeted: Vec<(usize, String)> = s
        .bands
        .iter()
        .enumerate()
        .filter_map(|(i, b)| {
            let l = mask_label(b.channels, &s.channel_layout, s.channels);
            (!l.is_empty()).then_some((i + 1, l))
        })
        .collect();
    if !targeted.is_empty() {
        println!();
        println!("{}", p.bold("per-channel bands"));
        for (idx, l) in targeted {
            println!("  {:>2}  {}", idx, p.cyan(&l));
        }
    }
}

fn print_devices(p: &Paint, s: &resonance_ipc::DaemonState) {
    println!("{}", p.bold("output sinks"));
    if s.available_sinks.is_empty() {
        println!("  {}", p.dim("(no output devices reported yet)"));
    }
    for sink in &s.available_sinks {
        let active = s.active_output.as_deref() == Some(sink.as_str());
        let preferred = s.preferred_output.as_deref() == Some(sink.as_str());
        let marker = if active { p.green("●") } else { p.dim("○") };
        let tail = if preferred {
            format!("  {}", p.dim("(preferred)"))
        } else {
            String::new()
        };
        let label = s.sink_label(sink);
        // Friendly name first; keep the node.name dimmed so it's still usable in `set-output`.
        let id = if label == *sink {
            String::new()
        } else {
            format!("  {}", p.dim(sink))
        };
        println!("  {marker} {}{id}{tail}", p.cyan(&label));
    }
}

/// 12-cell intensity bar; fills on absolute value so bipolar effects still read.
fn bar(frac: f64) -> String {
    const WIDTH: usize = 12;
    let filled = ((frac.abs().clamp(0.0, 1.0)) * WIDTH as f64).round() as usize;
    let filled = filled.min(WIDTH);
    format!("{}{}", "█".repeat(filled), "░".repeat(WIDTH - filled))
}

/// Minimal ANSI colorizer; no-ops when stdout is not a terminal.
struct Paint {
    on: bool,
}

impl Paint {
    fn auto() -> Self {
        Self {
            on: std::io::stdout().is_terminal(),
        }
    }
    fn wrap(&self, code: &str, s: &str) -> String {
        if self.on {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
    fn dim(&self, s: &str) -> String {
        self.wrap("2", s)
    }
    fn bold(&self, s: &str) -> String {
        self.wrap("1", s)
    }
    fn red(&self, s: &str) -> String {
        self.wrap("31", s)
    }
    fn green(&self, s: &str) -> String {
        self.wrap("32", s)
    }
    fn cyan(&self, s: &str) -> String {
        self.wrap("36", s)
    }
    fn magenta_bold(&self, s: &str) -> String {
        self.wrap("1;35", s)
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Resolve a path against the *client's* working directory so the daemon — which
/// runs from a different cwd (often `/` under systemd) — operates on the file the
/// user meant. Leaves absolute paths untouched; the shell already expands `~`.
fn absolutize(path: String) -> String {
    let p = std::path::Path::new(&path);
    if p.is_absolute() {
        return path;
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(p).to_string_lossy().into_owned(),
        Err(_) => path,
    }
}

fn parse_bool(s: &str) -> Result<bool> {
    match s.to_ascii_lowercase().as_str() {
        "on" | "1" | "true" => Ok(true),
        "off" | "0" | "false" => Ok(false),
        _ => bail!("expected on/off, got '{s}'"),
    }
}

fn parse_slot(s: &str) -> Result<resonance_ipc::AbSlot> {
    match s.to_ascii_lowercase().as_str() {
        "a" => Ok(resonance_ipc::AbSlot::A),
        "b" => Ok(resonance_ipc::AbSlot::B),
        _ => bail!("expected slot a or b, got '{s}'"),
    }
}

/// Parse a channel spec against the live state: `all`, or a comma list of 0-based
/// indices / names (resolved against the device's actual layout, so names match
/// what `channel info` shows even on 4/5/7-channel devices). Indices/names beyond
/// the device's channel count are rejected.
fn parse_channel_mask(spec: &str, st: &resonance_ipc::DaemonState) -> Result<ChannelMask> {
    let s = spec.trim();
    if s.eq_ignore_ascii_case("all") {
        return Ok(ChannelMask::ALL);
    }
    let channels = st.channels;
    let mut idxs = Vec::new();
    for tok in s.split(',') {
        let t = tok.trim();
        if t.is_empty() {
            continue;
        }
        let idx = match t.parse::<usize>() {
            Ok(n) => n,
            Err(_) => resolve_name(t, st).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown channel '{t}' (use an index or a name from `channel info`)"
                )
            })?,
        };
        if channels != 0 && idx >= channels {
            bail!("channel {idx} out of range (device has {channels} channels)");
        }
        idxs.push(idx);
    }
    if idxs.is_empty() {
        bail!("no channels specified");
    }
    Ok(ChannelMask::from_indices(idxs))
}

/// Resolve a channel name to its index: prefer the live layout (count-correct on
/// any device), falling back to the fixed WAVE-order aliases.
fn resolve_name(name: &str, st: &resonance_ipc::DaemonState) -> Option<usize> {
    st.channel_layout
        .iter()
        .position(|l| l.eq_ignore_ascii_case(name))
        .or_else(|| channel_name_index(name))
}

/// Fixed WAVE-order name→index fallback (used only when the live layout lacks the
/// name). Aliases: FL/L, LFE/SUB, RL/BL, RR/BR.
fn channel_name_index(s: &str) -> Option<usize> {
    Some(match s.to_ascii_uppercase().as_str() {
        "FL" | "L" | "MONO" => 0,
        "FR" | "R" => 1,
        "FC" | "C" => 2,
        "LFE" | "SUB" => 3,
        "RL" | "BL" => 4,
        "RR" | "BR" => 5,
        "SL" => 6,
        "SR" => 7,
        _ => return None,
    })
}

/// Short `[FL,FR]`-style label for a band's channel target — empty when global.
fn mask_label(m: ChannelMask, layout: &[String], channels: usize) -> String {
    if channels == 0 || m.is_global(channels) {
        return String::new();
    }
    let names: Vec<String> = (0..channels)
        .filter(|&i| m.contains(i))
        .map(|i| layout.get(i).cloned().unwrap_or_else(|| format!("ch{i}")))
        .collect();
    // A non-global mask that selects no in-range channel is degenerate (e.g. an
    // out-of-range spec); show it explicitly rather than an empty `[]`.
    if names.is_empty() {
        return "[none]".to_string();
    }
    format!("[{}]", names.join(","))
}

/// Lowercase name the CLI prints and accepts for an effect (matches
/// `parse_effect`'s keys, so `status` output round-trips back into `set`).
fn effect_cli_name(id: FxEffectId) -> &'static str {
    match id {
        FxEffectId::Fidelity => "fidelity",
        FxEffectId::Ambience => "ambience",
        FxEffectId::Surround => "surround",
        FxEffectId::DynamicBoost => "dynamic_boost",
        FxEffectId::Bass => "bass",
        FxEffectId::Loudness => "loudness",
        FxEffectId::Crossfeed => "crossfeed",
    }
}

/// Human-readable dither state for `status` (`None` = off).
fn dither_label(bits: Option<u32>) -> String {
    match bits {
        None => "off".to_string(),
        Some(b) => format!("{b}-bit"),
    }
}

/// Parse a `dither` argument (`off` | `16` | `20` | `24`) into `Option<u32>`.
fn parse_dither(s: &str) -> Result<Option<u32>> {
    match s.to_ascii_lowercase().as_str() {
        "off" | "none" | "0" => Ok(None),
        "16" => Ok(Some(16)),
        "20" => Ok(Some(20)),
        "24" => Ok(Some(24)),
        _ => bail!("dither must be off/16/20/24, got '{s}'"),
    }
}

fn parse_effect(s: &str) -> Result<FxEffectId> {
    match s {
        "fidelity" => Ok(FxEffectId::Fidelity),
        "ambience" => Ok(FxEffectId::Ambience),
        "surround" => Ok(FxEffectId::Surround),
        "dynamic_boost" | "dynamic" => Ok(FxEffectId::DynamicBoost),
        "bass" => Ok(FxEffectId::Bass),
        "loudness" => Ok(FxEffectId::Loudness),
        "crossfeed" => Ok(FxEffectId::Crossfeed),
        _ => bail!(
            "unknown effect '{s}': use fidelity/ambience/surround/dynamic_boost/bass/loudness/crossfeed"
        ),
    }
}
