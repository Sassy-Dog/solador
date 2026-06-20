#!/usr/bin/env bash
set -euo pipefail

# Local mirror of CI's Lint job (.github/workflows/ci.yml). Run via `./dev lint`
# or automatically by the pre-push hook (.githooks/pre-push). Catches the exact
# SwiftLint/SwiftFormat failures CI would, before a push burns a CI round-trip.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

ensure_project_root

CI_FILE=".github/workflows/ci.yml"

# Warn (never fail) when a locally-installed linter differs from the version CI
# pins. Brew installs float on upgrade, and a version skew is the usual cause of
# "clean locally, red in CI" (or the reverse). ci.yml is the single source of
# truth for the pins, so we read them straight from it.
check_version() {
    local tool=$1 local_version=$2 yaml_key=$3
    local pinned
    pinned=$(grep -E "${yaml_key}:" "$CI_FILE" 2>/dev/null | grep -oE "[0-9]+\.[0-9]+\.[0-9]+" | head -1 || true)
    if [[ -n "$pinned" && "$local_version" != "$pinned" ]]; then
        log_warning "$tool $local_version differs from CI-pinned $pinned — results may not match CI"
    fi
}

# Preflight: both tools must be installed.
for tool in swiftlint swiftformat; do
    if ! command_exists "$tool"; then
        log_error "$tool not found — install with: brew install $tool"
        exit 1
    fi
done

check_version "swiftlint" "$(swiftlint version)" "SWIFTLINT_VERSION"
check_version "swiftformat" "$(swiftformat --version)" "SWIFTFORMAT_VERSION"

status=0

log_info "SwiftLint (--strict, baselined)…"
if swiftlint lint --baseline lint-baseline.json --strict --quiet; then
    log_success "SwiftLint clean"
else
    log_error "SwiftLint found non-baselined violations"
    status=1
fi

log_info "SwiftFormat (--lint)…"
if swiftformat --lint . ; then
    log_success "SwiftFormat clean"
else
    log_error "SwiftFormat would reformat files — run ./dev format to fix"
    status=1
fi

if [[ $status -eq 0 ]]; then
    log_success "Lint passed (mirrors CI)"
else
    log_error "Lint failed — fix the above before pushing (CI runs the same checks)"
fi

exit $status
