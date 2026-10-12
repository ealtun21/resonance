#!/bin/bash
# Guest side: run a command line inside the logged-in GUI session (audio + TCC work there, not over
# ssh), through a throwaway LaunchAgent, and print its output and exit status.
#   gui-run.sh [-t SECS] 'command line'
# The process is a child of launchd in gui/<uid>, so TCC attributes it to itself (like the product's
# own daemon agent), never to sshd.
set -u
T=120
[ "${1:-}" = -t ] && { T=$2; shift 2; }
uid=$(id -u)
L=com.lc.exec
OUT=$HOME/.lc-exec.out
RC=$HOME/.lc-exec.rc
rm -f "$OUT" "$RC"
printf '#!/bin/bash\nexport PATH=$HOME/.local/bin:$HOME/.cargo/bin:$PATH\n%s\necho $? > %s\n' "$1" "$RC" >"$HOME/.lc-exec.sh"
cat >"$HOME/.lc-exec.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>$L</string>
<key>ProgramArguments</key><array><string>/bin/bash</string><string>$HOME/.lc-exec.sh</string></array>
<key>StandardOutPath</key><string>$OUT</string><key>StandardErrorPath</key><string>$OUT</string>
<key>RunAtLoad</key><false/></dict></plist>
EOF
launchctl bootout "gui/$uid/$L" 2>/dev/null
launchctl bootstrap "gui/$uid" "$HOME/.lc-exec.plist" || exit 99
launchctl kickstart "gui/$uid/$L" || exit 98
end=$((SECONDS + T))
while [ ! -f "$RC" ] && [ $SECONDS -lt $end ]; do sleep 1; done
launchctl bootout "gui/$uid/$L" 2>/dev/null
cat "$OUT" 2>/dev/null
[ -f "$RC" ] || { echo "gui-run: timed out after ${T}s" >&2; exit 124; }
exit "$(cat "$RC")"
