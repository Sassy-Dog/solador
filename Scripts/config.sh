#!/usr/bin/env bash

# DevCanopy Configuration
export APP_NAME="DevCanopy"
export BUNDLE_IDENTIFIER="com.sassydog.devcanopy"
export VERSION="0.1.0"
export MINIMUM_MACOS_VERSION="14.0"

# Build configurations
export CONFIGURATIONS=("Debug" "Release")
export DEFAULT_CONFIGURATION="Debug"

# Directories
export BUILD_DIR="build"
export DERIVED_DATA_PATH="$BUILD_DIR/DerivedData"
export PRODUCTS_DIR="$BUILD_DIR/Products"

# Xcode settings
export SCHEME_NAME="DevCanopy"
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