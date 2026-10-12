#!/bin/bash
# Guest side: product lifecycle matrix against a real install, judged by `resonance verify`
# (pitch + response of real tones through the live path) and the daemon's own meters.
#   lifecycle.sh BUNDLE [UPGRADE_BUNDLE]
# BUNDLE is a signed Resonance.app built by build-app.sh (TCC must already be granted to its
# signing identity, as in the e2e base image). UPGRADE_BUNDLE (a second build, same identity)
# adds the "replace under a running daemon" step. Needs BlackHole 2ch/16ch and the audiodev
# helper; exits non-zero on any failed check. ~3 minutes, run from `vmctl.sh script`.
set -u
BUNDLE=${1:?bundle}
UPGRADE=${2:-}
APP=/Applications/Resonance.app
export PATH=$HOME/.local/bin:$PATH
HERE=$(cd "$(dirname "$0")" && pwd)
GUI="bash $HERE/gui-run.sh"
AUDIODEV=${AUDIODEV:-$HOME/e2e-bin/audiodev}
LOG=$HOME/Library/Logs/resonance/resonanced.out.log
fail=0
ok() { echo "PASS  $1"; }
bad() { echo "FAIL  $1"; fail=1; }
check() { local what=$1; shift; if "$@" >/dev/null 2>&1; then ok "$what"; else bad "$what"; fi; }
now() { python3 -c 'import time;print(time.time())'; }
reach() { resonance status >/dev/null 2>&1; }
wait_reach() { for _ in $(seq 1 $((${1:-6} * 10))); do reach && return 0; sleep 0.1; done; return 1; }
level() { resonance status 2>/dev/null | sed -n 's/^levels  in \(.*\) dB  out.*/\1/p'; }
# the pilot loops in the GUI session until stopped
tone_on() {
  python3 - <<'E'
import math, struct, wave
w = wave.open('/tmp/lc-1k.wav', 'wb'); w.setnchannels(2); w.setsampwidth(2); w.setframerate(48000)
s = [int(8000 * math.sin(2 * math.pi * 1000 * i / 48000)) for i in range(48000 * 30)]
w.writeframes(b''.join(struct.pack('<hh', v, v) for v in s))
E
  launchctl bootout gui/$(id -u)/com.lc.tone 2>/dev/null
  cat >/tmp/lc-tone.plist <<E
<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>Label</key><string>com.lc.tone</string>
<key>ProgramArguments</key><array><string>/bin/bash</string><string>-c</string><string>while :; do /usr/bin/afplay /tmp/lc-1k.wav; done</string></array>
<key>RunAtLoad</key><true/></dict></plist>
E
  launchctl bootstrap gui/$(id -u) /tmp/lc-tone.plist
}
tone_off() { launchctl bootout gui/$(id -u)/com.lc.tone 2>/dev/null; }
audible() { # level above -60 dB within $1 seconds
  for _ in $(seq 1 $(($1 * 5))); do l=$(level); v=${l%%.*}; [ -n "$v" ] && [ "$v" != "-inf" ] && [ "$v" -gt -60 ] && return 0; sleep 0.2; done; return 1
}
verify() { $GUI -t 90 "resonance verify --freqs 100,1000,6000" >/tmp/lc-verify.out 2>&1; local rc=$?; tail -3 /tmp/lc-verify.out | sed 's/^/        /'; return $rc; }

echo "== install"
resonance daemon uninstall >/dev/null 2>&1; pkill -x resonance-tray 2>/dev/null
sudo rm -rf $APP && sudo cp -R "$BUNDLE" $APP && sudo chown -R "$USER" $APP
check "bundle signature is valid" codesign --verify --deep --strict $APP
mkdir -p ~/.local/bin; ln -sf $APP/Contents/MacOS/resonance ~/.local/bin/resonance
$AUDIODEV default "BlackHole 2ch"; $AUDIODEV rate "BlackHole 2ch" 48000 >/dev/null
resonance daemon enable >/dev/null
check "launchd agent up and serving within 6 s" wait_reach 6
check "plist is RunAtLoad and the agent is loaded" bash -c 'grep -A1 RunAtLoad ~/Library/LaunchAgents/com.ealtun21.resonanced.plist | grep -q true && launchctl print gui/$(id -u)/com.ealtun21.resonanced >/dev/null'
resonance output "BlackHole 16ch" >/dev/null; sleep 3
check "tap delivers audio: verify PASS" verify

echo "== crash recovery"
t0=$(now); kill -9 "$(pgrep -x resonanced)"; sleep 0.2
if wait_reach 6; then ok "kill -9: launchd restarted the daemon in $(python3 -c "print(round($(now)-$t0,2))") s"; else bad "kill -9: daemon did not come back"; fi
sleep 4
check "verify after the restart (tap + consent survive)" verify
resonance daemon stop >/dev/null; sleep 1
check "SIGTERM stop is final (KeepAlive honours a clean exit)" bash -c '! pgrep -x resonanced'
check "socket removed on clean stop" bash -c '! ls "$TMPDIR"/resonance.sock'
resonance daemon start >/dev/null; check "daemon start serves again" wait_reach 6

echo "== sample rate and device changes (pilot playing)"
resonance output "BlackHole 2ch" >/dev/null; sleep 2
tone_on; sleep 3
for r in 44100 96000 48000; do
  $AUDIODEV rate "BlackHole 2ch" $r >/dev/null; sleep 3
  check "rate $r: status follows" bash -c "resonance status | grep -q 'format  $r Hz'"
  check "rate $r: audio flows again" audible 8
done
resonance output "BlackHole 16ch" >/dev/null; sleep 2
$AUDIODEV default "Apple Virtual Sound Device"; sleep 2
tone_off; tone_on; sleep 4
check "pinned output, OS default moved: tap follows the default" audible 8
tone_off
$AUDIODEV default "BlackHole 2ch"; resonance output auto >/dev/null; sleep 3

echo "== upgrade over a running install"
if [ -n "$UPGRADE" ]; then
  tone_on; sleep 3; old=$(pgrep -x resonanced)
  sudo rm -rf $APP && sudo cp -R "$UPGRADE" $APP && sudo chown -R "$USER" $APP
  check "old daemon keeps serving while its bundle is replaced" bash -c "reach() { resonance status >/dev/null 2>&1; }; reach && kill -0 $old"
  resonance daemon restart >/dev/null
  check "restart picks up the new binary (pid changed)" bash -c "[ \"\$(pgrep -x resonanced)\" != \"$old\" ]"
  check "new build keeps the capture grant (audio within 8 s)" audible 8
  tone_off
else
  echo "SKIP  no upgrade bundle given"
fi

echo "== uninstall"
resonance daemon disable >/dev/null; resonance daemon uninstall >/dev/null
check "no resonanced process" bash -c '! pgrep -x resonanced'
check "plist removed" bash -c '! test -e ~/Library/LaunchAgents/com.ealtun21.resonanced.plist'
check "agent unloaded" bash -c '! launchctl print gui/$(id -u)/com.ealtun21.resonanced'
check "socket and pid file removed" bash -c '! ls "$TMPDIR"/resonance.sock "$TMPDIR"/resonanced.pid'
check "no leftover tap aggregate device" bash -c "! $AUDIODEV list | grep -i 'resonance'"
tone_off
echo; [ $fail = 0 ] && echo LIFECYCLE-PASS || echo LIFECYCLE-FAIL
exit $fail
