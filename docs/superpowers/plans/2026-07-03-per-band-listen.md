# Per-band Listen/Bandpass Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generalize the shipped per-band *solo* into a transient per-band *audition* with two modes — **Solo** (bypass other bands) and **Listen** (band-pass the band's operating region at unity gain) — across DSP, IPC, daemon, Windows APO, and all three clients.

**Architecture:** Replace `ProcessorChain::solo: Option<usize>` with `audition: Option<BandAudition>` (`{band, mode}`) plus a cached `audition_filter: Option<ApoFilter>` built from the target band via a type-aware mapping to an existing `FilterType`. The rename rides the same wire/snapshot/UI plumbing solo shipped through (PR #60); APO snapshot bumps v8→v9. No new DSP math — Listen reuses `BandPass`/`LowPass`/`HighPass`.

**Tech Stack:** Rust workspace (resonance-dsp/-ipc/-daemon/-apo/-cli/-gui/-tui), postcard IPC, seqlock shmem for the Windows APO, egui (GUI), ratatui (TUI).

## Global Constraints

- Conventional Commits, all lowercase. No AI-related content anywhere. No Co-Authored-By / AI-attribution trailer (project convention).
- Run `make check` (fmt --check + clippy -D warnings [pedantic] + test --all) before each commit; `make fmt-fix` to apply rustfmt.
- f64 throughout the DSP. Builder pattern for chains. Functional style (iterators/closures) preferred.
- The audition is **transient**: never written to `Profile`, cleared on release, auto-cleared by the daemon on any band-table edit, and it **forces the IIR path** (suspends linear-phase while active).
- postcard IPC wire is non-self-describing + version-locked: new `Command`/`DaemonState` fields append LAST; the reused `SetBandSolo` ordinal slot becomes `SetBandAudition`.
- Windows APO snapshot: bump `STATE_VERSION` on any `#[repr(C)]` change.

---

## File Structure

- `crates/resonance-dsp/src/chain.rs` — `AuditionMode`, `BandAudition`, `ProcessorChain::audition` + `audition_filter`, `set_audition`, `build_audition_filter`, cascade/FIR-gate changes, tests.
- `crates/resonance-ipc/src/lib.rs` — `AuditionMode`, `BandAudition`, `Command::SetBandAudition`, `DaemonState.audition`, round-trip test, 4 DaemonState literals.
- `crates/resonance-daemon/src/state.rs` — `AudioCommand::SetBandAudition`, snapshot `audition`.
- `crates/resonance-daemon/src/ipc_server.rs` — `handle_set_band_audition`, `clear_audition_if_active`, dispatch arm + guard.
- `crates/resonance-daemon/src/audio/mod.rs` — `apply_command` arm.
- `crates/resonance-apo/src/state.rs` — snapshot v9 (`audition_mode`), encode/decode/apply, test.
- `crates/resonance-cli/src/main.rs` — `audition` subcommand (replaces `band-solo`), status badge, parse test.
- `crates/resonance-gui/src/ui/bands.rs` — ear-icon cycle Off→Solo→Listen→Off.
- `crates/resonance-gui/src/ui/icons.rs` — (optional) `Icon::Listen`; else reuse `Icon::Solo` + S/L overlay.
- `crates/resonance-tui/src/{main.rs, app.rs, ui.rs}` — `L` cycle, row tag, status badge, help/footer.
- `docs/ROADMAP.md`, `CLAUDE.md` — move listen to shipped, document audition modes.

---

## Task 1: DSP — audition types, generalize solo→audition, Listen filter

**Files:**
- Modify: `crates/resonance-dsp/src/chain.rs`
- Test: `crates/resonance-dsp/src/chain.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `pub enum AuditionMode { Solo, Listen }` (Debug, Clone, Copy, PartialEq, Eq); `pub struct BandAudition { pub band: usize, pub mode: AuditionMode }` (Debug, Clone, Copy, PartialEq, Eq); `ProcessorChain::audition: Option<BandAudition>` (pub); `ProcessorChain::set_audition(&mut self, a: Option<BandAudition>)`.
- Consumes: existing `ApoFilter`, `FilterType`, `ApoFilter::builder()`, `ApoFilter::process_channel(f64, usize) -> f64`.

- [ ] **Step 1: Add the audition types + fields**

In `chain.rs`, near `PhaseMode` add:

```rust
/// Per-band audition mode. `Solo` bypasses every other band (hear this band's
/// effect on the full signal); `Listen` bypasses ALL bands and runs one
/// band-pass/low-pass/high-pass isolating this band's operating region at unity
/// gain (hear the raw content there, regardless of the band's boost/cut).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditionMode {
    Solo,
    Listen,
}

