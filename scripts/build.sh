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
#                          this. Produces
#                          target/universal-apple-darwin/release/bundle/macos/Solador.app.
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
# The bundle is unsigned by DEFAULT and signed on request: `--sign` applies the
# Developer ID identity, `--notarize` adds the .dmg, Apple's verdict and the
# staple (#306). Both are off unless asked for, so a contributor without the
# certificate still gets a bundle. `--sign` also produces the updater payload —
# a minisigned `.app.tar.gz` (#308) — because that file is a copy of the signed
# .app and there is no honest version of it before the signature. The release
# train is #15.
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
WANT_SIGN=false
WANT_NOTARIZE=false
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
        --notarize)
            # Implies --sign: there is nothing to notarize about an unsigned
            # bundle, and Apple rejects one.
            WANT_NOTARIZE=true
            WANT_SIGN=true
            WANT_BUNDLE=true
            shift
            ;;
        --sign)
            # Off by default, and deliberately not implied by --release: a
            # contributor without the certificate must still be able to produce
            # a bundle. `scripts/publish.sh` is what turns it on (#306).
            WANT_SIGN=true
            WANT_BUNDLE=true
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

# Every bundle is universal (#335). The first release ever cut, v2026.8.110,
# was arm64-only — no `--target` was passed, so the bundle inherited whatever
# the builder happened to be, and a `macos-*-arm64` runner produced something
# an Intel Mac cannot execute. The bundle's own floor is macOS 14.0, which
# Intel Macs run, so "it built fine" and "our users can run it" were two
# different claims.
#
# `universal-apple-darwin` is a Tauri CLI pseudo-target: it builds both real
# triples and lipos them, which is why BOTH have to be installed. The triple
# itself is `MACOS_UNIVERSAL_TARGET` from config.sh — it is part of the output
# path, so publish.sh has to agree with this file about it.
MACOS_UNIVERSAL_ARCHS=(x86_64 arm64)

