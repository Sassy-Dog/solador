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

# Signing (will be configured later)
export DEVELOPMENT_TEAM=""
export CODE_SIGN_IDENTITY=""

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