/// A transient single-band audition: which band, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandAudition {
    pub band: usize,
    pub mode: AuditionMode,
}
```

Replace the `solo` field on `ProcessorChain` (the doc comment + `pub solo: Option<usize>`) with:

```rust
    /// Transient per-band audition (Solo or Listen). Runtime-only: not part of
    /// the builder, never persisted to a `Profile`, cleared on release, and
    /// auto-cleared by the daemon on any band-table edit. Forces the IIR path
    /// (suspends linear-phase for the duration) so the cascade skip isolates the
    /// band directly. Effects/convolution/crossfeed/dither still run — the
    /// audition isolates only among the EQ bands.
    pub audition: Option<BandAudition>,
    /// The prepared Listen filter (band-pass/low-pass/high-pass at the target
    /// band's Fc/Q). Built off the RT hot path by [`ProcessorChain::set_audition`]
    /// when the mode is `Listen`; `None` for Solo / no audition.
    audition_filter: Option<ApoFilter>,
```

In `ProcessorChainBuilder::build`, replace `solo: None,` with:

```rust
            audition: None,
            audition_filter: None,
```

- [ ] **Step 2: Add `set_audition` + `build_audition_filter`, remove `set_solo`**

Replace the `set_solo` method with:

```rust
    /// Set (or clear) the transient per-band audition. For `Listen`, builds the
    /// type-aware audition filter from the target band; for `Solo`/`None` clears
    /// it. Out-of-range indices are accepted verbatim (they mute every band until
    /// cleared); callers validate against the live band count. Transient — never
    /// persisted.
    pub fn set_audition(&mut self, audition: Option<BandAudition>) {
        self.audition_filter = match audition {
            Some(BandAudition { band, mode: AuditionMode::Listen }) => self
                .filters
                .get(band)
                .and_then(|b| build_audition_filter(b, self.channels, self.sample_rate)),
            _ => None,
        };
        self.audition = audition;
    }
```

Add a free function at module scope (below the `impl ProcessorChain` block, near `db_to_linear`):

```rust
/// Build the Listen-mode audition filter for `band`: a unity-gain filter that
/// isolates the band's operating region, using an existing `FilterType`.
/// Peaking-style bands → band-pass at Fc/Q; shelves + pass filters → a plain
/// (Butterworth) low-/high-pass at Fc. `None` if the coefficients are
/// unrealizable at `sr` (Listen then simply bypasses all bands).
fn build_audition_filter(band: &ApoFilter, channels: usize, sr: f64) -> Option<ApoFilter> {
    use crate::filter::FilterType::{
        AllPass, BandPass, HighPass, HighPassQ, HighShelf, HighShelf12Db, HighShelfQ, LowPass,
        LowPassQ, LowShelf, LowShelf12Db, LowShelfQ, Notch, Peaking,
    };
    let (ft, q) = match band.filter_type {
        Peaking | Notch | AllPass | BandPass => (BandPass, band.q),
        LowShelf | LowShelf12Db | LowShelfQ | LowPass | LowPassQ => {
            (LowPass, std::f64::consts::FRAC_1_SQRT_2)
        }
        HighShelf | HighShelf12Db | HighShelfQ | HighPass | HighPassQ => {
            (HighPass, std::f64::consts::FRAC_1_SQRT_2)
        }
    };
    ApoFilter::builder()
        .filter_type(ft)
        .freq(band.freq)
        .gain_db(0.0)
        .q(q)
        .enabled(true)
        .channels(channels)
        .sample_rate(sr)
        .build()
        .ok()
}
```

- [ ] **Step 3: Update the FIR gate + cascade for both modes**

In `process`, change the FIR-want line from `&& self.solo.is_none()` to:

```rust
            && self.audition.is_none()
