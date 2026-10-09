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
- Through the Process Tap the recording is **not** bit-equal to the offline render, and cannot be: the tap's
  leg of the aggregate device contains a sample-rate converter (a low-pass at about 0.85 of Nyquist, no
  drift or clock option turns it off) while a sub-device of the same aggregate is bit-exact. Evidence and
  the options tried: spec section 14.4; `tapcap.m` is the tool (build it with
  `clang -fobjc-arc -framework Foundation -framework CoreAudio tapcap.m -o tapcap`, sign it into the
  `Resonance.app` bundle like the daemon, run it from the GUI session with `SECS=n OUT=file`, and play
  a noise stimulus from another process while it runs).
- So macOS is judged against the render per octave on every channel: transfer gain within 0.03 dB and
  in-band SNR of at least 50 dB (measured: below 0.001 dB and 57-86 dB), plus the pilot's pitch. Octaves stop at
  0.33 of the sample rate, below the converter's roll-off.
- The VM's audio threads drop whole 512-frame buffers now and then (zero blocks in the recording; the daemon's ring
  reported no underrun, spec section 14.4); blocks holding one are excluded from the gain estimate and counted, and a failing run with dropouts is rerun
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
