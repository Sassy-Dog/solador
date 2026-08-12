#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

log_info "Cleaning build artifacts"

# Clean build directory
if [[ -d "$BUILD_DIR" ]]; then
    log_info "Removing $BUILD_DIR..."
    rm -rf "$BUILD_DIR"
fi

# Clean Xcode derived data for this project
if [[ -d "$HOME/Library/Developer/Xcode/DerivedData" ]]; then
    log_info "Cleaning Xcode DerivedData..."
    find "$HOME/Library/Developer/Xcode/DerivedData" -name "$APP_NAME*" -type d -exec rm -rf {} + 2>/dev/null || true
fi

log_success "Clean complete"