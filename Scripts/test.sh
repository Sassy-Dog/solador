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
    log_success "Swift tests passed"
else
    log_error "Swift tests failed"
    exit 1
fi

# --- Rust workspace (crates/metrics, crates/viewmodel, crates/agentclient,
# app/src-tauri) --- additive: does not touch anything above. agent/ is a
# separate Cargo workspace with its own CI job (agent-tests) and is
# deliberately not run here.
# Skip-with-a-warning when the toolchain is absent, matching how the frontend
# suite below handles a missing npm. This script is a convenience aggregator;
# CI gates each stack in its own job (rust-workspace, agent-tests), so a
# machine without Rust should get a loud skip, not a red run. Hard-failing here
# turned the Swift-only CI runner red after 350 passing Swift tests (PR #126).
if command_exists cargo; then
    log_info "Running Rust workspace tests (crates/*, app/src-tauri)…"
    if cargo test --locked --workspace; then
        log_success "Rust workspace tests passed"
    else
        log_error "Rust workspace tests failed"
        exit 1
    fi
else
    log_warning "cargo not found — skipping Rust workspace tests (crates/*, app/src-tauri)"
fi

# --- Frontend e2e (Playwright), tests/frontend --- the only thing that
# exercises app/ui/ under the app's real CSP; mirrors CI's rust-workspace job.
# Needs BOTH npm and cargo: the suite's `pretest` shells out to
# `cargo run -p devcanopy-app -- --dump` to generate its fixtures, so npm alone
# is not enough. Checking only npm let the Swift-only runner get as far as
# downloading 94 MB of Chromium before dying on `cargo: command not found`.
if command_exists npm && command_exists cargo; then
    if [[ ! -d "tests/frontend/node_modules" ]]; then
        log_info "Installing frontend test dependencies…"
        (cd tests/frontend && npm ci)
    fi

    log_info "Running frontend e2e tests (tests/frontend)…"
    if (cd tests/frontend && npx playwright install chromium && npm test); then
        log_success "Frontend e2e tests passed"
    else
        log_error "Frontend e2e tests failed"
        exit 1
    fi
else
    log_warning "npm and/or cargo not found — skipping frontend e2e tests (tests/frontend)"
fi

log_success "All tests passed"