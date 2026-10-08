#!/bin/bash
# Per-run preparation inside the macOS guest: build the daemon and the agent, wrap the
# daemon in a minimal signed Resonance.app, sign the agent with the same identity,
# install the two LaunchAgents. Log: ~/prepare.log. Prints PREPARE-DONE last.
set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
SRC="${SRC:-$HOME/resonance}"
IDENTITY="Resonance Local Signing"
KEYCHAIN="$HOME/Library/Keychains/resonance-signing.keychain-db"
cd "$SRC"

cargo build --profile e2e-build -p resonance-daemon -p resonance-e2e

# The signing keychain is locked outside a GUI login; we own its password.
security unlock-keychain -p resonance "$KEYCHAIN"
security list-keychains -d user -s "$KEYCHAIN" "$HOME/Library/Keychains/login.keychain-db" || true

APP=/Applications/Resonance.app
sudo rm -rf "$APP"
sudo mkdir -p "$APP/Contents/MacOS"
sudo chown -R "$USER" "$APP"
VERSION="$(grep -m1 '^version = ' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
sed -e "s/__VERSION__/$VERSION/g" -e 's|<string>resonance-gui</string>|<string>resonanced</string>|' \
  contrib/macos/Info.plist > "$APP/Contents/Info.plist"
cp target/e2e-build/resonanced "$APP/Contents/MacOS/resonanced"
codesign --force --sign "$IDENTITY" --identifier resonanced --entitlements contrib/macos/entitlements.plist "$APP/Contents/MacOS/resonanced"
codesign --force --sign "$IDENTITY" --identifier com.ealtun21.resonance --entitlements contrib/macos/entitlements.plist "$APP"

# The agent records BlackHole through CoreAudio input, which needs the microphone grant.
mkdir -p "$HOME/e2e-bin"
cp target/e2e-build/resonance-e2e "$HOME/e2e-bin/resonance-e2e"
codesign --force --sign "$IDENTITY" --identifier com.resonance.e2e "$HOME/e2e-bin/resonance-e2e"

mkdir -p "$HOME/Library/LaunchAgents"
for f in daemon agent; do
  sed "s|@HOME@|$HOME|g" "contrib/e2e/macos/$f.plist" > "$HOME/Library/LaunchAgents/e2e.$f.plist"
done
echo PREPARE-DONE
