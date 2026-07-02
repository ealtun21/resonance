//! Linear-phase FIR synthesis from the static filter bank.
//!
//! Samples the cascaded biquad magnitude response on an FFT grid, applies a
//! pure `N/2`-sample delay ramp (zero phase distortion), and windows the
//! inverse transform into a symmetric FIR kernel — one kernel per channel so
//! per-channel band masks survive the linearisation. The kernel is convolved
//! by the existing partitioned [`crate::convolution::ConvolutionEngine`]; this
//! module is synthesis only.

use crate::convolution::IrData;
use crate::filter::{ApoFilter, BandScope};
use rustfft::FftPlanner;
use rustfft::num_complex::Complex;
use std::f64::consts::PI;

/// Base FFT grid length at rates ≤ 48 kHz (~2.9 Hz resolution). Doubles per
/// rate octave so the low-frequency resolution is rate-independent.
const BASE_GRID: usize = 16_384;

/// FFT grid / kernel length for a sample rate: `BASE_GRID` at ≤ 48 kHz,
/// doubling each time the rate does (96 k → 32768, 192 k → 65536).
#[must_use]
pub fn grid_len(sample_rate: f64) -> usize {
    let mut n = BASE_GRID;
    let mut r = 48_000.0;
    while sample_rate > r * 1.001 {
        n *= 2;
        r *= 2.0;
    }
    n
}

/// Whether a band is realised by the linear-phase FIR (vs staying on the IIR
/// path): enabled, realisable at the current rate, plain `Stereo` scope and no
/// dynamics. Mid/Side bands would need an M/S kernel pair and dynamic bands
/// are level-dependent — both stay IIR (documented hybrid).
#[must_use]
pub fn is_linearizable(f: &ApoFilter) -> bool {
    f.enabled && f.is_realizable() && f.scope == BandScope::Stereo && f.dynamics().is_none()
}

/// Render the linearizable bands to per-channel symmetric FIR kernels at
/// `sample_rate`. Returns `None` when no band qualifies (mode falls back to
/// pure IIR at zero cost).
///
/// Construction: the bank magnitude is sampled on the length-`N` DFT grid and
/// given a pure `N/2`-sample delay ramp — with an integer delay the spectrum
/// is real (`mag · (−1)^k`) and Hermitian, so the inverse transform is an
/// exactly even sequence about `N/2`: linear phase by construction. A Hann
/// window (periodic, zero at the wrap tap) bounds spectral leakage.
#[must_use]
pub fn render(filters: &[ApoFilter], channels: usize, sample_rate: f64) -> Option<IrData> {
    let active: Vec<&ApoFilter> = filters.iter().filter(|f| is_linearizable(f)).collect();
    if active.is_empty() || channels == 0 || sample_rate <= 0.0 {
        return None;
    }
    let n = grid_len(sample_rate);
    let mut planner = FftPlanner::new();
    let ifft = planner.plan_fft_inverse(n);

    let kernels = (0..channels)
        .map(|ch| {
            let mut spec: Vec<Complex<f64>> = (0..n)
                .map(|k| {
                    let w = 2.0 * PI * k as f64 / n as f64;
                    // Real coefficients make |H| symmetric about Nyquist, so
                    // evaluating the upper half at its own `w` is the mirror.
                    let mag: f64 = active
                        .iter()
                        .filter(|f| f.mask.contains(ch))
                        .flat_map(|f| f.sections())
                        .map(|c| biquad_mag(c, w))
                        .product();
                    // e^{−jπk} = (−1)^k — the integer N/2-sample delay ramp.
                    let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
                    Complex::new(mag * sign, 0.0)
                })
                .collect();
            ifft.process(&mut spec);
            let scale = 1.0 / n as f64;
            let mut h: Vec<f64> = spec.iter().map(|c| c.re * scale).collect();
            for (i, t) in h.iter_mut().enumerate() {
                // Periodic Hann: 1 at the N/2 centre, exactly 0 at the wrap tap.
                let win = 0.5 * (1.0 - (2.0 * PI * i as f64 / n as f64).cos());
                *t *= win;
            }
            // The maths gives an even sequence; pin mirror pairs bit-equal so
            // float drift can't break the symmetry the mode promises.
            for i in 1..=(n / 2) {
                let j = n - i;
                let avg = f64::midpoint(h[i], h[j]);
                h[i] = avg;
                h[j] = avg;
            }
            h[0] = 0.0;
            h
        })
        .collect();

    Some(IrData {
        name: "linear-phase eq".into(),
        path: String::new(),
        sample_rate,
        channels: kernels,
    })
}

