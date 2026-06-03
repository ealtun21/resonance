use anyhow::{Result, bail};
use resonance_ipc::{Command, FxEffectId, Response};
use std::{
    env,
    io::{BufReader, BufWriter},
    os::unix::net::UnixStream,
    path::PathBuf,
};

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = parse_args(&args)?;

    let sock = socket_path();
    let stream = UnixStream::connect(&sock)
        .map_err(|e| anyhow::anyhow!("cannot connect to daemon at {}: {e}", sock.display()))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = BufWriter::new(stream);

    resonance_ipc::transport::write_command(&mut writer, &cmd)?;
    let resp = resonance_ipc::transport::read_response(&mut reader)?;

    match resp {
        Response::Ok => {}
        Response::State(s) => {
            println!("power: {}", if s.enabled { "on" } else { "off" });
            println!("preamp: {:.1} dB", s.preamp_db);
            println!("preset: {}", s.current_preset.as_deref().unwrap_or("none"));
            println!("sample_rate: {}", s.sample_rate);
            println!("effects:");
            println!(
                "  fidelity:      {:.0}%  ({})",
                s.effects.fidelity_intensity * 100.0,
                if s.effects.fidelity_enabled {
                    "on"
                } else {
                    "off"
                }
            );
            println!(
                "  ambience:      {:.0}%  ({})",
                s.effects.ambience_intensity * 100.0,
                if s.effects.ambience_enabled {
                    "on"
                } else {
                    "off"
                }
            );
            println!(
                "  surround:      {:.0}%  ({})",
                s.effects.surround_intensity * 100.0,
                if s.effects.surround_enabled {
                    "on"
                } else {
                    "off"
                }
            );
            println!(
                "  dynamic_boost: {:.0}%  ({})",
                s.effects.dynamic_boost_intensity * 100.0,
                if s.effects.dynamic_boost_enabled {
                    "on"
                } else {
                    "off"
                }
            );
            println!(
                "  bass:          {:.0}%  ({})",
                s.effects.bass_intensity * 100.0,
                if s.effects.bass_enabled { "on" } else { "off" }
            );
            println!("bands ({}):", s.bands.len());
            for (i, b) in s.bands.iter().enumerate() {
                println!(
                    "  [{i}] {:.1} Hz  {:+.1} dB  Q={:.3}  {}",
                    b.freq,
                    b.gain_db,
                    b.q,
                    if b.enabled { "on" } else { "off" }
                );
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

fn parse_args(args: &[String]) -> Result<Command> {
    match args {
        [] => Ok(Command::GetState),
        [sub] if sub == "status" => Ok(Command::GetState),
        [sub] if sub == "shutdown" => Ok(Command::Shutdown),
        [sub, path] if sub == "load" => Ok(Command::LoadPreset { path: path.clone() }),
        [sub, dir] if sub == "list" => Ok(Command::ListPresets { dir: dir.clone() }),
        [sub, val] if sub == "power" => Ok(Command::SetPower {
            enabled: parse_bool(val)?,
        }),
        [sub, val] if sub == "preamp" => Ok(Command::SetPreamp {
            db: val.parse::<f64>()?,
        }),
        [sub, effect, val] if sub == "set" => Ok(Command::SetEffectIntensity {
            effect: parse_effect(effect)?,
            value: val.parse::<f64>()? / 100.0,
        }),
        _ => bail!(
            "usage: resonance [status|load <path>|list <dir>|power on|off|preamp <db>|set <effect> <0-100>|shutdown]"
        ),
    }
}

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
