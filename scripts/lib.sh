#!/usr/bin/env bash
set -euo pipefail

# Color output functions
color_red() { printf "\033[31m%s\033[0m\n" "$1"; }
color_green() { printf "\033[32m%s\033[0m\n" "$1"; }
color_yellow() { printf "\033[33m%s\033[0m\n" "$1"; }
color_blue() { printf "\033[34m%s\033[0m\n" "$1"; }
color_cyan() { printf "\033[36m%s\033[0m\n" "$1"; }
color_gray() { printf "\033[90m%s\033[0m\n" "$1"; }

# Logging functions with emoji
log_info() { echo "$(color_blue "ℹ️  $1")"; }
log_success() { echo "$(color_green "✅ $1")"; }
log_warning() { echo "$(color_yellow "⚠️  $1")"; }
log_error() { echo "$(color_red "❌ $1")"; }
log_debug() { 
    if [[ "${DEBUG:-0}" == "1" ]]; then
        echo "$(color_gray "🔍 $1")"
    fi
}

# Check if command exists
command_exists() {
    command -v "$1" >/dev/null 2>&1
}

# Ensure we're in the project root
ensure_project_root() {
    if [[ ! -f "Cargo.toml" || ! -d "app/src-tauri" ]]; then
        log_error "Not in Solador project root directory"
        exit 1
    fi
}

# Check for clean git working tree
ensure_clean_working_tree() {
    if ! git diff --quiet || ! git diff --staged --quiet; then
        log_error "Working tree is not clean. Please commit or stash changes."
        exit 1
    fi
}

# Get current git branch
get_current_branch() {
    git rev-parse --abbrev-ref HEAD
}

# Check if on main branch
ensure_main_branch() {
    local current_branch
    current_branch=$(get_current_branch)
    if [[ "$current_branch" != "main" ]]; then
        log_error "Not on main branch (current: $current_branch)"
        exit 1
    fi
}

# Versioning lives in dedicated single-source scripts (docs/VERSIONING.md, org
# Versioning spec §3) — NOT here:
#   scripts/get-version-info.sh   marketing CalVer + the §4 release mint (--tag)
#   scripts/get-build-number.sh   build number (total commit count, --at <ref>)
# The old semver helpers (parse_version / increment_version) and the inline
# build-number counter were removed with the CalVer adoption (issue #98).

# Export functions for use in other scripts
export -f color_red color_green color_yellow color_blue color_cyan color_gray
export -f log_info log_success log_warning log_error log_debug
export -f command_exists ensure_project_root ensure_clean_working_tree
export -f get_current_branch ensure_main_branch