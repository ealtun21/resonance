//! Convolution / impulse-response engine — two-stage non-uniform partitioned
//! overlap-save FFT convolution for room correction, speaker correction,
//! HRTF and reverb IRs.
//!
//! The IR is split at [`HEAD_LEN`] taps into two uniform stages, each
//! transformed once at load time. The *head* uses [`BLOCK`]-sample partitions
//! so the stage keeps its fixed [`BLOCK`]-sample latency
//! ([`ConvolutionEngine::latency_frames`]; the daemon reports it so the UIs
//! can show the added delay). The *tail* uses [`TAIL_BLOCK`]-sample
//! partitions, amortising the per-block cost that made long IRs infeasible
//! under uniform partitioning (Gardner-style, two levels). `HEAD_LEN` is
//! exactly two tail blocks, so a completed tail block finishes one full
//! [`RATIO`]-sub-block cycle before its earliest output is due — the tail
//! FFTs and delay-line MACs are spread across that cycle's sub-blocks instead
//! of spiking one of them, keeping the RT path deterministic with no worker
//! thread. The engine buffers arbitrary-length chain blocks to the partition
//! boundary.
//!
//! Rate + width behaviour matches the rest of the chain: the source IR is kept
//! (shared via `Arc`) so a sample-rate rebind re-resamples and re-transforms it
//! at the new rate, and a channel-count change resizes the per-channel state.
//! IR-channel mapping: a mono IR applies to every audio channel; a multi-channel
//! IR maps channel-for-channel, with the last IR channel covering any extra
//! audio channels.
//!
//! Off (the default) and with no IR loaded the stage is a bit-exact passthrough.

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::collections::VecDeque;
use std::sync::Arc;

/// Input samples per partition. The engine buffers chain blocks to this
/// boundary, so it is also the added latency in frames (~5.3 ms at 48 kHz —
/// the same trade-off as the resampler's `CHUNK`).
pub const BLOCK: usize = 256;

/// FFT size: two blocks, the classic overlap-save configuration (each FFT
/// window holds the previous block + the current one).
const FFT_LEN: usize = 2 * BLOCK;

/// Input samples per tail partition. Larger partitions amortise the delay-line
/// MAC over more output samples, which is what makes multi-second IRs cheap.
const TAIL_BLOCK: usize = 4096;

/// Tail FFT size: two tail blocks (overlap-save, like the head).
const TAIL_FFT_LEN: usize = 2 * TAIL_BLOCK;

/// Taps covered by the head stage before the tail takes over. Exactly two
/// tail blocks — load-bearing: a tail input block's earliest output through
/// tail partition 0 is due two tail blocks after that block starts, so a
/// completed block leaves one full cycle of sub-blocks to compute in.
const HEAD_LEN: usize = 2 * TAIL_BLOCK;

/// Head blocks per tail block — the tail work-spreading cycle length.
const RATIO: usize = TAIL_BLOCK / BLOCK;

/// Upper bound on the usable IR length, in seconds at the DSP rate. The tail
/// stage makes the per-sample cost nearly flat in IR length, but the delay
/// line and kernel spectra still cost ~32 bytes per tap per channel each;
/// 10 s covers hall/church reverbs and any room correction while bounding
/// memory (~90 MB for a stereo 10 s IR at 96 kHz). Longer files are truncated
/// (the daemon logs it).
pub const MAX_IR_SECONDS: f64 = 10.0;

/// Frames per `process` call the FIFOs are pre-sized for (matches the
/// resampler's `MAX_INPUT_FRAMES`); larger calls still work but may grow them.
const MAX_INPUT_FRAMES: usize = 8192;

/// A decoded impulse response at its native sample rate, shared immutably so
/// rebuilding the kernel (rate rebind, channel resize, chain clone) never
/// copies the source audio.
#[derive(Debug, Clone)]
pub struct IrData {
    /// Display name (typically the file stem).
    pub name: String,
    /// Source path, for persistence and re-loading.
    pub path: String,
    /// Native sample rate of the file.
    pub sample_rate: f64,
    /// De-interleaved samples, one `Vec` per IR channel.
    pub channels: Vec<Vec<f64>>,
}

impl IrData {
    /// Build from interleaved samples (`ir_channels` wide). Returns `None` when
    /// the input is empty or the parameters are inconsistent.
    #[must_use]
    pub fn from_interleaved(
        name: String,
        path: String,
        sample_rate: f64,
        ir_channels: usize,
        interleaved: &[f64],
    ) -> Option<Self> {
        if ir_channels == 0 || interleaved.is_empty() || sample_rate <= 0.0 {
            return None;
        }
        let frames = interleaved.len() / ir_channels;
        if frames == 0 {
            return None;
        }
        let channels = (0..ir_channels)
            .map(|ch| {
                (0..frames)
                    .map(|f| interleaved[f * ir_channels + ch])
                    .collect()
            })
            .collect();
        Some(Self {
            name,
            path,
            sample_rate,
            channels,
        })
    }

    /// Frames per channel at the IR's native rate.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }
}

/// Summary of the active IR for status/UI display.
#[derive(Debug, Clone, PartialEq)]
pub struct ConvolutionInfo {
    pub name: String,
    pub path: String,
    /// Native rate of the source file.
    pub ir_sample_rate: f64,
    pub ir_channels: usize,
    /// Tap count actually convolved, at the DSP rate (after resampling and the
    /// [`MAX_IR_SECONDS`] cap).
    pub taps: usize,
    /// Fixed added latency in frames at the DSP rate.
    pub latency_frames: usize,
}

/// Per-audio-channel streaming state.
#[derive(Clone)]
struct ChannelState {
    /// Input samples waiting to fill the next block (`< BLOCK` after a call).
    pending: VecDeque<f64>,
    /// Convolved samples ready to emit. Primed with [`BLOCK`] zeros so the
    /// stage has a *fixed* latency instead of a fluctuating one.
    ready: VecDeque<f64>,
    /// Sliding time window: previous block in the first half, current in the
    /// second (the overlap-save FFT input).
    segment: Vec<f64>,
    /// Frequency-delay line: one input spectrum per partition, newest first.
    fdl: VecDeque<Vec<Complex<f64>>>,
    /// Tail-stage streaming state, present only when the IR outgrows
    /// [`HEAD_LEN`] — short IRs pay nothing for the second stage.
    tail: Option<TailChannelState>,
}

