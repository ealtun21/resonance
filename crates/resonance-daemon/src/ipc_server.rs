use crate::config::{self, ConvolutionProfile, KnownSinks, Mappings, Profile};
use crate::state::{AppControl, AudioCommand, SharedState, SinkCtl};
use anyhow::Result;
use resonance_dsp::chain::ProcessorChain;
use resonance_dsp::channel::{ChannelMask as DspMask, ChannelMatrix};
use resonance_dsp::convolution::{ConvolutionEngine, IrData, MAX_IR_SECONDS};
use resonance_dsp::filter::{ApoFilter, FilterError, FilterType};
use resonance_ipc::{
    AbSlot, BandScope, BandState, BandType, ChannelMask, Command, EffectsState, FxEffectId,
    Response, RoutingMatrix,
};
use resonance_preset::model::{ApoFilterType, EqBand};
use resonance_preset::{
    apo::{parse_apo, write_apo},
    fac::parse_fac,
};
use tracing::{error, info, warn};

#[cfg(unix)]
pub async fn run(state: SharedState) -> Result<()> {
    use tokio::net::UnixListener;
    let sock_path = crate::shutdown::socket_path();
    let _ = tokio::fs::remove_file(&sock_path).await;

    let listener = UnixListener::bind(&sock_path)?;
    info!("IPC listening on {}", sock_path.display());

    loop {
        match listener.accept().await {
            Ok((stream, _)) => spawn_client(stream, state.clone()),
            Err(e) => error!("accept error: {e}"),
        }
    }
}

#[cfg(windows)]
pub async fn run(state: SharedState) -> Result<()> {
    use tokio::net::TcpListener;
    // Windows has no usable AF_UNIX in tokio; bind a loopback TCP socket on an
    // ephemeral port and advertise it via the port file so clients can dial it.
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let port_file = resonance_ipc::paths::port_file_path();
    if let Some(dir) = port_file.parent() {
        let _ = tokio::fs::create_dir_all(dir).await;
    }
    tokio::fs::write(&port_file, port.to_string()).await?;
    info!(
        "IPC listening on 127.0.0.1:{port} (port file {})",
        port_file.display()
    );

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let _ = stream.set_nodelay(true);
                spawn_client(stream, state.clone());
            }
            Err(e) => error!("accept error: {e}"),
        }
    }
}

/// Spawn a task to service one client connection over any async stream.
fn spawn_client<S>(stream: S, state: SharedState)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        if let Err(e) = handle_client(stream, state).await {
            warn!("client error: {e}");
        }
    });
}

async fn handle_client<S>(stream: S, state: SharedState) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (reader, writer) = tokio::io::split(stream);
    let mut reader = tokio::io::BufReader::new(reader);
    let mut writer = tokio::io::BufWriter::new(writer);

    loop {
        let Ok(cmd) = read_command_async(&mut reader).await else {
            break;
        };

        // Shutdown must reply *before* the process exits, or the client always
        // sees a torn connection and reports failure for a successful shutdown.
        let is_shutdown = matches!(cmd, Command::Shutdown);
        let response = dispatch(cmd, &state).await;
        if let Err(e) = write_response_async(&mut writer, &response).await {
            warn!("write error: {e}");
            break;
        }
        if is_shutdown {
            use tokio::io::AsyncWriteExt;
            let _ = writer.flush().await;
            info!("shutdown requested");
            crate::shutdown::cleanup();
            std::process::exit(0);
        }
    }
    Ok(())
}

