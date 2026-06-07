//! WASAPI backend (via cpal loopback capture) — Windows.
//!
//! Architecture:
//!   1. A capture device is opened in WASAPI **loopback** mode: cpal turns any
//!      render (output) endpoint into an input stream by setting
//!      `AUDCLNT_STREAMFLAGS_LOOPBACK`, so we receive whatever is mixed to that
//!      endpoint. Captured frames → SPSC ring buffer (interleaved stereo f32).
//!   2. A render device → cpal output stream → pops from the ring, runs the DSP
//!      chain, writes processed samples back.
//!   3. Both negotiate the endpoint's shared-mode mix format (typically 32-bit
//!      float); the DSP chain is re-bound to the render rate.
//!   4. A supervisor thread holds the streams alive, polls the device list and
//!      reports it, watches `route_rx` for preferred-render changes (rebuilds on
//!      change), and reconnects with exponential backoff capped at 5 s — same
//!      shape as the PipeWire and CoreAudio backends.
//!
//! ## The loopback feedback caveat
//!
//! Unlike the macOS Process Tap (which is created `MutedWhenTapped`, silencing
//! the original path), WASAPI loopback does **not** mute the endpoint it taps.
//! So if the capture endpoint and the render endpoint are the *same* device,
//! you get both the original (unprocessed) audio *and* our processed render at
//! once — and our render is itself captured on the next pass, i.e. feedback.
//!
//! The clean, double-audio-free setup on Windows is a **virtual cable**
//! (e.g. VB-Audio "VB-CABLE"): set the cable as the Windows default playback
//! device so apps render into it, then let the daemon loopback-capture the
//! cable and render to the real speakers. This backend auto-detects a VB-Audio
//! cable and prefers it as the capture source; otherwise it falls back to
//! loopback-capturing the default render endpoint and logs a loud warning.
//!
//! Overrides:
//!   - `RESONANCE_WIN_CAPTURE` — substring match to pick the capture endpoint.
//!   - `route_rx` / preferred-output — picks the render endpoint (as elsewhere).

use super::{CHANNELS, apply_command};
use crate::meters::{AtomicMeters, Sample, peak_rms, peak_rms_f32};
use crate::state::AudioCommand;
use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Host, SampleFormat, StreamConfig};
use resonance_dsp::chain::ProcessorChain;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc as std_mpsc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Ring-buffer capacity (interleaved stereo f32 samples) between the loopback
/// capture stream and the render stream. ~340 ms at 48 kHz stereo — generous
/// jitter headroom without painful latency.
const RING_CAPACITY_SAMPLES: usize = 32_768;

/// Target ring fill (stereo frames) the resampler holds — ~43 ms at 48 kHz.
/// Big enough to ride out scheduling jitter, small enough to keep latency low.
const TARGET_FILL_FRAMES: usize = 2048;

/// EMA smoothing factor for the measured ring fill, used only to decide the
/// rare drift slip below. Small = heavy smoothing (ignores per-block jitter).
const FILL_EMA_ALPHA: f64 = 0.02;

/// Drift-correction hysteresis (stereo frames). When the smoothed ring fill
/// strays this far from the target, we slip a single frame (drop or repeat) to
/// pull it back. Large so slips are rare (seconds to minutes apart at ppm clock
/// drift) — one held/dropped sample is inaudible, and crucially we do NOT
/// continuously interpolate, which would lowpass/colour the signal.
const SLIP_HYSTERESIS_FRAMES: f64 = 512.0;

/// Substring of a VB-Audio virtual cable's *render* endpoint name. When such a
/// device is the default playback target, apps mix into it and we can
/// loopback-capture it without double-audio. Matched case-insensitively.
const VB_CABLE_HINTS: &[&str] = &[
    "cable",
    "vb-audio",
    "vb-cable",
    "voicemeeter",
    // Our own renamed playback endpoint (see win_devices::rename_cable_render).
    "resonance eq",
];