impl ChannelState {
    fn new(partitions: usize, tail_partitions: usize) -> Self {
        let mut ready = VecDeque::with_capacity(BLOCK + MAX_INPUT_FRAMES);
        ready.extend(std::iter::repeat_n(0.0, BLOCK));
        Self {
            pending: VecDeque::with_capacity(BLOCK + MAX_INPUT_FRAMES),
            ready,
            segment: vec![0.0; FFT_LEN],
            fdl: (0..partitions)
                .map(|_| vec![Complex::new(0.0, 0.0); FFT_LEN])
                .collect(),
            tail: (tail_partitions > 0).then(|| TailChannelState::new(tail_partitions)),
        }
    }

    fn reset(&mut self) {
        self.pending.clear();
        self.ready.clear();
        self.ready.extend(std::iter::repeat_n(0.0, BLOCK));
        self.segment.fill(0.0);
        for spec in &mut self.fdl {
            spec.fill(Complex::new(0.0, 0.0));
        }
        if let Some(tail) = self.tail.as_mut() {
            tail.reset();
        }
    }
}

/// Per-audio-channel tail-stage state. One *cycle* is [`RATIO`] head blocks =
/// one tail block; `phase` is the position within it. During cycle `C` the
/// stage emits `out` (computed last cycle), collects the cycle's input into
/// `collect`, and spreads the FFT + delay-line MAC for the *next* segment
/// across the sub-blocks, accumulating in `acc` and finishing into `next`.
#[derive(Clone)]
struct TailChannelState {
    /// Input samples of the in-progress tail block, one head block written
    /// per sub-block at offset `phase * BLOCK`.
    collect: Vec<f64>,
    /// Tail overlap-save window: previous tail block + the completed one.
    segment: Vec<f64>,
    /// Tail frequency-delay line: one tail-block spectrum per partition,
    /// newest first.
    fdl: VecDeque<Vec<Complex<f64>>>,
    /// Spectrum accumulator built up across the cycle's MAC phases.
    acc: Vec<Complex<f64>>,
    /// Tail output segment being emitted this cycle.
    out: Vec<f64>,
    /// Tail output segment being computed this cycle (emitted next cycle).
    next: Vec<f64>,
    /// Position within the [`RATIO`]-sub-block cycle.
    phase: usize,
}

impl TailChannelState {
    fn new(partitions: usize) -> Self {
        Self {
            collect: vec![0.0; TAIL_BLOCK],
            segment: vec![0.0; TAIL_FFT_LEN],
            fdl: (0..partitions)
                .map(|_| vec![Complex::new(0.0, 0.0); TAIL_FFT_LEN])
                .collect(),
            acc: vec![Complex::new(0.0, 0.0); TAIL_FFT_LEN],
            out: vec![0.0; TAIL_BLOCK],
            next: vec![0.0; TAIL_BLOCK],
            phase: 0,
        }
    }

    fn reset(&mut self) {
        self.collect.fill(0.0);
        self.segment.fill(0.0);
        for spec in &mut self.fdl {
            spec.fill(Complex::new(0.0, 0.0));
        }
        self.acc.fill(Complex::new(0.0, 0.0));
        self.out.fill(0.0);
        self.next.fill(0.0);
        self.phase = 0;
    }
}

/// The prepared kernel: IR partition spectra at the current DSP rate plus the
/// per-channel runtime state and FFT plans/scratch.
#[derive(Clone)]
struct Kernel {
    /// `[ir_channel][partition][bin]`, with the inverse-FFT 1/N normalisation
    /// folded in at load time. Shared so cloning a chain never re-transforms.
    partitions: Arc<Vec<Vec<Vec<Complex<f64>>>>>,
    /// Tail-stage kernel, present only when the IR outgrows [`HEAD_LEN`].
    tail: Option<TailKernel>,
    /// Taps convolved at the DSP rate (post-resample, post-cap).
    taps: usize,
    fft: Arc<dyn Fft<f64>>,
    ifft: Arc<dyn Fft<f64>>,
    state: Vec<ChannelState>,
    /// FFT working buffer (forward input / spectrum accumulator per block).
    work: Vec<Complex<f64>>,
    /// Spectrum multiply-accumulate target for the current block.
    acc: Vec<Complex<f64>>,
    scratch: Vec<Complex<f64>>,
}

/// Shared tail-stage kernel: [`TAIL_BLOCK`]-tap partition spectra plus the
/// tail FFT plans and their working storage.
#[derive(Clone)]
struct TailKernel {
    /// `[ir_channel][partition][bin]` at [`TAIL_FFT_LEN`], 1/N folded in.
    partitions: Arc<Vec<Vec<Vec<Complex<f64>>>>>,
    fft: Arc<dyn Fft<f64>>,
    ifft: Arc<dyn Fft<f64>>,
    /// Forward-FFT working buffer for completed tail blocks.
    work: Vec<Complex<f64>>,
    scratch: Vec<Complex<f64>>,
}

impl std::fmt::Debug for Kernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kernel")
            .field("taps", &self.taps)
            .field("ir_channels", &self.partitions.len())
            .field("audio_channels", &self.state.len())
            .finish_non_exhaustive()
    }
}

