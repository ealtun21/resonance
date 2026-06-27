//! CoreAudio backend (via cpal + native Core Audio Process Tap) — macOS.
//!
//! Architecture:
//!   1. `SystemAudioTap` (see `system_tap.rs`) builds a CATapDescription
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
//!      backoff capped at 5 s, same shape as the PipeWire backend's loop.
//!
//! Result: every app's audio (Apple Music, browser, games, calls) flows
//! through the DSP chain — no BlackHole, no kernel extension, no manual
//! routing. Requires macOS 14.2+ and the "System Audio Capture" /
//! "Microphone" TCC permission (prompted on first run).

use super::hal_input::HalInputStream;
use super::system_tap::{SystemAudioTap, TAP_DEVICE_NAME};
use super::{CHANNELS, apply_command};
use crate::meters::{AtomicMeters, Sample, peak_rms, peak_rms_f32};
use crate::state::AudioCommand;
use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Host, StreamConfig};
use objc2_core_audio::AudioObjectID;
use resonance_dsp::chain::ProcessorChain;
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

pub fn spawn(ctx: super::BackendCtx) -> Result<JoinHandle<()>> {
    let super::BackendCtx {
        cmd_rx,
        spectrum_tx,
        initial_chain,
        output_tx,
        route_rx,
        sinks_tx,
        meters,
    } = ctx;
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
            // The tap is reusable across stream reconnects — keep it alive
            // for the whole supervisor lifetime so we don't churn the
            // system-audio routing on every device hotplug. Lazily created
            // on the first successful setup (avoids prompting for the
            // System Audio Capture permission before the daemon has fully
            // come up).
            let mut tap: Option<SystemAudioTap> = None;

            loop {
                while let Ok(name) = route_rx.try_recv() {
                    // Empty = follow the OS default output (clear the pin).
                    preferred_output = if name.is_empty() { None } else { Some(name) };
                }

                if tap.is_none() {
                    match SystemAudioTap::create() {
                        Ok(t) => tap = Some(t),
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
                let agg_id = tap.as_ref().unwrap().aggregate_id();
                match run_streams(
                    Arc::clone(&shared),
                    pref_snapshot.as_deref(),
                    &output_tx,
                    &sinks_tx,
                    &mut last_active_output,
                    &route_rx,
                    agg_id,
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

fn run_streams(
    shared: Arc<Mutex<SharedRt>>,
    preferred: Option<&str>,
    output_tx: &tokio::sync::mpsc::UnboundedSender<String>,
    sinks_tx: &tokio::sync::mpsc::UnboundedSender<Vec<(String, String)>>,
    last_active_output: &mut Option<String>,
    route_rx: &std_mpsc::Receiver<String>,
    tap_aggregate_id: AudioObjectID,
) -> Result<StreamExit> {
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

    {
        let mut guard = shared.lock().unwrap();
        // Re-bind the DSP chain to the output device's sample rate so the
        // filter *and* effect coefficients are correct for the rate we render at.
        guard.chain.rebind_sample_rate(sample_rate as f64);
        // The process tap captures the stereo system mix, and the IOProc always
        // emits L/R pairs, so the chain processes stereo regardless of
        // RESONANCE_CHANNELS. Pin its width to the stereo buffers the callback
        // feeds — a stray env override must not desync chain.channels from the
        // interleaved-stereo work buffer (which would misframe every block).
        guard.chain.set_channels(2);
    }

    // Report the active output device by name (the daemon uses this to map
    // device → profile). Dedup so we don't spam the channel.
    if last_active_output.as_deref() != Some(output_name.as_str()) {
        *last_active_output = Some(output_name.clone());
        let _ = output_tx.send(output_name.clone());
    }
    publish_sinks(&host, sinks_tx);

    // ── Stream construction ────────────────────────────────────────────────
    let out_channels = out_cfg.channels.max(1) as usize;

    let (ring_tx, ring_rx) = rtrb::RingBuffer::<f32>::new(RING_CAPACITY_SAMPLES);

    // Stream error → flag; the supervisor sees it on the next poll and rebuilds.
    let stream_err = Arc::new(AtomicBool::new(false));

    // Raw HAL IOProc on the aggregate-tap device — bypasses cpal/AUHAL,
    // which doesn't reliably surface the tap stream and was reading silence.
    let hal_input = HalInputStream::open(tap_aggregate_id, ring_tx, sample_rate as f64)
        .with_context(|| "open HAL input on tap aggregate")?;
    // Publish the capture rate so `status` can show whether the IOProc is
    // resampling (tap rate ≠ output rate).
    shared
        .lock()
        .unwrap()
        .meters
        .set_capture_rate(hal_input.capture_rate);
    let input_callbacks = Arc::clone(&hal_input.callback_count);
    let input_nonzero = Arc::clone(&hal_input.nonzero_blocks);

    let output_stream = build_output_stream(
        &output_dev,
        &out_cfg,
        out_channels,
        ring_rx,
        Arc::clone(&shared),
        Arc::clone(&stream_err),
    )
    .with_context(|| "build output stream")?;
    output_stream.play().with_context(|| "play output stream")?;

    info!(
        "CoreAudio ready — {sample_rate} Hz, HAL tap input → DSP ({CHANNELS} ch) → \
         output {out_channels} ch"
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
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn build_output_stream(
    device: &Device,
    cfg: &StreamConfig,
    channels: usize,
    mut ring_rx: rtrb::Consumer<f32>,
    shared: Arc<Mutex<SharedRt>>,
    err_flag: Arc<AtomicBool>,
) -> Result<cpal::Stream> {
    let err_cb = {
        let e = Arc::clone(&err_flag);
        move |err| {
            warn!("CoreAudio output stream error: {err}");
            e.store(true, Ordering::Relaxed);
        }
    };
    // Pre-allocate a per-callback stereo scratch buffer (avoids alloc on the
    // audio thread). Sized for one reasonable callback; grown if needed.
    let mut stereo: Vec<f32> = Vec::with_capacity(8192 * 2);

    let data_cb = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
        let frames = data.len() / channels.max(1);
        let needed_stereo = frames * 2;
        if stereo.len() < needed_stereo {
            stereo.resize(needed_stereo, 0.0);
        }
        let buf = &mut stereo[..needed_stereo];

        // Pull `frames` stereo samples from the input ring. If short, zero-pad
        // — better silence than xrun chaos.
        let avail_samples = ring_rx.slots();
        let avail_frames = avail_samples / 2;
        let copy_frames = frames.min(avail_frames);
        // Optional backlog drain: if more than `frames + slack` is queued,
        // discard the excess so we don't grow steady-state latency.
        let backlog_frames = avail_frames.saturating_sub(frames);
        let drop_frames = backlog_frames.saturating_sub(DRAIN_SLACK_FRAMES);

        for _ in 0..drop_frames * 2 {
            let _ = ring_rx.pop();
        }

        for i in 0..copy_frames {
            buf[i * 2] = ring_rx.pop().unwrap_or(0.0);
            buf[i * 2 + 1] = ring_rx.pop().unwrap_or(0.0);
        }
        // Zero the tail if we ran short.
        for i in copy_frames..frames {
            buf[i * 2] = 0.0;
            buf[i * 2 + 1] = 0.0;
        }

        // ── DSP ───────────────────────────────────────────────────────────
        let mut s = shared.lock().unwrap();

        // Apply pending IPC commands first so latency is one block at most.
        while let Ok(cmd) = s.cmd_rx.pop() {
            apply_command(&mut s.chain, cmd);
        }
        // Publish the live DSP rate for `status` (the chain was rebound to the
        // output device rate at stream setup).
        s.meters.set_sample_rate(s.chain.sample_rate);

        let (in_peak, in_rms) = peak_rms_f32(buf);

        if s.chain.enabled {
            let need_f64 = needed_stereo;
            if s.scratch.len() < need_f64 {
                s.scratch.resize(need_f64, 0.0);
            }
            if s.routed.len() < need_f64 {
                s.routed.resize(need_f64, 0.0);
            }
            // Promote to f64, process, demote.
            for (dst, src) in s.scratch[..need_f64].iter_mut().zip(buf.iter()) {
                *dst = *src as f64;
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
                // Square output routing (L/R swap / per-channel gain) on the
                // stereo buffer — parity with the PipeWire/APO backends. `route`
                // copies for the no-matrix / identity case.
                let out: &[f64] = if chain.routing.is_some() {
                    chain.route(&scratch[..need_f64], &mut routed[..need_f64]);
                    &routed[..need_f64]
                } else {
                    &scratch[..need_f64]
                };
                let pr = peak_rms(out);
                for (dst, src) in buf.iter_mut().zip(out.iter()) {
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

        // Feed the spectrum ring from the OUTPUT (`buf`) in both branches: the
        // post-DSP signal when enabled, the passthrough signal when bypassed
        // (power off) — `buf` already holds whatever we render. Doing this
        // outside the `enabled` check keeps the analyzer tracking live audio
        // when power is off instead of freezing on the last processed frame.
        let cap = s.spectrum_tx.slots();
        let push_n = frames.min(cap);
        for i in 0..push_n {
            let m = (buf[i * 2] + buf[i * 2 + 1]) * 0.5;
            let _ = s.spectrum_tx.push(m);
        }
        drop(s);

        // ── Render to output ──────────────────────────────────────────────
        render_stereo_to_output(buf, data, channels);

        let _ = err_flag.load(Ordering::Relaxed);
    };

    let stream = device
        .build_output_stream(cfg, data_cb, err_cb, None)
        .with_context(|| "build_output_stream")?;
    Ok(stream)
}

/// Map the processed interleaved-stereo buffer (`stereo`, `frames*2`) onto the
/// output device's `channels` layout, writing exactly `data.len()` samples:
///   - 1ch  → L/R average (mono downmix)
///   - 2ch  → straight copy
///   - N>2  → stereo on ch0/ch1, the rest silent (the upmix; true surround needs
///            a real source, which the system tap doesn't provide).
/// Pure (no device/RT state) so it's unit-testable without an audio device.
fn render_stereo_to_output(stereo: &[f32], data: &mut [f32], channels: usize) {
    match channels {
        0 => {}
        1 => {
            for (i, d) in data.iter_mut().enumerate() {
                *d = 0.5 * (stereo[i * 2] + stereo[i * 2 + 1]);
            }
        }
        2 => {
            let n = data.len().min(stereo.len());
            data[..n].copy_from_slice(&stereo[..n]);
        }
        n => {
            let frames = data.len() / n;
            for f in 0..frames {
                let base = f * n;
                data[base] = stereo[f * 2];
                data[base + 1] = stereo[f * 2 + 1];
                for d in data[base + 2..base + n].iter_mut() {
                    *d = 0.0;
                }
            }
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
        let count = host.output_devices().map(|d| d.count()).unwrap_or(0);
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
    fn render_mono_downmix_averages_lr() {
        let stereo = [0.4f32, 0.6, 0.2, 0.8]; // 2 frames
        let mut data = [0.0f32; 2];
        render_stereo_to_output(&stereo, &mut data, 1);
        assert!((data[0] - 0.5).abs() < 1e-6 && (data[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn render_stereo_is_passthrough() {
        let stereo = [1.0f32, 2.0, 3.0, 4.0];
        let mut data = [0.0f32; 4];
        render_stereo_to_output(&stereo, &mut data, 2);
        assert_eq!(data, stereo);
    }

    #[test]
    fn render_upmix_places_stereo_on_front_pair_rest_silent() {
        let stereo = [1.0f32, 2.0, 3.0, 4.0]; // 2 frames
        let mut data = [9.0f32; 12]; // 2 frames * 6ch, pre-filled to catch non-writes
        render_stereo_to_output(&stereo, &mut data, 6);
        assert_eq!(data[0..6], [1.0, 2.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(data[6..12], [3.0, 4.0, 0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn render_zero_channels_is_noop() {
        let stereo = [1.0f32, 2.0];
        let mut data: [f32; 0] = [];
        render_stereo_to_output(&stereo, &mut data, 0); // must not panic
    }
}
