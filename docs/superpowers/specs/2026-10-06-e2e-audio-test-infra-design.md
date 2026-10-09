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
| 1 | **pass** (2026-10-07). macOS Sequoia 15.8.1 (Intel) boots and installs under quickemu on this AMD host; the whole install was automated with HMP keys, QMP mouse clicks and screenshots (recipe in `docs/superpowers/spikes/2026-10-07-macos-vm/`, install about 1.5 h). Daemon built in the VM, run from a LaunchAgent as a minimal ad-hoc-signed `/Applications/Resonance.app` with BlackHole 2ch as the default output (set with a small CoreAudio tool; Homebrew no longer supports Intel, BlackHole comes from the vendor `.pkg`). First launch raises two prompts (microphone, then "Record Your System Audio"); clicking Allow on both is the one-time grant. After that `HAL tap IOProc` reports **705 with audio** out of about 1960 callbacks while five system sounds play, identically on two further unattended reruns (no prompt). Not yet measured: the 16-channel tap (`16 ch` on BlackHole 16ch) | M3 proceeds as designed with a one-time automated grant in the image build. The same prompt explains spike 3: coreaudiod blocks `AudioDeviceCreateIOProcID` until it is answered, so on a runner nobody can answer it |
| 2 | **Scream works with a re-signed catalog** (2026-10-07). Windows 10 LTSC eval VM under quickemu, unattended install and SSH both work (recipe in `docs/superpowers/spikes/2026-10-07-windows-vm/`). Scream 4.0 as shipped does not install (catalog signed by an individual Sectigo cert that expired 2023-07-06, no timestamp: setupapi `0x800b0109`); after re-signing `scream.cat` with a local code-signing cert trusted in Root + TrustedPublisher and `bcdedit /set testsigning on`, it installs. Endpoint format is set by patching `{f19f064d-...},0` under the endpoint's MMDevices `Properties` (as SYSTEM, after taking key ownership) and restarting AudioEndpointBuilder + Audiosrv. Criterion 1 (8 ch at 44.1 / 48 / 96 / 192 kHz): **pass**. Criterion 3 (flat loopback bit-exact): **pass** at all four rates, 8 ch, seeded noise through `play.exe` and `resonanced --measure-loopback`, 0 differing samples, identical lag on all channels (1056 frames at 48 kHz, 970 at 44.1, 2112 at 96, 4224 at 192). Criterion 2 (APO attaches with `ch=8`): **pass after W0 (2026-10-08)**; the first attempt failed because of the slot choice, see the W0 paragraph below this table. The registry attach is present and verified, audiosrv was restarted, but audiodg never instantiates the APO for the 8-channel Scream endpoint (no `LockForProcess` line; the only instance in `apo.log` is the earlier 2-ch HDA run). Switching the endpoint back to 2 ch gave `0x88890008` (unsupported format) from the player, so the 2-ch control was not completed | M2 can use Scream (test-signed) for 8-ch WASAPI plus loopback exactness today; APO processing of multichannel endpoints needs its own investigation (base-class format negotiation for >2 ch, or the Scream pin not offering effects) before Windows multichannel scenarios can claim APO coverage. W0 resolved this: attach with GFX (slot 2) and APO scenarios work at 8 ch |

