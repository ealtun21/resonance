//! Offline render of the daemon's exported chain (`ResetAndExportChain`).

use anyhow::{Context, Result, bail};
use resonance_apo::state::{ir_path_for, read_chain_fresh, read_ir_blob};
use resonance_dsp::chain::ProcessorChain;
use std::path::Path;
use std::sync::Arc;

/// Render block size. IIR stages are block-size independent; FFT stages are
/// covered by `resonance-dsp`'s `block_size_tests` (spike 4).
pub const BLOCK_FRAMES: usize = 1024;

/// Rebuild the exported chain at `channels`/`rate` with the APO's own builder
/// (`ChainSnapshot::build_full_chain`), from zeroed state. Notes list stages
/// the builder rejected.
pub fn load_exported_chain(
    path: &Path,
    channels: usize,
    rate: f64,
) -> Result<(ProcessorChain, Vec<String>)> {
    let (_, snap, _) = read_chain_fresh(path)
        .with_context(|| format!("read chain snapshot {}", path.display()))?;
    let ir = match snap.convolution_generation {
        0 => None,
        generation => {
            let (blob, ir) = read_ir_blob(&ir_path_for(path)).context("read IR sidecar")?;
            if blob != generation {
                bail!("IR sidecar generation {blob} != snapshot generation {generation}");
            }
            Some(Arc::new(ir))
        }
    };
    let (mut chain, notes) = snap.build_full_chain(channels, rate, ir.as_ref());
    chain.reset();
    Ok((chain, notes))
}

/// Process interleaved f32 `input` exactly as the `PipeWire` filter does
/// (`resonance-daemon/src/audio/pipewire.rs`): f32 → f64, `process`, square
/// route when the matrix matches the width, `as f32`.
#[must_use]
pub fn render(chain: &mut ProcessorChain, input: &[f32], block_frames: usize) -> Vec<f32> {
    let ch = chain.channels;
    let route = matches!(&chain.routing, Some(m) if m.in_ch() == ch && m.out_ch() == ch);
    let mut out = Vec::with_capacity(input.len());
    let mut routed = vec![0.0f64; block_frames * ch];
    for block in input.chunks(block_frames * ch) {
        let mut buf: Vec<f64> = block.iter().map(|&s| f64::from(s)).collect();
        chain.process(&mut buf);
        if route {
            chain.route(&buf, &mut routed[..buf.len()]);
            out.extend(routed[..buf.len()].iter().map(|&v| v as f32));
        } else {
            out.extend(buf.iter().map(|&v| v as f32));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stimulus::generate;
    use resonance_apo::state::ApoStateWriter;
    use resonance_dsp::filter::{ApoFilter, FilterType};

    fn tmp(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("resonance-e2e-render-{tag}-{}", std::process::id()))
            .join("chain.bin")
    }

    fn eq_chain() -> ProcessorChain {
        let mut b = ProcessorChain::builder().channels(2).sample_rate(48_000.0);
        for (t, f, g, q) in [
            (FilterType::Peaking, 1000.0, 6.0, 1.41),
            (FilterType::LowShelf, 120.0, 4.0, 0.7),
        ] {
            b = b.add_filter(
                ApoFilter::builder()
                    .filter_type(t)
                    .freq(f)
                    .gain_db(g)
                    .q(q)
                    .enabled(true)
                    .channels(2)
                    .sample_rate(48_000.0)
                    .build()
                    .unwrap(),
            );
        }
        b.build()
    }

    #[test]
    fn exported_flat_chain_is_a_bit_exact_passthrough() {
        let path = tmp("flat");
        ApoStateWriter::create(&path).unwrap().publish(
            &ProcessorChain::builder()
                .channels(2)
                .sample_rate(48_000.0)
                .build(),
        );
        let (mut c, notes) = load_exported_chain(&path, 2, 48_000.0).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        let input = generate(48_000, 2, 0.2).samples;
        assert_eq!(render(&mut c, &input, BLOCK_FRAMES), input);
    }

    #[test]
    fn rebuilt_eq_chain_renders_identically_to_the_original() {
        let path = tmp("eq");
        let mut original = eq_chain();
        ApoStateWriter::create(&path).unwrap().publish(&original);
        let (mut rebuilt, _) = load_exported_chain(&path, 2, 48_000.0).unwrap();
        let input = generate(48_000, 2, 0.2).samples;
        let a = render(&mut original, &input, BLOCK_FRAMES);
        assert_eq!(a, render(&mut rebuilt, &input, BLOCK_FRAMES));
        assert_ne!(a, input, "the EQ must change the signal");
    }

    #[test]
    fn iir_render_does_not_depend_on_block_size() {
        let input = generate(48_000, 2, 0.2).samples;
        assert_eq!(
            render(&mut eq_chain(), &input, 64),
            render(&mut eq_chain(), &input, 1024)
        );
    }

    #[test]
    fn missing_snapshot_is_an_error() {
        assert!(load_exported_chain(&tmp("missing"), 2, 48_000.0).is_err());
    }
}
