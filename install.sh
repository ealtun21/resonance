#!/usr/bin/env bash
# Resonance installer.
#
#   ./install.sh
#
# On Arch-based systems this builds a real pacman package from the bundled
# PKGBUILD (AUR-style) and installs it with `makepkg -si`, so it can later be
# removed with `sudo pacman -R resonance`.
#
# On any other distro it falls back to `cargo build --release` and installs the
# binaries into PREFIX/bin (default /usr/local).
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFIX="${PREFIX:-/usr/local}"
BINS=(resonanced resonance resonance-tui resonance-gui)

cd "$REPO_DIR"

if command -v makepkg >/dev/null 2>&1; then
    echo ">> Arch detected — building pacman package via PKGBUILD"
    if [[ $EUID -eq 0 ]]; then
        echo "!! Do not run as root; makepkg refuses root. Re-run as a normal user." >&2
        exit 1
    fi
    exec makepkg -si --noconfirm
fi

echo ">> Non-Arch system — building from source with cargo"
if ! command -v cargo >/dev/null 2>&1; then
    echo "!! cargo not found. Install Rust: https://rustup.rs" >&2
    exit 1
fi

cargo build --release --locked --all

SUDO=""
if [[ ! -w "$PREFIX/bin" ]]; then
    SUDO="sudo"
fi

echo ">> Installing binaries into $PREFIX/bin (using: ${SUDO:-none})"
for b in "${BINS[@]}"; do
    $SUDO install -Dm755 "target/release/$b" "$PREFIX/bin/$b"
done
$SUDO install -Dm644 LICENSE "$PREFIX/share/licenses/resonance/LICENSE"

echo ">> Done. Start the daemon with: RUST_LOG=info resonanced"