**W0 result (2026-10-08).** Cause: Scream is a legacy WDM driver that ships no effect slots and no processing modes, and audiodg does not load an EFX (slot 7) APO for such an endpoint. Hypotheses 1 (`Disable_SysFx`: not set on either endpoint) and 2 (format negotiation) were not the cause. Attaching the same CLSID in the legacy GFX slot (`{D04E05A6-...},2` plus `{D3993A3F-...},2` = DEFAULT mode, all other slots removed; `spikes/2026-10-07-windows-vm/attach-slot.ps1 -Slot 2`) makes audiodg log `LockForProcess hr=0x0 ch=8 rate=192000 maxFrames=1920` and `APOProcess first call ... ch=8` on the 8-ch endpoint. Distinguishing signal: the HDA endpoint carried original SFX/MFX entries (slots 5/6) and works on EFX; Scream had none of slots 1/2/5/6/7. `install-apo.ps1` and `attach-endpoint.ps1` currently pick GFX only when a legacy slot exists without a modern one, so an endpoint with no slots at all gets EFX and is silently unprocessed. Decision: the e2e provisioning attaches Scream with `attach-slot.ps1 -Slot 2`; the production installer is left unchanged because changing the no-slots default would move working USB/Bluetooth endpoints off EFX without a way to test them here. Follow-up (separate issue): detect "driver supports no modes" properly before changing the default.
| 3 | **not viable as-is** (2026-10-07). `macos-15` runner is macOS 15.7.9, SIP disabled, TCC.db writable (a `kTCCServiceAudioCapture` row can be inserted). BlackHole 2ch installs and appears after `sudo killall coreaudiod`. The daemon builds and creates the process tap and aggregate (`Process tap ready`, 48 kHz 2 ch; 44.1 kHz when no BlackHole), then **blocks permanently** on the `resonance-coreaudio` thread inside `AudioObjectGetPropertyData` -> `HAL_HardwarePlugIn_ObjectHasProperty` -> `semaphore_wait` (stack from `sample`), before "CoreAudio ready" is logged and before any `HAL tap IOProc` line. Same hang with output = BlackHole 2ch and with the runner's built-in Null Audio Device, with and without the TCC row, so TCC is not the cause; the VM run shows the real cause: `AudioDeviceCreateIOProcID` blocks in coreaudiod until the "Record Your System Audio" prompt is answered, and nobody can answer it on the runner (a TCC.db row alone did not release it) | the release job cannot run the live quick tier on a GitHub-hosted macOS runner; macOS live e2e = self-hosted/VM only (M3), release job offline-only |
| 4 | convolution: bit-identical at block sizes 64–4096 and random. Linear-phase EQ: bit-identical when the chain starts in the FIR path (what the harness uses); differs at every size tried (max abs diff ~0.57 against block 1024, persisting to the end of the signal) only across the IIR-to-FIR switch-over (DSP-E2) | exact compare is valid for IIR, convolution and linear phase from a fresh chain; only a live mode switch is block-size dependent (finding DSP-E2, not covered by a scenario) |

### 14.1 Windows leg results (2026-10-08)

`cargo xtask e2e --os windows` boots an overlay of the base image, builds and installs in the guest and
runs the agent; `--tier full` passes every applicable scenario (22 run, 31 reported as not applicable with
reasons). What the first full runs taught us, each handled in the harness rather than hidden:

- **Windows' audio engine limits float mixes that approach full scale.** `fx-fidelity` (render peak 1.0),
  `convolution` (peak 2.1) and `long-full-eq` failed with bursts of small errors around each peak (first
  attempt: gain 0.59, 4 % residual for the convolution). The APO engine itself is exact: driving the APO FFI
  directly (`examples/apo_vs_render.rs`) gives output bit-identical to the offline render at 441, 480 and
  1024 frame blocks. Resolution: the runner renders first and, if the render peaks above -1 dBFS, plays the
  stimulus attenuated by a power of two (exact in f32) and re-renders; the report notes the attenuation.
- **DSP-E3 (new, open, benign).** With the APO worker rebuilding the chain after format lock, the
  linear-phase output differs from the offline render by about 1e-13 (-265 dBFS), vs bit-identical when the
  engine is driven before the worker's first rebuild. Cause not isolated (kernel built on the worker vs
  inline). The Windows `linear-phase` scenario therefore uses `compare_by_os.windows = tolerance -240 dBFS`.
- **QEMU HDA is stereo at 48 kHz only; Scream is 8 channels only** (the registry format is ignored when the
  driver does not offer it) **and its rate table lacks 176.4 kHz**. A rejected format makes Windows drop the
  endpoint until it is re-enabled; the runner recovers (`select-endpoint.ps1 -Force`) and reports the failure.
- **Endpoints change GUID on disable/enable**; the daemon's attach-on-new-endpoint path re-attaches the APO.
- **Clock skew host/guest broke incremental builds** (tar restored host mtimes older than the guest's build
  outputs, so cargo reused a stale binary). Source sync uses `tar -m`; guest builds set `CARGO_INCREMENTAL=0`.
- Latency is not measured on Windows yet (no Resonance-off reference without uninstalling the APO).

### 14.2 macOS leg results (2026-10-08)

`cargo xtask e2e --os macos` works end to end on the base image: quick tier 4 of 4 on repeated runs, full tier
27 scenarios (2 and 16 channels, 44.1 to 192 kHz; 26 not applicable with reasons) all pass, one flake.
Topology and checks are in `contrib/e2e/macos/README.md`. What it took, and what it found:

- **The earlier "freezes" were my diagnosis, not the guest.** Screenshots came from a stale file when the
  screendump failed, and ssh banner timeouts under load looked like a hang. The guest stayed up through the
  full tier with 4 vCPUs.
