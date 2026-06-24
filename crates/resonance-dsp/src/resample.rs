//! Streaming sample-rate conversion at the capture↔playback boundaries.
//!
//! The DSP `ProcessorChain` runs at a single rate; backends must reconcile a
//! capture device clocked at one rate with a playback device clocked at another
//! (Bluetooth codec switches, SCO 16 kHz, 44.1 kHz DACs). Handing a buffer
//! captured at rate A to a sink clocked at rate B without conversion makes the
//! playback clock reinterpret the buffer — a pitch shift plus glitches. This
//! module wraps [`rubato`]'s asynchronous sinc resampler so a backend can feed
//! arbitrary-length interleaved blocks in one rate and pull interleaved blocks
//! out at another.
//!
//! Design points:
//! - **Bypass when rates match.** [`StreamResampler::is_bypass`] is true when
//!   `from == to`; callers should skip conversion entirely on the common 48 k
//!   path so it costs nothing.
//! - **RT-safe steady state.** All staging buffers are pre-allocated for a
//!   bounded input block; [`StreamResampler::process`] does no heap allocation
//!   once the buffers have settled (the output staging Vec reaches its high-water
//!   mark within the first few blocks and then only ever clears, keeping
//!   capacity).
//! - **Interleaved in, interleaved out.** rubato works on per-channel planar
//!   slices; the interleave/deinterleave happens here so backends keep their
//!   interleaved buffers.
//! - **Quality.** A windowed-sinc interpolator (Blackman-Harris, 256-tap,
//!   256× oversampling) keeps THD+N far below audibility — see the resampler
//!   tests in `resonance-dsp`.

use rubato::{
    Resampler, Sample, SincFixedIn, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

/// Input frames consumed per internal rubato call. A fixed-input resampler
/// requires exactly this many frames per `process`, so input is buffered to
/// this boundary. Smaller → lower buffering latency (`chunk / from_hz`),
/// more per-call overhead. 256 frames ≈ 5.3 ms at 48 kHz — a good balance for
/// a live EQ path.
const CHUNK: usize = 256;

/// Upper bound on the interleaved input length a single [`StreamResampler::process`]
/// call will be handed, in frames. Backends deliver ≤1–2 k frames per audio
/// callback; 8192 is generous headroom so staging never reallocates.
const MAX_INPUT_FRAMES: usize = 8192;

/// High-quality windowed-sinc parameters. `f_cutoff` is auto-scaled by rubato
/// for downsampling ratios (anti-aliasing), so 0.95 is the upsampling cutoff.
fn sinc_params() -> SincInterpolationParameters {
    SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    }
}

/// Interleaved streaming sample-rate converter from `from_hz` to `to_hz`.
///
/// Generic over the sample type so a backend can resample in its native format
/// (`f32` for PipeWire/CoreAudio ring buffers, `f64` for the offline DSP
/// harness) without an extra conversion pass.
pub struct StreamResampler<T: Sample> {
    from_hz: f64,
    to_hz: f64,
    channels: usize,
    inner: Option<Inner<T>>,
    /// Interleaved output produced by the most recent `process` call. Reused
    /// across calls (cleared, capacity retained) so steady state is alloc-free.
    out_inter: Vec<T>,
}

struct Inner<T: Sample> {
    rs: SincFixedIn<T>,
    /// Interleaved input not yet consumed into a full [`CHUNK`] (always
    /// `< CHUNK * channels` samples between calls).
    in_acc: Vec<T>,
    /// Per-channel deinterleaved staging for one chunk (`channels × CHUNK`).
    in_planar: Vec<Vec<T>>,
    /// Per-channel output staging (`channels × output_frames_max`).
    out_planar: Vec<Vec<T>>,
}

impl<T: Sample> StreamResampler<T> {
    /// Build a resampler from `from_hz` to `to_hz` for `channels` interleaved
    /// channels. When the rates are equal (or either is non-positive) the
    /// resampler is a bypass: [`is_bypass`](Self::is_bypass) returns true and
    /// [`process`](Self::process) returns its input unchanged.
    pub fn new(from_hz: f64, to_hz: f64, channels: usize) -> Self {
        let channels = channels.max(1);
        let bypass = !(from_hz > 0.0 && to_hz > 0.0) || from_hz == to_hz;
        let inner = if bypass {
            None
        } else {
            // ratio = output rate / input rate (see rubato docs).
            let ratio = to_hz / from_hz;
            // max_relative_ratio 2.0 leaves room to nudge the ratio later for
            // clock-drift tracking without rebuilding the resampler.
            let rs = SincFixedIn::<T>::new(ratio, 2.0, sinc_params(), CHUNK, channels)
                .expect("valid resampler ratio");
            let out_max = rs.output_frames_max();
            let in_planar = vec![vec![T::zero(); CHUNK]; channels];
            let out_planar = vec![vec![T::zero(); out_max]; channels];
            let mut in_acc = Vec::with_capacity((CHUNK + MAX_INPUT_FRAMES) * channels);
            in_acc.clear();
            Some(Inner {
                rs,
                in_acc,
                in_planar,
                out_planar,
            })
        };

        // Worst case per call: ceil(MAX_INPUT_FRAMES / CHUNK) + 1 chunks, each
        // producing ≤ out_max frames. Pre-size so steady state never grows.
        let out_cap = match &inner {
            Some(i) => (MAX_INPUT_FRAMES / CHUNK + 2) * i.rs.output_frames_max() * channels,
            None => MAX_INPUT_FRAMES * channels,
        };

        Self {
            from_hz,
            to_hz,
            channels,
            inner,
            out_inter: Vec::with_capacity(out_cap),
        }
    }

