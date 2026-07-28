#!/usr/bin/env bash
set -euo pipefail

# Local mirror of CI's Lint job (.github/workflows/ci.yml). Run via `./dev lint`
# or automatically by the pre-push hook (.githooks/pre-push). Catches the exact
# SwiftLint/SwiftFormat failures CI would, before a push burns a CI round-trip.
#
# Runs the SAME pinned binaries CI downloads (provisioned by lint-tools.sh into
# build/lint-tools/), so a floating Homebrew install can never make lint pass
# locally and fail in CI — or the reverse.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

ensure_project_root

source "$SCRIPT_DIR/lint-tools.sh"
SWIFTLINT=$(ensure_pinned_swiftlint) || exit 1
SWIFTFORMAT=$(ensure_pinned_swiftformat) || exit 1

status=0

log_info "SwiftLint $("$SWIFTLINT" version) (--strict, baselined)…"
if "$SWIFTLINT" lint --baseline lint-baseline.json --strict --quiet; then
    log_success "SwiftLint clean"
else
    log_error "SwiftLint found non-baselined violations"
    status=1
fi

log_info "SwiftFormat $("$SWIFTFORMAT" --version) (--lint)…"
if "$SWIFTFORMAT" --lint . ; then
    log_success "SwiftFormat clean"
else
    log_error "SwiftFormat would reformat files — run ./dev format to fix"
    status=1
fi

if [[ $status -eq 0 ]]; then
    log_success "Swift lint passed (mirrors CI)"
else
    log_error "Swift lint failed"
fi

# --- Rust workspace (crates/metrics, crates/viewmodel, crates/agentclient,
# app/src-tauri) --- additive: does not touch the Swift status above. Mirrors
# CI's rust-workspace job (fmt --check + clippy -D warnings). agent/ has its
# own CI job (agent-tests) and is deliberately not linted here.
if command_exists cargo; then
    log_info "cargo fmt --all -- --check…"
    if cargo fmt --all -- --check; then
        log_success "cargo fmt clean"
    else
        log_error "cargo fmt would reformat files — run: cargo fmt --all"
        status=1
    fi

    log_info "cargo clippy --workspace --all-targets -- -D warnings…"
    if cargo clippy --locked --workspace --all-targets -- -D warnings; then
        log_success "cargo clippy clean"
    else
        log_error "cargo clippy found warnings"
        status=1
    fi
else
    log_error "cargo not found — install the Rust toolchain (rust-toolchain.toml pins the version) to lint the Rust workspace"
    status=1
fi

if [[ $status -eq 0 ]]; then
    log_success "Lint passed (mirrors CI)"
else
    log_error "Lint failed — fix the above before pushing (CI runs the same checks)"
fi

exit $status