ensure_universal_targets() {
    local triple
    for triple in aarch64-apple-darwin x86_64-apple-darwin; do
        if rustup target list --installed 2>/dev/null | grep -qx "$triple"; then
            log_debug "rust target $triple already installed"
            continue
        fi
        log_info "Installing rust target $triple (needed for $MACOS_UNIVERSAL_TARGET)"
        rustup target add "$triple"
    done
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
#
# Checked PER ARCHITECTURE, which a universal binary makes load-bearing (#335).
# `vtool -show-build-version` on a fat binary prints one block per slice, and
# this used to take the first `minos` and stop — so a build where one slice
# carried a different floor read as clean. That is the thin-binary failure in
# another costume: verifying one slice of two and reporting on the whole.
assert_deployment_floor() {
    local binary="$1" want="$2" arch minos
    if ! command_exists vtool; then
        log_error "vtool not found — cannot verify the macOS deployment floor (install the Xcode command line tools)"
        exit 1
    fi
    for arch in "${MACOS_UNIVERSAL_ARCHS[@]}"; do
        minos="$(vtool -arch "$arch" -show-build-version "$binary" 2>/dev/null \
                 | awk '$1 == "minos" { print $2; exit }')"
        if [[ "$minos" != "$want" ]]; then
            log_error "LC_BUILD_VERSION minos is '${minos:-<missing>}' for $arch, expected '$want' (MACOSX_DEPLOYMENT_TARGET did not reach the CLI build)"
            exit 1
        fi
        log_success "LC_BUILD_VERSION minos = $minos ($arch)"
    done
}

# The binary must actually be fat. Passing `--target universal-apple-darwin` is
# not evidence that it worked — a thin binary passes every other check in this
# file identically, ships, and fails only on a user's Intel Mac. Same standard
# as the version numbers: derive, then assert it back out of the artifact.
assert_universal_binary() {
    local binary="$1" got want
    if ! command_exists lipo; then
        log_error "lipo not found — cannot verify the binary is universal (install the Xcode command line tools)"
        exit 1
    fi
    # Sorted so the comparison does not depend on lipo's ordering.
    got="$(lipo -archs "$binary" 2>/dev/null | tr ' ' '\n' | sort | tr '\n' ' ' | sed 's/ $//')"
    want="$(printf '%s\n' "${MACOS_UNIVERSAL_ARCHS[@]}" | sort | tr '\n' ' ' | sed 's/ $//')"
    if [[ "$got" != "$want" ]]; then
        log_error "binary architectures are '${got:-<none>}', expected '$want'"
        log_error "(a thin binary here means Intel Macs get something they cannot run — #335)"
        exit 1
    fi
    log_success "architectures = $got"
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

    # Universal, on every bundling path including CI's debug one (#335). CI
    # builds the same shape the release builds for the same reason #303 put the
    # bundler in CI at all: a path only exercised at release time is a path
    # where "all green" carries no information.
    ensure_universal_targets
    cli_args+=(--target "$MACOS_UNIVERSAL_TARGET")

    [[ "$CARGO_PROFILE" == "debug" ]] && cli_args+=(--debug)

    # productName is read back out of the config rather than assumed from
    # APP_NAME, the same way run.sh does it, so the two cannot drift.
    local product_name bundle_dir app_bundle plist executable min_os
    product_name="$(plutil -extract productName raw -o - "$TAURI_CONF")"
    # `--target` moves cargo's output under the triple, so the bundle is no
    # longer at target/<profile>/bundle. Derived from the same variable passed
    # to the CLI so the two cannot drift into a "reported success but produced
    # no bundle" failure.
    bundle_dir="$ROOT_DIR/target/$MACOS_UNIVERSAL_TARGET/$CARGO_PROFILE/bundle"
    app_bundle="$bundle_dir/macos/$product_name.app"
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
    # Order matters: prove both slices exist BEFORE asserting a floor for each
    # of them, so a thin binary fails saying "not universal" rather than
    # "missing minos for arm64", which reads like a toolchain problem.
    assert_universal_binary "$executable"
    assert_deployment_floor "$executable" "$min_os"

    if [[ "$WANT_SIGN" == true ]]; then
        sign_app "$app_bundle" "$plist" "$marketing_version" "$build_number"
    fi

    if [[ "$WANT_NOTARIZE" == true ]]; then
        local dmg identity
        identity="$(resolve_signing_identity)" || exit 1
        dmg="$bundle_dir/dmg/${product_name}-${marketing_version}.dmg"
        mkdir -p "$(dirname "$dmg")"
        make_dmg "$app_bundle" "$dmg" "$identity"
        notarize_and_staple "$dmg"
        # The ticket Apple just issued covers every signed item in the
        # submission, the .app inside the .dmg included — so it can be stapled
        # into the .app too, which is what the updater ships.
        staple_app "$app_bundle"
    fi

    # The updater payload (#308), LAST — after the stamp, after the signature,
    # after the staple. See `make_updater_archive` for why the Tauri bundler
    # does not make this file.
    if [[ "$WANT_SIGN" == true ]]; then
        make_updater_archive "$app_bundle" "$marketing_version" "$build_number"
    fi

    log_success "Bundled $app_bundle"
}

# ---------------------------------------------------------------------------
# Signing (#306)
# ---------------------------------------------------------------------------

# The identity is resolved from the keychain by PREFIX, not configured.
#
# A certificate's full name carries the team id, and its SHA-1 is per-machine;
# pinning either in the repo makes a second machine's build fail for a reason
# that reads like a code problem. `APPLE_SIGNING_IDENTITY` overrides for CI,
# where the keychain is built on the fly.
#
# `Developer ID Application` and nothing else: `Apple Distribution` is the Mac
# App Store lane and notarization refuses it, and this repo distributes
# directly (#15). The name Gatekeeper shows is derived by Apple from the team's
# legal entity, so it reads "Sassy Dog Enterprises LLC" and cannot be made to
# read anything else.
SIGNING_IDENTITY_PREFIX="Developer ID Application"

resolve_signing_identity() {
    if [[ -n "${APPLE_SIGNING_IDENTITY:-}" ]]; then
        printf '%s' "$APPLE_SIGNING_IDENTITY"
        return 0
    fi

    local matches count
    matches="$(security find-identity -v -p codesigning 2>/dev/null \
        | sed -n "s/.*\"\(${SIGNING_IDENTITY_PREFIX}[^\"]*\)\".*/\1/p")"
    count="$(printf '%s' "$matches" | grep -c . || true)"

    case "$count" in
        1) printf '%s' "$matches" ;;
        0)
            log_error "No '$SIGNING_IDENTITY_PREFIX' certificate in the keychain."
            log_error "Install the team's Developer ID cert, or set APPLE_SIGNING_IDENTITY."
            return 1
            ;;
        *)
            # Ambiguous is refused, never resolved by picking the first. Signing
            # with the wrong identity produces an artifact that installs fine
            # here and is rejected on every machine that is not this one.
            log_error "$count '$SIGNING_IDENTITY_PREFIX' certificates match — refusing to guess:"
            printf '%s\n' "$matches" | sed 's/^/    /' >&2
            log_error "Set APPLE_SIGNING_IDENTITY to the one you mean."
            return 1
            ;;
    esac
}

