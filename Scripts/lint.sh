#!/usr/bin/env bash
set -euo pipefail

# Local mirror of CI's lint gates (.github/workflows/ci.yml). Run via
# `./dev lint` or automatically by the pre-push hook (.githooks/pre-push), so a
# push never burns a CI round-trip on a formatting failure.
#
# Swift is not linted here or in CI: the SwiftUI app is frozen (see CLAUDE.md),
# and the SwiftLint/SwiftFormat pins it needed lived in ci.yml, which no longer
# carries them. `cargo fmt`/`clippy` below are what CI's rust-workspace job runs.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

ensure_project_root

status=0

# --- Rust workspace (crates/*, app/src-tauri). Mirrors CI's rust-workspace job
# (fmt --check + clippy -D warnings). agent/ has its own CI job (agent-tests)
# and is deliberately not linted here.
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
