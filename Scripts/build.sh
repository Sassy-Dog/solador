#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

# Default values
CONFIGURATION="$DEFAULT_CONFIGURATION"
CLEAN_FIRST=0

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --release)
            CONFIGURATION="Release"
            shift
            ;;
        --configuration)
            CONFIGURATION="$2"
            shift 2
            ;;
        --clean-first)
            CLEAN_FIRST=1
            shift
            ;;
        *)
            log_error "Unknown option: $1"
            exit 1
            ;;
    esac
done

log_info "Building $APP_NAME ($CONFIGURATION)"

# Clean if requested
if [[ $CLEAN_FIRST -eq 1 ]]; then
    log_info "Cleaning build artifacts..."
    "$SCRIPT_DIR/clean.sh"
fi

# Ensure project exists
if [[ ! -d "$PROJECT_NAME" ]]; then
    log_warning "Xcode project not found. Generating..."
    "$SCRIPT_DIR/generate-project.sh"
fi

# Derive the two decoupled numbers from their single-source scripts
# (Docs/VERSIONING.md). Both honor the org replay pins — MARKETING_VERSION /
# BUILD_NUMBER env are emitted verbatim — which is how publish.sh threads the
# §4-minted version into this build instead of letting it re-resolve.
BUILD_NUMBER="$("$SCRIPT_DIR/get-build-number.sh")"
MARKETING_VERSION="$("$SCRIPT_DIR/get-version-info.sh" --version)"
log_info "Version: $MARKETING_VERSION ($BUILD_NUMBER)"

# Build with xcodebuild
log_info "Building..."

# Forward the Sentry DSN (issue #18) into the SentryDSN Info.plist key via the
# SENTRY_DSN build setting. Defaults to empty when unset (project.yml), so a
# normal local build ships no DSN and Sentry no-ops — telemetry stays opt-in.
# publish.sh resolves it from Doppler and exports it (issue #75); CI deliberately
# leaves it unset so the no-op path stays exercised. Never hardcoded.
SENTRY_DSN="${SENTRY_DSN:-}"

# The team id, same shape as the DSN above: resolved outside this file, passed
# as a command-line build setting so it overrides whatever xcodegen wrote.
# Omitted entirely when unresolved — passing DEVELOPMENT_TEAM="" would override
# automatic signing with nothing, which is worse than not passing it.
resolve_development_team
SIGNING_SETTINGS=()
if [[ -n "${DEVELOPMENT_TEAM:-}" ]]; then
    SIGNING_SETTINGS+=("DEVELOPMENT_TEAM=$DEVELOPMENT_TEAM")
fi

if command_exists xcbeautify; then
    xcodebuild \
        -project "$PROJECT_NAME" \
        -scheme "$SCHEME_NAME" \
        -configuration "$CONFIGURATION" \
        -derivedDataPath "$DERIVED_DATA_PATH" \
        -allowProvisioningUpdates \
        CURRENT_PROJECT_VERSION="$BUILD_NUMBER" \
        MARKETING_VERSION="$MARKETING_VERSION" \
        SENTRY_DSN="$SENTRY_DSN" \
        ${SIGNING_SETTINGS[@]+"${SIGNING_SETTINGS[@]}"} \
        build | xcbeautify
else
    xcodebuild \
        -project "$PROJECT_NAME" \
        -scheme "$SCHEME_NAME" \
        -configuration "$CONFIGURATION" \
        -derivedDataPath "$DERIVED_DATA_PATH" \
        -allowProvisioningUpdates \
        CURRENT_PROJECT_VERSION="$BUILD_NUMBER" \
        MARKETING_VERSION="$MARKETING_VERSION" \
        SENTRY_DSN="$SENTRY_DSN" \
        ${SIGNING_SETTINGS[@]+"${SIGNING_SETTINGS[@]}"} \
        -quiet \
        build
fi

if [[ $? -eq 0 ]]; then
    log_success "Build completed successfully"
    
    # Show build location
    APP_PATH="$DERIVED_DATA_PATH/Build/Products/$CONFIGURATION/$APP_NAME.app"
    if [[ -d "$APP_PATH" ]]; then
        log_info "App location: $APP_PATH"
    fi
else
    log_error "Build failed"
    exit 1
fi