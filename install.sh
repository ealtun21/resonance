#!/usr/bin/env bash
# Resonance installer.
#
# It never compiles unless it must:
#   - On Arch it installs a pacman-tracked package of the *prebuilt* binaries
#     (clean uninstall + dependency handling, no compile) via the resonance-eq-bin
#     PKGBUILD.
#   - On macOS it downloads Resonance.app and copies it to /Applications
#     (with sudo if available) or ~/Applications.
#   - Elsewhere it drops the prebuilt Linux binaries straight into PREFIX/bin.
# Source builds happen only when you ask for them (PACMAN_BUILD=1 / FROM_SOURCE=1)
# or when there is no prebuilt for your architecture.
#
#   # 1. curl | bash  (no checkout)
#   curl -fsSL https://raw.githubusercontent.com/ealtun21/resonance/master/install.sh | bash
#       Linux/Arch:     pacman-tracked prebuilt package (no compile) via makepkg
#       Linux/other:    download the prebuilt binary tarball into PREFIX/bin
#       macOS:          download Resonance.app, install + xattr-clear + symlink CLI
#       PACMAN_BUILD=1: on Arch, download the tagged source and `makepkg -si` (compiles)
#       NO_PACMAN=1:    skip pacman; install plain (untracked) prebuilt binaries
#       FROM_SOURCE=1:  build from source instead of using prebuilts
#
#   # 2. inside a git checkout: builds from source
#   ./install.sh            # Arch -> makepkg -si; macOS -> .app bundle; else cargo build + install
#
# Env overrides:
#   RESONANCE_VERSION=v0.3.0   pin a release tag (default: latest)
#   PREFIX=/usr/local          install prefix (default: /usr/local, or ~/.local if unprivileged)
#   FROM_SOURCE=1              force source build even inside a checkout
#   PACMAN_BUILD=1             on Arch, build a pacman-tracked package from source
#   NO_PACMAN=1                on Arch, skip pacman and use the plain prebuilt binaries
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

# True if the filesystem carrying /usr is read-only (immutable distros:
# SteamOS, Fedora Silverblue, etc.). There /usr cannot be written even with
# sudo, and disabling the lock is reverted on the next OS update — so a home
# install is the only thing that survives.
rootfs_readonly() {
    if command -v steamos-readonly >/dev/null 2>&1; then
        steamos-readonly status 2>/dev/null | grep -qiw enabled && return 0
    fi
    # Field 4 of /proc/mounts is the comma-separated option list. Match `ro`
    # only as a whole option — `grep -w ro` also matches the `ro` inside the
    # common `errors=remount-ro` (Debian/Ubuntu ext4), which is NOT a read-only
    # mount and would wrongly force a home install on ordinary systems.
    awk '$2=="/" || $2=="/usr" { print $4 }' /proc/mounts 2>/dev/null \
        | grep -qE '(^|,)ro(,|$)' && return 0
    return 1
}

pick_prefix() {
    if [[ -n "${PREFIX:-}" ]]; then echo "$PREFIX"; return; fi
    # On an immutable rootfs, install into the always-writable home prefix.
    if rootfs_readonly; then echo "$HOME/.local"; return; fi
    if [[ $EUID -eq 0 || -w /usr/local/bin ]] || command -v sudo >/dev/null 2>&1; then
        echo /usr/local
    else
        echo "$HOME/.local"
    fi
}

is_macos() {
    [[ "$(uname -s)" == "Darwin" ]]
}

APPID="io.github.ealtun21.Resonance"

