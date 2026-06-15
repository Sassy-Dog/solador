#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

# Default values
BUMP_TYPE="patch"
SKIP_TESTS=0

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --bump)
            BUMP_TYPE="$2"
            shift 2
            ;;
        --skip-tests)
            SKIP_TESTS=1
            shift
            ;;
        *)
            log_error "Unknown option: $1"
            exit 1
            ;;
    esac
done

log_info "Publishing $APP_NAME"

PROJECT_YML="$SCRIPT_DIR/../project.yml"

# Read the MARKETING_VERSION declared in project.yml (the fallback used by
# Xcode-direct builds). Scripts/build.sh overrides it from config.sh's VERSION,
# but the two sources must agree so every build path reports the same version.
get_project_yml_version() {
    sed -n 's/^[[:space:]]*MARKETING_VERSION:[[:space:]]*//p' "$PROJECT_YML" | head -1 | tr -d '"'
}

# --- Pre-flight guards: fail before mutating anything ---------------------

# Ensure clean working tree
ensure_clean_working_tree

# Ensure on main branch
ensure_main_branch

# Verify the two version sources already agree. If they've drifted, abort and
# let a human reconcile — a publish must not paper over an existing mismatch.
PROJECT_YML_VERSION="$(get_project_yml_version)"
if [[ "$PROJECT_YML_VERSION" != "$VERSION" ]]; then
    log_error "Version sources disagree before publish:"
    log_error "  Scripts/config.sh VERSION       = $VERSION"
    log_error "  project.yml MARKETING_VERSION   = $PROJECT_YML_VERSION"
    log_error "Reconcile these (they must match) and re-run."
    exit 1
fi

# Verify local main is up to date with origin BEFORE mutating. A rejected push
# after we've committed and tagged would strand a local-only tag and bump commit.
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

# Run tests unless skipped
if [[ $SKIP_TESTS -eq 0 ]]; then
    log_info "Running tests..."
    "$SCRIPT_DIR/test.sh"
fi

# Calculate new version
OLD_VERSION="$VERSION"
NEW_VERSION=$(increment_version "$OLD_VERSION" "$BUMP_TYPE")

log_info "Bumping version: $OLD_VERSION → $NEW_VERSION ($BUMP_TYPE)"

# --- Mutate: bump BOTH version sources in lockstep ------------------------

# Update version in config.sh (source for scripted builds)
sed -i '' "s/export VERSION=\"$OLD_VERSION\"/export VERSION=\"$NEW_VERSION\"/" "$SCRIPT_DIR/config.sh"

# Update MARKETING_VERSION in project.yml (fallback for Xcode-direct builds)
sed -i '' "s/^\([[:space:]]*MARKETING_VERSION:[[:space:]]*\)$OLD_VERSION$/\1$NEW_VERSION/" "$PROJECT_YML"

# Confirm both rewrites landed and now agree.
NEW_CONFIG_VERSION="$(sed -n 's/^export VERSION="\(.*\)"$/\1/p' "$SCRIPT_DIR/config.sh" | head -1)"
NEW_PROJECT_VERSION="$(get_project_yml_version)"
if [[ "$NEW_CONFIG_VERSION" != "$NEW_VERSION" || "$NEW_PROJECT_VERSION" != "$NEW_VERSION" ]]; then
    log_error "Version bump did not apply cleanly to both sources:"
    log_error "  Scripts/config.sh VERSION     = $NEW_CONFIG_VERSION"
    log_error "  project.yml MARKETING_VERSION = $NEW_PROJECT_VERSION"
    log_error "  expected                      = $NEW_VERSION"
    log_error "Working tree left modified for inspection; nothing was committed."
    exit 1
fi

# Build release version
log_info "Building release version..."
"$SCRIPT_DIR/build.sh" --release

# Commit version change
git add "$SCRIPT_DIR/config.sh" "$PROJECT_YML"
git commit -m "chore: bump version to $NEW_VERSION"

# Create the annotated tag locally.
TAG="v$NEW_VERSION"
git tag -a "$TAG" -m "Release $NEW_VERSION"

# Push main FIRST. Only push the tag after main lands, so a rejected main push
# never strands a remote tag pointing at an unpushed commit.
log_info "Pushing main to origin..."
if ! git push origin main; then
    log_error "Pushing main failed. The bump commit and tag '$TAG' remain LOCAL ONLY."
    log_error "Recover with:  git reset --hard origin/main && git tag -d $TAG"
    log_error "Then re-run publish once main is reconciled."
    exit 1
fi

log_info "Pushing tag $TAG to origin..."
git push origin "$TAG"

log_success "Published $APP_NAME $NEW_VERSION"
log_info "Tag: $TAG"
log_info ""
log_info "There is no automated release pipeline yet (see issue #15 for"
log_info "signing/notarization/Sparkle). To cut a distributable build manually:"
log_info "  1. The Release .app was just built at:"
log_info "       build/DerivedData/Build/Products/Release/$APP_NAME.app"
log_info "  2. Zip it:  (cd build/DerivedData/Build/Products/Release && \\"
log_info "                zip -r ~/Desktop/$APP_NAME-$NEW_VERSION.zip $APP_NAME.app)"
log_info "  3. Optionally attach the zip to a GitHub Release for tag $TAG:"
log_info "       gh release create $TAG ~/Desktop/$APP_NAME-$NEW_VERSION.zip \\"
log_info "         --title \"$APP_NAME $NEW_VERSION\" --generate-notes"
log_info "  4. The build is unsigned/un-notarized — fine for local use; do NOT"
log_info "     distribute it externally until #15 lands."