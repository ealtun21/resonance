# Remove Loudness Effect — Design

**Date:** 2026-07-26
**Status:** Approved
**Scope:** delete the `Loudness` effect (ISO 226:2023 equal-loudness compensation)
entirely, across every crate and front-end. Pure deletion, no replacement.

## Background

Loudness is a Resonance-native effect (no FxSound `.fac` or APO `.txt` mapping —
`Profile::from_preset` always defaults it off). User reports it's buggy enough
to break the program, and considers it redundant with Dynamic Boost (the
loudness-maximizer effect: makeup gain + lookahead brickwall limiter). Decision:
remove it outright rather than fix it.

No other effect, UI, or IPC surface special-cases `Loudness` by name — every
front-end iterates `FxEffectId::ALL` generically (`EffectsState::get`/`set`,
CLI `for id in FxEffectId::ALL`, TUI `EFFECT_NAMES` array, GUI has no
per-effect code at all) — so this is mechanical deletion, not a redesign.

## Touch points

1. **`resonance-dsp/src/effects.rs`** — delete the `iso226` submodule, the
   `LOUDNESS_*` consts, `LoudnessBand`, `LoudnessEffect` (struct + inherent
   impl + `Effect` impl), and the `loudness_contour_tests` module.
2. **`resonance-dsp/src/effects_tests.rs`** — delete `loudness_gain_db_at` and
   its 3 tests (`loudness_zero_intensity_passthrough`,
   `loudness_boosts_bass_and_treble_relative_to_mid`,
   `loudness_gain_grows_with_intensity`); drop `LoudnessEffect` from the import.
3. **`resonance-dsp/src/chain.rs`** — remove the `FxEffect::Loudness` variant
   and its `ALL` entry (7→6), the `loudness` field on `ProcessorChain`, its
   `process()` call, and its match arms in `set_effect_intensity`,
   `get_effect_state`, `set_effect_enabled`, `reset()`, `rebind_sample_rate`,
   and every chain constructor.
4. **`resonance-ipc/src/lib.rs`** — remove `FxEffectId::Loudness` + its `ALL`
   entry (7→6), its `label()` arm, its `From<FxEffectId> for FxEffect` arm,
   `EffectsState::loudness_intensity`/`loudness_enabled`, and their `get()`/
   `set()` match arms.
5. **`resonance-apo/src/state.rs`** (Windows shared-memory bridge) — remove
   `ChainSnapshot::loudness` + its `Default` entry + the three
   `FxEffect::Loudness` call sites (`effect()` builder, `apply_to`,
   `build_chain`); bump `STATE_VERSION` 9→10 with a new doc-comment line
   (`v10: − Loudness effect removed`).
6. **`resonance-daemon/src/config.rs`** — remove `loudness_intensity`/
   `loudness_enabled` from `Profile::from_preset`'s default construction (and
   its explanatory comment).
7. **`resonance-cli/src/main.rs`** — remove the `FxEffectId::Loudness` match
   arms in `effect_cli_name` and the name parser; drop "loudness" from the
   effect-name help string and the "(Loudness, Crossfeed, …)" comment.
8. **`resonance-gui/src/app.rs`** — remove `loudness_intensity`/
   `loudness_enabled` from `demo_state()`'s `EffectsState` literal.
9. **`resonance-tui/src/app.rs`** — remove the `"Loudness"` entry from
   `EFFECT_NAMES` (7→6; the array is already sized off
   `FxEffectId::ALL.len()`, no separate constant to fix).
10. **`docs/ROADMAP.md`** — drop the "loudness compensation (ISO 226:2023
    equal-loudness)" bullet from "Where Resonance is already ahead".
11. **`CLAUDE.md`** (local-only, gitignored — no commit needed) — remove the
    signal-flow-diagram `Loudness` line and the backlog bullet mentioning
    `FxEffect::Loudness`.

## Compatibility

- Old saved `.toml` profiles carrying `loudness_intensity`/`loudness_enabled`
  keys keep loading fine — no `deny_unknown_fields` anywhere in the codebase,
  so the unknown keys are silently ignored. No migration code needed.
- The `postcard` IPC wire and the Windows APO shared-memory layout both shift
  (enum discriminant index / struct byte offset) for `Crossfeed`, the one
  variant/field after `Loudness`. This is the same class of breaking change
  every past `STATE_VERSION` bump made — daemon, clients, and the APO DLL are
  already required to ship together; no compat shim.

## Testing

`make check` (fmt --check + clippy -D warnings + test --all) green with 4 fewer
DSP tests (no replacements needed — pure deletion). Confirm the effect list is
6 entries everywhere it's rendered (`resonance status`, GUI effects panel, TUI
effects column).

## Out of scope

- No consolidating any Loudness behavior into Dynamic Boost — this is removal,
  not a merge.
- No changes to any other effect or to `.fac`/APO preset parsing.

## Addendum (post-execution correction)

Execution discovered `STATE_VERSION` 10 is already claimed by the unmerged
`worktree-win-lifecycle-fixes` branch (PR #65, a daemon liveness-heartbeat
field — real, VM-verified work, not yet on `master`). Bumping to 10 here would
let two incompatible `SharedState` layouts both claim version 10. Corrected to
bump `STATE_VERSION` 9→**11** instead (skipping the reserved 10), with a doc-
comment noting why 10 is skipped. See `docs/superpowers/plans/2026-07-26-remove-loudness-effect.md`'s
matching addendum and `[[loudness-effect-removed]]` memory.