# Sign the bundle, and prove the signature covers the stamped Info.plist.
#
# **This runs last, after the CFBundleVersion stamp, and the order is not a
# preference.** Info.plist is sealed by the signature, so editing it afterwards
# yields `invalid Info.plist (plist or signature have been modified)` — measured,
# not assumed. That is also why Tauri's own signing is left off: the CLI signs
# during bundling, which is before this script has stamped anything.
sign_app() {
    local app="$1" plist="$2" marketing_version="$3" build_number="$4"

    local identity
    identity="$(resolve_signing_identity)" || exit 1
    log_info "Signing $app"
    log_info "  identity: $identity"

    # --options runtime is the hardened runtime, which notarization requires.
    # --timestamp is a trusted timestamp, without which the signature stops
    # validating the day the certificate expires rather than surviving it.
    if ! codesign --force --options runtime --timestamp \
            --sign "$identity" "$app"; then
        log_error "codesign failed"
        exit 1
    fi

    if ! codesign --verify --strict --verbose=2 "$app" 2>&1 | sed 's/^/    /'; then
        log_error "the signature does not verify"
        exit 1
    fi

    # The ordering guard. If the stamp is ever moved after this call, these two
    # assertions still pass — the plist really does say the right thing — and
    # the verify above is what fails instead. Both are kept so the report names
    # which half broke.
    assert_plist_key CFBundleShortVersionString "$marketing_version" "$plist"
    assert_plist_key CFBundleVersion "$build_number" "$plist"

    # Assert the hardened runtime made it in, rather than trusting the flag we
    # passed: an unhardened bundle notarizes as a failure, minutes later, with
    # a message that does not mention this step.
    #
    # Captured, then matched — NOT piped into `grep -q`. Under `set -o pipefail`
    # grep exits at the first match, codesign takes SIGPIPE, and the pipeline
    # reports failure for a properly hardened bundle. Measured: this check
    # rejected a signature whose flags were `0x10000(runtime)`.
    local sig_flags
    sig_flags="$(codesign -d --verbose=2 "$app" 2>&1 || true)"
    case "$sig_flags" in
        *"flags="*"runtime"*) ;;
        *)
            log_error "the signature carries no hardened runtime — notarization would reject it"
            exit 1
            ;;
    esac

    log_success "Signed and hardened: $identity"
}

