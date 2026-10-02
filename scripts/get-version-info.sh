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
# MIGRATION (§6): this repo previously shipped semver (last tag v0.1.1).
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
#   annotated tag and pushes it. Exactly two mint sites exist in this repo, and
#   both are scripts/publish.sh (§4 mode 2, local mint — the CI-green pre-check
#   lives in publish.sh): `--tag --push` for the cockpit (`./dev publish`), and
#   `--agent-tag --push` for the agent (`./dev publish --agent`, #490). The
#   agent mint is below; it shares the output contract and nothing else.
#
# THE AGENT'S OWN NUMBER (#490, part of #472) — derived here too, because the
# algorithm has exactly one home:
#
#   * Agent version (--agent-version): the version of THIS checkout's agent.
#       - a shallow clone cannot count <k> below, so it yields NOTHING (exit 1,
#         a reason on stderr), exactly as the cockpit's build refuses one;
#       - an `agent-v*` tag at HEAD → that tag's version (`agent-v2026.11.1`
#         → `2026.11.1`), the highest if several;
#       - otherwise a SOURCE build: `<base>+dev.<k>.g<sha>`, where <base> is
#         the highest `agent-v*` tag reachable from HEAD — before one exists,
#         `LAST_COMBINED_AGENT_RELEASE` (scripts/config.sh) — <k> is the
#         number of commits since it and <sha> is HEAD's abbreviated commit.
#         `+dev` is how a source build never claims to be a release: the
#         agent's own updater answers it "no applicable release" (exit 4).
#   * Agent mint (--agent-tag): `agent-vYYYY.M.N`, UTC. Unlike the cockpit's
#     CalVer the patch is NOT a commit count — it is a release counter, so an
#     agent release exists only when someone cuts one:
#       1. REUSE an `agent-v*` tag already at HEAD (`action=reuse`). This is
#          what lets scripts/assert-release-tag.sh --agent re-run the mint and
#          get the tag under test back instead of the next number;
#       2. otherwise N = 1 + the highest patch among THIS month's shipped
#          agent versions (1 in a month with none). Shipped means the
#          `agent-v*` tags on the REMOTE (`git ls-remote --tags origin`, never
#          local tags — a local tag is not a release) plus
#          `LAST_COMBINED_AGENT_RELEASE`. A shipped tag cannot be at HEAD by
#          now (step 1 took it), so "excluding HEAD's" needs no code;
#       3. REFUSE a result that does not sort strictly above EVERY shipped
#          version (a future-month tag, a skewed clock), and REFUSE a name
#          whose `agent-v…` RELEASE exists (`gh release view`): a deleted tag
#          drops out of step 2's max, and re-minting its number would publish
#          different bytes under a version some host already runs — which
#          then sits at exit 4 forever, the hash differing while the version
#          is not newer. Never delete an `agent-v*` tag.
#     A failed remote probe FAILS CLOSED, and so does a missing `origin`: the
#     cockpit mint falls back to local tags there, this one does not, because
#     its whole premise is the remote's list. Concurrent mints are serialised
#     by the remote — the second push of one tag is rejected.
#     Output contract: version=YYYY.M.N / tag=agent-vYYYY.M.N / action=create|reuse.
#
# Usage:
#   bash scripts/get-version-info.sh                # JSON with all fields
#   bash scripts/get-version-info.sh --version      # 2026.7.3
#   bash scripts/get-version-info.sh --build        # 152  (total commits)
#   bash scripts/get-version-info.sh --commit       # 007b474
#   bash scripts/get-version-info.sh --full-with-sha
#   bash scripts/get-version-info.sh --tag          # §4 mint, DRY RUN
#   bash scripts/get-version-info.sh --tag --push   # §4 mint: create + push tag
#   bash scripts/get-version-info.sh --agent-version      # 2026.10.14+dev.3.g1a2b3c4
#   bash scripts/get-version-info.sh --agent-tag          # agent mint, DRY RUN
#   bash scripts/get-version-info.sh --agent-tag --push   # agent mint: create + push tag
#
# Replay pins / test seams (org-canonical names, §3):
#   MARKETING_VERSION=2026.7.9   → emitted verbatim (no recomputation). A pin
#                                  is NEVER auto-bumped: if the pinned tag
#                                  exists on a different commit, --tag fails.
#   BUILD_NUMBER=42              → pins --build (see get-build-number.sh).
#   VERSION_DATE_OVERRIDE=YYYY-MM-DD → pin "today" (year/month/month-start).
#   VERSION_PATCH_OVERRIDE=N     → pin the monthly patch so tests don't depend
#                                  on host git history.
#   LAST_COMBINED_AGENT_RELEASE=vYYYY.M.N → the agent's legacy base (#490).
#                                  Read as text from scripts/config.sh unless
#                                  the environment already carries it (which
#                                  `source scripts/config.sh` arranges, and
#                                  the tests use to stage the bridge).
#
# AGENT_MARKETING_VERSION (the agent's build pin) is NOT read here: the build
# plumbing — crates/buildversion and scripts/build-agent.sh — consumes it before
# this script is asked, and `--agent-tag` honours no pin at all (its answer
# comes from the remote, not from the caller).

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

