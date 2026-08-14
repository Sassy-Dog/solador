#!/usr/bin/env bash

# Solador configuration.
#
# Deliberately small. Everything the Xcode build needed -- scheme, bundle id,
# deployment target, derived-data paths, signing identity -- left with the
# original macOS app. The Tauri build reads its own values from
# `app/src-tauri/tauri.conf.json`, which is the single source for the bundle's
# identity; duplicating any of it here is how the two drift.

export APP_NAME="Solador"

# The cargo package for the cockpit binary, in the root workspace.
export TAURI_PACKAGE="solador-app"

# The Tauri CLI the release path bundles with (#303).
#
# The bundler is NOT in `tauri-build`: that build.rs helper reads `bundle.*`
# (deployment floor, embedded Info.plist) but assembles no `.app` — no
# Contents/MacOS, no hdiutil. Bundling lives in the CLI, which is a separate
# crate on a separate release train, so it has to be named and pinned here.
#
# 2.11.4, NOT 2.11.5. `Cargo.lock` resolves `tauri` to 2.11.5 and `tauri-build`
# to 2.6.3, and matching the CLI to the runtime is the point of pinning — but
# `tauri-cli` publishes its own patch numbers and **2.11.5 does not exist**
# (`https://index.crates.io/ta/ur/tauri-cli` tops out at 2.11.4). Same 2.11
# train is the tightest match available; the CLI errors on a real
# runtime/CLI mismatch by itself, and `--ignore-version-mismatches` is
# deliberately never passed so that check keeps its teeth.
#
# Bump this and `Cargo.lock`'s `tauri` together, never one alone.
export TAURI_CLI_VERSION="2.11.4"

# Version is NOT configured here (org Versioning spec §3/§10: no hand-maintained
# version fields). Both numbers derive from git via their single-source scripts:
#   marketing version → scripts/get-version-info.sh   (CalVer YYYY.M.<commits-this-month>)
#   build number      → scripts/get-build-number.sh   (total commit count, monotonic)
# See docs/VERSIONING.md.

# Directories
export BUILD_DIR="build"

# Signing.
#
# The Apple team id is deliberately NOT stored in this repository. It arrives
# from the environment instead: `.envrc.local` locally, `secrets.*` in a release
# workflow.
#
# It is not a secret either: a team id ships in the signature of every binary
# Apple distributes. Unset is a warning, never a wall — a contributor must
# still be able to build.
export DEVELOPMENT_TEAM="${DEVELOPMENT_TEAM:-}"

# Load local overrides if they exist
if [[ -f "scripts/config.local.sh" ]]; then
    source "scripts/config.local.sh"
fi
