#!/usr/bin/env bash
#
# Renders brand/icon.svg into the app's icon set, and brand/mark.svg into the
# frontend's copy.
#
# Nothing in app/src-tauri/icons/ or app/ui/mark.svg should be edited by hand --
# they are outputs. `brand/` is the source. Run this after changing either.
#
#   ./Scripts/generate-icons.sh
#
# Two mirror tests fail if the outputs drift from brand/, so a forgotten run is
# caught by `./dev test` rather than by someone noticing a stale icon.

set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
ROOT_DIR="$( cd "$SCRIPT_DIR/.." && pwd )"

BRAND="$ROOT_DIR/brand"
ICONS="$ROOT_DIR/app/src-tauri/icons"
UI="$ROOT_DIR/app/ui"
NODE_MODULES="$ROOT_DIR/tests/frontend/node_modules"

# Chromium, not ImageMagick -- see the header of render-icons.mjs for why. It
# comes from the frontend test suite's install, which is the same dependency
# `./dev test` already needs.
if [[ ! -d "$NODE_MODULES/playwright" ]]; then
    log_error "Playwright is not installed."
    echo "    This script rasterises the SVG with Chromium. Install it once with:"
    echo "      cd tests/frontend && npm ci && npx playwright install chromium"
    exit 1
fi

command_exists magick || { log_error "ImageMagick (magick) is required for the .ico"; exit 1; }

log_info "Rendering $(basename "$BRAND")/icon.svg…"

# Tauri's own set. Rounded everywhere except the .ico sources: Windows applies no
# mask of its own, so a rounded .ico would show a shrunken icon in a square slot.
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

MACOS_SIZES="icon_16x16.png:16:rounded,icon_16x16@2x.png:32:rounded,\
icon_32x32.png:32:rounded,icon_32x32@2x.png:64:rounded,\
icon_128x128.png:128:rounded,icon_128x128@2x.png:256:rounded,\
icon_256x256.png:256:rounded,icon_256x256@2x.png:512:rounded,\
icon_512x512.png:512:rounded,icon_512x512@2x.png:1024:rounded"

SQUARE_SIZES="ico-16.png:16:square,ico-24.png:24:square,ico-32.png:32:square,\
ico-48.png:48:square,ico-64.png:64:square,ico-128.png:128:square,ico-256.png:256:square"

TAURI_SIZES="32x32.png:32:rounded,128x128.png:128:rounded,\
128x128@2x.png:256:rounded,icon.png:512:rounded"

ICONSET="$TMP/icon.iconset"
mkdir -p "$ICONSET"

node "$SCRIPT_DIR/render-icons.mjs" "$BRAND/icon.svg" "$ICONSET"     "$MACOS_SIZES"  "$NODE_MODULES" >/dev/null
node "$SCRIPT_DIR/render-icons.mjs" "$BRAND/icon.svg" "$TMP/square" "$SQUARE_SIZES" "$NODE_MODULES" >/dev/null
node "$SCRIPT_DIR/render-icons.mjs" "$BRAND/icon.svg" "$ICONS"      "$TAURI_SIZES"  "$NODE_MODULES" >/dev/null

log_success "PNG set rendered"

# .icns -- macOS only. iconutil ships with Xcode's command line tools; on a
# non-mac the existing .icns is left in place rather than silently omitted,
# because a bundle without one falls back to a generic document icon.
if command_exists iconutil; then
    iconutil -c icns "$ICONSET" -o "$ICONS/icon.icns"
    log_success "icon.icns ($(du -h "$ICONS/icon.icns" | cut -f1 | tr -d ' '))"
else
    log_warning "iconutil not found (not macOS?) -- icon.icns left as-is"
fi

magick "$TMP/square/ico-16.png" "$TMP/square/ico-24.png" "$TMP/square/ico-32.png" \
       "$TMP/square/ico-48.png" "$TMP/square/ico-64.png" "$TMP/square/ico-128.png" \
       "$TMP/square/ico-256.png" "$ICONS/icon.ico"
log_success "icon.ico ($(du -h "$ICONS/icon.ico" | cut -f1 | tr -d ' '))"

# The frontend cannot reach ../../brand: its dist root is app/ui, and the app's
# CSP is `img-src 'self' data:`. So the mark is copied in, and a test asserts the
# copy still matches its source.
cp "$BRAND/mark.svg" "$UI/mark.svg"
log_success "app/ui/mark.svg copied from brand/"

log_success "Icons generated. Commit app/src-tauri/icons/ and app/ui/mark.svg."