```

Replace the cascade-skip block (the `let solo = self.solo;` + `if let Some(s) = solo {...}` inside the loop) with:

```rust
        let audition = self.audition;
        for (idx, filter) in self.filters.iter_mut().enumerate() {
            // Transient audition: Solo runs only the target band; Listen skips
            // ALL bands (the audition filter below replaces them). Either way the
            // audition forces the IIR path (fir_active is false).
            if let Some(a) = audition {
                match a.mode {
                    AuditionMode::Solo if idx != a.band => continue,
                    AuditionMode::Solo => {}
                    AuditionMode::Listen => continue,
                }
            }
```

(Keep the existing `if fir_active && crate::linphase::is_linearizable(filter) { continue; }` line immediately after.)

- [ ] **Step 4: Run the Listen audition filter after the cascade**

Immediately after the band-cascade `for` loop closes (before the `if fir_active {` block), add:

```rust
        // Listen mode: the isolated region is auditioned by one filter in place
        // of the (skipped) bands.
        if matches!(
            self.audition,
            Some(BandAudition { mode: AuditionMode::Listen, .. })
        ) {
            if let Some(f) = self.audition_filter.as_mut() {
                let frames = buf.len() / channels;
                for frame in 0..frames {
                    for ch in 0..channels {
                        let i = frame * channels + ch;
                        buf[i] = f.process_channel(buf[i], ch);
                    }
                }
            }
        }
```

- [ ] **Step 5: Retarget the solo test + add Listen tests**

Rename `solo_isolates_a_single_band` and update its `set_solo(Some(i))` calls to `set_audition(Some(BandAudition { band: i, mode: AuditionMode::Solo }))` and `set_solo(None)` to `set_audition(None)`. Then add a Listen test after it:

```rust
    #[test]
    fn listen_bandpasses_a_peaking_band() {
        use crate::filter::FilterType;
        // A +12 dB peak at 1 kHz. In Listen the +12 is irrelevant (unity BP) —
        // the point is: energy survives near 1 kHz, far probes are killed.
        let mk = || {
            ProcessorChain::builder()
                .channels(1)
                .sample_rate(48_000.0)
                .add_filter(band(FilterType::Peaking, 1_000.0, 12.0, 2.0))
                .build()
        };
        let gain_at = |chain: &mut ProcessorChain, hz: f64| -> f64 {
            let n = 8_192usize;
            let w = 2.0 * std::f64::consts::PI * hz / 48_000.0;
            #[allow(clippy::cast_precision_loss)]
            let mut buf: Vec<f64> = (0..n).map(|i| (w * i as f64).sin() * 0.5).collect();
            let input = buf.clone();
            chain.process(&mut buf);
            let rms = |s: &[f64]| (s.iter().map(|x| x * x).sum::<f64>() / s.len() as f64).sqrt();
            rms(&buf[2_048..]) / rms(&input[2_048..])
        };

        let mut c = mk();
        c.set_audition(Some(BandAudition { band: 0, mode: AuditionMode::Listen }));
        // In-band passes ~unity; far out-of-band is strongly attenuated by the BP.
        assert!(gain_at(&mut c, 1_000.0) > 0.5, "1 kHz should pass in Listen");
        assert!(gain_at(&mut c, 100.0) < 0.2, "100 Hz should be cut by the BP");
        assert!(gain_at(&mut c, 10_000.0) < 0.2, "10 kHz should be cut by the BP");
    }

    #[test]
    fn listen_low_shelf_uses_low_pass() {
        use crate::filter::FilterType;
        let mut c = ProcessorChain::builder()
            .channels(1)
            .sample_rate(48_000.0)
            .add_filter(band(FilterType::LowShelf, 500.0, 6.0, 0.707))
            .build();
        let gain_at = |chain: &mut ProcessorChain, hz: f64| -> f64 {
            let n = 8_192usize;
            let w = 2.0 * std::f64::consts::PI * hz / 48_000.0;
            #[allow(clippy::cast_precision_loss)]
            let mut buf: Vec<f64> = (0..n).map(|i| (w * i as f64).sin() * 0.5).collect();
            let input = buf.clone();
            chain.process(&mut buf);
            let rms = |s: &[f64]| (s.iter().map(|x| x * x).sum::<f64>() / s.len() as f64).sqrt();
            rms(&buf[2_048..]) / rms(&input[2_048..])
        };
        c.set_audition(Some(BandAudition { band: 0, mode: AuditionMode::Listen }));
        assert!(gain_at(&mut c, 100.0) > 0.7, "low shelf → LP: 100 Hz passes");
        assert!(gain_at(&mut c, 8_000.0) < 0.2, "low shelf → LP: 8 kHz cut");
    }
```

- [ ] **Step 6: Run the DSP tests**

Run: `cargo test -p resonance-dsp audition_ listen_ solo`
Expected: the retargeted solo test + `listen_bandpasses_a_peaking_band` + `listen_low_shelf_uses_low_pass` all PASS. (If `> 0.5`/`< 0.2` thresholds are off for the codebase's BP Q normalization, adjust the constants — the in-band-passes / out-of-band-cut *relationship* is the assertion.)

- [ ] **Step 7: Commit**

```bash
git add crates/resonance-dsp/src/chain.rs
git commit -m "feat(dsp): generalize band solo into solo/listen audition"
```

---

## Task 2: IPC + daemon — SetBandAudition command, RT, state, guard

**Files:**
- Modify: `crates/resonance-ipc/src/lib.rs`
- Modify: `crates/resonance-daemon/src/state.rs`
- Modify: `crates/resonance-daemon/src/ipc_server.rs`
- Modify: `crates/resonance-daemon/src/audio/mod.rs`
- Test: `crates/resonance-ipc/src/lib.rs` (tests mod)

**Interfaces:**
- Consumes: Task 1's `AuditionMode`, `BandAudition` (re-declared in `resonance-ipc` as serde types; the daemon maps ipc↔dsp).
- Produces: `Command::SetBandAudition { index: Option<usize>, mode: AuditionMode }`; `DaemonState.audition: Option<BandAudition>`; `AudioCommand::SetBandAudition(Option<resonance_dsp::chain::BandAudition>)`.

- [ ] **Step 1: Add serde audition types + command + state field (ipc)**

In `resonance-ipc/src/lib.rs`, add near the other enums:

```rust
/// Per-band audition mode (mirrors `resonance_dsp::chain::AuditionMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditionMode {
    Solo,
    Listen,
}

/// A transient single-band audition (mirrors `resonance_dsp::chain::BandAudition`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BandAudition {
    pub band: usize,
    pub mode: AuditionMode,
}
```

Replace `Command::SetBandSolo { index: Option<usize> }` (the doc comment + variant, the LAST variant) with:

```rust
    /// Transiently audition a single EQ band. `Some(index)` auditions that band
    /// in `mode` (Solo = bypass others; Listen = band-pass the band's region);
    /// `None` clears. Never persisted; suspends linear-phase while active. The
    /// daemon clears any active audition on any band-mutating command as a
    /// stuck-audio guard.
    SetBandAudition {
        index: Option<usize>,
        mode: AuditionMode,
    },
