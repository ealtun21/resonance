#!/bin/bash
# Provision the macOS e2e VM: Homebrew, BlackHole, SwitchAudioSource, rust.
set -x
export NONINTERACTIVE=1
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
eval "$(/usr/local/bin/brew shellenv)"
brew install switchaudio-osx
brew install --cask blackhole-2ch blackhole-16ch
curl -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.99.0
echo PROVISION-DONE
