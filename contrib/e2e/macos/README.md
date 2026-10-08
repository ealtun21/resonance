# macOS e2e guest

`cargo xtask e2e --os macos` boots a throwaway overlay of the base image (Intel macOS 15 under
quickemu, 4 vCPU), copies the source in, runs `prepare.sh` (build, signed `Resonance.app`, LaunchAgents),
starts the agent inside the logged-in session through launchd, and pulls `~/e2e-out` back.

## Topology

BlackHole devices only, no real hardware:

| Scenario channels | Played into (default output, tapped) | Daemon renders into (recorded) |
|---|---|---|
| 2 | BlackHole 2ch | BlackHole 16ch |
| 16 | BlackHole 16ch | BlackHole 64ch |

The tap takes its device's whole channel layout, so other channel counts are "not applicable" with a reason.
The result goes to a *different* BlackHole so the unprocessed audio is never in the recording.

## What is checked, and why it is not bit-exact

- Harness: playing into a BlackHole and recording the same one is bit-exact (`examples/loopback_check.rs`,
  also run as the Resonance-off reference of every format).
- Through the Process Tap the recording is **not** bit-equal to the offline render: the tap's aggregate
  drift-compensates (resamples). Measured against the render: flat within about 0.1 dB from 50 Hz to 16 kHz,
  coherence about 0.985, a roll-off above 20 kHz. So macOS is judged by per-octave transfer gain (±0.3 dB,
  GCC-PHAT aligned per 0.5 s segment), coherence, and the pilot's pitch.
- The daemon drains its ring to its slack and zero-fills on underrun, so scheduling jitter shows as dropouts;
  blocks holding one are excluded from the gain estimate and counted, and a failing run with dropouts is rerun
  once (a passing rerun is a flake).
- Latency = the daemon path's floor over five fresh daemon starts minus the bare BlackHole loopback, plus the
  chain's own delay from the render. The floor is a property of the ring (0-85 ms of slack by callback phase),
  so the gate is wide (30 % or 30 ms); negative values mean the daemon path is shorter than the BlackHole
  loopback reference, which carries its own safety offset. Use the baseline, not the sign.

## The base image (one-time, partly manual)

Built by hand once (recipe: `docs/superpowers/spikes/2026-10-07-macos-vm/`), then `provision.sh` over ssh:
BlackHole 2/16/64 ch from the vendor pkgs, the `audiodev` helper, the stable signing identity, auto-login.
Three prompts must be answered once with Allow, since TCC grants are keyed to the signing identity and then
survive rebuilds: microphone for `Resonance`, "Record Your System Audio" for `Resonance`, microphone for
`resonance-e2e`. `xtask` finds the image in `$RESONANCE_E2E_HOME/macos/base/`.