- **MAC-E1 (finding): the Process Tap is not bit-transparent.** Playing into BlackHole 2ch and recording the same
  device is bit-exact, but through tap and daemon the recording matches the render only to coherence about
  0.985, with a flat -0.1 dB passband and a roll-off above 20 kHz. Cause and measurements: section 14.4 (the
  tap's leg of the aggregate resamples; the 0.985 and -0.1 dB were dropout and alignment artefacts of the old
  check, the converter itself is flat to 0.001 dB in band).
- **MAC-E2 (finding): the daemon underruns under VM scheduling jitter.** About 30 % of 4096-frame blocks held a
  zero-filled underrun in some runs, and the delay occasionally jumps mid-run (the ring dropping its backlog
  beyond the 4096-frame slack). Per-segment alignment and dropout accounting keep the gain estimate valid; a
  failure with dropouts reruns once.
- **MAC-E3 (finding): the ring's latency is set by callback phase at start**, 0-85 ms of slack plus occasional
  startup excess, so the same chain measured 29 to 228 ms across daemon starts. The harness reports the floor
  over five starts plus the chain's exact delay.
- Alignment uses GCC-PHAT: plain cross-correlation locked onto the wrong lag for EQ'd sweeps (it reported
  -135 ms added latency for the EQ scenario).
- TCC grants (daemon: microphone and system audio; agent: microphone) survive rebuilds because the signing
  identity is stable; they are answered once while making the image.

### 14.3 Windows image from scratch (2026-10-08)

`cargo xtask e2e image windows` builds the base image with no hand steps (third attempt; ISO and virtio media
cached) and `--os windows --tier quick` passes 5 of 5 on it with the same latency figures as the hand-made image.
The first two attempts found: the unattended install powers the VM off at the end of a setup phase instead of
rebooting (xtask now restarts a guest that is not running while it waits for ssh), and the OpenSSH
feature-on-demand capability does not install on this image, so sshd never started (the answer file now runs
`sshd.ps1`, which installs the Win32-OpenSSH release; validated on an overlay of the working image first).

### 14.4 macOS: can the recording be bit-equal? (2026-10-09)

Answer: **no, and not only in the test.** The Process Tap's leg of an aggregate device contains a
sample-rate converter that cannot be switched off, so the product path (tap, daemon, output) is not
bit-transparent either. MAC-E1 stands, now with its cause and numbers.

Method: `contrib/e2e/macos/tapcap.m` creates the tap and aggregate the way the daemon does, with
every option settable from the environment, and records all input channels of the aggregate. A
broadband noise stimulus was played into BlackHole 2ch from another process and compared with the
source sample by sample (48 kHz unless noted).

| Question | Result |
|---|---|
| Is the aggregate or the tap the culprit? | Same aggregate (`SUB=BlackHole2ch_UID MASTER=1 MUTE=0`), same IOProc buffer: the BlackHole **sub-device channels are bit-exact** (100 % of samples equal, error -600 dBFS); the **tap channels are not** (0 % equal, error -19 dBFS broadband). |
| Shape of the tap leg | Flat to 0.00 dB with coherence 1.000 up to 16.8 kHz, -0.7 dB at 19.2-20.2 kHz, -4.5 dB at 20.2-21.1, -12.5 at 21.1-22.1, -31 at 22.1-23, -65 dB above: a linear-phase low-pass at about 0.85 of Nyquist plus 1632 frames (34 ms) of extra delay. At 44.1 kHz and 96 kHz the same shape sits at the same fraction of Nyquist (96 kHz: -0.4 dB at 38.4-40.3 kHz, -27 dB at 44.2-46.1 kHz). The aggregate's nominal rate does matter: setting it to 96 kHz over a 48 kHz tap resamples and doubles the callbacks. |
| In-band error | After removing everything above 16 kHz from both signals: -95 dBFS rms in clean stretches (80 dB below a -15 dBFS signal), bursts of -35 dBFS peak where the VM drops a buffer. Per 4096-sample octave averages in the runner: 57-86 dB SNR, gain within 0.001 dB. |
| `kAudioSubTapDriftCompensationKey` = 0 | No change at all (identical numbers to 1 digit). The composition read back from the device confirms `drift = 0`. |
| Drift quality 0 / 127 | No change. |
| Aggregate clock device / main sub-device = the tapped device, = the tap UID, stacked | Clock = the tapped device or main sub-device: a steeper filter (-1.6 dB at 19.2-20.2, -11 at 20.2-21.1, -37 at 21.1-22.1, -80 above) but still not exact (in-band SNR 58-70 dB, not better). Clock or main = the tap UID, stacked: the original filter. |
| Tap kind | Device-bound, global stereo, global mono: identical response. |
| `CATapMuteBehavior` unmuted / muted / muted-when-tapped | Identical. |
| Rate | The tap inherits the device rate (tap format reads 44.1/48/96/192 kHz) and the daemon's own `HalInputStream` takes the bypass path in every run (`capture matches output - no resampling`), so the daemon adds no conversion. |