# ---------------------------------------------------------------------------
# DMG, notarization, staple (#306)
# ---------------------------------------------------------------------------

# The .dmg is built HERE rather than by `cargo tauri build --bundles dmg`, for
# the same ordering reason signing is: the CLI would package the app it just
# bundled, which is before the CFBundleVersion stamp and before the signature.
# A .dmg wrapping an unsigned, unstamped .app is not the artifact we ship.
make_dmg() {
    local app="$1" dmg="$2" identity="$3"

    rm -f "$dmg"
    log_info "Building $dmg"
    # UDZO is compressed and read-only. No fancy window layout: that is what
    # Tauri's dmg bundler adds, and we cannot use it (above).
    if ! hdiutil create -volname "$APP_NAME" -srcfolder "$app" \
            -ov -format UDZO "$dmg" >/dev/null; then
        log_error "hdiutil failed to build the disk image"
        exit 1
    fi

    # The .dmg is signed too. Notarization staples to the container that is
    # actually distributed, and Gatekeeper checks the container a user
    # double-clicks — an unsigned .dmg around a signed .app still warns.
    # No --options runtime here: the hardened runtime is a property of
    # executable code, and a disk image is not.
    if ! codesign --force --timestamp --sign "$identity" "$dmg"; then
        log_error "could not sign the disk image"
        exit 1
    fi
    log_success "Built and signed $dmg"
}

