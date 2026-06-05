# systemd user service

`resonanced.service` runs the Resonance daemon as a per-user service alongside
PipeWire.

## Install

```sh
# Build + install the binary somewhere on PATH for your user.
cargo build --release -p resonance-daemon
install -Dm755 target/release/resonanced ~/.local/bin/resonanced

# Install the unit.
install -Dm644 contrib/systemd/resonanced.service \
    ~/.config/systemd/user/resonanced.service

systemctl --user daemon-reload
systemctl --user enable --now resonanced.service
```

## Manage

```sh
systemctl --user status resonanced
systemctl --user restart resonanced
journalctl --user -u resonanced -f
```

The daemon enforces a single instance via `$XDG_RUNTIME_DIR/resonanced.pid` and
removes its socket + pidfile on `SIGTERM`/`SIGINT`, so `systemctl --user stop`
leaves no stale files behind.
