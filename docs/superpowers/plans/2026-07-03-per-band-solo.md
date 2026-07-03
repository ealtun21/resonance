# Plan — Per-band solo / listen (all-platform)

## Context

Original pick was per-app EQ (top of `docs/ROADMAP.md`), but exploration proved it
cannot be full-parity: Windows' APO is endpoint-level with no per-session DSP hook, so
per-app EQ would be macOS+Linux only. Per the parity-first requirement, per-app EQ is
dropped and must be removed from the ROADMAP backlog.

Next feature must work on all three platforms. Assessment of the remaining ROADMAP
features:
- Input / source selection — ruled out: parity-risky. The whole capture architecture is
  output/render-bound (Linux null-sink monitor, macOS process-tap on output, Windows
  render-endpoint APO); EQ'ing an arbitrary input/source doesn't map cleanly to
  macOS/Windows.
- Per-band solo / listen — chosen: guaranteed all-platform. Pure `resonance-dsp`
  ProcessorChain change plus a UI toggle. The same ProcessorChain runs in the Linux
  daemon, the macOS daemon, and inside the Windows APO (the APO links the resonance-dsp
  engine staticlib), so one DSP change lights up all three once the solo flag rides the
  config wire.

Feature: while tuning, audition one band in isolation — an ear/solo toggle per band.
Small DSP + a UI affordance (ROADMAP: "eye icon or listen ear icon").

## Scope (v1)

- Per-band SOLO = a transient chain flag: when band i is soloed, the chain processes
  only band i and bypasses every other band, so you hear exactly what that one band
  does. Transient (not saved to profile, no undo, cleared on release) — same "don't
  dirty the EQ" treatment per-app volume uses (GUI `queue` not `queue_edit`; TUI no
  `push_undo`).
- Not persisted, single-band (one soloed band at a time; toggling another moves the
  solo; toggling the same one clears it).
- Richer "LISTEN" (bandpass audition) — hearing only the frequency region a band covers
  (FabFilter-style) — is a follow-up, not v1. v1 solo (bypass-others) is the "small DSP"
  the ROADMAP describes.

## Architecture

One transient solo: `Option<usize>` on ProcessorChain, plumbed the same way band edits
already are, plus one Windows-APO wire-schema bump.

- DSP (`crates/resonance-dsp/src/chain.rs`): add `solo: Option<usize>` to the struct
  (default None, not in the builder — set at runtime only). In the band cascade loop,
  skip any band whose index ≠ the soloed one:
  `if let Some(s) = self.solo { if idx != s { continue; } }`. (Enumerate the loop for
  the index.) Effects/convolution/crossfeed/dither still run — solo isolates among the
  EQ bands. A soloed but `enabled==false` band is temporarily audible (that's the point
  of auditioning).
- Linear-phase interaction: when `phase_mode == linear`, static bands are baked into the
  FIR kernel (`eq_fir`), so a cascade skip wouldn't isolate them. While a band is soloed,
  force the IIR path (treat as if FIR inactive) so the skip works directly — document
  that solo suspends linear-phase for the duration. Simpler than re-rendering a one-band
  FIR.
- Transient command: new `Command::SetBandSolo { index: Option<usize> }` — appended last
  in `resonance-ipc/src/lib.rs` (after SetPhaseMode; postcard ordinal rule), mirrored by
  an `AudioCommand::SetBandSolo` (state.rs). Handler `handle_set_band_solo` in
  ipc_server.rs (mirror handle_set_phase_mode), pushed over the existing rtrb ring
  (state.rs send). Do NOT write it into the persisted shadow the way band edits do — keep
  it out of Profile/snapshot so it never lands in a saved profile. Publish current solo
  on DaemonState (a `solo_band: Option<usize>`, `#[serde(default)]`, appended last) so
  clients render the active toggle.
- RT install: apply_command arm sets `chain.solo` (audio/mod.rs). Trivial — no
  allocation.
- Windows APO: the solo flag must reach the APO's ProcessorChain. Add it to the APO state
  written over the seqlock shmem (`crates/resonance-apo/src/state.rs`, apo_state.bin) —
  bump the snapshot version (v7 → v8) and apply solo in the APO's chain exactly like the
  daemon. The daemon already mirrors commands to apo_writer; solo rides the same mirror.

