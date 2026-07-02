# Non-uniform partitioned convolution (issue #41)

## Problem

The convolution engine uses uniform 256-sample partitions; per-block cost grows
linearly with IR length, so IRs are capped at `MAX_IR_SECONDS = 2.0`. Church /
hall reverb IRs and some room corrections exceed 2 s.

## Design: two-stage Gardner partitioning

Split the prepared IR `h` (post-resample, post-cap) at `HEAD_LEN = 8192` taps:

- **Head stage** — unchanged uniform `BLOCK = 256` partitions (FFT 512,
  overlap-save) covering `h[0..8192)`: at most 32 partitions. Keeps the fixed
  `BLOCK`-frame latency exactly as today.
- **Tail stage** — uniform `TAIL_BLOCK = 4096` partitions (FFT 8192,
  overlap-save) covering `h[8192..)`. One tail cycle = `RATIO = 16` head
  blocks.

`HEAD_LEN = 2 × TAIL_BLOCK` is load-bearing: the contribution of tail input
block `B` through tail partition 0 first affects output cycle `B + 2`, and
block `B` is only complete at the end of cycle `B`. That leaves exactly one
full cycle (16 head-block callbacks) to compute each tail output segment, so
the tail work is **spread across the 16 sub-blocks** instead of spiking:

- phase 0: slide tail window, forward FFT of the just-completed block,
  rotate tail FDL, zero the tail accumulator
- phases 0..=15: MAC `div_ceil(P_tail, 16)` partitions each
- phase 15: inverse FFT, write the next cycle's tail segment, swap buffers

Per-channel tail state: collect buffer (4096), overlap-save window (8192),
FDL, accumulator (8192), current + next output segments (4096 each), phase
counter. Channels with an IR ≤ 8192 taps allocate **no tail state** — the
short-IR path is byte-for-byte today's engine (modulo ≤32 head partitions).

Head output and the precomputed tail segment are summed per sub-block before
being pushed to the `ready` FIFO. Latency stays `BLOCK`. No worker thread —
deterministic, allocation-free RT path, safe inside audiodg.exe (APO).

## Cap and cost

`MAX_IR_SECONDS`: 2.0 → **10.0**.

| IR @ 96 kHz | today (uniform 256) | new (head 32 + tail 4096) |
|---|---|---|
| 2 s | ~1500 cMAC/sample/ch | ~410 (≈3.7× cheaper) |
| 10 s | rejected | ~530 cMAC/sample/ch |

Memory: FDL + kernel ≈ 32 B/tap/ch each → 10 s @ 96 kHz stereo ≈ 90 MB.
Documented; a `realfft` (real-spectrum) follow-up can halve it later.

## Not changing

Public API (`load_ir`, `process`, `info`, `latency_frames`, `rebind_sample_rate`,
`set_channels`, `reset`), IPC protocol, IR blob format (APO), latency, WAV
loading. `ConvolutionInfo.taps` keeps its meaning.

## Testing

- Unit (resonance-dsp): sparse-delta long IR vs analytic reference (exact
  alignment across the 8192 boundary); dense 10 k-tap IR vs direct
  convolution; block-size invariance with a long IR; cap = 10 s truncation;
  reset/re-enable clears tail; rebind + set_channels with long IR; all 16
  existing tests unchanged.
- Bench: extend `benches/chain.rs` with 2 s and 10 s IR block cost.
- Integration: daemon IPC test loading a >2 s IR end-to-end.
- Manual: Linux live `resonance verify` A/B with a long IR; Windows VM
  audiodg APO with a long IR; macOS build + full test suite over ssh.