/// Route one client command to its handler and produce the reply.
///
/// Each arm is a thin delegation to a focused handler so the wiring of command
/// variants to behaviour stays scannable; the handlers own the side effects and
/// reply construction. Only the two parse-heavy commands run async (they offload
/// to a blocking thread); everything else is synchronous.
async fn dispatch(cmd: Command, state: &SharedState) -> Response {
    match cmd {
        Command::GetState => {
            state.mark_polled();
            Response::State(state.snapshot())
        }
        Command::SetPower { enabled } => handle_set_power(state, enabled),
        Command::SetPreamp { db } => handle_set_preamp(state, db),
        Command::SetEffectIntensity { effect, value } => {
            handle_set_effect_intensity(state, effect, value)
        }
        Command::SetEffectEnabled { effect, enabled } => {
            handle_set_effect_enabled(state, effect, enabled)
        }
        Command::LoadPreset { path } => handle_load_preset(state, path).await,
        Command::ImportPreset { path, name } => handle_import_preset(path, name).await,
        Command::RenameProfile { from, to } => {
            result_to_response(config::rename_profile(&from, &to))
        }
        Command::ListPresets { dir } => Response::PresetList(list_presets(dir.as_deref())),
        Command::SaveProfile { name } => handle_save_profile(state, &name),
        Command::LoadProfile { name } => handle_load_profile(state, name),
        Command::DeleteProfile { name } => result_to_response(config::delete_profile(&name)),
        Command::DuplicateProfile { from, to } => handle_duplicate_profile(&from, &to),
        Command::ExportProfileNamed { name, path } => handle_export_profile_named(&name, &path),
        Command::ListProfiles => Response::PresetList(config::list_profiles()),
        Command::MapOutput { profile } => handle_map_output(state, profile),
        Command::UnmapOutput => handle_unmap_output(state),
        Command::MapOutputFor { node_name, profile } => {
            handle_map_output_for(state, &node_name, &profile)
        }
        Command::UnmapOutputFor { node_name } => handle_unmap_output_for(state, &node_name),
        Command::ForgetSink { node_name } => handle_forget_sink(state, &node_name),
        Command::ListMappings => Response::Mappings(Mappings::load().list()),
        Command::SetBand {
            index,
            freq,
            gain_db,
            q,
        } => handle_set_band(state, index, freq, gain_db, q),
        Command::SetBandEnabled { index, enabled } => {
            handle_set_band_enabled(state, index, enabled)
        }
        Command::AddBand {
            band_type,
            freq,
            gain_db,
            q,
        } => handle_add_band(state, band_type, freq, gain_db, q),
        Command::RemoveBand { index } => handle_remove_band(state, index),
        Command::SetBandType { index, band_type } => handle_set_band_type(state, index, band_type),
        Command::SetBandSlope {
            index,
            slope_db_oct,
        } => handle_set_band_slope(state, index, slope_db_oct),
        Command::SetBandScope { index, scope } => handle_set_band_scope(state, index, scope),
        Command::SetBandChannels { index, channels } => {
            handle_set_band_channels(state, index, channels)
        }
        Command::SetChannelRouting { matrix } => handle_set_channel_routing(state, &matrix),
        Command::SwapChannels { a, b } => handle_swap_channels(state, a, b),
        Command::ClearRouting => handle_clear_routing(state),
        Command::SetOutputTarget { node_name } => handle_set_output_target(state, node_name),
        Command::FollowSystemOutput => handle_follow_system_output(state),
        Command::ApplyState {
            preamp_db,
            enabled,
            bands,
            effects,
        } => handle_apply_state(state, preamp_db, enabled, bands, effects),
        Command::Reset => handle_reset(state),
        Command::ExportApo { path } => handle_export_apo(state, &path),
        Command::ExportProfile { path } => handle_export_profile(state, &path),
        Command::StoreSlot { slot } => handle_store_slot(state, slot),
        Command::RecallSlot { slot } => handle_recall_slot(state, slot),
        Command::SetAppVolume { key, volume } => handle_set_app_volume(state, key, volume),
        Command::SetAppMute { key, muted } => handle_set_app_mute(state, key, muted),
        Command::SetSinkVolume { name, volume } => handle_set_sink_volume(state, name, volume),
        Command::SetSinkMute { name, muted } => handle_set_sink_mute(state, name, muted),
        Command::SetDither { bits } => handle_set_dither(state, bits),
        Command::SetConvolutionIr { path } => handle_set_convolution_ir(state, path).await,
        Command::ClearConvolutionIr => handle_clear_convolution(state),
        Command::SetConvolutionEnabled { enabled } => {
            handle_set_convolution_enabled(state, enabled)
        }
        Command::CaptureOutput { frames } => handle_capture_output(state, frames),
        // The actual cleanup + exit happens in `handle_client` after this Ok is
        // flushed to the client (see the `is_shutdown` branch there).
        Command::Shutdown => Response::Ok,
    }
}

/// Map a `Result<(), String>` config/IO outcome onto the IPC reply: `Ok` on
/// success, the error string passed straight back to the client otherwise. Used
/// for the many commands whose only job is to call a `config::*` operation.
fn result_to_response(r: Result<(), String>) -> Response {
    match r {
        Ok(()) => Response::Ok,
        Err(e) => Response::Error(e),
    }
}

/// Forward a per-app volume request to whichever component owns app control
/// (backend main-loop thread, or the Windows WASAPI control task). The volume
/// is clamped to the model's `0.0..=4.0`; backends further clamp to what they
/// support. Control-plane only — never touches the RT DSP chain.
fn handle_set_app_volume(state: &SharedState, key: String, volume: f64) -> Response {
    state.forward_app_ctl(AppControl::SetVolume {
        key,
        volume: volume.clamp(0.0, 4.0),
    });
    Response::Ok
}

/// Forward a per-app mute request (see [`handle_set_app_volume`]).
fn handle_set_app_mute(state: &SharedState, key: String, muted: bool) -> Response {
    state.forward_app_ctl(AppControl::SetMute { key, muted });
    Response::Ok
}

/// Forward a per-output-sink volume request to the backend (`PipeWire` main-loop
/// thread). The volume is clamped to `0.0..=4.0`; the backend further clamps to
/// what the device supports. Control-plane only — never touches the RT chain.
fn handle_set_sink_volume(state: &SharedState, name: String, volume: f64) -> Response {
    state.forward_sink_ctl(SinkCtl::SetVolume {
        name,
        volume: volume.clamp(0.0, 4.0),
    });
    Response::Ok
}

/// Forward a per-output-sink mute request (see [`handle_set_sink_volume`]).
fn handle_set_sink_mute(state: &SharedState, name: String, muted: bool) -> Response {
    state.forward_sink_ctl(SinkCtl::SetMute { name, muted });
    Response::Ok
}

/// Toggle the master power (bypass) flag.
fn handle_set_power(state: &SharedState, enabled: bool) -> Response {
    state.send(AudioCommand::SetPower(enabled), |chain| {
        chain.enabled = enabled;
    });
    Response::Ok
}

/// Set the pre-EQ makeup gain, rejecting hostile non-finite input.
///
/// A non-finite value reaches here from e.g. `Preamp: nan` in an APO `.txt`;
/// `db_to_linear(NaN) = NaN` would silence/poison the whole output, so reject it
/// and clamp the rest to a sane dB range.
fn handle_set_preamp(state: &SharedState, db: f64) -> Response {
    if !db.is_finite() {
        return Response::Error("preamp must be a finite number".to_string());
    }
    let db = db.clamp(-60.0, 24.0);
    state.send(AudioCommand::SetPreamp(db), move |chain| {
        chain.preamp_db = db;
    });
    Response::Ok
}

/// Set one effect's intensity (0..=1 mapped per-effect inside the chain).
fn handle_set_effect_intensity(state: &SharedState, effect: FxEffectId, value: f64) -> Response {
    let fx: resonance_dsp::chain::FxEffect = effect.into();
    state.send(
        AudioCommand::SetEffectIntensity { effect: fx, value },
        |chain| {
            chain.set_effect_intensity(fx, value);
        },
    );
    Response::Ok
}

