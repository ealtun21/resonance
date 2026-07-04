# Troubleshooting

## Verify the audio path (`resonance verify`)

Rather than trusting your ears, `resonance verify` plays real tones through the
live path, captures the daemon's post-DSP output, and checks pitch (±25 %
windowed FFT peak) and frequency response (least-squares sine fit) against the EQ
prediction — or against a saved `--save-baseline` / `--baseline` A/B pair.
Scriptable (`--json`, exit code). Works on all three platforms.

```sh
resonance verify --json
resonance verify --save-baseline before.json
# …change EQ…
resonance verify --baseline before.json
```

## Linux / PipeWire

- **No "Resonance EQ" sink / no audio** — make sure PipeWire (and WirePlumber)
  are running and the daemon has started (`resonance status`). The daemon sets
  its virtual sink as the default via WirePlumber metadata.
- **Wrong pitch after a Bluetooth device switch** — the pipeline follows the
  output device rate and resamples at the boundaries; if you hit a pitch/rate
  issue, check the negotiated rate in `resonance status` (`format` line) and file
  it.
- **Stale socket after a crash** — the daemon cleans up its socket + pidfile on
  exit; if a stale `.sock` blocks startup, remove
  `$XDG_RUNTIME_DIR/resonance.sock`.

## macOS

- **Every block is silent / no capture** — the Screen Recording grant is missing
  or was invalidated. Re-add `Resonance.app` in System Settings → Privacy &
  Security → Screen Recording and launch via Launch Services (not a terminal).
  See [Installation → macOS](Installation#macos).
- **Grant disappears after rebuilding** — every `build-app.sh` rebuild changes
  the bundle's cdhash, so TCC drops the grant. Sign with a stable identity
  (`SIGN_IDENTITY=…`) to keep it across builds.
- **Launched from a terminal doesn't work** — TCC attributes a terminal-spawned
  child's permissions to the terminal. Use `open ~/Applications/Resonance.app`.

## Windows

- **Some apps go silent** — DRM/protected-audio apps may mute while the unsigned
  APO is active. This is expected; uninstall (Add/Remove Programs) to detach the
  APO and restore settings.
- **No effect** — confirm the installer set `DisableProtectedAudioDG` and the APO
  is attached to your current playback device; switching the default device may
  require re-attaching.

## Logs

Set `RUST_LOG=debug` and restart the daemon for verbose logging. Log locations
are listed in [Configuration → File locations](Configuration#file-locations).

If something looks wrong, open an
[issue](https://github.com/ealtun21/resonance/issues) with the daemon log and
your `resonance status` output.
