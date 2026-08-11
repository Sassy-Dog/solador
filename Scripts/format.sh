#!/usr/bin/env bash
set -euo pipefail

# Auto-fix formatting in place across the Rust workspace — the bulk counterpart
# to `./dev lint`, which only reports.
#

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

ensure_project_root

if command_exists cargo; then
    log_info "Formatting Rust workspace sources in place (cargo fmt)…"
    cargo fmt --all
    log_success "Rust formatting complete"
else
    log_warning "cargo not found — skipped formatting the Rust workspace"
fi

log_success "Formatting complete — review changes with: git diff"