impl Kernel {
    /// Resample the IR to `sample_rate`, cap it to [`MAX_IR_SECONDS`], and
    /// transform the first [`HEAD_LEN`] taps into [`BLOCK`]-sized partitions
    /// and the remainder into [`TAIL_BLOCK`]-sized ones. Returns `None` for a
    /// degenerate IR (no channels / no samples).
    // similar_names: fft/ifft pairs are the domain's own vocabulary.
    #[allow(clippy::similar_names)]
    fn prepare(ir: &IrData, sample_rate: f64, channels: usize) -> Option<Self> {
        if ir.channels.is_empty() || ir.frames() == 0 || sample_rate <= 0.0 {
            return None;
        }
        // Cap at the *source* rate first so the resampler never chews through
        // minutes of audio only to have most of it truncated afterwards.
        let src_cap = (MAX_IR_SECONDS * ir.sample_rate) as usize;
        let dst_cap = (MAX_IR_SECONDS * sample_rate) as usize;

        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(FFT_LEN);
        let ifft = planner.plan_fft_inverse(FFT_LEN);
        let tail_fft = planner.plan_fft_forward(TAIL_FFT_LEN);
        let tail_ifft = planner.plan_fft_inverse(TAIL_FFT_LEN);
        let scratch_len = fft
            .get_inplace_scratch_len()
            .max(ifft.get_inplace_scratch_len());
        let tail_scratch_len = tail_fft
            .get_inplace_scratch_len()
            .max(tail_ifft.get_inplace_scratch_len());

        let mut taps = 0usize;
        let mut heads: Vec<Vec<Vec<Complex<f64>>>> = Vec::with_capacity(ir.channels.len());
        let mut tails: Vec<Vec<Vec<Complex<f64>>>> = Vec::with_capacity(ir.channels.len());
        for src in &ir.channels {
            let capped = &src[..src.len().min(src_cap.max(1))];
            let mut h = resample_ir(capped, ir.sample_rate, sample_rate);
            h.truncate(dst_cap.max(1));
            if h.is_empty() {
                return None;
            }
            taps = taps.max(h.len());

            let split = h.len().min(HEAD_LEN);
            heads.push(partition_spectra(
                &h[..split],
                BLOCK,
                FFT_LEN,
                fft.as_ref(),
                scratch_len,
            ));
            tails.push(partition_spectra(
                &h[split..],
                TAIL_BLOCK,
                TAIL_FFT_LEN,
                tail_fft.as_ref(),
                tail_scratch_len,
            ));
        }
        // Every IR channel must span the same number of partitions so a single
        // FDL depth serves them all: pad shorter channels with zero spectra.
        let nparts = heads.iter().map(Vec::len).max().unwrap_or(0);
        if nparts == 0 {
            return None;
        }
        for specs in &mut heads {
            specs.resize(nparts, vec![Complex::new(0.0, 0.0); FFT_LEN]);
        }
        let tail_nparts = tails.iter().map(Vec::len).max().unwrap_or(0);
        for specs in &mut tails {
            specs.resize(tail_nparts, vec![Complex::new(0.0, 0.0); TAIL_FFT_LEN]);
        }
        let tail = (tail_nparts > 0).then(|| TailKernel {
            partitions: Arc::new(tails),
            fft: tail_fft,
            ifft: tail_ifft,
            work: vec![Complex::new(0.0, 0.0); TAIL_FFT_LEN],
            scratch: vec![Complex::new(0.0, 0.0); tail_scratch_len],
        });

        Some(Self {
            partitions: Arc::new(heads),
            tail,
            taps,
            fft,
            ifft,
            state: (0..channels.max(1))
                .map(|_| ChannelState::new(nparts, tail_nparts))
                .collect(),
            work: vec![Complex::new(0.0, 0.0); FFT_LEN],
            acc: vec![Complex::new(0.0, 0.0); FFT_LEN],
            scratch: vec![Complex::new(0.0, 0.0); scratch_len],
        })
    }

    /// IR channel feeding audio channel `ch`: mono broadcasts, otherwise
    /// channel-for-channel with the last IR channel covering the excess.
    fn ir_channel_for(&self, ch: usize) -> usize {
        ch.min(self.partitions.len() - 1)
    }

    /// Convolve one full block for audio channel `ch`: consume [`BLOCK`]
    /// pending samples, emit [`BLOCK`] ready samples.
    fn process_block(&mut self, ch: usize) {
        let ir_ch = self.ir_channel_for(ch);
        let st = &mut self.state[ch];

        // Slide the overlap-save window: old current half becomes the previous
        // half, the new block fills the second half.
        st.segment.copy_within(BLOCK.., 0);
        for s in &mut st.segment[BLOCK..] {
            *s = st.pending.pop_front().unwrap_or(0.0);
        }

        for (w, &s) in self.work.iter_mut().zip(st.segment.iter()) {
            *w = Complex::new(s, 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.work, &mut self.scratch);

        // Rotate the FDL, reusing the oldest spectrum's storage for the newest.
        if let Some(mut oldest) = st.fdl.pop_back() {
            oldest.copy_from_slice(&self.work);
            st.fdl.push_front(oldest);
        }

        // Spectrum multiply-accumulate across the delay line.
        self.acc.fill(Complex::new(0.0, 0.0));
        for (spec, part) in st.fdl.iter().zip(self.partitions[ir_ch].iter()) {
            for ((a, x), h) in self.acc.iter_mut().zip(spec.iter()).zip(part.iter()) {
                *a += x * h;
            }
        }
        self.ifft
            .process_with_scratch(&mut self.acc, &mut self.scratch);

        // Overlap-save: the first half is circular wrap garbage; the second
        // half is the valid linear convolution of the new block. With a tail
        // stage, sum in its precomputed segment and run this sub-block's
        // share of the next segment's work.
        if let (Some(tk), Some(ts)) = (self.tail.as_mut(), st.tail.as_mut()) {
            let base = ts.phase * BLOCK;
            st.ready.extend(
                self.acc[BLOCK..]
                    .iter()
                    .zip(ts.out[base..base + BLOCK].iter())
                    .map(|(c, &t)| c.re + t),
            );

            if ts.phase == 0 {
                // A tail block just completed: window it, transform it,
                // rotate the tail FDL, start a fresh accumulation.
                ts.segment.copy_within(TAIL_BLOCK.., 0);
                ts.segment[TAIL_BLOCK..].copy_from_slice(&ts.collect);
                for (w, &s) in tk.work.iter_mut().zip(ts.segment.iter()) {
                    *w = Complex::new(s, 0.0);
                }
                tk.fft.process_with_scratch(&mut tk.work, &mut tk.scratch);
                if let Some(mut oldest) = ts.fdl.pop_back() {
                    oldest.copy_from_slice(&tk.work);
                    ts.fdl.push_front(oldest);
                }
                ts.acc.fill(Complex::new(0.0, 0.0));
            }

            // Stash this sub-block's input for the tail block in progress
            // (after the phase-0 hand-off above so it is never clobbered).
            ts.collect[base..base + BLOCK].copy_from_slice(&st.segment[BLOCK..]);

            // This phase's slice of the tail delay-line MAC — spread so no
            // single sub-block pays for the whole line.
            let parts = &tk.partitions[ir_ch];
            let chunk = parts.len().div_ceil(RATIO);
            let lo = (ts.phase * chunk).min(parts.len());
            let hi = ((ts.phase + 1) * chunk).min(parts.len());
            for (spec, part) in ts.fdl.iter().skip(lo).zip(parts[lo..hi].iter()) {
                for ((a, x), h) in ts.acc.iter_mut().zip(spec.iter()).zip(part.iter()) {
                    *a += x * h;
                }
            }

            if ts.phase == RATIO - 1 {
                // Cycle ends: finish the segment and hand it over for
                // emission throughout the next cycle.
                tk.ifft.process_with_scratch(&mut ts.acc, &mut tk.scratch);
                for (o, c) in ts.next.iter_mut().zip(ts.acc[TAIL_BLOCK..].iter()) {
                    *o = c.re;
                }
                std::mem::swap(&mut ts.out, &mut ts.next);
                ts.phase = 0;
            } else {
                ts.phase += 1;
            }
        } else {
            st.ready.extend(self.acc[BLOCK..].iter().map(|c| c.re));
        }
    }
}

/// Transform `h` into overlap-save partition spectra of `block` taps each,
/// zero-padded to `fft_len`, with the inverse FFT's 1/N folded in so the RT
/// path never scales. Empty input yields no partitions.
fn partition_spectra(
    h: &[f64],
    block: usize,
    fft_len: usize,
    fft: &dyn Fft<f64>,
    scratch_len: usize,
) -> Vec<Vec<Complex<f64>>> {
    let norm = 1.0 / fft_len as f64;
    let mut scratch = vec![Complex::new(0.0, 0.0); scratch_len];
    h.chunks(block)
        .map(|part| {
            let mut buf = vec![Complex::new(0.0, 0.0); fft_len];
            for (b, &s) in buf.iter_mut().zip(part.iter()) {
                *b = Complex::new(s * norm, 0.0);
            }
            fft.process_with_scratch(&mut buf, &mut scratch);
            buf
        })
        .collect()
}

/// Partitioned-convolution chain stage. Off + empty by default (bit-exact
/// passthrough); [`ConvolutionEngine::load_ir`] arms it.
#[derive(Debug, Clone)]
pub struct ConvolutionEngine {
    enabled: bool,
    channels: usize,
    sample_rate: f64,
    source: Option<Arc<IrData>>,
    kernel: Option<Kernel>,
}

impl ConvolutionEngine {
    #[must_use]
    pub fn new(channels: usize, sample_rate: f64) -> Self {
        Self {
            enabled: false,
            channels: channels.max(1),
            sample_rate,
            source: None,
            kernel: None,
        }
    }

