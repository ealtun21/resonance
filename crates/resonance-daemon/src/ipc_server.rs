use crate::config::{self, KnownSinks, Mappings, Profile};
use crate::state::{AudioCommand, SharedState};
use anyhow::Result;
use resonance_dsp::chain::ProcessorChain;
use resonance_dsp::filter::{ApoFilter, FilterError, FilterType};
use resonance_ipc::{AbSlot, BandType, Command, Response};
use resonance_preset::model::{ApoFilterType, EqBand};
use resonance_preset::{
    apo::{parse_apo, write_apo},
    fac::parse_fac,
};
use tokio::net::{UnixListener, UnixStream};
use tracing::{error, info, warn};

pub async fn run(state: SharedState) -> Result<()> {
    let sock_path = crate::shutdown::socket_path();
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

        Command::LoadPreset { path } => {
            // Parse off the runtime (GraphicEQ files curve-fit); apply once parsed.
            let p = path.clone();
            match tokio::task::spawn_blocking(move || parse_preset_file(&p)).await {
                Ok(Ok(preset)) => {
                    apply_preset(preset, state);
                    state.0.lock().unwrap().current_preset = Some(path);
                    Response::Ok
                }
                Ok(Err(e)) => Response::Error(e),
                Err(e) => Response::Error(format!("load task failed: {e}")),
            }
        }

        Command::ImportPreset { path, name } => {
            // Parsing a preset can be expensive — a GraphicEQ `.txt` runs a
            // curve-fit optimisation. Do it (and the file IO) on a blocking
            // thread so the async IPC loop keeps serving other clients.
            tokio::task::spawn_blocking(move || import_preset(path, name))
                .await
                .unwrap_or_else(|e| Response::Error(format!("import task failed: {e}")))
        }

        Command::RenameProfile { from, to } => match config::rename_profile(&from, &to) {
            Ok(()) => Response::Ok,
            Err(e) => Response::Error(e),
        },

        Command::ListPresets { dir } => Response::PresetList(list_presets(dir.as_deref())),

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

        Command::MapOutputFor { node_name, profile } => {
            if config::load_profile(&profile).is_err() {
                return Response::Error(format!("profile '{profile}' not found"));
            }
            let mut maps = Mappings::load();
            maps.set(node_name.clone(), profile.clone());
            match maps.save() {
                Ok(()) => {
                    // If this is the device we're feeding right now, apply it.
                    let is_active = state.0.lock().unwrap().active_output.as_deref()
                        == Some(node_name.as_str());
                    if is_active && load_profile(&profile, state).is_ok() {
                        state.0.lock().unwrap().mapped_profile = Some(profile);
                    }
                    Response::Ok
                }
                Err(e) => Response::Error(e),
            }
        }

        Command::UnmapOutputFor { node_name } => {
            let mut maps = Mappings::load();
            maps.remove(&node_name);
            match maps.save() {
                Ok(()) => {
                    let mut inner = state.0.lock().unwrap();
                    if inner.active_output.as_deref() == Some(node_name.as_str()) {
                        inner.mapped_profile = None;
                    }
                    Response::Ok
                }
                Err(e) => Response::Error(e),
            }
        }

        Command::ForgetSink { node_name } => {
            // Drop both the remembered description and any mapping. PipeWire
            // re-adds it (sinks task → KnownSinks::remember) when it next appears.
            let mut known = KnownSinks::load();
            known.devices.remove(&node_name);
            let _ = known.save();
            let mut maps = Mappings::load();
            maps.remove(&node_name);
            let _ = maps.save();
            let mut inner = state.0.lock().unwrap();
            let present: std::collections::HashSet<String> =
                inner.available_sinks.iter().cloned().collect();
            // Drop the description only if the device isn't currently present.
            inner
                .sink_descriptions
                .retain(|(n, _)| n != &node_name || present.contains(n));
            if inner.active_output.as_deref() == Some(node_name.as_str()) {
                inner.mapped_profile = None;
            }
            Response::Ok
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

        Command::ApplyState {
            preamp_db,
            enabled,
            bands,
            effects,
        } => {
            let (sr, channels) = {
                let inner = state.0.lock().unwrap();
                (inner.chain.sample_rate, inner.chain.channels)
            };
            let profile = Profile {
                preamp_db,
                enabled,
                effects,
                bands,
            };
            let chain_rt = profile.clone().into_chain(channels, sr);
            let chain_shadow = profile.into_chain(channels, sr);
            state.replace_chain(chain_rt, chain_shadow);
            Response::Ok
        }

        Command::Reset => {
            let (sr, channels) = {
                let inner = state.0.lock().unwrap();
                (inner.chain.sample_rate, inner.chain.channels)
            };
            state.replace_chain(flat_chain(channels, sr), flat_chain(channels, sr));
            state.0.lock().unwrap().current_preset = None;
            Response::Ok
        }

        Command::ExportApo { path } => {
            let snap = state.snapshot();
            let text = export_apo_text(&snap);
            match std::fs::write(&path, text) {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error(format!("write '{path}': {e}")),
            }
        }

        Command::ExportProfile { path } => {
            let profile = Profile::from_state(&state.snapshot());
            match config::export_profile_file(&path, &profile) {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error(e),
            }
        }

        Command::StoreSlot { slot } => {
            let profile = Profile::from_state(&state.snapshot());
            state.0.lock().unwrap().ab_slots[slot_index(slot)] = Some(profile);
            Response::Ok
        }

        Command::RecallSlot { slot } => {
            let stored = state.0.lock().unwrap().ab_slots[slot_index(slot)].clone();
            match stored {
                Some(profile) => {
                    let (sr, channels) = {
                        let inner = state.0.lock().unwrap();
                        (inner.chain.sample_rate, inner.chain.channels)
                    };
                    let chain_rt = profile.clone().into_chain(channels, sr);
                    let chain_shadow = profile.into_chain(channels, sr);
                    state.replace_chain(chain_rt, chain_shadow);
                    Response::Ok
                }
                None => Response::Error("slot is empty — store it first".to_string()),
            }
        }

        Command::Shutdown => {
            info!("shutdown requested");
            crate::shutdown::cleanup();
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

/// A default chain with no EQ bands, 0 dB preamp, and every effect disabled —
/// the `Reset` target ("flat EQ, all effects off").
fn flat_chain(channels: usize, sample_rate: f64) -> ProcessorChain {
    use resonance_dsp::chain::FxEffect;
    let mut chain = ProcessorChain::builder()
        .channels(channels)
        .sample_rate(sample_rate)
        .build();
    for fx in [
        FxEffect::Fidelity,
        FxEffect::Ambience,
        FxEffect::Surround,
        FxEffect::DynamicBoost,
        FxEffect::Bass,
    ] {
        chain.set_effect_intensity(fx, 0.0);
        chain.set_effect_enabled(fx, false);
    }
    chain
}

fn slot_index(slot: AbSlot) -> usize {
    match slot {
        AbSlot::A => 0,
        AbSlot::B => 1,
    }
}

/// Render the daemon's current preamp + bands as EqualizerAPO `.txt` text.
fn export_apo_text(snap: &resonance_ipc::DaemonState) -> String {
    let bands: Vec<EqBand> = snap
        .bands
        .iter()
        .map(|b| EqBand {
            filter_type: band_type_to_apo(b.band_type),
            freq: b.freq,
            gain_db: b.gain_db,
            q: b.q,
            enabled: b.enabled,
        })
        .collect();
    write_apo(snap.preamp_db, &bands)
}

fn band_type_to_apo(t: BandType) -> ApoFilterType {
    match t {
        BandType::Peaking => ApoFilterType::Peaking,
        BandType::LowShelf => ApoFilterType::LowShelf,
        BandType::HighShelf => ApoFilterType::HighShelf,
        BandType::LowPass => ApoFilterType::LowPassQ,
        BandType::HighPass => ApoFilterType::HighPassQ,
        BandType::BandPass => ApoFilterType::BandPass,
        BandType::Notch => ApoFilterType::Notch,
        BandType::AllPass => ApoFilterType::AllPass,
    }
}

/// Read + parse a preset file, dispatching on extension (`.fac` vs APO `.txt`).
fn parse_preset_file(path: &str) -> Result<resonance_preset::model::Preset, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    if path.ends_with(".fac") {
        parse_fac(&content).map_err(|e| e.to_string())
    } else {
        parse_apo(&content).map_err(|e| e.to_string())
    }
}

/// Default profile name for an imported file: its stem (e.g. `Rock.fac` → `Rock`).
fn file_stem_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "imported".to_string())
}

