# Per-band Listen / Bandpass — design

## Context

Per-band **solo** shipped 2026-07-03 (PR #60): a transient chain flag
(`ProcessorChain::solo: Option<usize>`) that bypasses every band but one so you
hear that band's effect on the full signal. The ROADMAP filed **listen/bandpass**
as its "richer follow-up": hear *only the frequency region a band covers* — a
band-pass audition (FabFilter-style) — rather than the whole signal with the
other bands bypassed.

Decision (this design): rather than add a second parallel flag, **generalize solo
into one transient _audition_ with two modes** (Solo | Listen). The `solo` field,
`Command::SetBandSolo`, `DaemonState.solo_band`, the CLI `band-solo` verb, and the
APO v8 `solo_band` slot are all ~hours old and have no external consumers
(v0.8.0, version-locked IPC), so they are **renamed** cleanly to the audition
model — not kept as dead aliases.

## Semantics

A single **transient audition**, one band at a time, with two modes:

- **Solo** — bypass every other band; run only the soloed band (today's shipped
  behavior, unchanged).
- **Listen** — bypass *all* bands and insert one *audition filter* that isolates
  the target band's operating region, at **unity gain** (you hear the raw content
  in that region regardless of the band's boost/cut — the point is deciding
  *where* to place a band).

Both modes:
- Transient: never written to a `Profile`; cleared on release.
- Auto-cleared by the daemon on any band-table edit (a stale index must not
  outlive its band) — the existing `clear_solo_if_active` guard, renamed
  `clear_audition_if_active`.
- **Force the IIR path** (suspend linear-phase for the audition's duration,
  kernel retained → fades back on clear), exactly as solo does today.
- Isolate only among the EQ bands — effects / convolution / crossfeed / dither
  still run (solo parity; the user can disable effects independently).

### Type-aware audition filter (Listen mode)

The audition filter reuses the existing `FilterType` variants, built at the
**band's own Fc and Q** (unity gain):

| Band type                                        | Audition filter        |
| ------------------------------------------------ | ---------------------- |
| Peaking, Notch, AllPass, BandPass                | `BandPass` @ Fc, Q     |
| LowShelf / LowShelf12Db / LowShelfQ, LowPass(Q)  | `LowPass` @ Fc         |
| HighShelf / HighShelf12Db / HighShelfQ, HighPass(Q) | `HighPass` @ Fc     |

Rationale: peaking-style bands act on a region around Fc → a band-pass tracks it
(width from Q). Shelves and pass filters act on a *half*-band on one side of the
corner → a low-/high-pass at Fc auditions that side. All targets map to a filter
type the engine already builds, so Listen adds a *selection*, not new DSP math.

