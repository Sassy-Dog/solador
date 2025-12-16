#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

# Build first (pass through all arguments)
"$SCRIPT_DIR/build.sh" "$@"

# Parse configuration from arguments to find built app
CONFIGURATION="$DEFAULT_CONFIGURATION"
for arg in "$@"; do
    case $arg in
        --release)
            CONFIGURATION="Release"
            ;;
    esac
done

# Run the built app
APP_PATH="$DERIVED_DATA_PATH/Build/Products/$CONFIGURATION/$APP_NAME.app"

if [[ ! -d "$APP_PATH" ]]; then
    log_error "Built app not found at: $APP_PATH"
    exit 1
fi

log_info "Running $APP_NAME ($CONFIGURATION)..."

# Kill any existing instance
pkill -f "$APP_NAME" 2>/dev/null || true

# Run the app
open "$APP_PATH"

log_success "$APP_NAME launched"