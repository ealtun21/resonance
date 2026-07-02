# Linear-phase EQ mode — design

Date: 2026-07-03
Status: approved for autonomous execution (user delegated task choice; review
gates waived per the standing autonomous-run precedent — single squash-merge,
trivially revertable)

## Context

Backlog item 8's last open feature (ROADMAP medium-value): an EQ path with no
phase rotation, for mixing/critical listening. Implemented by rendering the
static filter bank's magnitude response to a **symmetric FIR kernel** and
convolving through the existing non-uniform partitioned engine
(`resonance-dsp/src/convolution.rs`), exactly as the ROADMAP suggests.

## Semantics (locked)

- New chain-level mode: `PhaseMode { Minimum (default), Linear }`.
  `Minimum` = today's biquad path, bit-exact regression.
- In `Linear` mode the **linearizable** bands are removed from the IIR pass and
  realised as one FIR kernel per channel:
  - linearizable = `enabled && realizable && scope == Stereo && dynamics: none`
    (any band type — shelves/HP/LP/peaking/notch/…, cascades included);
  - **mask-aware per-channel render**: channel `k`'s kernel is the cascaded
    magnitude response of every linearizable band whose mask contains `k`
    (the engine already supports per-channel IRs);
  - non-linearizable bands (Mid/Side scope, dynamics) stay on the IIR path —
    a documented hybrid. Dynamic EQ is inherently level-dependent and cannot
    be linear-phase; M/S needs its own M/S kernel pair (deferred, YAGNI).
- Kernel synthesis (dsp): sample the cascaded biquad magnitude `|H|` on an
  `N`-point FFT grid (`N = 16384` at ≤ 48 kHz, ×2 at 96 kHz, ×4 at 192 kHz —
  keeps ~2.9 Hz resolution), build the zero-phase (real) spectrum, inverse FFT,
  rotate the peak to the centre (`N/2` shift) and apply a Hann window →
  `N`-tap symmetric FIR. Latency = `N/2` samples + the engine's fixed
  `BLOCK` (256) — ~171 ms at 48 kHz, reported like the IR latency.
- Preamp stays a scalar (phase-neutral). Effects/convolution unchanged; the
  EQ FIR runs where the filter bank runs (before the user-IR convolution) via
  a **second** `ConvolutionEngine` instance (`eq_fir`).
- Kernel length is fixed (no user knob, v1).

## Rebuild flow

- The RT path never allocates: the daemon (IPC thread) re-renders the kernel
  on every band-affecting command **while linear mode is on** (and on mode
  entry), then swaps it in via a prepared-engine `AudioCommand` — the same
  pattern as `SetConvolution`. Render cost is one 16 k FFT per channel
  (< 1 ms release).
- Rate/channel changes: RT `rebind_sample_rate`/`set_channels` re-prepare from
  the retained kernel source, mirroring the user-IR engine; the daemon also
  re-renders on the next command (grid length is rate-dependent).
- Windows APO: `ChainSnapshot` carries `phase_mode` (v7). The APO's existing
  worker thread renders the kernel off-RT from the snapshot's band table using
  the same dsp function, and the lock path attaches it (IR-blob worker
  precedent). Render failure = fall back to minimum phase (never silence).

## Model / IPC

- dsp: `PhaseMode` on `ProcessorChain` + `render_linear_kernel(filters, n, sr,
  channels) -> IrData`-style pure function in a new `linphase.rs` (kernel
  synthesis is its own unit; convolution.rs stays transport).
- ipc: `Command::SetPhaseMode { linear: bool }` (appended), `DaemonState.
  phase_mode: bool` + latency surfaced, `Profile.phase_mode` with
  `#[serde(default)]` (old profiles = minimum).
- CLI: `resonance phase linear|minimum|min` + status line (`phase linear
  (+171.0 ms)` when active). GUI: toggle in the Settings dialog (not a per-band
  control — chain-level). TUI: Preferences-adjacent chain toggle + status
  badge; keys chosen from what's free at wiring time.
- `resonance verify`: linear mode changes group delay, not magnitude — the
  static FR prediction still holds. Pitch check unaffected. No mode-pick
  change; add a settle note only if live runs show the FIR tail needs it.

## Tests (offline, deterministic)

dsp (`linphase.rs` + `chain.rs`):
1. Kernel is exactly symmetric (`h[i] == h[N-1-i]`) → linear phase by
   construction.
2. Magnitude match: FIR response within ±0.25 dB of the IIR cascade response
   at probe frequencies across the band set (peaking/shelf/HP mix), away from
   the resolution floor (≥ 40 Hz).
3. Group delay flat: cross-correlation peak of a filtered sine burst sits at
   `N/2 + BLOCK` for low AND high frequency (equal delay = no rotation).
4. `Minimum` mode bit-exact to today's output (regression).
5. Hybrid split: a dynamics band + a static band in linear mode — static band
   realised in FIR (removed from IIR), dynamic band still morphs (reuse the
   dynamics chain test with mode on).
6. Mask-aware: band masked to ch0 only shapes ch0's kernel.
7. Rate rebind 48k→96k: grid rescales, magnitude still matches, no NaN.
8. Mode off→on→off returns bit-exact to minimum path (engine cleared).

ipc/daemon: command + profile round-trip incl. serde default; daemon test that
a band edit in linear mode swaps a fresh kernel (taps change when a band
changes); APO snapshot round-trip of `phase_mode`.

## Verification gates

Same as dynamic EQ: Linux `make check`, cross-clippy 1.96 msvc + darwin,
Windows VM (`--test-threads=1` for apo), macOS suite + clippy, PR → merge on
green, watch post-merge `windows-installer` via `gh run list` conclusions.
Live Linux check (release daemon, current window): `resonance phase linear` →
`verify` predicted-mode PASS (magnitude unchanged) + latency line appears;
run only after confirming ambient silence via the meters (user request:
no audible/conflicting tones; if the `restest` rig is ready by then, run it
there instead).