/// Set (or clear) the final-stage output dither target bit depth.
fn handle_set_dither(state: &SharedState, bits: Option<u32>) -> Response {
    state.send(AudioCommand::SetDither { bits }, move |chain| {
        chain.set_dither(bits);
    });
    Response::Ok
}

/// Decode, resample and FFT-prepare a WAV impulse response, then swap the
/// prepared engine onto the chain. All the heavy lifting (file IO, sinc
/// resampling, partition FFTs — real work for a 2 s IR) runs on a blocking
/// thread; the RT thread only installs the finished kernel.
async fn handle_set_convolution_ir(state: &SharedState, path: String) -> Response {
    let (channels, sample_rate) = {
        let inner = state.0.lock().unwrap();
        // Live format, not the frozen shadow (see SharedState::rebuild_chain).
        let sr = inner
            .meters
            .sample_rate()
            .unwrap_or(inner.chain.sample_rate);
        let ch = inner.meters.channels().unwrap_or(inner.chain.channels);
        (ch, sr)
    };
    let prepared = tokio::task::spawn_blocking(move || -> Result<ConvolutionEngine, String> {
        let ir = crate::ir::load_wav_ir(&path)?;
        let seconds = ir.frames() as f64 / ir.sample_rate;
        if seconds > MAX_IR_SECONDS {
            warn!(
                "impulse response '{}' is {seconds:.2}s — truncated to {MAX_IR_SECONDS}s",
                ir.name
            );
        }
        let mut engine = ConvolutionEngine::new(channels, sample_rate);
        engine.load_ir(std::sync::Arc::new(ir))?;
        Ok(engine)
    })
    .await;
    match prepared {
        Ok(Ok(engine)) => {
            info!(
                "impulse response loaded: {}",
                engine.info().map_or_else(String::new, |i| i.path)
            );
            state.send(
                AudioCommand::SetConvolution(Box::new(engine.clone())),
                move |chain| {
                    chain.convolution = engine;
                    // The shadow chain's format can differ from the live one the
                    // engine was prepared at; force-match (no-op when equal).
                    chain.convolution.rebind_sample_rate(chain.sample_rate);
                    chain.convolution.set_channels(chain.channels);
                },
            );
            Response::Ok
        }
        Ok(Err(e)) => Response::Error(e),
        Err(e) => Response::Error(format!("impulse-response load task failed: {e}")),
    }
}

/// Drop the convolution IR entirely (passthrough, zero added latency).
fn handle_clear_convolution(state: &SharedState) -> Response {
    state.send(AudioCommand::ClearConvolution, |chain| {
        chain.convolution.clear();
    });
    info!("impulse response cleared");
    Response::Ok
}

/// Bypass or re-arm the convolution stage without dropping the loaded IR.
fn handle_set_convolution_enabled(state: &SharedState, enabled: bool) -> Response {
    let loaded = state.0.lock().unwrap().chain.convolution.source().is_some();
    if !loaded {
        return Response::Error("no impulse response loaded — load a .wav IR first".to_string());
    }
    state.send(AudioCommand::SetConvolutionEnabled(enabled), move |chain| {
        chain.convolution.set_enabled(enabled);
    });
    Response::Ok
}

/// Return the freshest `frames` post-DSP mono samples (the spectrum feed) with
/// the live DSP rate, for the `resonance verify` harness. Shorter than asked
/// while the rolling buffer is still filling; empty where the daemon owns no
/// audio path (Windows — the APO does the DSP in audiodg).
fn handle_capture_output(state: &SharedState, frames: u32) -> Response {
    let inner = state.0.lock().unwrap();
    let want = (frames as usize).min(crate::state::CAPTURE_BUF);
    let have = inner.capture.len();
    let take = have.min(want);
    let samples: Vec<f32> = inner.capture.iter().skip(have - take).copied().collect();
    let rate = inner
        .meters
        .sample_rate()
        .unwrap_or(inner.chain.sample_rate);
    Response::Capture { rate, samples }
}

