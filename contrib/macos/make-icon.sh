#!/usr/bin/env bash
# Regenerate contrib/macos/Resonance.icns from the brand SVG.
#
# Apple's .icns format embeds multiple resolutions (1×/2× for 16/32/128/
# 256/512 — ten PNGs total). Finder, Launchpad, Dock, and Cmd-Tab pick
# the one that matches the current display scale; without the full set
# the icon looks blurry on Retina.
#
# Requires: `iconutil` (built into macOS) and one of `rsvg-convert`
# (Homebrew: `brew install librsvg`) or `qlmanage` (built-in, slower
# fallback).
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
SVG="$REPO_DIR/contrib/io.github.ealtun21.Resonance.svg"
OUT="$REPO_DIR/contrib/macos/Resonance.icns"
ICONSET="$(mktemp -d)/Resonance.iconset"
trap 'rm -rf "$(dirname "$ICONSET")"' EXIT
mkdir -p "$ICONSET"

echo "Rendering iconset → $ICONSET"
for spec in \
    "16 16x16" "32 16x16@2x" \
    "32 32x32" "64 32x32@2x" \
    "128 128x128" "256 128x128@2x" \
    "256 256x256" "512 256x256@2x" \
    "512 512x512" "1024 512x512@2x"; do
    size=$(echo "$spec" | cut -d' ' -f1)
    name=$(echo "$spec" | cut -d' ' -f2)
    if command -v rsvg-convert >/dev/null; then
        rsvg-convert -w "$size" -h "$size" "$SVG" -o "$ICONSET/icon_$name.png"
    else
        # Fallback: qlmanage. Slower, lower fidelity at small sizes, but
        # always available on macOS.
        qlmanage -t -s "$size" -o "$ICONSET" "$SVG" >/dev/null 2>&1
        mv "$ICONSET/$(basename "$SVG").png" "$ICONSET/icon_$name.png"
    fi
done

echo "Packing → $OUT"
iconutil -c icns "$ICONSET" -o "$OUT"
echo "OK: $(file "$OUT")"
