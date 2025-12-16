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

# Ensure clean working tree
ensure_clean_working_tree

# Ensure on main branch
ensure_main_branch

# Run tests unless skipped
if [[ $SKIP_TESTS -eq 0 ]]; then
    log_info "Running tests..."
    "$SCRIPT_DIR/test.sh"
fi

# Calculate new version
OLD_VERSION="$VERSION"
NEW_VERSION=$(increment_version "$OLD_VERSION" "$BUMP_TYPE")

log_info "Bumping version: $OLD_VERSION → $NEW_VERSION ($BUMP_TYPE)"

# Update version in config.sh
sed -i '' "s/export VERSION=\"$OLD_VERSION\"/export VERSION=\"$NEW_VERSION\"/" "$SCRIPT_DIR/config.sh"

# Build release version
log_info "Building release version..."
"$SCRIPT_DIR/build.sh" --release

# Commit version change
git add "$SCRIPT_DIR/config.sh"
git commit -m "chore: bump version to $NEW_VERSION"

# Create and push tag
TAG="v$NEW_VERSION"
git tag -a "$TAG" -m "Release $NEW_VERSION"

log_info "Pushing to origin..."
git push origin main
git push origin "$TAG"

log_success "Published $APP_NAME $NEW_VERSION"
log_info "Tag: $TAG"
log_info "Next steps:"
log_info "  1. GitHub Actions will build and create a release"
log_info "  2. Download the built app from GitHub Releases"
log_info "  3. Notarize if needed for distribution"