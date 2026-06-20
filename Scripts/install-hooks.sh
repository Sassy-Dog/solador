#!/usr/bin/env bash
set -euo pipefail

# One-time setup: point git at the committed .githooks/ directory so the pre-push
# lint gate runs. Idempotent — safe to re-run after a fresh clone.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"

ensure_project_root

git config core.hooksPath .githooks
chmod +x .githooks/* 2>/dev/null || true

log_success "Git hooks installed (core.hooksPath → .githooks)"
log_info "pre-push now runs ./dev lint — bypass a single push with: git push --no-verify"
