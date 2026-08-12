#!/usr/bin/env bash

# Single Source of Truth for Version Information
# ==============================================
# This repo's §3 interface-contract owner per the org Versioning spec (v1.0):
# the CalVer algorithm exists exactly once, here; every consumer
# (scripts/build.sh's MARKETING_VERSION build setting, scripts/publish.sh's
# release mint) delegates or reads this script's output. Version is never
# computed anywhere else. See docs/VERSIONING.md for this repo's instance.
#
# TWO DECOUPLED NUMBERS — do not conflate them:
#
#   * Marketing version (this script, --version): CalVer that ROLLS MONTHLY.
#       Format: YYYY.M.<commits-this-month>  (non-padded month; e.g. 2026.7.3)
#       Patch = commits on main since the 1st of the current month (UTC), so
#       it resets to 1 on the 1st. Floored at 1 — never X.Y.0.
#   * Build number (scripts/get-build-number.sh, --build): the TOTAL commit
#       count — globally MONOTONIC, never resets, never date-gated. It is the
#       CFBundleVersion; a monthly-resetting counter there breaks strict
#       within-train increase (the org 2026-06-01 incident).
#
# Year/month come from `date -u` (UTC) everywhere — never the local clock.
#
# MIGRATION (§6): devcanopy previously shipped semver (last tag v0.1.1).
# semver → CalVer needs NO cutover gate: 2026.M.P strictly exceeds 0.1.1, so
# the switch is monotonic-safe mid-month. There is no legacy branch to
# preserve — this script is pure §2.
#
# §4 CANONICAL MINT (--tag) — the floor-collision fix (what2wear#170):
#   The monthly floor maps commit-count 0 and 1 to the same patch, so a
#   post-month-roll release of a prior-month commit mints vYYYY.M.1 and the
#   first real commit of the month would collide with it. `--tag` resolves
#   the FINAL version through a probe/reuse/bump ladder:
#     * probe is REMOTE-visible (`git ls-remote --tags origin`), never
#       locally-cached tags; a failed probe FAILS CLOSED (never mint blind);
#     * tag exists AND its peeled commit ($TAG^{commit}) == HEAD → reuse
#       (idempotent re-run);
#     * tag exists at a different commit → bump patch until free (the bumped
#       version IS the version);
#     * tag free → create.
#   Output contract: one resolved (version, tag, action) triple on stdout.
#   `--tag` alone is a read-only dry run; `--tag --push` also creates the
#   annotated tag and pushes it. Exactly one mint site exists in this repo:
#   scripts/publish.sh invoking `--tag --push` (§4 mode 2, local mint — the
#   CI-green pre-check lives in publish.sh).
#
# Usage:
#   bash scripts/get-version-info.sh                # JSON with all fields
#   bash scripts/get-version-info.sh --version      # 2026.7.3
#   bash scripts/get-version-info.sh --build        # 152  (total commits)
#   bash scripts/get-version-info.sh --commit       # 007b474
#   bash scripts/get-version-info.sh --full-with-sha
#   bash scripts/get-version-info.sh --tag          # §4 mint, DRY RUN
#   bash scripts/get-version-info.sh --tag --push   # §4 mint: create + push tag
#
# Replay pins / test seams (org-canonical names, §3):
#   MARKETING_VERSION=2026.7.9   → emitted verbatim (no recomputation). A pin
#                                  is NEVER auto-bumped: if the pinned tag
#                                  exists on a different commit, --tag fails.
#   BUILD_NUMBER=42              → pins --build (see get-build-number.sh).
#   VERSION_DATE_OVERRIDE=YYYY-MM-DD → pin "today" (year/month/month-start).
#   VERSION_PATCH_OVERRIDE=N     → pin the monthly patch so tests don't depend
#                                  on host git history.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Current date (UTC), honoring the VERSION_DATE_OVERRIDE test seam.
get_today() {
    if [[ -n "${VERSION_DATE_OVERRIDE:-}" ]]; then
        echo "$VERSION_DATE_OVERRIDE"
    else
        date -u +%Y-%m-%d
    fi
}

