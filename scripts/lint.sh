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

# --- Every shell script this repo ships. Mirrors the two shell gates added to
# CI's agent-tests job by #269, widened in #390.
#
# agent/deploy/ is the agent's only path onto a host, and was the least-exercised
# code in the repo: #268 broke every deploy while both of these gates were green,
# because neither of them was pointed at those files.
#
# scripts/ is the same shape one level up. `build-agent.sh` builds and signs the
# four published agent binaries and runs ONLY on a `v*` tag, so an ungated break
# there surfaces mid-release — the exact failure mode above, on the path that has
# no second chance. The whole directory is covered rather than that one file,
# because the next release script would otherwise arrive equally unguarded.
#
# (The word "shell·check" is spelt out in this comment rather than written as one
# word: shellcheck reads `# shellcheck …` at the start of a comment as a
# directive and refuses to parse the file when it is prose.)
SHELL_SOURCES=(agent/deploy/*.sh scripts/*.sh dev prd)

log_info "bash -n over ${#SHELL_SOURCES[@]} shell sources…"
if bash -n "${SHELL_SOURCES[@]}"; then
    log_success "shell syntax clean"
else
    log_error "a shell script has a syntax error"
    status=1
fi

# The linter below is not a repo dependency, so a machine without it gets a loud
# skip rather than a red run — the same rule scripts/test.sh applies to a missing
# toolchain (PR #126). CI runs it unconditionally, so the gate itself never
# skips. (Named obliquely for the reason the block above records: a comment that
# opens with the tool's name is read as a directive, and #390 pointed the tool at
# this file for the first time.)
if command_exists shellcheck; then
    log_info "shellcheck -S warning over ${#SHELL_SOURCES[@]} shell sources…"
    if shellcheck -S warning "${SHELL_SOURCES[@]}"; then
        log_success "shellcheck clean"
    else
        log_error "shellcheck found problems in a shell script"
        status=1
    fi
else
    log_warning "shellcheck not found — skipping the shell lint (CI still runs it; brew install shellcheck)"
fi

# --- The secrets guard and its corpus, exactly as CI's `secrets-guard` job
# runs them (#391). Sub-second, dependency-free, and the one gate whose
# failure is invisible until a fork PR reads an empty secret.
log_info "secrets guard…"
if "$SCRIPT_DIR/secrets-guard.sh" >/dev/null && "$SCRIPT_DIR/secrets-guard-test.sh" >/dev/null; then
    log_success "secrets guard clean, and its corpus behaves"
else
    log_error "the secrets guard, or its self-test, failed — run scripts/secrets-guard.sh and scripts/secrets-guard-test.sh"
    status=1
fi

# --- A deliberate absence, asserted: the agent must not depend on the release
# tooling in crates/updatefeed (#391). The same script CI's `secrets-guard`
# job runs; see its header for why it lists the tree rather than asking
# `cargo tree -i`.
if command_exists cargo; then
    log_info "cargo tree: agent/ must not resolve crates/updatefeed…"
    if "$SCRIPT_DIR/agent-deps-guard.sh" >/dev/null; then
        log_success "agent/ does not resolve solador-updatefeed"
    else
        log_error "agent/ resolves solador-updatefeed, or cargo tree failed — run scripts/agent-deps-guard.sh"
        status=1
    fi
fi

if [[ $status -eq 0 ]]; then
    log_success "Lint passed (mirrors CI)"
else
    log_error "Lint failed — fix the above before pushing (CI runs the same checks)"
fi

exit $status
