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
# 2.12.1, matching `Cargo.lock`'s `tauri` 2.12.1 (`tauri-build` 2.7.1), because
# matching the CLI to the runtime is the point of pinning. `tauri-cli`
# publishes its own patch numbers, so an exact match is not guaranteed: when
# tauri 2.11.5 shipped, the CLI topped out at 2.11.4 and the same train was the
# tightest match available. The CLI errors on a real runtime/CLI mismatch by
# itself, and `--ignore-version-mismatches` is deliberately never passed so
# that check keeps its teeth.
#
# Bump this and `Cargo.lock`'s `tauri` together, never one alone. That is
# enforced (#485): `scripts/tauri-cli-pin-guard.sh` fails `./dev lint` and CI
# unless the two share major.minor (a patch difference passes, per the above).
# It reads the line below as text, so keep it a plain `export TAURI_CLI_VERSION="X.Y.Z"`.
export TAURI_CLI_VERSION="2.12.1"

# The ShellCheck both `./dev lint` and CI's ShellCheck step run (#548). Before
# the pin CI used whatever the ubuntu-latest image carried (0.9.0) and a Mac
# used Homebrew's (0.11.0), so a script could lint clean locally and fail CI.
# `scripts/shellcheck-pin-guard.sh` fails `./dev lint` on any other version;
# CI downloads exactly this release and checks the tarball against the SHA-256
# below (the release publishes no checksum file, so the pin carries its own):
# `shellcheck-v<version>.linux.x86_64.tar.xz`. Bump both together. Both are read
# as text, so keep them plain `export NAME="..."` lines.
export SHELLCHECK_VERSION="0.11.0"
export SHELLCHECK_SHA256_LINUX_X86_64="8c3be12b05d5c177a04c29e3c78ce89ac86f1595681cab149b65b97c4e227198"

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
# under target/<triple>/, so build-agent.sh and release-agent.yml must agree on
# where each binary landed.
#
# musl, NOT gnu, for Linux. A dynamically linked gnu build resolves the
# builder's glibc and dies on any older host with `GLIBC_2.xx not found` — a
# failure invisible to us and fatal to a stranger's first install. The agent
# reads /proc and shells out, so static linking costs it nothing.
export AGENT_PACKAGE="solador-agent"
export AGENT_LINUX_TARGETS="x86_64-unknown-linux-musl aarch64-unknown-linux-musl"
export AGENT_MACOS_TARGETS="aarch64-apple-darwin x86_64-apple-darwin"

# The last combined release (#490, part of #472): the newest `v*` tag whose
# release carried the agent's binaries, from the days when every desktop tag was
# also an agent release (#390). The bridge, `v2026.10.14`, published 2026-10-02,
# is that release — the first one built by the consumer that finds releases
# through `agent-latest`, and the last that `release.yml`'s old agent leg
# produced (the leg moved to `release-agent.yml`, on `agent-v*` tags, in #491).
#
# It records HISTORY; it is never a version anyone chooses. The agent's own
# versions are `agent-vYYYY.M.N` tags (docs/VERSIONING.md), and this is the only
# place the legacy numbering is known. Two readers, both in
# `scripts/get-version-info.sh`:
#   * `--agent-tag` counts it among the versions already SHIPPED, so the first
#     agent release in the bridge's own month numbers above it and no later
#     release can sort at or below a release that went out;
#   * `--agent-version` builds a source build's `<base>+dev.<k>.g<sha>` from it
#     until an `agent-v*` tag is reachable.
#
# Read as TEXT by get-version-info.sh (which must not source this whole file),
# so keep it a plain `export LAST_COMBINED_AGENT_RELEASE="vYYYY.M.N"` line.
# Moving it is a decision about history, not a bump: only if a later `v*` release
# with agent assets was cut before the agent leg left `release.yml` (the lookup
# is `gh release view <tag> --json assets`, newest `v*` tag whose assets include
# `solador-agent-*`).
export LAST_COMBINED_AGENT_RELEASE="v2026.10.14"

# The agent's own macOS floor, and it is DELIBERATELY not the cockpit's.
#
# `.cargo/config.toml` declares `MACOSX_DEPLOYMENT_TARGET = "14.0"` for the
# workspace, justified there by the frontend's `adoptedStyleSheets` requirement.
# The agent has no frontend and no webview; inheriting that number would publish
# a binary advertised for "Intel Macs" that an Intel Mac on macOS 12 or 13
# cannot execute — a floor arrived at by accident, which is the same mistake
# #335 caught in the bundle's architecture.
#
# 11.0 (Big Sur) is the lowest release both published architectures share:
# aarch64-apple-darwin does not exist below it. `[env]` in a cargo config yields
# to an inherited value, so exporting this from build-agent.sh is what puts it
# in force — measured back out of every artifact with `vtool`, per slice, rather
# than assumed.
export AGENT_MACOS_MIN_VERSION="11.0"

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

# The minisign signer for the agent's published binaries (#390) and for
# `agent-latest.json` (#391), through the one implementation in
# scripts/agent-signing.sh. `rsign2` is minisign's Rust implementation by the
# same author, so the `.minisig` files it writes are ordinary minisign
# signatures — verifiable today with the `minisign` CLI by anyone who downloads
# a release, and by the release and feed workflows themselves, which re-check
# every signature against the committed public key before uploading it.
#
# The agent VERIFIES these signatures itself since #393: `solador-agent update`
# checks the feed and the downloaded binary under the public key(s) its
# `build.rs` compiles in from the two files named below — `minisign-verify`,
# the same crate the producer uses, never a copy of this signer.
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
# signatures nobody can check), and by anyone who downloads a binary. It is
# also the trust root `crates/updatefeed::agent` verifies the feed's inputs
# under, and the file — not any prose — is the authority on the key's id.
#
# Its sibling `agent/release-signing-key-next.pub` is the STANDBY (#393): a
# second key the agent's updater also trusts, provisioned by
# `scripts/agent-standby-key.sh` into a Doppler config that syncs nowhere, so
# a lost or compromised active key is a release rather than a recall. The
# release signs under THIS file only; the standby's name is deliberately not a
# variable here, because nothing in the release path may reach for it.
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
# Apple distributes. Unset never blocks anything — it only stops `./dev run`
# narrowing its choice of Apple Development identity to one team — so a
# contributor can still build.
export DEVELOPMENT_TEAM="${DEVELOPMENT_TEAM:-}"

# Load local overrides if they exist
if [[ -f "scripts/config.local.sh" ]]; then
    source "scripts/config.local.sh"
fi
