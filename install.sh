#!/usr/bin/env bash
# Resonance installer.
#
# It prefers the system package manager when one can produce a tracked package
# (currently Arch/pacman via the in-tree PKGBUILD — clean uninstall, automatic
# dependency handling). Otherwise it falls back to prebuilt release binaries
# (curl | bash) or a plain `cargo build` install (inside a checkout).
#
#   # 1. curl | bash  (no checkout)
#   curl -fsSL https://raw.githubusercontent.com/ealtun21/resonance/master/install.sh | bash
#       Arch:  download the tagged source and `makepkg -si` (pacman-tracked)
#       else:  download the prebuilt binary tarball into PREFIX/bin
#
#   # 2. inside a git checkout: builds from source
#   ./install.sh            # Arch -> makepkg -si, else cargo build + install
#
# Env overrides:
#   RESONANCE_VERSION=v0.2.0   pin a release tag (default: latest)
#   PREFIX=/usr/local          install prefix (default: /usr/local, or ~/.local if unprivileged)
#   FROM_SOURCE=1              force source build even inside a checkout
#   NO_PKG_MANAGER=1           skip the package-manager path; use binaries/cargo only
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

APPID="io.github.ealtun21.Resonance"

# Install the GUI desktop entry + icon + AppStream metadata so it shows up in
# the application menu. $1 = directory containing the contrib/ files (skipped if
# missing). $2 = prefix, $3 = sudo prefix ("" or "sudo").
install_desktop() {
    local contrib="$1" prefix="$2" sudo="$3"
    [[ -f "$contrib/$APPID.desktop" ]] || return 0
    info "Installing desktop entry + icon for the application menu"
    $sudo install -Dm644 "$contrib/$APPID.desktop" \
        "$prefix/share/applications/$APPID.desktop"
    [[ -f "$contrib/$APPID.svg" ]] && $sudo install -Dm644 "$contrib/$APPID.svg" \
        "$prefix/share/icons/hicolor/scalable/apps/$APPID.svg"
    [[ -f "$contrib/$APPID.metainfo.xml" ]] && $sudo install -Dm644 \
        "$contrib/$APPID.metainfo.xml" "$prefix/share/metainfo/$APPID.metainfo.xml"
    # Refresh the icon cache so the menu picks the icon up immediately.
    command -v gtk-update-icon-cache >/dev/null 2>&1 \
        && $sudo gtk-update-icon-cache -q "$prefix/share/icons/hicolor" 2>/dev/null || true
}

# Install binaries from $1 into PREFIX/bin (+ license). $2 = optional contrib dir
# for desktop integration.
install_tree() {
    local srcdir="$1" contrib="${2:-}" prefix bindir sudo=""
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
    [[ -n "$contrib" ]] && install_desktop "$contrib" "$prefix" "$sudo"
    info "Done. Make sure $bindir is on your PATH, then run: RUST_LOG=info resonanced"
}

# Resolve the release tag to use (RESONANCE_VERSION, or latest from the API).
resolve_version() {
    if [[ -n "${RESONANCE_VERSION:-}" ]]; then echo "$RESONANCE_VERSION"; return; fi
    command -v curl >/dev/null 2>&1 || err "curl required to resolve latest release"
    info "Resolving latest release tag" >&2
    # Buffer the API response first: piping curl straight into `grep -m1` makes
    # grep close the pipe early, so curl aborts its write with `curl: (23)`.
    local json ver
    json="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest")" \
        || err "could not reach GitHub API; set RESONANCE_VERSION"
    ver="$(printf '%s' "$json" | grep -m1 '"tag_name"' \
             | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
    [[ -n "$ver" ]] || err "could not determine latest tag; set RESONANCE_VERSION"
    echo "$ver"
}

# -------- package-manager path: Arch pacman via makepkg (no checkout) --------
# Downloads the tagged source (which carries the PKGBUILD) and builds a
# pacman-tracked package. Returns non-zero so the caller can fall back.
install_arch_pkg_remote() {
    command -v makepkg >/dev/null 2>&1 || return 1
    command -v curl    >/dev/null 2>&1 || return 1
    command -v tar     >/dev/null 2>&1 || return 1
    [[ $EUID -ne 0 ]] || { info "makepkg refuses root; skipping pacman path"; return 1; }

    local ver; ver="$(resolve_version)" || return 1
    local tmp; tmp="$(mktemp -d)"; _CLEANUP_DIR="$tmp"
    info "Downloading source $ver for a pacman-tracked build"
    curl -fSL --proto '=https' --tlsv1.2 -o "$tmp/src.tar.gz" \
        "https://github.com/$REPO/archive/refs/tags/${ver}.tar.gz" || return 1
    tar -C "$tmp" -xzf "$tmp/src.tar.gz" || return 1
    local srcdir; srcdir="$(find "$tmp" -maxdepth 1 -type d -name 'resonance-*' | head -1)"
    [[ -n "$srcdir" && -f "$srcdir/PKGBUILD" ]] || return 1
    info "Building pacman package via makepkg (installs deps automatically)"
    ( cd "$srcdir" && makepkg -si --noconfirm ) || return 1
}

# -------- prebuilt path (curl | bash) --------
install_prebuilt() {
    command -v curl >/dev/null 2>&1 || err "curl required"
    command -v tar  >/dev/null 2>&1 || err "tar required"
    local arch; arch="$(uname -m)"
    [[ "$arch" == "x86_64" ]] || err "no prebuilt for arch '$arch'; build from source instead"

    local ver; ver="$(resolve_version)"
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

    # Desktop integration is not in the binary tarball — fetch it from the tag so
    # the GUI appears in the application menu with its icon.
    local contrib="$tmp/contrib"
    mkdir -p "$contrib"
    local raw="https://raw.githubusercontent.com/$REPO/${ver}/contrib"
    for f in "$APPID.desktop" "$APPID.svg" "$APPID.metainfo.xml"; do
        curl -fsSL -o "$contrib/$f" "$raw/$f" 2>/dev/null || true
    done

    install_tree "$tmp/$name" "$contrib"
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
    install_tree "target/release" "$dir/contrib"
}

main() {
    local dir
    # Inside a checkout: source path already prefers makepkg on Arch.
    if dir="$(script_dir)" && [[ -f "$dir/Cargo.toml" || -f "$dir/PKGBUILD" ]]; then
        install_from_source "$dir"
        return
    fi

    # No checkout (curl | bash). Prefer a package-manager-tracked install first.
    if [[ "${NO_PKG_MANAGER:-0}" != "1" && "${FROM_SOURCE:-0}" != "1" ]] \
        && command -v pacman >/dev/null 2>&1; then
        info "Arch/pacman detected — trying a package-manager install"
        if install_arch_pkg_remote; then return; fi
        info "Package-manager path unavailable; falling back to prebuilt binaries"
    fi

    install_prebuilt
}

main "$@"
