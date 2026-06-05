use crate::config::{self, Mappings, Profile};
use crate::state::{AudioCommand, SharedState};
use anyhow::Result;
use resonance_dsp::chain::ProcessorChain;
use resonance_dsp::filter::{ApoFilter, FilterError, FilterType};
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

        Command::SaveProfile { name } => {
            let profile = Profile::from_state(&state.snapshot());
            match config::save_profile(&name, &profile) {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error(e),
            }
        }

        Command::LoadProfile { name } => match load_profile(&name, state) {
            Ok(()) => {
                state.0.lock().unwrap().current_preset = Some(name);
                Response::Ok
            }
            Err(e) => Response::Error(e),
        },

        Command::DeleteProfile { name } => match config::delete_profile(&name) {
            Ok(()) => Response::Ok,
            Err(e) => Response::Error(e),
        },

        Command::ListProfiles => Response::PresetList(config::list_profiles()),

        Command::MapOutput { profile } => {
            let Some(output) = state.0.lock().unwrap().active_output.clone() else {
                return Response::Error("no active output device detected yet".to_string());
            };
            if config::load_profile(&profile).is_err() {
                return Response::Error(format!("profile '{profile}' not found"));
            }
            let mut maps = Mappings::load();
            maps.set(output, profile.clone());
            match maps.save() {
                Ok(()) => {
                    // Apply immediately so the mapping takes effect now.
                    if load_profile(&profile, state).is_ok() {
                        state.0.lock().unwrap().mapped_profile = Some(profile);
                    }
                    Response::Ok
                }
                Err(e) => Response::Error(e),
            }
        }

        Command::UnmapOutput => {
            let Some(output) = state.0.lock().unwrap().active_output.clone() else {
                return Response::Error("no active output device detected yet".to_string());
            };
            let mut maps = Mappings::load();
            maps.remove(&output);
            match maps.save() {
                Ok(()) => {
                    state.0.lock().unwrap().mapped_profile = None;
                    Response::Ok
                }
                Err(e) => Response::Error(e),
            }
        }

        Command::ListMappings => Response::Mappings(Mappings::load().list()),

        Command::SetBand {
            index,
            freq,
            gain_db,
            q,
        } => {
            state.send(
                AudioCommand::SetBand {
                    index,
                    freq,
                    gain_db,
                    q,
                },
                |chain| {
                    if let Some(f) = chain.filters.get(index) {
                        let (ft, en) = (f.filter_type, f.enabled);
                        if let Ok(new_f) = build_band(chain, ft, freq, gain_db, q, en) {
                            chain.filters[index] = new_f;
                        }
                    }
                },
            );
            Response::Ok
        }

        Command::SetBandEnabled { index, enabled } => {
            state.send(AudioCommand::SetBandEnabled { index, enabled }, |chain| {
                if let Some(f) = chain.filters.get_mut(index) {
                    f.enabled = enabled;
                }
            });
            Response::Ok
        }

        Command::AddBand {
            band_type,
            freq,
            gain_db,
            q,
        } => {
            state.send(
                AudioCommand::AddBand {
                    band_type,
                    freq,
                    gain_db,
                    q,
                },
                |chain| {
                    if let Ok(nf) = build_band(chain, band_type.into(), freq, gain_db, q, true) {
                        chain.filters.push(nf);
                    }
                },
            );
            Response::Ok
        }

        Command::RemoveBand { index } => {
            state.send(AudioCommand::RemoveBand { index }, |chain| {
                if index < chain.filters.len() {
                    chain.filters.remove(index);
                }
            });
            Response::Ok
        }

        Command::SetBandType { index, band_type } => {
            state.send(AudioCommand::SetBandType { index, band_type }, |chain| {
                if let Some(f) = chain.filters.get(index) {
                    let (freq, gain_db, q, en) = (f.freq, f.gain_db, f.q, f.enabled);
                    if let Ok(nf) = build_band(chain, band_type.into(), freq, gain_db, q, en) {
                        chain.filters[index] = nf;
                    }
                }
            });
            Response::Ok
        }

        Command::SetOutputTarget { node_name } => {
            let route_tx = state.0.lock().unwrap().route_tx.clone();
            let _ = route_tx.send(node_name.clone());
            state.0.lock().unwrap().preferred_output = Some(node_name);
            Response::Ok
        }

        Command::Shutdown => {
            info!("shutdown requested");
            std::process::exit(0);
        }

        _ => Response::Error("unhandled command".to_string()),
    }
}

/// Build an `ApoFilter` matching the chain's sample rate / channel count.
fn build_band(
    chain: &ProcessorChain,
    filter_type: FilterType,
    freq: f64,
    gain_db: f64,
    q: f64,
    enabled: bool,
) -> Result<ApoFilter, FilterError> {
    ApoFilter::builder()
        .filter_type(filter_type)
        .freq(freq)
        .gain_db(gain_db)
        .q(q)
        .enabled(enabled)
        .channels(chain.channels)
        .sample_rate(chain.sample_rate)
        .build()
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
    // Build chain twice: one for RT thread, one to update the shadow (GetState reads shadow)
    let chain_rt = preset.clone().into_chain(channels, sr);
    let chain_shadow = preset.into_chain(channels, sr);
    state.replace_chain(chain_rt, chain_shadow);
    Ok(())
}

/// Load a named profile from the config dir and apply it to the chain.
fn load_profile(name: &str, state: &SharedState) -> Result<(), String> {
    let profile = config::load_profile(name)?;
    let (sr, channels) = {
        let inner = state.0.lock().unwrap();
        (inner.chain.sample_rate, inner.chain.channels)
    };
    let chain_rt = profile.clone().into_chain(channels, sr);
    let chain_shadow = profile.into_chain(channels, sr);
    state.replace_chain(chain_rt, chain_shadow);
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
