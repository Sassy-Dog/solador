#!/usr/bin/env bash
set -euo pipefail

# Local mirror of CI's lint gates (.github/workflows/ci.yml). Run via
# `./dev lint` or automatically by the pre-push hook (.githooks/pre-push), so a
# push never burns a CI round-trip on a formatting failure.
#
# `cargo fmt`/`clippy` below are what CI's rust-workspace job runs.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

ensure_project_root

status=0

# --- Rust workspace (crates/*, app/src-tauri). Mirrors CI's rust-workspace job
# (fmt --check + clippy -D warnings). The agent is a workspace member, so
# --workspace/--all lint it here as well.
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

# --- agent/deploy/*.sh. Mirrors the two shell gates added to CI's agent-tests
# job by #269. These scripts are the agent's only path onto a host and were the
# least-exercised code in the repo: #268 broke every deploy while `bash -n` and
# shellcheck were clean — because nothing ran them.
log_info "bash -n agent/deploy/*.sh…"
if bash -n agent/deploy/*.sh; then
    log_success "agent/deploy shell syntax clean"
else
    log_error "agent/deploy has a shell syntax error"
    status=1
fi

# shellcheck is not a repo dependency, so a machine without it gets a loud skip
# rather than a red run — the same rule scripts/test.sh applies to a missing
# toolchain (PR #126). CI runs it unconditionally, so the gate itself never skips.
if command_exists shellcheck; then
    log_info "shellcheck -S warning agent/deploy/*.sh…"
    if shellcheck -S warning agent/deploy/*.sh; then
        log_success "shellcheck clean"
    else
        log_error "shellcheck found problems in agent/deploy"
        status=1
    fi
else
    log_warning "shellcheck not found — skipping agent/deploy lint (CI still runs it; brew install shellcheck)"
fi

if [[ $status -eq 0 ]]; then
    log_success "Lint passed (mirrors CI)"
else
    log_error "Lint failed — fix the above before pushing (CI runs the same checks)"
fi

exit $status
