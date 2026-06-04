#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

# Parse arguments
LOG_MODE=""
LOG_LEVEL=""
BUILD_ARGS=()

while [[ $# -gt 0 ]]; do
    case $1 in
        --log)
            LOG_MODE="$2"
            shift 2
            ;;
        --log-level)
            LOG_LEVEL="$2"
            shift 2
            ;;
        *)
            BUILD_ARGS+=("$1")
            shift
            ;;
    esac
done

# Build first (pass through build arguments)
"$SCRIPT_DIR/build.sh" "${BUILD_ARGS[@]}"

# Parse configuration from arguments to find built app
CONFIGURATION="$DEFAULT_CONFIGURATION"
for arg in "${BUILD_ARGS[@]}"; do
    case $arg in
        --release)
            CONFIGURATION="Release"
            ;;
    esac
done

# Run the built app
APP_PATH="$DERIVED_DATA_PATH/Build/Products/$CONFIGURATION/$APP_NAME.app"
BINARY_PATH="$APP_PATH/Contents/MacOS/$APP_NAME"

if [[ ! -d "$APP_PATH" ]]; then
    log_error "Built app not found at: $APP_PATH"
    exit 1
fi

# Kill any existing instance and WAIT for it to fully exit. pkill only signals;
# launching while the old process is still terminating makes LaunchServices fail
# with error -600 ("every other time"). Poll until it's gone (bounded ~3s).
pkill -x "$APP_NAME" 2>/dev/null || true
for _ in $(seq 1 30); do
    pgrep -x "$APP_NAME" >/dev/null 2>&1 || break
    sleep 0.1
done

# Run with console logging if requested
if [[ "$LOG_MODE" == "console" ]] || [[ "$LOG_MODE" == "both" ]]; then
    log_info "Running $APP_NAME ($CONFIGURATION) with console logging..."
    
    # Create Logs directory
    mkdir -p "Logs"
    
    # Create timestamped log file
    TIMESTAMP=$(date +"%Y%m%d_%H%M%S")
    LOG_FILE="Logs/devcanopy_${TIMESTAMP}.log"
    
    # Set up environment variables for logging
    ENV_VARS=()
    ENV_VARS+=("DEVCANOPY_LOG_CONSOLE=1")
    
    if [[ -n "$LOG_LEVEL" ]]; then
        ENV_VARS+=("DEVCANOPY_LOG_LEVEL=$LOG_LEVEL")
    fi
    
    # Run binary directly in foreground with logging
    log_info "Logs will be saved to: $LOG_FILE"
    env "${ENV_VARS[@]}" "$BINARY_PATH" 2>&1 | tee "$LOG_FILE"
else
    # Normal launch (background). Retry once on a transient LaunchServices error
    # (-600) in case the previous instance hadn't fully deregistered yet.
    log_info "Running $APP_NAME ($CONFIGURATION)..."
    if ! open "$APP_PATH" 2>/dev/null; then
        sleep 0.7
        open "$APP_PATH"
    fi
    log_success "$APP_NAME launched"
fi