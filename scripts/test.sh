#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

log_info "Running tests for $APP_NAME"

# --- Rust workspace (crates/*, app/src-tauri, agent). One workspace, so
# --workspace covers the agent too.
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

# --- agent/deploy helper tests. Mirrors CI's agent-tests job (#269). Needs
# nothing but bash: cargo, curl and sleep are stubbed, and the two cases that
# use the real cargo report themselves as skipped when it is absent. Since
# #392 the signature-rejection cases need the real minisign the same way, and
# skip — loudly, in the summary — without it; CI requires it.
if ! command_exists minisign; then
    log_warning "minisign not found — the install.sh signature cases will report SKIP (brew install minisign)"
fi
#
# On a Mac the suite runs under stock /bin/bash 3.2 — the interpreter the
# installer and its launcher actually execute under there — rather than
# whatever newer bash Homebrew put first on PATH; CI's macOS job does the same.
DEPLOY_TEST_SHELL="bash"
if [[ "$(uname -s)" == "Darwin" && -x /bin/bash ]]; then
    DEPLOY_TEST_SHELL="/bin/bash"
fi
log_info "Running agent deploy helper tests (agent/deploy/lib_test.sh, under $DEPLOY_TEST_SHELL)…"
if "$DEPLOY_TEST_SHELL" agent/deploy/lib_test.sh; then
    log_success "Agent deploy helper tests passed"
else
    log_error "Agent deploy helper tests failed"
    exit 1
fi

# --- Frontend e2e (Playwright), tests/frontend --- the only thing that
# exercises app/ui/ under the app's real CSP; mirrors CI's rust-workspace job.
# Needs BOTH npm and cargo: the suite's `pretest` shells out to
# `cargo run -p solador-app -- --dump` to generate its fixtures, so npm alone
# is not enough. Checking only npm let the original-only runner get as far as
# downloading 94 MB of Chromium before dying on `cargo: command not found`.
if command_exists npm && command_exists cargo; then
    if [[ ! -d "tests/frontend/node_modules" ]]; then
        log_info "Installing frontend test dependencies…"
        (cd tests/frontend && npm ci)
    fi

    # The e2e server's bind must resolve no name (#401): the stdlib's
    # reverse-DNS lookup cost 35s of the readiness window on the hosted macOS
    # runner and is invisible on a laptop, so this is asserted, not timed.
    # Stdlib unittest, no dependencies -- python3 is already what serves the
    # suite. Mirrors CI's "Frontend server bind test" step.
    log_info "Running frontend server bind test (tests/frontend/csp_server_test.py)…"
    if (cd tests/frontend && python3 -m unittest -v csp_server_test); then
        log_success "Frontend server bind test passed"
    else
        log_error "Frontend server bind test failed"
        exit 1
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