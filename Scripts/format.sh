#!/usr/bin/env bash
set -euo pipefail

# Auto-fix formatting in place with SwiftFormat (respects .swiftformat). The
# bulk/manual counterpart to `./dev lint`; the PostToolUse hook formats files
# one at a time as they're edited, this fixes the whole tree at once.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

ensure_project_root

if ! command_exists swiftformat; then
    log_error "swiftformat not found — install with: brew install swiftformat"
    exit 1
fi

log_info "Formatting Swift sources in place (swiftformat)…"
swiftformat .
log_success "Formatting complete — review changes with: git diff"
