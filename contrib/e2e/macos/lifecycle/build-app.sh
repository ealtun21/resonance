#!/bin/bash
# Guest side: release-build the full app bundle with the product's own build-app.sh, signed with the
# image's stable identity, into ~/build/Resonance.app (nothing is installed yet). Log: ~/build-app.log.
set -eu
export PATH="$HOME/.cargo/bin:$PATH"
K=$HOME/Library/Keychains/resonance-signing.keychain-db
security unlock-keychain -p resonance "$K"
security list-keychains -d user -s "$K" "$HOME/Library/Keychains/login.keychain-db"
cd "$HOME/resonance"
APP_OUT="$HOME/build/Resonance.app" INSTALL_PATH=/Applications/Resonance.app CLI_DIR="$HOME/.local/bin" \
  bash contrib/macos/build-app.sh
echo BUILD-DONE
