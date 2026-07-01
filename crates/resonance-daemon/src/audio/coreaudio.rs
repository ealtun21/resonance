//! `CoreAudio` backend (via cpal + native Core Audio Process Tap) — macOS.
//!
//! Architecture:
//!   1. `SystemAudioTap` (see `system_tap.rs`) builds a `CATapDescription`
//!      that taps every running process's output. Apple wraps the tap in a
//!      private aggregate device that appears in the Core Audio HAL as a
//!      regular input device — cpal sees and opens it normally.
//!   2. Aggregate-tap input stream → SPSC ring buffer (interleaved f32).
//!      Because the tap is created with `MutedWhenTapped`, opening it
//!      silences the original audio path: the speakers no longer hear the
//!      pre-DSP signal, only what we render below.
//!   3. Default (or preferred) output device → cpal output stream → pops
//!      from the ring, runs the DSP chain, writes processed samples back.
//!   4. Both streams negotiate a common sample rate (target 48 kHz; falls
//!      back to whatever both devices support).
//!   5. A supervisor thread holds the streams alive, polls the device list
//!      and reports it to the daemon, watches `route_rx` for preferred-output
//!      changes (rebuilds when they change), and tears down the tap on
//!      shutdown so the system goes back to normal routing.
//!   6. On any stream/setup error, the supervisor rebuilds with exponential
//!      backoff capped at 5 s, same shape as the `PipeWire` backend's loop.
//!
//! Result: every app's audio (Apple Music, browser, games, calls) flows
//! through the DSP chain — no `BlackHole`, no kernel extension, no manual
//! routing. Requires macOS 14.2+ and the "System Audio Capture" /
//! "Microphone" TCC permission (prompted on first run).

use super::hal_input::HalInputStream;
use super::system_tap::{
    SystemAudioTap, TAP_DEVICE_NAME, default_output_rate, default_output_uid,
    set_aggregate_nominal_rate,
};
use super::{CHANNELS, apply_command};
use crate::meters::{AtomicMeters, Sample, peak_rms, peak_rms_f32};
use crate::state::{AppControl, AudioCommand};
use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Host, StreamConfig};
use resonance_dsp::chain::ProcessorChain;
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc as std_mpsc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Ring-buffer capacity (interleaved stereo f32 samples) between input and
/// output streams. ~340 ms at 48 kHz stereo — generous headroom for jitter
/// without making latency painful.
const RING_CAPACITY_SAMPLES: usize = 32_768;

/// Maximum number of samples drained from the input ring in a single output
/// callback if more are queued than the callback asks for. Prevents an
/// unbounded backlog when the output device runs faster than input briefly.
const DRAIN_SLACK_FRAMES: usize = 4096;

/// Publish the live per-application stream list (~1 Hz) on a dedicated thread.
/// Read-only Core Audio process enumeration — no taps, no TCC — with the user's
/// per-app volume/mute overlaid. Exits when the daemon drops the receiver.
fn spawn_app_enumeration(
    apps_tx: tokio::sync::mpsc::UnboundedSender<Vec<resonance_ipc::AppStream>>,
    gains: Arc<Mutex<HashMap<String, (f64, bool)>>>,
) {
    thread::Builder::new()
        .name("resonance-mac-apps".into())
        .spawn(move || {
            loop {
                let mut apps = super::mac_apps::enumerate();
                super::app_streams::overlay_control_state(&mut apps, &gains.lock().unwrap());
                if apps_tx.send(apps).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(1000));
            }
        })
        .ok();
}

/// Drain per-app volume/mute requests into the shared gain map on a dedicated
/// thread. The mixer increment will read this map to apply gains to the per-app
/// taps; for now it backs the published-list overlay. Exits on sender drop.
fn spawn_app_control(
    app_ctl_rx: std_mpsc::Receiver<AppControl>,
    gains: Arc<Mutex<HashMap<String, (f64, bool)>>>,
) {
    thread::Builder::new()
        .name("resonance-mac-appctl".into())
        .spawn(move || {
            while let Ok(ctl) = app_ctl_rx.recv() {
                let mut map = gains.lock().unwrap();
                match ctl {
                    AppControl::SetVolume { key, volume } => {
                        map.entry(key).or_insert((1.0, false)).0 = volume.clamp(0.0, 4.0);
                    }
                    AppControl::SetMute { key, muted } => {
                        map.entry(key).or_insert((1.0, false)).1 = muted;
                    }
                }
            }
        })
        .ok();
}

