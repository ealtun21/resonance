//! Offline N-channel DSP harness. Deterministic, device-free, CI-friendly —
//! proves per-channel EQ targeting, the routing matrix, dynamic channel-count
//! changes, and that every effect survives non-stereo layouts. This is the
//! correctness backbone for the cross-platform N-channel work (no PipeWire /
//! CoreAudio / WASAPI needed to run it).

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
    let src: Vec<f64> = (0..40).map(|i| i as f64 * 0.1).collect();
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
    let input: Vec<f64> = (0..600).map(|i| ((i as f64) * 0.011).sin() * 0.7).collect();
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