    /// True when no conversion is needed (`from == to`). Callers should branch
    /// on this and use their input buffer directly to avoid an extra copy.
    pub fn is_bypass(&self) -> bool {
        self.inner.is_none()
    }

    pub fn from_hz(&self) -> f64 {
        self.from_hz
    }

    pub fn to_hz(&self) -> f64 {
        self.to_hz
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Added latency in **output** frames (the resampler's group delay). Zero
    /// when bypassing. Backends report this so the daemon's `status` can show
    /// the true end-to-end latency.
    pub fn output_delay_frames(&self) -> usize {
        self.inner
            .as_ref()
            .map(|i| i.rs.output_delay())
            .unwrap_or(0)
    }

    /// Resample one interleaved block (`from_hz`) and return the interleaved
    /// output produced (`to_hz`). The returned slice borrows internal storage
    /// valid until the next call. Output length varies per call (the resampler
    /// emits whole chunks as enough input accumulates); it may be empty when the
    /// first chunk is still filling.
    ///
    /// On bypass this returns `input` unchanged (no copy).
    pub fn process<'a>(&'a mut self, input: &'a [T]) -> &'a [T] {
        let Some(inner) = self.inner.as_mut() else {
            return input;
        };
        let channels = self.channels;
        self.out_inter.clear();

        inner.in_acc.extend_from_slice(input);
        let chunk_samples = CHUNK * channels;

        while inner.in_acc.len() >= chunk_samples {
            // Deinterleave the leading chunk into planar per-channel buffers.
            for (ch, plane) in inner.in_planar.iter_mut().enumerate() {
                for (frame, slot) in plane.iter_mut().enumerate() {
                    *slot = inner.in_acc[frame * channels + ch];
                }
            }

            let (_in_used, out_n) = inner
                .rs
                .process_into_buffer(&inner.in_planar, &mut inner.out_planar, None)
                .expect("resampler chunk");

            // Interleave the produced output frames.
            for frame in 0..out_n {
                for plane in inner.out_planar.iter() {
                    self.out_inter.push(plane[frame]);
                }
            }

            // Drop the consumed chunk from the front of the accumulator.
            inner.in_acc.copy_within(chunk_samples.., 0);
            let new_len = inner.in_acc.len() - chunk_samples;
            inner.in_acc.truncate(new_len);
        }

        &self.out_inter
    }

    /// Drop any buffered input and reset the resampler's internal history.
    pub fn reset(&mut self) {
        if let Some(inner) = self.inner.as_mut() {
            inner.in_acc.clear();
            inner.rs.reset();
        }
        self.out_inter.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bypass_returns_input_untouched() {
        let mut rs = StreamResampler::<f64>::new(48_000.0, 48_000.0, 2);
        assert!(rs.is_bypass());
        let input = vec![0.1, -0.2, 0.3, -0.4, 0.5, -0.6];
        let out = rs.process(&input);
        assert_eq!(out, input.as_slice());
        assert_eq!(rs.output_delay_frames(), 0);
    }

    #[test]
    fn zero_or_negative_rate_is_bypass() {
        assert!(StreamResampler::<f64>::new(0.0, 48_000.0, 2).is_bypass());
        assert!(StreamResampler::<f64>::new(48_000.0, -1.0, 2).is_bypass());
    }

    #[test]
    fn upsampling_emits_roughly_ratio_more_frames() {
        // 48 k → 96 k, stereo. Feed 4096 frames; expect ~2× out (minus the
        // priming chunk + group delay still in the pipeline).
        let mut rs = StreamResampler::<f64>::new(48_000.0, 96_000.0, 2);
        assert!(!rs.is_bypass());
        let frames_in = 4096;
        let input: Vec<f64> = (0..frames_in * 2)
            .map(|i| (i as f64 * 0.001).sin())
            .collect();
        let out = rs.process(&input);
        let frames_out = out.len() / 2;
        // Allow generous slack for chunk buffering + group delay.
        assert!(
            frames_out > frames_in * 3 / 2 && frames_out < frames_in * 5 / 2,
            "expected ~2× frames, got {frames_out} from {frames_in}"
        );
    }
}
