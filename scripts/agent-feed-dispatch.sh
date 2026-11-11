#!/usr/bin/env bash
set -euo pipefail

# The agent train's feed step (#757): dispatch publish-agent-feed.yml AT the
# release tag, identify the run that dispatch started, and wait for it. This is
# the body of release-agent.yml's `feed` job, kept a script so `./dev test` can
# run it against a stub `gh` (scripts/agent-feed-dispatch-test.sh) — a release
# workflow can only be exercised by a real release.
#
# WHY A DISPATCH. A called workflow's job that declares `environment: prd` is not
# given the environment's secret, so the feed's signing key resolved empty. A
# dispatched run is a top-level run at the tag, where it resolves. This script
# holds no secret and no environment: `gh` takes GH_TOKEN from the job, which has
# `actions: write` and nothing else.
#
# Usage: scripts/agent-feed-dispatch.sh <agent-vYYYY.M.N tag>
# Needs: GITHUB_REPOSITORY, GH_TOKEN (and GH_REPO or a checkout for `gh`), `jq`.
#   GITHUB_SERVER_URL                         default https://github.com
#   SOLADOR_FEED_DISPATCH_ATTEMPTS / _SLEEP   the fallback's polling, default 12
#                                             tries 5 seconds apart (the test sets
#                                             a short loop with no sleep)
#
# THE RUN IS IDENTIFIED, NEVER GUESSED. Current `gh` prints the new run's URL and
# its id is taken from there. If it prints none, the run is the `workflow_dispatch`
# run of that workflow at the tag created at or after a timestamp taken just
# before the dispatch, and anything but exactly one is a failure.
#
# THE JOB IS GREEN ONLY WHEN THE RUN'S CONCLUSION IS READ AS `success`. `gh run
# watch --exit-status` exits non-zero for ANY non-success conclusion and also for
# an API error mid-watch (a 5xx, a rate limit), when the run may be healthy. So a
# failed watch is followed by a read of the run's own `status` and `conclusion`,
# and the message names exactly what was read: a `success` read passes (that read
# IS the confirmation, said as a notice), `failure` gets the re-run advice, any
# other conclusion is named, a run still going is "could not watch", and a read
# that fails too says the state is unknown. A state that was not read is never
# printed.

tag="${1:-}"
if [[ $# -ne 1 || ! "$tag" =~ ^agent-v[0-9]{4}\.[0-9]{1,2}\.[0-9]+$ ]]; then
    echo "usage: agent-feed-dispatch.sh <agent-vYYYY.M.N tag>" >&2
    exit 2
fi
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
attempts="${SOLADOR_FEED_DISPATCH_ATTEMPTS:-12}"
nap="${SOLADOR_FEED_DISPATCH_SLEEP:-5}"
workflow="publish-agent-feed.yml"

started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
if ! out="$(gh workflow run "$workflow" --ref "$tag" -f tag="$tag" 2>&1)"; then
    echo "$out" >&2
    echo "::error::could not dispatch $workflow at $tag"
    exit 1
fi
echo "$out"

# Current `gh` prints the new run's URL; take its id from there.
run_id="$(grep -Eo '/actions/runs/[0-9]+' <<< "$out" | head -n1 | grep -Eo '[0-9]+' || true)"
if [[ -z "$run_id" ]]; then
    matches=""
    attempt=1
    while [[ $attempt -le $attempts ]]; do
        listed="$(gh run list --workflow "$workflow" --event workflow_dispatch \
            --branch "$tag" --limit 20 --json databaseId,createdAt || true)"
        matches="$(jq -r --arg s "$started" '.[] | select(.createdAt >= $s) | .databaseId' <<< "$listed" 2>/dev/null || true)"
        [[ -n "$matches" ]] && break
        echo "attempt $attempt: the dispatched run is not listed yet, waiting…"
        attempt=$((attempt + 1))
        [[ $attempt -le $attempts ]] && sleep "$nap"
    done
    if [[ -z "$matches" ]]; then
        echo "::error::dispatched $workflow at $tag but found no run for it"
        exit 1
    fi
    if [[ "$(printf '%s\n' "$matches" | grep -c .)" != "1" ]]; then
        echo "::error::more than one $workflow dispatch run at $tag since the dispatch; refusing to guess which is this job's"
        printf '%s\n' "$matches"
        exit 1
    fi
    run_id="$matches"
fi

url="${GITHUB_SERVER_URL:-https://github.com}/$GITHUB_REPOSITORY/actions/runs/$run_id"
echo "Dispatched run: $url"
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    echo "### Agent feed run: $url" >> "$GITHUB_STEP_SUMMARY"
fi

if gh run watch "$run_id" --exit-status; then
    echo "The agent feed run passed: $url"
    exit 0
fi

# The watch failed. Read the run itself rather than assume why.
if ! view="$(gh run view "$run_id" --json status,conclusion 2>&1)"; then
    echo "::error::the watch failed and the run's state could not be read, so it is unknown: $url"
    exit 1
fi
status="$(jq -r '.status // ""' <<< "$view" 2>/dev/null || true)"
conclusion="$(jq -r '.conclusion // ""' <<< "$view" 2>/dev/null || true)"
if [[ -z "$status" ]]; then
    echo "::error::the watch failed and the run's state could not be read, so it is unknown: $url"
    exit 1
fi
if [[ "$status" != "completed" ]]; then
    echo "::error::could not watch the agent feed run to completion (status $status): check $url"
    exit 1
fi
case "$conclusion" in
    success)
        echo "::notice::gh run watch could not follow the agent feed run, but the run concluded success: $url"
        exit 0
        ;;
    failure)
        echo "::error::the agent feed run concluded failure: $url"
        echo "Re-run its failed jobs, or dispatch $workflow again from $tag."
        exit 1
        ;;
    *)
        echo "::error::the agent feed run concluded ${conclusion:-with no conclusion}: $url"
        exit 1
        ;;
esac
