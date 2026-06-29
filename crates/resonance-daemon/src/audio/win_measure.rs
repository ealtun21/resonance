//! Windows loopback **measurement** helper (diagnostic only).
//!
//! Windows does its DSP in the in-engine APO (`resonance-apo`, loaded into
//! `audiodg.exe`) — the daemon runs no audio backend there (see `audio::mod`).
//! This module is **not** an audio path; it's the `resonanced --measure-loopback`
//! diagnostic that WASAPI loopback-captures whatever actually reaches an output
//! endpoint and writes it to a raw file, so the APO's end-of-chain effect can be
//! compared objectively against a reference.

use super::win_devices;
use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Host, SampleFormat, StreamConfig};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Best-effort human-readable cpal device name.
fn device_name(d: &Device) -> String {
    match d.description() {
        Ok(desc) => desc.name().to_string(),
        Err(_) => "(unknown)".to_string(),
    }
}

/// Enumerate render endpoints as `(cpal Device, friendly name)`, aligned by
/// index. The friendly name comes from the Windows `MMDevice` API (unique, e.g.
/// "Speakers (VB-Audio Virtual Cable)"); cpal's own description is ambiguous, so
/// it's only a fallback when the `MMDevice` lookup is unavailable/short.
fn enumerate_outputs(host: &Host) -> (Vec<Device>, Vec<String>) {
    let outputs: Vec<Device> = host
        .output_devices()
        .map(|it| it.collect())
        .unwrap_or_default();
    let friendly = win_devices::render_friendly_names();
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

/// WASAPI loopback-capture the output endpoint whose friendly name contains
/// `dev_substr`, writing raw interleaved f32le to `out_path` for `secs` seconds.
/// Used by `resonanced --measure-loopback` to capture the true end-of-chain
/// signal (what actually reaches the speakers) for objective spectral
/// comparison. Prints the captured rate/channels.
pub fn measure_loopback(dev_substr: &str, out_path: &str, secs: u64) -> Result<()> {
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
                        let _ = w.write_all(&(f32::from(*s) / 32768.0).to_le_bytes());
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
