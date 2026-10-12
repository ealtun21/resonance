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

## Driving the guest desktop (lifecycle and consent work)

`vmctl.sh` is the host-side handle on a throwaway overlay in `$RESONANCE_E2E_HOME/macos/lifecycle` (own ssh
port 22722, so it runs beside `cargo xtask e2e`, which picks free ports and its own overlays):

    contrib/e2e/macos/vmctl.sh reset       # fresh overlay of the base image (discards guest changes)
    contrib/e2e/macos/vmctl.sh boot        # headless, plus a QMP socket for input and screenshots
    contrib/e2e/macos/vmctl.sh ssh 'cmd'   # shell as e2e; `script` takes a script on stdin
    contrib/e2e/macos/vmctl.sh shot [png]  # 1920x1080 screendump of the console (default lifecycle/shot.png)
    contrib/e2e/macos/vmctl.sh click X Y [N] [right]   # pixels of that screenshot
    contrib/e2e/macos/vmctl.sh type 'text' / key ret meta_l+spc
    contrib/e2e/macos/vmctl.sh lifecycle   # sync, release-build the .app, run the lifecycle matrix
    contrib/e2e/macos/vmctl.sh kill

What each piece needed, and what works (all measured on macOS 15.8.1, quickemu, 16 vCPU):

- **Screen and input**: QMP `screendump` and `input-send-event` (`vmgui.py`; HMP `mouse_move` does not move
  the pointer). The earlier "black screen with a cursor" was the lock screen: auto-login runs, then macOS
  locks. Typing the password (`e2etest`) through `vmctl.sh type` / `key ret` unlocks it.
  A "disk you attached was not readable" dialog from quickemu's extra drive is harmless (Ignore).
- **Running things in the GUI session**: ssh sessions have no audio or TCC context, so audio work goes through
  a throwaway LaunchAgent (`lifecycle/gui-run.sh 'cmd'`), which makes the process a child of launchd in
  `gui/501`, the same attribution as the product's own daemon agent (older notes found `launchctl asuser`
  from ssh keeps sshd as the responsible process; not re-measured here).
- **Consent dialogs**: the first start of a binary whose signing requirement has no grant raises two dialogs,
  "access the microphone" then "Record Your System Audio". Both are clicked through QMP: with the guest at
  1920x1080 the Allow buttons are at (1018, 393) and (1018, 407). Until they are answered `coreaudiod` blocks
  the daemon's tap setup after "Process tap ready", while IPC, status and `daemon stop` keep working.
  `TCC.db` is readable with `sudo sqlite3` (SIP filesystem protection is off in the guest) but clicking the
  dialog is simpler than writing a `csreq`.
- **Grants are one row per bundle id and follow the last signature**: after answering the dialogs for an
  ad-hoc build, the cert-signed build prompted again (and the reverse). Do not mix identities in one overlay;
  `reset` to get back to the image's grant.
- **Tray**: the menu opens on a *right* click on the menu-bar icon (left click launches the GUI), then the
  items are plain clicks (Power, Daemon > Start / Stop / Restart worked end to end).
- **Not drivable**: the egui GUI. The guest has no Metal adapter, so `resonance-gui` exits at start with
  `RequestAdapterError(NotFound)`; the tray's "Open UI" cannot be exercised either. The TUI was not driven.

`lifecycle/lifecycle.sh BUNDLE [UPGRADE_BUNDLE]` is the matrix `vmctl.sh lifecycle` runs: install, launchd agent,
`resonance verify` through the live tap, `kill -9` recovery, stop/start, rate changes (44.1/96/48 kHz), a pinned
output with the OS default moved, upgrade over a running daemon, and uninstall cleanliness. It needs a full
release build of the app (about 12 minutes cold) and a granted identity, so it is not part of the quick tier.

## The base image (one-time, partly manual)

Built by hand once (recipe: `docs/superpowers/spikes/2026-10-07-macos-vm/`), then `provision.sh` over ssh:
BlackHole 2/16/64 ch from the vendor pkgs, the `audiodev` helper, the stable signing identity, auto-login.
Three prompts must be answered once with Allow, since TCC grants are keyed to the signing identity and then
survive rebuilds: microphone for `Resonance`, "Record Your System Audio" for `Resonance`, microphone for
`resonance-e2e`. `xtask` finds the image in `$RESONANCE_E2E_HOME/macos/base/`.