# Marketing CalVer: YYYY.M.<commits-since-the-1st-of-the-month> (UTC).
# Floored at 1 so a month with no commits yet never emits X.Y.0. NOTE: the
# floor makes count 0 and count 1 indistinguishable; resolution of the FINAL
# patch happens at the §4 mint (--tag), which bumps past any taken tag.
get_version() {
    # Replay pin (§3): consumed verbatim.
    if [[ -n "${MARKETING_VERSION:-}" ]]; then
        echo "$MARKETING_VERSION"
        return
    fi

    local today year month month_start patch
    today=$(get_today)
    year="${today%%-*}"
    month=$(echo "$today" | cut -d- -f2)
    month=$((10#$month))                       # non-padded (7, not 07)

    if [[ -n "${VERSION_PATCH_OVERRIDE:-}" ]]; then
        patch="$VERSION_PATCH_OVERRIDE"
    elif command -v git >/dev/null 2>&1 && git rev-parse --git-dir >/dev/null 2>&1; then
        month_start="${today%-*}-01T00:00:00Z"
        patch=$(git rev-list --count --since="$month_start" HEAD 2>/dev/null || echo 0)
    else
        patch=0
    fi
    [[ "$patch" = "0" ]] && patch=1

    echo "${year}.${month}.${patch}"
}

# Build number — total commit count, owned by get-build-number.sh (§3: one
# owner per capability; this is delegation, not duplication).
get_build() {
    bash "$SCRIPT_DIR/get-build-number.sh"
}

# Short commit SHA.
get_commit() {
    if command -v git >/dev/null 2>&1 && git rev-parse --git-dir >/dev/null 2>&1; then
        git rev-parse --short HEAD 2>/dev/null || echo "unknown"
    else
        echo "unknown"
    fi
}

# Full version string with SHA, e.g. "v2026.7.3 (007b474)".
get_full_version_with_sha() {
    echo "v$(get_version) ($(get_commit))"
}

# Build timestamp, ISO 8601 UTC.
get_build_time() {
    date -u +"%Y-%m-%dT%H:%M:%SZ"
}

# Probe a tag and echo the COMMIT it (peeled) points to, or "" if absent.
# Remote-visible when an `origin` remote exists: `git ls-remote --tags` asks
# the remote directly, so the probe sees tags pushed after this checkout was
# taken and never trusts locally-cached tag state (§4 requirement). Annotated
# tags are peeled via the `^{}` advertisement; `rev-parse` on an annotated
# tag would return the tag OBJECT, not the commit — never compare that.
# Falls back to local tags only when no origin remote is configured (isolated
# test fixtures / detached local repos).
# Exit: 0 on a definitive answer; non-zero if the remote probe itself failed
# (network/auth) — callers must FAIL CLOSED on that, never mint blind.
probe_tag_commit() {
    local tag="$1" out peeled plain
    if git remote get-url origin >/dev/null 2>&1; then
        out=$(git ls-remote --tags origin "refs/tags/${tag}" "refs/tags/${tag}^{}") || return 1
        if [[ -z "$out" ]]; then
            echo ""
            return 0
        fi
        peeled=$(printf '%s\n' "$out" | awk -v r="refs/tags/${tag}^{}" '$2 == r { print $1 }')
        plain=$(printf '%s\n' "$out" | awk -v r="refs/tags/${tag}" '$2 == r { print $1 }')
        # Annotated tags advertise a peeled ^{} line (the commit); lightweight
        # tags advertise only the plain line (already the commit).
        echo "${peeled:-$plain}"
        return 0
    fi
    if git rev-parse -q --verify "refs/tags/${tag}" >/dev/null 2>&1; then
        git rev-list -n1 "refs/tags/${tag}"
    else
        echo ""
    fi
}

# §4 canonical mint: resolve (version, tag, action) for HEAD, optionally
# creating + pushing the tag. Human-readable progress goes to stderr; stdout
# carries only the machine-readable output contract:
#   version=YYYY.M.P
#   tag=vYYYY.M.P
#   action=create|reuse
mint_tag() {
    local push="$1"
    local head_commit version train patch tag existing action attempts
    head_commit=$(git rev-parse "HEAD^{commit}")
    version=$(get_version)
    train="${version%.*}"
    patch="${version##*.}"

    attempts=0
    while :; do
        tag="v${version}"
        if ! existing=$(probe_tag_commit "$tag"); then
            echo "error: remote tag probe failed for $tag (network/auth?) — refusing to mint blind" >&2
            exit 1
        fi
        if [[ -z "$existing" ]]; then
            action="create"
            break
        elif [[ "$existing" = "$head_commit" ]]; then
            action="reuse"
            echo "Tag $tag already points at $head_commit — reusing (idempotent re-run)" >&2
            break
        fi
        # Collision: tag exists on a DIFFERENT commit (the month-roll floor
        # collision, or any stale tag). Bump the patch and retry — the bumped
        # version IS the version. Never bare-skip (ships a release under a
        # tag pointing at the wrong commit), never bare-fail (blocks the train).
        if [[ -n "${MARKETING_VERSION:-}" ]]; then
            echo "error: pinned MARKETING_VERSION=$MARKETING_VERSION but tag $tag exists at $existing (not $head_commit)." >&2
            echo "       A pin is consumed verbatim and never auto-bumped — pick a different pin." >&2
            exit 1
        fi
        echo "Tag $tag exists at $existing (not $head_commit) — bumping patch" >&2
        patch=$((10#$patch + 1))
        version="${train}.${patch}"
        attempts=$((attempts + 1))
        if [[ "$attempts" -gt 1000 ]]; then
            echo "error: gave up after 1000 bump attempts (tag namespace runaway?)" >&2
            exit 1
        fi
    done

    if [[ "$action" = "create" && "$push" = "true" ]]; then
        git tag -a "$tag" -m "Release $version" "$head_commit"
        if git remote get-url origin >/dev/null 2>&1; then
            git push origin "refs/tags/$tag" >&2
        fi
        echo "Created and pushed tag: $tag" >&2
    elif [[ "$action" = "create" ]]; then
        echo "Dry run: would create tag $tag at $head_commit (pass --push to mint)" >&2
    fi

    printf 'version=%s\ntag=%s\naction=%s\n' "$version" "$tag" "$action"
}

# Main execution
case "${1:-}" in
    --version)
        get_version
        ;;
    --build)
        get_build
        ;;
    --commit)
        get_commit
        ;;
    --full-with-sha)
        get_full_version_with_sha
        ;;
    --build-time)
        get_build_time
        ;;
    --tag)
        PUSH=false
        if [[ "${2:-}" = "--push" ]]; then
            PUSH=true
        elif [[ -n "${2:-}" ]]; then
            echo "usage: $0 --tag [--push]" >&2
            exit 2
        fi
        mint_tag "$PUSH"
        ;;
    "")
        # Default: JSON with all version information.
        VERSION=$(get_version)
        BUILD=$(get_build)
        COMMIT=$(get_commit)
        FULL_VERSION_WITH_SHA=$(get_full_version_with_sha)
        BUILD_TIME=$(get_build_time)

        cat <<EOF
{
  "version": "$VERSION",
  "build": $BUILD,
  "commit": "$COMMIT",
  "fullVersionWithSha": "$FULL_VERSION_WITH_SHA",
  "buildTime": "$BUILD_TIME"
}
EOF
        ;;
    *)
        echo "usage: $0 [--version|--build|--commit|--full-with-sha|--build-time|--tag [--push]]" >&2
        exit 2
        ;;
esac