/// Magnitude of one biquad section at angular frequency `w` (rad/sample).
fn biquad_mag(c: &crate::filter::BiquadCoeffs, w: f64) -> f64 {
    let z1 = Complex::from_polar(1.0, -w);
    let z2 = Complex::from_polar(1.0, -2.0 * w);
    let num = Complex::new(c.b0, 0.0) + z1 * c.b1 + z2 * c.b2;
    let den = Complex::new(1.0, 0.0) + z1 * c.a1 + z2 * c.a2;
    (num / den).norm()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{ApoFilter, DynParams, FilterType};

    const SR: f64 = 48_000.0;

    fn band(ft: FilterType, freq: f64, gain_db: f64, q: f64, slope: u8) -> ApoFilter {
        ApoFilter::builder()
            .filter_type(ft)
            .freq(freq)
            .gain_db(gain_db)
            .q(q)
            .slope_db_oct(slope)
            .enabled(true)
            .channels(2)
            .sample_rate(SR)
            .build()
            .unwrap()
    }

    /// IIR reference: cascade magnitude of the whole bank at `freq`, in dB.
    fn iir_gain_db(filters: &[ApoFilter], freq: f64) -> f64 {
        let w = 2.0 * PI * freq / SR;
        let mag: f64 = filters
            .iter()
            .map(|f| {
                f.sections()
                    .copied()
                    .collect::<Vec<_>>()
                    .iter()
                    .map(|c| biquad_mag(c, w))
                    .product::<f64>()
            })
            .product();
        20.0 * mag.log10()
    }

    /// FIR gain at `freq` in dB: evaluate the kernel's DTFT directly.
    fn fir_gain_db(taps: &[f64], freq: f64) -> f64 {
        let w = 2.0 * PI * freq / SR;
        let h: Complex<f64> = taps
            .iter()
            .enumerate()
            .map(|(k, &t)| Complex::from_polar(t, -w * k as f64))
            .sum();
        20.0 * h.norm().log10()
    }

    #[test]
    fn kernel_is_symmetric() {
        let filters = vec![band(FilterType::Peaking, 1_000.0, 6.0, 1.0, 12)];
        let ir = render(&filters, 2, SR).expect("kernel");
        let h = &ir.channels[0];
        let n = h.len();
        assert_eq!(n, grid_len(SR));
        for i in 1..n {
            assert!(
                (h[i] - h[n - i]).abs() < 1e-12,
                "asymmetric at {i}: {} vs {}",
                h[i],
                h[n - i]
            );
        }
        assert!(h[0].abs() < 1e-12, "wrap tap must be windowed to zero");
    }

    #[test]
    fn magnitude_matches_iir_bank() {
        // Peaking + steep shelf + high-pass — the shapes the IIR bank uses.
        let filters = vec![
            band(FilterType::Peaking, 1_000.0, 6.0, 1.0, 12),
            band(FilterType::HighShelf, 8_000.0, -4.0, 0.707, 24),
            band(FilterType::HighPassQ, 60.0, 0.0, 0.707, 12),
        ];
        let ir = render(&filters, 1, SR).expect("kernel");
        for freq in [
            40.0, 60.0, 120.0, 400.0, 1_000.0, 2_500.0, 8_000.0, 16_000.0,
        ] {
            let want = iir_gain_db(&filters, freq);
            let got = fir_gain_db(&ir.channels[0], freq);
            assert!(
                (got - want).abs() < 0.25,
                "at {freq} Hz: fir {got:.2} dB vs iir {want:.2} dB"
            );
        }
    }

    #[test]
    fn masks_render_per_channel_kernels() {
        use crate::channel::ChannelMask;
        let mut only_ch1 = band(FilterType::Peaking, 2_000.0, 6.0, 2.0, 12);
        only_ch1.mask = ChannelMask::single(1);
        let filters = vec![band(FilterType::Peaking, 500.0, -3.0, 1.0, 12), only_ch1];
        let ir = render(&filters, 2, SR).expect("kernel");
        assert_eq!(ir.channels.len(), 2);
        // ch0 sees only the global band; ch1 sees both.
        let g0 = fir_gain_db(&ir.channels[0], 2_000.0);
        let g1 = fir_gain_db(&ir.channels[1], 2_000.0);
        assert!(
            g0.abs() < 0.3,
            "ch0 must not carry the masked band: {g0:.2}"
        );
        assert!((g1 - 6.0).abs() < 0.4, "ch1 must carry it: {g1:.2}");
    }

    #[test]
    fn skips_ms_and_dynamic_bands() {
        let mut side = band(FilterType::Peaking, 1_000.0, 6.0, 1.0, 12);
        side.scope = BandScope::Mid;
        let mut dynamic = band(FilterType::Peaking, 4_000.0, 0.0, 2.0, 12);
        dynamic.set_dynamics(Some(DynParams::DEFAULT), SR).unwrap();
        assert!(!is_linearizable(&side));
        assert!(!is_linearizable(&dynamic));
        // A bank with ONLY non-linearizable bands renders nothing.
        assert!(render(&[side, dynamic], 2, SR).is_none());
    }

    #[test]
    fn empty_and_disabled_render_none() {
        let mut off = band(FilterType::Peaking, 1_000.0, 6.0, 1.0, 12);
        off.enabled = false;
        assert!(render(&[], 2, SR).is_none());
        assert!(render(&[off], 2, SR).is_none());
    }

    #[test]
    fn grid_doubles_with_rate() {
        assert_eq!(grid_len(44_100.0), BASE_GRID);
        assert_eq!(grid_len(48_000.0), BASE_GRID);
        assert_eq!(grid_len(96_000.0), BASE_GRID * 2);
        assert_eq!(grid_len(192_000.0), BASE_GRID * 4);
    }
}