# Submit to Apple, wait for the verdict, staple it into the artifact.
#
# Credentials are the **ASC API key**, never an Apple ID plus app-specific
# password: the key is revocable on its own, scoped, and carries no account
# password. Names are the org-canonical `APPLE_ASC_*` (CLAUDE.md); the value
# lives in Doppler `_stores/apple` and reaches this script through the
# environment, never through the repo.
notarize_and_staple() {
    local dmg="$1"

    local missing=()
    [[ -z "${APPLE_ASC_KEY_ID:-}" ]]     && missing+=(APPLE_ASC_KEY_ID)
    [[ -z "${APPLE_ASC_ISSUER_ID:-}" ]]  && missing+=(APPLE_ASC_ISSUER_ID)
    [[ -z "${APPLE_ASC_KEY_BASE64:-}" ]] && missing+=(APPLE_ASC_KEY_BASE64)
    if (( ${#missing[@]} )); then
        log_error "notarization needs the App Store Connect API key: ${missing[*]} unset"
        log_error "source it from Doppler _stores/apple (see docs/SECRETS.md)"
        exit 1
    fi

    # The .p8 exists only as a file because notarytool takes a path. 0600, in a
    # private temp dir, removed on every exit path including a failed submit.
    local keydir keyfile
    keydir="$(mktemp -d)"
    keyfile="$keydir/asc.p8"
    trap 'rm -rf "$keydir"' EXIT
    ( umask 077; printf '%s' "$APPLE_ASC_KEY_BASE64" | base64 --decode > "$keyfile" )
    if [[ ! -s "$keyfile" ]]; then
        log_error "APPLE_ASC_KEY_BASE64 did not decode to a key"
        exit 1
    fi

    log_info "Submitting $dmg to Apple (this waits for the verdict)…"
    # --wait turns "submitted" into "accepted or rejected". Without it the
    # staple below runs against a ticket Apple has not issued yet and fails
    # with a message about the ticket, not about the submission.
    if ! xcrun notarytool submit "$dmg" \
            --key "$keyfile" \
            --key-id "$APPLE_ASC_KEY_ID" \
            --issuer "$APPLE_ASC_ISSUER_ID" \
            --wait --timeout 30m; then
        log_error "notarization was rejected — run 'xcrun notarytool log <id>' with the submission id above"
        exit 1
    fi

    rm -rf "$keydir"
    trap - EXIT

    # Staple, so the artifact validates offline. Without it Gatekeeper has to
    # reach Apple on first launch, and a user opening it on a plane sees the
    # unidentified-developer refusal.
    if ! xcrun stapler staple "$dmg"; then
        log_error "could not staple the notarization ticket"
        exit 1
    fi

    # Verify against the artifact, not against our own record of what we did.
    if ! xcrun stapler validate "$dmg"; then
        log_error "the stapled ticket does not validate"
        exit 1
    fi
    # `--type install` is the disk-image assessment; the default (`execute`)
    # answers a different question and passes on things Gatekeeper would block.
    if ! spctl -a -vvv --type install "$dmg" 2>&1 | sed 's/^/    /'; then
        log_error "Gatekeeper rejects the disk image"
        exit 1
    fi

    log_success "Notarized and stapled: $dmg"
}

# Staple the notarization ticket into the .app as well as the .dmg.
#
# The .dmg is what a human downloads; the .app inside it is what the *updater*
# ships (as a tarball, below), and `stapler staple <dmg>` staples the disk image
# only. A ticket is looked up by the code signature's cdhash, and the notary
# service issues one covering every signed item in the submission — so the .app
# that was inside the .dmg we just submitted has a ticket waiting, and this is
# the call that fetches and attaches it.
#
# Without it an updated app has to reach Apple on first launch to be assessed.
# That is exactly the machine-on-a-plane failure the .dmg's staple exists to
# prevent, arriving through the other door.
staple_app() {
    local app="$1"

    log_info "Stapling the notarization ticket into $(basename "$app")"
    if ! xcrun stapler staple "$app"; then
        log_error "could not staple the ticket into the .app"
        log_error "(the .dmg is notarized, so the ticket exists — this is a lookup, not a submission)"
        exit 1
    fi
    if ! xcrun stapler validate "$app"; then
        log_error "the .app's stapled ticket does not validate"
        exit 1
    fi
    log_success "Stapled $(basename "$app")"
}

# ---------------------------------------------------------------------------
# The updater payload (#308)
# ---------------------------------------------------------------------------

# Build and minisign the `.app.tar.gz` the in-app updater downloads.
#
# **Why this is not `bundle.createUpdaterArtifacts: true`.** That flag does
# produce a `.app.tar.gz` plus a `.sig` — but it produces them *inside*
# `cargo tauri build`, from the `.app` as it exists at that moment, which is
# before this script stamps CFBundleVersion, before it signs, and before it
# staples. The tarball would therefore carry a DIFFERENT artifact from the one
# in the .dmg: same release, same name, `CFBundleVersion` still holding the
# marketing version, no staple. A mislabelled artifact is the failure
# `release.yml`'s tag assertion exists to prevent, and shipping it as the update
# payload would put it on every machine rather than in one download.
#
# It is the same ordering argument the .dmg already lost: `make_dmg` uses
# `hdiutil` here rather than `--bundles dmg` because the CLI would package the
# app before the stamp. This is that argument a second time, and the answer is
# the same one.
#
# The flag has a second cost worth recording: with it set, `tauri-cli` **errors**
# when `TAURI_SIGNING_PRIVATE_KEY` is unset ("A public key has been found, but
# no private key"), which would fail CI's unsigned bundle job on every PR.
#
# The mechanism is unchanged from what #308 specified — minisign, through the
# pinned Tauri CLI's own signer, with the key already in `release.yml`'s job
# env. Only the moment moves.
make_updater_archive() {
    local app="$1" marketing_version="$2" build_number="$3"

    local product out_dir archive
    product="$(basename "$app" .app)"
    out_dir="$(dirname "$app")"
    # Versioned like the .dmg. The updater does not care what the file is
    # called — it reads the URL out of `latest.json` — but a release whose two
    # artifacts are named on different schemes is a release someone has to
    # decode.
    archive="$out_dir/${product}-${marketing_version}.app.tar.gz"

    if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
        log_error "the updater payload needs the minisign signing key: TAURI_SIGNING_PRIVATE_KEY unset"
        log_error "source it from Doppler solador/prd (#305), or drop --sign to build an unsigned bundle"
        exit 1
    fi
    # Empty rather than unset, deliberately. `cargo tauri signer sign` reads
    # this from the environment; with it *unset* and an encrypted key, the
    # minisign crate falls back to prompting on a tty — which on a runner means
    # the job hangs until it times out instead of failing. An empty password
    # against an encrypted key fails immediately and says so.
    export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"

    # Delete first, so "it exists afterwards" means THIS build made it — the
    # same reason `build_bundle` removes the .app before bundling.
    rm -f "$archive" "$archive.sig"

    log_info "Building $archive"
    # The archive's single top-level entry must be `<product>.app`: the plugin
    # extracts every entry with its FIRST path component stripped and renames
    # the result over the installed bundle (tauri-plugin-updater 2.10.1,
    # `install_inner`). `-C` to the parent is what produces that shape.
    #
    # `--no-mac-metadata` / COPYFILE_DISABLE: without them bsdtar adds an
    # AppleDouble `._` sibling for every file carrying an extended attribute,
    # which the plugin would faithfully extract into the installed bundle.
    # Tauri's own tar (the Rust `tar` crate) writes no xattrs at all, so this
    # keeps the archive byte-comparable with what the bundler would have made.
    if ! ( cd "$out_dir" && COPYFILE_DISABLE=1 tar --no-mac-metadata -czf "$archive" "$product.app" ); then
        log_error "could not build the updater archive"
        exit 1
    fi

    # Assert the archive really contains THIS build's app, not a leftover.
    # Reading the version back out of the artifact is the same standard
    # `assert_plist_key` holds the bundle to — and here it also proves the
    # ordering, because an archive made before the stamp carries the marketing
    # version in CFBundleVersion and would fail this line.
    local extracted archived_version archived_build
    extracted="$(mktemp -d)"
    if ! tar -xzf "$archive" -C "$extracted" "$product.app/Contents/Info.plist"; then
        log_error "the updater archive has no $product.app/Contents/Info.plist — wrong archive shape"
        rm -rf "$extracted"
        exit 1
    fi
    archived_version="$(plutil -extract CFBundleShortVersionString raw -o - "$extracted/$product.app/Contents/Info.plist" 2>/dev/null || true)"
    archived_build="$(plutil -extract CFBundleVersion raw -o - "$extracted/$product.app/Contents/Info.plist" 2>/dev/null || true)"
    rm -rf "$extracted"
    if [[ "$archived_version" != "$marketing_version" || "$archived_build" != "$build_number" ]]; then
        log_error "the updater archive carries version '${archived_version:-<missing>}' build '${archived_build:-<missing>}', expected '$marketing_version' / '$build_number'"
        log_error "(the archive was made before the CFBundleVersion stamp — see this function's header)"
        exit 1
    fi
    # And that the signature travelled with it. `_CodeSignature/CodeResources`
    # is a plain file inside the bundle, so tar carries it; its absence means
    # the archived app is unsigned and every install would be a downgrade in
    # trust.
    if ! tar -tzf "$archive" | grep -q "^$product.app/Contents/_CodeSignature/CodeResources$"; then
        log_error "the updater archive carries no code signature"
        exit 1
    fi
    log_success "Archived $marketing_version (build $archived_build), signed"

    # minisign, through the pinned CLI's own signer — the same code path
    # `createUpdaterArtifacts` would have used, and the same key. Output
    # discarded: it prints the signature (public, but noise) and the key's
    # location.
    if ! cargo tauri signer sign "$archive" >/dev/null; then
        log_error "could not sign the updater archive with the minisign key"
        exit 1
    fi
    if [[ ! -s "$archive.sig" ]]; then
        log_error "the signer reported success but produced no $archive.sig"
        exit 1
    fi

    log_success "Updater payload: $archive (+ .sig)"
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