/// Import a preset file into the managed profile store. Sync + CPU-heavy
/// (GraphicEQ files curve-fit) — call from a blocking context, never directly on
/// the async runtime.
fn import_preset(path: String, name: Option<String>) -> Response {
    // Our own `.toml` exports load directly as a Profile; `.fac` / APO `.txt`
    // go through the preset parser first.
    let profile = if path.ends_with(".toml") {
        config::load_profile_file(&path)
    } else {
        parse_preset_file(&path).map(|p| Profile::from_preset(&p))
    };
    match profile {
        Ok(profile) => {
            let raw = name.unwrap_or_else(|| file_stem_name(&path));
            let profile_name = config::sanitize_name(&raw);
            if profile_name.is_empty() {
                return Response::Error("profile name is empty".to_string());
            }
            match config::save_profile(&profile_name, &profile) {
                Ok(()) => Response::Imported(profile_name),
                Err(e) => Response::Error(e),
            }
        }
        Err(e) => Response::Error(e),
    }
}

/// Build the DSP chain from an already-parsed preset and swap it in.
fn apply_preset(preset: resonance_preset::model::Preset, state: &SharedState) {
    let (sr, channels) = {
        let inner = state.0.lock().unwrap();
        (inner.chain.sample_rate, inner.chain.channels)
    };
    // Build chain twice: one for RT thread, one to update the shadow (GetState reads shadow)
    let chain_rt = preset.clone().into_chain(channels, sr);
    let chain_shadow = preset.into_chain(channels, sr);
    state.replace_chain(chain_rt, chain_shadow);
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

/// List preset files. `Some(dir)` scans that directory; `None` scans the XDG
/// preset library + system dirs, with user entries shadowing system ones.
fn list_presets(dir: Option<&str>) -> Vec<String> {
    match dir {
        Some(d) => list_dir_presets(std::path::Path::new(d)),
        None => {
            let _ = std::fs::create_dir_all(resonance_ipc::paths::user_preset_dir());
            // filename → full path; first writer wins (search dirs are user-first).
            let mut by_name = std::collections::BTreeMap::new();
            for d in resonance_ipc::paths::preset_search_dirs() {
                for path in list_dir_presets(&d) {
                    if let Some(fname) = std::path::Path::new(&path)
                        .file_name()
                        .and_then(|f| f.to_str())
                    {
                        by_name.entry(fname.to_string()).or_insert(path);
                    }
                }
            }
            by_name.into_values().collect()
        }
    }
}

fn list_dir_presets(dir: &std::path::Path) -> Vec<String> {
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