So the resampling is in CoreAudio, between the tap and the aggregate's IOProc, for any tap
description and any aggregate description the public API accepts. The only bit-exact capture on this
machine is the sub-device input of a loopback driver, which is a different product (a virtual
device the user routes to) and is what the harness already uses for its Resonance-off reference.

What else keeps a macOS recording from matching the render:

- 512-frame zero blocks in the recording, about one per 0.7 s in this VM (11 in an 8 s tone,
  THD+N -18 dB). They are not the daemon's ring: with a counter in the output callback the daemon
  reported 0 underruns, 0 late callbacks and a 0 us lock wait, and the tap IOProc saw no zero block
  inside the stimulus, so the buffers are lost after the tap (the daemon's write into BlackHole 16ch
  or the agent's recording) when the VM's audio threads miss a deadline. A fixed 2048-frame buffer on
  the agent's streams cut the affected blocks from 22 of 69 to 4 of 69. A change in the daemon's output
  callback to read the ring all-or-nothing made no difference and was dropped. Same-device topology (tap and render to
  one BlackHole) did not change it either.
- This is why the soak, THD+N and rate-stress scenarios stay "not applicable" on macOS: the steady
  tone cannot be gap-free here, and a floor would only measure the VM.

What is checked instead (and what changed): the recording is judged against the offline render on
**every channel** (it was channel 0 only), per octave up to 0.33 of the sample rate, which is
well below the converter's roll-off. Limits, from the full tier (all linear chains, 2 and 16
channels, 44.1-192 kHz): gain within 0.03 dB (measured below 0.001 dB; was 0.3 dB) and in-band SNR
at least 50 dB (measured 57-86 dB; was coherence 0.95, about 13 dB). Fidelity (nonlinear, measured
0.012 dB / 39 dB) and the dithered all-effects chain (0.5 dB / 26 dB: DynBoost's limiter and the
dither PRNG do not line up with the render) have their own limits in `effects.toml`. Every run's notes
carry the per-octave worst case, and a "n of m segments bit-exact" line that reads 0 of 6 and keeps
the answer to this section visible.

## 15. Steady-tone scenarios: soak, THD+N and SNR, rate stress (2026-10-09)

Pitch and quality regressions that a bit-exact compare cannot see (a rate renegotiation that leaves the
pitch wrong, slow drift, a resampler that is merely mediocre) are caught by scenarios that play a pure
tone and judge the recording on its own. `kind` in the scenario file selects them (`render` is the
default: everything in sections 7-9); they live in `contrib/e2e/scenarios/tone.toml`, have no latency
baseline, and the analysis is in `checks.rs` / `common.rs` (shared by every OS).

| kind | stimulus | judged |
|---|---|---|
| `soak` | 600 s of the 997 Hz pilot at -12 dBFS (`full` only) | per 10 s window: pilot pitch error <= `PITCH_TOLERANCE` (0.01 %); level span across windows <= 0.05 dB; per 20 ms block: level within 0.25 dB of the run's median; no run of >= 16 all-zero frames |
| `thdn` | 8 s tone snapped to an FFT bin (`tone_hz`, default 997 Hz; 65536-frame rectangular window, so no leakage), steady middle of the recording | THD+N (everything but DC and the fundamental, harmonics 2-10 included) and SNR (everything that is not a harmonic) per channel against `expect.max_thdn_db` / `expect.min_snr_db`; pitch within tolerance |
| `stress` | the pilot through `events` (`force_rate`, `switch_device`, `switch_back`) | after each event, outage (longest zero run within `max_gap_ms` + 0.5 s) <= `max_gap_ms`; then each settled stretch, on whichever device carries the audio: pitch, level within 0.5 dB of the first stretch, no gap over 1 ms; the daemon still answers IPC at the end |