```

Replace `DaemonState.solo_band` (doc + `pub solo_band: Option<usize>`, the LAST field) with:

```rust
    /// Transient per-band audition (`None` = none). Runtime-only — never
    /// persisted; published so clients render the active toggle + mode. Appended
    /// LAST + `serde` default, same compatibility note as `apps`/`sinks`.
    #[serde(default)]
    pub audition: Option<BandAudition>,
```

- [ ] **Step 2: Update the 4 DaemonState literals (ipc)**

In `resonance-ipc/src/lib.rs` test literal, replace `solo_band: Some(1),` with `audition: Some(BandAudition { band: 1, mode: AuditionMode::Listen }),`.

- [ ] **Step 3: Run ipc build to find the other 3 literals**

Run: `cargo build -p resonance-ipc -p resonance-gui -p resonance-tui 2>&1 | grep -E "missing field|solo_band" | head`
Expected: errors at `resonance-gui/src/app.rs`, `resonance-tui/src/ui.rs`. In each, replace `solo_band: None,` with `audition: None,`.

- [ ] **Step 4: Replace the ipc round-trip test**

Rename `band_solo_command_round_trips` → `band_audition_command_round_trips`:

```rust
    #[test]
    fn band_audition_command_round_trips() {
        command_round_trip(&Command::SetBandAudition {
            index: Some(2),
            mode: AuditionMode::Solo,
        });
        command_round_trip(&Command::SetBandAudition {
            index: Some(2),
            mode: AuditionMode::Listen,
        });
        command_round_trip(&Command::SetBandAudition {
            index: None,
            mode: AuditionMode::Solo,
        });
    }
```

- [ ] **Step 5: AudioCommand + snapshot (daemon state.rs)**

Replace `AudioCommand::SetBandSolo(Option<usize>)` (doc + variant) with:

```rust
    /// Transiently audition a single EQ band (`Some`) or clear (`None`). Never
    /// persisted — the shadow chain carries it only so snapshots and the APO
    /// mirror reflect the live audition.
    SetBandAudition(Option<resonance_dsp::chain::BandAudition>),
```

Add the ipc↔dsp mapping helpers near the top of `state.rs` (after the imports):

```rust
fn dsp_audition(a: resonance_ipc::BandAudition) -> resonance_dsp::chain::BandAudition {
    resonance_dsp::chain::BandAudition {
        band: a.band,
        mode: match a.mode {
            resonance_ipc::AuditionMode::Solo => resonance_dsp::chain::AuditionMode::Solo,
            resonance_ipc::AuditionMode::Listen => resonance_dsp::chain::AuditionMode::Listen,
        },
    }
}

fn ipc_audition(a: resonance_dsp::chain::BandAudition) -> resonance_ipc::BandAudition {
    resonance_ipc::BandAudition {
        band: a.band,
        mode: match a.mode {
            resonance_dsp::chain::AuditionMode::Solo => resonance_ipc::AuditionMode::Solo,
            resonance_dsp::chain::AuditionMode::Listen => resonance_ipc::AuditionMode::Listen,
        },
    }
}
```

In `snapshot()`, replace `solo_band: chain.solo,` with:

```rust
            audition: chain.audition.map(ipc_audition),
```

Add `BandAudition` to the `resonance_ipc` import list in `state.rs` if not present.

- [ ] **Step 6: apply_command arm (daemon audio/mod.rs)**

Replace `AudioCommand::SetBandSolo(index) => chain.set_solo(index),` with:

```rust
        AudioCommand::SetBandAudition(a) => chain.set_audition(a),
```

- [ ] **Step 7: Handler + guard + dispatch (daemon ipc_server.rs)**

Replace `handle_set_band_solo` with:

```rust
/// Transiently audition a single EQ band (Solo or Listen), or clear. Out-of-range
/// indices are rejected so a stray command can't silently mute all bands. Never
/// persisted; suspends linear-phase while active.
fn handle_set_band_audition(
    state: &SharedState,
    index: Option<usize>,
    mode: resonance_ipc::AuditionMode,
) -> Response {
    let audition = match index {
        Some(i) => {
            let n = state.0.lock().unwrap().chain.filters.len();
            if i >= n {
                return Response::Error(format!("band index {i} out of range (have {n})"));
            }
            Some(crate::state::dsp_audition(resonance_ipc::BandAudition { band: i, mode }))
        }
        None => None,
    };
    state.send(AudioCommand::SetBandAudition(audition), move |chain| {
        chain.set_audition(audition);
    });
    match (index, mode) {
        (Some(i), resonance_ipc::AuditionMode::Solo) => info!("band audition: solo {i}"),
        (Some(i), resonance_ipc::AuditionMode::Listen) => info!("band audition: listen {i}"),
        (None, _) => info!("band audition cleared"),
    }
    Response::Ok
}
```

Replace `clear_solo_if_active` with:

```rust
/// Stuck-audio guard: clear an active audition before any command that mutates
/// the band table runs, so an audition never survives a band add/remove/edit or a
/// profile load (a stale index could otherwise mute or mis-target audio). No-op
/// when nothing is auditioned.
fn clear_audition_if_active(state: &SharedState) {
    let active = state.0.lock().unwrap().chain.audition.is_some();
    if active {
        state.send(AudioCommand::SetBandAudition(None), |chain| {
            chain.set_audition(None);
        });
    }
}
```

In `dispatch`, replace the `clear_solo_if_active(state)` call with `clear_audition_if_active(state)`. In `dispatch_inner`, replace the arm `Command::SetBandSolo { index } => handle_set_band_solo(state, index),` with:

```rust
        Command::SetBandAudition { index, mode } => handle_set_band_audition(state, index, mode),
