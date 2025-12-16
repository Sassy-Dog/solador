#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

log_info "Running tests for $APP_NAME"

# Ensure project exists
if [[ ! -d "$PROJECT_NAME" ]]; then
    log_warning "Xcode project not found. Generating..."
    "$SCRIPT_DIR/generate-project.sh"
fi

# Run tests
if command_exists xcbeautify; then
    xcodebuild test \
        -project "$PROJECT_NAME" \
        -scheme "$SCHEME_NAME" \
        -configuration "Debug" \
        -derivedDataPath "$DERIVED_DATA_PATH" \
        -destination "platform=macOS" \
        | xcbeautify
else
    xcodebuild test \
        -project "$PROJECT_NAME" \
        -scheme "$SCHEME_NAME" \
        -configuration "Debug" \
        -derivedDataPath "$DERIVED_DATA_PATH" \
        -destination "platform=macOS"
fi

if [[ $? -eq 0 ]]; then
    log_success "All tests passed"
else
    log_error "Tests failed"
    exit 1
fi