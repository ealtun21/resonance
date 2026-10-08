# Open Work — Task List and Worktree Plan

Date: 2026-09-19. Status: planned, nothing started. Worktrees are created from the
project root at `.worktrees/<name>` (one task cluster per worktree) when a section is
picked up, not before. Each task below is the smallest unit that carries its own
test cycle; commit only after `make check` passes, conventional-commit style.

## 0. Setup + housekeeping (no worktree)

- [ ] Add `.worktrees/` to `.gitignore`; verify with `git check-ignore .worktrees`
      before the first worktree exists.
- [ ] Delete merged local branches: `feat/dynamic-eq`, `feat/gui-hide-panes`, `feat/tray-icon`.
- [ ] `git remote prune origin`.
- [ ] Delete stale remote branches: `feat/convolution-nonuniform`, `feat/preset-metadata`, `feat/tray-icon`.
- [ ] `rm 2026-06-*.log` (10 locally-ignored VBox logs in the project root).
- [ ] Replace `.superpowers/sdd/progress.md` (points at the merged capture-eq-device branch).

User-side only (needs live devices — cannot be done in a worktree):

- #42 macOS: 3-session + live-verify run needs one macOS GUI/TCC session.
- PR #65: Windows live-verify pass (R2) still outstanding before merge.
- #45/rubato: final audiodg-only verification.

## 1. `.worktrees/gui-basic-mode` — finish the "no processing" state

Branch `wip/gui-basic-mode` exists (12 commits, base older than current master).

- [ ] Set up: `git worktree add .worktrees/gui-basic-mode wip/gui-basic-mode`, rebase onto master.
- [ ] Complete the basic-mode ("no processing") UI state per the existing WIP: verify every
      control is correctly disabled/shown, no dead buttons.
- [ ] Update/extend UI state tests (no real audio; follow the project's UI test pattern).
- [ ] `make check`, split WIP commits into conventional-commit messages, push `feat/gui-basic-mode`.

## 2. `.claude/worktrees/win-lifecycle-fixes` — PR #65 (worktree already exists)

- [ ] Review open PR #65 (8 of 9 commits not on master).
- [ ] Coordinate Windows live-verify (user-side) → merge when green.

Merge this **first** of the daemon-heavy work: `tracing-spans` and `rubato-kernel`
touch the same daemon/DSP crates and will rebase on it.

## 3. `.worktrees/flatpak-ci` — #10 packaging: Flatpak CI + .deb

Flatpak manifest already exists at `contrib/flatpak/` with no CI wiring; RPM + AUR are done.

- [ ] T1: add `.github/workflows/flatpak.yml` — bump manifest version from tag,
      `flatpak-builder` (clean build only, no publish) on a Linux runner.
- [ ] T2: `.deb` packaging: minimal script that builds a binary `.deb` from release
      artifacts (no dh-rust), added as a `release.yml` artifact next to RPM/AUR.
- [ ] T3: `make check` + a local `flatpak-builder` dry-run if flatpak is available.

## 4. `.worktrees/tracing-spans` — #11 tracing spans in the main pipeline

Criterion benches + fuzz targets for convolution/spatial are already shipped; this is the
remaining half of #11.

- [ ] T1: `tracing` spans across the main audio pipeline (engine tick / graph exec /
      output write), fixed span names, no per-sample `Span::enter`.
- [ ] T2: default off; enable via `RUST_LOG` (e.g. `resonance=info,dsp=debug`) with a
      file layer option so daemon logs don't go to a dead tty.
- [ ] T3: verify spans appear (`RUST_LOG=debug resonance …` on a test graph; `resonance verify`).

## 5. `.worktrees/verify-harness` — #11 `resonance verify`/compose harness

- [ ] T1: one-page spec: what "compose harness" means here (scripted end-to-end verify
      steps: build graph → daemon up → verify → teardown), confirm with user.
- [ ] T2: implement the harness per spec + tests on a throwaway daemon instance.
- [ ] T3: document in `docs/wiki/` or `docs/` where verify lives today.

## 6. `.worktrees/pages-site` — #18 GH Pages site

- [ ] T1: static site from `docs/wiki/` content (vendored minimal theme, no build deps
      beyond what CI has).
- [ ] T2: Pages deploy workflow + CI job that builds and deploys.
- [ ] T3: README quickstart links the new URL.

## 7. `.worktrees/rubato-kernel` — #45 real in-band (C) streaming resampler

The offline windowed-sinc workaround in `crates/.../convolution.rs` is already shipped and
stays as-is; this task is the C kernel for audiodg streaming only.

- [ ] T1: port the Python test vectors into Rust/C fixtures (bit-exact expectations).
- [ ] T2: C streaming resampler library + Rust bindings.
- [ ] T3: wire into the engine behind the audiodg path only; flag-gated.
- [ ] T4: parity tests vs Python oracle + latency/perf note. **Do not restart or
      re-route the live daemon** while developing; use `resonance verify` on test graphs.

## 8. `.worktrees/routing-matrix` — N×N MADI routing-matrix editor

Currently implicit; needs a real N×N graph editor.

- [ ] T1: spec doc — N×N source/dest matrix model, constraints, validation rules.
- [ ] T2: core routing model + unit tests (pure logic, no daemon dependency).
- [ ] T3: IPC + engine wiring (route change while running).
- [ ] T4: GUI editor (egui grid widget).
- [ ] T5: TUI readout + minimal edit.

## 9. `.worktrees/gui-redesign` — GUI visual redesign (egui)

Design work lives **outside git** in `/home/nyverino/resonance-mockups/resonance-overhaul/`
(plan: `docs/superpowers/plans/2026-07-26-gui-overhaul-mockup-polish.md`, verified by
screenshots, not audio). The egui port gets a worktree.

- [ ] T1 (outside git): finish the mockup polish pass on the existing HTML/CSS mockups.
- [ ] T2: translate polished mockups to egui screens 1:1 (layout/style only, no behavior).
- [ ] T3: swap the port in behind today's screens; `make check` + screenshot diff pass.

## 10. `.worktrees/input-picker` — input device/source selection

The last open feature gap in `docs/ROADMAP.md`.

- [ ] T1: spec — input device/port discovery per backend (alsa/wasapi/audiodg), display,
      selection semantics, persistence.
- [ ] T2: backend port-enumeration API + tests.
- [ ] T3: GUI picker + engine wiring for the selected source.
- [ ] T4: TUI display of the active input.

## Suggested order / conflicts

1. Housekeeping (section 0) — minutes, do whenever.
2. PR #65 (section 2) — unblocks daemon/DSP rebases.
3. `tracing-spans` + `rubato-kernel` + `verify-harness` — daemon/DSP cluster; run after #65.
4. `gui-basic-mode` — independent, can start immediately.
5. GUI cluster: `gui-redesign`, `routing-matrix` T4, `input-picker` T3 all touch egui —
   decide the redesign's screen layout early (finish T1/T2) before the others land their
   GUI work, to cut rebase churn.
6. `flatpak-ci` and `pages-site` — independent, parallel-safe with everything.