Scenarios: `soak-flat`, `soak-eq` (four linear bands, `profiles/tone-eq.toml`); `thdn-flat` (also in the
quick tier: about 12 s), `thdn-eq`, `thdn-content-48k-device-44k1` and `thdn-headset-16k-mono` (48 kHz
content converted by PipeWire's stream adapter), plus `thdn-hf-*` variants of those two with the tone at
0.4 x the lower rate (17.6 kHz and 6.4 kHz) where imaging and aliasing show; `bt-rate-stress`: 66 s, graph
rate 48 -> 44.1 -> 48 -> 44.1 -> 48 kHz, two round trips to a 16 kHz mono device, then 44.1 -> 48 kHz
again, one event every 6 s.

**Measured (2026-10-09, Linux container, and Windows APO on the HDA endpoint) and limits.**

| scenario | measured THD+N / SNR | limit |
|---|---|---|
| `thdn-flat`, Linux and Windows | -153.7 dB / 153.8 dB (the f32 quantisation floor of the tone) | <= -140 dB / >= 140 dB |
| `thdn-eq` (4 bands), Linux and Windows | -152.5 dB / 152.5 dB | <= -140 dB / >= 140 dB |
| `thdn-content-48k-device-44k1` | -144.2 dB / 144.2 dB | <= -110 dB / >= 110 dB |
| `thdn-hf-content-48k-device-44k1` (17.6 kHz) | -143.6 dB / 143.6 dB | <= -110 dB / >= 110 dB |
| `thdn-headset-16k-mono` | -146.2 dB / 146.5 dB | <= -110 dB / >= 110 dB |
| `thdn-hf-headset-16k-mono` (6.4 kHz) | -144.6 dB / 144.6 dB | <= -110 dB / >= 110 dB |

The flat and EQ paths are bit-exact to the offline render, so THD+N is the render's, i.e. the stimulus's
own: the limit sits 13 dB above it. The resampled paths are not bit-exact; PipeWire's resampler measures
-144 dB, and the limit (-110 dB) is far below what a linear or low-order interpolator gives (-60 to -80 dB)
yet well above run-to-run noise. Soak: both runs 59 windows per channel, worst pitch error below
0.00001 %, level span 0.0000 dB, worst 20 ms block 0.013 dB from the median (limit 0.25 dB), no gaps. On
Windows (APO in audiodg, HDA endpoint) the same two soaks pass with worst pitch error 0.00094 % (flat) and
0.00052 % (EQ), against the 0.01 % tolerance: the VM's loopback clock is not the player's.
Rate stress: pitch error 0.0001-0.0005 % in every settled stretch, level within 0.001 dB, outages of 11-43 ms
for a graph-rate flip, 43-64 ms for returning to the stereo device, 0.43-1.0 s going to the mono device
(the daemon's reconnect after a channel-count change).

What this does not cover: the player and the null sink share the graph clock, so slow drift between a
real device clock and the player (what a Bluetooth link adds) cannot appear on Linux; the soak proves that
Resonance itself adds no drift, dropouts or level change over ten minutes. On Windows the same holds for
the VM's HDA endpoint. The rate stress changes the rate the daemon follows and its channel count; it cannot
reproduce a codec's own clock.

Platforms: `soak` and `thdn` run on Linux and on Windows when the format is a VM endpoint's (stereo 48 kHz
HDA; resampled variants are "not applicable" there as for every rate-conversion scenario); `stress` is
Linux only (needs a steerable graph). macOS lists all three as not applicable: its tap's aggregate resamples
(MAC-E1) and the daemon underruns under VM scheduling (MAC-E2), so no floor could be set honestly; revisit
once those are fixed.

Findings while building it: `reset_devices` created the second device once per `switch_device` event, so a
scenario with two switches got two nodes of the same name and the audio went to the other one (fixed:
one device however many events). A daemon switch from a mono device back to stereo takes up to ~1.6 s
(reconnect backoff), longer than the 500 ms default gap; `bt-rate-stress` states `max_gap_ms = 2500`.

## 16. Windows long-run slip: the recorder, not the product (2026-10-09)

`long-full-eq@96000x8` (60 s, 8 ch) failed deterministically after the soaks: the recording was bit-exact
against the render, then at one fixed frame lost 768 frames (8 ms) and was bit-exact again, shifted.
The frame is 3,932,160 = 3840 * 2^10 stimulus-plus-pre-roll frames: exactly where the recorder's
`Vec<f32>`, which doubles from the first callback's size (3840 samples) and grows inside the WASAPI
loopback capture callback, outgrows its 120 MiB block and reallocates to 240 MiB. That callback stalled for
up to 28 ms (instrumented: 1-3 ms otherwise); the engine's loopback buffer overran and dropped a few
ms. Whether the realloc is slow depends on guest memory state after the soaks, hence "passes alone".
Fix: the recording buffer is reserved and page-committed before the stream starts, so the callback never
allocates. A recording that still slips is now reported as `slip of N frames (skipped) at frame F`
(re-aligning the last 16384 frames of the window), the slowest capture callback is logged, and a failing
Windows scenario also saves `apo.log`.
