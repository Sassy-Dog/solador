#!/usr/bin/env bash
set -euo pipefail

# Build the Tauri cockpit binary from the root Rust workspace.
#
# This compiles the app; it does not *package* one. Producing a distributable
# .app/.dmg -- bundle activation, the version derived from git rather than
# tauri.conf.json's hardcoded 0.1.0, signing, notarization and the update
# appcast -- is issue #15, and nothing here should pretend to do it.
#
# `./dev run` builds and launches (and wraps the binary in a throwaway .app so
# macOS gives it an icon and stable Keychain ACLs); this is the build-only path
# for CI-shaped checks and for `./prd`.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

ROOT_DIR="$( cd "$SCRIPT_DIR/.." && pwd )"
CARGO_PROFILE="debug"
CARGO_ARGS=()

while [[ $# -gt 0 ]]; do
    case $1 in
        --release)
            CARGO_PROFILE="release"
            CARGO_ARGS+=("--release")
            shift
            ;;
        *)
            CARGO_ARGS+=("$1")
            shift
            ;;
    esac
done

log_info "Building $TAURI_PACKAGE ($CARGO_PROFILE)..."
cargo build --manifest-path "$ROOT_DIR/Cargo.toml" -p "$TAURI_PACKAGE" \
    ${CARGO_ARGS[@]+"${CARGO_ARGS[@]}"}

log_success "Built $ROOT_DIR/target/$CARGO_PROFILE/$TAURI_PACKAGE"