```

Make `dsp_audition` reachable from `ipc_server.rs` (it's `crate::state::dsp_audition` — mark it `pub(crate)` in state.rs).

- [ ] **Step 8: Build + test**

Run: `cargo test -p resonance-ipc -p resonance-daemon 2>&1 | grep -E "test result|error" | head`
Expected: all green, `band_audition_command_round_trips` passes.

- [ ] **Step 9: Commit**

```bash
git add crates/resonance-ipc crates/resonance-daemon
git commit -m "feat(daemon): setbandaudition command + solo/listen state"
```

---

## Task 3: Windows APO — snapshot v9 with audition mode

**Files:**
- Modify: `crates/resonance-apo/src/state.rs`
- Test: `crates/resonance-apo/src/state.rs` (tests mod)

**Interfaces:**
- Consumes: Task 1's `chain.audition`, `AuditionMode`, `BandAudition`, `set_audition`.
- Produces: `ChainSnapshot.solo_band` (index slot, unchanged name) + new `audition_mode: u32` (0 = Solo, 1 = Listen).

- [ ] **Step 1: Bump version + add the mode field**

In `state.rs`, bump `STATE_VERSION` 8 → 9 and add a doc line `/// v9: + audition mode (solo/listen) beside the solo_band index.`. Add a field to `ChainSnapshot` right after `pub solo_band: u32,`:

```rust
    /// Audition mode for `solo_band`: 0 = Solo (bypass others), 1 = Listen
    /// (band-pass the band's region). Ignored when `solo_band == SOLO_NONE`.
    pub audition_mode: u32,
```

Add `audition_mode: 0,` to BOTH struct literals (the `Default` impl and the `From<&ProcessorChain>`/`from_chain` builder). If a reserved `_pad` u32 is free adjacent to `solo_band`, reuse it instead of growing the struct; otherwise appending is fine (v9 rejects stale files).

- [ ] **Step 2: Encode from the chain**

Replace the `solo_band: solo_encode(chain.solo),` line in `from_chain` with:

```rust
            solo_band: chain.audition.map_or(SOLO_NONE, |a| audition_index_encode(a.band)),
            audition_mode: chain
                .audition
                .map_or(0, |a| u32::from(a.mode == resonance_dsp::chain::AuditionMode::Listen)),
```

Rename `solo_encode` → `audition_index_encode` (same body; it just clamps a `usize` band index to the `u32` slot with the `SOLO_NONE` guard).

- [ ] **Step 3: Decode helper**

Replace the `solo()` method with:

```rust
    /// Decode the transient audition (`SOLO_NONE` → `None`).
    #[must_use]
    pub fn audition(&self) -> Option<resonance_dsp::chain::BandAudition> {
        (self.solo_band != SOLO_NONE).then_some(resonance_dsp::chain::BandAudition {
            band: self.solo_band as usize,
            mode: if self.audition_mode == 1 {
                resonance_dsp::chain::AuditionMode::Listen
            } else {
                resonance_dsp::chain::AuditionMode::Solo
            },
        })
    }
```

- [ ] **Step 4: Apply in build_chain + apply_to**

Replace `chain.set_solo(self.solo());` (in `build_chain`) and `chain.set_solo(self.solo());` (in `apply_to`) both with:

```rust
        chain.set_audition(self.audition());
```

- [ ] **Step 5: Replace the round-trip test**

Rename `solo_round_trips_and_applies_in_place` → `audition_round_trips_and_applies_in_place`; update `chain.set_solo(Some(1))` → `chain.set_audition(Some(resonance_dsp::chain::BandAudition { band: 1, mode: resonance_dsp::chain::AuditionMode::Listen }))`, assert `snap.audition()` returns `Some({band:1, Listen})`, `fresh.audition` / `rebuilt.audition` match, and clearing → `None`. Also assert `snap.audition_mode == 1` for Listen and `0` for a Solo audition.

- [ ] **Step 6: Test + cross-clippy**

