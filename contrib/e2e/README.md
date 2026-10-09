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
