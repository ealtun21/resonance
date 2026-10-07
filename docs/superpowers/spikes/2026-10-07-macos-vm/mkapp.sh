#!/bin/bash
# Minimal Resonance.app (daemon only) for the e2e VM. Mirrors build-app.sh's signing.
set -euo pipefail
R=$HOME/resonance
APP=/Applications/Resonance.app
sudo rm -rf "$APP"
sudo mkdir -p "$APP/Contents/MacOS"
sudo chown -R "$USER" "$APP"
VERSION="$(grep -m1 '^version = ' "$R/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')"
sed -e "s/__VERSION__/$VERSION/g" -e 's|<string>resonance-gui</string>|<string>resonanced</string>|' \
    "$R/contrib/macos/Info.plist" > "$APP/Contents/Info.plist"
cp "$R/target/release/resonanced" "$APP/Contents/MacOS/resonanced"
codesign --force --sign - --identifier resonanced --entitlements "$R/contrib/macos/entitlements.plist" "$APP/Contents/MacOS/resonanced"
codesign --force --sign - --identifier com.ealtun21.resonance --entitlements "$R/contrib/macos/entitlements.plist" "$APP"
codesign -dv "$APP" 2>&1 | head -4
echo APP-OK