Run: `cargo test -p resonance-apo audition 2>&1 | grep -E "test result|error"`
Then: `cargo clippy --target x86_64-pc-windows-msvc -p resonance-apo -p resonance-daemon -p resonance-ipc -p resonance-dsp 2>&1 | grep -E "^error|warning: [^r]" ; echo done`
Expected: test green; clippy clean (ignore `ring` GNU-compiler warnings — those crates aren't in this set).

- [ ] **Step 7: Commit**

```bash
git add crates/resonance-apo
git commit -m "feat(apo): snapshot v9 carries the audition mode"
```

---

## Task 4: CLI — `audition` subcommand + status

**Files:**
- Modify: `crates/resonance-cli/src/main.rs`
- Test: `crates/resonance-cli/src/main.rs` (tests mod)

**Interfaces:**
- Consumes: `Command::SetBandAudition`, `resonance_ipc::AuditionMode`, `DaemonState.audition`.

- [ ] **Step 1: Replace the `BandSolo` subcommand**

Replace the `BandSolo { target: String }` variant (doc + variant) with:

```rust
    /// Audition one EQ band: `solo` bypasses every other band, `listen`
    /// band-passes the band's frequency region. Transient — never saved;
    /// suspends linear-phase while active. `off` clears the audition.
    Audition {
        /// Band index (1-based, as shown in `status`), or `off` to clear
        target: String,
        /// Mode: solo | listen (default solo). Ignored for `off`.
        mode: Option<String>,
    },
```

- [ ] **Step 2: Replace the handler**

Replace the `Sub::BandSolo { target } => {...}` arm with:

```rust
        Sub::Audition { target, mode } => {
            if matches!(
                target.to_ascii_lowercase().as_str(),
                "off" | "none" | "clear"
            ) {
                return Ok(Command::SetBandAudition {
                    index: None,
                    mode: resonance_ipc::AuditionMode::Solo,
                });
            }
            let index: usize = target
                .parse()
                .map_err(|_| anyhow::anyhow!("band index must be a 1-based number or `off`"))?;
            if index == 0 {
                bail!("band index is 1-based (see `status`)");
            }
            let mode = match mode.as_deref().unwrap_or("solo").to_ascii_lowercase().as_str() {
                "solo" | "s" => resonance_ipc::AuditionMode::Solo,
                "listen" | "l" => resonance_ipc::AuditionMode::Listen,
                other => bail!("mode must be solo or listen, got {other}"),
            };
            Ok(Command::SetBandAudition {
                index: Some(index - 1),
                mode,
            })
        }
```

- [ ] **Step 3: Update the status badge**

In `print_status` (the bands section), replace the `solo_note` match on `s.solo_band` with:

```rust
        let solo_note = match s.audition {
            Some(a) => {
                let m = match a.mode {
                    resonance_ipc::AuditionMode::Solo => "SOLO",
                    resonance_ipc::AuditionMode::Listen => "LISTEN",
                };
                format!("  {}", p.yellow(&format!("{m} band {}", a.band + 1)))
            }
            None => String::new(),
        };
```

And the per-row `solo_tail`:

```rust
            let solo_tail = match s.audition {
                Some(a) if a.band == i => {
                    let m = match a.mode {
                        resonance_ipc::AuditionMode::Solo => "◀ solo",
                        resonance_ipc::AuditionMode::Listen => "◀ listen",
                    };
                    format!("  {}", p.yellow(m))
                }
                _ => String::new(),
            };
```

- [ ] **Step 4: Replace the parse test**

Rename `band_solo_parses_index_off_and_rejects_bad_input` → `audition_parses_index_mode_off_and_rejects_bad_input`:

```rust
    #[test]
    fn audition_parses_index_mode_off_and_rejects_bad_input() {
        assert!(matches!(
            to_ipc_command(Sub::Audition { target: "3".into(), mode: None }).unwrap(),
            Command::SetBandAudition { index: Some(2), mode: resonance_ipc::AuditionMode::Solo }
        ));
        assert!(matches!(
            to_ipc_command(Sub::Audition { target: "3".into(), mode: Some("listen".into()) }).unwrap(),
            Command::SetBandAudition { index: Some(2), mode: resonance_ipc::AuditionMode::Listen }
        ));
        for off in ["off", "none", "clear"] {
            assert!(matches!(
                to_ipc_command(Sub::Audition { target: off.into(), mode: None }).unwrap(),
                Command::SetBandAudition { index: None, .. }
            ));
        }
        assert!(to_ipc_command(Sub::Audition { target: "0".into(), mode: None }).is_err());
        assert!(to_ipc_command(Sub::Audition { target: "x".into(), mode: None }).is_err());
        assert!(to_ipc_command(Sub::Audition { target: "1".into(), mode: Some("bogus".into()) }).is_err());
    }
```

- [ ] **Step 5: Test**

Run: `cargo test -p resonance-cli audition 2>&1 | grep -E "test result|error"`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/resonance-cli
git commit -m "feat(cli): audition subcommand (solo/listen) replacing band-solo"
```

---

## Task 5: GUI — ear-icon cycles Off→Solo→Listen→Off

**Files:**
- Modify: `crates/resonance-gui/src/ui/bands.rs`
- Modify: `crates/resonance-gui/src/ui/icons.rs` (optional Listen glyph)

**Interfaces:**
- Consumes: `Command::SetBandAudition`, `resonance_ipc::{AuditionMode, BandAudition}`, `DaemonState.audition`, `kit::icon_btn_active`, `Icon::Solo`.

- [ ] **Step 1: Replace the solo cell with a cycle**

In `band_row_cells`, replace the shipped solo block (the `let soloed = state.solo_band == Some(i);` … `queue(Command::SetBandSolo {...})` block) with:

```rust
        // Audition (Solo/Listen) toggle — transient (queue, not queue_edit).
        // Click cycles Off → Solo → Listen → Off. Accent-lit while active; the
        // tooltip names the current/next mode.
        let cur = state.audition.filter(|a| a.band == i).map(|a| a.mode);
        let (active, tip) = match cur {
            None => (false, "Solo: audition only this band (click again → Listen)"),
            Some(resonance_ipc::AuditionMode::Solo) => {
                (true, "Solo (click → Listen: band-pass this band's region)")
            }
            Some(resonance_ipc::AuditionMode::Listen) => {
                (true, "Listen: band-pass region (click → off)")
            }
        };
        if kit::icon_btn_active(ui, Icon::Solo, 24.0, active, tip) {
            let next = match cur {
                None => Some(resonance_ipc::AuditionMode::Solo),
                Some(resonance_ipc::AuditionMode::Solo) => Some(resonance_ipc::AuditionMode::Listen),
                Some(resonance_ipc::AuditionMode::Listen) => None,
            };
            self.queue(Command::SetBandAudition {
                index: next.map(|_| i),
                mode: next.unwrap_or(resonance_ipc::AuditionMode::Solo),
            });
        }
```

- [ ] **Step 2: Distinguish the mode visually (S/L overlay)**

`icon_btn_active` fills accent for both modes; to tell Solo from Listen, paint a one-letter tag over the icon when active. After the `if kit::icon_btn_active(...)` call, the response rect is consumed; instead, add a tiny helper in `bands.rs` that draws the letter. Simplest within the existing kit: change the tooltip only (Step 1 already does) AND rely on the row highlight. If a glyph is wanted, add `Icon::Listen` in `icons.rs` (e.g. an ear with a dot) and select the icon by mode:

```rust
        let icon = match cur {
            Some(resonance_ipc::AuditionMode::Listen) => Icon::Listen,
            _ => Icon::Solo,
        };
        if kit::icon_btn_active(ui, icon, 24.0, active, tip) { /* ...as Step 1... */ }
```

If adding `Icon::Listen`: add the variant, the `ALL` entry, the `paths` match arm, and a `draw_listen` fn (reuse `draw_solo`'s headphone body + a filled center dot `p.dot(0.50, 0.60, 0.06)` to read as "listening"). Keep this step OPTIONAL — the tooltip + row tint from Task 6-parity already disambiguate; implement the glyph only if the reviewer wants it.

- [ ] **Step 3: Build**

Run: `cargo build -p resonance-gui 2>&1 | grep -E "error|warning: unused" ; echo done`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/resonance-gui
git commit -m "feat(gui): band audition ear-icon cycles solo/listen"
```

---

## Task 6: TUI — `L` cycles, row tag, status badge

**Files:**
- Modify: `crates/resonance-tui/src/app.rs`
- Modify: `crates/resonance-tui/src/main.rs`
- Modify: `crates/resonance-tui/src/ui.rs`

**Interfaces:**
- Consumes: `Command::SetBandAudition`, `resonance_ipc::{AuditionMode, BandAudition}`, `DaemonState.audition`.

- [ ] **Step 1: Replace `toggle_band_solo` with a cycle (app.rs)**

```rust
    /// Cycle the selected band's audition (`L` key): Off → Solo → Listen → Off.
    /// Solo bypasses other bands; Listen band-passes this band's region.
    /// Transient — no undo entry, never saved; suspends linear-phase while
    /// active. The daemon auto-clears it on any band-table edit.
    pub fn cycle_band_audition(&mut self) {
        if !matches!(self.focus, Panel::Bands | Panel::Graph) {
            return;
        }
        let Some(state) = &self.state else { return };
        let idx = self.band_cursor;
        if idx >= state.bands.len() {
            return;
        }
        let cur = state.audition.filter(|a| a.band == idx).map(|a| a.mode);
        let next = match cur {
            None => Some(resonance_ipc::AuditionMode::Solo),
            Some(resonance_ipc::AuditionMode::Solo) => Some(resonance_ipc::AuditionMode::Listen),
            Some(resonance_ipc::AuditionMode::Listen) => None,
        };
        self.send(Command::SetBandAudition {
            index: next.map(|_| idx),
            mode: next.unwrap_or(resonance_ipc::AuditionMode::Solo),
        });
        self.refresh_state();
    }
```

Ensure `resonance_ipc::AuditionMode` is imported (or use the full path).

- [ ] **Step 2: Wire the `L` key (main.rs)**

Replace `KeyCode::Char('L') if band_focus => app.toggle_band_solo(),` with:

```rust
        KeyCode::Char('L') if band_focus => app.cycle_band_audition(),
```

- [ ] **Step 3: Row tag (ui.rs)**

Replace the `let soloed = ...` line + the `let type_name = if soloed { format!("{type_name} solo") } ...` block with:

```rust
    let audition = app.state.as_ref().and_then(|s| s.audition).filter(|a| a.band == i);
    // Audition tag is never pref-gated — an active audition must always be visible.
    let type_name = match audition.map(|a| a.mode) {
        Some(resonance_ipc::AuditionMode::Solo) => format!("{type_name} solo"),
        Some(resonance_ipc::AuditionMode::Listen) => format!("{type_name} listen"),
        None => type_name,
    };
```

And in the row-background block, replace `if soloed {` with `if audition.is_some() {`.

- [ ] **Step 4: Status badge (ui.rs)**

Replace the solo-badge block (`if let Some(i) = app.state.as_ref().and_then(|s| s.solo_band) {...}`) with:

```rust
    // Audition badge — a transient mode that alters what you hear, so always
    // visible (never behind a pref). Bold yellow, 1-based band + mode.
    if let Some(a) = app.state.as_ref().and_then(|s| s.audition) {
        let m = match a.mode {
            resonance_ipc::AuditionMode::Solo => "SOLO",
            resonance_ipc::AuditionMode::Listen => "LISTEN",
        };
        spans.push(Span::styled(
            format!("{m} {}", a.band + 1),
            Style::default().fg(Color::Yellow).bold(),
        ));
        spans.push(sep());
    }
```

- [ ] **Step 5: Help + footer (ui.rs)**

Replace the help line `key("L", "solo band (audition one; press again to clear)")` with:

```rust
        key("L", "audition band: cycle off → solo → listen"),
```

Replace the two footer `[L] solo` substrings with `[L] audition`.

- [ ] **Step 6: Build + test**

Run: `cargo build -p resonance-tui 2>&1 | grep -E "error|warning: unused" ; echo done`
Then: `cargo test -p resonance-tui 2>&1 | grep -E "test result|error" | head`
Expected: clean build, tests green.

- [ ] **Step 7: Commit**

```bash
git add crates/resonance-tui
git commit -m "feat(tui): band audition L cycles solo/listen"
```

---

## Task 7: Docs + full check

**Files:**
- Modify: `docs/ROADMAP.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: ROADMAP — move listen to shipped**

Remove the `**Per-band listen/bandpass**` bullet under "Lower / niche". In the "Where Resonance is already ahead" paragraph, extend the per-band-solo sentence to note the audition now has a Listen mode (band-pass the band's region, type-aware, unity gain).

- [ ] **Step 2: CLAUDE.md — audition modes**

Update the APO-filter-bank line in the signal-flow diagram: the "transient per-band SOLO" note becomes "transient per-band AUDITION (Solo = bypass others; Listen = type-aware band-pass of the band's region; runtime-only, forces IIR)".

- [ ] **Step 3: Full check**

Run: `make fmt-fix && make check 2>&1 | grep -E "error|warning:|FAILED|Diff in|test result: FAILED" | head; echo "exit ok if empty above"`
Expected: no errors/warnings/failures.

- [ ] **Step 4: Commit**

```bash
git add docs/ROADMAP.md CLAUDE.md
git commit -m "docs: per-band listen/bandpass shipped; audition modes"
```

---

## Verification (after all tasks)

- `make check` green (Linux) + `cargo clippy --target x86_64-pc-windows-msvc -p resonance-apo -p resonance-daemon -p resonance-ipc -p resonance-dsp` clean.
- **macOS M2 live** (swap+cert-sign+kickstart recipe, [[macos-test-findings]]): load a known 2-band EQ; `resonance audition 1 listen` → `verify --json` shows the band's region survives and out-of-band probes drop; `audition 1 solo` matches the solo baseline; `audition off` restores. (Audition tones brief/low-amp.)
- **Windows real MSVC**: `cargo build --release -p resonance-daemon -p resonance-cli` + `build-apo.ps1` + `cargo test -p resonance-apo -- --test-threads=1` all green (kill any running `resonanced.exe` first). In-graph audiodg loopback deferred (needs v9 DLL redeploy: `build-apo.ps1` → `install-apo.ps1`).
- PR → squash-merge on green (standing authorization); watch post-merge `windows-installer` + `CI` conclusions via `gh run list`.

## Self-review notes

- Spec coverage: semantics (Task 1), type-aware mapping (Task 1 `build_audition_filter`), transient/force-IIR/auto-clear (Tasks 1-2), wire rename (Task 2), APO v9 (Task 3), CLI/GUI/TUI cycle (Tasks 4-6), docs (Task 7) — all covered.
- Type names consistent across tasks: `AuditionMode`, `BandAudition`, `set_audition`, `SetBandAudition`, `audition` (field), `audition_mode` (APO u32), `build_audition_filter`, `dsp_audition`/`ipc_audition`, `cycle_band_audition`, `handle_set_band_audition`, `clear_audition_if_active`.
- No placeholders; every code step shows the code. GUI Listen glyph (Task 5 Step 2) is explicitly optional.
