# Live-audio test infrastructure — M0 (spikes) + M1 (agent + Linux) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove the four risky unknowns (M0), then ship `cargo xtask e2e --os linux`: a podman container with its own PipeWire in which the `resonance-e2e` agent plays signals through the real Resonance daemon, records the device output, and checks it bit-exactly against an offline render, with latency gating and a resampling report (M1).

**Architecture:** A new never-shipped crate `resonance-e2e` holds pure, unit-tested modules (stimulus, compare, render, scenario, ratechain, latency, report, checks) plus a Linux-only layer (`linux::pw` native PipeWire play+record on one graph clock, `linux::env` device/daemon/state control) and a `runner` that executes scenarios. The daemon gains one IPC command that exports its live chain through the existing APO state-file writer and restarts the RT chain from zero state; the APO's chain builder becomes a shared `ChainSnapshot::build_full_chain`, so the agent's offline render uses the exact builder that runs inside audiodg. A small `xtask` crate builds the binaries, builds/runs the container, and returns the agent's exit code.

**Tech Stack:** Rust 1.99.0 (pinned), pipewire-rs 0.10, rustfft 6, serde/toml/serde_json, hound, clap 4, podman (rootless), Arch Linux container image with PipeWire + WirePlumber.

**Spec:** `docs/superpowers/specs/2026-10-06-e2e-audio-test-infra-design.md`. Executors read the spec first.

**Scope of this plan:** M0 and M1 only. M2 (Windows VM), M3 (macOS VM) and M4 (runner/CI wiring) get their own plans after the spike results (Task 4) are appended to the spec, because their concrete steps depend on those results (which Windows virtual device, how macOS TCC is granted, whether the GitHub runner can capture).

## Global Constraints

- Toolchain is pinned in `rust-toolchain.toml` (1.99.0). `make check` (fmt --check, clippy pedantic `-D warnings`, `cargo test --all`) must pass before every commit.
- Commits: Conventional Commits, all lowercase. **No `Co-Authored-By` or any AI attribution in commits, PRs or files** (repo rule).
- IPC wire is postcard by ordinal and unversioned: new `Command` variants are **appended last**, never inserted.
- `resonance-e2e` and `xtask` have `publish = false` and are never added to release artifacts.
- `resonance-cli` must stay musl-buildable: it may only gain pure-Rust dependencies.
- Never touch the user's desktop PipeWire session or running daemon. Everything that plays or records audio runs inside the e2e container (or, in M2/M3, the VMs).
- Bit-exact means every f32 sample equal (`==`) after one integer lag shared by all channels.
- Latency gate: fail if added latency > baseline + max(1 ms, 10 % of baseline). Missing baseline fails unless `--update-baseline`.
- Flake rule: rerun a failed scenario once; mark it `flake` only if the first attempt had OS-reported discontinuities **and** the rerun passes.
- Scenarios hold at most 32 bands (`resonance_apo::state::MAX_FILTERS`).
- Live-test binaries are built with the `e2e-build` cargo profile (release speed, fast incremental). Rust has no fast-math, so results match release builds bit for bit.

## Review Focus

These are the inputs and failure modes the spec implies but no feature test exercises directly, most likely first. Each one has a pinned test in the task named.

1. **Typo in a scenario file** (unknown key, misspelled effect) silently runs a different scenario, e.g. flat instead of EQ'd. Expected: load error naming the key. Pinned in Task 11 (`deny_unknown_fields`, unknown-effect test).
2. **Silent recording** (daemon writes zeros, wrong output target). Expected: a clear "recording is silent" failure, never a pass. Pinned in Task 9.
3. **Capture started after the stimulus body began** (late stream link). Expected: `truncated`, fail. Pinned in Task 9.
4. **Daemon still on the previous device format** when playback starts. Expected: `verify_on` refuses to measure. Pinned in Task 16.
5. **Missing latency baseline** for a new scenario. Expected: failure telling you to run `--update-baseline`, not a silent pass. Pinned in Task 13.

---

## File structure

| Path | Responsibility |
|---|---|
| `crates/resonance-dsp/src/analysis.rs` | Shared signal analysis: cross-correlation lag, FFT peak, power spectrum, band levels (moved from `resonance-cli/src/verify.rs`) |
| `crates/resonance-apo/src/state.rs` | `ChainSnapshot::build_full_chain`: the one chain builder for APO + agent |
| `crates/resonance-apo/src/ffi.rs` | APO uses `build_full_chain` (Windows-only file) |
| `crates/resonance-ipc/src/lib.rs` | `Command::ResetAndExportChain { path }` |
| `crates/resonance-daemon/src/ipc_server.rs` | Handler for it |
| `crates/resonance-e2e/src/stimulus.rs` | Stimulus + chirp train generators |
| `crates/resonance-e2e/src/compare.rs` | Alignment + exact/tolerance comparison |
| `crates/resonance-e2e/src/render.rs` | Load exported chain, offline render mirroring the PipeWire filter |
| `crates/resonance-e2e/src/scenario.rs` | Scenario TOML schema, expansion, selection |
| `crates/resonance-e2e/src/ratechain.rs` | Rate hops + resampling attribution |
| `crates/resonance-e2e/src/latency.rs` | Chirp arrival lags, median, baselines, verdict |
| `crates/resonance-e2e/src/report.rs` | Result types, status settling, markdown |
| `crates/resonance-e2e/src/checks.rs` | Pitch, band-gain, gap, still-flowing checks |
| `crates/resonance-e2e/src/linux/mod.rs` | `pub mod env; pub mod pw;` |
| `crates/resonance-e2e/src/linux/pw.rs` | Native PipeWire play + multi-record on one graph clock |
| `crates/resonance-e2e/src/linux/env.rs` | Null-sink devices, graph rate, `pw-dump` parsing, daemon process, on/off verification |
| `crates/resonance-e2e/src/runner.rs` | Scenario execution, harness self-check, flake rerun, artifacts |
| `crates/resonance-e2e/src/main.rs` | `resonance-e2e run …` CLI |
| `xtask/src/main.rs` | `cargo xtask e2e` host orchestration |
| `contrib/e2e/linux/{Containerfile,entrypoint.sh}` | Container image |
| `contrib/e2e/scenarios/*.toml`, `profiles/`, `fixtures/`, `baselines/linux.toml`, `README.md` | Scenarios, shared profiles, APO fixture, latency baselines, docs |

---

# M0 — Spikes

Spikes are throwaway. Their **only** deliverable is a result table appended to the spec (Task 4). Spike code lives in the scratchpad or on throwaway branches, never on a feature branch. The exception is Spike 4's test, which is kept if it passes.

### Task 1: Spike 4 — FFT stages vs caller block size

Question: are linear-phase EQ and partitioned convolution outputs bit-identical whatever block sizes the host hands us?

**Files:**
- Create (kept if it passes): `crates/resonance-dsp/src/block_size_tests.rs`
- Modify: `crates/resonance-dsp/src/lib.rs` (register the test module)

- [ ] **Step 1: Write the test**

```rust
// crates/resonance-dsp/src/block_size_tests.rs
//! The e2e harness compares live output bit-exactly against an offline render.
//! The live host picks the block sizes, so every stage must produce the same
//! samples regardless of how the input is chunked.

use crate::chain::{PhaseMode, ProcessorChain};
use crate::convolution::IrData;
use crate::filter::{ApoFilter, FilterType};
use std::sync::Arc;

const RATE: f64 = 48_000.0;
const CH: usize = 2;

fn noise(frames: usize) -> Vec<f64> {
    let mut s = 0x2545_F491_4F6C_DD1Du64;
    (0..frames * CH)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        })
        .collect()
}

fn eq_filters() -> Vec<ApoFilter> {
    [(FilterType::Peaking, 1000.0, 6.0, 1.41), (FilterType::LowShelf, 120.0, 4.0, 0.7)]
        .into_iter()
        .map(|(t, f, g, q)| {
            ApoFilter::builder()
                .filter_type(t)
                .freq(f)
                .gain_db(g)
                .q(q)
                .channels(CH)
                .sample_rate(RATE)
                .build()
                .unwrap()
        })
        .collect()
}

fn linear_chain() -> ProcessorChain {
    let mut b = ProcessorChain::builder().channels(CH).sample_rate(RATE);
    for f in eq_filters() {
        b = b.add_filter(f);
    }
    let mut c = b.build();
    c.set_phase_mode(PhaseMode::Linear);
    let k = crate::linphase::render(&c.filters, CH, RATE).expect("kernel");
    c.eq_fir.load_ir(Arc::new(k)).unwrap();
    c
}

fn conv_chain() -> ProcessorChain {
    let mut c = ProcessorChain::builder().channels(CH).sample_rate(RATE).build();
    let taps: Vec<f64> = noise(6000).iter().step_by(CH).enumerate()
        .map(|(i, v)| v * (-(i as f64) / 900.0).exp())
        .collect();
    let ir = IrData { name: "t".into(), path: String::new(), sample_rate: RATE, channels: vec![taps] };
    c.convolution.load_ir(Arc::new(ir)).unwrap();
    c.convolution.set_enabled(true);
    c
}

fn run(mut chain: ProcessorChain, input: &[f64], sizes: &mut dyn Iterator<Item = usize>) -> Vec<f64> {
    let mut out = Vec::with_capacity(input.len());
    let mut pos = 0;
    while pos < input.len() {
        let n = (sizes.next().unwrap().max(1) * CH).min(input.len() - pos);
        let mut buf = input[pos..pos + n].to_vec();
        chain.process(&mut buf);
        out.extend_from_slice(&buf);
        pos += n;
    }
    out
}

fn assert_block_size_independent(make: fn() -> ProcessorChain) {
    let input = noise(48_000);
    let reference = run(make(), &input, &mut std::iter::repeat(1024));
    for fixed in [64usize, 128, 480, 4096] {
        let got = run(make(), &input, &mut std::iter::repeat(fixed));
        assert!(got == reference, "block size {fixed} changed the output");
    }
    let mut lcg = 12_345u64;
    let mut random = std::iter::from_fn(move || {
        lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        Some(1 + (lcg >> 33) as usize % 2048)
    });
    assert!(run(make(), &input, &mut random) == reference, "random block sizes changed the output");
}

#[test]
fn linear_phase_output_is_independent_of_block_size() {
    assert_block_size_independent(linear_chain);
}

#[test]
fn convolution_output_is_independent_of_block_size() {
    assert_block_size_independent(conv_chain);
}
```

Register in `crates/resonance-dsp/src/lib.rs` next to the other test modules:

```rust
#[cfg(test)]
mod block_size_tests;
```

- [ ] **Step 2: Run it**

Run: `cargo test -p resonance-dsp block_size -- --nocapture`
Expected: either PASS (record "bit-identical") or FAIL naming the first block size that changes the output (record which stage and which sizes).

- [ ] **Step 3: Record and keep or discard**

If both pass: keep the file (it guards the e2e exact checks), run `make check`, commit:

```bash
git add crates/resonance-dsp/src/block_size_tests.rs crates/resonance-dsp/src/lib.rs
git commit -m "test(dsp): fft stages are bit-identical across caller block sizes"
```

If either fails: delete the file and the `mod` line. Record the failing stage. Task 20 then sets `compare = { tolerance_dbfs = -120 }` on that stage's scenarios.

### Task 2: Spike 1 — Intel macOS 15 VM with Process Tap audio

Question: does Intel macOS run under quickemu on this Ryzen 9 9950X3D, and does the daemon's Process Tap receive real audio when started by a LaunchAgent, with no TCC prompt after one-time preparation?

**Files:** none in the repo. VM in `~/.cache/resonance-e2e/spike-macos/`.

- [ ] **Step 1: Create and boot the VM**

```bash
mkdir -p ~/.cache/resonance-e2e/spike-macos && cd ~/.cache/resonance-e2e/spike-macos
quickget macos sequoia
quickemu --vm macos-sequoia.conf --display spice
```

Expected: OpenCore picker, then macOS Recovery. If it hangs or kernel-panics on this AMD CPU, retry with `quickget macos sonoma` (14.6 still has the Process Tap API). If neither boots, stop here and record "macOS VM: not viable on this host".

- [ ] **Step 2: Install macOS (the one manual step)**

In the SPICE window: Disk Utility → erase the virtual disk (APFS) → Reinstall macOS → Setup Assistant with user `e2e`, password `e2e`, skip Apple ID. Then System Settings → General → Sharing → Remote Login: on. Time every step for the image checklist.

- [ ] **Step 3: Provision over ssh** (quickemu forwards guest port 22 to host port 22220)

```bash
ssh -p 22220 e2e@127.0.0.1 'softwareupdate -l | grep -i "command line"'
ssh -p 22220 e2e@127.0.0.1 'sudo softwareupdate -i "<label from previous output>" --agree-to-license'
ssh -p 22220 e2e@127.0.0.1 'curl -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.99.0'
ssh -p 22220 e2e@127.0.0.1 '/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)" && /usr/local/bin/brew install --cask blackhole-2ch blackhole-16ch && /usr/local/bin/brew install switchaudio-osx'
git -C ~/Documents/resonance archive --format=tar HEAD | ssh -p 22220 e2e@127.0.0.1 'mkdir -p ~/resonance && tar -x -C ~/resonance'
ssh -p 22220 e2e@127.0.0.1 'cd ~/resonance && ~/.cargo/bin/cargo build --release -p resonance-daemon -p resonance-cli && contrib/macos/make-signing-cert.sh && contrib/macos/build-app.sh'
```

- [ ] **Step 4: Run daemon + player from a LaunchAgent, with BlackHole as output**

In the guest, set the default output: `SwitchAudioSource -s "BlackHole 2ch"`. Write `~/Library/LaunchAgents/e2e.spike.plist` with ProgramArguments `["/bin/sh","-c","/Applications/Resonance.app/Contents/MacOS/resonanced > /tmp/d.log 2>&1 & sleep 3; afplay /System/Library/Sounds/Submarine.aiff; sleep 2; kill %1"]` and `RunAtLoad` false. Then over ssh:

```bash
ssh -p 22220 e2e@127.0.0.1 'launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/e2e.spike.plist; launchctl kickstart gui/$(id -u)/e2e.spike; sleep 10; grep -E "IOProc|with audio" /tmp/d.log'
```

Expected (pass): a line reporting IOProc callbacks **with audio > 0**. If it shows `0 with audio`, TCC denied silently.

- [ ] **Step 5: If denied, find the pre-grant path**

In order, stopping at the first that works:
1. In the SPICE window, approve the prompt once, then re-run Step 4 twice. Pass if audio flows unattended on both reruns. The grant is keyed to the stable signing identity, so it survives rebuilds.
2. Boot Recovery → Terminal → `csrutil disable` → reboot. Insert a `kTCCServiceAudioCapture` row for `com.ealtun21.resonance` into `~/Library/Application Support/com.apple.TCC/TCC.db` (schema from `sqlite3 … .schema access`). Re-run Step 4.

Record which path worked and the exact commands.

- [ ] **Step 6: Record results.** Data for Task 4: macOS version that booted; boot time; manual-step checklist; which TCC path worked; whether a 16ch BlackHole tap reports `16 ch`.

### Task 3: Spike 2 — Windows multichannel virtual device; Spike 3 — GitHub macOS runner capture

**Files:** none in the repo. VM in `~/.cache/resonance-e2e/spike-windows/`; a throwaway branch `spike/gh-macos-capture`, deleted afterwards.

- [ ] **Step 1: Windows 11 VM via quickemu (unattended)**

```bash
mkdir -p ~/.cache/resonance-e2e/spike-windows && cd ~/.cache/resonance-e2e/spike-windows
quickget windows 11
quickemu --vm windows-11.conf --display spice
```

Expected: unattended install completes to a desktop (quickget writes the unattended answer file). In the guest (PowerShell as admin): `Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0; Start-Service sshd; Set-Service sshd -StartupType Automatic`. Install the host key into `C:\ProgramData\ssh\administrators_authorized_keys`. Confirm `ssh -p 22220 Quickemu@127.0.0.1 hostname`.

- [ ] **Step 2: Try Scream (signed driver) first**

Download the latest Scream release zip from `https://github.com/duncanthrax/scream/releases` in the guest and run its installer for x64. In Sound settings, set the Scream endpoint to 8 channels (7.1) and in turn to 44.1, 48, 96 and 192 kHz (Properties → Advanced).

Pass criteria, all required:
1. The endpoint offers 8 channels at 44.1–192 kHz.
2. The APO attaches: build `resonance_apo.dll` (`contrib/windows/build-apo.ps1` after installing VS BuildTools + rustup 1.99.0) and run `install-apo.ps1` targeting the Scream endpoint. `apo.log` must show `process … ch=8`.
3. Flat loopback is bit-exact: with a flat chain, play a float32 8ch WAV of seeded noise and capture `resonanced.exe --measure-loopback "<Scream endpoint>" out.raw 3`. `scp` it back and compare on the host after integer alignment: zero differing samples.

- [ ] **Step 3: If Scream fails a criterion, try SysVAD**

`bcdedit /set testsigning on`, reboot. Build SysVAD from `microsoft/Windows-driver-samples` (`audio/sysvad`, TabletAudioSample) with the WDK and install with `devcon`. Apply the same three criteria. Record which device passes, or "stereo only" if neither does.

- [ ] **Step 4: Spike 3 — GitHub-hosted `macos-15` capture**

On branch `spike/gh-macos-capture`, add `.github/workflows/spike-macos-capture.yml` (workflow_dispatch only):

```yaml
name: spike-macos-capture
on: workflow_dispatch
jobs:
  capture:
    runs-on: macos-15
    steps:
      - uses: actions/checkout@v4
      - run: rustup toolchain install
      - run: brew install --cask blackhole-2ch && brew install switchaudio-osx
      - run: SwitchAudioSource -s "BlackHole 2ch" && csrutil status || true
      - name: Grant audio capture (user + system TCC.db)
        run: |
          for db in "$HOME/Library/Application Support/com.apple.TCC/TCC.db" "/Library/Application Support/com.apple.TCC/TCC.db"; do
            sudo sqlite3 "$db" ".schema access" || true
          done
      - run: cargo build --release -p resonance-daemon
      - run: |
          ./target/release/resonanced > d.log 2>&1 &
          sleep 3; afplay /System/Library/Sounds/Submarine.aiff; sleep 2; kill %1 || true
          cat d.log | grep -E "IOProc|with audio" || true
```

Push, run it with `gh workflow run spike-macos-capture --ref spike/gh-macos-capture`, read the log. If capture is `0 with audio`, use the printed schema to insert a `kTCCServiceAudioCapture` row (column list must match the schema) for the `resonanced` binary path. Re-run up to three times. Pass = audio > 0. Afterwards: `git push origin --delete spike/gh-macos-capture`.

### Task 4: Record spike results in the spec

**Files:**
- Modify: `docs/superpowers/specs/2026-10-06-e2e-audio-test-infra-design.md` (append section 14)

- [ ] **Step 1: Append the results**

```markdown
## 14. Spike results (YYYY-MM-DD)

| # | Result | Consequence |
|---|---|---|
| 1 | <macOS version that booted / "not viable">; TCC path: <one-time approve / TCC.db with SIP off / failed>; 16ch tap: <yes/no> | <M3 proceeds as designed / macOS = release arm64 job only> |
| 2 | <Scream / SysVAD / neither>: <channels, rates, APO attach, loopback bit-exact> | <M2 device choice / Windows multichannel scenarios platform-restricted> |
| 3 | <capture worked with: … / failed> | <release job runs live quick tier / offline only> |
| 4 | <bit-identical / stage X differs at block size Y> | <exact everywhere / tolerance_dbfs = -120 for X> |
```

- [ ] **Step 2: Commit**

```bash
git add docs/superpowers/specs/2026-10-06-e2e-audio-test-infra-design.md
git commit -m "docs(superpowers): e2e test infra spike results"
```

---

# M1 — Agent + Linux container

### Task 5: Shared analysis module in `resonance-dsp`

**Files:**
- Create: `crates/resonance-dsp/src/analysis.rs`
- Modify: `crates/resonance-dsp/src/lib.rs` (add `pub mod analysis;`)
- Modify: `crates/resonance-cli/src/verify.rs` (delete `xcorr_fft`, `best_integer_lag`, `fft_peak_hz`, `power_spectrum`; import them)
- Modify: `crates/resonance-cli/Cargo.toml` (add `resonance-dsp`)

**Interfaces:**
- Produces: `resonance_dsp::analysis::{xcorr_fft(&[f64], &[f64]) -> Vec<f64>, best_integer_lag(&[f64], &[f64], usize) -> isize, fft_peak_hz(&[f32], f64, f64) -> f64, power_spectrum(&[f64]) -> Vec<f64>, band_levels_db(&[f64], f64, &[f64]) -> Vec<f64>}`

- [ ] **Step 1: Write the failing test for the new function**

