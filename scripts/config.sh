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

# Every macOS bundle is universal (#335), and the triple lives here because it
# is part of the OUTPUT PATH: `--target` moves cargo's output under the triple,
# so build.sh, publish.sh and the workflows must all agree on where the bundle
# landed. v2026.8.110 shipped arm64-only for want of this flag; a second copy of
# the string drifting from the first is how the artifact goes missing instead.
export MACOS_UNIVERSAL_TARGET="universal-apple-darwin"

# The Windows release target (#341), named for the same output-path reason as
# the universal triple above: build.sh passes it to `cargo tauri build
# --target`, which moves the bundle under target/<triple>/, so build.sh and
# release.yml must agree on where the installer landed. Explicit rather than
# inherited from the builder — v2026.8.110 shipped the wrong architecture
# because no target was named, and "whatever the runner happens to be" is not
# a release decision.
export WINDOWS_TARGET="x86_64-pc-windows-msvc"

# The per-host metrics agent's published targets (#390), named here for the same
# output-path reason as the two triples above: `--target` moves cargo's output
# under target/<triple>/, so build-agent.sh and release.yml must agree on where
# each binary landed.
#
# musl, NOT gnu, for Linux. A dynamically linked gnu build resolves the
# builder's glibc and dies on any older host with `GLIBC_2.xx not found` — a
# failure invisible to us and fatal to a stranger's first install. The agent
# reads /proc and shells out, so static linking costs it nothing.
export AGENT_PACKAGE="solador-agent"
export AGENT_LINUX_TARGETS="x86_64-unknown-linux-musl aarch64-unknown-linux-musl"
export AGENT_MACOS_TARGETS="aarch64-apple-darwin x86_64-apple-darwin"

# The cross-linker for the Linux targets (#390, resolving the design's open
# item): zig, driven by cargo-zigbuild. Chosen over `cross` because it needs no
# Docker on the runner, is faster per target, and musl is precisely its
# strength.
#
# Installed from PyPI rather than crates.io — deliberately. The `cargo-zigbuild`
# wheel depends on `ziglang`, so ONE pin brings the linker and its driver at a
# combination the publisher tested together; `cargo install cargo-zigbuild` pins
# only half of that and leaves the zig version to whatever is on PATH.
export CARGO_ZIGBUILD_VERSION="0.23.4"

# The minisign signer for the agent's published binaries (#390). `rsign2` is
# minisign's Rust implementation by the same author, so the `.minisig` files it
# writes are ordinary minisign signatures — verifiable with the `minisign` CLI
# by anyone who downloads a release, and with `minisign-verify` (already in this
# workspace, via crates/updatefeed) by the agent itself.
#
# NOT the Tauri signer that produces the app's `.sig`. That one wraps minisign
# in an extra base64 layer of its own and, more to the point, carries the app's
# key: the agent's keypair is deliberately separate, so a compromise of one
# cannot yield the other. The app updates on a laptop; the agent runs unattended
# as a service on servers, which is the higher-value target.
export RSIGN_VERSION="0.6.6"

# The public half of that keypair, committed so the signature is checkable —
# both by CI, which verifies every artifact against this file before uploading
# it (a mis-provisioned private key fails the release rather than shipping
# signatures nobody can check), and by anyone who downloads a binary.
export AGENT_SIGNING_PUBKEY="agent/release-signing-key.pub"

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