    /// Prepare and install an IR at the engine's current rate/width, enabling
    /// the stage.
    ///
    /// # Errors
    /// Fails on a degenerate IR (no channels, or channels with no samples).
    pub fn load_ir(&mut self, ir: Arc<IrData>) -> Result<(), String> {
        let kernel = Kernel::prepare(&ir, self.sample_rate, self.channels)
            .ok_or_else(|| format!("impulse response '{}' has no usable samples", ir.name))?;
        self.kernel = Some(kernel);
        self.source = Some(ir);
        self.enabled = true;
        Ok(())
    }

    /// Drop the IR entirely (passthrough, zero latency).
    pub fn clear(&mut self) {
        self.kernel = None;
        self.source = None;
        self.enabled = false;
    }

    /// Bypass or re-arm without dropping the IR. Re-enabling restarts from
    /// silence (stale reverb tails are cleared).
    pub fn set_enabled(&mut self, on: bool) {
        if on && !self.enabled {
            self.reset();
        }
        self.enabled = on;
    }

    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The loaded IR source, if any (independent of the enabled flag).
    #[must_use]
    pub fn source(&self) -> Option<&Arc<IrData>> {
        self.source.as_ref()
    }

    /// Status summary of the loaded IR, if any.
    #[must_use]
    pub fn info(&self) -> Option<ConvolutionInfo> {
        let (ir, k) = (self.source.as_ref()?, self.kernel.as_ref()?);
        Some(ConvolutionInfo {
            name: ir.name.clone(),
            path: ir.path.clone(),
            ir_sample_rate: ir.sample_rate,
            ir_channels: ir.channels.len(),
            taps: k.taps,
            latency_frames: self.latency_frames(),
        })
    }

    /// Fixed added latency in frames: [`BLOCK`] while convolving, 0 bypassed.
    #[must_use]
    pub fn latency_frames(&self) -> usize {
        if self.enabled && self.kernel.is_some() {
            BLOCK
        } else {
            0
        }
    }

    /// Clear the streaming state (FIFOs, window, delay line) without touching
    /// the IR — the "history reset" counterpart of the effects' `reset`.
    pub fn reset(&mut self) {
        if let Some(k) = self.kernel.as_mut() {
            for st in &mut k.state {
                st.reset();
            }
        }
    }

    /// Re-prepare the kernel at a new DSP rate from the retained source IR.
    /// No-op when the rate is unchanged or nothing is loaded. Rebuilding
    /// resamples + re-transforms the IR, so on a live rate switch this is a
    /// heavyweight (but rare) operation — the same cost class as the effects'
    /// full rebuilds around it.
    // float_cmp: exact compare of stored rate vs incoming is the no-op guard.
    #[allow(clippy::float_cmp)]
    pub fn rebind_sample_rate(&mut self, sample_rate: f64) {
        if self.sample_rate == sample_rate {
            return;
        }
        self.sample_rate = sample_rate;
        if let Some(ir) = self.source.clone() {
            self.kernel = Kernel::prepare(&ir, sample_rate, self.channels);
        }
    }

    /// Resize the per-channel streaming state for a new processing width.
    /// Sample history restarts from silence (unavoidable on a layout change).
    pub fn set_channels(&mut self, channels: usize) {
        if channels == 0 || self.channels == channels {
            return;
        }
        self.channels = channels;
        if let Some(k) = self.kernel.as_mut() {
            let nparts = k.partitions.first().map_or(0, Vec::len);
            let tail_nparts = k
                .tail
                .as_ref()
                .map_or(0, |t| t.partitions.first().map_or(0, Vec::len));
            k.state = (0..channels)
                .map(|_| ChannelState::new(nparts, tail_nparts))
                .collect();
        }
    }

