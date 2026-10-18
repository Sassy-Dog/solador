#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

# `./dev clean` clears cargo's target directory, which is where every build
# this repo makes lands. Cargo never prunes it: each dependency version it has
# compiled keeps its own artifacts, so a stream of Dependabot bumps grew one
# checkout's target/debug to 19 GB while the last build was a fraction of that.
#
# `--over-budget` cleans only when target/ is larger than TARGET_BUDGET_GB
# (scripts/config.sh) and otherwise reports the size and leaves it. The budget
# sits well above one debug plus one release build, so the latest build always
# survives and only the accumulation is removed. tidy-repo runs this form
# (.claude/sassy-dog/tidy-repo.md).

usage() {
    echo "Usage: ./dev clean [--over-budget]" >&2
}

OVER_BUDGET=false
for arg in "$@"; do
    case "$arg" in
        --over-budget) OVER_BUDGET=true ;;
        -h|--help) usage; exit 0 ;;
        *) log_error "Unknown option: $arg"; usage; exit 2 ;;
    esac
done

REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
TARGET_DIR="$REPO_DIR/target"

# Kilobytes, so the comparison stays integral; 0 when there is nothing there.
target_kb() {
    if [[ -d "$TARGET_DIR" ]]; then
        du -sk "$TARGET_DIR" | cut -f1
    else
        echo 0
    fi
}

human() {
    awk -v kb="$1" 'BEGIN { printf "%.1f GB", kb / 1048576 }'
}

BEFORE_KB="$(target_kb)"
BUDGET_KB=$((TARGET_BUDGET_GB * 1048576))

if [[ "$OVER_BUDGET" == true ]] && ((BEFORE_KB <= BUDGET_KB)); then
    log_info "target/ is $(human "$BEFORE_KB"), within the ${TARGET_BUDGET_GB} GB budget; leaving it"
    exit 0
fi

log_info "Cleaning build artifacts (target/ is $(human "$BEFORE_KB"))"

(cd "$REPO_DIR" && cargo clean)

# The pre-Tauri build directory, still ignored and still possibly on disk.
if [[ -d "$REPO_DIR/$BUILD_DIR" ]]; then
    log_info "Removing $BUILD_DIR/..."
    rm -rf "${REPO_DIR:?}/$BUILD_DIR"
fi

if [[ -d "$REPO_DIR/tests/frontend/test-results" ]]; then
    rm -rf "$REPO_DIR/tests/frontend/test-results"
fi

log_success "Clean complete: freed $(human $((BEFORE_KB - $(target_kb))))"
