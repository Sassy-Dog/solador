#!/usr/bin/env bash
set -euo pipefail

# Auto-fix formatting in place with SwiftFormat (respects .swiftformat). The
# bulk/manual counterpart to `./dev lint`; the PostToolUse hook formats files
# one at a time as they're edited, this fixes the whole tree at once.
#
# Uses the same CI-pinned binary as ./dev lint (lint-tools.sh), so formatted
# output always satisfies the lint gate.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

ensure_project_root

source "$SCRIPT_DIR/lint-tools.sh"
SWIFTFORMAT=$(ensure_pinned_swiftformat) || exit 1

log_info "Formatting Swift sources in place (swiftformat $("$SWIFTFORMAT" --version))…"
"$SWIFTFORMAT" .
log_success "Swift formatting complete"

# Rust workspace (crates/wire, crates/viewmodel, crates/agentclient,
# app/src-tauri) -- additive, matches ./dev lint's `cargo fmt --check`.
# agent/ has its own toolchain/CI and is left to it.
if command_exists cargo; then
    log_info "Formatting Rust workspace sources in place (cargo fmt)…"
    cargo fmt --all
    log_success "Rust formatting complete"
else
    log_warning "cargo not found — skipped formatting the Rust workspace"
fi

log_success "Formatting complete — review changes with: git diff"