Q handling: the audition filter takes the band's Q for `BandPass`; `LowPass` /
`HighPass` use the plain (Butterworth) variant so the audition edge is neutral
regardless of the band's stored Q. Fc is clamped to the realizable range at the
live rate (same guard the bands use); if the audition filter can't be built
(unrealizable Fc), none is set — Listen then just bypasses all bands (the region
isn't auditioned), never panics.

## Architecture

### DSP (`crates/resonance-dsp/src/chain.rs`, `filter.rs`)

- New types (in `chain.rs` or a small module):
  ```rust
  pub enum AuditionMode { Solo, Listen }
  pub struct BandAudition { pub band: usize, pub mode: AuditionMode }
  ```
- Replace `ProcessorChain::solo: Option<usize>` with
  `audition: Option<BandAudition>` (default `None`, runtime-only, not in the
  builder, never persisted).
- Add a cached `audition_filter: Option<ApoFilter>` — the prepared Listen filter
  (built off the RT hot path when the audition is set/changed, from the target
  band via the type-aware mapping). `None` in Solo mode / no audition.
- `set_audition(&mut self, a: Option<BandAudition>)` replaces `set_solo`: stores
  the audition and, for Listen, builds `audition_filter` from `filters[band]`
  (clamped to the live rate); for Solo/None clears it.
- `process()`:
  - The FIR gate gains `&& self.audition.is_none()` (both modes force IIR),
    replacing the current `self.solo.is_none()`.
  - The cascade skip becomes: with an audition set, skip every band whose index
    ≠ `audition.band` (same as today). In **Solo** the surviving band runs as
    normal. In **Listen** the surviving band is *also* skipped and, after the
    cascade, the cached `audition_filter` runs once over the buffer (per-channel,
    mask ALL) in place of the band.
  - Because `audition_filter` is a plain `ApoFilter`, its per-channel biquad
    state lives inside it; rebuilt fresh on each `set_audition`, so no stale
    state across auditions.

### Wire (`resonance-ipc`, `resonance-daemon`)

- `resonance-ipc`: `AuditionMode` + `BandAudition` (serde, postcard-stable).
  Replace `Command::SetBandSolo{index}` with
  `Command::SetBandAudition{index:Option<usize>, mode:AuditionMode}` (still
  appended last — the SetBandSolo ordinal slot is reused, wire is version-locked).
  Replace `DaemonState.solo_band:Option<usize>` with
  `audition:Option<BandAudition>` (`#[serde(default)]`, appended last).
- `resonance-daemon`: `AudioCommand::SetBandSolo` → `SetBandAudition(Option<BandAudition>)`;
  `handle_set_band_solo` → `handle_set_band_audition` (validates index in range,
  rejects out-of-range so a stray command can't mute all bands); `apply_command`
  arm calls `chain.set_audition(..)`; snapshot publishes `audition`;
  `clear_solo_if_active` → `clear_audition_if_active` (same band-mutating-command
  guard). Solo stays out of `Profile` (unchanged — `Profile::from_state` never
  reads it).

### Windows APO (`crates/resonance-apo/src/state.rs`)

- Snapshot **v8 → v9**: add `audition_mode: u32` (0 = Solo, 1 = Listen) beside the
  existing `solo_band: u32` index slot (`SOLO_NONE` still = no audition). Encode
  from `chain.audition`; decode in `apply_to` / `build_chain` → `set_audition`,
  which builds the same type-aware audition filter inside the APO chain. Reuse a
  reserved pad for `audition_mode` if one is free (no size growth), else append.

## UIs (cycle one control)

- **CLI** (`resonance-cli`): `resonance audition <index> <solo|listen>` and
  `audition <index> off` / `audition off`. `band-solo` verb removed (renamed).
  Status shows the audition mode + band (the existing `SOLO band N` badge becomes
  `SOLO band N` / `LISTEN band N`).
- **GUI** (`resonance-gui`): the shipped ear-icon toggle becomes a **cycle**:
  Off → Solo → Listen → Off. The icon tints accent when active; a tiny `S`/`L`
  overlay or the tooltip communicates the current mode. Sent via `queue` (not
  `queue_edit`) — still transient. Reuse `Icon::Solo`; add a distinct look or a
  second `Icon::Listen` glyph if the S/L overlay reads poorly.
- **TUI** (`resonance-tui`): `L` cycles Off → Solo → Listen → Off on the selected
  band. Row tag shows `solo` / `listen`; the status badge shows the mode. Help +
  footer updated. No new pref gate (transient action on existing rows).

## Testing / verification

- **DSP unit** (`chain.rs`): 
  - Listen on a peaking band @1 kHz over a broadband/multi-tone input → energy
    survives near 1 kHz, far probes (e.g. 200 Hz, 8 kHz) strongly attenuated.
  - Listen on a low-shelf → low-pass audition: lows pass, highs cut.
  - Listen on a high-shelf → high-pass audition: highs pass, lows cut.
  - Solo mode still isolates by bypass (existing `solo_isolates_a_single_band`
    retargeted to the audition API).
  - Clearing the audition restores the full cascade.
- **IPC** round-trip: `SetBandAudition` (Solo, Listen, off) + `DaemonState.audition`.
- **APO** v9 round-trip: `from_chain` → `apply_to`/`build_chain` preserves band +
  mode; sentinel = no audition.
- **CLI** parse: `audition <idx> solo|listen|off`, 1-based index, bad input bails.
- **Live**: macOS M2 `resonance verify` (Listen → only the band's region survives
  the FR); Windows real-MSVC build + `resonance-apo` tests; Linux `make check` +
  windows-msvc cross-clippy. Windows in-graph audiodg loopback deferred (same
  snapshot-mirror mechanism, needs the DLL redeploy).

## Critical files

- `crates/resonance-dsp/src/chain.rs` — audition field + filter, cascade, FIR gate.
- `crates/resonance-dsp/src/filter.rs` — reuse `ApoFilter` for the audition filter
  (no new filter math; the mapping picks an existing `FilterType`).
- `crates/resonance-ipc/src/lib.rs` — `AuditionMode`/`BandAudition`,
  `SetBandAudition`, `DaemonState.audition`.
- `crates/resonance-daemon/src/{state.rs, ipc_server.rs, audio/mod.rs}` — command,
  handler, RT arm, snapshot, clear-on-edit guard.
- `crates/resonance-apo/src/state.rs` — snapshot v9 (`audition_mode`), apply.
- UIs: `resonance-cli/src/main.rs`, `resonance-gui/src/ui/{bands.rs, icons.rs}`,
  `resonance-tui/src/{main.rs, app.rs, ui.rs}`.
- Docs: `docs/ROADMAP.md` (move listen to shipped), `CLAUDE.md` (audition modes).

## Non-goals (v1)

- No persistence of the audition (transient by design).
- No multiple simultaneous auditions.
- No separate audition Q/width control — Listen uses the band's own Fc/Q via the
  type-aware mapping.