Create `crates/resonance-dsp/src/analysis.rs` with only the tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u64) -> Vec<f64> {
        let mut s = seed | 1;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                ((s >> 33) as f64 / (1u64 << 31) as f64) - 1.0
            })
            .collect()
    }

    #[test]
    fn band_levels_are_density_normalised_across_rates_and_lengths() {
        let edges = [1000.0, 8000.0];
        // Same per-sample variance spread over twice the bandwidth: 96 k reads
        // 10·log10(2) ≈ 3.01 dB lower per Hz than 48 k.
        let l48 = band_levels_db(&noise(144_000, 3), 48_000.0, &edges)[0];
        let l96 = band_levels_db(&noise(288_000, 5), 96_000.0, &edges)[0];
        assert!((l48 - l96 - 3.0103).abs() < 0.1, "48k {l48} 96k {l96}");
        // Length does not change a density.
        let short = band_levels_db(&noise(96_000, 7), 48_000.0, &edges)[0];
        let long = band_levels_db(&noise(192_000, 9), 48_000.0, &edges)[0];
        assert!((short - long).abs() < 0.1, "short {short} long {long}");
    }

    #[test]
    fn empty_band_reads_negative_infinity() {
        let l = band_levels_db(&noise(4800, 1), 48_000.0, &[30_000.0, 31_000.0]);
        assert_eq!(l, vec![f64::NEG_INFINITY]);
    }

    #[test]
    fn best_integer_lag_recovers_a_known_shift() {
        let a = noise(8192, 7);
        let mut b = vec![0.0; a.len()];
        b[137..].copy_from_slice(&a[..a.len() - 137]);
        assert_eq!(best_integer_lag(&a, &b, 1024), 137);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p resonance-dsp analysis`
Expected: compile error, `cannot find function band_levels_db` (add `pub mod analysis;` to `lib.rs` first so the module is compiled).

- [ ] **Step 3: Implement: move four functions verbatim, add `band_levels_db`**

Above the tests in `analysis.rs`:

```rust
//! Signal-analysis helpers shared by `resonance verify` and the e2e test
//! agent: FFT cross-correlation alignment, tone-peak detection, spectra.

use rustfft::{FftPlanner, num_complex::Complex};
use std::f64::consts::PI;

fn hann(i: usize, n: usize) -> f64 {
    0.5 * (1.0 - (2.0 * PI * i as f64 / n as f64).cos())
}
```

Then move `xcorr_fft`, `best_integer_lag`, `fft_peak_hz` and `power_spectrum` from `crates/resonance-cli/src/verify.rs` (currently lines 882–923, 805–848 and 1056–1069) into this file unchanged, except:
- make each `pub` and add `#[must_use]`
- drop their local `use rustfft::…` lines (the module imports it)
- replace the inline Hann expression with `hann(i, n)`

Then add:

```rust
/// Mean power spectral density per band, in dB, for the bands
/// `[edges_hz[i], edges_hz[i + 1])`. Hann-windowed and normalised by
/// `rate · Σw²`, so a stationary signal reads the same level at any length,
/// and levels are comparable across sample rates (per Hz). A band containing
/// no FFT bin reads `f64::NEG_INFINITY`.
#[must_use]
pub fn band_levels_db(x: &[f64], rate: f64, edges_hz: &[f64]) -> Vec<f64> {
    let n = x.len();
    if n < 2 || edges_hz.len() < 2 {
        return Vec::new();
    }
    let p = power_spectrum(x);
    let w2: f64 = (0..n).map(|i| hann(i, n).powi(2)).sum();
    let norm = rate * w2;
    let hz_per_bin = rate / n as f64;
    edges_hz
        .windows(2)
        .map(|e| {
            let lo = (e[0] / hz_per_bin).ceil() as usize;
            let hi = ((e[1] / hz_per_bin).ceil() as usize).min(p.len());
            if lo >= hi {
                return f64::NEG_INFINITY;
            }
            let mean = p[lo..hi].iter().sum::<f64>() / (hi - lo) as f64 / norm;
            10.0 * mean.max(1e-300).log10()
        })
        .collect()
}
```

In `verify.rs`, delete the four functions and add near the top:

```rust
use resonance_dsp::analysis::{best_integer_lag, fft_peak_hz, power_spectrum};
```

verify.rs's own tests keep calling these names through `use super::*`. In `crates/resonance-cli/Cargo.toml` `[dependencies]`, add `resonance-dsp = { path = "../resonance-dsp" }` (pure Rust, so the musl build is unaffected).

- [ ] **Step 4: Run tests**

Run: `cargo test -p resonance-dsp analysis && cargo test -p resonance-cli verify && cargo build --release --target x86_64-unknown-linux-musl -p resonance-cli`
Expected: all PASS; the musl build succeeds.

- [ ] **Step 5: `make check`, then commit**

```bash
git add crates/resonance-dsp/src/analysis.rs crates/resonance-dsp/src/lib.rs crates/resonance-cli/src/verify.rs crates/resonance-cli/Cargo.toml
git commit -m "refactor(dsp): move signal analysis helpers out of verify into resonance-dsp"
```

### Task 6: One chain builder for the APO and the agent

**Files:**
- Modify: `crates/resonance-apo/src/state.rs` (add `ChainSnapshot::build_full_chain` + tests)
- Modify: `crates/resonance-apo/src/ffi.rs` (use it; replace `attach_ir`/`attach_eq_fir` with `log_attached`)

**Interfaces:**
- Produces: `ChainSnapshot::build_full_chain(&self, channels: usize, sample_rate: f64, ir: Option<&Arc<IrData>>) -> (ProcessorChain, Vec<String>)`. The `Vec<String>` holds one note per rejected stage.

- [ ] **Step 1: Write the failing tests** (in `state.rs`'s existing `#[cfg(test)] mod tests`)

```rust
    fn peaking_chain(linear: bool) -> ProcessorChain {
        let f = resonance_dsp::filter::ApoFilter::builder()
            .filter_type(resonance_dsp::filter::FilterType::Peaking)
            .freq(1000.0)
            .gain_db(6.0)
            .q(1.0)
            .channels(2)
            .sample_rate(48_000.0)
            .build()
            .unwrap();
        let mut c = ProcessorChain::builder().channels(2).sample_rate(48_000.0).add_filter(f).build();
        if linear {
            c.set_phase_mode(resonance_dsp::chain::PhaseMode::Linear);
        }
        c
    }

    #[test]
    fn build_full_chain_attaches_ir_and_linear_phase_kernel() {
        let mut snap = ChainSnapshot::from_chain(&peaking_chain(true));
        snap.convolution_generation = 7;
        snap.convolution_enabled = 1;
        let ir = std::sync::Arc::new(resonance_dsp::convolution::IrData {
            name: "t".into(),
            path: String::new(),
            sample_rate: 48_000.0,
            channels: vec![vec![1.0, 0.5, 0.25]],
        });
        let (built, notes) = snap.build_full_chain(2, 48_000.0, Some(&ir));
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(built.convolution.info().map(|i| i.taps), Some(3));
        assert!(built.convolution.enabled());
        assert!(built.eq_fir.info().is_some(), "linear-phase kernel attached");
    }

    #[test]
    fn build_full_chain_without_ir_or_linear_phase_is_build_chain() {
        let snap = ChainSnapshot::from_chain(&peaking_chain(false));
        let (built, notes) = snap.build_full_chain(2, 48_000.0, None);
        assert!(notes.is_empty());
        assert!(built.convolution.info().is_none());
        assert!(built.eq_fir.info().is_none());
        assert_eq!(built.filters.len(), 1);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p resonance-apo build_full_chain`
Expected: compile error, `no method named build_full_chain`.

- [ ] **Step 3: Implement in `impl ChainSnapshot`** (after `build_chain`)

```rust
    /// Build the chain exactly as the APO's worker does: parameters
    /// ([`Self::build_chain`]), then the referenced convolution IR, then the
    /// linear-phase kernel. Returns one note per rejected stage; that stage is
    /// then skipped (never silence). The e2e agent uses this same builder for
    /// its offline render, so the render cannot drift from audiodg.
    #[must_use]
    pub fn build_full_chain(
        &self,
        channels: usize,
        sample_rate: f64,
        ir: Option<&std::sync::Arc<resonance_dsp::convolution::IrData>>,
    ) -> (ProcessorChain, Vec<String>) {
        let mut chain = self.build_chain(channels, sample_rate);
        let mut notes = Vec::new();
        if let Some(ir) = ir.filter(|_| self.convolution_generation != 0) {
            match chain.convolution.load_ir(std::sync::Arc::clone(ir)) {
                Ok(()) => chain.convolution.set_enabled(self.convolution_enabled != 0),
                Err(e) => notes.push(format!("convolution IR rejected: {e}")),
            }
        }
        let kernel = (chain.phase_mode == resonance_dsp::chain::PhaseMode::Linear)
            .then(|| resonance_dsp::linphase::render(&chain.filters, channels, sample_rate))
            .flatten();
        if let Some(kernel) = kernel {
            if let Err(e) = chain.eq_fir.load_ir(std::sync::Arc::new(kernel)) {
                notes.push(format!("linear-phase kernel rejected: {e}"));
            }
        }
        (chain, notes)
    }
```

- [ ] **Step 4: Make the APO use it** (`crates/resonance-apo/src/ffi.rs`)

Delete `fn attach_ir` and `fn attach_eq_fir`. Add:

```rust
/// Log what a freshly built chain carries (IR probe + linear-phase taps).
/// Off the RT path only: worker thread / format lock.
fn log_attached(chain: &ProcessorChain, sample_rate: f64) {
    if let Some(info) = chain.convolution.info() {
        let mut probe = chain.convolution.clone();
        let mut buf = vec![0.0f64; 2048];
        buf[0] = 1.0;
        probe.process(&mut buf, 1);
        let sum: f64 = buf.iter().sum();
        let peak = buf.iter().fold(0.0f64, |a, &b| a.max(b.abs()));
        log::line(&format!(
            "IR attached: taps {}, engine_rate {}, probe dc {sum:.4} peak {peak:.4}",
            info.taps, chain.sample_rate
        ));
    }
    if let Some(info) = chain.eq_fir.info() {
        log::line(&format!("linear-phase kernel attached: {} taps at {sample_rate} Hz", info.taps));
    }
}
```

In the worker rebuild (was lines 278–280):

```rust
                    if need_rebuild {
                        let (c, notes) = snap.build_full_chain(channels, sr, cached_ir.as_ref());
                        for n in &notes {
                            log::line(n);
                        }
                        log_attached(&c, sr);
```

At format lock (was lines 442–448):

```rust
        let mut chain = match snap.as_ref() {
            Some(s) => {
                let (c, notes) = s.build_full_chain(ch, sample_rate, load_ir_blob(s).as_ref());
                for n in &notes {
                    log::line(n);
                }
                log_attached(&c, sample_rate);
                c
            }
            None => build_chain(None, ch, sample_rate),
        };
        chain.reset();
```

- [ ] **Step 5: Verify, including the Windows-only file**

Run: `cargo test -p resonance-apo && cargo clippy -p resonance-apo --all-targets --target x86_64-pc-windows-msvc -- -D warnings`
Expected: tests PASS; the Windows-target clippy is clean (`ffi.rs` only compiles for Windows).

- [ ] **Step 6: `make check`, then commit**

```bash
git add crates/resonance-apo/src/state.rs crates/resonance-apo/src/ffi.rs
git commit -m "refactor(apo): share one full chain builder between the apo and its callers"
```

### Task 7: `ResetAndExportChain` IPC command

**Files:**
- Modify: `crates/resonance-ipc/src/lib.rs` (append variant + round-trip test)
- Modify: `crates/resonance-daemon/src/ipc_server.rs` (dispatch arm, handler, tests)

**Interfaces:**
- Consumes: `resonance_apo::state::{ApoStateWriter, MAX_FILTERS, read_chain_fresh}`, `SharedState::replace_chain(rt, shadow)`
- Produces: `Command::ResetAndExportChain { path: String }` → `Response::Ok` (snapshot at `path`, IR sidecar at `ir_path_for(path)`, RT chain replaced with a zero-state copy) or `Response::Error(String)`

- [ ] **Step 1: Write the failing tests**

In `crates/resonance-ipc/src/lib.rs` tests, next to the other `command_round_trip` calls:

```rust
        command_round_trip(&Command::ResetAndExportChain { path: "/tmp/chain.bin".into() });
```

In `crates/resonance-daemon/src/ipc_server.rs` tests:

```rust
    fn export_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("resonance-export-{tag}-{}", std::process::id()))
            .join("chain.bin")
    }

    #[tokio::test]
    async fn reset_and_export_chain_writes_snapshot_and_resets_rt_chain() {
        let (state, mut rx) = test_state();
        {
            let f = resonance_dsp::filter::ApoFilter::builder()
                .filter_type(resonance_dsp::filter::FilterType::Peaking)
                .freq(1000.0).gain_db(3.0).q(1.0).channels(2).sample_rate(48_000.0)
                .build().unwrap();
            state.0.lock().unwrap().chain.filters = vec![f];
        }
        while rx.pop().is_ok() {}
        let path = export_path("ok");
        let resp = dispatch(
            Command::ResetAndExportChain { path: path.to_string_lossy().into_owned() },
            &state,
        )
        .await;
        assert!(matches!(resp, Response::Ok), "{resp:?}");
        let (_, snap, _) = resonance_apo::state::read_chain_fresh(&path).expect("snapshot written");
        assert_eq!(snap.num_filters, 1);
        assert!(
            matches!(rx.pop(), Ok(AudioCommand::ReplaceChain(_))),
            "RT thread receives a fresh chain"
        );
    }

    #[tokio::test]
    async fn reset_and_export_chain_refuses_more_bands_than_a_snapshot_holds() {
        let (state, _rx) = test_state();
        {
            let f = resonance_dsp::filter::ApoFilter::builder()
                .filter_type(resonance_dsp::filter::FilterType::Peaking)
                .freq(1000.0).gain_db(3.0).q(1.0).channels(2).sample_rate(48_000.0)
                .build().unwrap();
            state.0.lock().unwrap().chain.filters = vec![f; resonance_apo::state::MAX_FILTERS + 1];
        }
        let path = export_path("too-many");
        let resp = dispatch(
            Command::ResetAndExportChain { path: path.to_string_lossy().into_owned() },
            &state,
        )
        .await;
        assert!(matches!(&resp, Response::Error(e) if e.contains("at most 32")), "{resp:?}");
        assert!(!path.exists(), "nothing written on refusal");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p resonance-ipc && cargo test -p resonance-daemon reset_and_export`
Expected: compile error, `no variant named ResetAndExportChain`.

- [ ] **Step 3: Implement**

In `crates/resonance-ipc/src/lib.rs`, as the **last** `Command` variant (after `SetBandAudition { … },`):

```rust
    /// Test harness (`resonance-e2e`): write the live chain as an APO state
    /// file at `path` (IR sidecar beside it, see
    /// `resonance_apo::state::ir_path_for`) and hand the RT thread a zero-state
    /// copy, so the next audio is processed exactly like an offline render of
    /// that file from its first sample. Append-only (see note above).
    ResetAndExportChain { path: String },
```

In `crates/resonance-daemon/src/ipc_server.rs` `dispatch`, next to `Command::CaptureOutput`:

```rust
        Command::ResetAndExportChain { path } => handle_reset_and_export_chain(state, &path),
```

and the handler:

```rust
/// Export the live chain for the e2e harness, then restart the RT chain from
/// zeroed filter/effect state (see `Command::ResetAndExportChain`).
fn handle_reset_and_export_chain(state: &SharedState, path: &str) -> Response {
    use resonance_apo::state::{ApoStateWriter, MAX_FILTERS};
    let mut chain = state.0.lock().unwrap().chain.clone();
    if chain.filters.len() > MAX_FILTERS {
        return Response::Error(format!(
            "chain has {} bands; a snapshot holds at most {MAX_FILTERS}",
            chain.filters.len()
        ));
    }
    let mut writer = match ApoStateWriter::create(std::path::Path::new(path)) {
        Ok(w) => w,
        Err(e) => return Response::Error(format!("create '{path}': {e}")),
    };
    writer.publish(&chain);
    chain.reset();
    state.replace_chain(chain.clone(), chain);
    Response::Ok
}
```

If `dispatch` classifies commands (e.g. the `band_mutating` match at the top), leave `ResetAndExportChain` out: it does not mutate bands.

- [ ] **Step 4: Run tests**

Run: `cargo test -p resonance-ipc && cargo test -p resonance-daemon`
Expected: PASS.

- [ ] **Step 5: `make check`, then commit**

```bash
git add crates/resonance-ipc/src/lib.rs crates/resonance-daemon/src/ipc_server.rs
git commit -m "feat(daemon): reset-and-export-chain ipc command for the e2e harness"
```

### Task 8: `resonance-e2e` crate + stimulus generator

**Files:**
- Create: `crates/resonance-e2e/Cargo.toml`, `crates/resonance-e2e/src/lib.rs`, `crates/resonance-e2e/src/stimulus.rs`
- Modify: `Cargo.toml` (workspace members; `[profile.e2e-build]`)

**Interfaces:**
- Produces: `stimulus::{PILOT_HZ: f64, Stimulus { rate: u32, channels: usize, samples: Vec<f32>, body: Range<usize> }, Stimulus::frames(&self) -> usize, generate(rate: u32, channels: usize, body_secs: f64) -> Stimulus, CHIRPS: usize, ChirpTrain { rate, channels, samples: Vec<f32>, emit: Vec<usize>, chirp_frames: usize }, chirp_train(rate: u32, channels: usize) -> ChirpTrain}`

- [ ] **Step 1: Crate scaffolding**

`crates/resonance-e2e/Cargo.toml`:

```toml
[package]
name = "resonance-e2e"
description = "Live-audio end-to-end test agent (never shipped)"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true
publish = false

[dependencies]
resonance-dsp = { path = "../resonance-dsp" }
resonance-ipc = { path = "../resonance-ipc" }
resonance-apo = { path = "../resonance-apo" }
anyhow = { workspace = true }
clap = { workspace = true }
hound = { workspace = true }
rustfft = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
toml = { workspace = true }

[target.'cfg(target_os = "linux")'.dependencies]
pipewire = { workspace = true }

[lints]
workspace = true
```

`crates/resonance-e2e/src/lib.rs`:

```rust
//! Live-audio end-to-end test agent: plays deterministic signals through the
//! real OS audio stack with Resonance in the path, records the device output,
//! and checks it against an offline render of the daemon's own chain.
//! See `docs/superpowers/specs/2026-10-06-e2e-audio-test-infra-design.md`.

pub mod stimulus;
```

Root `Cargo.toml`: add `"crates/resonance-e2e"` to `members`, and after `[profile.release]`:

```toml
# Live-audio e2e builds (`cargo xtask e2e`): release-speed DSP so 192 kHz ×
# 16 ch never xruns, with fast incremental rebuilds. Rust has no fast-math,
# so output matches release builds bit for bit.
[profile.e2e-build]
inherits = "release"
opt-level = 3
lto = false
codegen-units = 16
incremental = true
strip = false
```

- [ ] **Step 2: Write the failing tests** (bottom of `stimulus.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use resonance_dsp::analysis::fft_peak_hz;

    #[test]
    fn stimulus_is_deterministic() {
        assert_eq!(generate(48_000, 2, 0.5).samples, generate(48_000, 2, 0.5).samples);
    }

    #[test]
    fn stimulus_channels_differ() {
        let s = generate(48_000, 2, 0.5);
        let differing = s.body.clone().filter(|&i| s.samples[i * 2] != s.samples[i * 2 + 1]).count();
        assert!(differing > s.body.len() * 9 / 10, "{differing}");
    }

    #[test]
    fn stimulus_is_framed_by_silence_and_peaks_at_or_below_minus_12_dbfs() {
        let s = generate(96_000, 3, 0.5);
        assert!(s.samples[..s.body.start * 3].iter().all(|&v| v == 0.0));
        assert!(s.samples[s.body.end * 3..].iter().all(|&v| v == 0.0));
        let peak = s.samples.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(peak <= 0.2512 && peak > 0.1, "{peak}");
    }

    #[test]
    fn pilot_is_detectable_in_the_body() {
        let s = generate(48_000, 1, 3.0);
        let hz = fft_peak_hz(&s.samples[s.body.clone()], 48_000.0, PILOT_HZ);
        assert!(((hz - PILOT_HZ) / PILOT_HZ).abs() < 1e-4, "{hz}");
    }

    #[test]
    fn chirp_train_marks_each_emission() {
        let t = chirp_train(48_000, 2);
        assert_eq!(t.emit.len(), CHIRPS);
        for &e in &t.emit {
            assert_eq!(t.samples[(e - 1) * 2], 0.0, "silence before chirp");
            assert!(t.samples[(e + 20) * 2].abs() > 0.0, "chirp present");
        }
    }
}
```

Run: `cargo test -p resonance-e2e`. Expected: compile errors (functions missing).

- [ ] **Step 3: Implement** (top of `stimulus.rs`)

```rust
//! Deterministic test signals: the bit-exact stimulus and the latency chirps.

use std::f64::consts::PI;
use std::ops::Range;

/// Pilot tone mixed into every stimulus channel; resampling and event
/// scenarios read their pitch check from it.
pub const PILOT_HZ: f64 = 997.0;
const LEAD_SECS: f64 = 0.5;
const TAIL_SECS: f64 = 0.5;
// Component peaks sum to 0.25 (−12 dBFS), so no stage can clip it.
const NOISE_AMP: f64 = 0.125;
const SWEEP_AMP: f64 = 0.0625;
const PILOT_AMP: f64 = 0.0625;

/// Interleaved f32 signal with digital silence around the body.
#[derive(Debug, Clone)]
pub struct Stimulus {
    pub rate: u32,
    pub channels: usize,
    pub samples: Vec<f32>,
    /// Frame range of the non-silent body.
    pub body: Range<usize>,
}

impl Stimulus {
    #[must_use]
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels
    }
}

/// xorshift64* → uniform in [-1, 1). A distinct seed per channel means a
/// swapped or misrouted channel can never compare equal.
struct Noise(u64);

impl Noise {
    fn new(channel: usize) -> Self {
        Self(0x9E37_79B9_7F4A_7C15 ^ ((channel as u64 + 1).wrapping_mul(0xD1B5_4A32_D192_ED03)))
    }

    fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let v = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
        v as f64 / (1u64 << 52) as f64 - 1.0
    }
}

/// `body_secs` of noise + exponential sweep (20 Hz → 0.45·rate) + pilot at
/// `rate`, framed by 0.5 s of digital silence on each side.
#[must_use]
pub fn generate(rate: u32, channels: usize, body_secs: f64) -> Stimulus {
    let r = f64::from(rate);
    let lead = (LEAD_SECS * r) as usize;
    let body = (body_secs * r) as usize;
    let tail = (TAIL_SECS * r) as usize;
    let mut samples = vec![0.0f32; (lead + body + tail) * channels];
    let (f0, f1) = (20.0, 0.45 * r);
    let k = (f1 / f0).ln();
    let dur = body as f64 / r;
    for c in 0..channels {
        let mut noise = Noise::new(c);
        for i in 0..body {
            let t = i as f64 / r;
            let sweep = (2.0 * PI * f0 * dur / k * ((k * t / dur).exp() - 1.0)).sin();
            let pilot = (2.0 * PI * PILOT_HZ * t).sin();
            let s = NOISE_AMP * noise.next() + SWEEP_AMP * sweep + PILOT_AMP * pilot;
            samples[(lead + i) * channels + c] = s as f32;
        }
    }
    Stimulus { rate, channels, samples, body: lead..lead + body }
}

pub const CHIRPS: usize = 5;

/// [`CHIRPS`] identical 50 ms linear chirps (200 Hz → 8 kHz, −12 dBFS, all
/// channels), 0.5 s apart after 0.2 s of silence. `emit[j]` is chirp j's
/// first frame.
#[derive(Debug, Clone)]
pub struct ChirpTrain {
    pub rate: u32,
    pub channels: usize,
    pub samples: Vec<f32>,
    pub emit: Vec<usize>,
    pub chirp_frames: usize,
}

#[must_use]
pub fn chirp_train(rate: u32, channels: usize) -> ChirpTrain {
    let r = f64::from(rate);
    let chirp_frames = (0.05 * r) as usize;
    let spacing = (0.5 * r) as usize;
    let lead = (0.2 * r) as usize;
    let frames = lead + spacing * CHIRPS + (0.5 * r) as usize;
    let mut samples = vec![0.0f32; frames * channels];
    let (f0, f1) = (200.0, 8000.0_f64.min(0.45 * r));
    let dur = chirp_frames as f64 / r;
    let emit: Vec<usize> = (0..CHIRPS).map(|j| lead + j * spacing).collect();
    for &start in &emit {
        for i in 0..chirp_frames {
            let t = i as f64 / r;
            let s = (0.25 * (2.0 * PI * (f0 * t + (f1 - f0) * t * t / (2.0 * dur))).sin()) as f32;
            for c in 0..channels {
                samples[(start + i) * channels + c] = s;
            }
        }
    }
    ChirpTrain { rate, channels, samples, emit, chirp_frames }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p resonance-e2e`
Expected: PASS (5 tests).

- [ ] **Step 5: `make check`, then commit**

```bash
git add Cargo.toml Cargo.lock crates/resonance-e2e
git commit -m "feat(e2e): resonance-e2e crate with deterministic stimulus and chirp train"
```

### Task 9: Alignment + comparison

**Files:**
- Create: `crates/resonance-e2e/src/compare.rs`
- Modify: `crates/resonance-e2e/src/lib.rs` (`pub mod compare;`)

**Interfaces:**
- Produces: `compare::{CompareMode::{Exact, Tolerance { dbfs: f64 }} (Serialize; Deserialize from "exact" | { tolerance_dbfs = f64 }), CompareOutcome { lag: isize, channel_lag_mismatch: Option<(usize, isize)>, max_abs_err: f32, first_diff: Option<(usize, usize)>, truncated: bool, silent: bool }, CompareOutcome::passes(&self, CompareMode) -> bool, CompareOutcome::describe(&self) -> String, compare(expected: &[f32], recorded: &[f32], channels: usize, window: Range<usize>, max_lag: usize) -> CompareOutcome}`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn noise(frames: usize, ch: usize) -> Vec<f32> {
        let mut s = 0x1234_5678_9ABC_DEF1u64;
        (0..frames * ch)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                ((s >> 40) as f32 / (1u32 << 24) as f32) - 0.5
            })
            .collect()
    }

    fn delayed(x: &[f32], ch: usize, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * ch];
        out.extend_from_slice(x);
        out
    }

    fn padded(x: &[f32], ch: usize) -> Vec<f32> {
        // Expected render: 200 frames of silence, then the signal.
        delayed(x, ch, 200)
    }

    #[test]
    fn identical_after_delay_passes_exact() {
        let exp = padded(&noise(4000, 2), 2);
        let rec = delayed(&exp, 2, 37);
        let o = compare(&exp, &rec, 2, 200..4200, 512);
        assert_eq!(o.lag, 37);
        assert!(o.passes(CompareMode::Exact), "{}", o.describe());
    }

    #[test]
    fn one_flipped_lsb_fails_exact_and_reports_where() {
        let exp = padded(&noise(4000, 2), 2);
        let mut rec = delayed(&exp, 2, 37);
        let i = (37 + 1234) * 2 + 1;
        rec[i] = f32::from_bits(rec[i].to_bits() ^ 1);
        let o = compare(&exp, &rec, 2, 200..4200, 512);
        assert_eq!(o.first_diff, Some((1234, 1)));
        assert!(!o.passes(CompareMode::Exact));
        assert!(o.passes(CompareMode::Tolerance { dbfs: -120.0 }));
    }

    #[test]
    fn swapped_channels_fail() {
        let exp = padded(&noise(4000, 2), 2);
        let rec: Vec<f32> = exp.chunks(2).flat_map(|f| [f[1], f[0]]).collect();
        assert!(!compare(&exp, &rec, 2, 200..4200, 512).passes(CompareMode::Exact));
    }

    #[test]
    fn per_channel_slip_is_reported() {
        let exp = padded(&noise(4000, 2), 2);
        let mut rec = delayed(&exp, 2, 37);
        // Channel 1 arrives one frame later than channel 0.
        let frames = rec.len() / 2;
        for f in (1..frames).rev() {
            rec[f * 2 + 1] = rec[(f - 1) * 2 + 1];
        }
        rec[1] = 0.0;
        let o = compare(&exp, &rec, 2, 200..4200, 512);
        assert_eq!(o.channel_lag_mismatch, Some((1, 38)));
        assert!(!o.passes(CompareMode::Exact));
    }

    #[test]
    fn dropped_frame_fails() {
        let exp = padded(&noise(4000, 2), 2);
        let mut rec = delayed(&exp, 2, 37);
        rec.drain((37 + 2000) * 2..(37 + 2001) * 2);
        assert!(!compare(&exp, &rec, 2, 200..4200, 512).passes(CompareMode::Exact));
    }

    #[test]
    fn recording_ending_early_is_truncated() {
        let exp = padded(&noise(4000, 2), 2);
        let mut rec = delayed(&exp, 2, 37);
        rec.truncate((37 + 3000) * 2);
        let o = compare(&exp, &rec, 2, 200..4200, 512);
        assert!(o.truncated && !o.passes(CompareMode::Exact));
    }

    // Review Focus 3: capture linked after the body began.
    #[test]
    fn capture_started_after_the_body_began_is_truncated() {
        let exp = padded(&noise(4000, 2), 2);
        let rec = exp[(200 + 300) * 2..].to_vec(); // first 300 body frames never recorded
        let o = compare(&exp, &rec, 2, 200..4200, 512);
        assert!(o.truncated && !o.passes(CompareMode::Exact), "{}", o.describe());
    }

    // Review Focus 2: a silent recording must fail clearly.
    #[test]
    fn silent_recording_fails_and_says_so() {
        let exp = padded(&noise(4000, 2), 2);
        let rec = vec![0.0f32; exp.len() + 200];
        let o = compare(&exp, &rec, 2, 200..4200, 512);
        assert!(o.silent && !o.passes(CompareMode::Tolerance { dbfs: 0.0 }));
        assert!(o.describe().contains("silent"));
    }

    #[test]
    fn compare_mode_parses_from_scenario_toml() {
        #[derive(serde::Deserialize)]
        struct W {
            compare: CompareMode,
        }
        let e: W = toml::from_str(r#"compare = "exact""#).unwrap();
        assert_eq!(e.compare, CompareMode::Exact);
        let t: W = toml::from_str("compare = { tolerance_dbfs = -120 }").unwrap();
        assert_eq!(t.compare, CompareMode::Tolerance { dbfs: -120.0 });
    }
}
```

Run: `cargo test -p resonance-e2e compare`. Expected: compile errors.

- [ ] **Step 2: Implement**

```rust
//! Align a recording to its expected render, then compare every sample.