    /// Convolve an interleaved buffer in place. Passthrough when disabled or
    /// no IR is loaded. Output is delayed by [`BLOCK`] frames (primed with
    /// silence), so alignment is fixed rather than fluctuating with the
    /// caller's block size.
    pub fn process(&mut self, buf: &mut [f64], channels: usize) {
        if !self.enabled || channels == 0 {
            return;
        }
        let Some(k) = self.kernel.as_mut() else {
            return;
        };
        if k.state.len() < channels {
            return; // width mismatch: never index out of bounds on the RT path
        }
        let frames = buf.len() / channels;
        for ch in 0..channels {
            for f in 0..frames {
                k.state[ch].pending.push_back(buf[f * channels + ch]);
            }
            while k.state[ch].pending.len() >= BLOCK {
                k.process_block(ch);
            }
            let st = &mut k.state[ch];
            for f in 0..frames {
                buf[f * channels + ch] = st.ready.pop_front().unwrap_or(0.0);
            }
        }
    }
}

/// Offline-resample one IR channel from `from_hz` to `to_hz` with a direct
/// windowed-sinc evaluation (Blackman window, ±[`SINC_HALF_TAPS`] input taps).
///
/// Deliberately self-contained rather than reusing the streaming `rubato`
/// resampler: IR preparation also runs inside `audiodg.exe` (the Windows APO),
/// where the streaming path was observed to produce a corrupted kernel while
/// the identical code passed everywhere else (see issue #45). A closed-form
/// per-sample evaluation has no internal state to corrupt and is deterministic
/// in any host process; IRs are short enough that the O(len × taps) cost is
/// milliseconds.
///
/// The taps are scaled by `from/to`: resampling preserves the *waveform's*
/// amplitude, but a convolution kernel must preserve the *filter's* frequency
/// response, and a filter's gain is the rate-weighted sum of its taps (e.g. an
/// upsampled delta would otherwise gain `to/from` at DC).
// float_cmp: the exact rate match gates the copy-through fast path.
#[allow(clippy::float_cmp)]
fn resample_ir(samples: &[f64], from_hz: f64, to_hz: f64) -> Vec<f64> {
    const SINC_HALF_TAPS: isize = 48;
    if !(from_hz > 0.0 && to_hz > 0.0) || from_hz == to_hz || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = to_hz / from_hz;
    let expected = ((samples.len() as f64) * ratio).ceil() as usize;
    // Downsampling must anti-alias at the OUTPUT Nyquist: cutoff (as a fraction
    // of the input Nyquist) = ratio. Upsampling keeps the full input band.
    let cutoff = ratio.min(1.0);
    let filter_gain = from_hz / to_hz;
    // The window spans ±half_width input samples around the output position;
    // widen it when downsampling so the (narrower) sinc keeps enough lobes.
    let half_width = (SINC_HALF_TAPS as f64 / cutoff).ceil() as isize;

    let sinc = |u: f64| {
        if u.abs() < 1e-12 {
            1.0
        } else {
            (std::f64::consts::PI * u).sin() / (std::f64::consts::PI * u)
        }
    };
    let mut out = Vec::with_capacity(expected);
    for j in 0..expected {
        // Position of output sample j on the input's time axis.
        let t = j as f64 / ratio;
        let centre = t.round() as isize;
        let mut acc = 0.0;
        for i in (centre - half_width)..=(centre + half_width) {
            if i < 0 || i as usize >= samples.len() {
                continue;
            }
            let d = t - i as f64;
            let x = d / half_width as f64; // in [-1, 1] inside the support
            let window = 0.42
                + 0.5 * (std::f64::consts::PI * x).cos()
                + 0.08 * (2.0 * std::f64::consts::PI * x).cos();
            acc += samples[i as usize] * cutoff * sinc(cutoff * d) * window;
        }
        out.push(acc * filter_gain);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Direct time-domain convolution, the obviously-correct reference.
    fn direct_conv(x: &[f64], h: &[f64]) -> Vec<f64> {
        let mut y = vec![0.0; x.len()];
        for (n, out) in y.iter_mut().enumerate() {
            for (k, &hk) in h.iter().enumerate() {
                if n >= k {
                    *out += x[n - k] * hk;
                }
            }
        }
        y
    }

    fn ir_from_taps(taps: Vec<Vec<f64>>, rate: f64) -> Arc<IrData> {
        Arc::new(IrData {
            name: "test".into(),
            path: "/test.wav".into(),
            sample_rate: rate,
            channels: taps,
        })
    }

    fn noise(len: usize) -> Vec<f64> {
        // Deterministic pseudo-noise (no RNG dep, reproducible).
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                ((x >> 11) as f64) / ((1u64 << 53) as f64) - 0.5
            })
            .collect()
    }

    #[test]
    #[allow(clippy::float_cmp)] // disabled/empty = exact passthrough
    fn disabled_or_empty_is_bit_exact_passthrough() {
        let mut e = ConvolutionEngine::new(2, 48_000.0);
        let input: Vec<f64> = (0..512).map(|i| (f64::from(i) * 0.01).sin()).collect();
        let mut buf = input.clone();
        e.process(&mut buf, 2);
        assert_eq!(buf, input, "empty engine must pass through");
        assert_eq!(e.latency_frames(), 0);

        e.load_ir(ir_from_taps(vec![vec![1.0]], 48_000.0)).unwrap();
        e.set_enabled(false);
        let mut buf = input.clone();
        e.process(&mut buf, 2);
        assert_eq!(buf, input, "disabled engine must pass through");
        assert_eq!(e.latency_frames(), 0);
    }

    #[test]
    fn identity_ir_delays_by_exactly_one_block() {
        let mut e = ConvolutionEngine::new(1, 48_000.0);
        e.load_ir(ir_from_taps(vec![vec![1.0]], 48_000.0)).unwrap();
        assert_eq!(e.latency_frames(), BLOCK);

        let input = noise(4 * BLOCK);
        let mut buf = input.clone();
        e.process(&mut buf, 1);
        for s in &buf[..BLOCK] {
            assert!(s.abs() < 1e-12, "priming region must be silence");
        }
        for i in BLOCK..buf.len() {
            assert!(
                (buf[i] - input[i - BLOCK]).abs() < 1e-9,
                "identity IR: out[{i}] should equal in[{}]",
                i - BLOCK
            );
        }
    }

    #[test]
    fn matches_direct_convolution_across_partitions() {
        // 1000 taps spans 4 partitions — exercises the FDL accumulation.
        let h = noise(1000);
        let x = noise(3000);
        let reference = direct_conv(&x, &h);

        let mut e = ConvolutionEngine::new(1, 48_000.0);
        e.load_ir(ir_from_taps(vec![h], 48_000.0)).unwrap();
        let mut buf = x.clone();
        // Feed extra zeros to flush the last block out of the FIFO.
        buf.extend(std::iter::repeat_n(0.0, BLOCK));
        e.process(&mut buf, 1);

        for (i, r) in reference.iter().enumerate() {
            assert!(
                (buf[i + BLOCK] - r).abs() < 1e-9,
                "partitioned vs direct mismatch at {i}: {} vs {r}",
                buf[i + BLOCK]
            );
        }
    }

