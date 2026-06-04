use crate::state::{AudioCommand, SharedState};
use anyhow::Result;
use resonance_ipc::{Command, Response};
use resonance_preset::{apo::parse_apo, fac::parse_fac};
use std::{env, path::PathBuf};
use tokio::net::{UnixListener, UnixStream};
use tracing::{error, info, warn};

pub async fn run(state: SharedState) -> Result<()> {
    let sock_path = socket_path();
    let _ = tokio::fs::remove_file(&sock_path).await;

    let listener = UnixListener::bind(&sock_path)?;
    info!("IPC listening on {}", sock_path.display());

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, state).await {
                        warn!("client error: {e}");
                    }
                });
            }
            Err(e) => {
                error!("accept error: {e}");
            }
        }
    }
}

async fn handle_client(stream: UnixStream, state: SharedState) -> Result<()> {
    let (reader, writer) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(reader);
    let mut writer = tokio::io::BufWriter::new(writer);

    loop {
        let cmd = match read_command_async(&mut reader).await {
            Ok(c) => c,
            Err(_) => break,
        };

        let response = dispatch(cmd, &state).await;
        if let Err(e) = write_response_async(&mut writer, &response).await {
            warn!("write error: {e}");
            break;
        }
    }
    Ok(())
}

async fn dispatch(cmd: Command, state: &SharedState) -> Response {
    match cmd {
        Command::GetState => Response::State(state.snapshot()),

        Command::SetPower { enabled } => {
            state.send(AudioCommand::SetPower(enabled), |chain| {
                chain.enabled = enabled;
            });
            Response::Ok
        }

        Command::SetPreamp { db } => {
            state.send(AudioCommand::SetPreamp(db), |chain| {
                chain.preamp_db = db;
            });
            Response::Ok
        }

        Command::SetEffectIntensity { effect, value } => {
            let fx: resonance_dsp::chain::FxEffect = effect.into();
            state.send(
                AudioCommand::SetEffectIntensity { effect: fx, value },
                |chain| {
                    chain.set_effect_intensity(fx, value);
                },
            );
            Response::Ok
        }

        Command::SetEffectEnabled { effect, enabled } => {
            let fx: resonance_dsp::chain::FxEffect = effect.into();
            state.send(
                AudioCommand::SetEffectEnabled {
                    effect: fx,
                    on: enabled,
                },
                |chain| {
                    chain.set_effect_enabled(fx, enabled);
                },
            );
            Response::Ok
        }

        Command::LoadPreset { path } => match load_preset(&path, state) {
            Ok(_) => {
                state.0.lock().unwrap().current_preset = Some(path);
                Response::Ok
            }
            Err(e) => Response::Error(e),
        },

        Command::ListPresets { dir } => Response::PresetList(list_presets(&dir)),

        Command::WatchPreset { path } => {
            state.0.lock().unwrap().watched_preset = Some(path);
            Response::Ok
        }

        Command::UnwatchPreset { .. } => {
            state.0.lock().unwrap().watched_preset = None;
            Response::Ok
        }

        Command::Shutdown => {
            info!("shutdown requested");
            std::process::exit(0);
        }

        _ => Response::Error("unhandled command".to_string()),
    }
}

fn load_preset(path: &str, state: &SharedState) -> Result<(), String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let preset = if path.ends_with(".fac") {
        parse_fac(&content).map_err(|e| e.to_string())?
    } else {
        parse_apo(&content).map_err(|e| e.to_string())?
    };

    let (sr, channels) = {
        let inner = state.0.lock().unwrap();
        (inner.chain.sample_rate, inner.chain.channels)
    };
    let new_chain = preset.into_chain(channels, sr);
    state.send(AudioCommand::ReplaceChain(Box::new(new_chain)), |_| {});
    Ok(())
}

fn list_presets(dir: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let n = name.to_string_lossy();
            n.ends_with(".fac") || n.ends_with(".txt")
        })
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect()
}

fn socket_path() -> PathBuf {
    if let Ok(p) = env::var(resonance_ipc::SOCKET_PATH_ENV) {
        return PathBuf::from(p);
    }
    let runtime = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime).join(resonance_ipc::DEFAULT_SOCKET_FILENAME)
}

async fn read_command_async(
    reader: &mut tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
) -> Result<Command> {
    use tokio::io::AsyncReadExt;
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    Ok(postcard::from_bytes(&buf)?)
}

async fn write_response_async(
    writer: &mut tokio::io::BufWriter<tokio::net::unix::OwnedWriteHalf>,
    resp: &Response,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let bytes = postcard::to_stdvec(resp)?;
    let len = (bytes.len() as u32).to_le_bytes();
    writer.write_all(&len).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}
