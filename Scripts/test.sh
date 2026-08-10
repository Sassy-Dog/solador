#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

log_info "Running tests for $APP_NAME"

# The SwiftUI app is frozen and is no longer built or tested here or in CI
# (see CLAUDE.md). Its sources and tests remain in the repo; open the Xcode
# project with `./dev xcode` to run them by hand.

# --- Rust workspace (crates/*, app/src-tauri). agent/ is a separate Cargo
# workspace with its own CI job (agent-tests) and is deliberately not run here.
# Skip-with-a-warning when the toolchain is absent, matching how the frontend
# suite below handles a missing npm. This script is a convenience aggregator;
# CI gates each stack in its own job (rust-workspace, agent-tests), so a
# machine without Rust should get a loud skip, not a red run — the rule dates
# to PR #126, where hard-failing here turned a runner red over a toolchain it
# was never meant to have.
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
# `cargo run -p solador-app -- --dump` to generate its fixtures, so npm alone
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