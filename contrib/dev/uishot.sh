#!/usr/bin/env bash
# uishot.sh — screenshot the resonance-gui at one or more window sizes under a
# headless Xvfb display, so the UI can be eyeballed at arbitrary widths without
# touching the user's live session.
#
# Usage:
#   contrib/dev/uishot.sh [--bin PATH] [--out DIR] [--wait SECS] WxH [WxH ...]
#   contrib/dev/uishot.sh                       # default size set
#
# Each WxH (e.g. 1240x760, 900x650, 480x720) is rendered to <out>/ui_WxH.png.
# Requires: Xvfb, imagemagick (import), a built resonance-gui. Software GL via
# llvmpipe is forced so it works on a headless server with no GPU.
set -euo pipefail

BIN="${RESONANCE_GUI_BIN:-target/debug/resonance-gui}"
OUT="${UISHOT_OUT:-/tmp/user/1000/claude-1000/-home-nyverino-Documents-resonance/ed9b8810-e204-4aec-8c00-0909d65b9d23/scratchpad/shots}"
WAIT=4
SIZES=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin)  BIN="$2"; shift 2 ;;
    --out)  OUT="$2"; shift 2 ;;
    --wait) WAIT="$2"; shift 2 ;;
    *)      SIZES+=("$1"); shift ;;
  esac
done
[[ ${#SIZES[@]} -eq 0 ]] && SIZES=(1240x760 1000x680 760x640 460x760)

mkdir -p "$OUT"
[[ -x "$BIN" ]] || { echo "no binary at $BIN — build first (cargo build -p resonance-gui)"; exit 1; }

# Pick a free X display number.
DISP=99
while [[ -e "/tmp/.X11-unix/X${DISP}" ]]; do DISP=$((DISP+1)); done

shoot() {
  local size="$1" w h
  w="${size%x*}"; h="${size#*x}"
  # Fresh per-size egui/app storage so every capture starts from the shipped
  # defaults (default layout, theme, reference off) instead of inheriting the
  # developer's accumulated panel sizes. The daemon socket lives under
  # XDG_RUNTIME_DIR (left untouched), so the UI still shows live state.
  # Set UISHOT_REAL=1 to capture the developer's real persisted state instead.
  local xdg=""
  if [[ "${UISHOT_REAL:-0}" != "1" ]]; then
    xdg="$(mktemp -d)"
  fi
  # Xvfb screen exactly the window size: the app pins itself to (0,0) at that
  # inner size (RESONANCE_WINDOW_SIZE), so the whole root == the app content.
  Xvfb ":${DISP}" -screen 0 "${w}x${h}x24" -nolisten tcp >/dev/null 2>&1 &
  local xvfb=$!
  sleep 0.6
  # `env -u WAYLAND_DISPLAY`: without this, winit prefers the real Wayland
  # socket and the window opens on the user's actual desktop, not Xvfb.
  env -u WAYLAND_DISPLAY \
    ${xdg:+XDG_DATA_HOME="$xdg/data" XDG_CONFIG_HOME="$xdg/config"} \
    DISPLAY=":${DISP}" \
    WINIT_UNIX_BACKEND=x11 \
    LIBGL_ALWAYS_SOFTWARE=1 \
    RESONANCE_WINDOW_SIZE="${w}x${h}" \
    RUST_LOG=warn \
    "$BIN" >/dev/null 2>&1 &
  local app=$!
  sleep "$WAIT"
  if DISPLAY=":${DISP}" import -window root "${OUT}/ui_${size}.png" 2>/dev/null; then
    echo "wrote ${OUT}/ui_${size}.png (${w}x${h})"
  else
    echo "FAILED to capture ${size}"
  fi
  kill "$app" 2>/dev/null || true
  wait "$app" 2>/dev/null || true
  kill "$xvfb" 2>/dev/null || true
  wait "$xvfb" 2>/dev/null || true
  [[ -n "$xdg" ]] && rm -rf "$xdg"
}

for s in "${SIZES[@]}"; do shoot "$s"; done
echo "done → $OUT"