    #[test]
    fn streaming_is_block_size_invariant() {
        // Chopping the input into awkward block sizes must yield the same
        // output as one big call — the FIFOs make the engine size-agnostic.
        let h = noise(700);
        let x = noise(2500);

        let mut one = ConvolutionEngine::new(1, 48_000.0);
        one.load_ir(ir_from_taps(vec![h.clone()], 48_000.0))
            .unwrap();
        let mut whole = x.clone();
        one.process(&mut whole, 1);

        let mut chopped = ConvolutionEngine::new(1, 48_000.0);
        chopped.load_ir(ir_from_taps(vec![h], 48_000.0)).unwrap();
        let mut pieces = Vec::new();
        let sizes = [480usize, 37, 1024, 3, 256, 700];
        let mut off = 0;
        for &s in sizes.iter().cycle() {
            if off >= x.len() {
                break;
            }
            let end = (off + s).min(x.len());
            let mut part = x[off..end].to_vec();
            chopped.process(&mut part, 1);
            pieces.extend_from_slice(&part);
            off = end;
        }

        for (i, (a, b)) in whole.iter().zip(pieces.iter()).enumerate() {
            assert!(
                (a - b).abs() < 1e-12,
                "block-size variance changed output at {i}: {a} vs {b}"
            );
        }
    }

    #[test]
    fn stereo_ir_convolves_channels_independently() {
        // L IR = delta (identity), R IR = one-block delay (delta at BLOCK).
        let mut r_ir = vec![0.0; BLOCK + 1];
        r_ir[BLOCK] = 1.0;
        let mut e = ConvolutionEngine::new(2, 48_000.0);
        e.load_ir(ir_from_taps(vec![vec![1.0], r_ir], 48_000.0))
            .unwrap();

        let mono = noise(4 * BLOCK);
        let mut buf: Vec<f64> = mono.iter().flat_map(|&s| [s, s]).collect();
        e.process(&mut buf, 2);

        for f in BLOCK..(mono.len()) {
            let l = buf[f * 2];
            assert!(
                (l - mono[f - BLOCK]).abs() < 1e-9,
                "L must be the identity path at frame {f}"
            );
        }
        for f in (2 * BLOCK)..mono.len() {
            let r = buf[f * 2 + 1];
            assert!(
                (r - mono[f - 2 * BLOCK]).abs() < 1e-9,
                "R must be delayed one extra block at frame {f}"
            );
        }
    }

    #[test]
    fn mono_ir_broadcasts_to_all_channels() {
        let mut e = ConvolutionEngine::new(2, 48_000.0);
        e.load_ir(ir_from_taps(vec![vec![0.5]], 48_000.0)).unwrap();
        let mono = noise(2 * BLOCK);
        let mut buf: Vec<f64> = mono.iter().flat_map(|&s| [s, -s]).collect();
        e.process(&mut buf, 2);
        for f in BLOCK..mono.len() {
            assert!((buf[f * 2] - 0.5 * mono[f - BLOCK]).abs() < 1e-9);
            assert!((buf[f * 2 + 1] + 0.5 * mono[f - BLOCK]).abs() < 1e-9);
        }
    }

