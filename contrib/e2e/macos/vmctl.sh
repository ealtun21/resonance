#!/bin/bash
# Drive a macOS e2e guest from the host: boot an overlay, run ssh, take screenshots, type keys, click.
# Everything lives in $LC_DIR (default ~/.cache/resonance-e2e/macos/lifecycle); the base image is only
# ever a backing file. Usage: vmctl.sh reset|boot|ssh|script|scp|shot|key|type|click|sync|lifecycle|kill (see README.md)
set -eu
H=${RESONANCE_E2E_HOME:-$HOME/.cache/resonance-e2e}
LC_DIR=${LC_DIR:-$H/macos/lifecycle}
PORT=${LC_PORT:-22722}
KEY=${RESONANCE_E2E_SSH_KEY:-$H/ssh/id_ed25519}
SSH_OPTS=(-i "$KEY" -p "$PORT" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=8)
HERE=$(cd "$(dirname "$0")" && pwd)
QMP=$LC_DIR/qmp.sock

reset() { # fresh overlay of the base image (discards all guest changes)
  B=$H/macos/base
  "$0" kill || true
  rm -rf "$LC_DIR/vm" && mkdir -p "$LC_DIR/vm"
  qemu-img create -q -f qcow2 -F qcow2 -b "$B/disk.qcow2" "$LC_DIR/vm/disk.qcow2"
  for f in OpenCore.qcow2 OVMF_CODE.fd OVMF_VARS-1920x1080.fd; do cp "$B/$f" "$LC_DIR/vm/"; done
  truncate -s 1M "$LC_DIR/vm/RecoveryImage.img"
  printf '#!/usr/bin/quickemu --vm\nguest_os="macos"\ndisk_img="vm/disk.qcow2"\nimg="vm/RecoveryImage.img"\nmacos_release="sequoia"\ncpu_cores="16"\nram="10G"\n' >"$LC_DIR/vm.conf"
}

case ${1:-} in
reset) reset ;;
boot) # headless, with a QMP socket for input events and screendump
  cd "$LC_DIR"
  quickemu --vm vm.conf --display none --ssh-port "$PORT" --extra_args "-qmp unix:$QMP,server,nowait" >boot.log 2>&1
  tail -3 boot.log ;;
ssh) shift; exec ssh "${SSH_OPTS[@]}" e2e@127.0.0.1 "$@" ;;
script) exec ssh "${SSH_OPTS[@]}" e2e@127.0.0.1 'PATH=$HOME/.local/bin:$HOME/.cargo/bin:$PATH bash -s' ;; # script on stdin
scp) shift; exec scp -q -i "$KEY" -P "$PORT" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR "$@" ;;
shot) # shot [out.png]: QMP screendump -> png (full 1920x1080 unless LC_SCALE is set)
  out=${2:-$LC_DIR/shot.png}
  python3 -I "$HERE/vmgui.py" "$QMP" shot "$out" ;;
key) shift; python3 -I "$HERE/vmgui.py" "$QMP" key "$@" ;;
type) shift; python3 -I "$HERE/vmgui.py" "$QMP" type "$*" ;;
click) shift; python3 -I "$HERE/vmgui.py" "$QMP" click "$@" ;;
sync) # copy the working tree (tracked + untracked, not ignored) into ~/resonance in the guest
  root=$(cd "$HERE/../../.." && pwd)
  "$0" ssh 'mkdir -p $HOME/resonance'
  (cd "$root" && git ls-files -z -c -o --exclude-standard | tar --null -T - -cf -) |
    "$0" ssh 'tar -xmf - -C $HOME/resonance' ;;
lifecycle) # sync + release-build the .app + run the lifecycle matrix (about 15 min on a cold build)
  "$0" sync
  "$0" ssh 'rm -rf ~/build-prev; [ -d ~/build/Resonance.app ] && mv ~/build ~/build-prev; true'
  "$0" ssh 'bash ~/resonance/contrib/e2e/macos/lifecycle/build-app.sh > ~/build-app.log 2>&1; tail -1 ~/build-app.log'
  # install the previous build first when there is one, so the upgrade step really upgrades
  "$0" ssh 'bash ~/resonance/contrib/e2e/macos/lifecycle/lifecycle.sh "$( [ -d ~/build-prev ] && echo ~/build-prev || echo ~/build )/Resonance.app" ~/build/Resonance.app' ;;
kill) # stop qemu
  if [ -f "$LC_DIR/vm/vm.pid" ]; then kill "$(cat "$LC_DIR/vm/vm.pid")" 2>/dev/null || true; sleep 2; fi ;;
*) echo "usage: $0 reset|boot|ssh|script|scp|shot|key|type|click|sync|lifecycle|kill" >&2; exit 2 ;;
esac
