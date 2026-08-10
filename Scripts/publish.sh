#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

# Publish a release — the repo's §4 mode-2 LOCAL MINT (org Versioning spec
# v1.0; Docs/VERSIONING.md). The version is CalVer derived from git by
# Scripts/get-version-info.sh; there is no version bump, no version commit,
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
            log_error "--bump is gone: versions are CalVer minted from git (Docs/VERSIONING.md)."
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

# Resolve the Sentry DSN (issue #75). #18's integration reads the DSN from the
# SENTRY_DSN build setting (→ SentryDSN Info.plist key) and no-ops when it is
# empty; local and CI builds leave it empty on purpose, but a *release* that
# silently never reports is the bug this guards. Doppler is the source of truth
# (devcanopy/dev). Resolved here — in pre-flight, not right before build.sh — so
# a missing DSN fails before the tag is minted and pushed, not after.
# Never log the value; log only that resolution happened.
if [[ $SKIP_SENTRY -eq 1 ]]; then
    SENTRY_DSN=""
    log_warning "--skip-sentry: releasing without a DSN (Sentry will no-op in this build)."
elif [[ -n "${SENTRY_DSN:-}" ]]; then
    log_info "Using SENTRY_DSN from the environment"
else
    log_info "Resolving SENTRY_DSN from Doppler (devcanopy/dev)..."
    if ! command_exists doppler; then
        log_error "SENTRY_DSN comes from the maintainer's Doppler project and is not available to contributors."
        log_error "Build with --skip-sentry, or pre-set SENTRY_DSN in the environment."
        exit 1
    fi
    # --plain writes the value to stdout only, so doppler's stderr is safe to
    # surface — it explains *why* (not logged in, no access, no such secret).
    if ! SENTRY_DSN="$(doppler secrets get SENTRY_DSN --project devcanopy --config dev --plain)"; then
        log_error "Could not read SENTRY_DSN from Doppler (project devcanopy, config dev)."
        log_error "Maintainers: check 'doppler login' and your access. Everyone else: pass --skip-sentry."
        exit 1
    fi
    if [[ -z "$SENTRY_DSN" ]]; then
        log_error "Doppler returned an empty SENTRY_DSN (project devcanopy, config dev)."
        log_error "Set the secret in Doppler, or pass --skip-sentry."
        exit 1
    fi
    log_success "Resolved SENTRY_DSN from Doppler (value not logged)"
fi
# build.sh reads SENTRY_DSN from the environment and forwards it to xcodebuild.
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
log_info "signing/notarization/Sparkle). To cut a distributable build manually:"
log_info "  1. The Release .app was just built at:"
log_info "       build/DerivedData/Build/Products/Release/$APP_NAME.app"
log_info "  2. Zip it:  (cd build/DerivedData/Build/Products/Release && \\"
log_info "                zip -r ~/Desktop/$APP_NAME-$VERSION.zip $APP_NAME.app)"
log_info "  3. Optionally attach the zip to a GitHub Release for tag $TAG:"
log_info "       gh release create $TAG ~/Desktop/$APP_NAME-$VERSION.zip \\"
log_info "         --title \"$APP_NAME $VERSION\" --generate-notes"
log_info "  4. The build is unsigned/un-notarized — fine for local use; do NOT"
log_info "     distribute it externally until #15 lands."