use resonance_dsp::analysis::best_integer_lag;
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// How a scenario's recording must match its offline render.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(from = "CompareToml")]
pub enum CompareMode {
    /// Every f32 sample equal (`==`).
    #[default]
    Exact,
    /// Largest absolute error at or below `dbfs` (relative to full scale).
    Tolerance { dbfs: f64 },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CompareToml {
    Named(ExactTag),
    Tolerance { tolerance_dbfs: f64 },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExactTag {
    Exact,
}

impl From<CompareToml> for CompareMode {
    fn from(t: CompareToml) -> Self {
        match t {
            CompareToml::Named(ExactTag::Exact) => Self::Exact,
            CompareToml::Tolerance { tolerance_dbfs } => Self::Tolerance { dbfs: tolerance_dbfs },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CompareOutcome {
    /// Frames the recording lags the expected render (channel 0).
    pub lag: isize,
    /// First channel whose own best lag differs from channel 0's.
    pub channel_lag_mismatch: Option<(usize, isize)>,
    pub max_abs_err: f32,
    /// First differing sample in the window: `(frame, channel)`.
    pub first_diff: Option<(usize, usize)>,
    /// The aligned window ran outside the recording.
    pub truncated: bool,
    /// The recording is all zeros over the aligned window.
    pub silent: bool,
}

impl CompareOutcome {
    #[must_use]
    pub fn passes(&self, mode: CompareMode) -> bool {
        if self.silent || self.truncated || self.channel_lag_mismatch.is_some() {
            return false;
        }
        match mode {
            CompareMode::Exact => self.first_diff.is_none(),
            CompareMode::Tolerance { dbfs } => f64::from(self.max_abs_err) <= 10f64.powf(dbfs / 20.0),
        }
    }

    /// One-line human summary for reports.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.silent {
            return "recording is silent".into();
        }
        if self.truncated {
            return format!("recording does not cover the stimulus (lag {} frames)", self.lag);
        }
        if let Some((c, l)) = self.channel_lag_mismatch {
            return format!("channel {c} is offset by {l} frames, channel 0 by {}", self.lag);
        }
        match self.first_diff {
            None => format!("bit-exact (lag {} frames)", self.lag),
            Some((f, c)) => {
                let db = 20.0 * f64::from(self.max_abs_err).max(1e-30).log10();
                format!("first difference at frame {f} channel {c}; max error {db:.1} dBFS")
            }
        }
    }
}

fn channel(x: &[f32], channels: usize, c: usize) -> Vec<f64> {
    x.iter().skip(c).step_by(channels).map(|&v| f64::from(v)).collect()
}

/// Find the recording's lag against `expected` on every channel (all must
/// agree), then compare `expected[window]` with the recording shifted by it.
#[must_use]
#[allow(clippy::float_cmp)] // bit-exactness is the point of this function
pub fn compare(
    expected: &[f32],
    recorded: &[f32],
    channels: usize,
    window: Range<usize>,
    max_lag: usize,
) -> CompareOutcome {
    let lag = best_integer_lag(&channel(expected, channels, 0), &channel(recorded, channels, 0), max_lag);
    let channel_lag_mismatch = (1..channels).find_map(|c| {
        let l = best_integer_lag(&channel(expected, channels, c), &channel(recorded, channels, c), max_lag);
        (l != lag).then_some((c, l))
    });
    let rec_frames = recorded.len() / channels;
    let (mut max_abs_err, mut first_diff, mut truncated, mut silent) = (0.0f32, None, false, true);
    for f in window {
        let Some(rf) = f.checked_add_signed(lag).filter(|&rf| rf < rec_frames) else {
            truncated = true;
            break;
        };
        for c in 0..channels {
            let (e, r) = (expected[f * channels + c], recorded[rf * channels + c]);
            silent &= r == 0.0;
            if e != r {
                max_abs_err = max_abs_err.max((e - r).abs());
                first_diff.get_or_insert((f, c));
            }
        }
    }
    CompareOutcome { lag, channel_lag_mismatch, max_abs_err, first_diff, truncated, silent }
}
```

Also add `pub mod compare;` to `lib.rs`. If `toml` isn't visible in tests, it's already a normal dependency.

- [ ] **Step 3: Run tests**

Run: `cargo test -p resonance-e2e compare`
Expected: PASS (9 tests).

- [ ] **Step 4: `make check`, then commit**

```bash
git add crates/resonance-e2e/src/compare.rs crates/resonance-e2e/src/lib.rs
git commit -m "feat(e2e): align recordings and compare them bit-exactly"
```

### Task 10: Offline render of the exported chain

**Files:**
- Create: `crates/resonance-e2e/src/render.rs`
- Modify: `crates/resonance-e2e/src/lib.rs` (`pub mod render;`)

**Interfaces:**
- Consumes: `ChainSnapshot::build_full_chain` (Task 6), the file written by `ResetAndExportChain` (Task 7)
- Produces: `render::{load_exported_chain(path: &Path, channels: usize, rate: f64) -> anyhow::Result<(ProcessorChain, Vec<String>)>, render(chain: &mut ProcessorChain, input: &[f32], block_frames: usize) -> Vec<f32>, BLOCK_FRAMES: usize}`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::stimulus::generate;
    use resonance_apo::state::ApoStateWriter;
    use resonance_dsp::filter::{ApoFilter, FilterType};

    fn tmp(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("resonance-e2e-render-{tag}-{}", std::process::id())).join("chain.bin")
    }

    fn eq_chain() -> ProcessorChain {
        let mut b = ProcessorChain::builder().channels(2).sample_rate(48_000.0);
        for (t, f, g, q) in [(FilterType::Peaking, 1000.0, 6.0, 1.41), (FilterType::LowShelf, 120.0, 4.0, 0.7)] {
            b = b.add_filter(
                ApoFilter::builder().filter_type(t).freq(f).gain_db(g).q(q)
                    .channels(2).sample_rate(48_000.0).build().unwrap(),
            );
        }
        b.build()
    }

    #[test]
    fn exported_flat_chain_is_a_bit_exact_passthrough() {
        let path = tmp("flat");
        ApoStateWriter::create(&path).unwrap()
            .publish(&ProcessorChain::builder().channels(2).sample_rate(48_000.0).build());
        let (mut c, notes) = load_exported_chain(&path, 2, 48_000.0).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        let input = generate(48_000, 2, 0.2).samples;
        assert_eq!(render(&mut c, &input, BLOCK_FRAMES), input);
    }

    #[test]
    fn rebuilt_eq_chain_renders_identically_to_the_original() {
        let path = tmp("eq");
        let mut original = eq_chain();
        ApoStateWriter::create(&path).unwrap().publish(&original);
        let (mut rebuilt, _) = load_exported_chain(&path, 2, 48_000.0).unwrap();
        let input = generate(48_000, 2, 0.2).samples;
        let a = render(&mut original, &input, BLOCK_FRAMES);
        assert_eq!(a, render(&mut rebuilt, &input, BLOCK_FRAMES));
        assert_ne!(a, input, "the EQ must change the signal");
    }

    #[test]
    fn iir_render_does_not_depend_on_block_size() {
        let input = generate(48_000, 2, 0.2).samples;
        assert_eq!(render(&mut eq_chain(), &input, 64), render(&mut eq_chain(), &input, 1024));
    }

    #[test]
    fn missing_snapshot_is_an_error() {
        assert!(load_exported_chain(&tmp("missing"), 2, 48_000.0).is_err());
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p resonance-e2e render`. Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! Offline render of the daemon's exported chain (`ResetAndExportChain`).

use anyhow::{Context, Result, bail};
use resonance_apo::state::{ir_path_for, read_chain_fresh, read_ir_blob};
use resonance_dsp::chain::ProcessorChain;
use std::path::Path;
use std::sync::Arc;

/// Render block size. IIR stages are block-size independent; FFT stages are
/// covered by `resonance-dsp`'s `block_size_tests` (spike 4).
pub const BLOCK_FRAMES: usize = 1024;

/// Rebuild the exported chain at `channels`/`rate` with the APO's own builder
/// (`ChainSnapshot::build_full_chain`), from zeroed state. Notes list stages
/// the builder rejected.
pub fn load_exported_chain(path: &Path, channels: usize, rate: f64) -> Result<(ProcessorChain, Vec<String>)> {
    let (_, snap, _) =
        read_chain_fresh(path).with_context(|| format!("read chain snapshot {}", path.display()))?;
    let ir = match snap.convolution_generation {
        0 => None,
        generation => {
            let (blob, ir) = read_ir_blob(&ir_path_for(path)).context("read IR sidecar")?;
            if blob != generation {
                bail!("IR sidecar generation {blob} != snapshot generation {generation}");
            }
            Some(Arc::new(ir))
        }
    };
    let (mut chain, notes) = snap.build_full_chain(channels, rate, ir.as_ref());
    chain.reset();
    Ok((chain, notes))
}

/// Process interleaved f32 `input` exactly as the PipeWire filter does
/// (`resonance-daemon/src/audio/pipewire.rs`): f32 → f64, `process`, square
/// route when the matrix matches the width, `as f32`.
#[must_use]
pub fn render(chain: &mut ProcessorChain, input: &[f32], block_frames: usize) -> Vec<f32> {
    let ch = chain.channels;
    let route = matches!(&chain.routing, Some(m) if m.in_ch() == ch && m.out_ch() == ch);
    let mut out = Vec::with_capacity(input.len());
    let mut routed = vec![0.0f64; block_frames * ch];
    for block in input.chunks(block_frames * ch) {
        let mut buf: Vec<f64> = block.iter().map(|&s| f64::from(s)).collect();
        chain.process(&mut buf);
        if route {
            chain.route(&buf, &mut routed[..buf.len()]);
            out.extend(routed[..buf.len()].iter().map(|&v| v as f32));
        } else {
            out.extend(buf.iter().map(|&v| v as f32));
        }
    }
    out
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p resonance-e2e render`
Expected: PASS (4 tests).

- [ ] **Step 5: `make check`, then commit**

```bash
git add crates/resonance-e2e/src/render.rs crates/resonance-e2e/src/lib.rs
git commit -m "feat(e2e): offline render through the apo's chain builder"
```

### Task 11: Scenario schema, expansion and selection

**Files:**
- Create: `crates/resonance-e2e/src/scenario.rs`
- Modify: `crates/resonance-e2e/src/lib.rs` (`pub mod scenario;`)

**Interfaces:**
- Consumes: `CompareMode` (Task 9), `resonance_ipc::{BandState, EffectsState}`
- Produces:
  - `Tier::{Quick, Full}` (serde snake_case, clap `ValueEnum`)
  - `Scenario { id: String, rate: u32, channels: usize, player_rate: u32, graph_rate: u32, body_secs: f64, in_quick: bool, platforms: Option<Vec<String>>, expected_fail: Option<String>, profile: Profile, expect: Expect, events: Vec<Event> }`
  - `Scenario::measures_latency(&self) -> bool`
  - `Profile { preamp_db: f64, bands: Vec<BandState>, effects: EffectsState, dither_bits: Option<u32>, linear_phase: bool, ir: Option<String>, preset: Option<PathBuf> }`
  - `Expect { compare: CompareMode, resample: Vec<AllowedHop>, max_gap_ms: f64 }`
  - `AllowedHop { hop: String, reason: String }`
  - `Event { at_secs: f64, kind: EventKind }`, `EventKind::{ForceRate { rate: u32 }, SwitchDevice { rate: u32, channels: usize }, RestartDaemon}`
  - `load_dir(dir: &Path) -> anyhow::Result<Vec<Scenario>>`, `select<'a>(all: &'a [Scenario], tier: Tier, filter: Option<&str>, os: &str) -> Vec<&'a Scenario>`, `glob_match(pat: &str, s: &str) -> bool`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(files: &[(&str, &str)]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "resonance-e2e-scn-{}-{}",
            std::process::id(),
            files[0].0.replace('/', "_")
        ));
        let _ = std::fs::remove_dir_all(&d);
        for (name, body) in files {
            let p = d.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        d
    }

    const FLAT: &str = r#"
[[scenario]]
id = "flat"
tier = "full"
rates = [44100, 48000]
channels = [2, 8]
quick = [[48000, 2]]
"#;