# Install the GUI desktop entry + icon + AppStream metadata so it shows up in
# the application menu. $1 = directory containing the contrib/ files (skipped if
# missing). $2 = prefix, $3 = sudo prefix ("" or "sudo").
#
# These are XDG application-launcher conventions — meaningless on macOS, where
# apps live in `.app` bundles. We skip them on Darwin.
install_desktop() {
    local contrib="$1" prefix="$2" sudo="$3"
    is_macos && return 0
    [[ -f "$contrib/$APPID.desktop" ]] || return 0
    info "Installing desktop entry + icon for the application menu"
    local desktop="$prefix/share/applications/$APPID.desktop"
    $sudo install -Dm644 "$contrib/$APPID.desktop" "$desktop"
    # Pin Exec to the absolute binary path: a home-prefix install ($HOME/.local
    # on an immutable rootfs) often isn't on the desktop session's PATH, so a
    # bare `resonance-gui` would fail to launch from the application menu.
    $sudo sed -i "s|^Exec=.*|Exec=$prefix/bin/resonance-gui|" "$desktop"
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

# -------- package-manager path: prebuilt -bin via makepkg (no checkout) -------
# Fetches the resonance-eq-bin PKGBUILD from the tag and builds a pacman-tracked
# package that installs the *prebuilt* release binaries — no compilation. This is
# the default on Arch. Returns non-zero so the caller can fall back.
install_arch_bin_remote() {
    command -v makepkg >/dev/null 2>&1 || return 1
    command -v curl    >/dev/null 2>&1 || return 1
    [[ $EUID -ne 0 ]] || { info "makepkg refuses root; skipping pacman path"; return 1; }

    local ver; ver="$(resolve_version)" || return 1
    local tmp; tmp="$(mktemp -d)"; _CLEANUP_DIR="$tmp"
    info "Fetching resonance-eq-bin PKGBUILD ($ver) for a pacman-tracked, no-compile install"
    # The release ships a PKGBUILD whose sha256sums match that release's tarball
    # exactly (stamped by CI). Prefer it; fall back to the tag tree for older
    # releases that didn't publish one.
    curl -fSL --proto '=https' --tlsv1.2 -o "$tmp/PKGBUILD" \
        "https://github.com/$REPO/releases/download/${ver}/PKGBUILD" \
        || curl -fSL --proto '=https' --tlsv1.2 -o "$tmp/PKGBUILD" \
            "https://raw.githubusercontent.com/$REPO/${ver}/contrib/aur-bin/PKGBUILD" \
        || return 1
    [[ -s "$tmp/PKGBUILD" ]] || return 1
    info "Building pacman package via makepkg (downloads prebuilt binaries, no compile)"
    ( cd "$tmp" && makepkg -si --noconfirm ) || return 1
}

# -------- macOS prebuilt path: download Resonance.app from the release --
# Returns non-zero on failure so the caller can fall back to a source
# build. Installs to /Applications when sudo is available (system-wide,
# Launchpad + Spotlight + Cmd-Tab visible) else ~/Applications.
install_macos_prebuilt() {
    command -v curl  >/dev/null 2>&1 || return 1
    command -v tar   >/dev/null 2>&1 || return 1
    command -v xattr >/dev/null 2>&1 || return 1
    local arch; arch="$(uname -m)"
    case "$arch" in
        arm64|x86_64) ;;
        *) info "no macOS prebuilt for arch '$arch'"; return 1 ;;
    esac

    local ver; ver="$(resolve_version)" || return 1
    local v="${ver#v}"
    local name="resonance-${v}-${arch}-macos"
    local base="https://github.com/$REPO/releases/download/${ver}"

    local tmp; tmp="$(mktemp -d)"; _CLEANUP_DIR="$tmp"
    info "Downloading $name.tar.gz ($ver)"
    if ! curl -fSL --proto '=https' --tlsv1.2 -o "$tmp/$name.tar.gz" \
            "$base/$name.tar.gz" 2>/dev/null; then
        info "macOS prebuilt not published for $ver (falling back to source)"
        return 1
    fi

    if curl -fsSL -o "$tmp/$name.tar.gz.sha256" "$base/$name.tar.gz.sha256" 2>/dev/null; then
        info "Verifying sha256"
        ( cd "$tmp" && shasum -a 256 -c "$name.tar.gz.sha256" >/dev/null ) \
            || err "checksum verification failed"
    fi

    tar -C "$tmp" -xzf "$tmp/$name.tar.gz"
    local app; app="$(find "$tmp/$name" -maxdepth 2 -name 'Resonance.app' | head -1)"
    [[ -d "$app" ]] || err "tarball did not contain Resonance.app"

    install_macos_app_bundle "$app"
}

# Build + install Resonance.app from a source checkout. Used when prebuilt
# isn't available (FROM_SOURCE=1, or no release tarball yet).
install_macos_from_source() {
    local dir="$1"
    cd "$dir"
    command -v cargo >/dev/null 2>&1 || err "cargo not found. Install Rust: https://rustup.rs"
    info "Building Resonance.app from source via contrib/macos/build-app.sh"
    contrib/macos/build-app.sh
    install_macos_app_bundle "$dir/Resonance.app"
}

