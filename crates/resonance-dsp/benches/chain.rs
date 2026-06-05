//! Throughput benchmark for the per-frame audio hot path.
//!
//! Run with `cargo bench -p resonance-dsp`. Catches DSP perf regressions before
//! they ship — the chain runs on the PipeWire RT thread, so frame time matters.

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use resonance_dsp::chain::{FxEffect, ProcessorChain};
use resonance_dsp::filter::{ApoFilter, FilterType};
use std::hint::black_box;

const SAMPLE_RATE: f64 = 48_000.0;
const CHANNELS: usize = 2;
/// One PipeWire quantum-ish block (256 frames × 2 ch).
const FRAMES: usize = 256;

/// Stereo sine sweep test buffer, interleaved.
fn test_buffer() -> Vec<f64> {
    (0..FRAMES * CHANNELS)
        .map(|i| ((i as f64) * 0.01).sin() * 0.5)
        .collect()
}

/// A 10-band EQ plus every effect engaged — a realistic "loud preset" load.
fn full_chain() -> ProcessorChain {
    let freqs = [
        31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
    ];
    let mut builder = ProcessorChain::builder()
        .channels(CHANNELS)
        .sample_rate(SAMPLE_RATE)
        .preamp_db(-3.0);
    for (i, f) in freqs.iter().enumerate() {
        let gain = if i % 2 == 0 { 3.0 } else { -2.0 };
        builder = builder.add_filter(
            ApoFilter::builder()
                .filter_type(FilterType::Peaking)
                .freq(*f)
                .gain_db(gain)
                .q(1.41)
                .enabled(true)
                .channels(CHANNELS)
                .sample_rate(SAMPLE_RATE)
                .build()
                .unwrap(),
        );
    }
    let mut chain = builder.build();
    chain.set_effect_intensity(FxEffect::Fidelity, 0.7);
    chain.set_effect_intensity(FxEffect::Ambience, 0.5);
    chain.set_effect_intensity(FxEffect::Surround, 0.6);
    chain.set_effect_intensity(FxEffect::DynamicBoost, 0.8);
    chain.set_effect_intensity(FxEffect::Bass, 0.4);
    chain
}

fn bench_chain(c: &mut Criterion) {
    let buffer = test_buffer();

    c.bench_function("process_eq_only", |b| {
        let freqs = [31.0, 125.0, 500.0, 2000.0, 8000.0];
        let mut builder = ProcessorChain::builder()
            .channels(CHANNELS)
            .sample_rate(SAMPLE_RATE);
        for f in freqs {
            builder = builder.add_filter(
                ApoFilter::builder()
                    .filter_type(FilterType::Peaking)
                    .freq(f)
                    .gain_db(3.0)
                    .q(1.41)
                    .enabled(true)
                    .channels(CHANNELS)
                    .sample_rate(SAMPLE_RATE)
                    .build()
                    .unwrap(),
            );
        }
        let mut chain = builder.build();
        b.iter_batched_ref(
            || buffer.clone(),
            |buf| chain.process(black_box(buf)),
            BatchSize::SmallInput,
        );
    });

    c.bench_function("process_full_chain", |b| {
        let mut chain = full_chain();
        b.iter_batched_ref(
            || buffer.clone(),
            |buf| chain.process(black_box(buf)),
            BatchSize::SmallInput,
        );
    });
}

criterion_group!(benches, bench_chain);
criterion_main!(benches);
