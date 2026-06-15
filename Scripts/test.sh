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

# In CI there are no signing certificates, and unit tests don't need signing.
# Build the tests unsigned there; local runs keep the project's normal signing.
EXTRA_ARGS=()
if [[ -n "${CI:-}" ]]; then
    EXTRA_ARGS=(CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY= DEVELOPMENT_TEAM=)
fi

# In CI, also gather line coverage into a result bundle so the workflow's
# coverage-floor gate (.github/workflows/ci.yml) can read it via `xccov`. Local
# runs stay lean and skip coverage instrumentation. Override the bundle path with
# RESULT_BUNDLE_PATH if needed.
COVERAGE_ARGS=()
if [[ -n "${CI:-}" ]]; then
    RESULT_BUNDLE_PATH="${RESULT_BUNDLE_PATH:-$BUILD_DIR/TestResults.xcresult}"
    rm -rf "$RESULT_BUNDLE_PATH"
    COVERAGE_ARGS=(-enableCodeCoverage YES -resultBundlePath "$RESULT_BUNDLE_PATH")
fi

# Run tests
if command_exists xcbeautify; then
    xcodebuild test \
        -project "$PROJECT_NAME" \
        -scheme "$SCHEME_NAME" \
        -configuration "Debug" \
        -derivedDataPath "$DERIVED_DATA_PATH" \
        -destination "platform=macOS" \
        "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}" \
        "${COVERAGE_ARGS[@]+"${COVERAGE_ARGS[@]}"}" \
        | xcbeautify
else
    xcodebuild test \
        -project "$PROJECT_NAME" \
        -scheme "$SCHEME_NAME" \
        -configuration "Debug" \
        -derivedDataPath "$DERIVED_DATA_PATH" \
        -destination "platform=macOS" \
        "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}" \
        "${COVERAGE_ARGS[@]+"${COVERAGE_ARGS[@]}"}"
fi

if [[ $? -eq 0 ]]; then
    log_success "All tests passed"
else
    log_error "Tests failed"
    exit 1
fi