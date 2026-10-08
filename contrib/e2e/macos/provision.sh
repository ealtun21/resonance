#!/bin/bash
# One-time provisioning of the macOS e2e guest (run over ssh as the e2e user, which
# has passwordless sudo in the throwaway VM). Idempotent. Needs the Command Line
# Tools (`xcode-select --install`, done by hand during image creation) because the
# guest has no Homebrew (it dropped Intel support): BlackHole comes from the vendor pkgs.
set -euo pipefail
SRC="${SRC:-$HOME/resonance}"
BH_VERSION=0.7.0

for ch in 2ch 16ch 64ch; do
  if [ ! -d "/Library/Audio/Plug-Ins/HAL/BlackHole${ch}.driver" ]; then
    pkg="/tmp/BlackHole${ch}.pkg"
    curl -fsSL -o "$pkg" "https://existential.audio/downloads/BlackHole${ch}-${BH_VERSION}.pkg"
    sudo installer -pkg "$pkg" -target /
  fi
done
sudo killall coreaudiod || true

if ! command -v cargo >/dev/null && [ ! -x "$HOME/.cargo/bin/cargo" ]; then
  curl -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain "$(sed -n 's/^channel = "\(.*\)"/\1/p' "$SRC/rust-toolchain.toml")" --profile minimal
fi

# CoreAudio helper (device list, default output, nominal rate).
mkdir -p "$HOME/e2e-bin"
cc "$SRC/contrib/e2e/macos/audiodev.c" -framework CoreAudio -framework CoreFoundation -o "$HOME/e2e-bin/audiodev"

# Stable local signing identity: the TCC grants (system audio capture for the daemon,
# microphone for the agent) are keyed to the certificate, so they survive rebuilds.
bash "$SRC/contrib/macos/make-signing-cert.sh"
# Auto-login: the agent and daemon only get audio and TCC in a logged-in GUI session
# (`sysadminctl -autologin` fails on this guest, so write /etc/kcpassword directly:
# the password XORed with Apple's fixed key, padded to a multiple of 12 bytes).
python3 - <<'PY' | sudo tee /etc/kcpassword >/dev/null
import sys
key = [0x7D, 0x89, 0x52, 0x23, 0xD2, 0xBC, 0xDD, 0xEA, 0xA3, 0xB9, 0x1F]
pw = list(b"e2etest") + [0]
pw += [0] * (-len(pw) % 12)
sys.stdout.buffer.write(bytes(b ^ key[i % len(key)] for i, b in enumerate(pw)))
PY
sudo chmod 600 /etc/kcpassword
sudo defaults write /Library/Preferences/com.apple.loginwindow autoLoginUser e2e
sudo pmset -a sleep 0 displaysleep 0 disksleep 0

echo PROVISION-DONE