    #[test]
    fn rebind_resamples_ir_to_new_rate() {
        // A 100-tap IR at 48 k becomes ~200 taps at 96 k.
        let mut e = ConvolutionEngine::new(1, 48_000.0);
        e.load_ir(ir_from_taps(vec![noise(100)], 48_000.0)).unwrap();
        assert_eq!(e.info().unwrap().taps, 100);
        e.rebind_sample_rate(96_000.0);
        let taps = e.info().unwrap().taps;
        assert!(
            (195..=205).contains(&taps),
            "expected ~200 taps at 96 k, got {taps}"
        );
        // Still convolves correctly after the rebind.
        let mut buf = noise(1024);
        e.process(&mut buf, 1);
        assert!(buf.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn resampled_identity_ir_preserves_signal_energy() {
        // A delta at 44.1 k resampled to 48 k is a sinc kernel, but it still
        // convolves to ≈ the input (unit DC gain): RMS in ≈ RMS out.
        let mut delta = vec![0.0; 64];
        delta[32] = 1.0; // centred so the sinc tails fit in the resampled IR
        let mut e = ConvolutionEngine::new(1, 48_000.0);
        e.load_ir(ir_from_taps(vec![delta], 44_100.0)).unwrap();

        let n = 8192u32;
        let tone: Vec<f64> = (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * 1000.0 / 48_000.0 * f64::from(i)).sin())
            .collect();
        let mut buf = tone.clone();
        e.process(&mut buf, 1);
        let rms = |v: &[f64]| (v.iter().map(|x| x * x).sum::<f64>() / v.len() as f64).sqrt();
        // Skip the priming + IR centring transient at the head.
        let (a, b) = (rms(&tone[1024..]), rms(&buf[1024..]));
        assert!(
            (a - b).abs() / a < 0.02,
            "resampled delta should preserve level: in {a:.4} out {b:.4}"
        );
    }

    #[test]
    fn overlong_ir_is_truncated_to_cap() {
        let cap = (MAX_IR_SECONDS * 48_000.0) as usize;
        let mut e = ConvolutionEngine::new(1, 48_000.0);
        e.load_ir(ir_from_taps(vec![vec![0.001; cap + 10_000]], 48_000.0))
            .unwrap();
        assert_eq!(e.info().unwrap().taps, cap);
    }

    #[test]
    fn set_channels_resizes_and_keeps_convolving() {
        let mut e = ConvolutionEngine::new(2, 48_000.0);
        e.load_ir(ir_from_taps(vec![vec![1.0]], 48_000.0)).unwrap();
        e.set_channels(6);
        let mono = noise(2 * BLOCK);
        let mut buf: Vec<f64> = mono.iter().flat_map(|&s| [s, s, s, s, s, s]).collect();
        e.process(&mut buf, 6);
        for f in BLOCK..mono.len() {
            for ch in 0..6 {
                assert!(
                    (buf[f * 6 + ch] - mono[f - BLOCK]).abs() < 1e-9,
                    "channel {ch} frame {f} after width change"
                );
            }
        }
    }

    #[test]
    fn clear_returns_to_passthrough() {
        let mut e = ConvolutionEngine::new(1, 48_000.0);
        e.load_ir(ir_from_taps(vec![vec![0.25]], 48_000.0)).unwrap();
        e.clear();
        assert!(e.info().is_none());
        let input = noise(512);
        let mut buf = input.clone();
        e.process(&mut buf, 1);
        assert_eq!(
            buf.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            input.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            "cleared engine must be bit-exact passthrough"
        );
    }

    #[test]
    fn reenabling_clears_stale_tails() {
        let mut e = ConvolutionEngine::new(1, 48_000.0);
        e.load_ir(ir_from_taps(vec![noise(2000)], 48_000.0))
            .unwrap();
        let mut buf = noise(4 * BLOCK);
        e.process(&mut buf, 1);
        e.set_enabled(false);
        e.set_enabled(true);
        // After re-enable, silence in must give (delayed) silence out — no
        // leftover reverb tail from before the bypass.
        let mut silence = vec![0.0; 4 * BLOCK];
        e.process(&mut silence, 1);
        assert!(
            silence.iter().all(|&s| s.abs() < 1e-12),
            "stale tail leaked through re-enable"
        );
    }

    #[test]
    fn ir_data_from_interleaved_deinterleaves() {
        let ir = IrData::from_interleaved(
            "x".into(),
            "/x.wav".into(),
            48_000.0,
            2,
            &[1.0, 10.0, 2.0, 20.0, 3.0, 30.0],
        )
        .unwrap();
        assert_eq!(ir.channels.len(), 2);
        assert_eq!(ir.channels[0], vec![1.0, 2.0, 3.0]);
        assert_eq!(ir.channels[1], vec![10.0, 20.0, 30.0]);
        assert_eq!(ir.frames(), 3);
        assert!(IrData::from_interleaved("x".into(), "p".into(), 48_000.0, 0, &[1.0]).is_none());
        assert!(IrData::from_interleaved("x".into(), "p".into(), 48_000.0, 2, &[]).is_none());
    }

    /// Sparse IR: mostly zeros with a few taps, so the analytic reference is
    /// a cheap sum of delayed copies — exact alignment checks stay fast even
    /// for IRs far past the head/tail partition boundary.
    fn sparse_ir(len: usize, taps: &[(usize, f64)]) -> Vec<f64> {
        let mut h = vec![0.0; len];
        for &(i, a) in taps {
            h[i] = a;
        }
        h
    }

    fn sparse_reference(x: &[f64], taps: &[(usize, f64)]) -> Vec<f64> {
        (0..x.len())
            .map(|n| {
                taps.iter()
                    .filter(|&&(i, _)| n >= i)
                    .map(|&(i, a)| a * x[n - i])
                    .sum()
            })
            .collect()
    }

    #[test]
    fn long_sparse_ir_matches_reference_across_head_tail_boundary() {
        // Taps straddle every alignment hazard: block edges, the head/tail
        // split at 8192, tail-partition edges and the IR's last sample.
        let taps = [
            (0usize, 1.0),
            (255, -0.5),
            (8191, 0.75),
            (8192, -0.25),
            (8193, 0.5),
            (12000, -0.125),
            (16384, 0.375),
            (20479, -0.0625),
        ];
        let h = sparse_ir(20480, &taps);
        let x = noise(30000);
        let reference = sparse_reference(&x, &taps);

        let mut e = ConvolutionEngine::new(1, 48_000.0);
        e.load_ir(ir_from_taps(vec![h], 48_000.0)).unwrap();
        let mut buf = x.clone();
        buf.extend(std::iter::repeat_n(0.0, BLOCK));
        e.process(&mut buf, 1);

        for (i, r) in reference.iter().enumerate() {
            assert!(
                (buf[i + BLOCK] - r).abs() < 1e-9,
                "long sparse IR mismatch at {i}: {} vs {r}",
                buf[i + BLOCK]
            );
        }
    }

    #[test]
    fn long_dense_ir_matches_direct_convolution() {
        // 10 000 dense taps crosses the head/tail boundary; 16 384 input
        // samples spans four tail cycles, so the segment swap runs repeatedly.
        let h = noise(10_000);
        let x = noise(16_384);
        let reference = direct_conv(&x, &h);

        let mut e = ConvolutionEngine::new(1, 48_000.0);
        e.load_ir(ir_from_taps(vec![h], 48_000.0)).unwrap();
        let mut buf = x.clone();
        buf.extend(std::iter::repeat_n(0.0, BLOCK));
        e.process(&mut buf, 1);

        for (i, r) in reference.iter().enumerate() {
            assert!(
                (buf[i + BLOCK] - r).abs() < 1e-9,
                "long dense IR mismatch at {i}: {} vs {r}",
                buf[i + BLOCK]
            );
        }
    }

    #[test]
    fn long_ir_streaming_is_block_size_invariant() {
        let taps = [(0usize, 1.0), (9000, 0.5), (16383, -0.25)];
        let h = sparse_ir(16_384, &taps);
        let x = noise(20_000);

        let mut one = ConvolutionEngine::new(1, 48_000.0);
        one.load_ir(ir_from_taps(vec![h.clone()], 48_000.0))
            .unwrap();
        let mut whole = x.clone();
        one.process(&mut whole, 1);

        let mut chopped = ConvolutionEngine::new(1, 48_000.0);
        chopped.load_ir(ir_from_taps(vec![h], 48_000.0)).unwrap();
        let mut pieces = Vec::new();
        let sizes = [480usize, 37, 1024, 3, 256, 700, 4096, 8191];
        let mut off = 0;
        for &s in sizes.iter().cycle() {
            if off >= x.len() {
                break;
            }
            let end = (off + s).min(x.len());
            let mut part = x[off..end].to_vec();
            chopped.process(&mut part, 1);
            pieces.extend_from_slice(&part);
            off = end;
        }

        for (i, (a, b)) in whole.iter().zip(pieces.iter()).enumerate() {
            assert!(
                (a - b).abs() < 1e-12,
                "long-IR block-size variance at {i}: {a} vs {b}"
            );
        }
    }

    #[test]
    fn mid_length_ir_is_no_longer_truncated() {
        // 3 s at 48 kHz — over the old 2 s cap, well under the new one.
        let taps = 3 * 48_000;
        let mut e = ConvolutionEngine::new(1, 48_000.0);
        e.load_ir(ir_from_taps(vec![vec![0.001; taps]], 48_000.0))
            .unwrap();
        assert_eq!(e.info().unwrap().taps, taps);
    }

    #[test]
    fn long_ir_reenabling_clears_stale_tails() {
        let mut e = ConvolutionEngine::new(1, 48_000.0);
        e.load_ir(ir_from_taps(vec![noise(20_000)], 48_000.0))
            .unwrap();
        let mut buf = noise(6 * 4096);
        e.process(&mut buf, 1);
        e.set_enabled(false);
        e.set_enabled(true);
        // A long IR keeps state in the tail stage too: silence in must give
        // silence out across several full tail cycles.
        let mut silence = vec![0.0; 6 * 4096];
        e.process(&mut silence, 1);
        assert!(
            silence.iter().all(|&s| s.abs() < 1e-12),
            "stale tail-stage state leaked through re-enable"
        );
    }

    #[test]
    fn long_ir_survives_rebind_and_channel_resize() {
        let taps = [(0usize, 1.0), (10_000, 0.5)];
        let h = sparse_ir(10_001, &taps);
        let mut e = ConvolutionEngine::new(2, 48_000.0);
        e.load_ir(ir_from_taps(vec![h], 48_000.0)).unwrap();

        e.rebind_sample_rate(96_000.0);
        let t = e.info().unwrap().taps;
        assert!(
            (19_900..=20_100).contains(&t),
            "expected ~20002 taps at 96 k, got {t}"
        );
        e.rebind_sample_rate(48_000.0);
        e.set_channels(4);

        // Back at the original rate + a new width the sparse reference must
        // still hold exactly on every channel.
        let x = noise(24_000);
        let reference = sparse_reference(&x, &taps);
        let mut buf: Vec<f64> = x.iter().flat_map(|&s| [s, s, s, s]).collect();
        buf.extend(std::iter::repeat_n(0.0, 4 * BLOCK));
        e.process(&mut buf, 4);
        for (i, r) in reference.iter().enumerate() {
            for ch in 0..4 {
                assert!(
                    (buf[(i + BLOCK) * 4 + ch] - r).abs() < 1e-6,
                    "ch {ch} mismatch at {i} after rebind+resize"
                );
            }
        }
    }

    /// Manual RT-budget check (`cargo test -p resonance-dsp --release -- \
    /// --ignored worst_case`): the tail work-spreading exists so no single
    /// 256-frame callback pays for the whole delay line. Feed a worst-case
    /// 10 s stereo IR at 96 kHz and require every per-call time to fit well
    /// inside the 2.67 ms real-time window. Ignored in CI: wall-clock timing
    /// is machine-dependent.
    #[test]
    #[ignore = "wall-clock timing; run manually in release"]
    fn worst_case_block_time_fits_rt_budget() {
        let rate = 96_000.0;
        let taps = (MAX_IR_SECONDS * rate) as usize;
        let mut e = ConvolutionEngine::new(2, rate);
        e.load_ir(ir_from_taps(vec![noise(taps)], rate)).unwrap();

        let block: Vec<f64> = noise(2 * BLOCK);
        let mut worst = std::time::Duration::ZERO;
        let mut total = std::time::Duration::ZERO;
        let calls = 4 * RATIO * 8; // several full tail cycles, warmed up
        for _ in 0..calls {
            let mut buf = block.clone();
            let t0 = std::time::Instant::now();
            e.process(&mut buf, 2);
            let dt = t0.elapsed();
            worst = worst.max(dt);
            total += dt;
        }
        let budget = std::time::Duration::from_micros(2666);
        eprintln!(
            "10 s stereo IR @96 kHz: mean {:?}, worst {:?} (budget {budget:?})",
            total / u32::try_from(calls).unwrap(),
            worst
        );
        assert!(
            worst < budget,
            "worst per-block time {worst:?} exceeds the RT budget {budget:?}"
        );
    }

    #[test]
    fn resample_ir_bypass_and_ratio() {
        let x = noise(1000);
        let same = resample_ir(&x, 48_000.0, 48_000.0);
        assert_eq!(same.len(), x.len());
        let up = resample_ir(&x, 48_000.0, 96_000.0);
        assert_eq!(up.len(), 2000);
        let down = resample_ir(&x, 96_000.0, 48_000.0);
        assert_eq!(down.len(), 500);
    }
}

#[cfg(test)]
mod downsample_repro {
    use super::*;

