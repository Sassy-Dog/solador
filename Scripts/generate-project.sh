#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

log_info "Generating Xcode project with XcodeGen"

# Check if XcodeGen is installed
if ! command_exists xcodegen; then
    log_error "XcodeGen is not installed. Please install it with: brew install xcodegen"
    exit 1
fi

# Check if project.yml exists
if [[ ! -f "project.yml" ]]; then
    log_error "project.yml not found. Cannot generate Xcode project."
    exit 1
fi

# Generate project
xcodegen generate

if [[ $? -eq 0 ]]; then
    log_success "Xcode project generated successfully"
else
    log_error "Failed to generate Xcode project"
    exit 1
fi