#[allow(clippy::too_many_arguments)]
pub fn spawn(
    cmd_rx: rtrb::Consumer<AudioCommand>,
    spectrum_tx: rtrb::Producer<f32>,
    initial_chain: ProcessorChain,
    output_tx: tokio::sync::mpsc::UnboundedSender<String>,
    route_rx: std_mpsc::Receiver<String>,
    sinks_tx: tokio::sync::mpsc::UnboundedSender<Vec<(String, String)>>,
    meters: Arc<AtomicMeters>,
) -> Result<JoinHandle<()>> {
    // Shared chain + RT state. The output callback locks for the duration of
    // each block — bounded and brief. Reconnect rebuilds streams but preserves
    // chain state (history, coefficients, command queue).
    let shared = Arc::new(Mutex::new(SharedRt {
        chain: initial_chain,
        cmd_rx,
        spectrum_tx,
        scratch: Vec::with_capacity(8192 * CHANNELS),
        meters: Arc::clone(&meters),
    }));

    Ok(thread::Builder::new()
        .name("resonance-wasapi".into())
        .spawn(move || {
            let mut preferred_output: Option<String> = None;
            let mut last_active_output: Option<String> = None;
            let mut backoff = Duration::from_millis(200);

            loop {
                while let Ok(name) = route_rx.try_recv() {
                    preferred_output = Some(name);
                }

                let started = Instant::now();
                let pref_snapshot = preferred_output.clone();
                match run_streams(
                    Arc::clone(&shared),
                    pref_snapshot.as_deref(),
                    &output_tx,
                    &sinks_tx,
                    &mut last_active_output,
                    &route_rx,
                ) {
                    Ok(StreamExit::RouteChanged(new_pref)) => {
                        preferred_output = Some(new_pref);
                        backoff = Duration::from_millis(50);
                    }
                    Ok(StreamExit::Ended) => {
                        warn!("WASAPI: stream ended; reconnecting…");
                    }
                    Err(e) => warn!("WASAPI: setup failed: {e:#}; retrying…"),
                }

                if started.elapsed() > Duration::from_secs(10) {
                    backoff = Duration::from_millis(200);
                }
                thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        })?)
}

/// Shared state between supervisor and the cpal output callback. Guarded by a
/// mutex because cpal callbacks run on their own audio thread; we only contend
/// during reconfiguration — per-block work happens inside the callback while it
/// owns the lock.
struct SharedRt {
    chain: ProcessorChain,
    cmd_rx: rtrb::Consumer<AudioCommand>,
    spectrum_tx: rtrb::Producer<f32>,
    /// Reusable interleaved f64 scratch buffer to avoid per-callback allocation.
    scratch: Vec<f64>,
    meters: Arc<AtomicMeters>,
}

enum StreamExit {
    /// The supervisor saw a `route_rx` update; rebuild against the new render
    /// device (returned in the variant).
    RouteChanged(String),
    /// Streams exited (device unplugged / error) — caller reconnects.
    Ended,
}

fn run_streams(
    shared: Arc<Mutex<SharedRt>>,
    preferred: Option<&str>,
    output_tx: &tokio::sync::mpsc::UnboundedSender<String>,
    sinks_tx: &tokio::sync::mpsc::UnboundedSender<Vec<(String, String)>>,
    last_active_output: &mut Option<String>,
    route_rx: &std_mpsc::Receiver<String>,
) -> Result<StreamExit> {
    let host = cpal::default_host();
    let (capture_dev, capture_loopback) = pick_capture_device(&host)?;
    let capture_name = device_name(&capture_dev);

    // Render selection uses unique MMDevice friendly names (cpal's endpoint
    // descriptions are ambiguous — both real speakers and the cable are
    // "Speakers"). Pick a non-cable endpoint so we never render into the cable
    // we're capturing (which would feed back).
    let (outputs, out_names) = enumerate_outputs(&host);
    let render_idx = pick_render_idx(&out_names, preferred)
        .ok_or_else(|| anyhow!("no render endpoint available"))?;
    let render_dev = outputs[render_idx].clone();
    let render_name = out_names[render_idx].clone();

    if capture_loopback && is_cable(&render_name) {
        warn!(
            "WASAPI: no real (non-cable) render endpoint found; rendering into '{render_name}' \
             will feed back. Connect real speakers/headphones, or set them as an available \
             output."
        );
    } else if capture_loopback {
        info!(
            "WASAPI: loopback-capturing '{capture_name}' → DSP → render '{render_name}' \
             (no virtual cable detected — for clean routing install VB-CABLE, set it as the \
             default playback device, and route the source app into it)"
        );
    } else {
        info!("WASAPI: capturing '{capture_name}' → DSP → render '{render_name}'");
    }

    // ── Render config (drives the DSP rate) ────────────────────────────────
    let out_default = render_dev
        .default_output_config()
        .with_context(|| "default_output_config")?;
    if out_default.sample_format() != SampleFormat::F32 {
        return Err(anyhow!(
            "render device '{render_name}' shared-mode format is {:?}, only F32 is supported",
            out_default.sample_format()
        ));
    }
    // Pin the ENTIRE chain (real output + cable + our streams) to one rate so
    // nothing resamples. Must equal VB-CABLE's NATIVE internal rate (44100) —
    // its WASAPI shared format can be set higher, but its actual stream runs at
    // 44.1k (dshow confirms), so a 48k shared format just makes Windows resample
    // at the cable boundary and roll off the highs. Match the cable's real rate
    // everywhere, including the physical output, so OG and through-us are equal.
    const CHAIN_RATE: u32 = 48_000;
    super::win_devices::set_endpoint_rate(&render_name, CHAIN_RATE);
    let sample_rate = CHAIN_RATE;
    let out_cfg = StreamConfig {
        channels: out_default.channels(),
        sample_rate,
        buffer_size: cpal::BufferSize::Default,
    };

    {
        // Re-bind DSP coefficients to the render device's sample rate.
        let mut s = shared.lock().unwrap();
        if s.chain.sample_rate as u32 != sample_rate {
            s.chain.sample_rate = sample_rate as f64;
            let sr = s.chain.sample_rate;
            for f in s.chain.filters.iter_mut() {
                let _ = f.update(f.filter_type, f.freq, f.gain_db, f.q, sr);
            }
        }
    }

    if last_active_output.as_deref() != Some(render_name.as_str()) {
        *last_active_output = Some(render_name.clone());
        let _ = output_tx.send(render_name.clone());
    }
    publish_sinks(&host, sinks_tx);

    // Auto-match the virtual cable's rate to the real output's rate so VB-CABLE
    // doesn't internally resample (which rolls off the highs). Mirrors FxSound's
    // device-format matching. No-op on non-cable captures.
    if !capture_loopback {
        super::win_devices::match_cable_endpoints_to(sample_rate);
    }

    // Capture endpoint's actual shared-mix rate (same reasoning as render: don't
    // trust cpal's default config). The resampler uses the known capture:render
    // ratio as its base.
    let capture_cfg = if capture_loopback {
        capture_dev.default_output_config()
    } else {
        capture_dev.default_input_config()
    }
    .with_context(|| "capture device config")?;
    // Cable was just matched to CHAIN_RATE (= sample_rate), so capture runs at
    // the same rate → base_ratio is exactly 1.0 (bit-exact, no resampling).
    let capture_rate = sample_rate;
    let render_rate = sample_rate as f64;

    info!(
        "WASAPI formats: capture {:?} @ {capture_rate} Hz ({} ch), render {:?} @ {sample_rate} Hz ({} ch)",
        capture_cfg.sample_format(),
        capture_cfg.channels(),
        out_default.sample_format(),
        out_default.channels(),
    );

    // ── Stream construction ────────────────────────────────────────────────
    let out_channels = out_cfg.channels.max(1) as usize;
    let (ring_tx, ring_rx) = rtrb::RingBuffer::<f32>::new(RING_CAPACITY_SAMPLES);
    let stream_err = Arc::new(AtomicBool::new(false));

    let input_stream = build_capture_input(
        &capture_dev,
        capture_loopback,
        capture_rate,
        ring_tx,
        Arc::clone(&stream_err),
    )
    .with_context(|| "build capture input stream")?;
    input_stream
        .play()
        .with_context(|| "play capture input stream")?;

    let output_stream = build_output_stream(
        &render_dev,
        &out_cfg,
        out_channels,
        capture_rate as f64,
        render_rate,
        ring_rx,
        Arc::clone(&shared),
        Arc::clone(&stream_err),
    )
    .with_context(|| "build output stream")?;
    output_stream.play().with_context(|| "play output stream")?;

    info!(
        "WASAPI ready — capture {capture_rate} Hz → render {sample_rate} Hz, DSP ({CHANNELS} ch) → \
         render {out_channels} ch"
    );

    // ── Supervisor loop ─────────────────────────────────────────────────────
    let mut sink_poll = Instant::now();
    let _keep_input_alive = &input_stream; // hold the stream until function exits
    loop {
        if stream_err.load(Ordering::Relaxed) {
            return Ok(StreamExit::Ended);
        }
        if let Ok(name) = route_rx.try_recv() {
            return Ok(StreamExit::RouteChanged(name));
        }
        if sink_poll.elapsed() >= Duration::from_millis(750) {
            sink_poll = Instant::now();
            publish_sinks(&host, sinks_tx);
            // Follow render-endpoint changes (device added/removed). Re-run the
            // same selection over the current endpoints; if the chosen render's
            // friendly name changed, rebuild. (Comparing friendly names, not
            // cpal's ambiguous descriptions, avoids spurious rebuilds.)
            if preferred.is_none() {
                let (_, names) = enumerate_outputs(&host);
                let now = pick_render_idx(&names, None).and_then(|i| names.get(i).cloned());
                if let Some(now) = now {
                    if now != render_name {
                        info!("WASAPI: render endpoint changed → {now}; rebuilding");
                        return Ok(StreamExit::Ended);
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Build a WASAPI capture stream feeding `ring_tx` (downmixed to stereo).
///
/// Two modes:
///   - `loopback = true`: `device` is a *render* endpoint; cpal sets the WASAPI
///     loopback flag transparently when `build_input_stream` is called on an
///     output device, so we receive whatever is mixed to it.
///   - `loopback = false`: `device` is a real *capture* endpoint (a virtual
///     cable's output, a mic); a normal input stream.
///
/// The config differs: loopback mirrors the render endpoint's output mix format,
/// a real capture endpoint reports an input config.
fn build_capture_input(
    device: &Device,
    loopback: bool,
    rate: u32,
    ring_tx: rtrb::Producer<f32>,
    err_flag: Arc<AtomicBool>,
) -> Result<cpal::Stream> {
    let cfg = if loopback {
        device
            .default_output_config()
            .with_context(|| "default_output_config for loopback")?
    } else {
        device
            .default_input_config()
            .with_context(|| "default_input_config for capture")?
    };
    let in_channels = cfg.channels().max(1) as usize;
    // Open at the endpoint's true shared-mix rate (passed in), not cpal's
    // default-config rate, so WASAPI doesn't resample the capture.
    let stream_cfg = StreamConfig {
        channels: cfg.channels(),
        sample_rate: rate,
        buffer_size: cpal::BufferSize::Default,
    };

    let err_cb = {
        let e = Arc::clone(&err_flag);
        move |err| {
            warn!("WASAPI loopback input error: {err}");
            e.store(true, Ordering::Relaxed);
        }
    };

    // Downmix arbitrary channel layout → stereo, push interleaved into the ring.
    // Dropped samples (ring full) are acceptable: the render side zero-pads on
    // underrun, so transient overruns degrade to brief glitches, not crashes.
    let stream = match cfg.sample_format() {
        SampleFormat::F32 => {
            let mut tx = ring_tx;
            device.build_input_stream(
                &stream_cfg,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    for frame in data.chunks(in_channels) {
                        let l = frame.first().copied().unwrap_or(0.0);
                        let r = if in_channels >= 2 { frame[1] } else { l };
                        let _ = tx.push(l);
                        let _ = tx.push(r);
                    }
                },
                err_cb,
                None,
            )
        }
        SampleFormat::I16 => {
            let mut tx = ring_tx;
            device.build_input_stream(
                &stream_cfg,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    const INV: f32 = 1.0 / 32768.0;
                    for frame in data.chunks(in_channels) {
                        let l = frame.first().copied().unwrap_or(0) as f32 * INV;
                        let r = if in_channels >= 2 {
                            frame[1] as f32 * INV
                        } else {
                            l
                        };
                        let _ = tx.push(l);
                        let _ = tx.push(r);
                    }
                },
                err_cb,
                None,
            )
        }
        other => return Err(anyhow!("unsupported loopback sample format {other:?}")),
    }
    .with_context(|| "build_input_stream")?;
    Ok(stream)
}

#[allow(clippy::too_many_arguments)]
fn build_output_stream(
    device: &Device,
    cfg: &StreamConfig,
    channels: usize,
    capture_rate: f64,
    render_rate: f64,
    mut ring_rx: rtrb::Consumer<f32>,
    shared: Arc<Mutex<SharedRt>>,
    err_flag: Arc<AtomicBool>,
) -> Result<cpal::Stream> {
    let err_cb = {
        let e = Arc::clone(&err_flag);
        move |err| {
            warn!("WASAPI output stream error: {err}");
            e.store(true, Ordering::Relaxed);
        }
    };
    // Pre-allocated per-callback stereo scratch (avoids alloc on the RT thread).
    let mut stereo: Vec<f32> = Vec::with_capacity(8192 * 2);

    // Resampler state. The BASE ratio is the known capture:render clock ratio —
    // reading this many input samples per output sample preserves pitch exactly.
    // A tiny, smoothed, clamped trim around it absorbs slow clock drift without
    // any audible pitch movement.
    let base_ratio = if render_rate > 0.0 {
        capture_rate / render_rate
    } else {
        1.0
    };
    let mut frac = 0.0f64; // fractional input-frame accumulator
    let mut fill_avg = 0.0f64; // EMA of ring fill (frames)
    let mut primed = false; // wait for the ring to pre-fill before consuming
    let (mut sc_l, mut sc_r) = (0.0f32, 0.0f32); // current (most recently popped) sample

    let data_cb = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
        let frames = data.len() / channels.max(1);
        let needed_stereo = frames * 2;
        if stereo.len() < needed_stereo {
            stereo.resize(needed_stereo, 0.0);
        }
        let buf = &mut stereo[..needed_stereo];

        // ── Consume the capture ring ────────────────────────────────────────
        // Step through the ring at the KNOWN base ratio (= capture/render rate)
        // and take the nearest sample — no interpolation, so at equal rates
        // (base_ratio == 1.0) this is a bit-exact 1:1 copy with no colouration.
        // Slow clock drift is corrected by a rare single-frame slip, not by
        // continuously bending the rate (which would lowpass the signal).
        let avail_frames = (ring_rx.slots() / 2) as f64;
        fill_avg += FILL_EMA_ALPHA * (avail_frames - fill_avg);

        // Pre-fill: output silence until the ring holds a target buffer, so we
        // don't underrun at startup.
        if !primed {
            if avail_frames >= TARGET_FILL_FRAMES as f64 {
                primed = true;
            } else {
                buf.fill(0.0);
            }
        }

        if primed {
            // Discrete drift slip: at most one frame per callback. Drop a frame
            // when the buffer is too full, repeat one when too empty.
            let drift = fill_avg - TARGET_FILL_FRAMES as f64;
            if drift > SLIP_HYSTERESIS_FRAMES {
                frac += 1.0; // consume one extra input frame (drop)
            } else if drift < -SLIP_HYSTERESIS_FRAMES {
                frac -= 1.0; // consume one fewer (repeat a sample)
            }

            for i in 0..frames {
                frac += base_ratio;
                while frac >= 1.0 {
                    match (ring_rx.pop(), ring_rx.pop()) {
                        (Ok(l), Ok(r)) => {
                            sc_l = l;
                            sc_r = r;
                        }
                        // Underrun: hold the last sample (less audible than a
                        // zero-insert click).
                        _ => {}
                    }
                    frac -= 1.0;
                }
                buf[i * 2] = sc_l;
                buf[i * 2 + 1] = sc_r;
            }
        }

        // ── DSP ───────────────────────────────────────────────────────────
        let mut s = shared.lock().unwrap();
        while let Ok(cmd) = s.cmd_rx.pop() {
            apply_command(&mut s.chain, cmd);
        }

        let (in_peak, in_rms) = peak_rms_f32(buf);

        if s.chain.enabled {
            let need_f64 = needed_stereo;
            if s.scratch.len() < need_f64 {
                s.scratch.resize(need_f64, 0.0);
            }
            for (dst, src) in s.scratch[..need_f64].iter_mut().zip(buf.iter()) {
                *dst = *src as f64;
            }
            let t0 = Instant::now();
            let SharedRt { chain, scratch, .. } = &mut *s;
            let scratch_slice = &mut scratch[..need_f64];
            chain.process(scratch_slice);
            let dt = t0.elapsed();
            let (out_peak, out_rms) = peak_rms(scratch_slice);
            for (dst, src) in buf.iter_mut().zip(s.scratch[..need_f64].iter()) {
                *dst = *src as f32;
            }

            // Spectrum: mono mix of the post-DSP signal.
            let cap = s.spectrum_tx.slots();
            let push_n = frames.min(cap);
            for i in 0..push_n {
                let m = (buf[i * 2] + buf[i * 2 + 1]) * 0.5;
                let _ = s.spectrum_tx.push(m);
            }

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
        drop(s);

        // ── Render: stereo → output channel count ──────────────────────────
        match channels {
            0 => {}
            1 => {
                for i in 0..frames {
                    data[i] = 0.5 * (buf[i * 2] + buf[i * 2 + 1]);
                }
            }
            2 => {
                data[..needed_stereo].copy_from_slice(buf);
            }
            n => {
                for i in 0..frames {
                    let base = i * n;
                    data[base] = buf[i * 2];
                    data[base + 1] = buf[i * 2 + 1];
                    for ch in 2..n {
                        data[base + ch] = 0.0;
                    }
                }
            }
        }

        let _ = err_flag.load(Ordering::Relaxed);
    };

    let stream = device
        .build_output_stream(cfg, data_cb, err_cb, None)
        .with_context(|| "build_output_stream")?;
    Ok(stream)
}

/// Whether a friendly device name belongs to a virtual cable (which must never
/// be a render target while we're capturing it — that would feed back).
fn is_cable(name: &str) -> bool {
    let n = name.to_lowercase();
    VB_CABLE_HINTS.iter().any(|h| n.contains(h))
}

/// Enumerate render endpoints as `(cpal Device, friendly name)`, aligned by
/// index. The friendly name comes from the Windows MMDevice API (unique, e.g.
/// "Speakers (VB-Audio Virtual Cable)"); cpal's own description is ambiguous, so
/// it's only a fallback when the MMDevice lookup is unavailable/short.
fn enumerate_outputs(host: &Host) -> (Vec<Device>, Vec<String>) {
    let outputs: Vec<Device> = host
        .output_devices()
        .map(|it| it.collect())
        .unwrap_or_default();
    let friendly = super::win_devices::render_friendly_names();
    let names = (0..outputs.len())
        .map(|i| {
            friendly
                .get(i)
                .filter(|s| !s.is_empty())
                .cloned()
                .unwrap_or_else(|| device_name(&outputs[i]))
        })
        .collect();
    (outputs, names)
}

/// Choose the render endpoint index from the friendly names. Priority:
///   1. The user's preferred output (exact friendly-name match).
///   2. The first non-cable endpoint (the real speakers/headphones).
///   3. Index 0 as a last resort.
fn pick_render_idx(names: &[String], preferred: Option<&str>) -> Option<usize> {
    if names.is_empty() {
        return None;
    }
    if let Some(p) = preferred {
        if let Some(i) = names.iter().position(|n| n == p) {
            return Some(i);
        }
        info!("preferred output '{p}' not found — falling back to first real endpoint");
    }
    names.iter().position(|n| !is_cable(n)).or(Some(0))
}

/// Pick the capture endpoint. Returns `(device, is_loopback)`. Priority:
///   1. `RESONANCE_WIN_CAPTURE` substring match — first against real capture
///      (input) endpoints, then against render endpoints (loopback).
///   2. A virtual cable's *capture* endpoint (e.g. "CABLE Output") — a clean,
///      non-loopback recording of whatever apps play into the cable.
///   3. A virtual cable's *render* endpoint (e.g. "CABLE Input") via loopback.
///   4. The system default render endpoint via loopback (feedback-prone; the
///      caller logs a warning when it coincides with the render target).
fn pick_capture_device(host: &Host) -> Result<(Device, bool)> {
    let inputs: Vec<Device> = host
        .input_devices()
        .map(|it| it.collect())
        .unwrap_or_default();
    let outputs: Vec<Device> = host
        .output_devices()
        .map(|it| it.collect())
        .unwrap_or_default();

    if let Ok(want) = std::env::var("RESONANCE_WIN_CAPTURE") {
        let want = want.trim().to_lowercase();
        if !want.is_empty() {
            if let Some(d) = inputs
                .iter()
                .find(|d| device_name(d).to_lowercase().contains(&want))
            {
                return Ok((d.clone(), false));
            }
            if let Some(d) = outputs
                .iter()
                .find(|d| device_name(d).to_lowercase().contains(&want))
            {
                return Ok((d.clone(), true));
            }
            warn!("RESONANCE_WIN_CAPTURE='{want}' matched no endpoint — falling back");
        }
    }

    // A virtual cable's capture endpoint is the cleanest source: a normal
    // recording of whatever apps render into the cable, no loopback feedback.
    if let Some(d) = inputs.iter().find(|d| {
        let n = device_name(d).to_lowercase();
        VB_CABLE_HINTS.iter().any(|h| n.contains(h))
    }) {
        info!("WASAPI: capturing virtual cable '{}'", device_name(d));
        return Ok((d.clone(), false));
    }

    // Otherwise loopback the cable's render endpoint if present.
    if let Some(d) = outputs.iter().find(|d| {
        let n = device_name(d).to_lowercase();
        VB_CABLE_HINTS.iter().any(|h| n.contains(h))
    }) {
        info!(
            "WASAPI: loopback-capturing virtual cable '{}'",
            device_name(d)
        );
        return Ok((d.clone(), true));
    }

    let default = host
        .default_output_device()
        .ok_or_else(|| anyhow!("no default output device to loopback-capture"))?;
    Ok((default, true))
}

/// Measurement helper: WASAPI loopback-capture the output endpoint whose
/// friendly name contains `dev_substr`, writing raw interleaved f32le to
/// `out_path` for `secs` seconds. Used by `resonanced --measure-loopback` to
/// capture the true end-of-chain signal (what actually reaches the speakers)
/// for objective spectral comparison. Prints the captured rate/channels.
pub fn measure_loopback(dev_substr: &str, out_path: &str, secs: u64) -> Result<()> {
    use std::io::Write;
    let host = cpal::default_host();
    let (outs, names) = enumerate_outputs(&host);
    let want = dev_substr.to_lowercase();
    let idx = names
        .iter()
        .position(|n| n.to_lowercase().contains(&want))
        .ok_or_else(|| anyhow!("no output endpoint matching '{dev_substr}'"))?;
    let dev = &outs[idx];
    let cfg = dev
        .default_output_config()
        .with_context(|| "default_output_config")?;
    let rate = cfg.sample_rate();
    let ch = cfg.channels() as usize;
    eprintln!(
        "measure: device='{}' rate={rate} ch={ch} fmt={:?}",
        names[idx],
        cfg.sample_format()
    );
    let stream_cfg = StreamConfig {
        channels: cfg.channels(),
        sample_rate: rate,
        buffer_size: cpal::BufferSize::Default,
    };
    let file = Arc::new(Mutex::new(std::io::BufWriter::new(std::fs::File::create(
        out_path,
    )?)));
    let err = move |e| eprintln!("measure stream error: {e}");
    let stream = match cfg.sample_format() {
        SampleFormat::F32 => {
            let f = Arc::clone(&file);
            dev.build_input_stream(
                &stream_cfg,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let mut w = f.lock().unwrap();
                    for s in data {
                        let _ = w.write_all(&s.to_le_bytes());
                    }
                },
                err,
                None,
            )
        }
        SampleFormat::I16 => {
            let f = Arc::clone(&file);
            dev.build_input_stream(
                &stream_cfg,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let mut w = f.lock().unwrap();
                    for s in data {
                        let _ = w.write_all(&(*s as f32 / 32768.0).to_le_bytes());
                    }
                },
                err,
                None,
            )
        }
        other => return Err(anyhow!("unsupported sample format {other:?}")),
    }
    .with_context(|| "build loopback input")?;
    stream.play().with_context(|| "play loopback input")?;
    std::thread::sleep(Duration::from_secs(secs));
    drop(stream);
    file.lock().unwrap().flush().ok();
    eprintln!("measure: wrote {out_path} (f32le, {ch} ch, {rate} Hz)");
    Ok(())
}

/// Best-effort human-readable cpal device name.
fn device_name(d: &Device) -> String {
    match d.description() {
        Ok(desc) => desc.name().to_string(),
        Err(_) => "(unknown)".to_string(),
    }
}

/// Enumerate render endpoints and publish `(name, description)` pairs. cpal has
/// no friendly per-device description, so the name is duplicated into both slots.
/// Virtual-cable endpoints are hidden — they're a capture source, not a render
/// target a user should pick.
fn publish_sinks(
    host: &Host,
    sinks_tx: &tokio::sync::mpsc::UnboundedSender<Vec<(String, String)>>,
) {
    let (_devs, names) = enumerate_outputs(host);
    let mut out: Vec<(String, String)> = names
        .into_iter()
        .filter(|n| !is_cable(n))
        .map(|n| (n.clone(), n))
        .collect();
    out.sort();
    out.dedup_by(|a, b| a.0 == b.0);
    let _ = sinks_tx.send(out);
}

#[cfg(test)]
mod tests {
    use super::*;

    // Compile-time invariants — the audio thread depends on these holding.
    const _RING_SIZE_OK: () = assert!(
        RING_CAPACITY_SAMPLES >= 9_600 * 2,
        "ring buffer must hold ≥100 ms of stereo 48 kHz audio"
    );
    // The resampler's target fill (plus drift headroom) must fit the ring.
    const _TARGET_FILL_OK: () = assert!(
        TARGET_FILL_FRAMES * 2 < RING_CAPACITY_SAMPLES,
        "TARGET_FILL_FRAMES must leave room in the ring"
    );

    #[test]
    fn pick_render_prefers_non_cable_then_falls_back() {
        let names = vec![
            "Speakers (VB-Audio Virtual Cable)".to_string(),
            "Speakers (High Definition Audio Device)".to_string(),
        ];
        // No preference → skip the cable, pick the real endpoint.
        assert_eq!(pick_render_idx(&names, None), Some(1));
        // Exact preferred match wins, even if it's the cable.
        assert_eq!(
            pick_render_idx(&names, Some("Speakers (VB-Audio Virtual Cable)")),
            Some(0)
        );
        // Unknown preference → fall back to the first non-cable endpoint.
        assert_eq!(pick_render_idx(&names, Some("nope")), Some(1));
        // All cables → last-resort index 0.
        assert_eq!(
            pick_render_idx(&["CABLE Output".to_string()], None),
            Some(0)
        );
        // No endpoints → None.
        assert_eq!(pick_render_idx(&[], None), None);
    }
}
