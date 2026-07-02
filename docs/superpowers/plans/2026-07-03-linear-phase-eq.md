# Linear-Phase EQ Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `PhaseMode::Linear` renders the static stereo filter bank to per-channel symmetric FIR kernels convolved by the existing partitioned engine — no phase rotation.

**Architecture:** New pure `linphase.rs` synthesiser in resonance-dsp (bank magnitude on an FFT grid → zero-phase IFFT → centre shift → Hann → `IrData`), a second `ConvolutionEngine` (`eq_fir`) on `ProcessorChain` running where the filter bank runs, daemon-side re-render + prepared-engine swap on band edits (SetConvolution pattern), APO worker render (IR-blob precedent), `SetPhaseMode` appended command.

**Tech Stack:** Rust workspace; rustfft (already a dep). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-07-03-linear-phase-eq-design.md` — semantics, linearizable-band rule, kernel sizing, test list.

## Global Constraints

- Conventional Commits lowercase, no AI trailers; `make check` before commit.
- Live daemon: only touch after ambient-silence check (meters), or on the `restest` rig if ready.
- Postcard append-only for `Command`; `Profile`/`BandState` serde defaults for disk back-compat.
- `Minimum` mode must stay bit-exact (regression tests exist).
- Linearizable = `enabled && realizable && scope==Stereo && dynamics none`; kernel N = 16384 × (rate/48k rounded up to power of two ≥ 1).

### Task 1: dsp — `linphase.rs` kernel synthesis (TDD)
Files: create `crates/resonance-dsp/src/linphase.rs`, register in `lib.rs`.
Produces: `pub fn grid_len(sample_rate: f64) -> usize`; `pub fn render(filters: &[ApoFilter], channels: usize, sample_rate: f64) -> Option<IrData>` (None when no linearizable band); `pub fn is_linearizable(f: &ApoFilter) -> bool`; biquad-cascade magnitude eval helper `|H(e^jω)|` from `BiquadCoeffs` (head + extra sections).
Tests: kernel symmetry; magnitude match ±0.25 dB vs IIR bank (peaking + 24 dB/oct shelf + HP mix, probes ≥ 40 Hz); mask-aware per-channel kernels; skips M/S + dynamic bands; empty → None; grid doubles at 96 k.
Commit: `feat(dsp): linear-phase fir synthesis from the filter bank`.

### Task 2: dsp — `PhaseMode` on the chain (TDD)
Files: `chain.rs` (+`filter.rs` only if a helper is needed).
Produces: `pub enum PhaseMode { Minimum, Linear }` + `ProcessorChain.phase_mode` + `pub eq_fir: ConvolutionEngine`; in `process`, when Linear: skip linearizable filters in the IIR loop (`is_linearizable`), run `eq_fir.process` straight after the filter loop (before `convolution`); `set_phase_mode(&mut self, PhaseMode)` clears `eq_fir` when leaving Linear; `rebind_sample_rate`/`set_channels` forward to `eq_fir`.
NOTE: the chain does NOT self-render (RT allocation ban) — `eq_fir` is loaded by the daemon/APO with a rendered kernel via `load_ir`.
Tests: spec tests 3 (group delay flat via burst cross-correlation at 200 Hz vs 8 kHz), 4 (Minimum bit-exact), 5 (hybrid dynamics), 8 (off→on→off bit-exact); latency = N/2 + BLOCK.
Commit: `feat(dsp): phasemode with fir bank path in the chain`.

### Task 3: ipc + daemon (TDD)
Files: `resonance-ipc/src/lib.rs` (append `Command::SetPhaseMode { linear: bool }`; `DaemonState.phase_mode: bool` + `eq_fir_latency_frames: usize`; round-trip tests), `resonance-daemon/src/config.rs` (`Profile.phase_mode` serde default false; `into_chain` sets mode only — kernel rendered by the caller), `state.rs` (AudioCommand::SetEqFir(Box<ConvolutionEngine>) + SetPhaseMode; snapshot fields), `audio/mod.rs` (arms: install engine w/ rebind like SetConvolution; set mode), `ipc_server.rs` (`handle_set_phase_mode`; a shared `re_render_eq_fir(state)` called by every band-mutating handler + profile-apply when linear — renders at meters' LIVE rate (#53 lesson) off the shadow band table, sends SetEqFir; `info!` logs).
Tests: profile round-trip + legacy default; band edit in linear mode swaps a kernel with different taps; mode off clears.
Commit: `feat(daemon): linear phase mode with live kernel re-render`.

### Task 4: Windows APO parity
Files: `resonance-apo/src/state.rs` (+`worker.rs`/`ffi.rs` as found): `ChainSnapshot.phase_mode: u32`, `STATE_VERSION` 6→7; worker thread renders via `linphase::render` from the snapshot-built filters at the engine rate, attaches like the IR-blob kernel; render failure → Minimum. Snapshot round-trip test + `--test-threads=1` suite.
Commit: `feat(apo): linear phase mode via worker-rendered kernel`.

### Task 5: front-ends
CLI: `resonance phase <linear|minimum|min>` + status `phase linear (+171.0 ms)` line; parse tests. GUI: Settings-dialog toggle (chain-level; latency shown beside it) — no per-band UI. TUI: settings/preferences chain toggle + status badge + help. Subagents mirror dynamic-EQ wiring shape; progressive disclosure NOT needed (single toggle, but keep copy honest about latency).
Commit(s): `feat(cli|gui|tui): linear phase toggle`.

### Task 6: docs + gates + PR
ROADMAP: move linear-phase to shipped (closes item 8 entirely). `make check`; cross-clippy 1.96 msvc+darwin; Windows VM + macOS detached-SHA runs; live Linux check per spec (silence-gated or restest rig); PR → squash-merge on green; watch post-merge `windows-installer` conclusions; update memory.
