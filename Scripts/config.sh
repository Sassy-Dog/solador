#!/usr/bin/env bash

# Solador Configuration
export APP_NAME="Solador"
export BUNDLE_IDENTIFIER="com.sassydog.devcanopy"
export MINIMUM_MACOS_VERSION="14.0"

# Version is NOT configured here (org Versioning spec §3/§10: no hand-maintained
# version fields). Both numbers derive from git via their single-source scripts:
#   marketing version → Scripts/get-version-info.sh   (CalVer YYYY.M.<commits-this-month>)
#   build number      → Scripts/get-build-number.sh   (total commit count, monotonic)
# See Docs/VERSIONING.md.

# Build configurations
export CONFIGURATIONS=("Debug" "Release")
export DEFAULT_CONFIGURATION="Debug"

# Directories
export BUILD_DIR="build"
export DERIVED_DATA_PATH="$BUILD_DIR/DerivedData"
export PRODUCTS_DIR="$BUILD_DIR/Products"

# Xcode settings
export SCHEME_NAME="Solador"
export PROJECT_NAME="DevCanopy.xcodeproj"

# Signing.
#
# The Apple team id is deliberately NOT stored in this repository — it used to
# sit in `project.yml`, which goes stale silently. It arrives from the
# environment instead: `.envrc.local` locally, `secrets.*` in a release
# workflow.
#
# It is not a secret either: a team id ships in the signature of every binary
# Apple distributes. Unset is a warning, never a wall — a contributor must
# still be able to build.
export DEVELOPMENT_TEAM="${DEVELOPMENT_TEAM:-}"
export CODE_SIGN_IDENTITY=""

# Reports on DEVELOPMENT_TEAM. There is nothing to resolve: it is read from the
# environment, and that is the entire contract.
#
# Locally, direnv exports it (see `.envrc`). In CI, a release workflow sets it
# from `secrets.*`. Neither is this script's business, and that is the point —
# a build script that knows where a value *comes from* has to be edited every
# time that changes, and every contributor has to learn the answer.
resolve_development_team() {
    [[ -n "$DEVELOPMENT_TEAM" ]] && return 0
    log_warning "DEVELOPMENT_TEAM is unset — letting Xcode pick a signing team."
    log_warning "Set it in .envrc.local if you need a specific one (see .envrc)."
    return 0
}

# Terminal support: the name→bundle-identifier mapping lives in the app
# (Views/Settings/SettingsView.swift, detectAvailableTerminals). It's intentionally
# not duplicated here as a bash associative array — that required bash 4+ and broke
# under macOS's stock bash 3.2 (e.g. CI runners).

# Default terminal (will detect installed ones)
export DEFAULT_TERMINAL="Terminal"

# API Configuration (placeholders - will be set via environment or UI)
export GITHUB_CLIENT_ID=""
export VERCEL_CLIENT_ID=""

# Feature flags
export ENABLE_DEBUG_MENU=1
export ENABLE_PERFORMANCE_OVERLAY=0

# Load local overrides if they exist
if [[ -f "Scripts/config.local.sh" ]]; then
    source "Scripts/config.local.sh"
fi