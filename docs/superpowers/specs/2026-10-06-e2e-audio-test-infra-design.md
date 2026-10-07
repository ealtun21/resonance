# Single-host live-audio test infrastructure — design

- **Date:** 2026-10-06
- **Status:** design approved section by section in conversation; awaiting written-spec review.
- **Sub-project 2 of 4** in the current work order: (1) CI green + toolchain pin — done, PR #66;
  (2) **this**; (3) robustness fixes from `docs/superpowers/plans/2026-09-19-audio-robustness-findings.md`,
  each verified with this infrastructure; (4) usability fixes, then a Material 3 restyle of the GUI.

## 1. Problem

Development is slowed by the build → deploy → test-on-physical-hardware loop. Today:

- Linux has a hand-built `restest` rig (separate PipeWire user, null sinks). On Linux and macOS,
  `resonance verify` records the daemon's output **inside the daemon** (`CaptureOutput` IPC), so it
  cannot see anything the OS does after Resonance (resampling into the device, volume, channel
  mixing). Only its Windows path records from the OS side (WASAPI loopback).
- The Windows test VM's disk (`~/vms/dockur-win10`) has been deleted; it was stereo-only and hand-built.
- The MacBook used for macOS testing has been wiped. There is no macOS test path at all.
- Nothing measures latency, nothing proves output is bit-identical to what the DSP should produce, and
  nothing records when or why audio gets resampled.

## 2. Goals

1. Test **real audio input and output** through each OS's real audio stack: PipeWire; CoreAudio with
   the Process Tap; the Windows audio engine with our APO inside `audiodg.exe`.
2. Prove **bit-perfect** output: the recorded OS output equals an offline render of the same chain,
   sample for sample, both flat and with EQ/effects on.
3. **Benchmark latency**: total, and the latency Resonance adds, with a regression gate.
4. Report **when and why resampling happens**, at every step of the path, and fail on any resampling
   that a scenario does not explicitly allow.
5. Cover **multi-channel** (up to 16 channels where the platform allows) and odd rates (16 kHz–192 kHz).
6. Run **everything on this one Linux host** (Ryzen 9 9950X3D, 32 threads, 60 GB RAM, KVM), with no
   other devices. One command runs it.
7. Keep GitHub Actions light: small checks and releases only.

## 3. Non-goals

- The physical DAC and the Bluetooth stack. No VM can test these. Bluetooth *formats* (16 kHz mono
  headset profile, 44.1 kHz A2DP) and mid-stream device switches are emulated instead.
- GUI/TUI testing (sub-project 4 has `contrib/dev/uishot.sh`).
- DSP CPU benchmarks (criterion benches already exist).
- Replacing `resonance verify`. It stays as the user-facing check on a real machine.

## 4. Decisions

| Topic | Decision |
|---|---|
| macOS on this host | Intel macOS VM via QEMU/KVM on this host for the local loop. Apple's licence only permits macOS VMs on Apple hardware; the user accepted this. Apple Silicon is covered by one GitHub-hosted `macos-15` job at release time. |
| Meaning of "bit-perfect" | Recorded output, after integer-sample alignment, equals the offline render of the same chain from the same input: every f32 sample equal (`==`), all channels, flat and with EQ/effects on. |
| Where the analysis runs | Inside each target, by the same binary that drives the live test (approach A). Filter coefficients use the platform math library (`sin`, `cos`, `pow`), which can differ in the last bit between glibc, macOS and Windows, so the offline render must run on the same OS and binary as the live path. |
| When it runs | One local command on demand; plus a self-hosted runner on this host for PRs from branches in this repo or PRs labelled `run-e2e` (never fork PRs automatically), every push to master, and nightly. |
| Latency | Reported per scenario plus a regression gate against committed per-OS baselines. |
| Windows "Resonance off" | APO **uninstalled**, not bypassed, and its absence verified before measuring. The same "verify, don't trust" rule applies to "off" and "on" on every OS. |
| GitHub usage | Existing `ci.yml` unchanged; `windows-installer` moves to tags + manual only; `release.yml` gains the arm64 macOS job and a gate on the local e2e status. No live-audio jobs on PRs. |

