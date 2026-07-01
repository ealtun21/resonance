//! Output dithering — TPDF (triangular probability density function) dither
//! applied as the final stage of the chain, before the f64 → output truncation.
//!
//! Dither decorrelates quantisation error from the signal: without it, quantising
//! a low-level signal produces harmonic distortion (a correlated stair-step);
//! with TPDF dither the error becomes a benign, signal-independent noise floor.
//!
//! `bits = None` (the default) is a bit-exact passthrough. A target depth of
//! 16/20/24 bits quantises to that grid. Note the OS sinks are f32 float, so this
//! is only audibly meaningful when a specific integer target depth matters
//! downstream — it is off by default.

/// A tiny, allocation-free xorshift64 PRNG. Deterministic (dither needs a
/// signal-independent noise source, not cryptographic randomness).
#[derive(Debug, Clone)]
struct Xorshift(u64);

impl Xorshift {
    fn seeded(channel: usize) -> Self {
        // Distinct non-zero seed per channel so channels dither independently.
        Self(0x9E37_79B9_7F4A_7C15 ^ ((channel as u64).wrapping_add(1)))
    }

    /// Next uniform in `[0, 1)` from the top 53 bits.
    fn next_f64(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        ((x >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

/// Final-stage TPDF dither + quantiser. Per-channel PRNG state.
#[derive(Debug, Clone)]
pub struct DitherStage {
    bits: Option<u32>,
    rng: Vec<Xorshift>,
}

impl DitherStage {
    #[must_use]
    pub fn new(channels: usize) -> Self {
        Self {
            bits: None,
            rng: (0..channels.max(1)).map(Xorshift::seeded).collect(),
        }
    }

    /// Target bit depth: `None` = off (bit-exact passthrough), else 16/20/24.
    pub fn set_bits(&mut self, bits: Option<u32>) {
        self.bits = bits;
    }

    #[must_use]
    pub fn bits(&self) -> Option<u32> {
        self.bits
    }

    /// Resize the per-channel PRNG state for a new channel count, keeping the
    /// target depth. (Rate-independent — no rebind on sample-rate change.)
    pub fn set_channels(&mut self, channels: usize) {
        let bits = self.bits;
        *self = Self::new(channels);
        self.bits = bits;
    }

    /// Dither + quantise `samples` (interleaved, `channels` wide) in place.
    /// No-op when the target depth is `None` (bit-exact).
    pub fn apply(&mut self, samples: &mut [f64], channels: usize) {
        let Some(bits) = self.bits else {
            return;
        };
        if bits == 0 || channels == 0 || self.rng.len() < channels {
            return;
        }
        // One LSB of the signed target grid: full-scale ±1.0 maps to ±2^(bits-1).
        let q = 1.0 / f64::from(1u32 << (bits - 1));
        let frames = samples.len() / channels;
        for frame in 0..frames {
            for ch in 0..channels {
                let idx = frame * channels + ch;
                let rng = &mut self.rng[ch];
                // Two independent uniforms → triangular in (−1, 1) LSB: TPDF.
                let tpdf = rng.next_f64() - rng.next_f64();
                samples[idx] = (samples[idx] / q + tpdf).round() * q;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DitherStage;
    use rustfft::{FftPlanner, num_complex::Complex};
    use std::f64::consts::PI;

    const SR: f64 = 48000.0;

    fn lsb(bits: u32) -> f64 {
        1.0 / f64::from(1u32 << (bits - 1))
    }

    #[test]
    #[allow(clippy::float_cmp)] // off = exact passthrough, bit-identical asserted
    fn dither_off_is_bit_exact() {
        let mut d = DitherStage::new(2);
        // Default is off.
        let input: Vec<f64> = (0..512)
            .map(|i| (f64::from(i) * 0.03).sin() * 0.5)
            .collect();
        let mut buf = input.clone();
        d.apply(&mut buf, 2);
        assert_eq!(buf, input);
    }

    #[test]
    fn dither_output_lies_on_quantization_grid() {
        let mut d = DitherStage::new(1);
        d.set_bits(Some(16));
        let q = lsb(16);
        let mut buf: Vec<f64> = (0..1024)
            .map(|i| (f64::from(i) * 0.05).sin() * 0.4)
            .collect();
        d.apply(&mut buf, 1);
        for &y in &buf {
            let k = y / q;
            assert!(
                (k - k.round()).abs() < 1e-6,
                "sample {y} is not on the {q}-spaced grid"
            );
        }
    }

    fn harmonic_energy(signal: &[f64], fund_bin: usize) -> f64 {
        let n = signal.len();
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(n);
        let mut buf: Vec<Complex<f64>> = signal.iter().map(|&x| Complex::new(x, 0.0)).collect();
        fft.process(&mut buf);
        // Sum power at the 2nd–7th harmonic bins (the distortion spurs).
        (2..=7).map(|h| buf[h * fund_bin].norm_sqr()).sum::<f64>()
    }

    #[test]
    fn dither_decorrelates_quantization_harmonics() {
        // A low-level tone quantised to 8 bits WITHOUT dither shows strong
        // harmonic spurs (correlated stair-step distortion). TPDF dither must
        // convert those spurs into a benign noise floor — so the harmonic energy
        // drops sharply.
        const N: usize = 32768;
        const FUND_BIN: usize = 300; // exact FFT bin → no leakage
        let q = lsb(8);
        let freq = FUND_BIN as f64 * SR / N as f64;
        let omega = 2.0 * PI * freq / SR;
        let amp = 1.5 * q; // ~1.5 LSB — quantisation is severe here
        let clean: Vec<f64> = (0..N).map(|i| amp * (omega * i as f64).sin()).collect();

        // Undithered quantisation (the reference distortion).
        let undithered: Vec<f64> = clean.iter().map(|&x| (x / q).round() * q).collect();

        // TPDF-dithered.
        let mut d = DitherStage::new(1);
        d.set_bits(Some(8));
        let mut dithered = clean.clone();
        d.apply(&mut dithered, 1);

        // The dithered signal must actually be quantised to the 8-bit grid —
        // otherwise a no-op "dither" would trivially pass (a clean sine has no
        // harmonics). This guards the comparison below.
        assert!(
            dithered
                .iter()
                .all(|&y| ((y / q).round() - y / q).abs() < 1e-6),
            "dithered output must be quantised to the target grid"
        );

        let h_undith = harmonic_energy(&undithered, FUND_BIN);
        let h_dith = harmonic_energy(&dithered, FUND_BIN);
        assert!(
            h_dith < h_undith * 0.5,
            "dither should suppress harmonic spurs: undithered {h_undith:.3e} vs dithered {h_dith:.3e}"
        );
    }

    #[test]
    fn dither_silence_stays_within_one_lsb() {
        // Dithering pure silence yields quantised dither noise in {−q, 0, +q}:
        // present (not all zero) but bounded by one LSB.
        let mut d = DitherStage::new(1);
        d.set_bits(Some(16));
        let q = lsb(16);
        let mut buf = vec![0.0f64; 4096];
        d.apply(&mut buf, 1);
        assert!(
            buf.iter().any(|&s| s != 0.0),
            "dither noise should be present"
        );
        assert!(
            buf.iter().all(|&s| s.abs() <= q + 1e-12 && s.is_finite()),
            "dithered silence must stay within one LSB and be finite"
        );
    }
}
