//! Offline N-channel DSP harness. Deterministic, device-free, CI-friendly —
//! proves per-channel EQ targeting, the routing matrix, dynamic channel-count
//! changes, and that every effect survives non-stereo layouts. This is the
//! correctness backbone for the cross-platform N-channel work (no `PipeWire` /
//! `CoreAudio` / WASAPI needed to run it).

// Test assertions compare exact expected values (routing copies samples
// verbatim), so float equality is intended throughout this module.
#![allow(clippy::float_cmp)]

use crate::chain::{FxEffect, ProcessorChain};
use crate::channel::{ChannelMask, ChannelMatrix};
use crate::filter::{ApoFilter, FilterType};

const SR: f64 = 48_000.0;

fn peaking_band(freq: f64, gain_db: f64, channels: usize, mask: ChannelMask) -> ApoFilter {
    ApoFilter::builder()
        .filter_type(FilterType::Peaking)
        .freq(freq)
        .gain_db(gain_db)
        .q(1.0)
        .enabled(true)
        .channels(channels)
        .sample_rate(SR)
        .channel_mask(mask)
        .build()
        .unwrap()
}

// ── ChannelMask ─────────────────────────────────────────────────────────────

#[test]
fn channel_mask_semantics() {
    let all = ChannelMask::ALL;
    assert!(all.contains(0) && all.contains(63));
    assert!(all.is_global(8));

    let none = ChannelMask::NONE;
    assert!(none.is_empty());
    assert!(!none.contains(0));

    let one = ChannelMask::single(3);
    assert!(one.contains(3) && !one.contains(2));

    let some = ChannelMask::from_indices([0, 2, 4]);
    assert!(some.contains(0) && some.contains(2) && some.contains(4) && !some.contains(1));
    assert!(!some.is_global(5));

    let stereo = ChannelMask::from_indices([0, 1]);
    assert!(stereo.is_global(2));

    // bitset round-trips through the IPC representation
    assert_eq!(ChannelMask::from_bits(some.bits()), some);

    // with / without
    assert!(one.with(5).contains(5));
    assert!(!one.with(5).without(5).contains(5));

    // out-of-range indices don't wrap the shift
    assert!(ChannelMask::single(64).is_empty());
}

// ── ChannelMatrix ───────────────────────────────────────────────────────────

#[test]
fn matrix_identity_is_passthrough() {
    let m = ChannelMatrix::identity(4);
    assert!(m.is_identity());
    let src: Vec<f64> = (0..40).map(|i| f64::from(i) * 0.1).collect();
    let mut dst = vec![0.0; 40];
    m.apply(&src, &mut dst);
    assert_eq!(src, dst);
}

#[test]
fn matrix_swap_swaps_lr() {
    let m = ChannelMatrix::swap(2, 0, 1);
    assert!(!m.is_identity());
    // frames (L,R): (1,2),(3,4) → swapped (2,1),(4,3)
    let src = vec![1.0, 2.0, 3.0, 4.0];
    let mut dst = vec![0.0; 4];
    m.apply(&src, &mut dst);
    assert_eq!(dst, vec![2.0, 1.0, 4.0, 3.0]);
}

#[test]
fn matrix_downmix_sums_to_mono() {
    let m = ChannelMatrix::new(2, 1, vec![0.5, 0.5]).unwrap();
    assert_eq!(m.out_ch(), 1);
    assert_eq!(m.in_ch(), 2);
    // frames (1,3),(2,4) → averages 2,3
    let src = vec![1.0, 3.0, 2.0, 4.0];
    let mut dst = vec![0.0; 2];
    m.apply(&src, &mut dst);
    assert_eq!(dst, vec![2.0, 3.0]);
}

