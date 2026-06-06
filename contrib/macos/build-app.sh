#!/usr/bin/env bash
# Build Resonance.app — a minimal macOS app bundle that wraps the
# resonanced binary so it can request the System Audio Recording / Audio
# Capture TCC permission. Without an app bundle (Info.plist + bundle
# identifier), macOS silently returns zero-filled buffers from the Process
# Tap API instead of issuing a permission prompt.
#
# Usage:
#   contrib/macos/build-app.sh                # release build, output to ./Resonance.app
#   APP_OUT=~/Applications contrib/macos/build-app.sh
#   SIGN_IDENTITY="Developer ID Application: …" contrib/macos/build-app.sh
#
# With SIGN_IDENTITY unset we use an ad-hoc signature ("-"), which is
# accepted by Gatekeeper for locally-built apps and is enough to let
# macOS register a TCC entry for the bundle.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
APP_OUT="${APP_OUT:-$REPO_DIR/Resonance.app}"
SIGN_IDENTITY="${SIGN_IDENTITY:--}"

echo ">> building release binaries (daemon + GUI + TUI + CLI)"
# All four binaries land in the bundle — the GUI is the user-facing
# entry point, the CLI + TUI are symlinked into ~/.local/bin by
# install.sh, and the daemon is what launchd spawns. Building them all
# together is faster than -p per crate (single cargo run, shared deps).
(cd "$REPO_DIR" && cargo build --release \
    -p resonance-daemon \
    -p resonance-gui \
    -p resonance-tui \
    -p resonance-cli)

BIN="$REPO_DIR/target/release/resonanced"
GUI="$REPO_DIR/target/release/resonance-gui"
for b in "$BIN" "$GUI" \
    "$REPO_DIR/target/release/resonance" \
    "$REPO_DIR/target/release/resonance-tui"; do
    [[ -x "$b" ]] || { echo "missing $b — build failed?" >&2; exit 1; }
done

echo ">> assembling app bundle at $APP_OUT"
rm -rf "$APP_OUT"
mkdir -p "$APP_OUT/Contents/MacOS"
mkdir -p "$APP_OUT/Contents/Resources"

cp "$REPO_DIR/contrib/macos/Info.plist" "$APP_OUT/Contents/Info.plist"
# Copy (don't symlink) so the bundle is self-contained even if the workspace
# is moved or rebuilt.
cp "$BIN" "$APP_OUT/Contents/MacOS/resonanced"

# App icon. Render the .icns fresh from our pure-Rust rasteriser
# (`resonance-gui --dump-iconset`) — gives sharp output at every macOS
# size (16…1024 with 1×/2× variants) instead of qlmanage's behaviour of
# embedding a small SVG in the top-left of a larger canvas.
ICONSET_DIR="$(mktemp -d)/Resonance.iconset"
mkdir -p "$APP_OUT/Contents/Resources"
GUI_BIN="$REPO_DIR/target/release/resonance-gui"
if [[ -x "$GUI_BIN" ]]; then
    "$GUI_BIN" --dump-iconset "$ICONSET_DIR"
    iconutil -c icns "$ICONSET_DIR" -o "$APP_OUT/Contents/Resources/Resonance.icns"
    rm -rf "$(dirname "$ICONSET_DIR")"
    # Mirror the icns back into contrib so distros / packagers can ship
    # it without running the full build.
    cp "$APP_OUT/Contents/Resources/Resonance.icns" \
        "$REPO_DIR/contrib/macos/Resonance.icns"
else
    # Fall back to pre-baked icns if the GUI binary isn't built yet.
    ICON_SRC="$REPO_DIR/contrib/macos/Resonance.icns"
    if [[ -f "$ICON_SRC" ]]; then
        cp "$ICON_SRC" "$APP_OUT/Contents/Resources/Resonance.icns"
    else
        echo "warn: no icon source — bundle will use the default white icon" >&2
    fi
fi

# Co-locate the CLI/TUI/GUI binaries inside the bundle so a single bundle
# install lays down the whole toolset. They aren't needed for TCC but make
# the bundle self-contained for distribution.
for extra in resonance resonance-tui resonance-gui; do
    src="$REPO_DIR/target/release/$extra"
    if [[ -x "$src" ]]; then
        cp "$src" "$APP_OUT/Contents/MacOS/$extra"
    fi
done

# Sign every Mach-O in the bundle. ad-hoc ("-") is enough for a local
# install — for distribution use a Developer ID identity.
#
# CRITICAL: the GUI (the bundle's main executable) needs the same TCC
# entitlements as the daemon, because TCC attributes a spawned child
# process's permissions to its "responsible process" (the GUI). Without
# entitlements on the GUI, the daemon's `AudioHardwareCreateProcessTap`
# call gets silently denied — we hit exactly that bug before.
ENT="$REPO_DIR/contrib/macos/entitlements.plist"
echo ">> codesigning with identity: $SIGN_IDENTITY (entitlements: $ENT)"
# Sign the embedded TUI + CLI (they don't need TCC entitlements, but must
# be signed so the bundle's seal is valid).
for extra in resonance resonance-tui; do
    bin="$APP_OUT/Contents/MacOS/$extra"
    [[ -x "$bin" ]] && codesign --force --sign "$SIGN_IDENTITY" "$bin"
done
# Daemon AND GUI both get entitlements so TCC sees the same identity
# whether the user launches via CLI, GUI, or the daemon-only path.
codesign --force --sign "$SIGN_IDENTITY" --entitlements "$ENT" \
    "$APP_OUT/Contents/MacOS/resonanced"
codesign --force --sign "$SIGN_IDENTITY" --entitlements "$ENT" \
    "$APP_OUT/Contents/MacOS/resonance-gui"
codesign --force --sign "$SIGN_IDENTITY" --entitlements "$ENT" "$APP_OUT"

# ── Install CLI symlinks so `resonance` + `resonance-tui` work in the
#    terminal without typing the bundle path each time. We prefer
#    ~/.local/bin (no sudo needed) and create it if missing.
CLI_DIR="${CLI_DIR:-$HOME/.local/bin}"
mkdir -p "$CLI_DIR"
for c in resonance resonance-tui; do
    ln -sf "$APP_OUT/Contents/MacOS/$c" "$CLI_DIR/$c"
done
echo ">> CLI symlinks installed in $CLI_DIR (add to PATH if missing):"
echo "     $CLI_DIR/resonance"
echo "     $CLI_DIR/resonance-tui"

echo
echo "Built $APP_OUT"
echo
echo "Install + run:"
echo "  1. Move the bundle to ~/Applications (or /Applications):"
echo "       cp -R '$APP_OUT' ~/Applications/"
echo "  2. Open it from Launchpad or Spotlight ('Resonance')."
echo "     The GUI auto-spawns the daemon and brings up the EQ window."
echo "  3. macOS will prompt for Audio Recording permission — approve."
echo "  4. Audio from every running app flows through the DSP chain."
echo
echo "Terminal usage (after PATH = \$HOME/.local/bin:\$PATH):"
echo "     resonance status"
echo "     resonance-tui"
