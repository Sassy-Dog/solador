#!/usr/bin/env bash
set -euo pipefail

# The two publish-time guards of the agent feed (#491, part of #472): the logic
# `.github/workflows/publish-agent-feed.yml`'s credential-free `agent-eligibility`
# job runs before `prd`'s reviewer is ever asked to approve a signing run.
#
#   scripts/agent-feed-guard.sh newest TAG [--force]   the FREEZE guard
#   scripts/agent-feed-guard.sh latest                 the LATEST guard
#
# Environment: GITHUB_REPOSITORY (`owner/name`) and, for `gh`, GH_TOKEN. Exit 0
# when the guard passes; exit 1 with an `::error::` line and the remedy when it
# refuses (a guard that cannot read what it guards refuses too, never passes);
# exit 2 on bad usage.
#
# Why these live in a script and not in the workflow's YAML: a release workflow
# can only be exercised by cutting a release, which is exactly the moment a
# wrong guard costs a frozen fleet. As a script they run from `./dev test` and
# CI against a stub `gh` (scripts/agent-feed-guard-test.sh), the way
# scripts/versioning-test.sh stubs its own tools.
#
# THE FREEZE GUARD (`newest`). The candidate must be the NEWEST published,
# non-prerelease `agent-v*` release, compared as CalVer (numerically per
# component, never lexically: .9 sorts below .10). Otherwise it refuses, unless
# `--force` — which the workflow wires to an explicit dispatch input. The reason
# is what a stale feed does: a replay at an older tag would `--clobber` a newer,
# validly signed `agent-latest.json` with an older one, every installed host
# would then answer "no applicable release" (exit 4), the unattended timer
# (#394) counts exit 4 as a quiet day, fresh installs would get the older
# release, and nothing would page anyone. Forward-only versions protect an
# installed host from a DOWNGRADE, not from a FREEZE.
#
# It reads the release LIST, never the served feed, on purpose: the feed can be
# missing or briefly mismatched in the middle of a `--clobber`, and the rule has
# to give the right answer exactly then. Three properties follow:
#   * re-running the publish at the CURRENT tag after a failed upload passes —
#     the recovery path the design depends on;
#   * a replay at an older tag is refused;
#   * a bad release can be pulled (turned back into a draft, or its RELEASE
#     deleted — never its tag) and the previous one re-served with no `--force`,
#     because the pulled one is no longer published. Installed hosts still never
#     go backwards: the forward-only rule is theirs, not this script's.
#
# The list is read completely, `gh api --paginate`, and the stub in the test
# returns ONE page unless `--paginate` is passed, so dropping the flag fails a
# case rather than going unnoticed. The cockpit's `v*` releases far outnumber the
# agent's, and a release list is newest-created-first: a newest agent release
# created before thirty later cockpit releases is on page two. A single page
# would call the candidate newest when it is not.
#
# What counts as a release here: `draft == false`, `prerelease == false`, a tag
# starting `agent-v`. A draft is not public; a prerelease (including the rolling
# `agent-latest`) is never offered. A published release tagged `agent-v…` that is
# not `agent-vYYYY.M.N` cannot have come from the mint and cannot be ordered, so
# it is reported and ignored rather than allowed to wedge or to win. The
# candidate itself must be on that list: eligibility has already required a
# published, stable release, so a candidate this guard cannot find (a release
# pulled back to a draft during `prd`'s approval wait, say) is a contradiction,
# and a contradiction is a refusal.
#
# `--force` waives exactly one refusal: "not the newest". The list must still be
# readable, so a forced run still names what it overrode, and "the candidate is
# not a published release" is never waived (force is for REPLAYING an older
# published release). Every other refusal in `agent-eligibility` (a draft, a
# prerelease, a missing asset, a dispatch not run at its tag) is outside this
# script and is never waived either.
#
# THE LATEST GUARD (`latest`). `releases/latest` must name a `v*` tag. The
# cockpit's updater and its in-app check read `releases/latest`, and an agent
# release that took the slot (published from the UI with "Set as the latest
# release" ticked; the draft was created with `--latest=false`, and a draft's
# flag does not obviously survive a UI publish) would point every installed
# cockpit's updater at a release with no `latest.json` and no `.app.tar.gz`. It
# fails naming the remedy, `gh release edit <tag> --latest=false`.

# `agent-vYYYY.M.N`, the exact shape the agent mint emits and
# scripts/assert-release-tag.sh --agent accepts (non-padded month 1-12, patch at
# least 1), with the patch bounded to nine digits so a version fits one integer
# below. Held in a variable: bash 3.2 reads a quoted right-hand side as a literal.
AGENT_TAG_RE='^agent-v([1-9][0-9]{3})\.([1-9]|1[0-2])\.([1-9][0-9]{0,8})$'

usage() {
    echo "usage: $0 newest agent-vYYYY.M.N [--force] | $0 latest" >&2
    exit 2
}

need_repo() {
    if [[ -z "${GITHUB_REPOSITORY:-}" ]]; then
        echo "::error::GITHUB_REPOSITORY is not set, so there is no repository to ask about"
        exit 2
    fi
}

need_jq() {
    if ! command -v jq >/dev/null 2>&1; then
        echo "::error::jq is not installed; the release list cannot be read, and an unread list is a refusal"
        exit 1
    fi
}

