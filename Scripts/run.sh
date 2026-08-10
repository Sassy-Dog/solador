#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

# Parse arguments
LOG_MODE=""
LOG_LEVEL=""
TAURI=0
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
        --tauri)
            TAURI=1
            shift
            ;;
        *)
            BUILD_ARGS+=("$1")
            shift
            ;;
    esac
done

# --tauri: run the cross-platform Tauri cockpit from the root Rust workspace
# instead of the SwiftUI app. Everything below this block is the SwiftUI path,
# untouched. exec keeps the caller's environment (SOLADOR_SEED_HOST,
# SOLADOR_STORE_DIR, ...) reaching the app.
if [[ $TAURI -eq 1 ]]; then
    ROOT_DIR="$( cd "$SCRIPT_DIR/.." && pwd )"
    TAURI_PACKAGE="solador-app"

    # --release composes with --tauri; anything else is the app's own argument
    # (e.g. the --dump fixture modes documented in app/README.md).
    CARGO_PROFILE="debug"
    CARGO_FLAGS=()
    APP_ARGS=()
    for arg in ${BUILD_ARGS[@]+"${BUILD_ARGS[@]}"}; do
        case $arg in
            --release)
                CARGO_PROFILE="release"
                CARGO_FLAGS+=("--release")
                ;;
            *)
                APP_ARGS+=("$arg")
                ;;
        esac
    done

    log_info "Building $TAURI_PACKAGE ($CARGO_PROFILE)..."
    cargo build --manifest-path "$ROOT_DIR/Cargo.toml" -p "$TAURI_PACKAGE" \
        ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"}

    TAURI_BINARY="$ROOT_DIR/target/$CARGO_PROFILE/$TAURI_PACKAGE"

    # cargo stamps a *fresh ad-hoc* signature on every relink, and a new signing
    # identity invalidates the Keychain ACLs the app's stored credentials carry —
    # so every rebuild re-prompts for every item (4+ prompts per run). Re-sign with
    # the stable Apple Development identity (team 52YMXC3348, the same one
    # project.yml gives the Swift Debug build) so the ACLs keep matching across
    # rebuilds. macOS only, and silently skipped where no identity is installed
    # (CI, other machines) — an unsigned run still works, it just re-prompts.
    if [[ "$(uname -s)" == "Darwin" ]]; then
        CODESIGN_IDENTITIES="$(security find-identity -v -p codesigning 2>/dev/null || true)"
        if [[ "$CODESIGN_IDENTITIES" == *"Apple Development"* ]]; then
            codesign --force --sign "Apple Development" "$TAURI_BINARY" >/dev/null 2>&1 ||
                log_warning "Could not re-sign $TAURI_PACKAGE — the Keychain may re-prompt"
        fi
    fi

    log_info "Running $TAURI_PACKAGE ($CARGO_PROFILE)..."
    exec "$TAURI_BINARY" ${APP_ARGS[@]+"${APP_ARGS[@]}"}
fi

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

    # Prune old console logs, keeping only the newest MAX_LOGS files. Without this,
    # every console run leaves a new timestamped log behind and Logs/ grows forever.
    MAX_LOGS=10
    ls -1t Logs/devcanopy_*.log 2>/dev/null | tail -n +"$MAX_LOGS" | while IFS= read -r old_log; do
        rm -f "$old_log"
    done

    # Create timestamped log file
    TIMESTAMP=$(date +"%Y%m%d_%H%M%S")
    LOG_FILE="Logs/devcanopy_${TIMESTAMP}.log"
    
    # Set up environment variables for logging
    ENV_VARS=()
    ENV_VARS+=("SOLADOR_LOG_CONSOLE=1")
    
    if [[ -n "$LOG_LEVEL" ]]; then
        ENV_VARS+=("SOLADOR_LOG_LEVEL=$LOG_LEVEL")
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