#!/bin/sh
# Start a private session bus, PipeWire and WirePlumber, then run the command.
set -eu
export XDG_RUNTIME_DIR=/run/e2e
# The agent refuses to run without this (it reconfigures audio).
export RESONANCE_E2E_SANDBOX=1
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
DBUS_SESSION_BUS_ADDRESS=$(dbus-daemon --session --fork --print-address)
export DBUS_SESSION_BUS_ADDRESS
pipewire >/tmp/pipewire.log 2>&1 &
wireplumber >/tmp/wireplumber.log 2>&1 &
i=0
until pw-cli info 0 >/dev/null 2>&1; do
    i=$((i + 1))
    [ "$i" -gt 100 ] && { cat /tmp/pipewire.log /tmp/wireplumber.log; exit 1; }
    sleep 0.1
done
exec "$@"
