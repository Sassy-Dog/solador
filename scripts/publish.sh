#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

# Local release: mint the tag, then build a signed, notarized, stapled .dmg.
#
# The blanket refusal that used to sit here is gone as of #306, because the
# thing it was refusing over now exists: `scripts/build.sh --sign --notarize`
# produces a distributable artifact, and this script's pre-flight is what makes
# minting a tag for it safe.
#
# **This mints and pushes a real tag.** There is no dry run by accident — use
# `--skip-mint` to exercise the build path without one (below).
#
# What is still NOT here, so nobody reads a green run as more than it is:
#   * #307 — release.yml, the same thing on CI against the `prd` environment.
#     Today this is a local-mint-only path (§4 mode 2).
#   * #308 — the update feed. A user who installs this .dmg has no way to be
#     told about the next one; they re-download it.
#   * #15  — the release train that ties those together, plus Windows/Linux.
#
# Signing and notarization live in build.sh, not here, and that is deliberate:
# they have to run AFTER the CFBundleVersion stamp (editing Info.plist breaks
# the signature), and putting them beside the stamp is what keeps the ordering
# reviewable. It also means the whole distributable path is exercisable with
# `./dev build --release --notarize` — no tag, no mint, no release.

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
SKIP_MINT=0

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
        --skip-mint)
            # Exercise the whole signed-and-notarized build WITHOUT minting or
            # pushing a tag. This is how #306's path was verified end to end
            # without cutting the repo's first release: everything below runs,
            # the version comes from the same derivation the mint would have
            # used, and no permanent tag is created.
            SKIP_MINT=1
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
# **This block reaches a real consumer as of #309**, and #306 is where that was
# resolved. It described a dead path for one release cycle and no longer does:
# the note that used to sit here — "no `sentry` crate in any manifest, no panic
# hook, reads an environment variable that reaches nothing" — was true when #277
# wrote it and stopped being true when `crates/crashreport` landed.
#
# The consumer is `crashreport::BUILD_DSN`, which is `option_env!("SENTRY_DSN")`
# — read at COMPILE time, from the environment cargo inherits, which is what the
# `export` below provides. It is deliberately not read at runtime: the SDK's own
# `apply_defaults` would happily take a `SENTRY_DSN` out of the user's shell,
# and an environment variable that can redirect this app's crash reports to a
# third party is not something to leave lying around.
#
# The gate is kept, not because a DSN-less build is broken — it is a silent
# no-op, and that is the common case for a local build — but because a
# *release* that cannot receive a report the operator opted into is a release
# nobody can debug. `--skip-sentry` is the deliberate way past it.
#
# A DSN still reports nothing on its own: crash reporting is opt-in and off in
# a fresh store (#18, #309). This makes reports possible, not automatic.
#
# Checked in pre-flight, so a missing DSN fails before the tag is minted and
# pushed rather than after. Never log the value.
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
# Exported so the compile below inherits it. build.sh never names SENTRY_DSN;
# it does not have to — `option_env!` reads the environment cargo was invoked
# with, and crashreport's build.rs declares the rerun dependency so changing the
# value actually rebuilds the crate rather than reusing a cached one.
export SENTRY_DSN

# Run tests unless skipped
if [[ $SKIP_TESTS -eq 0 ]]; then
    log_info "Running tests..."
    "$SCRIPT_DIR/test.sh"
fi

# --- Mint (§4): resolve the collision-free version and push the tag -------

if [[ $SKIP_MINT -eq 1 ]]; then
    # Same derivation the mint would have started from, so the artifact is
    # stamped exactly as a real release would stamp it — it simply carries no
    # tag. Never let this branch invent a version of its own.
    VERSION="$("$SCRIPT_DIR/get-version-info.sh" --version)"
    TAG=""
    ACTION="skipped"
    log_warning "--skip-mint: building $VERSION without minting or pushing a tag"
else
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
fi

# --- Build: every consumer reads the minted output (§4) -------------------

# Pin the minted version for the build so the artifact is stamped with exactly
# the version the tag carries — build.sh must not re-resolve it (the re-derive
# would diverge the day the bump ladder first fires).
log_info "Building release version $VERSION..."
MARKETING_VERSION="$VERSION" "$SCRIPT_DIR/build.sh" --release --notarize

log_success "Published $APP_NAME $VERSION"
if [[ -n "$TAG" ]]; then
    log_info "Tag: $TAG"
else
    log_warning "No tag: --skip-mint was passed, so this artifact is not a release."
fi

DMG="$(ls -t "$SCRIPT_DIR/../target/release/bundle/dmg/"*.dmg 2>/dev/null | head -1)"
log_info ""
log_info "Signed, notarized and stapled:"
log_info "  ${DMG:-<no .dmg found — check the build output above>}"
log_info ""
log_info "There is still no automated release pipeline (#307) and no update feed"
log_info "(#308), so distribution is manual and one-way:"
log_info "  1. Verify on a machine that did NOT build it — stapling is what makes"
log_info "     that check work offline, and it is the only honest test of it."
if [[ -n "$TAG" ]]; then
    log_info "  2. Attach it to a GitHub Release for $TAG:"
    log_info "       gh release create $TAG \"
    log_info "         \"${DMG:-<dmg>}\" \"
    log_info "         --title \"$APP_NAME $VERSION\" --generate-notes"
fi
log_info "  3. Anyone who installs it has no upgrade path until #308 lands."
