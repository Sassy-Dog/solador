#!/usr/bin/env bash
set -euo pipefail

# Build the Solador cockpit. Three shapes, deliberately different (#303):
#
#   ./dev build            plain `cargo build` — compile-only, unchanged. The
#                          fastest "does it still compile" loop.
#   ./dev build --bundle   `cargo tauri build --debug` — the same bundler the
#                          release path uses, at the debug profile's compile
#                          cost. This is what CI runs on every PR.
#   ./dev build --release  `cargo tauri build` — the RELEASE path. `./prd` is
#                          this. Produces target/release/bundle/macos/Solador.app.
#
# Why the CLI at all: the bundler is NOT in `tauri-build`. That build.rs helper
# reads `bundle.*` — deployment floor, the Info.plist embedded into the bare
# Mach-O — and assembles nothing: no Contents/MacOS, no Resources, no hdiutil
# anywhere in it. `bundle.active: true` on its own produces no artifact, which
# is why "flip the flag" was never the whole job. The bundler lives in
# `tauri-cli`, pinned by `TAURI_CLI_VERSION` in config.sh.
#
# `./dev run`, `./dev test` and `./dev lint` stay plain cargo, on purpose: the
# CLI has no test or lint command, and run.sh's `codesign --identifier
# solador-app` pinning — the thing that stops a 4+ prompt-per-run Keychain
# storm — has to keep applying to the dev loop's throwaway .app. `tauri build`
# signs as `app.solador.desktop` and moves the designated requirement.
#
# The bundle produced here is UNSIGNED, and that is correct for now: signing
# and notarization are #306, the updater is #308, the tag mint is #15.
#
# One CLI side effect to know about: `cargo tauri build` EDITS TRACKED SOURCE.
# It rewrites app/src-tauri/Cargo.toml's `tauri` / `tauri-build` entries into
# table form (`{ version = "2", features = [] }`) to keep their feature list in
# sync with tauri.conf.json. That normalized form is committed, so the rewrite
# is now a no-op and a bundle leaves the tree clean; CI asserts that with a
# `git diff --exit-code` after the build.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

ROOT_DIR="$( cd "$SCRIPT_DIR/.." && pwd )"
TAURI_CONF="$ROOT_DIR/app/src-tauri/tauri.conf.json"

CARGO_PROFILE="debug"
WANT_BUNDLE=false
BUNDLE_REQUESTED=false
PASSTHRU=()

while [[ $# -gt 0 ]]; do
    case $1 in
        --release)
            CARGO_PROFILE="release"
            # A release build IS the bundle path — there is no reason to
            # produce a bare optimized Mach-O nobody can install.
            WANT_BUNDLE=true
            shift
            ;;
        --bundle)
            WANT_BUNDLE=true
            BUNDLE_REQUESTED=true
            shift
            ;;
        *)
            PASSTHRU+=("$1")
            shift
            ;;
    esac
done

# ---------------------------------------------------------------------------
# Plain cargo (no bundle)
# ---------------------------------------------------------------------------

build_with_cargo() {
    local cargo_args=()
    [[ "$CARGO_PROFILE" == "release" ]] && cargo_args+=("--release")

    log_info "Building $TAURI_PACKAGE ($CARGO_PROFILE)..."
    cargo build --manifest-path "$ROOT_DIR/Cargo.toml" -p "$TAURI_PACKAGE" \
        ${cargo_args[@]+"${cargo_args[@]}"} ${PASSTHRU[@]+"${PASSTHRU[@]}"}

    log_success "Built $ROOT_DIR/target/$CARGO_PROFILE/$TAURI_PACKAGE"
}

# ---------------------------------------------------------------------------
# The Tauri CLI
# ---------------------------------------------------------------------------

