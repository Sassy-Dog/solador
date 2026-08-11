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
    # What we exec, and what codesign is pointed at. Both are reassigned to the
    # .app below on macOS; off macOS the bare binary stays correct.
    LAUNCH_BINARY="$TAURI_BINARY"
    SIGN_TARGET="$TAURI_BINARY"

    # Wrap the binary in a throwaway .app so the Dock, ⌘-Tab and the menu bar
    # show the mark and the product name instead of a generic executable.
    #
    # An icon cannot come from the binary. tauri-build embeds an Info.plist into
    # __TEXT,__info_plist (that is where "Solador" in the menu bar comes from),
    # but CFBundleIconFile names a file in Contents/Resources and a bare Mach-O
    # has no Resources — so a Dock icon needs a real bundle, and `bundle.active`
    # is false because we are not shipping from here.
    #
    # The binary is *launched from inside* the bundle rather than via `open`:
    # macOS reads the enclosing bundle either way, and exec keeps stdout,
    # stderr and the caller's environment (SOLADOR_SEED_HOST, SOLADOR_STORE_DIR,
    # the --dump modes) attached to the terminal. `open` would detach all of it.
    if [[ "$(uname -s)" == "Darwin" ]]; then
        # plutil reads JSON, so every value below comes from tauri.conf.json
        # rather than a second copy that drifts from it.
        TAURI_CONF="$ROOT_DIR/app/src-tauri/tauri.conf.json"
        BUNDLE_NAME="$(plutil -extract productName raw -o - "$TAURI_CONF")"
        BUNDLE_ID="$(plutil -extract identifier raw -o - "$TAURI_CONF")"
        BUNDLE_MIN_OS="$(plutil -extract 'bundle.macOS.minimumSystemVersion' raw -o - "$TAURI_CONF" 2>/dev/null || echo "14.0")"
        APP_BUNDLE="$ROOT_DIR/target/$CARGO_PROFILE/$BUNDLE_NAME.app"
        ICNS="$ROOT_DIR/app/src-tauri/icons/icon.icns"

        rm -rf "$APP_BUNDLE"
        mkdir -p "$APP_BUNDLE/Contents/MacOS" "$APP_BUNDLE/Contents/Resources"
        # A copy, not a symlink: codesign will not sign a bundle whose
        # executable is a link out of it, and cargo replaces the file on every
        # relink so a hard link would go stale. On APFS this is a clone.
        cp "$TAURI_BINARY" "$APP_BUNDLE/Contents/MacOS/$TAURI_PACKAGE"

        if [[ -f "$ICNS" ]]; then
            cp "$ICNS" "$APP_BUNDLE/Contents/Resources/icon.icns"
        else
            log_warning "app/src-tauri/icons/icon.icns missing — run ./Scripts/generate-icons.sh"
        fi

        cat > "$APP_BUNDLE/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key><string>$BUNDLE_NAME</string>
	<key>CFBundleDisplayName</key><string>$BUNDLE_NAME</string>
	<key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
	<key>CFBundleExecutable</key><string>$TAURI_PACKAGE</string>
	<key>CFBundleIconFile</key><string>icon</string>
	<key>CFBundlePackageType</key><string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
	<key>LSMinimumSystemVersion</key><string>$BUNDLE_MIN_OS</string>
	<key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
        LAUNCH_BINARY="$APP_BUNDLE/Contents/MacOS/$TAURI_PACKAGE"
        SIGN_TARGET="$APP_BUNDLE"
    fi

    # cargo stamps a *fresh ad-hoc* signature on every relink, and a new signing
    # identity invalidates the Keychain ACLs the app's stored credentials carry —
    # so every rebuild re-prompts for every item (4+ prompts per run). Re-sign with
    # the stable Apple Development identity (the team `config.sh` resolves, the
    # same one the Swift Debug build uses) so the ACLs keep matching across
    # rebuilds. macOS only, and silently skipped where no identity is installed
    # (CI, other machines) — an unsigned run still works, it just re-prompts.
    if [[ "$(uname -s)" == "Darwin" ]]; then
        CODESIGN_IDENTITIES="$(security find-identity -v -p codesigning 2>/dev/null || true)"
        SIGN_LINES="$(printf '%s\n' "$CODESIGN_IDENTITIES" | grep -F "Apple Development" || true)"
        # Narrow to this team when we know it. `--sign "Apple Development"` is a
        # PREFIX match, so a keychain holding certs for two teams makes codesign
        # fail with an ambiguity error — and that failure lands in the `||`
        # below as a warning nobody reads, leaving the binary ad-hoc signed and
        # the Keychain prompting on every run. The exact thing this block exists
        # to prevent.
        if [[ -n "${DEVELOPMENT_TEAM:-}" ]]; then
            TEAM_LINES="$(printf '%s\n' "$SIGN_LINES" | grep -F "($DEVELOPMENT_TEAM)" || true)"
            [[ -n "$TEAM_LINES" ]] && SIGN_LINES="$TEAM_LINES"
        fi
        # The SHA-1, not the name: a hash names exactly one certificate.
        SIGN_ID="$(printf '%s\n' "$SIGN_LINES" | head -n1 | awk '{print $2}')"
        if [[ -n "$SIGN_ID" ]]; then
            # Sign the BUNDLE, not the loose binary — signing the copy inside it
            # is what the ACLs will be matched against at launch.
            #
            # `--identifier` is pinned deliberately. Left alone, codesign takes
            # the identifier from CFBundleIdentifier, which would move the
            # designated requirement from `solador-app` (derived from the bare
            # binary's filename) to `app.solador.desktop`. Keychain ACLs are
            # bound to that requirement, so the change would re-prompt for every
            # stored credential exactly once — the storm this block exists to
            # prevent. Pinning keeps every ACL already granted valid.
            #
            # A real bundled release will sign as app.solador.desktop and pay
            # that one-time cost then, deliberately, rather than surprising
            # someone mid-dev-loop.
            codesign --force --identifier "$TAURI_PACKAGE" --sign "$SIGN_ID" \
                "$SIGN_TARGET" >/dev/null 2>&1 ||
                log_warning "Could not re-sign $TAURI_PACKAGE — the Keychain may re-prompt"
        fi
    fi

    log_info "Running $TAURI_PACKAGE ($CARGO_PROFILE)..."
    exec "$LAUNCH_BINARY" ${APP_ARGS[@]+"${APP_ARGS[@]}"}
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