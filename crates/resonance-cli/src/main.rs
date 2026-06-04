use anyhow::{Result, bail};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use resonance_ipc::{
    Command, FxEffectId, Response,
    transport::{read_response, write_command},
};
use std::{
    env,
    io::{self, BufReader, BufWriter, Write},
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
    /// List preset files in a directory
    List {
        /// Directory to scan
        dir: String,
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
    /// Watch a preset file and auto-reload on change
    Watch {
        /// Path to preset file
        path: String,
    },
    /// Stop watching the current preset file
    Unwatch {
        /// Path to stop watching
        path: String,
    },
    /// Send a raw shutdown signal to the daemon
    Shutdown,
    /// Print shell completions
    Completions {
        /// Shell: bash | zsh | fish | elvish | powershell
        shell: Shell,
    },
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

    let cmd = to_ipc_command(sub)?;
    let response = send(cmd)?;
    print_response(response)
}

fn to_ipc_command(sub: Sub) -> Result<Command> {
    match sub {
        Sub::Status => Ok(Command::GetState),
        Sub::Load { path } => Ok(Command::LoadPreset { path }),
        Sub::List { dir } => Ok(Command::ListPresets { dir }),
        Sub::Power { state } => Ok(Command::SetPower {
            enabled: parse_bool(&state)?,
        }),
        Sub::Preamp { db } => Ok(Command::SetPreamp { db }),
        Sub::Set { effect, value } => Ok(Command::SetEffectIntensity {
            effect: parse_effect(&effect)?,
            value: value.min(100) as f64 / 100.0,
        }),
        Sub::Watch { path } => Ok(Command::WatchPreset { path }),
        Sub::Unwatch { path } => Ok(Command::UnwatchPreset { path }),
        Sub::Shutdown => Ok(Command::Shutdown),
        Sub::Completions { .. } => unreachable!(),
    }
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
    match resp {
        Response::Ok => {}
        Response::State(s) => {
            let power = if s.enabled { "on" } else { "off" };
            let preset = s.current_preset.as_deref().unwrap_or("none");
            let watched = s.watched_preset.as_deref().unwrap_or("-");
            println!("power:       {power}");
            println!("preset:      {preset}");
            println!("watching:    {watched}");
            println!("preamp:      {:+.1} dB", s.preamp_db);
            println!("sample_rate: {:.0} Hz  {}ch", s.sample_rate, s.channels);
            println!();
            println!("effects:");
            let e = &s.effects;
            let row = |name: &str, int: f64, on: bool| {
                println!(
                    "  {name:<14} {:3.0}%  {}",
                    int * 100.0,
                    if on { "on" } else { "off" }
                );
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
            if !s.bands.is_empty() {
                println!();
                println!("bands ({}):", s.bands.len());
                for (i, b) in s.bands.iter().enumerate() {
                    println!(
                        "  [{:2}] {:8.1} Hz  {:+.1} dB  Q={:.3}  {}",
                        i + 1,
                        b.freq,
                        b.gain_db,
                        b.q,
                        if b.enabled { "on" } else { "off" }
                    );
                }
            }
        }
        Response::PresetList(list) => {
            for p in list {
                println!("{p}");
            }
        }
        Response::Error(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        Response::StateChanged(_) => {}
    }
    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn parse_bool(s: &str) -> Result<bool> {
    match s {
        "on" | "1" | "true" => Ok(true),
        "off" | "0" | "false" => Ok(false),
        _ => bail!("expected on/off, got '{s}'"),
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