# Version-pinned, and the check is on the *installed* version, not on presence:
# a machine carrying some other project's cargo-tauri would otherwise bundle
# with a CLI that has never been paired with this lock file.
#
# This does a user-level `cargo install` into ~/.cargo/bin, so it replaces any
# cargo-tauri already there. That is the deliberate cost of a pin — a CLI that
# drifts from the `tauri` crate in Cargo.lock is its own class of bug, and the
# CLI's own mismatch detector only fires on a mismatch it can see.
ensure_tauri_cli() {
    local installed=""

    if command_exists cargo-tauri; then
        installed="$(cargo tauri --version 2>/dev/null | awk 'NR == 1 { print $2 }')"
    fi

    if [[ "$installed" == "$TAURI_CLI_VERSION" ]]; then
        log_debug "cargo-tauri $installed already installed"
        return 0
    fi

    if [[ -n "$installed" ]]; then
        log_warning "cargo-tauri $installed found, this repo pins $TAURI_CLI_VERSION — replacing it"
    else
        log_info "cargo-tauri not found — installing the pinned $TAURI_CLI_VERSION"
    fi

    cargo install --locked "tauri-cli@$TAURI_CLI_VERSION"

    installed="$(cargo tauri --version 2>/dev/null | awk 'NR == 1 { print $2 }')"
    if [[ "$installed" != "$TAURI_CLI_VERSION" ]]; then
        log_error "cargo-tauri is '${installed:-<missing>}' after installing $TAURI_CLI_VERSION — is ~/.cargo/bin on PATH?"
        exit 1
    fi
}

# ---------------------------------------------------------------------------
# Fail-closed checks on the produced bundle
#
# Every one of these is a claim the release makes about itself. A build that
# cannot prove them is a red build, never a warning: a version that silently
# falls back to a hardcoded number is exactly the failure #303 exists to
# remove, and it looks identical to success from the outside.
# ---------------------------------------------------------------------------

assert_plist_key() {
    local key="$1" want="$2" plist="$3" got
    got="$(plutil -extract "$key" raw -o - "$plist" 2>/dev/null || true)"
    if [[ "$got" != "$want" ]]; then
        log_error "Info.plist $key is '${got:-<missing>}', expected '$want'"
        exit 1
    fi
    log_success "$key = $got"
}

# The macOS deployment floor, read back out of the Mach-O this build actually
# produced — LC_BUILD_VERSION's minos is what the linker recorded.
#
# Which source wins here is NOT the obvious one, and it was measured rather
# than reasoned about. `.cargo/config.toml`'s `[env] MACOSX_DEPLOYMENT_TARGET`
# is *shadowed* under the CLI: tauri-cli exports MACOSX_DEPLOYMENT_TARGET from
# `bundle.macOS.minimumSystemVersion` into the environment cargo inherits
# (tauri-cli 2.11.4 src/build.rs:110), and cargo's `[env]` table yields to an
# inherited value unless it declares `force = true`. tauri-build then re-emits
# the same value as `cargo:rustc-env` for the app crate itself (tauri-build
# 2.6.3 src/lib.rs:592). Measured: with config.toml set to 15.0 and the config
# left at 14.0, every crate in a CLI build still came out 14.0, while a plain
# `cargo build` of the same crate came out 15.0.
#
# So `.cargo/config.toml` governs the plain-cargo paths (`./dev test`,
# `./dev lint`, CI's Rust jobs) and `tauri.conf.json` governs this one. Both
# declare 14.0, which is exactly the no-drift arrangement config.toml's own
# comment describes — but only one of them is in force at a time.
assert_deployment_floor() {
    local binary="$1" want="$2" minos
    if ! command_exists vtool; then
        log_error "vtool not found — cannot verify the macOS deployment floor (install the Xcode command line tools)"
        exit 1
    fi
    minos="$(vtool -show-build-version "$binary" 2>/dev/null | awk '$1 == "minos" { print $2; exit }')"
    if [[ "$minos" != "$want" ]]; then
        log_error "LC_BUILD_VERSION minos is '${minos:-<missing>}', expected '$want' (MACOSX_DEPLOYMENT_TARGET did not reach the CLI build)"
        exit 1
    fi
    log_success "LC_BUILD_VERSION minos = $minos"
}

# ---------------------------------------------------------------------------
# Bundle via the Tauri CLI
# ---------------------------------------------------------------------------