    #[test]
    fn resample_ir_downsampling_preserves_a_short_delta() {
        // A centred delta in a short IR at 96 kHz, downsampled to 48 kHz, must
        // still be a ~unit-DC-gain kernel with its energy inside the window.
        let mut delta = vec![0.0; 64];
        delta[32] = 1.0;
        let out = resample_ir(&delta, 96_000.0, 48_000.0);
        assert_eq!(out.len(), 32);
        let dc: f64 = out.iter().sum();
        let peak = out.iter().copied().fold(0.0f64, |a, b| a.max(b.abs()));
        assert!(
            (dc - 1.0).abs() < 0.05,
            "DC gain should stay ~1.0 after 96k→48k, got {dc:.4} (peak {peak:.4})"
        );
    }
}

#[cfg(test)]
mod engine_downsample_repro {
    use super::*;

    #[test]
    fn engine_with_96k_delta_ir_at_48k_is_near_transparent() {
        let ir = std::sync::Arc::new(IrData {
            name: "d".into(),
            path: "/d.wav".into(),
            sample_rate: 96_000.0,
            channels: vec![{
                let mut v = vec![0.0; 64];
                v[32] = 1.0;
                v
            }],
        });
        let mut e = ConvolutionEngine::new(2, 48_000.0);
        e.load_ir(ir).unwrap();
        // Steady 1 kHz tone: output RMS must be ≈ input RMS (unit DC-ish gain).
        let n = 8192usize;
        let tone: Vec<f64> = (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * 1000.0 / 48_000.0 * i as f64).sin() * 0.25)
            .collect();
        let mut buf: Vec<f64> = tone.iter().flat_map(|&s| [s, s]).collect();
        e.process(&mut buf, 2);
        let rms = |v: &[f64]| (v.iter().map(|x| x * x).sum::<f64>() / v.len() as f64).sqrt();
        let (a, b) = (rms(&tone[2048..]), rms(&buf[4096..]));
        let gain_db = 20.0 * (b / a).log10();
        assert!(
            gain_db.abs() < 1.0,
            "96k delta IR at 48k engine should be ~0 dB, got {gain_db:.2} dB"
        );
    }
}
