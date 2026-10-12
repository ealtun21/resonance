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

    cargo xtask e2e --os linux,windows   # Windows leg: see windows/README.md
    cargo xtask e2e image windows        # one-time base image build

Prerequisites: rootless podman (Linux); quickemu, qemu-img, genisoimage (VM legs). The container has its own PipeWire with no
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

## Steady-tone scenarios

`kind = "thdn" | "soak" | "stress"` (`scenarios/tone.toml`) play a pure tone instead of the render
stimulus and judge the recording on its own, not against the offline render (spec section 15):

- `thdn`: THD+N and SNR of a bin-centred tone (`tone_hz`, default the 997 Hz pilot); the scenario
  states `expect.max_thdn_db` and `expect.min_snr_db`. Measured numbers are in the report notes.
- `soak`: 600 s of pilot, full tier only; pitch per 10 s window, level span, dropouts.
- `stress`: pilot through `events` (graph rate flips, device switches); pitch, level and gaps after
  each event settles (`expect.max_gap_ms` + 0.5 s). Event `switch_back` returns to the first device.

Linux and (`thdn`/`soak` on matched-rate stereo) Windows; macOS lists them as not applicable with the
reason. They have no latency baseline.

## Latency baselines

`baselines/linux.toml` holds each scenario's added latency (ms). A run fails
above baseline + max(1 ms, 10 %). After an intended change:
`cargo xtask e2e --tier full --update-baseline`, then commit the diff.

## Windows lifecycle of the installed product

`cargo xtask e2e --os windows --tier full` also builds the real installer in the guest
(`windows/lifecycle-build.ps1`: release binaries + APO dll + Inno Setup, two versions of the same
build) and drives `windows/lifecycle.ps1`: install, APO in the audio path, daemon kill -9 / graceful
exit / restart, audio-engine restart, upgrade over a running install (daemon + tray + GUI up), silent
uninstall back to the exact pre-install machine state. Every step is followed by `lifecycle-probe.ps1`,
which plays a WASAPI tone and records the endpoint's post-APO loopback with
`resonance verify --no-daemon`: the +6 dB @ 1 kHz / -6 dB @ 150 Hz test curve gives a ~11.7 dB spread
("eq"), a bypassed chain ~0 ("flat"). The transcript is `target/e2e/run-*/windows/lifecycle.txt`
(`PASS`/`FAIL` per check; exit code = failures). ~15 min on top of the full tier, so never in quick.

Two guest hazards `xtask` now removes (`windows/harden.ps1`, once per boot, a reboot included): the
evaluation licence is expired and `wlms` powers the guest off one hour after every boot, and `w32time`
steps the clock by hours ~30 min after boot (the RTC is the host's local time read in the guest's
timezone), which makes cargo ship stale binaries. `lifecycle-build.ps1` drops the workspace
fingerprints for the same reason.

### Driving the guest desktop by hand (and for one-off investigations)

The guest autologons `Quickemu` into session 1, so there is a real interactive desktop; boot a
throwaway overlay yourself with a QMP socket (`quickemu --vm X.conf --display none --ssh-port N
--extra_args "-qmp unix:$DIR/qmp.sock,server,nowait"`, see `xtask/src/windows.rs::make_overlay`).

- **See and click:** `windows/vmdesk.py SOCK shot out.png | click X Y [right|double] | type TEXT |
  key ctrl-alt-delete`. It talks QMP `screendump` / `input-send-event`, i.e. emulated *hardware*
  input, which reaches winit/egui windows where `SendInput` from a scheduled task does not. Wizard
  pages, tray icons and their menus, the GUI and message boxes were all driven this way.
- **Start a program in the desktop session from ssh:** ssh lands in session 0. Use a one-shot task
  that runs as the logged-on user, elevated, only while logged on:
  `schtasks /create /tn x /sc once /st 23:59 /ru Quickemu /it /rl highest /tr C:\PROGRA~1\Resonance\resonance-gui.exe /f`
  then `schtasks /run /tn x`. Use the 8.3 path: `/tr` cannot carry a quoted path with spaces.
- **Run as SYSTEM** (rustup lives in its profile, `wlms` and some keys need it): same, `/ru SYSTEM`.
- **Silent installer:** `setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /TASKS=autostart`
  (`skipifsilent` means nothing is launched; that is how the Run key is exercised on its own).
- **Not emulable:** S3/hibernate (`powercfg /a`: no firmware support). QMP `stop`/`cont` freezes
  the guest and only proves it survives a pause; it does not step the guest clock, so test clock
  steps with `Set-Date`.
- **Pitfalls seen:** the real `Quickemu` desktop is 1024x768, the GUI window does not fit it; a
  killed tray leaves a ghost icon until hovered; Inno's download page 302s to an HTML page, fetch the
  installer from the `jrsoftware/issrc` release assets; `findstr` on an `.exe` prints binary noise.