## 5. Architecture

```
this host
├── cargo xtask e2e ───────────────┬──────────────────┬───────────────────┐
│   (orchestrator, Linux only)     │                  │                   │
│                                  ▼                  ▼                   ▼
│                         podman container     Windows 11 VM        macOS 15 VM
│                         PipeWire + null      (quickemu/KVM)       (quickemu/KVM)
│                         sinks                audiodg + APO        CoreAudio + tap
│                         resonanced           resonanced           resonanced
│                         resonance-e2e        resonance-e2e        resonance-e2e
│                                  │                  │                   │
│                                  └──── report.json per OS (pulled back) ┘
└── target/e2e/<timestamp>/report.{json,md}, failure artifacts
```

The orchestrator never analyses audio. Each target runs the `resonance-e2e` agent, which plays,
records, renders offline, compares and writes JSON. The orchestrator boots environments, syncs
source, builds, runs the agent, collects the JSON and merges one report.

## 6. Environments

### 6.1 Host command

- `cargo xtask e2e` (alias in `.cargo/config.toml`; `make e2e` calls it). A small `xtask` crate,
  Linux host only.
  - `--os linux,windows,macos` (default: all three, in parallel)
  - `--tier quick|full` (default `quick`)
  - `--scenario <glob>`
  - `--keep`: leave the VM/container running after a failure for `ssh` inspection
  - `--update-baseline`: rewrite latency baselines (section 9.3)
- `cargo xtask e2e image <linux|windows|macos>` builds that OS's base image from `contrib/e2e/<os>/`.
- Data lives under `$RESONANCE_E2E_HOME` (default `~/.cache/resonance-e2e/`): base images, the ISO
  cache, per-OS persistent build-cache disks, per-run overlays.
- Every run boots a **throwaway qcow2 overlay** on top of the read-only base image; the overlay is
  deleted afterwards unless `--keep`. A second, persistent disk per OS holds the cargo target dir so
  in-guest builds are incremental.
- Source reaches a VM as a tar stream over ssh: tracked files plus uncommitted modifications
  (Windows has `tar.exe` built in). Windows and macOS build in-guest; Linux binaries are built on the
  host.

### 6.2 Linux

- Rootless podman container from `contrib/e2e/linux/Containerfile`, **Arch base**, so binaries built
  on this host (CachyOS) run inside it without a rebuild.
- Own PipeWire + WirePlumber, **no real devices**. It never touches the user's desktop session or
  running daemon.
- Each scenario creates its virtual device: a null sink with the requested channels, rate and F32
  format, plus `clock.force-rate` when the scenario pins the graph rate.

### 6.3 Windows

- Windows 11 VM via **quickemu** (installed; `swtpm` present for the TPM requirement), fully
  unattended install. The downloaded ISO is cached.
- `contrib/e2e/windows/provision.ps1` installs: OpenSSH server with the host key authorised; VS
  BuildTools (VCTools + Windows SDK); rustup with the toolchain from `rust-toolchain.toml`; the
  multichannel virtual sound device chosen by spike 2 (test-signing on if that device needs it);
  `DisableProtectedAudioDG = 1`; auto-login for a test user; a scheduled task that runs the agent
  **in the interactive session**. Audio only flows there, not in an ssh session.
- The APO is installed and uninstalled with the repo's own `contrib/windows/install-apo.ps1` /
  `uninstall-apo.ps1`, so every run also exercises them.
- Replaces the dockur VM and its port-forwarding workaround.

### 6.4 macOS

- Intel macOS 15 (Sequoia) VM via quickemu/OpenCore. The Process Tap API needs macOS 14.2 or later.
- **One manual step:** Apple's Setup Assistant must be clicked through once when creating the base
  image. `contrib/e2e/macos/README.md` holds the checklist.