/// Enable or bypass a single effect.
fn handle_set_effect_enabled(state: &SharedState, effect: FxEffectId, enabled: bool) -> Response {
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

/// Parse a preset file off the runtime, then swap the built chain in.
///
/// Parsing runs on a blocking thread because `GraphicEQ` files curve-fit; the
/// async IPC loop keeps serving other clients meanwhile.
async fn handle_load_preset(state: &SharedState, path: String) -> Response {
    let p = path.clone();
    match tokio::task::spawn_blocking(move || parse_preset_file(&p)).await {
        Ok(Ok(preset)) => {
            apply_preset(&preset, state);
            // Chain-replacing commands are logged (issue #43): a surprising
            // chain state must be explainable from the daemon log alone.
            info!("preset loaded: {path}");
            state.0.lock().unwrap().current_preset = Some(path);
            Response::Ok
        }
        Ok(Err(e)) => Response::Error(e),
        Err(e) => Response::Error(format!("load task failed: {e}")),
    }
}

/// Import a preset file into the managed profile store on a blocking thread.
///
/// Parsing can be expensive (a `GraphicEQ` `.txt` runs a curve-fit optimisation),
/// so offload the parse + file IO to keep the async IPC loop responsive.
async fn handle_import_preset(path: String, name: Option<String>) -> Response {
    tokio::task::spawn_blocking(move || import_preset(&path, name))
        .await
        .unwrap_or_else(|e| Response::Error(format!("import task failed: {e}")))
}

/// Persist the current live chain state as a named profile.
fn handle_save_profile(state: &SharedState, name: &str) -> Response {
    let profile = Profile::from_state(&state.snapshot());
    result_to_response(config::save_profile(name, &profile))
}

/// Load a named profile, apply it, and record it as the current preset.
fn handle_load_profile(state: &SharedState, name: String) -> Response {
    match load_profile(&name, state) {
        Ok(()) => {
            info!("profile loaded: {name}");
            state.0.lock().unwrap().current_preset = Some(name);
            Response::Ok
        }
        Err(e) => Response::Error(e),
    }
}

/// Copy a stored profile under a new name without touching the live chain.
fn handle_duplicate_profile(from: &str, to: &str) -> Response {
    match config::load_profile(from) {
        Ok(profile) => result_to_response(config::save_profile(to, &profile)),
        Err(e) => Response::Error(e),
    }
}

/// Export a *named* stored profile to a file (not the current chain state).
fn handle_export_profile_named(name: &str, path: &str) -> Response {
    match config::load_profile(name) {
        Ok(profile) => result_to_response(config::export_profile_file(path, &profile)),
        Err(e) => Response::Error(e),
    }
}

/// Map a profile to the active output device and apply it immediately.
fn handle_map_output(state: &SharedState, profile: String) -> Response {
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

/// Drop the mapping for the active output device.
fn handle_unmap_output(state: &SharedState) -> Response {
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

/// Map a profile to a named device; apply now only if that device is active.
fn handle_map_output_for(state: &SharedState, node_name: &str, profile: &str) -> Response {
    if config::load_profile(profile).is_err() {
        return Response::Error(format!("profile '{profile}' not found"));
    }
    let mut maps = Mappings::load();
    maps.set(node_name.to_owned(), profile.to_owned());
    match maps.save() {
        Ok(()) => {
            // If this is the device we're feeding right now, apply it.
            let is_active = state.0.lock().unwrap().active_output.as_deref() == Some(node_name);
            if is_active && load_profile(profile, state).is_ok() {
                state.0.lock().unwrap().mapped_profile = Some(profile.to_owned());
            }
            Response::Ok
        }
        Err(e) => Response::Error(e),
    }
}

/// Drop the mapping for a named device, clearing the live mapped-profile marker
/// only if that device is the one currently active.
fn handle_unmap_output_for(state: &SharedState, node_name: &str) -> Response {
    let mut maps = Mappings::load();
    maps.remove(node_name);
    match maps.save() {
        Ok(()) => {
            let mut inner = state.0.lock().unwrap();
            if inner.active_output.as_deref() == Some(node_name) {
                inner.mapped_profile = None;
            }
            Response::Ok
        }
        Err(e) => Response::Error(e),
    }
}

/// Forget a sink: drop its remembered description and any mapping.
///
/// `PipeWire` re-adds the sink (sinks task → `KnownSinks::remember`) when it next
/// appears, so this is safe to call on a present device.
fn handle_forget_sink(state: &SharedState, node_name: &str) -> Response {
    let mut known = KnownSinks::load();
    known.devices.remove(node_name);
    if let Err(e) = known.save() {
        return Response::Error(e);
    }
    let mut maps = Mappings::load();
    maps.remove(node_name);
    if let Err(e) = maps.save() {
        return Response::Error(e);
    }
    let mut inner = state.0.lock().unwrap();
    let present: std::collections::HashSet<String> =
        inner.available_sinks.iter().cloned().collect();
    // Drop the description only if the device isn't currently present.
    inner
        .sink_descriptions
        .retain(|(n, _)| n != node_name || present.contains(n));
    if inner.active_output.as_deref() == Some(node_name) {
        inner.mapped_profile = None;
    }
    Response::Ok
}

/// Edit one band's parameters in place, preserving its type, enabled flag and
/// channel mask (rebuilding the biquad from a fresh filter would otherwise reset
/// the mask to global).
fn handle_set_band(state: &SharedState, index: usize, freq: f64, gain_db: f64, q: f64) -> Response {
    state.send(
        AudioCommand::SetBand {
            index,
            freq,
            gain_db,
            q,
        },
        move |chain| {
            if let Some(f) = chain.filters.get(index) {
                let (ft, en, mask) = (f.filter_type, f.enabled, f.mask);
                if let Ok(new_f) = build_band(chain, ft, freq, gain_db, q, en, mask) {
                    chain.filters[index] = new_f;
                }
            }
        },
    );
    Response::Ok
}

/// Enable or bypass a single band.
fn handle_set_band_enabled(state: &SharedState, index: usize, enabled: bool) -> Response {
    state.send(
        AudioCommand::SetBandEnabled { index, enabled },
        move |chain| {
            if let Some(f) = chain.filters.get_mut(index) {
                f.enabled = enabled;
            }
        },
    );
    Response::Ok
}

/// Append a new band. New bands are global by default; retarget with
/// `SetBandChannels`.
fn handle_add_band(
    state: &SharedState,
    band_type: BandType,
    freq: f64,
    gain_db: f64,
    q: f64,
) -> Response {
    state.send(
        AudioCommand::AddBand {
            band_type,
            freq,
            gain_db,
            q,
        },
        move |chain| {
            if let Ok(nf) = build_band(
                chain,
                band_type.into(),
                freq,
                gain_db,
                q,
                true,
                DspMask::ALL,
            ) {
                chain.filters.push(nf);
            }
        },
    );
    Response::Ok
}

/// Remove the band at `index` (no-op if out of range).
fn handle_remove_band(state: &SharedState, index: usize) -> Response {
    state.send(AudioCommand::RemoveBand { index }, move |chain| {
        if index < chain.filters.len() {
            chain.filters.remove(index);
        }
    });
    Response::Ok
}

/// Change a band's filter type, preserving its other parameters and mask.
fn handle_set_band_type(state: &SharedState, index: usize, band_type: BandType) -> Response {
    state.send(
        AudioCommand::SetBandType { index, band_type },
        move |chain| {
            if let Some(f) = chain.filters.get(index) {
                let (freq, gain_db, q, en, mask) = (f.freq, f.gain_db, f.q, f.enabled, f.mask);
                if let Ok(nf) = build_band(chain, band_type.into(), freq, gain_db, q, en, mask) {
                    chain.filters[index] = nf;
                }
            }
        },
    );
    Response::Ok
}

/// Change an EQ band's filter slope (12/24/48 dB/oct).
fn handle_set_band_slope(state: &SharedState, index: usize, slope_db_oct: u8) -> Response {
    state.send(
        AudioCommand::SetBandSlope {
            index,
            slope_db_oct,
        },
        move |chain| {
            let sr = chain.sample_rate;
            if let Some(f) = chain.filters.get_mut(index) {
                let _ = f.set_slope(slope_db_oct, sr);
            }
        },
    );
    Response::Ok
}

/// Change an EQ band's stereo scope (Stereo/Mid/Side).
fn handle_set_band_scope(state: &SharedState, index: usize, scope: BandScope) -> Response {
    state.send(AudioCommand::SetBandScope { index, scope }, move |chain| {
        if let Some(f) = chain.filters.get_mut(index) {
            f.scope = scope.into();
        }
    });
    Response::Ok
}

/// Retarget an existing band to a channel subset (per-channel EQ).
///
/// Rejects an out-of-range band index rather than silently no-op'ing (mirrors
/// `SwapChannels`' range check).
fn handle_set_band_channels(state: &SharedState, index: usize, channels: ChannelMask) -> Response {
    let nbands = state.0.lock().unwrap().chain.filters.len();
    if index >= nbands {
        return Response::Error(format!("no band at index {index} (have {nbands})"));
    }
    let mask = channels.to_dsp();
    state.send(
        AudioCommand::SetBandChannels { index, mask },
        move |chain| {
            if let Some(f) = chain.filters.get_mut(index) {
                f.mask = mask;
            }
        },
    );
    Response::Ok
}

/// Install a client-supplied routing matrix.
///
/// The in-graph filter has a fixed `channels` ports in and out, so only a square
/// remap at the live channel count can be applied. Validate before reaching the
/// RT thread so a mismatched matrix never misframes a buffer (up/downmix to a
/// different width is a daemon-path feature).
fn handle_set_channel_routing(state: &SharedState, matrix: &RoutingMatrix) -> Response {
    let channels = state.0.lock().unwrap().chain.channels;
    match matrix.to_dsp() {
        Some(m) if m.in_ch() == channels && m.out_ch() == channels => {
            state.send(
                AudioCommand::SetRouting {
                    matrix: Some(m.clone()),
                },
                move |chain| chain.routing = Some(m),
            );
            Response::Ok
        }
        Some(_) => Response::Error(format!(
            "routing matrix must be square at the current channel count ({channels}×{channels})"
        )),
        None => Response::Error("invalid routing matrix dimensions".to_string()),
    }
}

/// Swap two processing channels (square remap at the current channel count).
/// Replaces any existing routing.
fn handle_swap_channels(state: &SharedState, a: usize, b: usize) -> Response {
    let channels = state.0.lock().unwrap().chain.channels;
    if !channel_indices_in_range(channels, a, b) {
        return Response::Error(format!(
            "channel index out of range (chain has {channels} channels)"
        ));
    }
    let m = ChannelMatrix::swap(channels, a, b);
    state.send(
        AudioCommand::SetRouting {
            matrix: Some(m.clone()),
        },
        move |chain| chain.routing = Some(m),
    );
    Response::Ok
}

/// Clear any installed routing matrix (identity passthrough).
fn handle_clear_routing(state: &SharedState) -> Response {
    state.send(AudioCommand::SetRouting { matrix: None }, |chain| {
        chain.routing = None;
    });
    Response::Ok
}

/// Pin the output to a specific device.
///
/// On Windows this switches the system default playback device (the APO follows
/// the engine's endpoint); elsewhere it routes via the `PipeWire` node.
fn handle_set_output_target(state: &SharedState, node_name: String) -> Response {
    #[cfg(target_os = "windows")]
    {
        crate::audio::win_devices::set_default_render_endpoint(&node_name);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let route_tx = state.0.lock().unwrap().route_tx.clone();
        if route_tx.send(node_name.clone()).is_err() {
            warn!("audio thread unavailable; output route '{node_name}' not applied live");
        }
    }
    state.0.lock().unwrap().preferred_output = Some(node_name);
    Response::Ok
}

/// Clear the output pin and follow the OS default device.
///
/// On non-Windows an empty route name signals the backend to track the default
/// device; on Windows the APO already sits on whatever endpoint is in use.
fn handle_follow_system_output(state: &SharedState) -> Response {
    #[cfg(not(target_os = "windows"))]
    {
        let route_tx = state.0.lock().unwrap().route_tx.clone();
        if route_tx.send(String::new()).is_err() {
            warn!("audio thread unavailable; system-default follow not applied live");
        }
    }
    state.0.lock().unwrap().preferred_output = None;
    Response::Ok
}

/// Replace the whole chain from an explicit (preamp, bands, effects) state.
fn handle_apply_state(
    state: &SharedState,
    preamp_db: f64,
    enabled: bool,
    bands: Vec<BandState>,
    effects: EffectsState,
) -> Response {
    let snap = state.snapshot();
    let profile = Profile {
        preamp_db,
        enabled,
        effects,
        bands,
        // ApplyState carries EQ + effects only (undo/redo, bulk edits); dither
        // and convolution are owned by their own commands, so preserve the live
        // settings across the rebuild rather than clobbering them to off.
        dither_bits: snap.dither_bits,
        convolution: snap.convolution.map(|c| ConvolutionProfile {
            path: c.path,
            enabled: c.enabled,
        }),
    };
    apply_profile_chain(&profile, state);
    info!("bulk state applied (ApplyState: undo/redo or bulk edit)");
    Response::Ok
}

/// Reset to a flat chain (no EQ, 0 dB preamp, all effects off) and clear the
/// current-preset marker.
fn handle_reset(state: &SharedState) -> Response {
    state.rebuild_chain(flat_chain);
    info!("chain reset to defaults");
    state.0.lock().unwrap().current_preset = None;
    Response::Ok
}

/// Write the current preamp + bands to an `EqualizerAPO` `.txt` file.
fn handle_export_apo(state: &SharedState, path: &str) -> Response {
    let snap = state.snapshot();
    let text = export_apo_text(&snap);
    match std::fs::write(path, text) {
        Ok(()) => Response::Ok,
        Err(e) => Response::Error(format!("write '{path}': {e}")),
    }
}

/// Export the current live chain state as a profile file.
fn handle_export_profile(state: &SharedState, path: &str) -> Response {
    let profile = Profile::from_state(&state.snapshot());
    result_to_response(config::export_profile_file(path, &profile))
}

/// Capture the current state into an A/B comparison slot.
fn handle_store_slot(state: &SharedState, slot: AbSlot) -> Response {
    let profile = Profile::from_state(&state.snapshot());
    state.0.lock().unwrap().ab_slots[slot_index(slot)] = Some(profile);
    Response::Ok
}

/// Recall a stored A/B slot, rebuilding the chain from it.
fn handle_recall_slot(state: &SharedState, slot: AbSlot) -> Response {
    let stored = state.0.lock().unwrap().ab_slots[slot_index(slot)].clone();
    match stored {
        Some(profile) => {
            apply_profile_chain(&profile, state);
            info!("A/B slot recalled");
            Response::Ok
        }
        None => Response::Error("slot is empty — store it first".to_string()),
    }
}

/// Whether both channel indices are valid for a chain of `channels` channels.
/// Pure bounds check extracted so the swap range rule is unit-testable.
fn channel_indices_in_range(channels: usize, a: usize, b: usize) -> bool {
    a < channels && b < channels
}

/// Build an `ApoFilter` matching the chain's sample rate / channel count, with an
/// explicit channel target (`DspMask::ALL` for a global band).
fn build_band(
    chain: &ProcessorChain,
    filter_type: FilterType,
    freq: f64,
    gain_db: f64,
    q: f64,
    enabled: bool,
    mask: DspMask,
) -> Result<ApoFilter, FilterError> {
    ApoFilter::builder()
        .filter_type(filter_type)
        .freq(freq)
        .gain_db(gain_db)
        .q(q)
        .enabled(enabled)
        .channels(chain.channels)
        .sample_rate(chain.sample_rate)
        .channel_mask(mask)
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

/// Render the daemon's current preamp + bands as `EqualizerAPO` `.txt` text,
/// including a `Convolution:` directive when an IR is loaded (issue #40).
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
            channels: b.channels.0,
        })
        .collect();
    let ir = snap.convolution.as_ref().map(|c| c.path.as_str());
    write_apo(snap.preamp_db, &bands, ir)
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
///
/// A relative `Convolution:` impulse-response path is resolved against the
/// preset file's own directory (`EqualizerAPO` semantics), so the stored path is
/// always loadable no matter the daemon's working directory.
fn parse_preset_file(path: &str) -> Result<resonance_preset::model::Preset, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let is_fac = std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("fac"));
    let mut preset = if is_fac {
        parse_fac(&content).map_err(|e| e.to_string())?
    } else {
        parse_apo(&content).map_err(|e| e.to_string())?
    };
    if let Some(ir) = preset.convolution.as_mut() {
        let p = std::path::Path::new(ir.as_str());
        if p.is_relative() {
            if let Some(dir) = std::path::Path::new(path).parent() {
                *ir = dir.join(p).to_string_lossy().into_owned();
            }
        }
    }
    Ok(preset)
}

