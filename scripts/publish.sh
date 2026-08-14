#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

# Releasing is not implemented.
#
# This script's CalVer minting and CI-verification pre-flight are sound and are
# kept as the scaffold for issue #15, which owns the release train.
#
# What EXISTS as of #303: `./dev build --release` produces a real
# `Solador.app` through `cargo tauri build`, stamped with the derived CalVer
# and build number. What is still missing is everything that makes a bundle
# distributable -- code signing and notarization (#306) and the update
# mechanism (#308). An unsigned, unnotarized .app is not a release; Gatekeeper
# refuses it on every machine that did not build it.
#
# Refusing HERE is deliberate -- before the tag is minted. The previous flow
# pushed a CalVer tag and only then built, which on a repo that cannot yet
# produce a *distributable* bundle would leave a permanent tag advertising a
# release that does not exist.
log_error "Releasing is not implemented yet -- see issue #15."
log_error "Bundling landed in #303, but signing/notarization (#306) and updates (#308) have not."
log_error "Build an unsigned bundle locally with: ./dev build --release"
exit 1

# Publish a release — the repo's §4 mode-2 LOCAL MINT (org Versioning spec
# v1.0; docs/VERSIONING.md). The version is CalVer derived from git by
# scripts/get-version-info.sh; there is no version bump, no version commit,
# and nothing to keep in sync — the old semver --bump flow and the
# config.sh/project.yml verified-in-sync duplicate pair are gone (issue #98).
#
# Flow: pre-flight guards → CI-green check → Sentry DSN → tests → mint
# (probe/reuse/bump ladder, creates + pushes the vYYYY.M.P tag) → Release build
# stamped with the MINTED version. The tag lands before the build on purpose: if
# the build fails, re-running publish reuses the same tag idempotently (§4
# same-commit reuse) — the same order a CI mint would use.

# Default values
SKIP_TESTS=0
SKIP_SENTRY=0

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --skip-tests)
            SKIP_TESTS=1
            shift
            ;;
        --skip-sentry)
            SKIP_SENTRY=1
            shift
            ;;
        --bump)
            log_error "--bump is gone: versions are CalVer minted from git (docs/VERSIONING.md)."
            exit 1
            ;;
        *)
            log_error "Unknown option: $1"
            exit 1
            ;;
    esac
done

log_info "Publishing $APP_NAME"

# --- Pre-flight guards: fail before mutating anything ---------------------

# Ensure clean working tree
ensure_clean_working_tree

# Ensure on main branch
ensure_main_branch

# Verify local main is up to date with origin BEFORE minting — never tag a
# stale local HEAD.
log_info "Checking local main is up to date with origin..."
git fetch origin main
LOCAL_HEAD="$(git rev-parse HEAD)"
REMOTE_HEAD="$(git rev-parse origin/main)"
if [[ "$LOCAL_HEAD" != "$REMOTE_HEAD" ]]; then
    if git merge-base --is-ancestor "$REMOTE_HEAD" "$LOCAL_HEAD"; then
        log_error "Local main is ahead of origin/main (unpushed commits). Push them first."
    else
        log_error "Local main is not up to date with origin/main."
        log_error "Run 'git pull --ff-only' and re-run publish."
    fi
    exit 1
fi

# §4 mode 2: a local mint MUST verify default-branch CI is green before
# tagging. Fail closed — no gh, no verdict, no run found ⇒ no mint.
log_info "Verifying CI is green for $LOCAL_HEAD..."
if ! command_exists gh; then
    log_error "gh CLI is required to verify CI before minting (brew install gh)."
    exit 1
fi
CI_GREEN_COUNT="$(gh run list --workflow CI --commit "$LOCAL_HEAD" \
    --json status,conclusion \
    --jq '[.[] | select(.status == "completed" and .conclusion == "success")] | length')" || {
    log_error "Could not query CI runs for $LOCAL_HEAD (gh error). Refusing to mint blind."
    exit 1
}
if [[ "${CI_GREEN_COUNT:-0}" -lt 1 ]]; then
    log_error "No green 'CI' workflow run found for $LOCAL_HEAD."
    log_error "Wait for CI on main to pass (gh run list --workflow CI --commit $LOCAL_HEAD) and re-run."
    exit 1
fi
log_success "CI is green for HEAD"

