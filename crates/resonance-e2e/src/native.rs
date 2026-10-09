//! Play a stimulus and record a device with cpal, on one process clock.
//! Windows records the playback endpoint itself (WASAPI loopback, so the audio
//! engine and APO are in the path); macOS records an input device (BlackHole).

use crate::common::Recording;
use anyhow::{Context, Result, bail, ensure};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Device, SampleFormat, StreamConfig};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

/// Recording starts this long before playback, so the first stimulus frame is
/// never missed; the compare step searches the alignment.
const PRE_ROLL: Duration = Duration::from_millis(300);

/// Stream config of `dev` at the scenario `rate`; the device may be wider than
/// `channels` (a 64-channel BlackHole recording a 2-channel scenario). Returns
/// the config and the device's channel count.
fn f32_config(
    dev: &Device,
    channels: usize,
    rate: u32,
    input: bool,
) -> Result<(StreamConfig, usize)> {
    let cfg = if input {
        dev.default_input_config()
    } else {
        dev.default_output_config()
    }
    .context("default stream config")?;
    ensure!(
        cfg.sample_format() == SampleFormat::F32,
        "device format is {:?}, want F32",
        cfg.sample_format()
    );
    let width = usize::from(cfg.channels());
    ensure!(
        width >= channels && cfg.sample_rate() == rate,
        "device is {width} ch @ {} Hz, scenario wants {channels} ch @ {rate} Hz",
        cfg.sample_rate()
    );
    Ok((cfg.config(), width))
}

/// Play `samples` on `play` while recording `record` (a loopback of the same
/// endpoint on Windows). Recording continues `tail_frames` after the last
/// stimulus frame. `record_input` selects an input (macOS) vs loopback stream.
pub fn play_and_record(
    play: &Device,
    record: &Device,
    record_input: bool,
    samples: &[f32],
    channels: usize,
    rate: u32,
    tail_frames: usize,
) -> Result<Recording> {
    let (play_cfg, play_width) = f32_config(play, channels, rate, false)?;
    ensure!(
        play_width == channels,
        "playback device is {play_width} ch, scenario wants exactly {channels}"
    );
    let (rec_cfg, rec_width) = f32_config(record, channels, rate, record_input)?;
    // The capture callback must never allocate: growing a hundreds-of-MiB Vec there (the
    // realloc copy, or the page faults of a fresh 2x block) stalls the loopback reader for
    // tens of ms, the engine's loopback buffer overruns and a few ms of audio are lost (a
    // "slip": spec section 16). Reserve the whole recording, committed, before it starts.
    let n = (samples.len() / channels + tail_frames + 2 * rate as usize) * rec_width;
    let mut buf = vec![0.0f32; n];
    buf.fill(1.0); // touch every page now, not in the callback
    buf.clear();
    let rec = Arc::new(Mutex::new(buf));
    let errors = Arc::new(AtomicUsize::new(0));
    let (rec2, err2) = (Arc::clone(&rec), Arc::clone(&errors));
    let worst = Arc::new(Mutex::new((Duration::ZERO, 0usize)));
    let worst2 = Arc::clone(&worst);
    let rec_stream = record
        .build_input_stream(
            &rec_cfg,
            move |d: &[f32], _| {
                let t = Instant::now();
                let mut v = rec2.lock().expect("rec lock");
                let before = v.capacity();
                v.extend_from_slice(d);
                let grew = usize::from(v.capacity() != before);
                let mut w = worst2.lock().expect("worst lock");
                w.0 = w.0.max(t.elapsed());
                w.1 += grew;
            },
            move |e| {
                eprintln!("record stream error: {e}");
                err2.fetch_add(1, Ordering::Relaxed);
            },
            None,
        )
        .context("build record stream")?;
    rec_stream.play().context("start record stream")?;
    std::thread::sleep(PRE_ROLL);

    let pos = Arc::new(AtomicUsize::new(0));
    let (pos2, err3) = (Arc::clone(&pos), Arc::clone(&errors));
    let data = samples.to_vec();
    let play_stream = play
        .build_output_stream(
            &play_cfg,
            move |out: &mut [f32], _| {
                let p = pos2.load(Ordering::Relaxed);
                for (i, o) in out.iter_mut().enumerate() {
                    *o = data.get(p + i).copied().unwrap_or(0.0);
                }
                pos2.store(p + out.len(), Ordering::Relaxed);
            },
            move |e| {
                eprintln!("play stream error: {e}");
                err3.fetch_add(1, Ordering::Relaxed);
            },
            None,
        )
        .context("build play stream")?;
    play_stream.play().context("start play stream")?;

    let want = samples.len() + tail_frames * channels;
    let deadline = Instant::now()
        + Duration::from_secs_f64(3.0 * samples.len() as f64 / (channels as f64 * f64::from(rate)))
        + Duration::from_secs(20);
    while pos.load(Ordering::Relaxed) < want {
        if Instant::now() > deadline {
            bail!(
                "playback stalled at {} of {want} samples",
                pos.load(Ordering::Relaxed)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // The device buffer still holds the last block: let it reach the loopback.
    std::thread::sleep(Duration::from_millis(300));
    drop(play_stream);
    drop(rec_stream);
    let (slowest, grows) = *worst.lock().expect("worst lock");
    eprintln!("record: slowest capture callback {slowest:?}, {grows} buffer growths");
    let wide = std::mem::take(&mut *rec.lock().expect("rec lock"));
    let samples = if rec_width == channels {
        wide
    } else {
        wide.chunks_exact(rec_width)
            .flat_map(|f| f[..channels].iter().copied())
            .collect()
    };
    Ok(Recording {
        node: "default".into(),
        channels,
        rate,
        first_tick: None,
        samples,
        discontinuities: u32::try_from(errors.load(Ordering::Relaxed)).unwrap_or(u32::MAX),
    })
}
