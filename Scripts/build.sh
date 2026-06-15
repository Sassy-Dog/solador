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

# Get build number
BUILD_NUMBER=$(get_build_number_from_git)
log_info "Version: $VERSION ($BUILD_NUMBER)"

# Build with xcodebuild
log_info "Building..."

# Forward the Sentry DSN (issue #18) into the SentryDSN Info.plist key via the
# SENTRY_DSN build setting. Defaults to empty when unset (project.yml), so a
# normal local build ships no DSN and Sentry no-ops — telemetry stays opt-in.
# Release/CI sets SENTRY_DSN from the GitHub Actions secret; never hardcoded.
SENTRY_DSN="${SENTRY_DSN:-}"

if command_exists xcbeautify; then
    xcodebuild \
        -project "$PROJECT_NAME" \
        -scheme "$SCHEME_NAME" \
        -configuration "$CONFIGURATION" \
        -derivedDataPath "$DERIVED_DATA_PATH" \
        -allowProvisioningUpdates \
        CURRENT_PROJECT_VERSION="$BUILD_NUMBER" \
        MARKETING_VERSION="$VERSION" \
        SENTRY_DSN="$SENTRY_DSN" \
        build | xcbeautify
else
    xcodebuild \
        -project "$PROJECT_NAME" \
        -scheme "$SCHEME_NAME" \
        -configuration "$CONFIGURATION" \
        -derivedDataPath "$DERIVED_DATA_PATH" \
        -allowProvisioningUpdates \
        CURRENT_PROJECT_VERSION="$BUILD_NUMBER" \
        MARKETING_VERSION="$VERSION" \
        SENTRY_DSN="$SENTRY_DSN" \
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