- `contrib/e2e/macos/provision.sh` (over ssh) installs: Xcode command-line tools (headless
  `softwareupdate`), rustup with the pinned toolchain, BlackHole 2ch and 16ch (`installer -pkg`, HAL
  plugins, no kext), the local signing cert (`contrib/macos/make-signing-cert.sh`) so the TCC
  identity is stable, SIP disabled via the OpenCore config, a pre-granted audio-capture permission,
  auto-login, and a LaunchAgent that runs the agent **in the logged-in session**. Launched via
  `launchctl kickstart`, the agent and daemon are attributed to launchd, not sshd; under sshd, TCC
  silently delivers zeros.

### 6.5 GitHub-hosted Apple Silicon (release only)

- One `macos-15` job in `release.yml`: installs BlackHole 2ch via brew, grants audio capture (spike 3),
  runs the `quick` tier.

### 6.6 Resources

Both VMs at 8 vCPU / 8 GB plus the container: about 16 of 32 threads and 17 GB RAM. VM disks about
40 GB each, allocated as used.

## 7. The test agent (`resonance-e2e`)

### 7.1 Crate

- New crate `crates/resonance-e2e`, binary `resonance-e2e`, `publish = false`, **never shipped**.
- It is not part of `resonance verify` because sample-accurate latency needs playback and capture in
  one process on the audio system's own clock (on Linux: native PipeWire streams), and the shipped CLI
  is a static musl build that cannot link libpipewire.
- Signal-analysis helpers used by both (cross-correlation alignment, FFT peak) move from
  `crates/resonance-cli/src/verify.rs` into a `resonance-dsp::analysis` module. `resonance-dsp`
  already depends on `rustfft`.

### 7.2 Playing and recording

| OS | Plays into (same process) | Records from |
|---|---|---|
| Linux | Default sink = Resonance, native PipeWire stream | The null sink's monitor, native PipeWire stream on the same graph clock (exact frame positions) |
| macOS | Default output (BlackHole), captured by our Process Tap | BlackHole's input after the daemon writes to it (CoreAudio, host-time stamps) |
| Windows | Default endpoint, through the APO in audiodg | WASAPI loopback of that endpoint (QPC-stamped packets) |

All streams are 32-bit float at the device rate, so playback itself cannot change the samples.

### 7.3 One scenario, step by step

1. Create/select the virtual device for the scenario's format (or verify the VM's device format).
2. Load the scenario's profile into the daemon, then **reset the chain** so the live filters start
   from exactly zero state, like a fresh offline chain.
3. Verify the **on** state (section 9.4).
4. Play the stimulus: 0.5 s digital silence, then about 3 s of seeded white noise plus a log sweep at
   −12 dBFS, then 0.5 s silence. Each channel uses **a different seed**, so a swapped or misrouted
   channel cannot pass.
5. Record the device output for the whole stimulus plus a tail long enough for the scenario's
   expected latency.
6. Render the same stimulus offline through the chain at the DSP rate the daemon reports. The chain
   comes from the daemon's own description of its live chain: a new IPC request returns its
   `ChainSnapshot`, which is rebuilt with `ChainSnapshot::build_chain`
   (`crates/resonance-apo/src/state.rs:281-354`), the builder the Windows APO already uses. The
   daemon already depends on `resonance-apo` on every platform. IRs and linear-phase kernels are
   prepared through the same functions the daemon uses. The snapshot holds at most `MAX_FILTERS`
   (32) bands (`state.rs:29`), so scenarios stay within 32 bands; the snapshot request fails loudly
   rather than truncating if a chain exceeds it.
7. Align and compare (section 9.1), measure latency (section 9.3), build the rate chain (section 9.2).

### 7.4 Output

One JSON object per scenario: id, OS, format, pass/fail/expected-fail/flake, comparison result
(largest error, first differing sample and channel), latency numbers, the rate chain, OS discontinuity
counts, and paths to failure artifacts.

## 8. Scenarios

### 8.1 Format

TOML files in `contrib/e2e/scenarios/`. Each declares:

- `device`: rate(s), channel count(s). A list expands into one scenario per combination.
- `profile`: an inline or referenced profile (EQ bands, effects, preamp, phase mode, IR).
- `events` (optional): mid-stream actions with timestamps.
- `expect`:
  - `compare = "exact"` (default) or `compare = { tolerance_dbfs = -120 }`
  - `resample = []` (default: none allowed) or a list of `{ hop, reason }` entries
  - `max_gap_ms` for event scenarios
- `platforms` (optional): restrict to some OSes, with a reason.
- `expected_fail` (optional): a finding ID from the robustness audit, e.g. `"WIN-4"`. Known-broken
  behaviour stays tracked without turning the suite red. A run that unexpectedly **passes** an
  `expected_fail` scenario is reported so the flag gets removed.
- `tier`: `quick` or `full`.

### 8.2 Initial set

1. **Format sweep, flat chain:** 44.1 / 48 / 88.2 / 96 / 176.4 / 192 kHz × 1 / 2 / 6 / 8 / 16 channels,
   capped per platform by what its virtual device supports. Exact, no resampling anywhere.
2. **Full EQ:** every filter type, slopes 12/24/48, mid/side scopes, per-channel EQ, preamp, a
   dynamic-EQ band; 2 and 8 channels at 48 and 96 kHz. Exact.
3. **Effects:** each effect alone, then all together, then dither on (dither is deterministic: a fixed
   per-channel xorshift seed, `crates/resonance-dsp/src/dither.rs:19`). Exact.
4. **Linear-phase and convolution:** exact, or `tolerance_dbfs = -120` if spike 4 shows that block
   sizes change the output.
5. **Unavoidable rate mismatches:** 16 kHz mono headset format, a 44.1 kHz device fed 48 kHz content,
   and similar. Each declares the hop that may resample and why. No sample comparison; it checks
   pitch (tone peak within ±0.01 %, interpolated FFT), frequency response (within ±0.1 dB of the
   offline-predicted response, up to 0.9 × the lowest Nyquist on the path), and that no other hop
   resamples.
6. **Events:** device rate change 48 → 96 kHz; device switch stereo → 8 channels; daemon restart;
   Windows: daemon killed → APO must fall back to bypass (`expected_fail = "WIN-10"` until PR #65
   merges); macOS: tap recreation. Checks: gap
   ≤ `max_gap_ms` (default 500), pitch and frequency response correct after the gap, no undeclared
   resampling, and audio still flowing at the end (catches the audit's "silent forever" class:
   MAC-19, MAC-23, WIN-10).
7. **Long run:** 60 s continuous, flat and full EQ, exact. Catches drift, dropouts, starvation.

### 8.3 Tiers

- `quick`: about 2 minutes per OS (a few formats, one full-EQ case, one event). Runs on gated PRs,
  master pushes and at release.
- `full`: everything, about 15–20 minutes per OS. Runs nightly and on demand.

## 9. Checks and report

### 9.1 Bit-exact comparison

- Find the integer lag between render and recording by cross-correlation on channel 0, then apply
  that lag to all channels. A different best lag on any channel is a failure (routing or channel
  slip).