    #[test]
    fn lists_expand_into_one_scenario_per_combination() {
        let s = load_dir(&dir_with(&[("a.toml", FLAT)])).unwrap();
        let ids: Vec<&str> = s.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["flat@44100x2", "flat@44100x8", "flat@48000x2", "flat@48000x8"]);
        assert_eq!(s[2].player_rate, 48_000);
        assert_eq!(s[2].graph_rate, 48_000);
        assert_eq!(s[2].expect.compare, CompareMode::Exact);
    }

    #[test]
    fn quick_tier_selects_only_listed_combinations() {
        let all = load_dir(&dir_with(&[("b.toml", FLAT)])).unwrap();
        let q: Vec<&str> = select(&all, Tier::Quick, None, "linux").iter().map(|s| s.id.as_str()).collect();
        assert_eq!(q, ["flat@48000x2"]);
        assert_eq!(select(&all, Tier::Full, Some("*x8"), "linux").len(), 2);
    }

    #[test]
    fn platform_restriction_filters_other_oses() {
        let src = format!("{FLAT}platforms = [\"windows\"]\n");
        let all = load_dir(&dir_with(&[("c.toml", &src)])).unwrap();
        assert!(select(&all, Tier::Full, None, "linux").is_empty());
    }

    #[test]
    fn profile_file_is_resolved_and_effects_map_by_name() {
        let scn = r#"
[[scenario]]
id = "eq"
tier = "quick"
rates = [48000]
channels = [2]
profile_file = "profiles/p.toml"
"#;
        let prof = r#"
preamp_db = -3.0
effects = { bass = 0.5, crossfeed = 0.25 }
[[bands]]
band_type = "Peaking"
freq = 1000.0
gain_db = 6.0
q = 1.41
enabled = true
"#;
        let s = &load_dir(&dir_with(&[("d.toml", scn), ("profiles/p.toml", prof)])).unwrap()[0];
        assert_eq!(s.profile.bands.len(), 1);
        assert!(s.profile.effects.bass_enabled && (s.profile.effects.bass_intensity - 0.5).abs() < 1e-12);
        assert!(s.profile.effects.crossfeed_enabled);
        assert!(!s.profile.effects.fidelity_enabled);
    }

    #[test]
    fn events_and_tolerance_parse() {
        let scn = r#"
[[scenario]]
id = "ev"
tier = "full"
rates = [48000]
channels = [2]
[scenario.expect]
compare = { tolerance_dbfs = -120 }
max_gap_ms = 3000
resample = [{ hop = "player->graph", reason = "content rate differs" }]
[[scenario.events]]
at_secs = 1.5
kind = "force_rate"
rate = 96000
[[scenario.events]]
at_secs = 2.0
kind = "restart_daemon"
"#;
        let s = &load_dir(&dir_with(&[("e.toml", scn)])).unwrap()[0];
        assert_eq!(s.expect.compare, CompareMode::Tolerance { dbfs: -120.0 });
        assert_eq!(s.events[0].kind, EventKind::ForceRate { rate: 96_000 });
        assert_eq!(s.events[1].kind, EventKind::RestartDaemon);
        assert!(!s.measures_latency(), "event scenarios skip latency");
    }

    // Review Focus 1: typos must fail loudly.
    #[test]
    fn unknown_keys_are_rejected() {
        let src = FLAT.replace("channels =", "chanels =");
        let err = load_dir(&dir_with(&[("f.toml", &src)])).unwrap_err();
        assert!(format!("{err:#}").contains("chanels"), "{err:#}");
    }

    #[test]
    fn unknown_effect_name_is_rejected() {
        let scn = format!("{FLAT}[scenario.profile]\neffects = {{ fidelty = 0.5 }}\n");
        let err = load_dir(&dir_with(&[("g.toml", &scn)])).unwrap_err();
        assert!(format!("{err:#}").contains("fidelty"), "{err:#}");
    }

    #[test]
    fn event_with_missing_field_or_unknown_kind_is_rejected() {
        for ev in ["kind = \"force_rate\"", "kind = \"reboot\""] {
            let scn = format!("{FLAT}[[scenario.events]]\nat_secs = 1.0\n{ev}\n");
            assert!(load_dir(&dir_with(&[(&format!("ev-{}.toml", ev.len()), &scn)])).is_err(), "{ev}");
        }
    }

    #[test]
    fn more_than_32_bands_is_rejected_at_load() {
        let band = "[[scenario.profile.bands]]\nband_type = \"Peaking\"\nfreq = 1000.0\ngain_db = 1.0\nq = 1.0\nenabled = true\n";
        let scn = format!("{FLAT}{}", band.repeat(33));
        let err = load_dir(&dir_with(&[("h.toml", &scn)])).unwrap_err();
        assert!(format!("{err:#}").contains("32"), "{err:#}");
    }

    #[test]
    fn glob_matches_stars() {
        assert!(glob_match("flat@*x8", "flat@96000x8"));
        assert!(glob_match("*", "anything"));
        assert!(!glob_match("eq*", "flat@48000x2"));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p resonance-e2e scenario`. Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! Scenario files (`contrib/e2e/scenarios/*.toml`): schema, expansion of
//! rate/channel lists into concrete scenarios, and tier/platform selection.

use crate::compare::CompareMode;
use anyhow::{Context, Result, bail, ensure};
use resonance_ipc::{BandState, EffectsState};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Quick,
    Full,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowedHop {
    pub hop: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    #[serde(default)]
    pub compare: CompareMode,
    #[serde(default)]
    pub resample: Vec<AllowedHop>,
    #[serde(default = "default_gap_ms")]
    pub max_gap_ms: f64,
}

fn default_gap_ms() -> f64 {
    500.0
}

impl Default for Expect {
    fn default() -> Self {
        Self { compare: CompareMode::Exact, resample: Vec::new(), max_gap_ms: default_gap_ms() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EventKind {
    /// Pin the PipeWire graph to a new rate (devices created without a fixed
    /// rate follow it).
    ForceRate { rate: u32 },
    /// Point the daemon at `e2e_dev2` (created with this format).
    SwitchDevice { rate: u32, channels: usize },
    RestartDaemon,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "EventToml")]
pub struct Event {
    pub at_secs: f64,
    pub kind: EventKind,
}

/// Flat on-disk form: `deny_unknown_fields` cannot combine with
/// `serde(flatten)`, so the kind is a string checked in `TryFrom`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventToml {
    at_secs: f64,
    kind: String,
    rate: Option<u32>,
    channels: Option<usize>,
}

impl TryFrom<EventToml> for Event {
    type Error = String;

    fn try_from(t: EventToml) -> Result<Self, String> {
        let need = |v: Option<u32>, f: &str| v.ok_or_else(|| format!("event `{}` needs `{f}`", t.kind));
        let kind = match t.kind.as_str() {
            "force_rate" => EventKind::ForceRate { rate: need(t.rate, "rate")? },
            "switch_device" => EventKind::SwitchDevice {
                rate: need(t.rate, "rate")?,
                channels: t.channels.ok_or("event `switch_device` needs `channels`")?,
            },
            "restart_daemon" => EventKind::RestartDaemon,
            other => {
                return Err(format!(
                    "unknown event kind `{other}` (force_rate, switch_device, restart_daemon)"
                ));
            }
        };
        Ok(Self { at_secs: t.at_secs, kind })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileToml {
    #[serde(default)]
    preamp_db: f64,
    #[serde(default)]
    bands: Vec<BandState>,
    /// Effect name → intensity (0..=1, bipolar effects −1..=1); listed = on.
    #[serde(default)]
    effects: BTreeMap<String, f64>,
    #[serde(default)]
    dither_bits: Option<u32>,
    #[serde(default)]
    linear_phase: bool,
    /// WAV path relative to the scenario file, or `synthetic:room`.
    #[serde(default)]
    ir: Option<String>,
    /// EqualizerAPO `.txt` loaded instead of `bands` (reaches all 14 filter types).
    #[serde(default)]
    preset: Option<PathBuf>,
}

/// What the agent loads into the daemon before playing.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Profile {
    pub preamp_db: f64,
    pub bands: Vec<BandState>,
    pub effects: EffectsState,
    pub dither_bits: Option<u32>,
    pub linear_phase: bool,
    pub ir: Option<String>,
    pub preset: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioToml {
    id: String,
    tier: Tier,
    rates: Vec<u32>,
    channels: Vec<usize>,
    #[serde(default)]
    quick: Vec<(u32, usize)>,
    #[serde(default)]
    player_rate: Option<u32>,
    #[serde(default)]
    graph_rate: Option<u32>,
    #[serde(default = "default_body_secs")]
    body_secs: f64,
    #[serde(default)]
    platforms: Option<Vec<String>>,
    #[serde(default)]
    expected_fail: Option<String>,
    #[serde(default)]
    profile: Option<ProfileToml>,
    #[serde(default)]
    profile_file: Option<PathBuf>,
    #[serde(default)]
    expect: Expect,
    #[serde(default)]
    events: Vec<Event>,
}

fn default_body_secs() -> f64 {
    3.0
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileToml {
    scenario: Vec<ScenarioToml>,
}

#[derive(Debug, Clone)]
pub struct Scenario {
    /// `<base id>@<rate>x<channels>`.
    pub id: String,
    pub rate: u32,
    pub channels: usize,
    pub player_rate: u32,
    pub graph_rate: u32,
    pub body_secs: f64,
    pub in_quick: bool,
    pub platforms: Option<Vec<String>>,
    pub expected_fail: Option<String>,
    pub profile: Profile,
    pub expect: Expect,
    pub events: Vec<Event>,
}

impl Scenario {
    /// Latency is measured only on steady, matched-rate scenarios.
    #[must_use]
    pub fn measures_latency(&self) -> bool {
        self.events.is_empty() && self.player_rate == self.rate && self.graph_rate == self.rate
    }
}

fn effects_from(map: &BTreeMap<String, f64>) -> Result<EffectsState> {
    let mut e = EffectsState::default();
    for (name, &v) in map {
        let (i, on) = match name.as_str() {
            "fidelity" => (&mut e.fidelity_intensity, &mut e.fidelity_enabled),
            "ambience" => (&mut e.ambience_intensity, &mut e.ambience_enabled),
            "surround" => (&mut e.surround_intensity, &mut e.surround_enabled),
            "dynamic_boost" => (&mut e.dynamic_boost_intensity, &mut e.dynamic_boost_enabled),
            "bass" => (&mut e.bass_intensity, &mut e.bass_enabled),
            "crossfeed" => (&mut e.crossfeed_intensity, &mut e.crossfeed_enabled),
            other => bail!(
                "unknown effect `{other}` (fidelity, ambience, surround, dynamic_boost, bass, crossfeed)"
            ),
        };
        *i = v;
        *on = true;
    }
    Ok(e)
}

fn profile_from(t: ProfileToml, base: &Path) -> Result<Profile> {
    ensure!(
        t.bands.len() <= resonance_apo::state::MAX_FILTERS,
        "{} bands; snapshots hold at most 32",
        t.bands.len()
    );
    let ir = t.ir.map(|p| {
        if p.starts_with("synthetic:") {
            p
        } else {
            base.join(p).to_string_lossy().into_owned()
        }
    });
    Ok(Profile {
        preamp_db: t.preamp_db,
        bands: t.bands,
        effects: effects_from(&t.effects)?,
        dither_bits: t.dither_bits,
        linear_phase: t.linear_phase,
        ir,
        preset: t.preset.map(|p| base.join(p)),
    })
}

fn expand(s: ScenarioToml, dir: &Path) -> Result<Vec<Scenario>> {
    ensure!(!s.rates.is_empty() && !s.channels.is_empty(), "`{}`: rates and channels must be non-empty", s.id);
    ensure!(s.body_secs > 0.0, "`{}`: body_secs must be positive", s.id);
    let profile_toml = match (s.profile, s.profile_file) {
        (Some(_), Some(_)) => bail!("`{}`: use either [profile] or profile_file, not both", s.id),
        (Some(p), None) => p,
        (None, Some(f)) => {
            let path = dir.join(&f);
            let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?
        }
        (None, None) => ProfileToml::default(),
    };
    let profile = profile_from(profile_toml, dir).with_context(|| format!("scenario `{}`", s.id))?;
    let mut out = Vec::new();
    for &rate in &s.rates {
        for &channels in &s.channels {
            ensure!((1..=64).contains(&channels), "`{}`: channels must be 1..=64", s.id);
            out.push(Scenario {
                id: format!("{}@{rate}x{channels}", s.id),
                rate,
                channels,
                player_rate: s.player_rate.unwrap_or(rate),
                graph_rate: s.graph_rate.unwrap_or(rate),
                body_secs: s.body_secs,
                in_quick: s.tier == Tier::Quick || s.quick.contains(&(rate, channels)),
                platforms: s.platforms.clone(),
                expected_fail: s.expected_fail.clone(),
                profile: profile.clone(),
                expect: s.expect.clone(),
                events: s.events.clone(),
            });
        }
    }
    Ok(out)
}

/// Load every `*.toml` directly in `dir` (sorted by name; subdirectories
/// such as `profiles/` are not scenario files) and expand them.
pub fn load_dir(dir: &Path) -> Result<Vec<Scenario>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    let mut all = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).with_context(|| format!("read {}", f.display()))?;
        let parsed: FileToml = toml::from_str(&text).with_context(|| format!("parse {}", f.display()))?;
        for s in parsed.scenario {
            all.extend(expand(s, dir).with_context(|| format!("in {}", f.display()))?);
        }
    }
    Ok(all)
}

/// `*` matches any run of characters; everything else matches literally.
#[must_use]
pub fn glob_match(pat: &str, s: &str) -> bool {
    match pat.split_once('*') {
        None => pat == s,
        Some((head, rest)) => {
            s.starts_with(head)
                && (0..=s.len() - head.len()).any(|i| {
                    s.is_char_boundary(head.len() + i) && glob_match(rest, &s[head.len() + i..])
                })
        }
    }
}

#[must_use]
pub fn select<'a>(all: &'a [Scenario], tier: Tier, filter: Option<&str>, os: &str) -> Vec<&'a Scenario> {
    all.iter()
        .filter(|s| tier == Tier::Full || s.in_quick)
        .filter(|s| filter.is_none_or(|p| glob_match(p, &s.id)))
        .filter(|s| s.platforms.as_ref().is_none_or(|p| p.iter().any(|x| x == os)))
        .collect()
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p resonance-e2e scenario`
Expected: PASS (10 tests).

- [ ] **Step 5: `make check`, then commit**

```bash
git add crates/resonance-e2e/src/scenario.rs crates/resonance-e2e/src/lib.rs
git commit -m "feat(e2e): scenario files with list expansion, tiers and strict parsing"
```

### Task 12: Rate chain and resampling attribution

**Files:**
- Create: `crates/resonance-e2e/src/ratechain.rs`
- Modify: `crates/resonance-e2e/src/lib.rs` (`pub mod ratechain;`)

**Interfaces:**
- Consumes: `scenario::AllowedHop`
- Produces: `ratechain::{Hop { name: String, rate: u32 }, RateChain { hops: Vec<Hop> }, By::{Os, Resonance}, Resampling { step: String, from_rate: u32, to_rate: u32, by: By, reason: Option<String> }, RateChain::resamplings(&self, allowed: &[AllowedHop], resonance_steps: &[&str]) -> Vec<Resampling>, LINUX_RESONANCE_STEPS: &[&str]}`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn chain(rates: [u32; 5]) -> RateChain {
        let names = ["player", "graph", "capture", "dsp", "device"];
        RateChain { hops: names.iter().zip(rates).map(|(n, r)| Hop { name: (*n).into(), rate: r }).collect() }
    }

    #[test]
    fn matched_rates_never_resample() {
        assert!(chain([48_000; 5]).resamplings(&[], LINUX_RESONANCE_STEPS).is_empty());
    }

    #[test]
    fn declared_os_step_carries_its_reason() {
        let allowed = [AllowedHop { hop: "player->graph".into(), reason: "48 k content into 16 k device".into() }];
        let r = chain([48_000, 16_000, 16_000, 16_000, 16_000]).resamplings(&allowed, LINUX_RESONANCE_STEPS);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].step, "player->graph");
        assert_eq!((r[0].from_rate, r[0].to_rate, r[0].by), (48_000, 16_000, By::Os));
        assert_eq!(r[0].reason.as_deref(), Some("48 k content into 16 k device"));
    }

    #[test]
    fn undeclared_resonance_step_has_no_reason() {
        let r = chain([48_000, 48_000, 44_100, 48_000, 48_000]).resamplings(&[], LINUX_RESONANCE_STEPS);
        let cap = r.iter().find(|x| x.step == "capture->dsp").unwrap();
        assert_eq!(cap.by, By::Resonance);
        assert!(cap.reason.is_none());
    }
}
```

- [ ] **Step 2: Run to verify they fail.** `cargo test -p resonance-e2e ratechain` → compile errors.

- [ ] **Step 3: Implement**

```rust
//! The sample rate at every step of the audio path, and which steps resample.

use crate::scenario::AllowedHop;
use serde::Serialize;

/// Linux steps where Resonance itself (not PipeWire) would resample.
pub const LINUX_RESONANCE_STEPS: &[&str] = &["capture->dsp"];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hop {
    pub name: String,
    pub rate: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RateChain {
    pub hops: Vec<Hop>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum By {
    Os,
    Resonance,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Resampling {
    /// `"<from hop>-><to hop>"`.
    pub step: String,
    pub from_rate: u32,
    pub to_rate: u32,
    pub by: By,
    /// The scenario's declared reason; `None` = undeclared (a failure).
    pub reason: Option<String>,
}

impl RateChain {
    /// Every step whose rate changes, attributed, and matched against the
    /// scenario's declared resampling.
    #[must_use]
    pub fn resamplings(&self, allowed: &[AllowedHop], resonance_steps: &[&str]) -> Vec<Resampling> {
        self.hops
            .windows(2)
            .filter(|w| w[0].rate != w[1].rate)
            .map(|w| {
                let step = format!("{}->{}", w[0].name, w[1].name);
                Resampling {
                    from_rate: w[0].rate,
                    to_rate: w[1].rate,
                    by: if resonance_steps.contains(&step.as_str()) { By::Resonance } else { By::Os },
                    reason: allowed.iter().find(|a| a.hop == step).map(|a| a.reason.clone()),
                    step,
                }
            })
            .collect()
    }
}
```

- [ ] **Step 4: Run tests.** `cargo test -p resonance-e2e ratechain` → PASS (3).

- [ ] **Step 5: `make check`, then commit**

```bash
git add crates/resonance-e2e/src/ratechain.rs crates/resonance-e2e/src/lib.rs
git commit -m "feat(e2e): attribute every resampling step on the audio path"
```

### Task 13: Latency measurement, baselines and verdict

**Files:**
- Create: `crates/resonance-e2e/src/latency.rs`
- Modify: `crates/resonance-e2e/src/lib.rs` (`pub mod latency;`)

**Interfaces:**
- Produces: `latency::{median(Vec<f64>) -> Option<f64>, arrival_lags(recorded_ch0: &[f64], template: &[f64], emit: &[usize], max_lag: usize) -> Vec<isize>, LatencyVerdict::{Ok, Regressed { baseline_ms: f64 }, CanLower { baseline_ms: f64 }, NoBaseline}, judge(measured_ms: f64, baseline_ms: Option<f64>) -> LatencyVerdict, failure(verdict: LatencyVerdict, measured_ms: f64, update_baseline: bool) -> Option<String>, load_baselines(&Path) -> anyhow::Result<BTreeMap<String, f64>>, save_baselines(&Path, &BTreeMap<String, f64>) -> anyhow::Result<()>}`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::stimulus::chirp_train;

    #[test]
    fn median_handles_odd_even_and_empty() {
        assert_eq!(median(vec![3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(vec![4.0, 1.0, 3.0, 2.0]), Some(2.5));
        assert_eq!(median(vec![]), None);
    }

    #[test]
    fn arrival_lags_find_each_delayed_chirp() {
        let t = chirp_train(48_000, 1);
        let delay = 1234usize;
        let mut rec = vec![0.0f64; delay];
        rec.extend(t.samples.iter().map(|&s| f64::from(s)));
        let template: Vec<f64> = t.samples[t.emit[0]..t.emit[0] + t.chirp_frames].iter().map(|&s| f64::from(s)).collect();
        let lags = arrival_lags(&rec, &template, &t.emit, 20_000);
        assert_eq!(lags, vec![1234; 5]);
    }

    #[test]
    fn judge_uses_1ms_floor_and_10_percent_margin() {
        assert_eq!(judge(5.9, Some(5.0)), LatencyVerdict::Ok);
        assert_eq!(judge(6.1, Some(5.0)), LatencyVerdict::Regressed { baseline_ms: 5.0 });
        assert_eq!(judge(3.9, Some(5.0)), LatencyVerdict::CanLower { baseline_ms: 5.0 });
        assert_eq!(judge(188.0, Some(171.0)), LatencyVerdict::Ok); // 10 % of 171 = 17.1
        assert_eq!(judge(189.0, Some(171.0)), LatencyVerdict::Regressed { baseline_ms: 171.0 });
    }

    // Review Focus 5: a missing baseline is a failure unless updating.
    #[test]
    fn missing_baseline_fails_unless_updating() {
        let msg = failure(judge(4.0, None), 4.0, false).expect("must fail");
        assert!(msg.contains("--update-baseline"), "{msg}");
        assert!(failure(judge(4.0, None), 4.0, true).is_none());
        assert!(failure(LatencyVerdict::CanLower { baseline_ms: 9.0 }, 4.0, false).is_none());
    }

    #[test]
    fn baselines_round_trip_and_missing_file_is_empty() {
        let p = std::env::temp_dir().join(format!("resonance-e2e-bl-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&p);
        assert!(load_baselines(&p).unwrap().is_empty());
        let m = BTreeMap::from([("flat@48000x2".to_string(), 1.25)]);
        save_baselines(&p, &m).unwrap();
        assert_eq!(load_baselines(&p).unwrap(), m);
    }
}
```

- [ ] **Step 2: Run to verify they fail.** `cargo test -p resonance-e2e latency` → compile errors.

- [ ] **Step 3: Implement**

```rust
//! Latency: chirp arrival lags, committed per-scenario baselines, verdict.

use anyhow::{Context, Result};
use resonance_dsp::analysis::best_integer_lag;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

#[must_use]
pub fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let m = v.len() / 2;
    Some(if v.len() % 2 == 1 { v[m] } else { (v[m - 1] + v[m]) / 2.0 })
}

/// Arrival lag in frames of each chirp: `template` (the raw chirp) against
/// the recording window starting at each emission frame.
#[must_use]
pub fn arrival_lags(recorded_ch0: &[f64], template: &[f64], emit: &[usize], max_lag: usize) -> Vec<isize> {
    emit.iter()
        .map(|&e| {
            let end = (e + template.len() + max_lag).min(recorded_ch0.len());
            let seg = recorded_ch0.get(e..end).unwrap_or(&[]);
            best_integer_lag(template, seg, max_lag)
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum LatencyVerdict {
    Ok,
    Regressed { baseline_ms: f64 },
    CanLower { baseline_ms: f64 },
    NoBaseline,
}

/// Fail above baseline + max(1 ms, 10 %); report "can lower" below the same margin.
#[must_use]
pub fn judge(measured_ms: f64, baseline_ms: Option<f64>) -> LatencyVerdict {
    let Some(b) = baseline_ms else {
        return LatencyVerdict::NoBaseline;
    };
    let margin = (0.1 * b.abs()).max(1.0);
    if measured_ms > b + margin {
        LatencyVerdict::Regressed { baseline_ms: b }
    } else if measured_ms < b - margin {
        LatencyVerdict::CanLower { baseline_ms: b }
    } else {
        LatencyVerdict::Ok
    }
}

/// The failure message a verdict produces, if any.
#[must_use]
pub fn failure(verdict: LatencyVerdict, measured_ms: f64, update_baseline: bool) -> Option<String> {
    match verdict {
        LatencyVerdict::Regressed { baseline_ms } if !update_baseline => Some(format!(
            "added latency {measured_ms:.2} ms exceeds baseline {baseline_ms:.2} ms + margin"
        )),
        LatencyVerdict::NoBaseline if !update_baseline => Some(format!(
            "no latency baseline (measured {measured_ms:.2} ms); run with --update-baseline"
        )),
        _ => None,
    }
}

/// Scenario id → added latency (ms). A missing file is an empty map.
pub fn load_baselines(path: &Path) -> Result<BTreeMap<String, f64>> {
    match std::fs::read_to_string(path) {
        Ok(t) => toml::from_str(&t).with_context(|| format!("parse {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

pub fn save_baselines(path: &Path, m: &BTreeMap<String, f64>) -> Result<()> {
    let body = format!(
        "# Added latency (ms) per scenario. Rewritten by `cargo xtask e2e --update-baseline`.\n{}",
        toml::to_string(m)?
    );
    std::fs::write(path, body).with_context(|| format!("write {}", path.display()))
}
```

- [ ] **Step 4: Run tests.** `cargo test -p resonance-e2e latency` → PASS (5).

- [ ] **Step 5: `make check`, then commit**

```bash
git add crates/resonance-e2e/src/latency.rs crates/resonance-e2e/src/lib.rs
git commit -m "feat(e2e): latency arrival lags, baselines and regression verdict"
```

### Task 14: Report types and markdown

**Files:**
- Create: `crates/resonance-e2e/src/report.rs`
- Modify: `crates/resonance-e2e/src/lib.rs` (`pub mod report;`)

**Interfaces:**
- Consumes: `CompareOutcome`, `LatencyVerdict`, `RateChain`, `Resampling`
- Produces:
  - `Status::{Pass, Fail, ExpectedFail, UnexpectedPass, Flake}`
  - `settle(failures_empty: bool, expected_fail: bool) -> Status`
  - `ScenarioResult` (fields below), `ScenarioResult::new(id: &str, os: &str) -> Self`
  - `Report { os: String, tier: String, results: Vec<ScenarioResult> }`, `Report::failed(&self) -> bool`, `Report::to_markdown(&self) -> String`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ratechain::{By, Resampling};

    fn result(id: &str, status: Status) -> ScenarioResult {
        ScenarioResult { status, ..ScenarioResult::new(id, "linux") }
    }

    #[test]
    fn settle_covers_expected_fail_both_ways() {
        assert_eq!(settle(true, false), Status::Pass);
        assert_eq!(settle(false, false), Status::Fail);
        assert_eq!(settle(false, true), Status::ExpectedFail);
        assert_eq!(settle(true, true), Status::UnexpectedPass);
    }

    #[test]
    fn only_fail_and_unexpected_pass_fail_the_run() {
        for (s, failed) in [
            (Status::Pass, false),
            (Status::ExpectedFail, false),
            (Status::Flake, false),
            (Status::Fail, true),
            (Status::UnexpectedPass, true),
        ] {
            let r = Report { os: "linux".into(), tier: "quick".into(), results: vec![result("x", s)] };
            assert_eq!(r.failed(), failed, "{s:?}");
        }
    }

    #[test]
    fn markdown_has_grid_failures_and_resampling_section() {
        let mut bad = result("eq@48000x2", Status::Fail);
        bad.failures.push("first difference at frame 10 channel 1".into());
        let mut rs = result("sco@16000x1", Status::Pass);
        rs.resamplings.push(Resampling {
            step: "player->graph".into(), from_rate: 48_000, to_rate: 16_000, by: By::Os,
            reason: Some("48 k content into a 16 k headset".into()),
        });
        rs.resamplings.push(Resampling {
            step: "capture->dsp".into(), from_rate: 16_000, to_rate: 48_000, by: By::Resonance, reason: None,
        });
        let md = Report { os: "linux".into(), tier: "quick".into(), results: vec![bad, rs] }.to_markdown();
        assert!(md.contains("| eq@48000x2 | FAIL |"), "{md}");
        assert!(md.contains("first difference at frame 10 channel 1"));
        assert!(md.contains("## Resampling"));
        assert!(md.contains("48 k content into a 16 k headset"));
        assert!(md.contains("**undeclared**"));
    }
}
```

- [ ] **Step 2: Run to verify they fail.** → compile errors.

- [ ] **Step 3: Implement**

```rust
//! Per-scenario results, final status, and the human-readable report.

use crate::compare::CompareOutcome;
use crate::latency::LatencyVerdict;
use crate::ratechain::{By, RateChain, Resampling};
use serde::Serialize;
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Fail,
    ExpectedFail,
    UnexpectedPass,
    Flake,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "FAIL",
            Self::ExpectedFail => "expected-fail",
            Self::UnexpectedPass => "UNEXPECTED PASS",
            Self::Flake => "flake",
        }
    }
}

#[must_use]
pub fn settle(failures_empty: bool, expected_fail: bool) -> Status {
    match (failures_empty, expected_fail) {
        (true, false) => Status::Pass,
        (false, false) => Status::Fail,
        (false, true) => Status::ExpectedFail,
        (true, true) => Status::UnexpectedPass,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioResult {
    pub id: String,
    pub os: String,
    pub status: Status,
    pub failures: Vec<String>,
    pub expected_fail: Option<String>,
    pub compare: Option<CompareOutcome>,
    pub latency_on_ms: Option<f64>,
    pub latency_off_ms: Option<f64>,
    pub added_latency_ms: Option<f64>,
    pub latency_verdict: Option<LatencyVerdict>,
    pub rate_chain: Option<RateChain>,
    pub resamplings: Vec<Resampling>,
    pub discontinuities: u32,
    pub notes: Vec<String>,
    pub artifacts: Option<String>,
}

impl ScenarioResult {
    #[must_use]
    pub fn new(id: &str, os: &str) -> Self {
        Self {
            id: id.into(),
            os: os.into(),
            status: Status::Pass,
            failures: Vec::new(),
            expected_fail: None,
            compare: None,
            latency_on_ms: None,
            latency_off_ms: None,
            added_latency_ms: None,
            latency_verdict: None,
            rate_chain: None,
            resamplings: Vec::new(),
            discontinuities: 0,
            notes: Vec::new(),
            artifacts: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub os: String,
    pub tier: String,
    pub results: Vec<ScenarioResult>,
}

impl Report {
    #[must_use]
    pub fn failed(&self) -> bool {
        self.results.iter().any(|r| matches!(r.status, Status::Fail | Status::UnexpectedPass))
    }

    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut md = format!("# Resonance e2e — {} ({})\n\n", self.os, self.tier);
        let count = |s: Status| self.results.iter().filter(|r| r.status == s).count();
        let _ = writeln!(
            md,
            "{} pass · {} fail · {} expected-fail · {} unexpected-pass · {} flake\n",
            count(Status::Pass), count(Status::Fail), count(Status::ExpectedFail),
            count(Status::UnexpectedPass), count(Status::Flake)
        );
        md.push_str("| Scenario | Status | Added latency | Resampling |\n|---|---|---|---|\n");
        for r in &self.results {
            let lat = r.added_latency_ms.map_or_else(|| "—".into(), |ms| format!("{ms:.2} ms"));
            let rs = if r.resamplings.is_empty() {
                "none".to_string()
            } else {
                r.resamplings.iter().map(|x| x.step.clone()).collect::<Vec<_>>().join(", ")
            };
            let _ = writeln!(md, "| {} | {} | {lat} | {rs} |", r.id, r.status.label());
        }
        let failing: Vec<_> = self.results.iter().filter(|r| !r.failures.is_empty()).collect();
        if !failing.is_empty() {
            md.push_str("\n## Failures\n\n");
            for r in failing {
                let tag = r.expected_fail.as_deref().map_or_else(String::new, |f| format!(" (expected: {f})"));
                let _ = writeln!(md, "### {}{tag}\n", r.id);
                for f in &r.failures {
                    let _ = writeln!(md, "- {f}");
                }
                if let Some(a) = &r.artifacts {
                    let _ = writeln!(md, "- artifacts: `{a}`");
                }
                md.push('\n');
            }
        }
        md.push_str("\n## Resampling\n\n");
        let mut any = false;
        for r in &self.results {
            for x in &r.resamplings {
                any = true;
                let by = match x.by {
                    By::Os => "OS",
                    By::Resonance => "Resonance",
                };
                let why = x.reason.as_deref().map_or_else(|| "**undeclared**".into(), str::to_owned);
                let _ = writeln!(md, "- `{}`: {} {} → {} Hz by {by}: {why}", r.id, x.step, x.from_rate, x.to_rate);
            }
        }
        if !any {
            md.push_str("No step resampled in any scenario.\n");
        }
        md
    }
}
```

- [ ] **Step 4: Run tests.** `cargo test -p resonance-e2e report` → PASS (3).

- [ ] **Step 5: `make check`, then commit**

```bash
git add crates/resonance-e2e/src/report.rs crates/resonance-e2e/src/lib.rs
git commit -m "feat(e2e): scenario results, status settling and markdown report"
```

### Task 15: Pitch, band-gain, gap and still-flowing checks

**Files:**
- Create: `crates/resonance-e2e/src/checks.rs`
- Modify: `crates/resonance-e2e/src/lib.rs` (`pub mod checks;`)

**Interfaces:**
- Produces: `checks::{pitch_error(samples_ch0: &[f32], rate: f64) -> f64, PITCH_TOLERANCE: f64, octave_edges(max_hz: f64) -> Vec<f64>, gain_db(input: &[f64], input_rate: f64, output: &[f64], output_rate: f64, edges: &[f64]) -> Vec<f64>, BAND_TOLERANCE_DB: f64, first_signal_frame(x: &[f32], channels: usize) -> Option<usize>, last_signal_frame(x: &[f32], channels: usize) -> Option<usize>, longest_zero_run(x: &[f32], channels: usize, frames: Range<usize>) -> usize, is_flowing(x: &[f32], channels: usize, frames: Range<usize>) -> bool}`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::stimulus::{PILOT_HZ, generate};

    fn ch0(x: &[f32], ch: usize) -> Vec<f64> {
        x.iter().step_by(ch).map(|&v| f64::from(v)).collect()
    }

    #[test]
    fn pitch_error_is_zero_for_the_stimulus_and_catches_the_44k1_48k_bug() {
        let s = generate(48_000, 1, 3.0);
        let body = &s.samples[s.body.clone()];
        assert!(pitch_error(body, 48_000.0) < PITCH_TOLERANCE);
        // Captured at 44.1 k, replayed as 48 k: pilot reads 8.8 % sharp.
        assert!(pitch_error(body, 48_000.0 * 48_000.0 / 44_100.0) > 0.05);
    }

    #[test]
    fn unity_path_has_zero_gain_and_doubling_reads_6db() {
        let s = generate(48_000, 1, 3.0);
        let x = ch0(&s.samples[s.body.clone()], 1);
        let edges = octave_edges(20_000.0);
        assert!(gain_db(&x, 48_000.0, &x, 48_000.0, &edges).iter().all(|g| g.abs() < 1e-9));
        let louder: Vec<f64> = x.iter().map(|v| v * 2.0).collect();
        let g = gain_db(&x, 48_000.0, &louder, 48_000.0, &edges);
        assert!(g.iter().all(|g| (g - 6.0206).abs() < 0.01), "{g:?}");
    }

    #[test]
    fn signal_edges_skip_leading_and_trailing_silence() {
        let mut x = vec![0.0f32; 2 * 100];
        x[2 * 10 + 1] = 0.3; // channel 1 only
        x[2 * 70] = -0.2;
        assert_eq!(first_signal_frame(&x, 2), Some(10));
        assert_eq!(last_signal_frame(&x, 2), Some(70));
        assert_eq!(first_signal_frame(&[0.0; 8], 2), None);
    }

    #[test]
    fn zero_runs_and_flow() {
        let mut x = vec![0.5f32; 2 * 1000];
        for v in &mut x[2 * 400..2 * 650] {
            *v = 0.0;
        }
        assert_eq!(longest_zero_run(&x, 2, 0..1000), 250);
        assert!(is_flowing(&x, 2, 900..1000));
        assert!(!is_flowing(&x, 2, 400..650));
    }

    #[test]
    fn octave_edges_start_at_125_hz_and_stop_below_max() {
        let e = octave_edges(10_000.0);
        assert!((e[0] - 125.0).abs() < 1e-9);
        assert!(*e.last().unwrap() <= 10_000.0);
    }
}
```

- [ ] **Step 2: Run to verify they fail.** → compile errors.

- [ ] **Step 3: Implement**

```rust
//! Checks for scenarios that cannot be bit-exact (resampling, events).

use crate::stimulus::PILOT_HZ;
use resonance_dsp::analysis::{band_levels_db, fft_peak_hz};
use std::ops::Range;

/// ±0.01 %: 1/6 of a cent. The 44.1↔48 kHz pitch bug is 8.8 %.
pub const PITCH_TOLERANCE: f64 = 1e-4;
pub const BAND_TOLERANCE_DB: f64 = 0.1;

/// Relative pilot-frequency error of a mono recording at `rate`.
#[must_use]
pub fn pitch_error(samples_ch0: &[f32], rate: f64) -> f64 {
    ((fft_peak_hz(samples_ch0, rate, PILOT_HZ) - PILOT_HZ) / PILOT_HZ).abs()
}

/// Octave band edges from 125 Hz up to `max_hz` (below 125 Hz a 3 s window
/// has too few bins for a ±0.1 dB estimate).
#[must_use]
pub fn octave_edges(max_hz: f64) -> Vec<f64> {
    std::iter::successors(Some(125.0), |&f| Some(f * 2.0)).take_while(|&f| f <= max_hz).collect()
}

/// Per-band gain (dB) of a path: level of `output` minus level of the
/// `input` that produced it. Levels are density-normalised, so input and
/// output may be at different sample rates (a resampling path). Compare a
/// live path's gain with the offline render's gain over the same bands.
#[must_use]
pub fn gain_db(input: &[f64], input_rate: f64, output: &[f64], output_rate: f64, edges: &[f64]) -> Vec<f64> {
    let li = band_levels_db(input, input_rate, edges);
    let lo = band_levels_db(output, output_rate, edges);
    lo.iter().zip(&li).map(|(o, i)| o - i).collect()
}

fn frame_is_zero(x: &[f32], channels: usize, f: usize) -> bool {
    x[f * channels..(f + 1) * channels].iter().all(|&v| v == 0.0)
}

/// First frame with any non-zero channel (where the stimulus arrived).
#[must_use]
pub fn first_signal_frame(x: &[f32], channels: usize) -> Option<usize> {
    (0..x.len() / channels).find(|&f| !frame_is_zero(x, channels, f))
}

/// Last frame with any non-zero channel.
#[must_use]
pub fn last_signal_frame(x: &[f32], channels: usize) -> Option<usize> {
    (0..x.len() / channels).rev().find(|&f| !frame_is_zero(x, channels, f))
}

/// Longest run of all-channel digital silence inside `frames`, in frames.
#[must_use]
pub fn longest_zero_run(x: &[f32], channels: usize, frames: Range<usize>) -> usize {
    let end = frames.end.min(x.len() / channels);
    let (mut best, mut cur) = (0, 0);
    for f in frames.start..end {
        cur = if frame_is_zero(x, channels, f) { cur + 1 } else { 0 };
        best = best.max(cur);
    }
    best
}

/// At least half the frames in `frames` carry signal.
#[must_use]
pub fn is_flowing(x: &[f32], channels: usize, frames: Range<usize>) -> bool {
    let end = frames.end.min(x.len() / channels);
    let n = end.saturating_sub(frames.start);
    n > 0 && (frames.start..end).filter(|&f| !frame_is_zero(x, channels, f)).count() * 2 >= n
}
```

- [ ] **Step 4: Run tests.** `cargo test -p resonance-e2e checks` → PASS (5).

(`== 0.0` comparisons are fine under the repo's active `float_cmp` lint, which exempts comparisons with zero.)

- [ ] **Step 5: `make check`, then commit**

```bash
git add crates/resonance-e2e/src/checks.rs crates/resonance-e2e/src/lib.rs
git commit -m "feat(e2e): pitch, band-gain, gap and flow checks"
```

### Task 16: Linux environment control (devices, graph rate, state, daemon)

**Files:**
- Create: `crates/resonance-e2e/src/linux/mod.rs`, `crates/resonance-e2e/src/linux/env.rs`
- Modify: `crates/resonance-e2e/src/lib.rs` (`#[cfg(target_os = "linux")] pub mod linux;`)

**Interfaces:**
- Consumes: `resonance_ipc::{DaemonState, transport::is_reachable}`
- Produces, in `linux::env`:
  - constants `DEVICE = "e2e_dev"`, `DEVICE2 = "e2e_dev2"`, `RESONANCE_SINK = "resonance"`, `PROCESSOR = "resonance-processor"`
  - `channel_positions(usize) -> Vec<(String, u32)>`
  - `create_device(name: &str, rate: Option<u32>, channels: usize) -> Result<()>`, `destroy_device(&str) -> Result<()>`
  - `force_graph_rate(Option<u32>) -> Result<()>`, `set_default_sink(&str) -> Result<()>`
  - `pw_dump() -> Result<serde_json::Value>`, `node_id(&Value, &str) -> Option<u64>`, `default_sink(&Value) -> Option<String>`, `node_rate(&Value, &str) -> Option<u32>`
  - `Daemon { pid: u32 }` with `Daemon::start(bin: &Path, log: &Path) -> Result<Daemon>`, `Daemon::stop(self) -> Result<()>`
  - `daemon_running() -> bool`
  - `verify_off(device: &str) -> Result<()>`, `verify_on(device: &str, state: &DaemonState, rate: u32, channels: usize) -> Result<()>`
  - `check_on_state(daemon_channels: usize, daemon_rate: f64, daemon_output: Option<&str>, device: &str, rate: u32, channels: usize) -> Result<()>`
  - `wait_for(what: &str, timeout: Duration, f: impl FnMut() -> Result<bool>) -> Result<()>`

- [ ] **Step 1: Write the failing tests** (pure parsing and verification logic; fixtures mimic `pw-dump`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dump(default: &str, nodes: &[(&str, Option<u32>)]) -> serde_json::Value {
        let mut v: Vec<serde_json::Value> = nodes.iter().enumerate().map(|(i, (name, rate))| {
            let mut props = json!({ "node.name": name, "media.class": "Audio/Sink" });
            if let Some(r) = rate { props["audio.rate"] = json!(r); }
            json!({ "id": 40 + i, "type": "PipeWire:Interface:Node", "info": { "props": props } })
        }).collect();
        v.push(json!({
            "id": 0, "type": "PipeWire:Interface:Metadata", "props": { "metadata.name": "default" },
            "metadata": [{ "subject": 0, "key": "default.audio.sink", "value": { "name": default } }]
        }));
        serde_json::Value::Array(v)
    }

    #[test]
    fn parses_nodes_default_sink_and_rate() {
        let d = dump("e2e_dev", &[("e2e_dev", Some(96_000)), ("resonance", None)]);
        assert_eq!(node_id(&d, "e2e_dev"), Some(40));
        assert_eq!(node_id(&d, "missing"), None);
        assert_eq!(default_sink(&d).as_deref(), Some("e2e_dev"));
        assert_eq!(node_rate(&d, "e2e_dev"), Some(96_000));
    }

    #[test]
    fn channel_positions_cover_standard_layouts_and_aux() {
        let names = |n| channel_positions(n).into_iter().map(|(s, _)| s).collect::<Vec<_>>().join(" ");
        assert_eq!(names(1), "MONO");
        assert_eq!(names(2), "FL FR");
        assert_eq!(names(8), "FL FR FC LFE RL RR SL SR");
        assert_eq!(channel_positions(16).len(), 16);
        assert!(names(16).starts_with("AUX0 AUX1"));
    }

    // Review Focus 4: daemon still on the previous device format.
    #[test]
    fn on_state_rejects_a_daemon_on_the_wrong_format_or_device() {
        let dev = Some("e2e_dev");
        assert!(check_on_state(8, 96_000.0, dev, "e2e_dev", 96_000, 8).is_ok());
        let e = check_on_state(2, 96_000.0, dev, "e2e_dev", 96_000, 8).unwrap_err();
        assert!(format!("{e}").contains("2 ch"), "{e}");
        assert!(check_on_state(8, 48_000.0, dev, "e2e_dev", 96_000, 8).is_err());
        assert!(check_on_state(8, 96_000.0, Some("other"), "e2e_dev", 96_000, 8).is_err());
        assert!(check_on_state(8, 96_000.0, None, "e2e_dev", 96_000, 8).is_err());
    }
}
```

- [ ] **Step 2: Run to verify they fail.** `cargo test -p resonance-e2e linux::env` → compile errors.

- [ ] **Step 3: Implement**

`crates/resonance-e2e/src/linux/mod.rs`:

```rust
//! Linux (PipeWire) backend of the e2e agent.

pub mod env;
pub mod pw;
```

In Task 16, create `pw.rs` as an empty file with a `//!` doc line. Task 17 fills it.

`crates/resonance-e2e/src/linux/env.rs`:

```rust
//! Devices, graph rate, PipeWire state and the daemon process, all inside
//! the e2e container. Never run this against a desktop session.

use anyhow::{Context, Result, bail, ensure};
use resonance_ipc::DaemonState;
use serde_json::Value;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const DEVICE: &str = "e2e_dev";
pub const DEVICE2: &str = "e2e_dev2";
/// The daemon's routable null sink (`audio/pipewire.rs` `create_null_sink`).
pub const RESONANCE_SINK: &str = "resonance";
pub const PROCESSOR: &str = "resonance-processor";

fn run(cmd: &mut Command) -> Result<String> {
    let out = cmd.output().with_context(|| format!("spawn {cmd:?}"))?;
    ensure!(out.status.success(), "{cmd:?} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn wait_for(what: &str, timeout: Duration, mut f: impl FnMut() -> Result<bool>) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f()? {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("timed out after {timeout:?} waiting for {what}")
}

/// SPA channel position names and ids for a width.
#[must_use]
pub fn channel_positions(channels: usize) -> Vec<(String, u32)> {
    use pipewire::spa::sys as s;
    let named: &[(&str, u32)] = match channels {
        1 => &[("MONO", s::SPA_AUDIO_CHANNEL_MONO)],
        2 => &[("FL", s::SPA_AUDIO_CHANNEL_FL), ("FR", s::SPA_AUDIO_CHANNEL_FR)],
        6 => &[
            ("FL", s::SPA_AUDIO_CHANNEL_FL), ("FR", s::SPA_AUDIO_CHANNEL_FR), ("FC", s::SPA_AUDIO_CHANNEL_FC),
            ("LFE", s::SPA_AUDIO_CHANNEL_LFE), ("RL", s::SPA_AUDIO_CHANNEL_RL), ("RR", s::SPA_AUDIO_CHANNEL_RR),
        ],
        8 => &[
            ("FL", s::SPA_AUDIO_CHANNEL_FL), ("FR", s::SPA_AUDIO_CHANNEL_FR), ("FC", s::SPA_AUDIO_CHANNEL_FC),
            ("LFE", s::SPA_AUDIO_CHANNEL_LFE), ("RL", s::SPA_AUDIO_CHANNEL_RL), ("RR", s::SPA_AUDIO_CHANNEL_RR),
            ("SL", s::SPA_AUDIO_CHANNEL_SL), ("SR", s::SPA_AUDIO_CHANNEL_SR),
        ],
        _ => &[],
    };
    if named.is_empty() {
        (0..channels).map(|i| (format!("AUX{i}"), s::SPA_AUDIO_CHANNEL_AUX0 + i as u32)).collect()
    } else {
        named.iter().map(|(n, id)| ((*n).to_string(), *id)).collect()
    }
}

/// A null sink acting as the "hardware" device. `rate: None` follows the graph.
pub fn create_device(name: &str, rate: Option<u32>, channels: usize) -> Result<()> {
    let pos: Vec<String> = channel_positions(channels).into_iter().map(|(n, _)| n).collect();
    let rate_prop = rate.map(|r| format!(" audio.rate={r}")).unwrap_or_default();
    let props = format!(
        "{{ factory.name=support.null-audio-sink node.name={name} node.description={name} \
         media.class=Audio/Sink object.linger=true audio.format=F32 audio.channels={channels}{rate_prop} \
         audio.position=[{}] monitor.channel-volumes=false }}",
        pos.join(" ")
    );
    run(Command::new("pw-cli").args(["create-node", "adapter", &props]))?;
    wait_for(&format!("device {name}"), Duration::from_secs(5), || Ok(node_id(&pw_dump()?, name).is_some()))
}

pub fn destroy_device(name: &str) -> Result<()> {
    if let Some(id) = node_id(&pw_dump()?, name) {
        run(Command::new("pw-cli").args(["destroy", &id.to_string()]))?;
    }
    wait_for(&format!("device {name} gone"), Duration::from_secs(5), || Ok(node_id(&pw_dump()?, name).is_none()))
}

pub fn force_graph_rate(rate: Option<u32>) -> Result<()> {
    let r = rate.unwrap_or(0).to_string();
    run(Command::new("pw-metadata").args(["-n", "settings", "0", "clock.force-rate", &r]))?;
    Ok(())
}

pub fn set_default_sink(name: &str) -> Result<()> {
    let v = format!("{{ \"name\": \"{name}\" }}");
    run(Command::new("pw-metadata").args(["0", "default.configured.audio.sink", &v]))?;
    wait_for(&format!("default sink {name}"), Duration::from_secs(5), || {
        Ok(default_sink(&pw_dump()?).as_deref() == Some(name))
    })
}

pub fn pw_dump() -> Result<Value> {
    serde_json::from_str(&run(&mut Command::new("pw-dump"))?).context("parse pw-dump")
}

fn nodes(dump: &Value) -> impl Iterator<Item = &Value> {
    dump.as_array().into_iter().flatten().filter(|o| o["type"] == "PipeWire:Interface:Node")
}

#[must_use]
pub fn node_id(dump: &Value, name: &str) -> Option<u64> {
    nodes(dump).find(|n| n["info"]["props"]["node.name"] == name).and_then(|n| n["id"].as_u64())
}

#[must_use]
pub fn node_rate(dump: &Value, name: &str) -> Option<u32> {
    let n = nodes(dump).find(|n| n["info"]["props"]["node.name"] == name)?;
    n["info"]["props"]["audio.rate"].as_u64()
        .or_else(|| n["info"]["params"]["Format"][0]["rate"].as_u64())
        .and_then(|r| u32::try_from(r).ok())
}

#[must_use]
pub fn default_sink(dump: &Value) -> Option<String> {
    dump.as_array()?
        .iter()
        .filter(|o| o["type"] == "PipeWire:Interface:Metadata" && o["props"]["metadata.name"] == "default")
        .flat_map(|o| o["metadata"].as_array().into_iter().flatten())
        .find(|m| m["key"] == "default.audio.sink")
        .and_then(|m| m["value"]["name"].as_str().map(str::to_owned))
}

#[must_use]
pub fn daemon_running() -> bool {
    std::fs::read_dir("/proc").into_iter().flatten().flatten().any(|e| {
        std::fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == "resonanced")
    })
}

pub struct Daemon {
    pid: u32,
}

impl Daemon {
    /// Start `resonanced` (stdout+stderr appended to `log`) and wait for its socket.
    pub fn start(bin: &Path, log: &Path) -> Result<Self> {
        let out = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
        let child = Command::new(bin)
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .spawn()
            .with_context(|| format!("start {}", bin.display()))?;
        let d = Self { pid: child.id() };
        wait_for("daemon socket", Duration::from_secs(10), || Ok(resonance_ipc::transport::is_reachable()))?;
        Ok(d)
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// SIGTERM, then wait until no `resonanced` process remains.
    pub fn stop(self) -> Result<()> {
        run(Command::new("kill").args(["-TERM", &self.pid.to_string()]))?;
        wait_for("daemon exit", Duration::from_secs(10), || Ok(!daemon_running()))
    }
}

/// Resonance verifiably absent: no process, no nodes, default sink = device.
pub fn verify_off(device: &str) -> Result<()> {
    ensure!(!daemon_running(), "a resonanced process is still running");
    let d = pw_dump()?;
    ensure!(node_id(&d, RESONANCE_SINK).is_none(), "\"Resonance EQ\" node still present");
    ensure!(node_id(&d, PROCESSOR).is_none(), "\"{PROCESSOR}\" node still present");
    let def = default_sink(&d);
    ensure!(def.as_deref() == Some(device), "default sink is {def:?}, want {device}");
    Ok(())
}

/// The daemon's reported format and output must match the scenario before
/// anything is measured.
pub fn check_on_state(
    daemon_channels: usize,
    daemon_rate: f64,
    daemon_output: Option<&str>,
    device: &str,
    rate: u32,
    channels: usize,
) -> Result<()> {
    ensure!(daemon_channels == channels, "daemon runs {daemon_channels} ch, scenario wants {channels}");
    ensure!(
        (daemon_rate - f64::from(rate)).abs() < 0.5,
        "daemon DSP rate {daemon_rate} Hz, scenario wants {rate}"
    );
    ensure!(daemon_output == Some(device), "daemon outputs to {daemon_output:?}, want {device}");
    Ok(())
}

/// Resonance verifiably in the path: process, sink is default, state matches.
pub fn verify_on(device: &str, state: &DaemonState, rate: u32, channels: usize) -> Result<()> {
    ensure!(daemon_running(), "no resonanced process");
    let d = pw_dump()?;
    ensure!(node_id(&d, RESONANCE_SINK).is_some(), "\"Resonance EQ\" node missing");
    let def = default_sink(&d);
    ensure!(def.as_deref() == Some(RESONANCE_SINK), "default sink is {def:?}, want {RESONANCE_SINK}");
    check_on_state(state.channels, state.sample_rate, state.active_output.as_deref(), device, rate, channels)
}
```

- [ ] **Step 4: Run tests.** `cargo test -p resonance-e2e linux::env` → PASS (3). These tests are pure; they parse JSON fixtures and never touch PipeWire.

- [ ] **Step 5: `make check`, then commit**

```bash
git add crates/resonance-e2e/src/linux crates/resonance-e2e/src/lib.rs
git commit -m "feat(e2e): linux device, graph-rate, state and daemon control"
```

### Task 17: Native PipeWire play + multi-record on one graph clock

**Files:**
- Modify: `crates/resonance-e2e/src/linux/pw.rs`

**Interfaces:**
- Consumes: `env::channel_positions`
- Produces:
  - `pw::Play<'a> { node: &'a str, rate: u32, channels: usize, samples: &'a [f32] }`
  - `pw::RecordTarget<'a> { node: &'a str, rate: u32, channels: usize }`
  - `pw::Timed { at_frame: usize, action: Box<dyn FnOnce() + Send> }`
  - `pw::Recording { node: String, channels: usize, rate: u32, first_tick: Option<u64>, samples: Vec<f32>, discontinuities: u32 }`
  - `pw::PlayRec { play_first_tick: Option<u64>, graph_rate: u32, play_discontinuities: u32, recordings: Vec<Recording> }`
  - `pw::play_and_record(play: &Play<'_>, record: &[RecordTarget<'_>], tail_frames: usize, events: Vec<Timed>, timeout: Duration) -> anyhow::Result<PlayRec>`

This module is only testable against a live PipeWire. Its test is the harness self-check in Task 18, which runs first in every container run, plus Task 19's container smoke run.

- [ ] **Step 1: Implement**

```rust
//! Play into one PipeWire node while recording other nodes' monitors, all in
//! one process on one graph clock. Positions come from `pw_stream_get_time`
//! ticks, so latency is exact; a tick jump between callbacks is an
//! OS-reported discontinuity (the flake rule's evidence).

use anyhow::{Result, bail};
use pipewire as pw;
use pw::{properties::properties, spa};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

pub struct Play<'a> {
    pub node: &'a str,
    pub rate: u32,
    pub channels: usize,
    pub samples: &'a [f32],
}

pub struct RecordTarget<'a> {
    pub node: &'a str,
    pub rate: u32,
    pub channels: usize,
}

/// An action run on its own thread once playback passes `at_frame`.
pub struct Timed {
    pub at_frame: usize,
    pub action: Box<dyn FnOnce() + Send>,
}

#[derive(Debug, Clone)]
pub struct Recording {
    pub node: String,
    pub channels: usize,
    pub rate: u32,
    pub first_tick: Option<u64>,
    pub samples: Vec<f32>,
    pub discontinuities: u32,
}

#[derive(Debug, Clone)]
pub struct PlayRec {
    pub play_first_tick: Option<u64>,
    pub graph_rate: u32,
    pub play_discontinuities: u32,
    pub recordings: Vec<Recording>,
}

/// Tick continuity tracker for one stream.
#[derive(Default)]
struct Clock {
    first: Option<u64>,
    last: Option<(u64, u64)>, // (ticks, expected tick advance)
    discontinuities: u32,
}

impl Clock {
    /// Record a callback at `t` that moved `frames` frames at `stream_rate`.
    /// Returns the graph rate (ticks per second) when known.
    fn tick(&mut self, t: &pw::stream::Time, frames: u64, stream_rate: u32) -> Option<u32> {
        let r = t.rate();
        let tps = (r.num > 0).then(|| u64::from(r.denom) / u64::from(r.num))?;
        let ticks = t.ticks();
        self.first.get_or_insert(ticks);
        if let Some((pt, adv)) = self.last {
            if adv > 0 && ticks != pt + adv {
                self.discontinuities += 1;
            }
        }
        // Expected advance in ticks; 0 = not exact (stream rate ≠ graph rate).
        let num = frames * tps;
        let adv = if num % u64::from(stream_rate) == 0 { num / u64::from(stream_rate) } else { 0 };
        self.last = Some((ticks, adv));
        u32::try_from(tps).ok()
    }
}

struct Shared {
    cursor: usize,
    play: Clock,
    recs: Vec<(Clock, Vec<f32>)>,
    graph_rate: u32,
}

fn format_param(rate: u32, channels: usize) -> Vec<u8> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(rate);
    info.set_channels(channels as u32);
    let mut pos = [0u32; spa::param::audio::MAX_CHANNELS];
    for (slot, (_, id)) in pos.iter_mut().zip(super::env::channel_positions(channels)) {
        *slot = id;
    }
    info.set_position(pos);
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(obj))
        .expect("serialize audio format")
        .0
        .into_inner()
}

#[allow(clippy::too_many_lines)] // one linear setup → run → collect pass
pub fn play_and_record(
    play: &Play<'_>,
    record: &[RecordTarget<'_>],
    tail_frames: usize,
    mut events: Vec<Timed>,
    timeout: Duration,
) -> Result<PlayRec> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let src_frames = play.samples.len() / play.channels;
    let wants: Vec<usize> = record
        .iter()
        .map(|t| ((src_frames + tail_frames) as u64 * u64::from(t.rate) / u64::from(play.rate)) as usize)
        .collect();
    let shared = Rc::new(RefCell::new(Shared {
        cursor: 0,
        play: Clock::default(),
        recs: record.iter().map(|_| (Clock::default(), Vec::new())).collect(),
        graph_rate: 0,
    }));
    events.sort_by_key(|e| e.at_frame);
    let events = Rc::new(RefCell::new(VecDeque::from(events)));
    let source: Rc<[f32]> = play.samples.into();

    let mut keep = Vec::new();
    // Capture streams first, so they are linked before the first played frame.
    for (j, t) in record.iter().enumerate() {
        let props = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Production",
            *pw::keys::TARGET_OBJECT => t.node,
            *pw::keys::STREAM_CAPTURE_SINK => "true",
            "node.dont-reconnect" => "true",
        };
        let stream = pw::stream::StreamBox::new(&core, &format!("e2e-record-{j}"), props)?;
        let (sh, ml, want, ch, rate) = (Rc::clone(&shared), mainloop.clone(), wants.clone(), t.channels, t.rate);
        let listener = stream
            .add_local_listener_with_user_data(())
            .process(move |stream, ()| {
                let Some(mut buffer) = stream.dequeue_buffer() else { return };
                let datas = buffer.datas_mut();
                let Some(data) = datas.first_mut() else { return };
                let size = data.chunk().size() as usize;
                let Some(bytes) = data.data() else { return };
                let frames = size / (4 * ch);
                let mut s = sh.borrow_mut();
                if let Ok(t) = stream.time() {
                    if let Some(gr) = s.recs[j].0.tick(&t, frames as u64, rate) {
                        s.graph_rate = gr;
                    }
                }
                s.recs[j].1.extend(
                    bytes[..frames * ch * 4].chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                );
                let done = s.recs.iter().zip(&want).all(|((_, v), &w)| v.len() / ch.max(1) >= w)
                    && s.cursor >= src_frames;
                drop(s);
                if done {
                    ml.quit();
                }
            })
            .register()?;
        let fmt = format_param(t.rate, t.channels);
        let mut params = [spa::pod::Pod::from_bytes(&fmt).expect("format pod")];
        stream.connect(
            spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )?;
        keep.push((stream, listener));
    }

    let props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Playback",
        *pw::keys::MEDIA_ROLE => "Production",
        *pw::keys::TARGET_OBJECT => play.node,
        "node.dont-reconnect" => "true",
    };
    let stream = pw::stream::StreamBox::new(&core, "e2e-play", props)?;
    let (sh, ev, src, ch, rate) = (Rc::clone(&shared), Rc::clone(&events), Rc::clone(&source), play.channels, play.rate);
    let listener = stream
        .add_local_listener_with_user_data(())
        .process(move |stream, ()| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let requested = buffer.requested() as usize;
            let datas = buffer.datas_mut();
            let Some(data) = datas.first_mut() else { return };
            let stride = 4 * ch;
            let Some(bytes) = data.data() else { return };
            let cap = bytes.len() / stride;
            let n = if requested > 0 { requested.min(cap) } else { cap };
            let mut s = sh.borrow_mut();
            for f in 0..n {
                for c in 0..ch {
                    let v = src.get((s.cursor + f) * ch + c).copied().unwrap_or(0.0);
                    bytes[f * stride + c * 4..f * stride + c * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
            }
            if let Ok(t) = stream.time() {
                s.play.tick(&t, n as u64, rate);
            }
            s.cursor += n;
            let cursor = s.cursor;
            drop(s);
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as _;
            *chunk.size_mut() = (stride * n) as _;
            let mut q = ev.borrow_mut();
            while q.front().is_some_and(|e| e.at_frame <= cursor) {
                let e = q.pop_front().expect("front checked");
                std::thread::spawn(e.action);
            }
        })
        .register()?;
    let fmt = format_param(play.rate, play.channels);
    let mut params = [spa::pod::Pod::from_bytes(&fmt).expect("format pod")];
    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    keep.push((stream, listener));

    let started = Instant::now();
    let timed_out = Rc::new(RefCell::new(false));
    let (ml, to) = (mainloop.clone(), Rc::clone(&timed_out));
    let timer = mainloop.loop_().add_timer(move |_| {
        if started.elapsed() > timeout {
            *to.borrow_mut() = true;
            ml.quit();
        }
    });
    timer.update_timer(Some(Duration::from_millis(100)), Some(Duration::from_millis(100))).into_result()?;
    mainloop.run();
    drop(keep);

    let s = shared.borrow();
    if *timed_out.borrow() {
        let got: Vec<usize> = s.recs.iter().zip(record).map(|((_, v), t)| v.len() / t.channels).collect();
        bail!("timed out after {timeout:?}: played {} of {src_frames} frames; recorded {got:?} of {wants:?}", s.cursor);
    }
    Ok(PlayRec {
        play_first_tick: s.play.first,
        graph_rate: s.graph_rate,
        play_discontinuities: s.play.discontinuities,
        recordings: s
            .recs
            .iter()
            .zip(record)
            .zip(&wants)
            .map(|(((clock, v), t), &w)| Recording {
                node: t.node.to_owned(),
                channels: t.channels,
                rate: t.rate,
                first_tick: clock.first,
                samples: v[..(w * t.channels).min(v.len())].to_vec(),
                discontinuities: clock.discontinuities,
            })
            .collect(),
    })
}
```

The pipewire-rs 0.10 names used here (`MainLoopRc`, `ContextRc::new`, `connect_rc`, `StreamBox::new`, `add_local_listener_with_user_data`, `Buffer::requested`, `Stream::time`, `Time::{ticks, rate}`, `Loop::add_timer`, `TimerSource::update_timer`) come from the crate's `examples/audio-capture.rs`, `examples/tone.rs` and `src/stream/mod.rs`. If one differs, follow the compiler and those examples. Keep the behaviour unchanged: capture first, ticks per callback, events on spawned threads, quit when every recording has its `want` frames and playback has finished.

- [ ] **Step 2: Build and lint**

Run: `cargo build -p resonance-e2e && cargo clippy -p resonance-e2e --all-targets -- -D warnings`
Expected: clean. Behaviour is verified by Task 18's harness self-check inside the container (Task 19).

- [ ] **Step 3: Commit**

```bash
git add crates/resonance-e2e/src/linux/pw.rs
git commit -m "feat(e2e): native pipewire play and multi-record on one graph clock"
```

### Task 18: Runner + `resonance-e2e run` CLI

**Files:**
- Create: `crates/resonance-e2e/src/runner.rs`, `crates/resonance-e2e/src/main.rs`
- Modify: `crates/resonance-e2e/src/lib.rs` (`#[cfg(target_os = "linux")] pub mod runner;`), `crates/resonance-e2e/Cargo.toml` (`[[bin]]`)

**Interfaces:**
- Consumes: everything from Tasks 8–17
- Produces: `runner::{RunOpts { scenarios: Vec<Scenario>, tier: Tier, out_dir: PathBuf, daemon_bin: PathBuf, baselines_path: PathBuf, update_baseline: bool }, run(&RunOpts) -> anyhow::Result<Report>, harness_check() -> anyhow::Result<()>, synthetic_ir(path: &Path, rate: u32) -> anyhow::Result<()>}`; binary `resonance-e2e run --scenarios DIR --baselines FILE --daemon BIN --out DIR [--tier quick|full] [--filter GLOB] [--update-baseline]`

- [ ] **Step 1: Write the failing test for the pure piece (synthetic IR)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_ir_is_a_decaying_stereo_float_wav() {
        let p = std::env::temp_dir().join(format!("resonance-e2e-ir-{}.wav", std::process::id()));
        synthetic_ir(&p, 48_000).unwrap();
        let r = hound::WavReader::open(&p).unwrap();
        let spec = r.spec();
        assert_eq!((spec.channels, spec.sample_rate, spec.sample_format), (2, 48_000, hound::SampleFormat::Float));
        let s: Vec<f32> = r.into_samples::<f32>().map(Result::unwrap).collect();
        assert_eq!(s.len(), 2 * 4096);
        let head: f32 = s[..512].iter().map(|v| v.abs()).sum();
        let tail: f32 = s[s.len() - 512..].iter().map(|v| v.abs()).sum();
        assert!(head > 10.0 * tail, "decays: head {head} tail {tail}");
    }
}
```

Run: `cargo test -p resonance-e2e runner` → compile errors.

- [ ] **Step 2: Implement `runner.rs`**

```rust
//! Execute scenarios against the live daemon inside the e2e container.

use crate::checks::{
    BAND_TOLERANCE_DB, PITCH_TOLERANCE, first_signal_frame, gain_db, is_flowing, last_signal_frame,
    longest_zero_run, octave_edges, pitch_error,
};
use crate::compare::{CompareMode, compare};
use crate::latency::{arrival_lags, failure, judge, load_baselines, median, save_baselines};
use crate::linux::env::{self, DEVICE, DEVICE2, Daemon, RESONANCE_SINK};
use crate::linux::pw::{Play, PlayRec, RecordTarget, Recording, Timed, play_and_record};
use crate::ratechain::{Hop, LINUX_RESONANCE_STEPS, RateChain};
use crate::render::{BLOCK_FRAMES, load_exported_chain, render};
use crate::report::{Report, ScenarioResult, Status, settle};
use crate::scenario::{EventKind, Scenario, Tier};
use crate::stimulus::{Stimulus, chirp_train, generate};
use anyhow::{Context, Result, bail, ensure};
use resonance_ipc::transport::SyncClient;
use resonance_ipc::{Command, DaemonState, Response};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const OS: &str = "linux";
/// Latency search range; also how long recording continues after the stimulus.
const MAX_LAG_SECS: f64 = 1.0;

pub struct RunOpts {
    pub scenarios: Vec<Scenario>,
    pub tier: Tier,
    pub out_dir: PathBuf,
    pub daemon_bin: PathBuf,
    pub baselines_path: PathBuf,
    pub update_baseline: bool,
}

fn ipc(cmd: Command) -> Result<()> {
    match SyncClient::connect()?.send_recv(cmd.clone())? {
        Response::Ok => Ok(()),
        Response::Error(e) => bail!("{cmd:?}: {e}"),
        other => bail!("{cmd:?}: unexpected {other:?}"),
    }
}

fn get_state() -> Result<DaemonState> {
    Ok(SyncClient::connect()?.get_state()?)
}

/// Deterministic stereo IR (exponentially decaying noise, 4096 taps) for
/// `ir = "synthetic:room"`, written as float WAV at `rate`.
pub fn synthetic_ir(path: &Path, rate: u32) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    let mut s = 0x0123_4567_89AB_CDEFu64;
    for i in 0..4096 {
        for _ in 0..2 {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let n = (s >> 40) as f32 / (1u32 << 24) as f32 - 0.5;
            w.write_sample(n * (-(i as f32) / 600.0).exp())?;
        }
    }
    w.finalize()?;
    Ok(())
}

fn ch0(x: &[f32], channels: usize) -> Vec<f64> {
    x.iter().step_by(channels).map(|&v| f64::from(v)).collect()
}

fn write_wav(path: &Path, x: &[f32], channels: usize, rate: u32) -> Result<()> {
    let spec = hound::WavSpec {
        channels: u16::try_from(channels)?,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &v in x {
        w.write_sample(v)?;
    }
    w.finalize()?;
    Ok(())
}

/// The harness itself must be bit-exact before Resonance is judged: a null
/// sink's own loopback (no Resonance in the path) has to compare equal.
pub fn harness_check() -> Result<()> {
    let _ = env::destroy_device(DEVICE);
    env::force_graph_rate(Some(48_000))?;
    env::create_device(DEVICE, Some(48_000), 2)?;
    let stim = generate(48_000, 2, 1.0);
    let pr = play_and_record(
        &Play { node: DEVICE, rate: 48_000, channels: 2, samples: &stim.samples },
        &[RecordTarget { node: DEVICE, rate: 48_000, channels: 2 }],
        48_000,
        Vec::new(),
        Duration::from_secs(20),
    )?;
    let o = compare(&stim.samples, &pr.recordings[0].samples, 2, stim.body.clone(), 48_000);
    ensure!(o.passes(CompareMode::Exact), "harness loopback is not bit-exact: {}", o.describe());
    Ok(())
}

/// Median total latency (ms) of a played chirp train on recording `rec_idx`.
fn latency_ms(pr: &PlayRec, rec_idx: usize, rate: u32) -> Result<f64> {
    let train = chirp_train(rate, 1);
    let rec = &pr.recordings[rec_idx];
    let template: Vec<f64> = train.samples[train.emit[0]..train.emit[0] + train.chirp_frames]
        .iter()
        .map(|&v| f64::from(v))
        .collect();
    let max_lag = (0.45 * f64::from(rate)) as usize;
    let lags = arrival_lags(&ch0(&rec.samples, rec.channels), &template, &train.emit, max_lag);
    let (Some(cap0), Some(play0)) = (rec.first_tick, pr.play_first_tick) else {
        bail!("stream positions unavailable");
    };
    let offset = cap0 as f64 - play0 as f64;
    let ms = lags.iter().map(|&l| (l as f64 + offset) * 1000.0 / f64::from(pr.graph_rate.max(1)));
    median(ms.collect()).context("no chirps")
}

fn play_train(target: &str, s: &Scenario) -> Result<f64> {
    let train = chirp_train(s.rate, s.channels);
    let pr = play_and_record(
        &Play { node: target, rate: s.rate, channels: s.channels, samples: &train.samples },
        &[RecordTarget { node: DEVICE, rate: s.rate, channels: s.channels }],
        s.rate as usize / 2,
        Vec::new(),
        Duration::from_secs(20),
    )?;
    latency_ms(&pr, 0, s.rate)
}

/// Fresh devices + graph rate for one scenario (no leftovers from the last).
fn reset_devices(s: &Scenario) -> Result<()> {
    let _ = env::destroy_device(DEVICE);
    let _ = env::destroy_device(DEVICE2);
    env::force_graph_rate(Some(s.graph_rate))?;
    let follows_graph = s.events.iter().any(|e| matches!(e.kind, EventKind::ForceRate { .. }));
    env::create_device(DEVICE, (!follows_graph).then_some(s.rate), s.channels)?;
    for e in &s.events {
        if let EventKind::SwitchDevice { rate, channels } = e.kind {
            env::create_device(DEVICE2, Some(rate), channels)?;
        }
    }
    env::set_default_sink(DEVICE)
}

fn apply_profile(s: &Scenario, dir: &Path) -> Result<()> {
    let p = &s.profile;
    match &p.preset {
        Some(preset) => ipc(Command::LoadPreset { path: preset.to_string_lossy().into_owned() })?,
        None => ipc(Command::ApplyState {
            preamp_db: p.preamp_db,
            enabled: true,
            bands: p.bands.clone(),
            effects: p.effects.clone(),
        })?,
    }
    ipc(Command::SetDither { bits: p.dither_bits })?;
    ipc(Command::SetPhaseMode { linear: p.linear_phase })?;
    if let Some(ir) = &p.ir {
        let path = if ir.starts_with("synthetic:") {
            let path = dir.join("synthetic-ir.wav");
            synthetic_ir(&path, s.rate)?;
            path.to_string_lossy().into_owned()
        } else {
            ir.clone()
        };
        ipc(Command::SetConvolutionIr { path })?;
    }
    Ok(())
}

/// Wait until the daemon reports the scenario's format on the scenario's
/// device; on timeout, fail with the actual mismatch.
fn await_format(s: &Scenario) -> Result<()> {
    let check = || -> Result<()> {
        let st = get_state()?;
        env::check_on_state(st.channels, st.sample_rate, st.active_output.as_deref(), DEVICE, s.rate, s.channels)
    };
    env::wait_for("daemon on the scenario device", Duration::from_secs(10), || Ok(check().is_ok()))
        .or_else(|_| check())
}

/// Recording targets and timed actions for a scenario's events.
fn event_actions(
    s: &Scenario,
    stim: &Stimulus,
    daemon: &Arc<Mutex<Option<Daemon>>>,
    daemon_bin: &Path,
    log: &Path,
) -> (Vec<RecordTarget<'static>>, Vec<Timed>) {
    let mut records = vec![RecordTarget { node: DEVICE, rate: s.rate, channels: s.channels }];
    let mut timed = Vec::new();
    for e in &s.events {
        let at_frame = stim.body.start + (e.at_secs * f64::from(s.player_rate)) as usize;
        let action: Box<dyn FnOnce() + Send> = match e.kind {
            EventKind::ForceRate { rate } => Box::new(move || {
                let _ = env::force_graph_rate(Some(rate));
            }),
            EventKind::SwitchDevice { rate, channels } => {
                records.push(RecordTarget { node: DEVICE2, rate, channels });
                Box::new(|| {
                    let _ = ipc(Command::SetOutputTarget { node_name: DEVICE2.into() });
                })
            }
            EventKind::RestartDaemon => {
                let (d, bin, log) = (Arc::clone(daemon), daemon_bin.to_path_buf(), log.to_path_buf());
                Box::new(move || {
                    let mut g = d.lock().expect("daemon lock");
                    if let Some(old) = g.take() {
                        let _ = old.stop();
                    }
                    *g = Daemon::start(&bin, &log).ok();
                    let _ = ipc(Command::SetOutputTarget { node_name: DEVICE.into() });
                })
            }
        };
        timed.push(Timed { at_frame, action });
    }
    (records, timed)
}

/// Steady matched-rate path: every sample must equal the offline render.
fn exact_checks(
    r: &mut ScenarioResult,
    s: &Scenario,
    stim: &Stimulus,
    rec: &Recording,
    expected: &[f32],
    dir: &Path,
) -> Result<()> {
    // Body plus up to 0.25 s of processed tail (reverb, FIR, IR).
    let window = stim.body.start..stim.body.end + stim.body.len().min(s.rate as usize / 4);
    let o = compare(expected, &rec.samples, s.channels, window, (MAX_LAG_SECS * f64::from(s.rate)) as usize);
    if !o.passes(s.expect.compare) {
        r.failures.push(o.describe());
        write_wav(&dir.join("recorded.wav"), &rec.samples, s.channels, s.rate)?;
        write_wav(&dir.join("expected.wav"), expected, s.channels, s.rate)?;
        let shift = usize::try_from(o.lag).unwrap_or(0) * s.channels;
        let diff: Vec<f32> = rec.samples.iter().skip(shift).zip(expected).map(|(a, b)| a - b).collect();
        write_wav(&dir.join("diff.wav"), &diff, s.channels, s.rate)?;
    }
    r.compare = Some(o);
    Ok(())
}

/// A rate converter is in the path, so not bit-exact by design: the pilot's
/// pitch and the path's per-octave gain must match the offline render's
/// gain at the DSP rate. Windows are aligned on the first arriving sample.
fn resample_checks(
    r: &mut ScenarioResult,
    s: &Scenario,
    stim: &Stimulus,
    rec: &Recording,
    export: &Path,
    state: &DaemonState,
) -> Result<()> {
    let ch = s.channels;
    let rec_rate = f64::from(rec.rate);
    let start = first_signal_frame(&rec.samples, rec.channels).context("recording is silent")?;
    let len = (stim.body.len() as f64 * rec_rate / f64::from(s.player_rate)) as usize;
    let end = (start + len).min(rec.samples.len() / rec.channels);
    let rec0 = ch0(&rec.samples[start * rec.channels..end * rec.channels], rec.channels);
    let rec0_f32: Vec<f32> = rec0.iter().map(|&v| v as f32).collect();
    let pe = pitch_error(&rec0_f32, rec_rate);
    if pe > PITCH_TOLERANCE {
        r.failures.push(format!("pilot pitch off by {:.4} %", pe * 100.0));
    }
    let dsp_rate = state.sample_rate;
    let stim_dsp = generate(dsp_rate as u32, ch, s.body_secs);
    let (mut chain, _) = load_exported_chain(export, state.channels, dsp_rate)?;
    let rendered = render(&mut chain, &stim_dsp.samples, BLOCK_FRAMES);
    let body = |x: &[f32], st: &Stimulus| ch0(&x[st.body.start * ch..st.body.end * ch], ch);
    let edges = octave_edges(0.45 * f64::from(s.player_rate.min(s.rate)).min(dsp_rate));
    let measured = gain_db(&body(&stim.samples, stim), f64::from(s.player_rate), &rec0, rec_rate, &edges);
    let expected = gain_db(&body(&stim_dsp.samples, &stim_dsp), dsp_rate, &body(&rendered, &stim_dsp), dsp_rate, &edges);
    for ((hz, m), e) in edges.iter().zip(&measured).zip(&expected) {
        if (m - e).abs() > BAND_TOLERANCE_DB {
            r.failures.push(format!("octave from {hz:.0} Hz: path gain {m:+.2} dB, render {e:+.2} dB"));
        }
    }
    Ok(())
}

/// Mid-stream events: correct pitch after the last event, gap within the
/// limit, and audio still flowing at the end.
fn event_checks(r: &mut ScenarioResult, s: &Scenario, stim: &Stimulus, pr: &PlayRec) {
    let last = pr.recordings.last().expect("at least one recording");
    let ch = last.channels;
    let rec_rate = f64::from(last.rate);
    let to_rec = |play_frame: f64| (play_frame * rec_rate / f64::from(s.player_rate)) as usize;
    let end = to_rec(stim.body.end as f64).min(last.samples.len() / ch);
    let last_event = s.events.iter().map(|e| e.at_secs).fold(0.0, f64::max);
    let after = to_rec(stim.body.start as f64 + (last_event + 0.5) * f64::from(s.player_rate)).min(end);
    let seg: Vec<f32> = last.samples[after * ch..end * ch].iter().step_by(ch).copied().collect();
    let pe = pitch_error(&seg, rec_rate);
    if pe > PITCH_TOLERANCE {
        r.failures.push(format!("pilot pitch off by {:.4} % after the event", pe * 100.0));
    }
    let gap_ms = match pr.recordings.as_slice() {
        // Device switch: old device's last audio → new device's first, on the graph clock.
        [a, b] => match (
            a.first_tick,
            b.first_tick,
            last_signal_frame(&a.samples, a.channels),
            first_signal_frame(&b.samples, b.channels),
        ) {
            (Some(ta), Some(tb), Some(la), Some(fb)) => {
                ((tb + fb as u64) as f64 - (ta + la as u64) as f64) * 1000.0 / f64::from(pr.graph_rate.max(1))
            }
            _ => f64::INFINITY,
        },
        // Same device: longest silence after the stimulus first arrived.
        _ => {
            let first = first_signal_frame(&last.samples, ch).unwrap_or(end);
            longest_zero_run(&last.samples, ch, first..end) as f64 * 1000.0 / rec_rate
        }
    };
    if gap_ms > s.expect.max_gap_ms {
        r.failures.push(format!("audio gap {gap_ms:.0} ms > {} ms", s.expect.max_gap_ms));
    }
    if !is_flowing(&last.samples, ch, end.saturating_sub((0.2 * rec_rate) as usize)..end) {
        r.failures.push("audio not flowing at the end of the stimulus".into());
    }
}

/// Everything between "daemon started" and "daemon stopped" for one scenario.
fn measure(
    r: &mut ScenarioResult,
    s: &Scenario,
    opts: &RunOpts,
    off_ms: &BTreeMap<(u32, usize), f64>,
    baselines: &BTreeMap<String, f64>,
    daemon: &Arc<Mutex<Option<Daemon>>>,
    dir: &Path,
) -> Result<()> {
    ipc(Command::SetOutputTarget { node_name: DEVICE.into() })?;
    await_format(s)?;
    apply_profile(s, dir)?;
    env::verify_on(DEVICE, &get_state()?, s.rate, s.channels)?;

    let export = dir.join("chain.bin");
    let export_str = export.to_string_lossy().into_owned();
    if s.measures_latency() {
        ipc(Command::ResetAndExportChain { path: export_str.clone() })?;
        let on = play_train(RESONANCE_SINK, s)?;
        let off = *off_ms.get(&(s.rate, s.channels)).context("no Resonance-off latency for this format")?;
        let added = on - off;
        let v = judge(added, baselines.get(&s.id).copied());
        (r.latency_on_ms, r.latency_off_ms, r.added_latency_ms, r.latency_verdict) =
            (Some(on), Some(off), Some(added), Some(v));
        r.failures.extend(failure(v, added, opts.update_baseline));
    }

    ipc(Command::ResetAndExportChain { path: export_str })?;
    let state = get_state()?;
    let stim = generate(s.player_rate, s.channels, s.body_secs);
    let (records, timed) = event_actions(s, &stim, daemon, &opts.daemon_bin, &dir.join("daemon.log"));
    let timeout = Duration::from_secs_f64(3.0 * stim.frames() as f64 / f64::from(s.player_rate) + 15.0);
    let pr = play_and_record(
        &Play { node: RESONANCE_SINK, rate: s.player_rate, channels: s.channels, samples: &stim.samples },
        &records,
        (MAX_LAG_SECS * f64::from(s.rate)) as usize,
        timed,
        timeout,
    )?;
    r.discontinuities = pr.play_discontinuities + pr.recordings.iter().map(|x| x.discontinuities).sum::<u32>();

    let end_state = get_state()?;
    let device_node = pr.recordings.last().map_or(DEVICE, |x| x.node.as_str());
    let rates = RateChain {
        hops: vec![
            Hop { name: "player".into(), rate: s.player_rate },
            Hop { name: "graph".into(), rate: pr.graph_rate },
            Hop { name: "capture".into(), rate: end_state.capture_rate as u32 },
            Hop { name: "dsp".into(), rate: end_state.sample_rate as u32 },
            Hop { name: "device".into(), rate: env::node_rate(&env::pw_dump()?, device_node).unwrap_or(pr.graph_rate) },
        ],
    };
    r.resamplings = rates.resamplings(&s.expect.resample, LINUX_RESONANCE_STEPS);
    for x in r.resamplings.iter().filter(|x| x.reason.is_none()) {
        r.failures.push(format!("undeclared resampling at {} ({} → {} Hz)", x.step, x.from_rate, x.to_rate));
    }
    r.rate_chain = Some(rates);

    if !s.events.is_empty() {
        event_checks(r, s, &stim, &pr);
    } else if s.player_rate == s.rate && s.graph_rate == s.rate {
        let (mut chain, notes) = load_exported_chain(&export, state.channels, state.sample_rate)?;
        r.notes.extend(notes);
        let expected = render(&mut chain, &stim.samples, BLOCK_FRAMES);
        exact_checks(r, s, &stim, &pr.recordings[0], &expected, dir)?;
    } else {
        resample_checks(r, s, &stim, &pr.recordings[0], &export, &state)?;
    }
    Ok(())
}

fn run_once(
    s: &Scenario,
    opts: &RunOpts,
    off_ms: &BTreeMap<(u32, usize), f64>,
    baselines: &BTreeMap<String, f64>,
) -> Result<ScenarioResult> {
    let dir = opts.out_dir.join(&s.id);
    std::fs::create_dir_all(&dir)?;
    let mut r = ScenarioResult::new(&s.id, OS);
    r.expected_fail.clone_from(&s.expected_fail);
    reset_devices(s)?;
    let daemon = Arc::new(Mutex::new(Some(Daemon::start(&opts.daemon_bin, &dir.join("daemon.log"))?)));
    if let Err(e) = measure(&mut r, s, opts, off_ms, baselines, &daemon, &dir) {
        r.failures.push(format!("{e:#}"));
    }
    if let Some(d) = daemon.lock().expect("daemon lock").take() {
        let _ = d.stop();
    }
    if !r.failures.is_empty() {
        if let Ok(d) = env::pw_dump() {
            let _ = std::fs::write(dir.join("pw-dump.json"), serde_json::to_vec_pretty(&d).unwrap_or_default());
        }
        r.artifacts = Some(dir.to_string_lossy().into_owned());
    }
    r.status = settle(r.failures.is_empty(), s.expected_fail.is_some());
    Ok(r)
}

/// Resonance-off latency per device format, measured with Resonance verifiably absent.
fn measure_off(scenarios: &[&Scenario]) -> Result<BTreeMap<(u32, usize), f64>> {
    let mut off = BTreeMap::new();
    for s in scenarios.iter().filter(|s| s.measures_latency()) {
        if off.contains_key(&(s.rate, s.channels)) {
            continue;
        }
        reset_devices(s)?;
        env::verify_off(DEVICE)?;
        off.insert((s.rate, s.channels), play_train(DEVICE, s)?);
    }
    Ok(off)
}

pub fn run(opts: &RunOpts) -> Result<Report> {
    harness_check().context("harness self-check (null-sink loopback)")?;
    let mut baselines = load_baselines(&opts.baselines_path)?;
    let selected: Vec<&Scenario> = opts.scenarios.iter().collect();
    let off = measure_off(&selected).context("measuring Resonance-off latency")?;
    let mut results = Vec::new();
    for s in &selected {
        eprintln!("e2e: {}", s.id);
        let mut r = run_once(s, opts, &off, &baselines)?;
        if r.status == Status::Fail && r.discontinuities > 0 {
            let first = r.failures.join("; ");
            let rerun = run_once(s, opts, &off, &baselines)?;
            r = if rerun.status == Status::Pass {
                ScenarioResult { status: Status::Flake, notes: vec![format!("first attempt: {first}")], ..rerun }
            } else {
                rerun
            };
        }
        if opts.update_baseline {
            if let Some(a) = r.added_latency_ms {
                baselines.insert(r.id.clone(), (a * 100.0).round() / 100.0);
            }
        }
        results.push(r);
    }
    if opts.update_baseline {
        save_baselines(&opts.baselines_path, &baselines)?;
    }
    Ok(Report { os: OS.into(), tier: format!("{:?}", opts.tier).to_lowercase(), results })
}
```

`crates/resonance-e2e/src/main.rs`:

```rust
//! `resonance-e2e run`: execute scenarios on this machine's live audio stack.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(about = "Resonance live-audio e2e agent (runs inside the e2e container/VM)")]
enum Cli {
    /// Run scenarios and write report.json + report.md into --out.
    Run {
        #[arg(long)]
        scenarios: PathBuf,
        #[arg(long)]
        baselines: PathBuf,
        #[arg(long)]
        daemon: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, value_enum, default_value_t = resonance_e2e::scenario::Tier::Quick)]
        tier: resonance_e2e::scenario::Tier,
        #[arg(long)]
        filter: Option<String>,
        #[arg(long)]
        update_baseline: bool,
    },
}

fn main() -> Result<ExitCode> {
    let Cli::Run { scenarios, baselines, daemon, out, tier, filter, update_baseline } = Cli::parse();
    run(scenarios, baselines, daemon, out, tier, filter, update_baseline)
}

#[cfg(target_os = "linux")]
fn run(
    scenarios: PathBuf,
    baselines: PathBuf,
    daemon: PathBuf,
    out: PathBuf,
    tier: resonance_e2e::scenario::Tier,
    filter: Option<String>,
    update_baseline: bool,
) -> Result<ExitCode> {
    use resonance_e2e::{runner, scenario};
    // This agent creates devices and changes the default sink: refuse to run
    // anywhere but the e2e container/VM, whose entrypoint sets this.
    anyhow::ensure!(
        std::env::var_os("RESONANCE_E2E_SANDBOX").is_some(),
        "refusing to run outside the e2e sandbox (RESONANCE_E2E_SANDBOX unset): \
         it would reconfigure this machine's audio"
    );
    let all = scenario::load_dir(&scenarios)?;
    let selected = scenario::select(&all, tier, filter.as_deref(), "linux").into_iter().cloned().collect();
    std::fs::create_dir_all(&out)?;
    let report = runner::run(&runner::RunOpts {
        scenarios: selected,
        tier,
        out_dir: out.clone(),
        daemon_bin: daemon,
        baselines_path: baselines,
        update_baseline,
    })?;
    std::fs::write(out.join("report.json"), serde_json::to_vec_pretty(&report)?)?;
    let md = report.to_markdown();
    std::fs::write(out.join("report.md"), &md)?;
    println!("{md}");
    Ok(if report.failed() { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

#[cfg(not(target_os = "linux"))]
fn run(
    _: PathBuf,
    _: PathBuf,
    _: PathBuf,
    _: PathBuf,
    _: resonance_e2e::scenario::Tier,
    _: Option<String>,
    _: bool,
) -> Result<ExitCode> {
    anyhow::bail!("resonance-e2e: only Linux is implemented so far (Windows: M2, macOS: M3)")
}
```

`Cargo.toml` of the crate: add

```toml
[[bin]]
name = "resonance-e2e"
path = "src/main.rs"
```

and `#[cfg(target_os = "linux")] pub mod runner;` in `lib.rs`. `ScenarioResult` needs `Clone` (already derived) for the flake path. `Scenario` must derive `Clone` (already does).

- [ ] **Step 3: Run tests and lint**

Run: `cargo test -p resonance-e2e && cargo clippy -p resonance-e2e --all-targets -- -D warnings`
Expected: PASS and clean. If a clippy pedantic lint fires on the runner (e.g. `cast_*` is allowed workspace-wide, `too_many_arguments` on the non-Linux `run`), fix it at the site or add a justified `#[allow]`, as the repo's lint policy requires.

- [ ] **Step 4: Commit**

```bash
git add crates/resonance-e2e
git commit -m "feat(e2e): scenario runner with verified on/off states, latency and flake rerun"
```

### Task 19: Container image + `cargo xtask e2e`

**Files:**
- Create: `contrib/e2e/linux/Containerfile`, `contrib/e2e/linux/entrypoint.sh`, `xtask/Cargo.toml`, `xtask/src/main.rs`, `.cargo/config.toml`
- Modify: `Cargo.toml` (members += `"xtask"`), `Makefile` (`e2e` target)

**Interfaces:**
- Consumes: the `resonance-e2e run` CLI (Task 18)
- Produces: `cargo xtask e2e [--os linux] [--tier quick|full] [--scenario GLOB] [--keep] [--update-baseline]`. Reports land in `target/e2e/run-<unix secs>/linux/`. The spec's `cargo xtask e2e image <os>` (VM base images under `$RESONANCE_E2E_HOME`) arrives with the VMs in M2/M3; the Linux image is a podman build that is cached and re-run on every invocation.

- [ ] **Step 1: Container files**

`contrib/e2e/linux/Containerfile`:

```dockerfile
# PipeWire + WirePlumber with no real devices: the e2e agent creates null
# sinks per scenario. Arch base so binaries built on the (CachyOS) host run
# here unchanged.
FROM docker.io/library/archlinux:base
RUN pacman -Syu --noconfirm --needed pipewire wireplumber dbus procps-ng \
    && pacman -Scc --noconfirm
COPY entrypoint.sh /usr/local/bin/e2e-entrypoint
ENTRYPOINT ["/usr/local/bin/e2e-entrypoint"]
```

`contrib/e2e/linux/entrypoint.sh` (mode 755):

```sh
#!/bin/sh
# Start a private session bus, PipeWire and WirePlumber, then run the command.
set -eu
export XDG_RUNTIME_DIR=/run/e2e
# The agent refuses to run without this (it reconfigures audio).
export RESONANCE_E2E_SANDBOX=1
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
DBUS_SESSION_BUS_ADDRESS=$(dbus-daemon --session --fork --print-address)
export DBUS_SESSION_BUS_ADDRESS
pipewire >/tmp/pipewire.log 2>&1 &
wireplumber >/tmp/wireplumber.log 2>&1 &
i=0
until pw-cli info 0 >/dev/null 2>&1; do
    i=$((i + 1))
    [ "$i" -gt 100 ] && { cat /tmp/pipewire.log /tmp/wireplumber.log; exit 1; }
    sleep 0.1
done
exec "$@"
```

- [ ] **Step 2: Smoke-test the image by hand**

Run: `podman build -t resonance-e2e-linux contrib/e2e/linux && podman run --rm resonance-e2e-linux pw-cli info 0 | head -3`
Expected: `id: 0` and the PipeWire core info.

- [ ] **Step 3: xtask crate**

`xtask/Cargo.toml`:

```toml
[package]
name = "xtask"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true
publish = false

[dependencies]
anyhow = { workspace = true }
clap = { workspace = true }

[lints]
workspace = true
```

`.cargo/config.toml`:

```toml
[alias]
xtask = "run --package xtask --"
```

`xtask/src/main.rs`:

```rust
//! Developer tasks for this host. `cargo xtask e2e` runs the live-audio suite
//! (spec: docs/superpowers/specs/2026-10-06-e2e-audio-test-infra-design.md).

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, ValueEnum};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

#[derive(Parser)]
enum Cli {
    /// Build, then run the live-audio scenarios in their environments.
    E2e {
        #[arg(long, value_delimiter = ',', default_value = "linux")]
        os: Vec<Os>,
        #[arg(long, default_value = "quick")]
        tier: String,
        /// Only scenarios whose id matches this glob (`*` wildcard).
        #[arg(long)]
        scenario: Option<String>,
        /// Leave the container running after a failure for inspection.
        #[arg(long)]
        keep: bool,
        /// Rewrite contrib/e2e/baselines/<os>.toml from this run.
        #[arg(long)]
        update_baseline: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Os {
    Linux,
    Windows,
    Macos,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask lives in the repo root").to_path_buf()
}

fn status(cmd: &mut Command) -> Result<bool> {
    Ok(cmd.status().with_context(|| format!("spawn {cmd:?}"))?.success())
}

fn main() -> Result<ExitCode> {
    let Cli::E2e { os, tier, scenario, keep, update_baseline } = Cli::parse();
    if os.iter().any(|o| *o != Os::Linux) {
        bail!("only --os linux exists so far (Windows: M2, macOS: M3)");
    }
    let root = repo_root();
    ensure!(
        status(Command::new("cargo").current_dir(&root).args([
            "build", "--profile", "e2e-build", "-p", "resonance-daemon", "-p", "resonance-e2e",
        ]))?,
        "build failed"
    );
    ensure!(
        status(Command::new("podman").args(["build", "-q", "-t", "resonance-e2e-linux"]).arg(root.join("contrib/e2e/linux")))?,
        "container image build failed"
    );
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let out = root.join(format!("target/e2e/run-{stamp}/linux"));
    std::fs::create_dir_all(&out)?;
    let name = format!("resonance-e2e-{stamp}");
    let contrib_mode = if update_baseline { "rw" } else { "ro" };
    let mut agent = vec![
        "/e2e/bin/resonance-e2e".to_string(), "run".into(),
        "--scenarios".into(), "/e2e/contrib/scenarios".into(),
        "--baselines".into(), "/e2e/contrib/baselines/linux.toml".into(),
        "--daemon".into(), "/e2e/bin/resonanced".into(),
        "--out".into(), "/e2e/out".into(),
        "--tier".into(), tier,
    ];
    if let Some(g) = scenario {
        agent.extend(["--filter".into(), g]);
    }
    if update_baseline {
        agent.push("--update-baseline".into());
    }
    // With --keep the container stays up after the agent exits (code in a file).
    let script = format!("{}; echo $? > /e2e/out/exit-code; [ \"$(cat /e2e/out/exit-code)\" = 0 ] || [ \"{keep}\" = false ] || exec sleep infinity", agent.join(" "));
    let mut run = Command::new("podman");
    run.args(["run", "--name", &name, "--security-opt", "label=disable"])
        .arg("-v").arg(format!("{}:/e2e/bin:ro", root.join("target/e2e-build").display()))
        .arg("-v").arg(format!("{}:/e2e/contrib:{contrib_mode}", root.join("contrib/e2e").display()))
        .arg("-v").arg(format!("{}:/e2e/out", out.display()));
    if keep {
        run.arg("-d");
    } else {
        run.arg("--rm");
    }
    run.args(["resonance-e2e-linux", "sh", "-c", &script]);
    let ok = status(&mut run)?;
    if keep {
        // Detached: wait for the agent's exit code to appear.
        while !out.join("exit-code").exists() {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
    let code = std::fs::read_to_string(out.join("exit-code")).unwrap_or_default();
    let passed = (keep || ok) && code.trim() == "0";
    println!("\nreport: {}", out.join("report.md").display());
    if keep {
        if passed {
            let _ = status(Command::new("podman").args(["rm", "-f", &name]));
        } else {
            println!("container kept: podman exec -it {name} bash   (remove: podman rm -f {name})");
        }
    }
    Ok(if passed { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}
```

Root `Cargo.toml` members: add `"xtask"`. `Makefile`: add `e2e` to `.PHONY` and

```make
e2e:
	cargo xtask e2e
```

- [ ] **Step 4: First container run (harness self-check + empty scenario set)**

Run: `mkdir -p contrib/e2e/scenarios contrib/e2e/baselines && cargo xtask e2e`
Expected: build succeeds; the container starts. The harness self-check (Task 18) passes (`harness loopback is not bit-exact` would mean the harness itself is broken: fix `linux/pw.rs` before continuing). The report shows zero scenarios; exit 0.

- [ ] **Step 5: `make check`, then commit**

```bash
git add contrib/e2e/linux xtask .cargo/config.toml Cargo.toml Cargo.lock Makefile
git commit -m "feat(e2e): linux container image and cargo xtask e2e"
```

### Task 20: Scenario set, fixtures, baselines, docs, and the first full run

**Files:**
- Create: `contrib/e2e/scenarios/{formats,eq,effects,phase-and-convolution,resample,events,long-run}.toml`, `contrib/e2e/profiles/full-eq.toml`, `contrib/e2e/fixtures/all-types.txt`, `contrib/e2e/baselines/linux.toml` (generated), `contrib/e2e/README.md`
- Modify: `docs/superpowers/plans/2026-09-19-audio-robustness-findings.md` (new `PW-*`/`CORE-*` findings the run exposes)

- [ ] **Step 1: Write the scenario files**

`contrib/e2e/profiles/full-eq.toml`:

```toml
# Every UI band type, all slopes, mid/side scopes, a per-channel band and a
# dynamic band. 8 bands (snapshots hold at most 32).
preamp_db = -6.0

[[bands]]
band_type = "Peaking"
freq = 1000.0
gain_db = 6.0
q = 1.41
enabled = true

[[bands]]
band_type = "LowShelf"
freq = 105.0
gain_db = 4.0
q = 0.7
enabled = true
slope_db_oct = 24

[[bands]]
band_type = "HighShelf"
freq = 8000.0
gain_db = -3.0
q = 0.7
enabled = true
slope_db_oct = 48

[[bands]]
band_type = "HighPass"
freq = 25.0
gain_db = 0.0
q = 0.707
enabled = true
slope_db_oct = 24

[[bands]]
band_type = "LowPass"
freq = 18000.0
gain_db = 0.0
q = 0.707
enabled = true

[[bands]]
band_type = "Notch"
freq = 3150.0
gain_db = 0.0
q = 8.0
enabled = true
scope = "Side"

[[bands]]
band_type = "BandPass"
freq = 250.0
gain_db = 0.0
q = 0.5
enabled = true
scope = "Mid"
channels = 1

[[bands]]
band_type = "Peaking"
freq = 6500.0
gain_db = 4.0
q = 2.0
enabled = true
dynamics = { threshold_db = -30.0, range_db = -6.0, attack_ms = 5.0, release_ms = 150.0 }
```

`contrib/e2e/fixtures/all-types.txt` (all 14 EqualizerAPO keywords → all 14 DSP filter types):

```
Preamp: -9 dB
Filter 1: ON PK Fc 1000 Hz Gain 4 dB Q 1.41
Filter 2: ON LS Fc 100 Hz Gain 3 dB
Filter 3: ON LS 12dB Fc 80 Hz Gain 2 dB
Filter 4: ON LSC Fc 120 Hz Gain 2 dB Q 0.7
Filter 5: ON HS Fc 10000 Hz Gain -2 dB
Filter 6: ON HS 12dB Fc 12000 Hz Gain -2 dB
Filter 7: ON HSC Fc 9000 Hz Gain -1 dB Q 0.7
Filter 8: ON LP Fc 19000 Hz
Filter 9: ON LPQ Fc 18500 Hz Q 0.707
Filter 10: ON HP Fc 20 Hz
Filter 11: ON HPQ Fc 22 Hz Q 0.707
Filter 12: ON BP Fc 2000 Hz Q 0.3
Filter 13: ON NO Fc 6000 Hz Q 10
Filter 14: ON AP Fc 500 Hz Q 0.7
```

`contrib/e2e/scenarios/formats.toml`:

```toml
# Flat chain across every rate × width: bit-exact, no resampling anywhere.
[[scenario]]
id = "flat"
tier = "full"
rates = [44100, 48000, 88200, 96000, 176400, 192000]
channels = [1, 2, 6, 8, 16]
quick = [[48000, 2], [96000, 8]]
```

`contrib/e2e/scenarios/eq.toml`:

```toml
[[scenario]]
id = "full-eq"
tier = "full"
rates = [48000, 96000]
channels = [2, 8]
quick = [[48000, 2]]
profile_file = "../profiles/full-eq.toml"

[[scenario]]
id = "apo-all-types"
tier = "full"
rates = [48000]
channels = [2]
[scenario.profile]
preset = "../fixtures/all-types.txt"
```

`contrib/e2e/scenarios/effects.toml`:

```toml
[[scenario]]
id = "fx-fidelity"
tier = "full"
rates = [48000]
channels = [2]
[scenario.profile]
effects = { fidelity = 0.7 }

[[scenario]]
id = "fx-ambience"
tier = "full"
rates = [48000]
channels = [2]
[scenario.profile]
effects = { ambience = 0.7 }

[[scenario]]
id = "fx-surround"
tier = "full"
rates = [48000]
channels = [2]
[scenario.profile]
effects = { surround = 0.7 }

[[scenario]]
id = "fx-dynamic-boost"
tier = "full"
rates = [48000]
channels = [2]
[scenario.profile]
effects = { dynamic_boost = 0.7 }

[[scenario]]
id = "fx-bass"
tier = "full"
rates = [48000]
channels = [2]
[scenario.profile]
effects = { bass = 0.7 }

[[scenario]]
id = "fx-crossfeed"
tier = "full"
rates = [48000]
channels = [2]
[scenario.profile]
effects = { crossfeed = 0.7 }

[[scenario]]
id = "fx-all-dithered"
tier = "quick"
rates = [48000]
channels = [2]
[scenario.profile]
dither_bits = 16
effects = { fidelity = 0.5, ambience = 0.5, surround = 0.5, dynamic_boost = 0.5, bass = 0.5, crossfeed = 0.5 }
```

`contrib/e2e/profiles/full-eq-linear.toml`: the same eight bands as `full-eq.toml` with linear phase on. Linear phase renders only the static stereo-scope bands to the FIR; the M/S and dynamic bands stay IIR (the hybrid path), and that is exactly what should be tested. Create it from the shared profile so the bands cannot drift by hand:

```bash
{ echo "linear_phase = true"; cat contrib/e2e/profiles/full-eq.toml; } > contrib/e2e/profiles/full-eq-linear.toml
```

`contrib/e2e/scenarios/phase-and-convolution.toml`. If Spike 4 (Task 1) found a stage block-size dependent, add `[scenario.expect]` with `compare = { tolerance_dbfs = -120 }` to that stage's scenario:

```toml
[[scenario]]
id = "linear-phase"
tier = "full"
rates = [48000, 96000]
channels = [2]
quick = [[48000, 2]]
profile_file = "../profiles/full-eq-linear.toml"

[[scenario]]
id = "convolution"
tier = "full"
rates = [48000, 96000]
channels = [2]
[scenario.profile]
ir = "synthetic:room"
```

`contrib/e2e/scenarios/resample.toml`:

```toml
[[scenario]]
id = "headset-16k-mono"
tier = "full"
rates = [16000]
channels = [1]
player_rate = 48000
[scenario.expect]
resample = [{ hop = "player->graph", reason = "48 kHz content into a 16 kHz mono headset-profile device; PipeWire's stream adapter converts" }]

[[scenario]]
id = "content-48k-device-44k1"
tier = "quick"
rates = [44100]
channels = [2]
player_rate = 48000
[scenario.expect]
resample = [{ hop = "player->graph", reason = "48 kHz content into a 44.1 kHz device; PipeWire's stream adapter converts" }]
```

`contrib/e2e/scenarios/events.toml`:

```toml
[[scenario]]
id = "rate-change-48k-96k"
tier = "quick"
rates = [48000]
channels = [2]
body_secs = 4.0
[scenario.expect]
resample = [{ hop = "player->graph", reason = "graph moved to 96 kHz mid-stream; the 48 kHz player stream is converted" }]
[[scenario.events]]
at_secs = 1.5
kind = "force_rate"
rate = 96000

[[scenario]]
id = "switch-stereo-to-8ch"
tier = "full"
rates = [48000]
channels = [2]
body_secs = 4.0
[[scenario.events]]
at_secs = 1.5
kind = "switch_device"
rate = 48000
channels = 8

[[scenario]]
id = "daemon-restart"
tier = "full"
rates = [48000]
channels = [2]
body_secs = 5.0
[scenario.expect]
max_gap_ms = 3000
[[scenario.events]]
at_secs = 1.5
kind = "restart_daemon"
```

`contrib/e2e/scenarios/long-run.toml`:

```toml
[[scenario]]
id = "long-flat"
tier = "full"
rates = [48000]
channels = [2]
body_secs = 60.0

[[scenario]]
id = "long-full-eq"
tier = "full"
rates = [96000]
channels = [8]
body_secs = 60.0
profile_file = "../profiles/full-eq.toml"
```

`profile_file` and `preset` resolve against the scenarios directory (`contrib/e2e/scenarios/`), hence `../profiles/…` and `../fixtures/…`. Confirm with `cargo test -p resonance-e2e scenario` that relative paths with `..` load (Task 11 joins them onto the directory).

- [ ] **Step 2: Keep the repo's scenario files parsing in CI**

Never run the agent on the host to "check" files: it reconfigures audio, which is why it refuses to run outside the sandbox. Add a unit test to `crates/resonance-e2e/src/scenario.rs`'s test module instead:

```rust
    #[test]
    fn repo_scenario_files_parse_and_quick_tier_is_not_empty() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contrib/e2e/scenarios");
        let all = load_dir(&dir).unwrap_or_else(|e| panic!("{e:#}"));
        assert!(!select(&all, Tier::Quick, None, "linux").is_empty());
        assert!(all.iter().all(|s| s.profile.preset.as_ref().is_none_or(|p| p.exists())));
    }
```

Run: `cargo test -p resonance-e2e repo_scenario_files`
Expected: PASS. A parse error names the file and key; fix the file, never the schema.

- [ ] **Step 3: First full run with baseline creation**

Run: `cargo xtask e2e --tier full --update-baseline`
Expected: the run completes; `contrib/e2e/baselines/linux.toml` is written; `target/e2e/run-*/linux/report.md` lists every scenario.

- [ ] **Step 4: Triage every failure** (systematic-debugging; never weaken a check to make it pass)

For each FAIL, open its artifacts (`recorded.wav`, `expected.wav`, `diff.wav`, `daemon.log`, `pw-dump.json`) and decide:
- **Harness bug** (the agent is wrong, e.g. wrong hop attribution or window): fix it in the agent with a unit test reproducing it, then commit `fix(e2e): …`.
- **Product bug** (Resonance is wrong, e.g. the live chain differs from the snapshot rebuild, undeclared resampling, a gap too long): append a finding to `docs/superpowers/plans/2026-09-19-audio-robustness-findings.md` section 6 (PipeWire, `PW-n`) or 7 (`CORE-n`/`DSP-n`) with the evidence (scenario id, first differing frame, artifacts). Then set `expected_fail = "PW-n"` on that scenario. Fixing it belongs to sub-project 3.

Re-run `cargo xtask e2e --tier full` until the report has no FAIL and no UNEXPECTED PASS.

- [ ] **Step 5: Docs**

`contrib/e2e/README.md`:

```markdown
# Live-audio e2e tests

Plays deterministic signals through the real OS audio stack with Resonance in
the path, records what reaches the device, and checks it against an offline
render of the daemon's own chain. Design:
`docs/superpowers/specs/2026-10-06-e2e-audio-test-infra-design.md`.

## Run

    cargo xtask e2e                      # quick tier, Linux container (~2 min)
    cargo xtask e2e --tier full          # everything
    cargo xtask e2e --scenario 'full-eq*'
    cargo xtask e2e --keep               # keep the container after a failure

Prerequisites: rootless podman. The container has its own PipeWire with no
real devices and never touches your desktop audio.

Reports: `target/e2e/run-<time>/linux/report.md` (+ `report.json`, and per
failing scenario: recorded/expected/diff WAVs, daemon log, `pw-dump.json`).

## Scenarios

`scenarios/*.toml`, one or more `[[scenario]]` tables each. Lists of `rates`
and `channels` expand into one scenario per combination (`id@rate x ch`);
`quick = [[rate, ch], …]` picks which also run in the quick tier. Unknown keys
are errors. Profiles: inline `[scenario.profile]` or `profile_file`; effects as
`{ name = intensity }`; `preset` loads an EqualizerAPO file; `ir` is a WAV path
or `synthetic:room`.

`expect.compare` is `"exact"` (default) or `{ tolerance_dbfs = -120 }`.
`expect.resample` lists the steps allowed to resample, each with its reason;
any other resampling fails. Known product bugs: `expected_fail = "<finding id>"`
from `docs/superpowers/plans/2026-09-19-audio-robustness-findings.md`.

## Latency baselines

`baselines/linux.toml` holds each scenario's added latency (ms). A run fails
above baseline + max(1 ms, 10 %). After an intended change:
`cargo xtask e2e --tier full --update-baseline`, then commit the diff.
```

- [ ] **Step 6: `make check`, then commit**

```bash
git add contrib/e2e crates/resonance-e2e/src/scenario.rs docs/superpowers/plans/2026-09-19-audio-robustness-findings.md
git commit -m "test(e2e): linux scenario set, latency baselines and docs"
```

- [ ] **Step 7: Update the spec's build-order status and open the PR**

Append to the spec's section 12: `M1 done (YYYY-MM-DD): cargo xtask e2e --os linux --tier full green; N expected-fail findings logged.` Commit as `docs(superpowers): e2e m1 status`. Push the branch and open a PR. The body summarizes the report grid and lists the new findings, with no AI attribution.
