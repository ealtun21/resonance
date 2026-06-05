#!/usr/bin/env bash
# Resonance installer — works two ways:
#
#   # 1. curl | bash  (no checkout): downloads the latest prebuilt release
#   curl -fsSL https://raw.githubusercontent.com/ealtun21/resonance/master/install.sh | bash
#
#   # 2. inside a git checkout: builds from source
#   ./install.sh            # Arch -> makepkg -si, else cargo build + install
#
# Env overrides:
#   RESONANCE_VERSION=v0.1.0   pin a release tag (default: latest)
#   PREFIX=/usr/local          install prefix (default: /usr/local, or ~/.local if unprivileged)
#   FROM_SOURCE=1              force source build even inside a checkout
set -euo pipefail

REPO="ealtun21/resonance"
BINS=(resonanced resonance resonance-tui resonance-gui)

_CLEANUP_DIR=""
trap '[[ -n "${_CLEANUP_DIR:-}" ]] && rm -rf "$_CLEANUP_DIR"' EXIT

err()  { echo "!! $*" >&2; exit 1; }
info() { echo ">> $*"; }

# Resolve the directory this script lives in, if it is a real file (not piped).
script_dir() {
    local src="${BASH_SOURCE[0]:-}"
    [[ -n "$src" && -f "$src" ]] || return 1
    cd "$(dirname "$src")" >/dev/null 2>&1 && pwd
}

pick_prefix() {
    if [[ -n "${PREFIX:-}" ]]; then echo "$PREFIX"; return; fi
    if [[ $EUID -eq 0 || -w /usr/local/bin ]] || command -v sudo >/dev/null 2>&1; then
        echo /usr/local
    else
        echo "$HOME/.local"
    fi
}

# Install the given files from a directory into PREFIX/bin (+ license).
install_tree() {
    local srcdir="$1" prefix bindir sudo=""
    prefix="$(pick_prefix)"
    bindir="$prefix/bin"
    if [[ ! -w "$bindir" && ! -w "$prefix" && $EUID -ne 0 ]]; then
        command -v sudo >/dev/null 2>&1 && sudo="sudo" \
            || err "Cannot write $bindir and sudo not found. Set PREFIX to a writable path."
    fi
    info "Installing into $bindir${sudo:+ (via sudo)}"
    for b in "${BINS[@]}"; do
        $sudo install -Dm755 "$srcdir/$b" "$bindir/$b"
    done
    [[ -f "$srcdir/LICENSE" ]] && \
        $sudo install -Dm644 "$srcdir/LICENSE" "$prefix/share/licenses/resonance/LICENSE" || true
    info "Done. Make sure $bindir is on your PATH, then run: RUST_LOG=info resonanced"
}

# -------- prebuilt path (curl | bash) --------
install_prebuilt() {
    command -v curl >/dev/null 2>&1 || err "curl required"
    command -v tar  >/dev/null 2>&1 || err "tar required"
    local arch; arch="$(uname -m)"
    [[ "$arch" == "x86_64" ]] || err "no prebuilt for arch '$arch'; build from source instead"

    local ver="${RESONANCE_VERSION:-}"
    if [[ -z "$ver" ]]; then
        info "Resolving latest release tag"
        ver="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
                 | grep -m1 '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
        [[ -n "$ver" ]] || err "could not determine latest tag; set RESONANCE_VERSION"
    fi
    local v="${ver#v}"
    local name="resonance-${v}-x86_64-linux"
    local base="https://github.com/$REPO/releases/download/${ver}"

    local tmp; tmp="$(mktemp -d)"; _CLEANUP_DIR="$tmp"
    info "Downloading $name.tar.gz ($ver)"
    curl -fSL --proto '=https' --tlsv1.2 -o "$tmp/$name.tar.gz" "$base/$name.tar.gz" \
        || err "download failed: $base/$name.tar.gz"

    if curl -fsSL -o "$tmp/$name.tar.gz.sha256" "$base/$name.tar.gz.sha256" 2>/dev/null; then
        info "Verifying sha256"
        ( cd "$tmp" && sha256sum -c "$name.tar.gz.sha256" >/dev/null ) \
            || err "checksum verification failed"
    else
        info "No checksum published; skipping verification"
    fi

    tar -C "$tmp" -xzf "$tmp/$name.tar.gz"
    install_tree "$tmp/$name"
}

# -------- source path (inside a checkout) --------
install_from_source() {
    local dir="$1"
    cd "$dir"
    if [[ "${FROM_SOURCE:-0}" != "1" ]] && command -v makepkg >/dev/null 2>&1 && [[ -f PKGBUILD ]]; then
        info "Arch detected — building pacman package via PKGBUILD"
        [[ $EUID -ne 0 ]] || err "do not run as root; makepkg refuses root"
        exec makepkg -si --noconfirm
    fi
    command -v cargo >/dev/null 2>&1 || err "cargo not found. Install Rust: https://rustup.rs"
    info "Building from source with cargo"
    cargo build --release --locked --all
    install_tree "target/release"
}

main() {
    local dir
    if dir="$(script_dir)" && [[ -f "$dir/Cargo.toml" || -f "$dir/PKGBUILD" ]]; then
        install_from_source "$dir"
    else
        install_prebuilt
    fi
}

main "$@"