- Over the aligned window (stimulus start to stimulus end plus the chain's tail), every f32 sample
  must be equal (`==`). A fractional lag never compares equal; that is intended, because it means
  resampling.
- `tolerance_dbfs` scenarios: the maximum absolute error, relative to full scale, must be at or below
  the stated level.

### 9.2 Rate chain ("when and why we resample")

Per scenario, the agent records the rate at each hop and marks every hop where it changes:

| OS | Hops |
|---|---|
| Linux | player stream → graph `clock.rate` → Resonance capture rate (`capture_rate`) → DSP rate (`sample_rate`) → device (null sink) |
| macOS | player stream → system mix / device nominal rate → tap format → DSP rate → output device |
| Windows | player stream → engine mix format → APO lock rate → device |

Each changing hop is attributed to the OS or to Resonance, with the reason (from the scenario's
declaration, or "undeclared" → failure). `report.md` ends with a generated **Resampling** section that
lists every resampling hop across all OSes and scenarios: the always-current answer to "when and why".

### 9.3 Latency

- A short chirp is played 5 times in each state and the median is used.
- **Total latency** = capture timestamp of the aligned chirp start − playback timestamp of the chirp
  start, on the shared clock of 7.2.
- **Added latency** = total with Resonance on − total with Resonance off, for the same device format.
- Baselines in `contrib/e2e/baselines/<os>.toml`, keyed by scenario ID. Fail if added latency exceeds
  baseline + max(1 ms, 10 % of baseline). An improvement past the same margin is reported as
  "baseline can be lowered" (not a failure).
- `--update-baseline` rewrites the files; the diff is committed so latency changes show in PR review.
  Linear-phase's expected ~171 ms is simply that scenario's baseline.
- To limit restarts, each OS run measures all the "off" baselines it needs in one pass before
  switching to "on".

### 9.4 Verified on/off states

Before any measurement, the agent proves the state. A failed check aborts the run with an error; a
measurement is never recorded against the wrong state.

| OS | Off (verified) | On (verified) |
|---|---|---|
| Linux | No daemon process; no "Resonance EQ" node in `pw-dump`; default sink is the null sink | Daemon running; "Resonance EQ" node present and default; daemon status reports the scenario's format |
| macOS | No daemon process; no Resonance aggregate device or process tap in the CoreAudio device list; default output is BlackHole | Daemon running; tap + aggregate present; status reports the scenario's format |
| Windows | `uninstall-apo.ps1` run, audio service restarted; no Resonance CLSID in the endpoint's FX registry entries; `resonance_apo.dll` **not** among `audiodg.exe`'s loaded modules | `install-apo.ps1` run, audio service restarted; DLL loaded in `audiodg.exe`; `apo.log` shows it processing |

Windows install/uninstall plus an audio-service restart takes about 10–20 s, so a Windows run does all
"off" measurements in one pass, then installs once.

### 9.5 Failure artifacts

Saved under `target/e2e/<timestamp>/<os>/<scenario>/`: recorded, expected and difference WAVs; daemon
log; OS audio state (`pw-dump` on Linux; the CoreAudio device list on macOS; endpoint formats plus
`apo.log` on Windows).

### 9.6 Flaky runs

A failed scenario reruns **once**. It is marked "environment flake" (reported, does not fail the run)
only if **both**:

- the first attempt had OS-reported discontinuities: PipeWire xrun notifications on the agent's
  streams, WASAPI `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY` on loopback packets, or CoreAudio
  processor-overload notifications; **and**
- the rerun passes.

Every other failure counts. A dropout the OS did not report means Resonance dropped audio.

### 9.7 Exit code and summary

The terminal prints a scenario × OS grid (pass/fail, added latency, resampling hops). Any failure,
latency regression or unexpected pass of an `expected_fail` gives a non-zero exit code.

## 10. CI

### 10.1 GitHub-hosted (light)

- `ci.yml`: unchanged (fmt, Clippy, unit tests on Linux and macOS).
- `windows.yml` (`windows-installer`): triggers change to tags + `workflow_dispatch` only. The local
  Windows VM builds, lints and tests Windows on every e2e run.
- `release.yml`:
  - first job: require a successful `e2e/quick` commit status on the tag's commit (via the GitHub
    API); otherwise refuse to publish
  - new `macos-15` job running the `quick` tier (section 6.5)

### 10.2 Self-hosted runner on this host

- Runs as a dedicated unprivileged user in the `kvm` group with its own rootless podman. It cannot read
  the user's home or reach the desktop audio session. `RESONANCE_E2E_HOME` points at a group-readable
  image directory; base images are opened read-only, and overlays live in the runner's own directory.
- New `e2e.yml`, `runs-on: [self-hosted, resonance-e2e]`, one run at a time (`concurrency`):
  - `pull_request`: only if the head repo is this repo, or the PR has the `run-e2e` label
  - `push` to master: so every master commit (rebase merges create new SHAs) gets a status the
    release gate can check
  - `schedule`: nightly `full`
- Posts `report.md` as the job summary and the `e2e/quick` commit status; uploads the run folder on
  failure.
- Repo setting to enable: "Require approval for all outside collaborators" for fork workflows.

