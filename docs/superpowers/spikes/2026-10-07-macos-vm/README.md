# macOS VM spike (2026-10-07), reproducible pieces

Host: quickemu `quickget macos sequoia` (macOS 15.8.1, Intel, boots on this AMD host). The disk is non-empty
after the first install, so quickemu stops attaching the recovery image; pass
`--extra_args "-qmp unix:/tmp/mac-qmp.sock,server,nowait -device ich9-ahci,id=sata9 -drive id=Rec9,if=none,format=raw,file=macos-sequoia/RecoveryImage.img -device ide-hd,bus=sata9.0,drive=Rec9"`.

Driving the GUI without a person: `qtype.py` sends HMP `sendkey` events (`mq` wraps it for the monitor socket),
`qmpclick.py` sends absolute mouse clicks through QMP `input-send-event` (HMP `mouse_move` does not move the
pointer), `macshot.sh` takes screenshots with `screendump`. In Recovery, Tab cycles the list and Space activates.

Install order: Recovery Terminal `diskutil eraseDisk APFS "Macintosh HD" GPT disk0`; Reinstall macOS (three
reboots, each time pick the "macOS Installer" entry, finally "Macintosh HD", in the OpenCore picker with
`system_reset` + arrow keys); Setup Assistant (user `e2e`, password `e2etest`); Terminal:
`sudo launchctl load -w /System/Library/LaunchDaemons/ssh.plist` and an `authorized_keys` entry; passwordless
sudo for `e2e` (VM only); `xcode-select --install` (click Install + Agree); `macprov.sh` (Homebrew no longer
supports Intel, so BlackHole comes from the existential.audio `.pkg` files and the output device is switched
with `setdefout.c`); build with cargo; `mkapp.sh` makes a minimal signed `/Applications/Resonance.app`;
`spike.plist` is the LaunchAgent that starts the daemon, plays sounds and logs `HAL tap IOProc`.

First launch shows two prompts (microphone, then "Record Your System Audio"); clicking Allow on both
(QMP click) is the whole pre-grant. coreaudiod blocks the daemon's `AudioDeviceCreateIOProcID` until the
second prompt is answered, which is the hang seen on the GitHub-hosted runner.
