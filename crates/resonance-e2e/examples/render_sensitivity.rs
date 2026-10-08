//! Diagnostic: how much does the render's output level move when the input is scaled by
//! `<scale_db>`? (A nonlinear chain, e.g. with Dynamic Boost, amplifies small input gain errors.)
//! usage: render_sensitivity <chain.bin> <rate> <channels> <scale_db> [render-out.wav]
#![allow(clippy::doc_markdown)]
use resonance_e2e::render::{BLOCK_FRAMES, load_exported_chain, render};
use resonance_e2e::stimulus::generate;

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (rate, ch): (u32, usize) = (a[2].parse().unwrap(), a[3].parse().unwrap());
    let k = 10f32.powf(a[4].parse::<f32>().unwrap() / 20.0);
    let stim = generate(rate, ch, 5.0);
    let run = |scale: f32| {
        let (mut c, _) =
            load_exported_chain(std::path::Path::new(&a[1]), ch, f64::from(rate)).unwrap();
        let input: Vec<f32> = stim.samples.iter().map(|v| v * scale).collect();
        render(&mut c, &input, BLOCK_FRAMES)
    };
    let (o1, o2) = (run(1.0), run(k));
    if let Some(path) = a.get(5) {
        let spec = hound::WavSpec {
            channels: ch as u16,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for v in &o1 {
            w.write_sample(*v).unwrap();
        }
        w.finalize().unwrap();
    }
    println!(
        "output rms ratio {:+.3} dB for input {:+} dB",
        20.0 * (rms(&o2) / rms(&o1)).log10(),
        a[4]
    );
}