#[test]
fn matrix_upmix_duplicates_mono() {
    // out0 = in0, out1 = in0
    let m = ChannelMatrix::new(1, 2, vec![1.0, 1.0]).unwrap();
    let src = vec![1.0, 2.0, 3.0];
    let mut dst = vec![0.0; 6];
    m.apply(&src, &mut dst);
    assert_eq!(dst, vec![1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
}

#[test]
fn matrix_apply_clamps_mismatched_dims_without_panic() {
    // Defense-in-depth: a matrix whose in_ch/out_ch don't match the buffers must
    // bound its frame loop to both buffers and never panic / write out of range.
    // (in_ch > buffer width: previously misframed; in_ch leading to over-long
    // dst writes: previously panicked.)
    let m = ChannelMatrix::new(3, 2, vec![0.0; 6]).unwrap();
    let src = vec![0.5; 8]; // 8 samples; in_ch=3 → 2 full frames
    let mut dst = vec![0.0; 8];
    m.apply(&src, &mut dst); // must not panic

    // The 1→2 expansion that previously overran dst when frames came from src.
    let m2 = ChannelMatrix::new(1, 2, vec![1.0, 1.0]).unwrap();
    let src2 = vec![0.5; 4];
    let mut dst2 = vec![0.0; 4]; // only 2 output frames fit
    m2.apply(&src2, &mut dst2); // frames clamped to dst capacity → no panic
    assert!(dst2.iter().all(|v| v.is_finite()));
}

#[test]
fn matrix_new_rejects_bad_dimensions() {
    assert!(ChannelMatrix::new(2, 2, vec![1.0, 0.0, 0.0]).is_none());
    assert!(ChannelMatrix::new(0, 2, vec![]).is_none());
    assert!(ChannelMatrix::new(2, 0, vec![]).is_none());
}

// ── Per-channel EQ ──────────────────────────────────────────────────────────

#[test]
fn per_channel_eq_isolates_to_masked_channel() {
    // A +12 dB peaking band targeting channel 0 only, on a 4-channel chain.
    let chain_channels = 4;
    let band = peaking_band(1000.0, 12.0, chain_channels, ChannelMask::single(0));
    let mut chain = ProcessorChain::builder()
        .channels(chain_channels)
        .sample_rate(SR)
        .add_filter(band)
        .build();

    let frames = 256;
    let mut buf = vec![0.0f64; frames * chain_channels];
    for f in 0..frames {
        let v = ((f as f64) * 0.05).sin() * 0.5;
        for c in 0..chain_channels {
            buf[f * chain_channels + c] = v; // identical signal on every channel
        }
    }
    let input = buf.clone();
    chain.process(&mut buf);

    let mut ch0_changed = false;
    for f in 0..frames {
        for c in 1..chain_channels {
            let idx = f * chain_channels + c;
            assert_eq!(
                buf[idx].to_bits(),
                input[idx].to_bits(),
                "channel {c} must be bit-untouched by a ch0-masked band"
            );
        }
        if buf[f * chain_channels].to_bits() != input[f * chain_channels].to_bits() {
            ch0_changed = true;
        }
    }
    assert!(ch0_changed, "the masked channel 0 should be filtered");
}

// ── set_channels (device renegotiation) ─────────────────────────────────────

#[test]
fn set_channels_resizes_and_passes_through() {
    let mut chain = ProcessorChain::builder()
        .channels(2)
        .sample_rate(SR)
        .build();
    chain.set_channels(6);
    assert_eq!(chain.channels, 6);

    // Default chain (no bands, all effects at 0) is a bit-exact passthrough at 6ch.
    let input: Vec<f64> = (0..600)
        .map(|i| (f64::from(i) * 0.011).sin() * 0.7)
        .collect();
    let mut buf = input.clone();
    chain.process(&mut buf);
    assert_eq!(buf, input);
}

#[test]
fn set_channels_keeps_global_band_on_new_channels() {
    let band = peaking_band(1000.0, 9.0, 2, ChannelMask::ALL);
    let mut chain = ProcessorChain::builder()
        .channels(2)
        .sample_rate(SR)
        .add_filter(band)
        .build();
    chain.set_channels(6);

    let frames = 200;
    let mut buf = vec![0.0f64; frames * 6];
    for f in 0..frames {
        let v = ((f as f64) * 0.05).sin() * 0.5;
        for c in 0..6 {
            buf[f * 6 + c] = v;
        }
    }
    let input = buf.clone();
    chain.process(&mut buf);

    // An ALL-mask band must affect every channel, including the 4 added by widening.
    for c in 0..6 {
        let changed = (0..frames).any(|f| buf[f * 6 + c].to_bits() != input[f * 6 + c].to_bits());
        assert!(
            changed,
            "channel {c} should be filtered after widening to 6ch"
        );
    }
}

#[test]
fn set_channels_is_noop_for_same_or_zero() {
    let mut chain = ProcessorChain::builder()
        .channels(2)
        .sample_rate(SR)
        .build();
    chain.set_channels(2);
    assert_eq!(chain.channels, 2);
    chain.set_channels(0);
    assert_eq!(chain.channels, 2);
}

// ── Effects across channel counts ───────────────────────────────────────────

#[test]
fn effects_run_finite_for_various_channel_counts() {
    for ch in [1usize, 2, 4, 6, 8] {
        let mut chain = ProcessorChain::builder()
            .channels(ch)
            .sample_rate(SR)
            .build();
        for e in FxEffect::ALL {
            chain.set_effect_enabled(e, true);
            chain.set_effect_intensity(e, 0.6);
        }
        let frames = 128;
        let mut buf = vec![0.0f64; frames * ch];
        for f in 0..frames {
            for c in 0..ch {
                buf[f * ch + c] = ((f as f64) * 0.03 + c as f64).sin() * 0.3;
            }
        }
        chain.process(&mut buf);
        assert!(
            buf.iter().all(|s| s.is_finite()),
            "ch={ch}: non-finite output from the effect chain"
        );
    }
}

#[test]
fn surround_passthrough_unless_stereo() {
    for ch in [1usize, 4, 6] {
        let mut chain = ProcessorChain::builder()
            .channels(ch)
            .sample_rate(SR)
            .build();
        chain.set_effect_enabled(FxEffect::Surround, true);
        chain.set_effect_intensity(FxEffect::Surround, 1.0);
        let frames = 64;
        let mut buf = vec![0.0f64; frames * ch];
        for (i, s) in buf.iter_mut().enumerate() {
            *s = ((i as f64) * 0.07).sin() * 0.4;
        }
        let input = buf.clone();
        chain.process(&mut buf);
        assert_eq!(
            buf, input,
            "surround must pass through unchanged for ch={ch}"
        );
    }

    // Stereo with L ≠ R: surround must alter the signal.
    let mut chain = ProcessorChain::builder()
        .channels(2)
        .sample_rate(SR)
        .build();
    chain.set_effect_enabled(FxEffect::Surround, true);
    chain.set_effect_intensity(FxEffect::Surround, 1.0);
    let frames = 64;
    let mut buf = vec![0.0f64; frames * 2];
    for f in 0..frames {
        buf[f * 2] = ((f as f64) * 0.07).sin() * 0.4;
        buf[f * 2 + 1] = ((f as f64) * 0.07 + 1.0).sin() * 0.4;
    }
    let input = buf.clone();
    chain.process(&mut buf);
    assert_ne!(buf, input, "surround must alter a stereo signal");
}

// ── Chain routing integration ───────────────────────────────────────────────

#[test]
fn chain_route_none_is_copy() {
    let chain = ProcessorChain::builder()
        .channels(2)
        .sample_rate(SR)
        .build();
    assert_eq!(chain.out_channels(), 2);
    let processed = vec![1.0, 2.0, 3.0, 4.0];
    let mut out = vec![0.0; 4];
    chain.route(&processed, &mut out);
    assert_eq!(out, processed);
}

#[test]
fn chain_route_swap_applies_matrix() {
    let mut chain = ProcessorChain::builder()
        .channels(2)
        .sample_rate(SR)
        .build();
    chain.routing = Some(ChannelMatrix::swap(2, 0, 1));
    assert_eq!(chain.out_channels(), 2);
    let processed = vec![1.0, 2.0, 3.0, 4.0];
    let mut out = vec![0.0; 4];
    chain.route(&processed, &mut out);
    assert_eq!(out, vec![2.0, 1.0, 4.0, 3.0]);
}

#[test]
fn chain_route_downmix_changes_width() {
    let mut chain = ProcessorChain::builder()
        .channels(2)
        .sample_rate(SR)
        .build();
    chain.routing = Some(ChannelMatrix::new(2, 1, vec![0.5, 0.5]).unwrap());
    assert_eq!(chain.out_channels(), 1);
    let processed = vec![1.0, 3.0, 2.0, 4.0];
    let mut out = vec![0.0; 2];
    chain.route(&processed, &mut out);
    assert_eq!(out, vec![2.0, 3.0]);
}

#[test]
fn chain_route_identity_matrix_is_copy() {
    // A square identity routing matrix is detected and short-circuited to a copy.
    let mut chain = ProcessorChain::builder()
        .channels(2)
        .sample_rate(SR)
        .build();
    chain.routing = Some(ChannelMatrix::identity(2));
    let processed = vec![0.5, -0.5, 0.25, -0.25];
    let mut out = vec![0.0; 4];
    chain.route(&processed, &mut out);
    assert_eq!(out, processed);
}

// ── Per-channel EQ: frequency-response isolation ─────────────────────────────

/// Steady-state gain (dB) of channel `ch` at `freq`, feeding the same tone on
/// every channel through the chain. Measures the second half (post-settle).
fn chain_channel_gain_db(chain: &mut ProcessorChain, ch: usize, channels: usize, freq: f64) -> f64 {
    chain.reset();
    let total = (SR * 0.4) as usize;
    let omega = 2.0 * std::f64::consts::PI * freq / SR;
    let mut buf = vec![0.0f64; total * channels];
    for f in 0..total {
        let s = (omega * f as f64).sin();
        for c in 0..channels {
            buf[f * channels + c] = s;
        }
    }
    chain.process(&mut buf);
    let half = total / 2;
    let (mut in_sq, mut out_sq) = (0.0f64, 0.0f64);
    for f in half..total {
        let x = (omega * f as f64).sin();
        let y = buf[f * channels + ch];
        in_sq += x * x;
        out_sq += y * y;
    }
    20.0 * (out_sq / in_sq).sqrt().log10()
}

#[test]
fn per_channel_eq_fr_isolated_to_masked_channel() {
    // +12 dB @ 1 kHz on channel 0 only, 4-channel chain.
    let mut chain = ProcessorChain::builder()
        .channels(4)
        .sample_rate(SR)
        .add_filter(peaking_band(1000.0, 12.0, 4, ChannelMask::single(0)))
        .build();
    assert!(
        (chain_channel_gain_db(&mut chain, 0, 4, 1000.0) - 12.0).abs() < 1.0,
        "ch0 @1k +12"
    );
    assert!(
        chain_channel_gain_db(&mut chain, 0, 4, 10000.0).abs() < 1.0,
        "ch0 @10k flat"
    );
    assert!(
        chain_channel_gain_db(&mut chain, 1, 4, 1000.0).abs() < 0.1,
        "ch1 @1k untouched"
    );
    assert!(
        chain_channel_gain_db(&mut chain, 3, 4, 1000.0).abs() < 0.1,
        "ch3 @1k untouched"
    );
}

#[test]
fn multiple_per_channel_bands_target_distinct_channels() {
    // Band A: +6 @ 1 kHz on ch0; Band B: +6 @ 5 kHz on ch1.
    let mut chain = ProcessorChain::builder()
        .channels(4)
        .sample_rate(SR)
        .add_filter(peaking_band(1000.0, 6.0, 4, ChannelMask::single(0)))
        .add_filter(peaking_band(5000.0, 6.0, 4, ChannelMask::single(1)))
        .build();
    assert!(
        (chain_channel_gain_db(&mut chain, 0, 4, 1000.0) - 6.0).abs() < 1.0,
        "ch0 @1k +6"
    );
    assert!(
        chain_channel_gain_db(&mut chain, 0, 4, 5000.0).abs() < 1.0,
        "ch0 @5k flat"
    );
    assert!(
        (chain_channel_gain_db(&mut chain, 1, 4, 5000.0) - 6.0).abs() < 1.0,
        "ch1 @5k +6"
    );
    assert!(
        chain_channel_gain_db(&mut chain, 1, 4, 1000.0).abs() < 1.0,
        "ch1 @1k flat"
    );
    assert!(
        chain_channel_gain_db(&mut chain, 2, 4, 1000.0).abs() < 0.1,
        "ch2 untouched"
    );
}

#[test]
fn masked_band_still_targets_channel_after_widen() {
    let mut chain = ProcessorChain::builder()
        .channels(2)
        .sample_rate(SR)
        .add_filter(peaking_band(1000.0, 12.0, 2, ChannelMask::single(0)))
        .build();
    chain.set_channels(6);
    assert!(
        (chain_channel_gain_db(&mut chain, 0, 6, 1000.0) - 12.0).abs() < 1.0,
        "ch0 still +12"
    );
    for c in 1..6 {
        assert!(
            chain_channel_gain_db(&mut chain, c, 6, 1000.0).abs() < 0.1,
            "ch{c} untouched after widen"
        );
    }
}

// ── ChannelMatrix edge cases ─────────────────────────────────────────────────

#[test]
fn matrix_swap_edge_cases() {
    assert!(
        ChannelMatrix::swap(2, 0, 0).is_identity(),
        "swap(a,a) = identity"
    );
    assert!(
        ChannelMatrix::swap(2, 0, 5).is_identity(),
        "out-of-range index leaves identity"
    );
    assert!(!ChannelMatrix::swap(4, 0, 1).is_identity());
}

#[test]
fn matrix_swap_correct_over_many_frames() {
    let m = ChannelMatrix::swap(4, 0, 2); // swap ch0 ↔ ch2
    let frames = 1000;
    let mut src = vec![0.0f64; frames * 4];
    for f in 0..frames {
        for c in 0..4 {
            src[f * 4 + c] = (f * 4 + c) as f64; // unique per slot
        }
    }
    let mut dst = vec![0.0f64; frames * 4];
    m.apply(&src, &mut dst);
    for f in 0..frames {
        assert_eq!(dst[f * 4], src[f * 4 + 2], "frame {f}: ch0←ch2");
        assert_eq!(dst[f * 4 + 2], src[f * 4], "frame {f}: ch2←ch0");
        assert_eq!(dst[f * 4 + 1], src[f * 4 + 1], "frame {f}: ch1 kept");
        assert_eq!(dst[f * 4 + 3], src[f * 4 + 3], "frame {f}: ch3 kept");
    }
}

#[test]
fn matrix_rectangular_3_to_2() {
    // out0 = in0 + in1; out1 = in0 + in2.
    let m = ChannelMatrix::new(3, 2, vec![1.0, 1.0, 0.0, 1.0, 0.0, 1.0]).unwrap();
    let src = vec![1.0, 2.0, 3.0]; // one frame
    let mut dst = vec![0.0; 2];
    m.apply(&src, &mut dst);
    assert_eq!(dst, vec![3.0, 4.0]);
}

#[test]
fn out_channels_reflects_routing() {
    let mut chain = ProcessorChain::builder()
        .channels(2)
        .sample_rate(SR)
        .build();
    assert_eq!(chain.out_channels(), 2, "no routing → processing width");
    chain.routing = Some(ChannelMatrix::new(2, 1, vec![0.5, 0.5]).unwrap());
    assert_eq!(chain.out_channels(), 1, "downmix → 1");
    chain.routing = Some(ChannelMatrix::swap(2, 0, 1));
    assert_eq!(chain.out_channels(), 2, "square swap → 2");
}

// ── reset clears running state ────────────────────────────────────────────────

#[test]
fn reset_clears_effect_tail() {
    let mut chain = ProcessorChain::builder()
        .channels(2)
        .sample_rate(SR)
        .build();
    chain.set_effect_enabled(FxEffect::Ambience, true);
    chain.set_effect_intensity(FxEffect::Ambience, 0.8);
    // Build a reverb tail with a loud burst.
    let mut burst = vec![0.5f64; 4096];
    chain.process(&mut burst);
    // Sanity: a clone fed silence still rings (tail present).
    let mut probe = chain.clone();
    let mut tail = vec![0.0f64; 4096];
    probe.process(&mut tail);
    assert!(
        tail.iter().map(|x| x * x).sum::<f64>() > 1e-6,
        "ambience should leave a tail"
    );
    // After reset, silence is silent.
    chain.reset();
    let mut after = vec![0.0f64; 4096];
    chain.process(&mut after);
    let e: f64 = after.iter().map(|x| x * x).sum();
    assert!(e < 1e-9, "reset must clear the reverb tail, got {e}");
}