/// Default profile name for an imported file: its stem (e.g. `Rock.fac` → `Rock`).
fn file_stem_name(path: &str) -> String {
    std::path::Path::new(path).file_stem().map_or_else(
        || "imported".to_string(),
        |s| s.to_string_lossy().into_owned(),
    )
}

/// Import a preset file into the managed profile store. Sync + CPU-heavy
/// (`GraphicEQ` files curve-fit) — call from a blocking context, never directly on
/// the async runtime.
fn import_preset(path: &str, name: Option<String>) -> Response {
    // Our own `.toml` exports load directly as a Profile; `.fac` / APO `.txt`
    // go through the preset parser first.
    let is_toml = std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("toml"));
    let profile = if is_toml {
        config::load_profile_file(path)
    } else {
        parse_preset_file(path).map(|p| Profile::from_preset(&p))
    };
    match profile {
        Ok(profile) => {
            let raw = name.unwrap_or_else(|| file_stem_name(path));
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

/// Build the DSP chain from an already-parsed preset and swap it in, loading
/// the preset's `Convolution:` impulse response when it carries one.
fn apply_preset(preset: &resonance_preset::model::Preset, state: &SharedState) {
    let ir = preset
        .convolution
        .as_deref()
        .and_then(|path| load_ir_reusing_live(state, path));
    state.rebuild_chain(|ch, sr| {
        let mut chain = preset.clone().into_chain(ch, sr);
        if let Some(ir) = ir.clone() {
            if let Err(e) = chain.convolution.load_ir(ir) {
                warn!("preset convolution IR not loaded: {e}");
            }
        }
        chain
    });
}

/// Decode an IR for a chain rebuild, reusing the live chain's already-decoded
/// samples when the path matches (undo/redo and preset re-loads skip the disk).
/// A missing/corrupt file degrades to a warning, never a failed apply.
fn load_ir_reusing_live(state: &SharedState, path: &str) -> Option<std::sync::Arc<IrData>> {
    let live = {
        let inner = state.0.lock().unwrap();
        inner
            .chain
            .convolution
            .source()
            .filter(|s| s.path == path)
            .cloned()
    };
    live.or_else(|| match crate::ir::load_wav_ir(path) {
        Ok(data) => Some(std::sync::Arc::new(data)),
        Err(e) => {
            warn!("convolution IR not loaded: {e}");
            None
        }
    })
}

/// Load a named profile from the config dir and apply it to the chain.
fn load_profile(name: &str, state: &SharedState) -> Result<(), String> {
    let profile = config::load_profile(name)?;
    apply_profile_chain(&profile, state);
    Ok(())
}

/// Rebuild the chain from a profile, restoring its convolution IR (if any).
///
/// The IR is re-read from its source WAV — unless the live chain already holds
/// the same file, in which case the decoded samples are reused (undo/redo and
/// A/B recall never re-hit the disk). A missing or corrupt IR file drops the
/// stage with a warning instead of failing the whole profile apply.
pub(crate) fn apply_profile_chain(profile: &Profile, state: &SharedState) {
    let ir = profile
        .convolution
        .as_ref()
        .and_then(|conv| load_ir_reusing_live(state, &conv.path));
    let conv_enabled = profile.convolution.as_ref().is_some_and(|c| c.enabled);
    state.rebuild_chain(|ch, sr| {
        let mut chain = profile.clone().into_chain(ch, sr);
        if let Some(ir) = ir.clone() {
            match chain.convolution.load_ir(ir) {
                Ok(()) => chain.convolution.set_enabled(conv_enabled),
                Err(e) => warn!("convolution IR not restored: {e}"),
            }
        }
        chain
    });
}

/// List preset files. `Some(dir)` scans that directory; `None` scans the XDG
/// preset library + system dirs, with user entries shadowing system ones.
fn list_presets(dir: Option<&str>) -> Vec<String> {
    if let Some(d) = dir {
        list_dir_presets(std::path::Path::new(d))
    } else {
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

fn list_dir_presets(dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(std::result::Result::ok)
        .filter(|e| {
            let name = e.file_name();
            let n = name.to_string_lossy();
            n.ends_with(".fac") || n.ends_with(".txt")
        })
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect()
}

async fn read_command_async<R>(reader: &mut R) -> Result<Command>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf);
    if len > resonance_ipc::transport::MAX_MSG_LEN {
        anyhow::bail!("message too large: {len} bytes");
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    Ok(postcard::from_bytes(&buf)?)
}

async fn write_response_async<W>(writer: &mut W, resp: &Response) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    let bytes = postcard::to_stdvec(resp)?;
    let len = (bytes.len() as u32).to_le_bytes();
    writer.write_all(&len).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_to_response_maps_ok_and_err() {
        assert!(matches!(result_to_response(Ok(())), Response::Ok));
        match result_to_response(Err("boom".to_string())) {
            Response::Error(e) => assert_eq!(e, "boom"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn slot_index_maps_a_to_0_and_b_to_1() {
        assert_eq!(slot_index(AbSlot::A), 0);
        assert_eq!(slot_index(AbSlot::B), 1);
    }

    #[test]
    fn band_type_to_apo_is_a_total_one_to_one_mapping() {
        // Spot-check the non-obvious mappings (the LP/HP knobs use the Q'd APO
        // variants) and confirm every BandType variant is handled.
        assert_eq!(band_type_to_apo(BandType::Peaking), ApoFilterType::Peaking);
        assert_eq!(
            band_type_to_apo(BandType::LowShelf),
            ApoFilterType::LowShelf
        );
        assert_eq!(
            band_type_to_apo(BandType::HighShelf),
            ApoFilterType::HighShelf
        );
        assert_eq!(band_type_to_apo(BandType::LowPass), ApoFilterType::LowPassQ);
        assert_eq!(
            band_type_to_apo(BandType::HighPass),
            ApoFilterType::HighPassQ
        );
        assert_eq!(
            band_type_to_apo(BandType::BandPass),
            ApoFilterType::BandPass
        );
        assert_eq!(band_type_to_apo(BandType::Notch), ApoFilterType::Notch);
        assert_eq!(band_type_to_apo(BandType::AllPass), ApoFilterType::AllPass);
    }

    #[test]
    fn file_stem_name_uses_stem_and_falls_back() {
        assert_eq!(file_stem_name("/presets/Rock.fac"), "Rock");
        assert_eq!(file_stem_name("Bass Boost.txt"), "Bass Boost");
        // No stem (e.g. a bare dotfile) falls back to a default name.
        assert_eq!(file_stem_name(""), "imported");
    }

    #[test]
    fn channel_indices_in_range_rejects_out_of_bounds() {
        // Valid stereo swap.
        assert!(channel_indices_in_range(2, 0, 1));
        // Either index at or beyond the channel count is rejected.
        assert!(!channel_indices_in_range(2, 0, 2));
        assert!(!channel_indices_in_range(2, 2, 0));
        // Zero channels rejects everything.
        assert!(!channel_indices_in_range(0, 0, 0));
    }

    // ── Convolution end-to-end (real WAV file → dispatch → chain) ──────────

    fn test_state() -> (SharedState, rtrb::Consumer<AudioCommand>) {
        let (tx, rx) = rtrb::RingBuffer::new(16);
        let (route_tx, _route_rx) = std::sync::mpsc::channel();
        let (app_tx, _app_rx) = std::sync::mpsc::channel();
        let (sink_tx, _sink_rx) = std::sync::mpsc::channel();
        (
            SharedState::new(
                tx,
                route_tx,
                std::sync::Arc::new(crate::meters::AtomicMeters::default()),
                app_tx,
                sink_tx,
            ),
            rx,
        )
    }

    /// Write a mono float32 WAV at 48 kHz (the test chains' rate → no resample,
    /// so tap values survive exactly) and return its path.
    fn write_test_ir(dir_tag: &str, taps: &[f32]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(dir_tag);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ir.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for &t in taps {
            w.write_sample(t).unwrap();
        }
        w.finalize().unwrap();
        path
    }

    #[tokio::test]
    async fn set_convolution_ir_loads_wav_end_to_end() {
        use resonance_dsp::convolution::BLOCK;
        let path = write_test_ir("resonance-ipcconv-load", &[1.0, 0.5]);
        let (state, mut rx) = test_state();

        let resp = dispatch(
            Command::SetConvolutionIr {
                path: path.to_string_lossy().into_owned(),
            },
            &state,
        )
        .await;
        assert!(
            matches!(resp, Response::Ok),
            "load should succeed: {resp:?}"
        );

        // The snapshot (shadow chain) reflects the loaded IR.
        let conv = state.snapshot().convolution.expect("IR should be loaded");
        assert_eq!(conv.name, "ir");
        assert_eq!(conv.taps, 2);
        assert_eq!(conv.ir_channels, 1);
        assert!(conv.enabled);
        assert_eq!(conv.latency_frames, BLOCK);

        // The RT thread receives the prepared engine over the command ring and
        // actually convolves: an impulse comes out delayed by one block with
        // the file's tap values.
        let mut rt_chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48_000.0)
            .build();
        while let Ok(cmd) = rx.pop() {
            crate::audio::apply_command(&mut rt_chain, cmd);
        }
        let frames = 3 * BLOCK;
        let mut buf = vec![0.0f64; frames * 2];
        buf[0] = 1.0; // impulse, both channels of frame 0
        buf[1] = 1.0;
        rt_chain.process(&mut buf);
        for ch in 0..2 {
            assert!(
                (buf[BLOCK * 2 + ch] - 1.0).abs() < 1e-9,
                "h[0] should appear at frame BLOCK on ch{ch}"
            );
            assert!(
                (buf[(BLOCK + 1) * 2 + ch] - 0.5).abs() < 1e-9,
                "h[1] should appear at frame BLOCK+1 on ch{ch}"
            );
            assert!(
                buf[ch].abs() < 1e-12,
                "priming region must be silent on ch{ch}"
            );
        }

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn convolution_toggle_and_clear_round_trip() {
        let path = write_test_ir("resonance-ipcconv-toggle", &[1.0]);
        let (state, _rx) = test_state();

        // Toggling with nothing loaded is a clean error, not a silent no-op.
        let resp = dispatch(Command::SetConvolutionEnabled { enabled: false }, &state).await;
        assert!(matches!(resp, Response::Error(_)));

        let path_str = path.to_string_lossy().into_owned();
        let resp = dispatch(Command::SetConvolutionIr { path: path_str }, &state).await;
        assert!(matches!(resp, Response::Ok));

        // Bypass: still reported (so the UI can re-arm it) but with zero latency.
        let resp = dispatch(Command::SetConvolutionEnabled { enabled: false }, &state).await;
        assert!(matches!(resp, Response::Ok));
        let conv = state
            .snapshot()
            .convolution
            .expect("IR kept while bypassed");
        assert!(!conv.enabled);
        assert_eq!(conv.latency_frames, 0);

        // Clear drops it entirely.
        let resp = dispatch(Command::ClearConvolutionIr, &state).await;
        assert!(matches!(resp, Response::Ok));
        assert!(state.snapshot().convolution.is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn missing_wav_file_returns_error_and_loads_nothing() {
        let (state, _rx) = test_state();
        let resp = dispatch(
            Command::SetConvolutionIr {
                path: "/nonexistent/definitely/missing-ir.wav".to_string(),
            },
            &state,
        )
        .await;
        match resp {
            Response::Error(e) => assert!(e.contains("missing-ir.wav"), "error names file: {e}"),
            other => panic!("expected Error, got {other:?}"),
        }
        assert!(state.snapshot().convolution.is_none());
    }

    #[tokio::test]
    async fn capture_output_returns_freshest_samples_and_rate() {
        let (state, _rx) = test_state();
        // Empty buffer → empty (but well-formed) reply.
        match dispatch(Command::CaptureOutput { frames: 128 }, &state).await {
            Response::Capture { samples, rate } => {
                assert!(samples.is_empty());
                assert!(rate > 0.0);
            }
            other => panic!("expected Capture, got {other:?}"),
        }
        // Fill the rolling buffer; asking for fewer frames returns the TAIL
        // (the freshest audio), not the head.
        {
            let mut inner = state.0.lock().unwrap();
            inner.capture.extend((0..1000).map(|i| i as f32));
        }
        match dispatch(Command::CaptureOutput { frames: 10 }, &state).await {
            Response::Capture { samples, .. } => {
                let want: Vec<f32> = (990..1000).map(|i| f32::from(i as u16)).collect();
                assert_eq!(samples, want, "must return the newest samples");
            }
            other => panic!("expected Capture, got {other:?}"),
        }
        // Asking for more than buffered returns everything available.
        match dispatch(Command::CaptureOutput { frames: 1_000_000 }, &state).await {
            Response::Capture { samples, .. } => assert_eq!(samples.len(), 1000),
            other => panic!("expected Capture, got {other:?}"),
        }
    }

    #[test]
    fn apply_profile_chain_restores_ir_from_wav_file() {
        let path = write_test_ir("resonance-ipcconv-profile", &[0.25]);
        let (state, _rx) = test_state();
        let profile = Profile {
            preamp_db: 0.0,
            enabled: true,
            effects: EffectsState::default(),
            bands: vec![],
            dither_bits: None,
            convolution: Some(ConvolutionProfile {
                path: path.to_string_lossy().into_owned(),
                enabled: true,
            }),
        };
        apply_profile_chain(&profile, &state);
        let conv = state.snapshot().convolution.expect("profile restores IR");
        assert_eq!(conv.taps, 1);
        assert!(conv.enabled);

        // A profile pointing at a missing file still applies — the IR is just
        // dropped (warned), never a failed profile load.
        let broken = Profile {
            convolution: Some(ConvolutionProfile {
                path: "/nonexistent/gone.wav".to_string(),
                enabled: true,
            }),
            ..profile
        };
        apply_profile_chain(&broken, &state);
        assert!(state.snapshot().convolution.is_none());

        let _ = std::fs::remove_file(&path);
    }
}
