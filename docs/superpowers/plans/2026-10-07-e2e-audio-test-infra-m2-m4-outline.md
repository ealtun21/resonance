# Live-audio test infrastructure: M2 (Windows), M3 (macOS), M4 (CI) outline

Follows `2026-10-06-e2e-audio-test-infra-m0-m1.md` (M1 shipped in PR #68). Spec:
`docs/superpowers/specs/2026-10-06-e2e-audio-test-infra-design.md`, with the spike results in section 14.
This is an outline at task granularity: each task names files, interfaces and a verification command, but
the step-by-step code is written when the task is picked up (the spikes changed enough assumptions that a
code-level plan written now would go stale).

## What the spikes changed

- **Windows:** quickget cannot download the Windows 11 ISO (Microsoft blocks it). The image is Windows 10
  Enterprise LTSC 2021 evaluation with a patched `autounattend.xml`. Scream 4.0 needs its catalog
  re-signed locally and test-signing on. Flat 8-channel loopback is bit-exact at 44.1 to 192 kHz. The APO
  does not instantiate on the 8-channel endpoint (open question, task W0).
- **macOS:** a GitHub-hosted macOS runner cannot run the live tier (the daemon blocks in a HAL plug-in
  property query). macOS live testing is VM only. macOS Sequoia boots under quickemu on this host; the
  install was driven with QMP absolute mouse events plus HMP keys.
- **Global constraint carried over:** never touch the user's desktop audio session; everything that plays
  or records runs in the VM. Security relaxation (self-signed certs, test-signing, TCC rows) is allowed
  inside the throwaway VMs only.

## M2: Windows

### W0: APO on multichannel endpoints (DONE 2026-10-08, see spec section 14)

Result: Scream needs the APO in the GFX slot (2), not EFX (7); with that, `LockForProcess ch=8` appears. The text below is the original plan, kept for the record.

Question: why does audiodg not instantiate `resonance_apo.dll` on the 8-channel Scream endpoint (no
`LockForProcess` in `apo.log`, although the FxProperties slot is attached and verified)?
Candidates, cheapest first: (1) the endpoint's "disable enhancements" flag (the
`{e4870e26-3cc5-4cd2-ba46-ca0a9a70ed04}` / `Disable_SysFx` property) is set by Scream's INF; (2)
`CBaseAudioProcessingObject` format negotiation rejects the multichannel `WAVEFORMATEXTENSIBLE`
(override `IsInputFormatSupported` and log what it is asked); (3) Scream's pin offers no effects mode.
Method: log from `IsInputFormatSupported`/`Initialize` into `apo.log`, rebuild in the VM
(`contrib/windows/build-apo.ps1`), re-run `run.ps1`. Done when `apo.log` shows `LockForProcess ... ch=8`
or the cause is recorded in spec section 14 with a decision.

### W1: Windows image build and provisioning (`xtask`)

- `cargo xtask e2e image windows`: download the LTSC eval ISO (cache under `$RESONANCE_E2E_HOME`), build
  `unattended.iso` from the checked-in `autounattend.xml` (spike dir), run quickemu headless, wait for SSH.
  A second pass installs Win32-OpenSSH (zip from GitHub; the FoD capability does not install), VS Build
  Tools, rustup (as SYSTEM via `schtasks`), re-signs and installs Scream, sets test-signing, disables the
  HDA endpoint, installs the APO, and snapshots the disk as the base image.
- Interface: a `windows` provisioning script set under `contrib/e2e/windows/` (move the spike scripts
  there), driven over ssh by a small Rust `xtask::vm` module (`ssh`, `scp`, `run_as_system`).
- Verification: `cargo xtask e2e image windows` twice; the second run is a no-op.

### W2: Windows backend of the agent (`crates/resonance-e2e/src/windows/`)

- `windows::pw` equivalent: `play_and_record` using cpal (play) plus the daemon's loopback capture, on one
  clock, returning the same `PlayRec` type so `runner`, `compare`, `report` are shared. Hop attribution:
  `WINDOWS_RESONANCE_STEPS` (none; Windows does its resampling in the audio engine).
- `windows::env`: endpoint format via the MMDevices registry value (port `setfmt.ps1`), start/stop
  `audiosrv`, APO on/off verification (`apo.log` markers, mirrors `verify_on`/`verify_off`).
- Move `runner` OS-specific parts behind a small trait (`Backend`) instead of `cfg(linux)`; keep
  `scenario` platform filters (`platforms = ["linux", "macos"]`) as the way to exclude scenarios.
- Verification: unit tests for the pure parts (registry blob patching, log parsing); the full run is
  `cargo xtask e2e --os windows --tier quick` green.

### W3: Windows scenarios

Reuse the Linux scenario files; multichannel APO scenarios run on Windows once Scream is attached with GFX (W0). Add flow-through (APO off, bit-exact loopback) scenarios for 8 ch at 44.1/48/96/192 kHz, which
the spike already proved. Latency baselines in `contrib/e2e/baselines/windows.toml`.

## Status 2026-10-08

W0-W3 done and verified (`cargo xtask e2e --os windows`, quick and full tiers pass). M4 workflows written, not yet run on a runner. M3 written, blocked on guest stability: see spec section 14.2. `image windows` from scratch is untested (the overlay base was hand-built in the spike). Windows latency baselines are not implemented.

## M3: macOS

### M0: macOS spike (done 2026-10-07)

Passed: the Sequoia VM installs unattended-by-script (recipe in `docs/superpowers/spikes/2026-10-07-macos-vm/`),
the daemon runs from a LaunchAgent as a signed minimal `Resonance.app`, and after clicking Allow on the two
first-launch prompts the tap reports audio (705 of about 1960 callbacks) on reruns without a prompt. Open:
the 16-channel tap on BlackHole 16ch, and whether the grant survives a rebuild (the plan's stable signing
identity: run `contrib/macos/make-signing-cert.sh` in the image).

### M1 (macOS): image build and agent backend

Same shape as W1/W2: `cargo xtask e2e image macos` (install automation script kept in
`contrib/e2e/macos/`, base disk snapshot), `macos::env` (SwitchAudioSource, BlackHole channel count,
launchd start/stop of the daemon), `macos::pw` equivalent (CoreAudio play + record on BlackHole through
the same `PlayRec`). Scenario filter excludes anything the stereo-bounded / tap-bounded macOS path cannot
do, with reasons.

## M4: CI

- Self-hosted runner as described in spec section 10.2, `e2e.yml` (`pull_request` for same-repo heads or
  the `run-e2e` label, `push` to master, nightly `full`), job summary from `report.md`, `e2e/quick`
  commit status.
- `release.yml`: require the `e2e/quick` status on the tag commit. The planned `macos-15` live quick job
  is dropped (spike 3); the release job runs the offline tier only and says so in its summary.
- `windows-installer` workflow: triggers change to tags plus `workflow_dispatch`.
- Verification: a dry-run PR from a branch in this repo; the status appears and gates a test tag.

## Open findings that affect these milestones

DSP-E2 (IIR to FIR switch-over depends on block size) and the Windows multichannel APO question (W0).
The Linux quick tier already covers everything else; see `contrib/e2e/README.md` for how scenarios work.