# Copy Resonance.app into /Applications (sudo path) or ~/Applications
# (no-sudo fallback), strip the quarantine xattr Gatekeeper would
# otherwise add, register with LaunchServices, and install CLI symlinks
# into ~/.local/bin.
install_macos_app_bundle() {
    local src="$1"
    local dest_root
    if [[ $EUID -eq 0 ]]; then
        dest_root="/Applications"
    elif command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
        dest_root="/Applications"
    else
        dest_root="$HOME/Applications"
    fi
    mkdir -p "$dest_root" 2>/dev/null || true
    local dest="$dest_root/Resonance.app"
    local sudo=""
    if [[ "$dest_root" == "/Applications" && $EUID -ne 0 ]]; then
        sudo="sudo"
    fi
    info "Installing to $dest${sudo:+ (via sudo)}"
    $sudo rm -rf "$dest"
    $sudo cp -R "$src" "$dest"
    # Strip the "downloaded from internet" quarantine xattr so the user
    # isn't blocked by Gatekeeper on first launch.
    $sudo xattr -dr com.apple.quarantine "$dest" 2>/dev/null || true

    # Register with Launch Services so Spotlight + Launchpad index it now.
    local lsreg=/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister
    [[ -x "$lsreg" ]] && "$lsreg" -f "$dest" >/dev/null 2>&1 || true

    # CLI symlinks so `resonance` + `resonance-tui` work in the terminal.
    local cli_dir="$HOME/.local/bin"
    mkdir -p "$cli_dir"
    for c in resonance resonance-tui; do
        ln -sf "$dest/Contents/MacOS/$c" "$cli_dir/$c"
    done

    info "Done. Launch Resonance from Spotlight / Launchpad."
    info "CLI: ensure $cli_dir is on PATH then run 'resonance status'."
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
    # macOS: build the .app bundle, not a raw binary install.
    if is_macos; then
        install_macos_from_source "$dir"
        return
    fi
    # `makepkg` is Arch-only.
    if [[ "${FROM_SOURCE:-0}" != "1" ]] \
        && command -v makepkg >/dev/null 2>&1 && [[ -f PKGBUILD ]]; then
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

    # macOS: prefer the prebuilt Resonance.app bundle from the release.
    # Falls back to a source build if the tarball isn't published yet or
    # the user passes FROM_SOURCE=1.
    if is_macos; then
        if [[ "${FROM_SOURCE:-0}" != "1" ]] && install_macos_prebuilt; then
            return
        fi
        command -v git   >/dev/null 2>&1 || err "git required for source install on macOS"
        command -v cargo >/dev/null 2>&1 || err "cargo not found. Install Rust: https://rustup.rs"
        local tmp; tmp="$(mktemp -d)"; _CLEANUP_DIR="$tmp"
        local ver; ver="$(resolve_version)"
        info "Cloning $REPO@$ver for source build"
        git -C "$tmp" clone --depth 1 --branch "$ver" "https://github.com/$REPO" src \
            || err "git clone failed"
        install_macos_from_source "$tmp/src"
        return
    fi

    # No checkout (curl | bash). On Arch, default to the pacman-tracked prebuilt
    # package (clean uninstall + dep handling, *no compile*). PACMAN_BUILD=1
    # opts into compiling a pacman package from source; NO_PACMAN=1 forces the
    # plain (untracked) prebuilt binaries.
    if [[ "${FROM_SOURCE:-0}" != "1" && "${NO_PACMAN:-0}" != "1" ]] \
        && command -v pacman >/dev/null 2>&1; then
        if [[ "${PACMAN_BUILD:-0}" == "1" ]]; then
            info "PACMAN_BUILD=1 — building a pacman-tracked package from source via makepkg"
            if install_arch_pkg_remote; then return; fi
        else
            info "Arch detected — installing the pacman-tracked prebuilt package (no compile)"
            if install_arch_bin_remote; then return; fi
        fi
        info "Package-manager path unavailable; falling back to prebuilt binaries"
    fi

    # FROM_SOURCE=1 outside a checkout (and not handled by the Arch makepkg path
    # above): clone the tag and build with cargo, matching the documented flag
    # and the macOS source path. Without this, FROM_SOURCE was silently ignored
    # on non-Arch Linux and prebuilt binaries were installed anyway.
    if [[ "${FROM_SOURCE:-0}" == "1" ]]; then
        command -v git   >/dev/null 2>&1 || err "git required for source install"
        command -v cargo >/dev/null 2>&1 || err "cargo not found. Install Rust: https://rustup.rs"
        local tmp; tmp="$(mktemp -d)"; _CLEANUP_DIR="$tmp"
        local ver; ver="$(resolve_version)"
        info "FROM_SOURCE=1 — cloning $REPO@$ver for a source build"
        git -C "$tmp" clone --depth 1 --branch "$ver" "https://github.com/$REPO" src \
            || err "git clone failed"
        install_from_source "$tmp/src"
        return
    fi

    install_prebuilt
}

main "$@"
