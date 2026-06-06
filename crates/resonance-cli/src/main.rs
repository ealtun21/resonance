mod autoeq;

use anyhow::{Result, bail};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use resonance_ipc::{
    Command, FxEffectId, Response,
    transport::{read_response, write_command},
};
use std::{
    env,
    io::{self, BufReader, BufWriter, IsTerminal, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
};

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
    /// Set an FxSound effect intensity (0–100)
    Set {
        /// Effect name: fidelity / ambience / surround / dynamic_boost / bass
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
        db: f64,
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
    /// Reset to defaults: flat EQ, all effects off, 0 dB preamp
    Reset,
    /// Export the current EQ to an EqualizerAPO .txt file
    Export {
        /// Output file path (e.g. ./my-eq.txt)
        path: String,
    },
    /// List available PipeWire output sinks and the active one
    Devices,
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
    /// Download an AutoEq headphone correction and import it as a profile
    Autoeq {
        /// Headphone name (e.g. "HD 600"); multiple words allowed
        query: Vec<String>,
    },
    /// Send a raw shutdown signal to the daemon
    Shutdown,
    /// Manage the resonanced systemd user service (start/stop/autostart)
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
    /// Write/refresh the systemd user unit file
    Install,
    /// Remove the systemd user unit file
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
        return print_response(resp);
    }

    // `daemon` controls the systemd user service; it never touches the socket.
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
        return print_response(resp);
    }

    let cmd = to_ipc_command(sub)?;
    let response = send(cmd)?;
    print_response(response)
}

fn to_ipc_command(sub: Sub) -> Result<Command> {
    match sub {
        Sub::Status => Ok(Command::GetState),
        Sub::Load { path } => Ok(Command::LoadPreset { path }),
        Sub::List { dir } => Ok(Command::ListPresets { dir }),
        Sub::Autoeq { .. } => unreachable!(),
        Sub::Power { state } => Ok(Command::SetPower {
            enabled: parse_bool(&state)?,
        }),
        Sub::Preamp { db } => Ok(Command::SetPreamp { db }),
        Sub::Set { effect, value } => Ok(Command::SetEffectIntensity {
            effect: parse_effect(&effect)?,
            value: value.min(100) as f64 / 100.0,
        }),
        Sub::Save { name } => Ok(Command::SaveProfile { name }),
        Sub::Profile { name } => Ok(Command::LoadProfile { name }),
        Sub::Profiles => Ok(Command::ListProfiles),
        Sub::RmProfile { name } => Ok(Command::DeleteProfile { name }),
        Sub::Map { profile } => Ok(Command::MapOutput { profile }),
        Sub::Unmap => Ok(Command::UnmapOutput),
        Sub::Maps => Ok(Command::ListMappings),
        Sub::Reset => Ok(Command::Reset),
        Sub::Export { path } => Ok(Command::ExportApo { path }),
        Sub::Store { slot } => Ok(Command::StoreSlot {
            slot: parse_slot(&slot)?,
        }),
        Sub::Recall { slot } => Ok(Command::RecallSlot {
            slot: parse_slot(&slot)?,
        }),
        Sub::Import { path, name } => Ok(Command::ImportPreset { path, name }),
        Sub::Rename { from, to } => Ok(Command::RenameProfile { from, to }),
        Sub::Shutdown => Ok(Command::Shutdown),
        Sub::Daemon { .. } | Sub::Devices | Sub::Completions { .. } => unreachable!(),
    }
}

fn run_daemon(action: &DaemonAction) -> Result<()> {
    use resonance_ipc::service;
    let p = Paint::auto();
    if !service::systemd_available() {
        bail!("systemctl --user is not available; cannot manage the daemon service");
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
    let path = socket_path();
    let stream = UnixStream::connect(&path)
        .map_err(|e| anyhow::anyhow!("cannot connect to daemon at {}: {e}", path.display()))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = BufWriter::new(stream);

    write_command(&mut writer, &cmd)?;
    writer.flush()?;
    Ok(read_response(&mut reader)?)
}

fn print_response(resp: Response) -> Result<()> {
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
        Response::StateChanged(_) => {}
    }
    Ok(())
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
    println!(
        "{}{:.0} Hz · {}ch",
        label("format"),
        s.sample_rate,
        s.channels
    );

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

    // Effects with intensity bars
    println!();
    println!("{}", p.bold("effects"));
    let e = &s.effects;
    let row = |name: &str, int: f64, on: bool| {
        let pct = (int * 100.0).round() as i32;
        let state = if on { p.green("on ") } else { p.dim("off") };
        println!("  {:<14} {} {:>4}%  {state}", name, p.cyan(&bar(int)), pct);
    };
    row("fidelity", e.fidelity_intensity, e.fidelity_enabled);
    row("ambience", e.ambience_intensity, e.ambience_enabled);
    row("surround", e.surround_intensity, e.surround_enabled);
    row(
        "dynamic_boost",
        e.dynamic_boost_intensity,
        e.dynamic_boost_enabled,
    );
    row("bass", e.bass_intensity, e.bass_enabled);

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
            println!(
                "  {:>2}  {}  {:>8.1} Hz  {:+5.1} dB  Q {:>4.2}  {state}",
                i + 1,
                p.cyan(b.band_type.abbrev()),
                b.freq,
                b.gain_db,
                b.q,
            );
        }
    }
}

fn print_devices(p: &Paint, s: &resonance_ipc::DaemonState) {
    println!("{}", p.bold("output sinks"));
    if s.available_sinks.is_empty() {
        println!("  {}", p.dim("(none reported by PipeWire yet)"));
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

fn parse_bool(s: &str) -> Result<bool> {
    match s {
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

fn parse_effect(s: &str) -> Result<FxEffectId> {
    match s {
        "fidelity" => Ok(FxEffectId::Fidelity),
        "ambience" => Ok(FxEffectId::Ambience),
        "surround" => Ok(FxEffectId::Surround),
        "dynamic_boost" | "dynamic" => Ok(FxEffectId::DynamicBoost),
        "bass" => Ok(FxEffectId::Bass),
        _ => bail!("unknown effect '{s}': use fidelity/ambience/surround/dynamic_boost/bass"),
    }
}

fn socket_path() -> PathBuf {
    if let Ok(p) = env::var(resonance_ipc::SOCKET_PATH_ENV) {
        return PathBuf::from(p);
    }
    let runtime = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime).join(resonance_ipc::DEFAULT_SOCKET_FILENAME)
}