# Resolve the Sentry DSN (issue #75).
#
# UNREACHABLE TODAY, and the consumer it was written for is gone. This script
# hard-exits above ("Releasing is not implemented"), so nothing below runs; and
# #18's integration — which is what read this DSN — lived in the macOS app that
# was deleted. It took the value from a SENTRY_DSN *build setting* forwarded by
# xcodebuild into a SentryDSN Info.plist key. This repo builds with plain cargo
# and has no Sentry SDK at all (no `sentry` crate in any manifest, no panic
# hook), so today this block reads an environment variable that reaches nothing.
#
# Kept, not deleted, because #15 owns the release path and must decide: either
# re-implement opt-in reporting for Tauri (#18's decision, and CLAUDE.md commits
# to "no telemetry or analytics by default"), or drop this gate. Do not read the
# guard below as evidence that releases currently report crashes.
#
# The shape is still the one #15 should keep: the environment is the source of
# truth, and it is checked here — in pre-flight — so a missing DSN fails before
# the tag is minted and pushed, not after. Never log the value.
if [[ $SKIP_SENTRY -eq 1 ]]; then
    SENTRY_DSN=""
    log_warning "--skip-sentry: releasing without a DSN (Sentry will no-op in this build)."
elif [[ -n "${SENTRY_DSN:-}" ]]; then
    log_info "Using SENTRY_DSN from the environment"
else
    log_error "SENTRY_DSN is not set, so this release would ship without crash reporting."
    log_error "Locally: put it in .envrc.local and let direnv export it (see .envrc)."
    log_error "In CI: set it from the workflow's secrets."
    log_error "Or pass --skip-sentry to release deliberately without one."
    exit 1
fi
# Exported for whatever #15's release build ends up being. `scripts/build.sh`
# does NOT read it today — it is a plain `cargo build` and references neither
# SENTRY_DSN nor xcodebuild.
export SENTRY_DSN

# Run tests unless skipped
if [[ $SKIP_TESTS -eq 0 ]]; then
    log_info "Running tests..."
    "$SCRIPT_DIR/test.sh"
fi

# --- Mint (§4): resolve the collision-free version and push the tag -------

log_info "Minting release tag (probe/reuse/bump ladder)..."
MINT_OUTPUT="$("$SCRIPT_DIR/get-version-info.sh" --tag --push)"
VERSION="$(printf '%s\n' "$MINT_OUTPUT" | sed -n 's/^version=//p')"
TAG="$(printf '%s\n' "$MINT_OUTPUT" | sed -n 's/^tag=//p')"
ACTION="$(printf '%s\n' "$MINT_OUTPUT" | sed -n 's/^action=//p')"
if [[ -z "$VERSION" || -z "$TAG" || -z "$ACTION" ]]; then
    log_error "Mint output contract violated (got: $MINT_OUTPUT)"
    exit 1
fi
log_success "Minted $TAG ($ACTION)"

# --- Build: every consumer reads the minted output (§4) -------------------

# Pin the minted version for the build so the artifact is stamped with exactly
# the version the tag carries — build.sh must not re-resolve it (the re-derive
# would diverge the day the bump ladder first fires).
log_info "Building release version $VERSION..."
MARKETING_VERSION="$VERSION" "$SCRIPT_DIR/build.sh" --release

log_success "Published $APP_NAME $VERSION"
log_info "Tag: $TAG"
log_info ""
log_info "There is no automated release pipeline yet (see issue #15 for"
log_info "signing/notarization and the update feed). To cut a build manually:"
log_info "  1. The Release .app was just built at:"
log_info "       build/DerivedData/Build/Products/Release/$APP_NAME.app"
log_info "  2. Zip it:  (cd build/DerivedData/Build/Products/Release && \\"
log_info "                zip -r ~/Desktop/$APP_NAME-$VERSION.zip $APP_NAME.app)"
log_info "  3. Optionally attach the zip to a GitHub Release for tag $TAG:"
log_info "       gh release create $TAG ~/Desktop/$APP_NAME-$VERSION.zip \\"
log_info "         --title \"$APP_NAME $VERSION\" --generate-notes"
log_info "  4. The build is unsigned/un-notarized — fine for local use; do NOT"
log_info "     distribute it externally until #15 lands."