build_bundle() {
    ensure_tauri_cli

    # Both numbers come from git, from their single-source scripts. Neither is
    # written down anywhere a human edits (docs/VERSIONING.md): the marketing
    # CalVer rolls monthly, the build number is the monotonic commit count.
    local marketing_version build_number
    marketing_version="$(bash "$SCRIPT_DIR/get-version-info.sh" --version)"
    build_number="$(bash "$SCRIPT_DIR/get-build-number.sh")"

    # tauri.conf.json carries NO `version` key — the version is not authored,
    # it is derived — so it arrives as a `--config` overlay instead. Tauri 2.11
    # has no environment variable for this; `--config` takes a JSON string and
    # merges it over the file, which is the supported mechanism.
    local cli_args=(--config "{\"version\":\"$marketing_version\"}")

    # macOS only, and only the .app: a .dmg needs hdiutil plus the signing
    # story that is #306's, and an unsigned .dmg is not something anyone should
    # be handed. `--bundles` names what we can actually stand behind today.
    cli_args+=(--bundles app)

    [[ "$CARGO_PROFILE" == "debug" ]] && cli_args+=(--debug)

    # productName is read back out of the config rather than assumed from
    # APP_NAME, the same way run.sh does it, so the two cannot drift.
    local product_name app_bundle plist executable min_os
    product_name="$(plutil -extract productName raw -o - "$TAURI_CONF")"
    app_bundle="$ROOT_DIR/target/$CARGO_PROFILE/bundle/macos/$product_name.app"
    plist="$app_bundle/Contents/Info.plist"

    # Delete first, so "it exists afterwards" means THIS build made it.
    #
    # Without this the checks below are worthless, and measurably so: setting
    # `bundle.active: false` made the CLI skip bundling entirely, and every
    # assertion then passed against the previous run's leftover .app —
    # `EXIT=0`, five green lines, no bundle produced. That is #269's shape
    # exactly, where a lenient fallback found a stale binary and reported
    # success. An artifact check has to be a check on *this* artifact.
    rm -rf "$app_bundle"

    log_info "Bundling $APP_NAME $marketing_version (build $build_number), $CARGO_PROFILE profile, unsigned..."

    # Run from app/src-tauri: the CLI locates tauri.conf.json relative to the
    # cwd, and `frontendDist: "../ui"` is relative to the config file.
    (
        cd "$ROOT_DIR/app/src-tauri"
        cargo tauri build "${cli_args[@]}" ${PASSTHRU[@]+"${PASSTHRU[@]}"}
    )

    if [[ ! -d "$app_bundle" ]]; then
        log_error "cargo tauri build reported success but produced no $app_bundle"
        log_error "(is bundle.active still true in app/src-tauri/tauri.conf.json?)"
        exit 1
    fi

    # CFBundleVersion. Tauri's config has exactly ONE version field, so the
    # bundler writes the marketing version into both plist keys. They are two
    # decoupled numbers here (docs/VERSIONING.md) and CFBundleVersion is the
    # machine-facing one, so stamp the build number over it.
    #
    # This MUST stay ahead of any codesign step (#306): editing Info.plist
    # after signing invalidates the signature, and the failure surfaces at
    # install time on someone else's machine.
    plutil -replace CFBundleVersion -string "$build_number" "$plist"

    assert_plist_key CFBundleShortVersionString "$marketing_version" "$plist"
    assert_plist_key CFBundleVersion "$build_number" "$plist"

    min_os="$(plutil -extract 'bundle.macOS.minimumSystemVersion' raw -o - "$TAURI_CONF")"
    assert_plist_key LSMinimumSystemVersion "$min_os" "$plist"

    executable="$app_bundle/Contents/MacOS/$(plutil -extract CFBundleExecutable raw -o - "$plist")"
    assert_deployment_floor "$executable" "$min_os"

    log_success "Bundled $app_bundle"
}

# ---------------------------------------------------------------------------

ensure_project_root

if [[ "$WANT_BUNDLE" != true ]]; then
    build_with_cargo
    exit 0
fi

if [[ "$(uname -s)" != "Darwin" ]]; then
    # Windows and Linux packaging are #15's, and neither has a bundler
    # configured here yet. Loud, never silent: a release build that quietly
    # degrades to a bare binary is a release nobody can install.
    if [[ "$BUNDLE_REQUESTED" == true ]]; then
        log_error "--bundle is macOS-only today (Windows/Linux packaging is #15) — nothing to bundle on $(uname -s)"
        exit 1
    fi
    log_warning "Bundling is macOS-only today (Windows/Linux packaging is #15) — building the bare $CARGO_PROFILE binary instead"
    build_with_cargo
    exit 0
fi

build_bundle
