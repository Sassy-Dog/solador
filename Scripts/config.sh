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
# The Apple team id is deliberately NOT stored in this repository. Org
# convention (company CLAUDE.md, "Shared vendor sources of truth") puts it in
# Doppler `_stores/apple` as TEAM_ID and states that consumer repos must not
# keep their own copy — `project.yml` used to be exactly such a copy.
#
# It is not a secret: a team id ships in the signature of every binary Apple
# distributes. The ladder below is about a single source of truth, not
# confidentiality, which is why an environment override comes first and a
# missing Doppler is a warning rather than a wall — a contributor must still be
# able to build.
export DEVELOPMENT_TEAM="${DEVELOPMENT_TEAM:-}"
export CODE_SIGN_IDENTITY=""

# Resolves DEVELOPMENT_TEAM, once, on demand.
#
# A function rather than a top-level lookup because config.sh is sourced by
# every script including `./dev` — paying a Doppler round-trip to run the tests
# would be absurd. Callers that actually sign (build.sh) invoke this; nothing
# else does.
resolve_development_team() {
    [[ -n "$DEVELOPMENT_TEAM" ]] && return 0

    if ! command -v doppler >/dev/null 2>&1; then
        log_warning "No DEVELOPMENT_TEAM set and no doppler CLI — letting Xcode choose a team."
        log_warning "Maintainers: install doppler. Everyone else: set DEVELOPMENT_TEAM, or ignore this."
        return 0
    fi
    # --plain writes the value to stdout only, so doppler's stderr is safe to
    # surface: it explains *why* (not logged in, no access, no such secret).
    if ! DEVELOPMENT_TEAM="$(doppler secrets get TEAM_ID --project devcanopy --config dev --plain 2>/dev/null)"; then
        DEVELOPMENT_TEAM=""
        log_warning "Could not read TEAM_ID from Doppler — letting Xcode choose a team."
        return 0
    fi
    export DEVELOPMENT_TEAM
    [[ -n "$DEVELOPMENT_TEAM" ]] && log_info "Resolved the Apple team id from Doppler"
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