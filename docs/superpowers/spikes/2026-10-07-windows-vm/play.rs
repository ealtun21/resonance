//! Spike-only: play a raw interleaved f32le file to the default output at its
//! native shared-mode config. `play <in.raw>`; exits when the file is consumed.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};

fn main() {
    let path = std::env::args().nth(1).expect("in.raw");
    let bytes = std::fs::read(&path).expect("read");
    let data: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let host = cpal::default_host();
    let dev = host.default_output_device().expect("no default output");
    let cfg = dev.default_output_config().expect("cfg");
    eprintln!("play: {cfg:?}");
    assert_eq!(cfg.sample_format(), cpal::SampleFormat::F32);
    let pos = Arc::new(Mutex::new(0usize));
    let p2 = Arc::clone(&pos);
    let stream = dev
        .build_output_stream(
            &cfg.config(),
            move |out: &mut [f32], _| {
                let mut p = p2.lock().unwrap();
                for s in out.iter_mut() {
                    *s = data.get(*p).copied().unwrap_or(0.0);
                    *p += 1;
                }
            },
            |e| eprintln!("stream error: {e}"),
            None,
        )
        .expect("build");
    stream.play().expect("play");
    let total = bytes.len() / 4;
    while *pos.lock().unwrap() < total + 48_000 * 8 {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    eprintln!("play: done");
}