## 11. Spikes (milestone 0)

Each spike is throwaway; results are appended to this spec before milestone 1 starts.

| # | Question | Pass | Fallback |
|---|---|---|---|
| 1 | Does Intel macOS 15 run under quickemu on this AMD CPU with BlackHole, and does the daemon's Process Tap receive real audio when launched by the LaunchAgent, with no TCC prompt after base-image prep? | Non-silent tap blocks from a played file, unattended | Try macOS 14.6. If TCC can't be pre-granted, add a one-time manual grant to the image checklist (signing identity is stable). If macOS can't boot at all, macOS is covered only by the release-time arm64 job, and the report says so. |
| 2 | Which Windows virtual device gives ≥ 8 channels at 44.1–192 kHz, accepts our APO, and loops back bit-exactly in a flat test? Candidates: Scream (signed), Microsoft SysVAD (test-signed). | One candidate meets all three | Stereo QEMU HDA endpoint only; Windows multichannel scenarios become `platforms`-restricted with the reason recorded. |
| 3 | Can a GitHub `macos-15` runner grant audio capture to the daemon and capture real audio? | Non-silent capture in a test workflow | The release job runs offline + offline-render chain tests only; its summary states live arm64 was not covered. |
| 4 | Are linear-phase and partitioned convolution outputs bit-identical across different caller block sizes (64 / 128 / 480 / 1024 / random)? | Bit-identical | Those scenarios use `tolerance_dbfs = -120`, and the report says why. |

## 12. Build order

- **M0:** spikes 1–4.
- **M1:** `resonance-dsp::analysis` extraction; snapshot IPC request; `resonance-e2e` agent;
  `xtask`; Linux container; scenario format; report; baselines. Done when
  `cargo xtask e2e --os linux --tier full` runs green (with `expected_fail` entries for known bugs).
- **M2:** Windows base image + provisioning, Windows agent support, verified APO on/off.
- **M3:** macOS base image + provisioning, macOS agent support.
- **M4:** self-hosted runner, `e2e.yml`, release gate, release-time arm64 job, `windows-installer`
  trigger change.

**M1 done (2026-10-07):** `cargo xtask e2e --os linux --tier full` green (53 scenarios, three consecutive runs); no `expected_fail` entries needed; 3 findings logged (DSP-E1, DSP-E2, PW-E1 in the audio-robustness findings).

Linux comes first because it is the cheapest environment: the agent, report and checks are proven
there before the slow VMs are involved.

**Harness work vs. bug fixing:** when a milestone's new tests expose existing bugs, they are logged
as robustness findings and marked `expected_fail` with their finding ID. They are not fixed inside
this sub-project unless the harness itself cannot work without the fix. Fixing them is sub-project 3.

## 13. Risks

- **macOS VM on AMD** may be unstable or may stop working with a future macOS; Intel macOS ends at
  macOS 26. Mitigation: spike 1, plus the arm64 release job as the second line.
- **VM timing jitter** under host load may cause real dropouts. Mitigation: the flake rule (9.6) and
  dedicated vCPUs.
- **Microsoft download changes** can break unattended Windows image builds. Mitigation: cache the ISO
  under `$RESONANCE_E2E_HOME`.
- **Disk:** about 100 GB for images and caches.
- **First image builds** take about 1–2 hours per VM; later runs reuse them.

## 14. Spike results (2026-10-07)

| # | Result | Consequence |
|---|---|---|
| 1 | not run (needs a manual macOS install in a SPICE window) | M3 plan waits for this spike |
| 2 | not run (Windows 11 VM + driver install) | M2 device choice open |
| 3 | not run (needs a throwaway branch pushed to origin and a workflow run) | release-job capture tier undecided |
| 4 | convolution: bit-identical at block sizes 64–4096 and random. Linear-phase EQ: differs at every size tried (max abs diff ~0.57 against block 1024, persisting to the end of the signal) | exact compare is valid for IIR and convolution only; linear-phase scenarios are `expected_fail` until the block-size dependence is fixed (a product finding, not fixable by `tolerance_dbfs`) |
