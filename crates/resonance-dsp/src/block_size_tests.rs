// crates/resonance-dsp/src/block_size_tests.rs
//! The e2e harness compares live output bit-exactly against an offline render.
//! The live host picks the block sizes, so every stage must produce the same
//! samples regardless of how the input is chunked.

use crate::chain::{PhaseMode, ProcessorChain};
use crate::convolution::IrData;
use crate::filter::{ApoFilter, FilterType};
use std::sync::Arc;

const RATE: f64 = 48_000.0;
const CH: usize = 2;

fn noise(frames: usize) -> Vec<f64> {
    let mut s = 0x2545_F491_4F6C_DD1Du64;
    (0..frames * CH)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        })
        .collect()
}

fn eq_filters() -> Vec<ApoFilter> {
    [
        (FilterType::Peaking, 1000.0, 6.0, 1.41),
        (FilterType::LowShelf, 120.0, 4.0, 0.7),
    ]
    .into_iter()
    .map(|(t, f, g, q)| {
        ApoFilter::builder()
            .filter_type(t)
            .freq(f)
            .gain_db(g)
            .q(q)
            .enabled(true)
            .channels(CH)
            .sample_rate(RATE)
            .build()
            .unwrap()
    })
    .collect()
}

fn linear_chain() -> ProcessorChain {
    let mut b = ProcessorChain::builder().channels(CH).sample_rate(RATE);
    for f in eq_filters() {
        b = b.add_filter(f);
    }
    let mut c = b.build();
    c.set_phase_mode(PhaseMode::Linear);
    let k = crate::linphase::render(&c.filters, CH, RATE).expect("kernel");
    c.eq_fir.load_ir(Arc::new(k)).unwrap();
    c
}

fn conv_chain() -> ProcessorChain {
    let mut c = ProcessorChain::builder()
        .channels(CH)
        .sample_rate(RATE)
        .build();
    let taps: Vec<f64> = noise(6000)
        .iter()
        .step_by(CH)
        .enumerate()
        .map(|(i, v)| v * (-(i as f64) / 900.0).exp())
        .collect();
    let ir = IrData {
        name: "t".into(),
        path: String::new(),
        sample_rate: RATE,
        channels: vec![taps],
    };
    c.convolution.load_ir(Arc::new(ir)).unwrap();
    c.convolution.set_enabled(true);
    c
}

fn run(
    mut chain: ProcessorChain,
    input: &[f64],
    sizes: &mut dyn Iterator<Item = usize>,
) -> Vec<f64> {
    let mut out = Vec::with_capacity(input.len());
    let mut pos = 0;
    while pos < input.len() {
        let n = (sizes.next().unwrap().max(1) * CH).min(input.len() - pos);
        let mut buf = input[pos..pos + n].to_vec();
        chain.process(&mut buf);
        out.extend_from_slice(&buf);
        pos += n;
    }
    out
}

fn assert_block_size_independent(make: fn() -> ProcessorChain) {
    let input = noise(48_000);
    let reference = run(make(), &input, &mut std::iter::repeat(1024));
    for fixed in [64usize, 128, 480, 4096] {
        let got = run(make(), &input, &mut std::iter::repeat(fixed));
        assert!(got == reference, "block size {fixed} changed the output");
    }
    let mut lcg = 12_345u64;
    let mut random = std::iter::from_fn(move || {
        lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        Some(1 + (lcg >> 33) as usize % 2048)
    });
    assert!(
        run(make(), &input, &mut random) == reference,
        "random block sizes changed the output"
    );
}

// Spike 4 (2026-10-07): FAILS. Output differs from block 1024 at every size tried
// (64..4096, max |diff| ~0.57, differences persist to the end of 1 s of noise), so
// live linear-phase output depends on the host quantum and cannot be compared
// against an offline render at a different block size. Product finding for the
// e2e plan (Task 20 marks linear-phase scenarios expected_fail).
#[test]
#[ignore = "known: linear-phase output depends on block size"]
fn linear_phase_output_is_independent_of_block_size() {
    assert_block_size_independent(linear_chain);
}

#[test]
fn convolution_output_is_independent_of_block_size() {
    assert_block_size_independent(conv_chain);
}