pub fn spawn(ctx: super::BackendCtx) -> Result<JoinHandle<()>> {
    let super::BackendCtx {
        cmd_rx,
        spectrum_tx,
        initial_chain,
        output_tx,
        route_rx,
        sinks_tx,
        meters,
        apps_tx,
        app_ctl_rx,
        sinks_vol_tx,
        sink_ctl_rx,
    } = ctx;
    // Per-output-sink volume isn't implemented on macOS yet; drop the channel
    // ends so they're inert.
    let _ = (sinks_vol_tx, sink_ctl_rx);
    // Per-app control state: `key -> (volume, muted)`, set via `app_ctl_rx` and
    // overlaid onto the enumerated list so the published volume reflects what the
    // user set. The muted-tap mixer (next increment) reads this same map to apply
    // the gains to audio; until then it is display-only.
    let app_gains: Arc<Mutex<HashMap<String, (f64, bool)>>> = Arc::new(Mutex::new(HashMap::new()));
    spawn_app_control(app_ctl_rx, Arc::clone(&app_gains));
    spawn_app_enumeration(apps_tx, Arc::clone(&app_gains));
    // Shared chain + RT state lives in an Arc<Mutex<…>>. The output callback
    // locks for the duration of each block — bounded, brief, never contended
    // against another high-priority thread. Reconnect rebuilds the streams but
    // preserves the chain state (history, coefficients, command queue).
    let shared = Arc::new(Mutex::new(SharedRt {
        chain: initial_chain,
        cmd_rx,
        spectrum_tx,
        scratch: Vec::with_capacity(8192 * CHANNELS),
        routed: Vec::with_capacity(8192 * CHANNELS),
        meters: Arc::clone(&meters),
    }));

    Ok(thread::Builder::new()
        .name("resonance-coreaudio".into())
        .spawn(move || {
            let mut preferred_output: Option<String> = None;
            let mut last_active_output: Option<String> = None;
            let mut backoff = Duration::from_millis(200);

            // ── Per-application mixer mode (opt-in: RESONANCE_PERAPP) ─────────
            // Tap every audio-producing app individually (muted), gain + sum to
            // stereo, then run the chain. Opt-in so the default device-bound tap
            // (multichannel, system-wide EQ) is never regressed.
            if std::env::var_os("RESONANCE_PERAPP").is_some() {
                info!("CoreAudio: per-application mixer mode (RESONANCE_PERAPP)");
                loop {
                    while let Ok(name) = route_rx.try_recv() {
                        preferred_output = if name.is_empty() { None } else { Some(name) };
                    }
                    let targets = super::mac_apps::enumerate_targets();
                    if targets.is_empty() {
                        // Nothing is producing audio — nothing to tap/mix yet.
                        thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                    let started = Instant::now();
                    match SystemAudioTap::create_app_mixer(&targets) {
                        Ok((tap, keys)) => {
                            let pref = preferred_output.clone();
                            match run_streams(
                                &shared,
                                pref.as_deref(),
                                &output_tx,
                                &sinks_tx,
                                &mut last_active_output,
                                &route_rx,
                                &tap,
                                Some((keys, Arc::clone(&app_gains))),
                            ) {
                                Ok(StreamExit::RouteChanged(new_pref)) => {
                                    preferred_output = if new_pref.is_empty() {
                                        None
                                    } else {
                                        Some(new_pref)
                                    };
                                    backoff = Duration::from_millis(50);
                                }
                                Ok(StreamExit::Ended) => {}
                                Err(e) => warn!("CoreAudio per-app: {e:#}; retrying…"),
                            }
                        }
                        Err(e) => {
                            warn!("CoreAudio per-app mixer tap failed: {e:#}; retrying…");
                            thread::sleep(backoff);
                            backoff = (backoff * 2).min(Duration::from_secs(5));
                            continue;
                        }
                    }
                    if started.elapsed() > Duration::from_secs(10) {
                        backoff = Duration::from_millis(200);
                    }
                    thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                }
            }

            // The tap binds to the current default output device so it inherits
            // that device's channel layout + sample rate. It is kept alive across
            // stream reconnects and recreated only when the bound device changes,
            // so we don't churn the system-audio routing on every hotplug. Lazily
            // created on first setup (avoids prompting for System Audio Capture
            // before the daemon is fully up).
            let mut tap: Option<SystemAudioTap> = None;
            let mut tap_uid: Option<String> = None;
            let mut tap_rate: Option<f64> = None;

            loop {
                while let Ok(name) = route_rx.try_recv() {
                    // Empty = follow the OS default output (clear the pin).
                    preferred_output = if name.is_empty() { None } else { Some(name) };
                }

                // (Re)create the tap when missing, bound to a now-stale device, or
                // bound at a stale rate. The device-bound tap inherits the
                // device's rate at creation, so a rate change (Audio MIDI Setup,
                // BT codec) needs a fresh tap to keep capture == output and the
                // resampler bypassed.
                let want_uid = default_output_uid().ok();
                let want_rate = default_output_rate();
                let rate_stale =
                    matches!((tap_rate, want_rate), (Some(a), Some(b)) if (a - b).abs() > 1.0);
                if tap.is_none() || tap_uid != want_uid || rate_stale {
                    let Some(uid) = want_uid.clone() else {
                        warn!("CoreAudio: no default output device yet; retrying…");
                        thread::sleep(backoff);
                        backoff = (backoff * 2).min(Duration::from_secs(5));
                        continue;
                    };
                    // Destroy the existing tap before creating the new one so the
                    // old aggregate is gone (resources freed) before we bind a
                    // fresh tap at the current device/rate.
                    tap = None;
                    tap_uid = None;
                    tap_rate = None;
                    match SystemAudioTap::create(&uid) {
                        Ok(t) => {
                            tap = Some(t);
                            tap_uid = Some(uid);
                            tap_rate = want_rate;
                        }
                        Err(e) => {
                            warn!("CoreAudio: tap setup failed: {e:#}; retrying…");
                            thread::sleep(backoff);
                            backoff = (backoff * 2).min(Duration::from_secs(5));
                            continue;
                        }
                    }
                }

                let started = Instant::now();
                let pref_snapshot = preferred_output.clone();
                match run_streams(
                    &shared,
                    pref_snapshot.as_deref(),
                    &output_tx,
                    &sinks_tx,
                    &mut last_active_output,
                    &route_rx,
                    tap.as_ref().unwrap(),
                    None,
                ) {
                    Ok(StreamExit::RouteChanged(new_pref)) => {
                        preferred_output = if new_pref.is_empty() {
                            None
                        } else {
                            Some(new_pref)
                        };
                        backoff = Duration::from_millis(50);
                    }
                    Ok(StreamExit::Ended) => {
                        warn!("CoreAudio: stream ended; reconnecting…");
                    }
                    Err(e) => warn!("CoreAudio: setup failed: {e:#}; retrying…"),
                }

                if started.elapsed() > Duration::from_secs(10) {
                    backoff = Duration::from_millis(200);
                }
                thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        })?)
}

/// Shared state between supervisor and output callback. Guarded by a mutex
/// because cpal callbacks run on their own audio thread and we only need
/// access during reconfiguration — actual per-block work happens inside the
/// callback while it owns the lock.
struct SharedRt {
    chain: ProcessorChain,
    cmd_rx: rtrb::Consumer<AudioCommand>,
    spectrum_tx: rtrb::Producer<f32>,
    /// Reusable interleaved f64 scratch buffer to avoid per-callback allocation.
    scratch: Vec<f64>,
    /// Second scratch for the routing matrix output (square remap); same size.
    routed: Vec<f64>,
    meters: Arc<AtomicMeters>,
}

enum StreamExit {
    /// The supervisor noticed a `route_rx` update; rebuild against the new
    /// preferred output (returned in the variant).
    RouteChanged(String),
    /// Streams exited cleanly (e.g. device unplugged) — caller reconnects.
    Ended,
}

/// Per-app mixer wiring for the output callback (per-app mode only). `keys` maps
/// each aggregate tap-channel-pair to its application key, in tap order — pair
/// `i` (input channels `[2i, 2i+1]`) is `keys[i]`. The callback gains each pair
/// by that app's volume (0 when muted) and sums to stereo before the chain.
struct MixerSpec {
    keys: Vec<String>,
    gains: Arc<Mutex<HashMap<String, (f64, bool)>>>,
}

#[allow(clippy::too_many_arguments)]
fn run_streams(
    shared: &Arc<Mutex<SharedRt>>,
    preferred: Option<&str>,
    output_tx: &tokio::sync::mpsc::UnboundedSender<String>,
    sinks_tx: &tokio::sync::mpsc::UnboundedSender<Vec<(String, String)>>,
    last_active_output: &mut Option<String>,
    route_rx: &std_mpsc::Receiver<String>,
    tap: &SystemAudioTap,
    mixer: Option<(Vec<String>, Arc<Mutex<HashMap<String, (f64, bool)>>>)>,
) -> Result<StreamExit> {
    // In per-app mode the ring carries `2 * keys.len()` channels (one stereo pair
    // per app) which the callback folds to a stereo mix, so the chain processes
    // stereo. Keep the app key list for live app-set churn detection below.
    let churn_keys: Option<Vec<String>> = mixer.as_ref().map(|(k, _)| k.clone());
    let tap_aggregate_id = tap.aggregate_id();
    let tap_native_rate = tap.native_rate();
    let host = cpal::default_host();
    let output_dev = pick_output_device(&host, preferred)?;
    let output_name = device_name(&output_dev);
    info!("CoreAudio: tap input = aggregate {tap_aggregate_id}, output = {output_name}");

    let out_default = output_dev
        .default_output_config()
        .with_context(|| "default_output_config")?;
    let sample_rate = out_default.sample_rate();
    let out_cfg = StreamConfig {
        channels: out_default.channels(),
        sample_rate,
        buffer_size: cpal::BufferSize::Default,
    };

    // Capture at the output rate when the tap can source it (≤ its native mix
    // rate) so hal_input bypasses the resampler — no 48 kHz → device-rate
    // conversion. Above the native rate the tap would under-deliver and the
    // output ring would starve, so cap the tap at native and let hal_input
    // resample up instead. A no-op when already matched; on a HAL refusal the
    // tap stays put and hal_input resamples, exactly as before.
    let tap_target = f64::from(sample_rate).min(tap_native_rate);
    set_aggregate_nominal_rate(tap_aggregate_id, tap_target);

    // Report the active output device by name (the daemon uses this to map
    // device → profile). Dedup so we don't spam the channel.
    if last_active_output.as_deref() != Some(output_name.as_str()) {
        *last_active_output = Some(output_name.clone());
        let _ = output_tx.send(output_name.clone());
    }
    publish_sinks(&host, sinks_tx);

    // ── Stream construction ────────────────────────────────────────────────
    let out_channels = out_cfg.channels.max(1) as usize;

    // Size the ring for ~340 ms of interleaved audio at the device width + rate,
    // so it scales with channel count (a fixed stereo size would be far too small
    // at 16ch/96k). The output callback's backlog-drain keeps steady-state
    // latency low regardless of this ceiling.
    let ring_capacity =
        ((f64::from(sample_rate) * 0.34) as usize * out_channels).max(RING_CAPACITY_SAMPLES);
    let (ring_tx, ring_rx) = rtrb::RingBuffer::<f32>::new(ring_capacity);

    // Stream error → flag; the supervisor sees it on the next poll and rebuilds.
    let stream_err = Arc::new(AtomicBool::new(false));

    // Raw HAL IOProc on the aggregate-tap device — bypasses cpal/AUHAL,
    // which doesn't reliably surface the tap stream and was reading silence.
    let hal_input = HalInputStream::open(tap_aggregate_id, ring_tx, f64::from(sample_rate))
        .with_context(|| "open HAL input on tap aggregate")?;
    let capture_channels = hal_input.capture_channels;

    {
        let mut guard = shared.lock().unwrap();
        // Re-bind the DSP chain to the output device's sample rate (filter +
        // effect coefficients) and set its width to the tap's channel count: the
        // device-bound tap delivers the device's full channel layout, so the
        // chain processes every channel (with per-channel EQ) instead of a forced
        // stereo mixdown.
        guard.chain.rebind_sample_rate(f64::from(sample_rate));
        // Per-app mixer folds the tap pairs to stereo before the chain, so the
        // chain runs at stereo width there; otherwise the device's full width.
        let chain_channels = if mixer.is_some() { 2 } else { capture_channels };
        guard.chain.set_channels(chain_channels);
        // Publish the live capture rate + width for `status` (and whether the
        // IOProc is resampling — it isn't, once the tap is bound to the device).
        guard.meters.set_capture_rate(hal_input.capture_rate);
        guard.meters.set_channels(chain_channels);
    }
    let input_callbacks = Arc::clone(&hal_input.callback_count);
    let input_nonzero = Arc::clone(&hal_input.nonzero_blocks);

    let output_stream = build_output_stream(
        &output_dev,
        &out_cfg,
        capture_channels,
        out_channels,
        ring_rx,
        Arc::clone(shared),
        Arc::clone(&stream_err),
        mixer.map(|(keys, gains)| MixerSpec { keys, gains }),
    )
    .with_context(|| "build output stream")?;
    output_stream.play().with_context(|| "play output stream")?;

    info!(
        "CoreAudio ready — {sample_rate} Hz, HAL tap input ({capture_channels} ch) → \
         DSP → output {out_channels} ch"
    );

    // ── Supervisor loop ───────────────────────────────────────────────────
    // Poll devices + route changes ~4×/sec. Streams play in their own threads.
    let mut sink_poll = Instant::now();
    let mut diag_poll = Instant::now();
    let started = Instant::now();
    let mut warned_silent = false;
    let _keep_input_alive = &hal_input; // hold the IOProc until function exits
    loop {
        if stream_err.load(Ordering::Relaxed) {
            return Ok(StreamExit::Ended);
        }
        if let Ok(name) = route_rx.try_recv() {
            // A route change forces a stream rebuild (the new output device
            // may differ from the current one).
            return Ok(StreamExit::RouteChanged(name));
        }
        if diag_poll.elapsed() >= Duration::from_secs(2) {
            diag_poll = Instant::now();
            let cb = input_callbacks.load(Ordering::Relaxed);
            let nz = input_nonzero.load(Ordering::Relaxed);
            info!("HAL tap IOProc: {cb} callbacks, {nz} with audio");
            // After 5 s with no non-zero blocks, surface a one-shot TCC hint.
            if !warned_silent && cb > 50 && nz == 0 && started.elapsed() >= Duration::from_secs(5) {
                warn!(
                    "tap IOProc has fired {cb} times but every block was silent — \
                     macOS is most likely refusing system audio capture. Open the \
                     bundled Resonance.app via Launch Services and grant the prompt: \
                     `open ~/Applications/Resonance.app` then approve System Audio \
                     Recording / Microphone in Privacy & Security."
                );
                warned_silent = true;
            }
        }
        if sink_poll.elapsed() >= Duration::from_millis(750) {
            sink_poll = Instant::now();
            publish_sinks(&host, sinks_tx);
            // Check if the default output changed under us (e.g. user plugged
            // in headphones). If so, rebuild so we follow it.
            if preferred.is_none() {
                if let Some(cur) = host.default_output_device().map(|d| device_name(&d)) {
                    if cur != output_name {
                        info!("CoreAudio: default output changed → {cur}; rebuilding");
                        return Ok(StreamExit::Ended);
                    }
                }
            }
            // Follow a sample-rate change on the SAME device (Audio MIDI Setup
            // change, BT codec renegotiation). cpal's default config reflects
            // the device's current nominal rate; a change means the streams and
            // the tap rate are stale, so rebuild to re-derive both.
            if let Ok(cfg) = output_dev.default_output_config() {
                let new_rate = cfg.sample_rate();
                if new_rate != sample_rate {
                    info!("CoreAudio: output rate {sample_rate} → {new_rate} Hz; rebuilding");
                    return Ok(StreamExit::Ended);
                }
            }
            // Per-app mode: rebuild the mixer when the set of audio-producing
            // apps changes (an app started or stopped playing), so taps track
            // the live set. Compared order-insensitively against the live keys.
            if let Some(keys) = &churn_keys {
                let mut cur: Vec<String> = super::mac_apps::enumerate_targets()
                    .into_iter()
                    .map(|(k, _)| k)
                    .collect();
                cur.sort();
                let mut have = keys.clone();
                have.sort();
                if cur != have {
                    info!("CoreAudio per-app: app set changed; rebuilding mixer");
                    return Ok(StreamExit::Ended);
                }
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[allow(clippy::too_many_arguments)]
fn build_output_stream(
    device: &Device,
    cfg: &StreamConfig,
    ring_channels: usize,
    device_channels: usize,
    mut ring_rx: rtrb::Consumer<f32>,
    shared: Arc<Mutex<SharedRt>>,
    err_flag: Arc<AtomicBool>,
    mixer: Option<MixerSpec>,
) -> Result<cpal::Stream> {
    let err_cb = {
        let e = Arc::clone(&err_flag);
        move |err| {
            warn!("CoreAudio output stream error: {err}");
            e.store(true, Ordering::Relaxed);
        }
    };
    let rc = ring_channels.max(1);
    let dc = device_channels.max(1);
    // Pre-allocate a per-callback work buffer (ring-channel-wide) — avoids alloc
    // on the audio thread. Grown if a callback ever asks for more frames.
    let mut work: Vec<f32> = Vec::with_capacity(8192 * rc);
    // Per-app mixer: stereo scratch the per-app pairs are gained+summed into,
    // becoming the chain input. Unused (stays empty) in the normal pass-through.
    let mut mix: Vec<f32> = Vec::new();

    let data_cb = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
        let frames = data.len() / dc;
        let needed = frames * rc;
        if work.len() < needed {
            work.resize(needed, 0.0);
        }
        let buf = &mut work[..needed];

        // Pull `frames` ring-channel-wide frames from the input ring; zero-pad if
        // short (silence beats xrun chaos). Drain any backlog beyond the slack so
        // steady-state latency doesn't grow.
        let avail_frames = ring_rx.slots() / rc;
        let copy_frames = frames.min(avail_frames);
        let backlog_frames = avail_frames.saturating_sub(frames);
        let drop_frames = backlog_frames.saturating_sub(DRAIN_SLACK_FRAMES);
        for _ in 0..drop_frames * rc {
            let _ = ring_rx.pop();
        }
        for s in buf.iter_mut().take(copy_frames * rc) {
            *s = ring_rx.pop().unwrap_or(0.0);
        }
        for s in buf.iter_mut().skip(copy_frames * rc) {
            *s = 0.0;
        }

        // ── Per-app fold → chain input ─────────────────────────────────────
        // Normal: the chain processes the ring buffer (device width) directly.
        // Per-app mixer: each app occupies channel pair `[2i,2i+1]`; gain it by
        // the app's volume (0 when muted) and sum to stereo — that sum is the
        // chain input, so the chain runs at stereo width. Summing all apps
        // reproduces the system mix, preserving system-wide EQ.
        let (chain_buf, cc): (&mut [f32], usize) = match &mixer {
            Some(m) => {
                let pairs = (rc / 2).min(m.keys.len());
                let cn = frames * 2;
                if mix.len() < cn {
                    mix.resize(cn, 0.0);
                }
                let mixb = &mut mix[..cn];
                let gains: Vec<f32> = {
                    let gmap = m.gains.lock().unwrap();
                    m.keys
                        .iter()
                        .map(|k| match gmap.get(k).copied() {
                            Some((_, true)) => 0.0,
                            Some((g, false)) => g as f32,
                            None => 1.0,
                        })
                        .collect()
                };
                for f in 0..frames {
                    let base = f * rc;
                    let mut l = 0.0f32;
                    let mut r = 0.0f32;
                    for (i, &g) in gains.iter().enumerate().take(pairs) {
                        l += buf[base + 2 * i] * g;
                        r += buf[base + 2 * i + 1] * g;
                    }
                    mixb[f * 2] = l;
                    mixb[f * 2 + 1] = r;
                }
                (mixb, 2)
            }
            None => (buf, rc),
        };
        let cneeded = frames * cc;

        // ── DSP ───────────────────────────────────────────────────────────
        let mut s = shared.lock().unwrap();

        // Apply pending IPC commands first so latency is one block at most.
        while let Ok(cmd) = s.cmd_rx.pop() {
            apply_command(&mut s.chain, cmd);
        }
        // Publish the live DSP rate for `status` (the chain was rebound to the
        // output device rate at stream setup).
        s.meters.set_sample_rate(s.chain.sample_rate);

        let (in_peak, in_rms) = peak_rms_f32(chain_buf);

        if s.chain.enabled {
            let need_f64 = cneeded;
            if s.scratch.len() < need_f64 {
                s.scratch.resize(need_f64, 0.0);
            }
            if s.routed.len() < need_f64 {
                s.routed.resize(need_f64, 0.0);
            }
            // Promote to f64, process, demote.
            for (dst, src) in s.scratch[..need_f64].iter_mut().zip(chain_buf.iter()) {
                *dst = f64::from(*src);
            }
            let t0 = Instant::now();
            // Split-borrow: separate `&mut chain`/`scratch`/`routed` so process +
            // route don't second-borrow `s` whole. Returns the post-DSP peaks; the
            // borrow ends with the block so the meter writes below can reborrow `s`.
            let (out_peak, out_rms) = {
                let SharedRt {
                    chain,
                    scratch,
                    routed,
                    ..
                } = &mut *s;
                chain.process(&mut scratch[..need_f64]);
                // Square output routing (per-channel gain / channel swap) — parity
                // with the PipeWire/APO backends. `route` copies for the identity
                // / no-matrix case.
                let out: &[f64] = if chain.routing.is_some() {
                    chain.route(&scratch[..need_f64], &mut routed[..need_f64]);
                    &routed[..need_f64]
                } else {
                    &scratch[..need_f64]
                };
                let pr = peak_rms(out);
                for (dst, src) in chain_buf.iter_mut().zip(out.iter()) {
                    *dst = *src as f32;
                }
                pr
            };
            let dt = t0.elapsed();

            let sr = s.chain.sample_rate;
            let budget = frames as f64 / sr;
            let load = if budget > 0.0 {
                (dt.as_secs_f64() / budget) as f32
            } else {
                0.0
            };
            s.meters.store(Sample {
                in_peak,
                out_peak,
                in_rms,
                out_rms,
                clip: out_peak >= 0.999,
                dsp_load: load,
                dsp_frame_us: dt.as_micros() as u32,
            });
        } else {
            // Passthrough — in==out, no DSP cost.
            s.meters.store(Sample {
                in_peak,
                out_peak: in_peak,
                in_rms,
                out_rms: in_rms,
                clip: in_peak >= 0.999,
                dsp_load: 0.0,
                dsp_frame_us: 0,
            });
        }

        // Feed the spectrum ring from the OUTPUT (`chain_buf`): per-frame mono
        // average across its channels. Outside the `enabled` check so the
        // analyzer keeps tracking live audio when power is off.
        let cap = s.spectrum_tx.slots();
        let push_n = frames.min(cap);
        for i in 0..push_n {
            let base = i * cc;
            let sum: f32 = chain_buf[base..base + cc].iter().sum();
            let _ = s.spectrum_tx.push(sum / cc as f32);
        }
        drop(s);

        // ── Render to output ──────────────────────────────────────────────
        render_to_output(chain_buf, data, cc, dc);

        let _ = err_flag.load(Ordering::Relaxed);
    };

    let stream = device
        .build_output_stream(cfg, data_cb, err_cb, None)
        .with_context(|| "build_output_stream")?;
    Ok(stream)
}

/// Map the processed interleaved buffer (`processed`, `src_channels` wide) onto
/// the output device buffer `data` (`dst_channels` wide), frame by frame: copy
/// the first `min(src, dst)` channels, zero-pad any extra device channels, drop
/// any extra source channels. The chain runs at the device width and the routing
/// matrix has done any in-width remap, so the common case is an equal-width copy.
/// Pure (no device/RT state) so it's unit-testable.
fn render_to_output(processed: &[f32], data: &mut [f32], src_channels: usize, dst_channels: usize) {
    if dst_channels == 0 {
        return;
    }
    for (f, frame) in data.chunks_mut(dst_channels).enumerate() {
        for (c, d) in frame.iter_mut().enumerate() {
            *d = if c < src_channels {
                processed.get(f * src_channels + c).copied().unwrap_or(0.0)
            } else {
                0.0
            };
        }
    }
}

/// Pick the output device the user prefers, falling back to the default.
/// Excludes our own tap aggregate device — that's an input, not a real
/// output target.
fn pick_output_device(host: &Host, preferred: Option<&str>) -> Result<Device> {
    if let Some(name) = preferred {
        if let Ok(devs) = host.output_devices() {
            for d in devs {
                if device_name(&d) == name {
                    return Ok(d);
                }
            }
            info!("preferred output '{name}' not found — falling back to default");
        }
    }
    host.default_output_device()
        .ok_or_else(|| anyhow!("no default output device"))
}

/// Best-effort human-readable name for a cpal device. `description().name()`
/// is the post-0.16 API; on transient errors we fall back to a placeholder so
/// callers can still match/log the device.
fn device_name(d: &Device) -> String {
    match d.description() {
        Ok(desc) => desc.name().to_string(),
        Err(_) => "(unknown)".to_string(),
    }
}

/// Enumerate output devices and publish `(name, description)` pairs. cpal does
/// not expose a friendly description per device, so we duplicate the name into
/// the description slot — clients fall back to the name regardless.
fn publish_sinks(
    host: &Host,
    sinks_tx: &tokio::sync::mpsc::UnboundedSender<Vec<(String, String)>>,
) {
    let Ok(devs) = host.output_devices() else {
        return;
    };
    let mut out: Vec<(String, String)> = devs
        .map(|d| {
            let n = device_name(&d);
            (n.clone(), n)
        })
        // Hide our own private aggregate so it doesn't appear in client UIs
        // as a selectable output — it's an input wrapper only.
        .filter(|(n, _)| n != TAP_DEVICE_NAME)
        .collect();
    out.sort();
    out.dedup_by(|a, b| a.0 == b.0);
    let _ = sinks_tx.send(out);
}

#[cfg(test)]
mod tests {
    use super::*;

    // Compile-time invariants on the buffer constants — the audio thread
    // depends on these holding, so we fail the build (not the test run) if
    // someone shrinks them.
    const _RING_SIZE_OK: () = assert!(
        RING_CAPACITY_SAMPLES >= 9_600 * 2,
        "ring buffer must hold ≥100 ms of stereo 48 kHz audio"
    );
    const _DRAIN_SLACK_OK: () = assert!(
        DRAIN_SLACK_FRAMES * 2 < RING_CAPACITY_SAMPLES,
        "DRAIN_SLACK_FRAMES must leave room in the ring"
    );

    #[test]
    fn host_lists_at_least_default_output() {
        // The default macOS host always has at least one output device
        // (the built-in speakers / aggregate). Asserts cpal links + the
        // host enumerates devices without permission prompts.
        let host = cpal::default_host();
        let count = host.output_devices().map_or(0, |d| d.count());
        assert!(count >= 1, "expected ≥1 output device, got {count}");
    }

    #[test]
    fn pick_output_falls_back_to_default_when_preferred_missing() {
        let host = cpal::default_host();
        let dev = pick_output_device(&host, Some("definitely-not-a-real-device-1234"))
            .expect("default output should exist");
        assert!(!device_name(&dev).is_empty(), "device name should resolve");
    }

    #[test]
    // Equal-width render is an exact copy; the float equality is intentional.
    #[allow(clippy::float_cmp)]
    fn render_equal_width_is_passthrough() {
        let processed = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0]; // 2 frames * 3ch
        let mut data = [0.0f32; 6];
        render_to_output(&processed, &mut data, 3, 3);
        assert_eq!(data, processed);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn render_narrow_src_zero_pads_extra_device_channels() {
        let processed = [1.0f32, 2.0, 3.0, 4.0]; // 2 frames * 2ch
        let mut data = [9.0f32; 2 * 4]; // dst 4ch, pre-filled to catch non-writes
        render_to_output(&processed, &mut data, 2, 4);
        assert_eq!(data[0..4], [1.0, 2.0, 0.0, 0.0]);
        assert_eq!(data[4..8], [3.0, 4.0, 0.0, 0.0]);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn render_wide_src_drops_extra_channels() {
        let processed = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0]; // 2 frames * 3ch
        let mut data = [0.0f32; 2 * 2]; // dst 2ch
        render_to_output(&processed, &mut data, 3, 2);
        assert_eq!(data[0..2], [1.0, 2.0]);
        assert_eq!(data[2..4], [4.0, 5.0]);
    }

    #[test]
    fn render_zero_channels_is_noop() {
        let processed = [1.0f32, 2.0];
        let mut data: [f32; 0] = [];
        render_to_output(&processed, &mut data, 2, 0); // must not panic
    }
}