# ---------------------------------------------------------------------------
# The agent's own number (#490). See the header for the rules; this is them.
# ---------------------------------------------------------------------------

# A CalVer as the mint emits it: a four-digit year, a NON-padded month 1-12 and
# a patch of at least 1 — never `2026.08.5`, never `2026.8.0`.
CALVER_RE='^([1-9][0-9]{3})\.([1-9]|1[0-2])\.([1-9][0-9]*)$'
AGENT_TAG_PREFIX="agent-v"

# calver_max — the highest of the CalVers on stdin (one per line), or nothing.
# Numeric per component, which a lexical sort gets wrong the first October.
calver_max() {
    sort -t. -k1,1n -k2,2n -k3,3n | tail -n 1
}

# calver_gt A B — A sorts strictly above B, component by component.
calver_gt() {
    local a1 a2 a3 b1 b2 b3
    IFS=. read -r a1 a2 a3 <<< "$1"
    IFS=. read -r b1 b2 b3 <<< "$2"
    if (( 10#$a1 != 10#$b1 )); then (( 10#$a1 > 10#$b1 )); return; fi
    if (( 10#$a2 != 10#$b2 )); then (( 10#$a2 > 10#$b2 )); return; fi
    (( 10#$a3 > 10#$b3 ))
}

# The legacy base, as a bare version: LAST_COMBINED_AGENT_RELEASE with its `v`
# dropped. The environment wins (the test seam; `source scripts/config.sh`
# exports the same value), else the line config.sh carries — read as TEXT,
# because sourcing the whole file here would run its `config.local.sh` hook and
# export a dozen unrelated names into every caller of a version script.
# Fails closed: a missing or malformed value would otherwise mint or derive
# against a base nobody chose.
last_combined_agent_version() {
    local raw="${LAST_COMBINED_AGENT_RELEASE:-}"
    if [[ -z "$raw" && -f "$SCRIPT_DIR/config.sh" ]]; then
        raw=$(sed -n 's/^export LAST_COMBINED_AGENT_RELEASE="\(.*\)"$/\1/p' "$SCRIPT_DIR/config.sh" | sed -n '1p')
    fi
    if [[ ! "$raw" =~ ^v([1-9][0-9]{3}\.([1-9]|1[0-2])\.[1-9][0-9]*)$ ]]; then
        echo "error: LAST_COMBINED_AGENT_RELEASE is '${raw:-<unset>}' — expected vYYYY.M.N (scripts/config.sh)" >&2
        return 1
    fi
    printf '%s\n' "${raw#v}"
}

# Is $1 an `agent-v<CalVer>` tag name? `agent-latest` (the rolling feed release)
# and anything else that merely starts with `agent-` is not an agent version,
# and a hand-made `agent-v2026.08.5` is not one either.
is_agent_tag() {
    [[ "$1" == "${AGENT_TAG_PREFIX}"* && "${1#"$AGENT_TAG_PREFIX"}" =~ $CALVER_RE ]]
}

# Keep only the `agent-v<CalVer>` names on stdin, printing each as its bare
# version.
agent_versions_of() {
    local name
    while IFS= read -r name; do
        if is_agent_tag "$name"; then
            printf '%s\n' "${name#"$AGENT_TAG_PREFIX"}"
        fi
    done
}

# The `agent-v*` tags the REMOTE advertises, one `<version> <commit>` per line
# (the commit an annotated tag PEELS to — `^{}` — never the tag object). This
# is the mint's whole view of what has shipped, and it is `ls-remote` on
# purpose: a local tag is not a release, and a stale clone must not mint a
# number someone else already pushed. Non-zero when the probe itself failed.
remote_agent_tags() {
    local out name commit
    out=$(git ls-remote --tags origin "refs/tags/${AGENT_TAG_PREFIX}*") || return 1
    printf '%s\n' "$out" | awk '
        NF >= 2 {
            ref = $2; sub(/^refs\/tags\//, "", ref)
            if (ref ~ /\^\{\}$/) { sub(/\^\{\}$/, "", ref); peeled[ref] = $1 } else { plain[ref] = $1 }
        }
        END { for (r in plain) print r, (r in peeled ? peeled[r] : plain[r]) }
    ' | while read -r name commit; do
        if is_agent_tag "$name"; then
            printf '%s %s\n' "${name#"$AGENT_TAG_PREFIX"}" "$commit"
        fi
    done
}

# Does a RELEASE named $1 exist? 0 yes, 1 no, 2 cannot tell. A release outlives
# its tag: delete the tag and the release (draft or not) is still there, which
# is exactly the number the mint must not hand out again.
agent_release_exists() {
    local tag="$1" out rc=0
    if ! command -v gh >/dev/null 2>&1; then
        echo "error: the gh CLI is required to ask whether a release named $tag exists (brew install gh) — refusing to mint blind" >&2
        return 2
    fi
    out=$(gh release view "$tag" 2>&1) || rc=$?
    if [[ "$rc" -eq 0 ]]; then
        return 0
    fi
    case "$out" in
        *"release not found"*) return 1 ;;
    esac
    echo "error: could not ask whether a release named $tag exists (gh exited $rc: ${out:-no output}) — refusing to mint blind" >&2
    return 2
}

# The agent version of this checkout. Prints the bare version, or prints
# nothing and returns 1 with the reason on stderr.
get_agent_version() {
    local shallow tagged base_tag base_commit base_version k sha
    if ! git rev-parse --git-dir >/dev/null 2>&1; then
        echo "error: not inside a git checkout, so the agent version cannot be derived" >&2
        return 1
    fi
    shallow=$(git rev-parse --is-shallow-repository 2>/dev/null || echo unknown)
    if [[ "$shallow" != "false" ]]; then
        echo "error: this checkout is shallow (or git could not say), so the commits since the agent's base cannot be counted — fetch full history (fetch-depth: 0, or git fetch --unshallow)" >&2
        return 1
    fi

    # An agent-v* tag at HEAD IS the version; the highest if there are several.
    tagged=$(git tag --points-at HEAD --list "${AGENT_TAG_PREFIX}*" | agent_versions_of | calver_max)
    if [[ -n "$tagged" ]]; then
        printf '%s\n' "$tagged"
        return 0
    fi

    # A source build: the highest agent-v* tag reachable from HEAD, else the
    # legacy base.
    base_version=$(git tag --merged HEAD --list "${AGENT_TAG_PREFIX}*" | agent_versions_of | calver_max)
    if [[ -n "$base_version" ]]; then
        base_tag="${AGENT_TAG_PREFIX}${base_version}"
    else
        base_version=$(last_combined_agent_version) || return 1
        base_tag="v${base_version}"
    fi
    if ! base_commit=$(git rev-parse --verify --quiet "refs/tags/${base_tag}^{commit}"); then
        echo "error: the base tag $base_tag is not in this checkout, so the commits since it cannot be counted — fetch tags (git fetch --tags)" >&2
        return 1
    fi
    k=$(git rev-list --count "${base_commit}..HEAD") || return 1
    sha=$(git rev-parse --short HEAD) || return 1
    printf '%s+dev.%s.g%s\n' "$base_version" "$k" "$sha"
}

# The agent mint. Same stdout contract as mint_tag. See the header.
mint_agent_tag() {
    local push="$1" head_commit tags reused legacy shipped shipped_max
    local today year month month_max patch version tag rc

    if ! git remote get-url origin >/dev/null 2>&1; then
        echo "error: no origin remote — the agent mint reads what has shipped from the remote's tags and never from local ones; refusing to mint blind" >&2
        exit 1
    fi
    head_commit=$(git rev-parse "HEAD^{commit}")
    if ! tags=$(remote_agent_tags); then
        echo "error: remote tag probe failed (network/auth?) — refusing to mint blind" >&2
        exit 1
    fi

    # 1. Reuse: an agent-v* tag already at HEAD answers for itself.
    reused=$(printf '%s\n' "$tags" | awk -v h="$head_commit" 'NF == 2 && $2 == h { print $1 }' | calver_max)
    if [[ -n "$reused" ]]; then
        echo "Tag ${AGENT_TAG_PREFIX}${reused} already points at $head_commit — reusing (idempotent re-run)" >&2
        printf 'version=%s\ntag=%s%s\naction=reuse\n' "$reused" "$AGENT_TAG_PREFIX" "$reused"
        return 0
    fi

    # 2. N = 1 + the highest patch among this month's shipped versions.
    if ! legacy=$(last_combined_agent_version); then
        exit 1
    fi
    shipped="$(printf '%s\n' "$tags" | awk 'NF == 2 { print $1 }')
$legacy"
    shipped_max=$(printf '%s\n' "$shipped" | calver_max)

    today=$(get_today)
    year="${today%%-*}"
    month=$(echo "$today" | cut -d- -f2)
    month=$((10#$month))
    month_max=$(printf '%s\n' "$shipped" | awk -F. -v y="$year" -v m="$month" '$1 == y && $2 == m && $3 + 0 > max { max = $3 + 0 } END { print max + 0 }')
    patch=$((month_max + 1))
    version="${year}.${month}.${patch}"
    tag="${AGENT_TAG_PREFIX}${version}"

    # 3. Refusals. Strictly above everything shipped...
    if ! calver_gt "$version" "$shipped_max"; then
        echo "error: $tag does not sort above $shipped_max, which has already shipped — refusing to mint a version a host would not take as newer (a future-month tag on the remote, or a skewed clock?)" >&2
        exit 1
    fi
    # ...and not a name a release already holds.
    rc=0
    agent_release_exists "$tag" || rc=$?
    case "$rc" in
        0)
            echo "error: a release named $tag already exists, so its number must not be minted again (a deleted tag does not free a release's version — never delete an agent-v* tag)" >&2
            exit 1
            ;;
        1) ;;
        *) exit 1 ;;
    esac

    if [[ "$push" = "true" ]]; then
        git tag -a "$tag" -m "Agent release $version" "$head_commit"
        if ! git push origin "refs/tags/$tag" >&2; then
            git tag -d "$tag" >&2
            echo "error: the remote refused $tag (another mint won the race?) — nothing was minted; re-run to take the next number" >&2
            exit 1
        fi
        echo "Created and pushed tag: $tag" >&2
    else
        echo "Dry run: would create tag $tag at $head_commit (pass --push to mint)" >&2
    fi

    printf 'version=%s\ntag=%s\naction=create\n' "$version" "$tag"
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
    --agent-version)
        get_agent_version
        ;;
    --agent-tag)
        PUSH=false
        if [[ "${2:-}" = "--push" ]]; then
            PUSH=true
        elif [[ -n "${2:-}" ]]; then
            echo "usage: $0 --agent-tag [--push]" >&2
            exit 2
        fi
        mint_agent_tag "$PUSH"
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
        echo "usage: $0 [--version|--build|--commit|--full-with-sha|--build-time|--tag [--push]|--agent-version|--agent-tag [--push]]" >&2
        exit 2
        ;;
esac