# calver_key TAG — one integer that orders agent tags the way the consumers do:
# ((year * 100 + month) * 10^10) + patch. Sets CALVER_KEY; returns 1 when TAG is
# not a well-formed agent tag. `10#` because a month or patch is decimal, and a
# leading-zero spelling would otherwise be read as octal.
calver_key() {
    if [[ ! "$1" =~ $AGENT_TAG_RE ]]; then
        return 1
    fi
    CALVER_KEY=$(( ( 10#${BASH_REMATCH[1]} * 100 + 10#${BASH_REMATCH[2]} ) * 10000000000 + 10#${BASH_REMATCH[3]} ))
}

cmd_newest() {
    local candidate="${1:-}" force=0
    [[ -n "$candidate" ]] || usage
    shift
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --force) force=1 ;;
            *) usage ;;
        esac
        shift
    done
    need_repo
    if ! calver_key "$candidate"; then
        echo "::error::'$candidate' is not an agent-vYYYY.M.N release tag"
        exit 2
    fi
    local candidate_key="$CALVER_KEY"
    need_jq

    local pages published
    if ! pages="$(gh api --paginate "repos/$GITHUB_REPOSITORY/releases")"; then
        echo "::error::could not list $GITHUB_REPOSITORY's releases, so the freeze guard cannot tell whether $candidate is the newest agent release"
        exit 1
    fi
    # `.[]` over every page: `gh --paginate` prints each page as its own JSON
    # array, and jq applies the filter to each value of that stream.
    if ! published="$(printf '%s\n' "$pages" | jq -r '.[]
            | select(.draft == false and .prerelease == false)
            | .tag_name
            | select(startswith("agent-v"))')"; then
        echo "::error::the release list from $GITHUB_REPOSITORY was not JSON this guard could read"
        exit 1
    fi

    local tag newest_tag="" newest_key=0 seen_candidate=0
    while IFS= read -r tag; do
        [[ -n "$tag" ]] || continue
        if ! calver_key "$tag"; then
            echo "::warning::ignoring the published release '$tag': it is not an agent-vYYYY.M.N tag, so it cannot be ordered"
            continue
        fi
        if [[ "$tag" == "$candidate" ]]; then
            seen_candidate=1
        fi
        if (( CALVER_KEY > newest_key )); then
            newest_key=$CALVER_KEY
            newest_tag="$tag"
        fi
    done <<< "$published"

    # A candidate this guard cannot find is not "an older release": it is a
    # release that is not public (a draft's assets are not downloadable, a
    # prerelease is never offered) or not there, and a feed for it would point
    # hosts at URLs that do not serve. `--force` is for REPLAYING an older
    # published release, so it never waives this — and the freeze remedy text
    # below would be the wrong advice for it.
    if [[ $seen_candidate -eq 0 ]]; then
        echo "::error::freeze guard: $candidate is not among $GITHUB_REPOSITORY's published, non-prerelease agent-v* releases${newest_tag:+ (the newest of them is $newest_tag)}"
        cat >&2 <<EOF
A feed for a release that is not published would point every host at assets that
do not serve (a draft's are not public; a prerelease is never offered). Publish
the release as a normal release and re-run; if it was only just published and
the API does not list it yet, re-run in a minute. 'force' does not waive this:
it is for replaying an older PUBLISHED release.
EOF
        exit 1
    fi

    local refusal=""
    if (( candidate_key < newest_key )); then
        refusal="$candidate is not the newest published agent release; $newest_tag is"
    fi

    if [[ -z "$refusal" ]]; then
        echo "$candidate is the newest published, non-prerelease agent release."
        return 0
    fi
    if [[ $force -eq 1 ]]; then
        echo "::warning::forced past the freeze guard: $refusal"
        echo "Forced: publishing the feed for $candidate although $refusal." >&2
        return 0
    fi
    echo "::error::freeze guard: $refusal"
    cat >&2 <<EOF
Publishing this feed would replace a newer agent-latest.json with an older one.
Every installed host would then answer "no applicable release" (exit 4), the
unattended timer counts that as a quiet day, and nothing would page anyone.
  * Recovering from a failed upload: re-run the publish at the CURRENT agent
    release's tag, the newest one — that passes.
  * Pulling a bad release: turn it back into a draft (or delete the release and
    KEEP its tag: the mint reads agent-v* tags, and a deleted tag's number would
    be handed out again for different bytes), then re-run this at the previous
    release's tag. No force is needed once the bad one is no longer published.
  * A deliberate replay of an older release: dispatch with force=true.
EOF
    exit 1
}

cmd_latest() {
    [[ $# -eq 0 ]] || usage
    need_repo
    need_jq

    local response tag
    if ! response="$(gh api "repos/$GITHUB_REPOSITORY/releases/latest")"; then
        echo "::error::could not read $GITHUB_REPOSITORY's latest release, so the latest guard cannot say the cockpit still owns that slot"
        exit 1
    fi
    if ! tag="$(printf '%s\n' "$response" | jq -r '.tag_name // empty')"; then
        echo "::error::the latest-release response from $GITHUB_REPOSITORY was not JSON this guard could read"
        exit 1
    fi
    if [[ -z "$tag" ]]; then
        echo "::error::$GITHUB_REPOSITORY's latest release response names no tag"
        exit 1
    fi
    if [[ "$tag" == v* ]]; then
        echo "releases/latest names $tag, a cockpit v* release; the agent has not taken that slot."
        return 0
    fi
    echo "::error::releases/latest names '$tag', not a cockpit v* release"
    cat >&2 <<EOF
The cockpit's updater reads releases/latest. An agent release holding that slot
sends every installed cockpit to a release with no latest.json and no updater
payload. Release the slot, then confirm the cockpit's newest release holds it and
re-run this workflow:

  gh release edit $tag --latest=false
  gh api repos/$GITHUB_REPOSITORY/releases/latest --jq .tag_name

(If the second command does not name the newest v* release, mark that one:
gh release edit <its tag> --latest.)
EOF
    exit 1
}

case "${1:-}" in
    newest) shift; cmd_newest "$@" ;;
    latest) shift; cmd_latest "$@" ;;
    *) usage ;;
esac