## Execution increments

1. DSP + unit tests (resonance-dsp): solo field + cascade skip + force-IIR-while-soloed.
   Test: a chain with two peaking bands; soloing band 0 leaves band 1's frequency
   unaffected and vice-versa (assert on process output magnitude). Cheapest, fully
   offline.
2. Daemon command/RT/state (resonance-ipc + resonance-daemon): SetBandSolo command +
   handler + AudioCommand + apply_command arm + DaemonState.solo_band. Keep it out of
   Profile. Daemon unit test (IPC round-trip).
3. Windows APO (resonance-apo): state v8 with solo, applied in the APO chain. Cross-clippy
   `--target x86_64-pc-windows-msvc` green.
4. UIs (all three, small): a solo/ear toggle per band.
   - CLI: `resonance band solo <index|off>` (new sub, mirror the band-* subs).
   - GUI: ear-icon toggle in the band row (bands.rs band_row_cells, reuse kit::toggle;
     send via `queue`, not `queue_edit`); highlight the soloed row. Optional: a solo
     affordance on the curve node (curve_view.rs draw_band_nodes).
   - TUI: an `s`-key solo on the selected band in Panel::Bands/Panel::Graph (main.rs key
     dispatch; render a solo marker in the band row ui.rs).
   - Progressive disclosure: no new pref needed (solo is a transient action on existing
     band rows).
5. Safety/UX: auto-clear solo on: band removed, band deselected (TUI/GUI focus change is
   optional), client disconnect (so a crashed GUI can't leave audio stuck on one band —
   clear solo if the controlling client drops, or expose it prominently + clear on any
   non-solo band command). Show the solo state clearly in status and the UIs so it's
   never silently active.
6. Docs: update `docs/ROADMAP.md` — remove Per-app EQ (not full-parity feasible; note
   why), move Per-band solo/listen to "already ahead" (v1 shipped), and note the
   "listen/bandpass" follow-up. Update CLAUDE.md DSP section (solo suspends
   linear-phase).

## Critical files

- `crates/resonance-dsp/src/chain.rs` — solo field, cascade skip, FIR gate.
- `crates/resonance-ipc/src/lib.rs` — Command::SetBandSolo append, DaemonState.solo_band,
  ordinal rule.
- `crates/resonance-daemon/src/state.rs` — AudioCommand::SetBandSolo, ring send; keep out
  of Profile/snapshot.
- `crates/resonance-daemon/src/ipc_server.rs` — handle_set_band_solo (mirror
  handle_set_phase_mode).
- `crates/resonance-daemon/src/audio/mod.rs` — apply_command arm.
- `crates/resonance-apo/src/state.rs` — state v7→v8 with solo, applied in the APO chain.
- UIs: `resonance-cli/src/main.rs`, `resonance-gui/src/ui/bands.rs` (+ curve_view.rs),
  `resonance-tui/src/{main.rs,ui.rs,app.rs}`.
- Docs: `docs/ROADMAP.md` (remove per-app EQ), `CLAUDE.md`.

## Verification

- Unit (make check): DSP solo isolation (two-band chain: solo each, assert only that
  band's FR survives); SetBandSolo IPC round-trip; solo never appears in a saved Profile.
  Green on Linux + windows-msvc cross-clippy.
- Linux live (restest rig, isolated restest uid 30011, null sink testout): load a
  multi-band EQ, `resonance band solo 2`, then `resonance verify --compare` (PR #59)
  baseline(all-bands) vs soloed → only band 2's response remains (other bands' peaks/dips
  gone). `band solo off` restores.
- macOS live (M2): same, via the daemon (cert-signed, verify --compare); confirm the
  APO/daemon chain honors solo.
- Windows live (dockur VM, v8 APO): rebuild+reregister the APO (v8), band solo via the
  daemon, verify --compare through the WASAPI loopback shows only the soloed band.
- UI: GUI ear toggle solos + highlights + clears; TUI `s`; CLI round-trips; status shows
  solo.
- Client-drop safety: kill the GUI while a band is soloed → solo auto-clears (audio not
  stuck).
