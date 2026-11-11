#!/usr/bin/env bash
set -euo pipefail

# The cockpit release train's failure hint (#753, #756): what a failed
# publish-feed.yml run means for the release it was publishing. It is the body
# of that workflow's last step, "Say what a failure after publishing means",
# moved here so scripts/release-failure-hint-test.sh can run the real bytes
# against a stub `gh` (a release workflow only runs on a tag push, so no PR
# exercises it). Prints the state and the way out; changes nothing, and never
# states a release state it could not read.
#
# Environment (the step passes these):
#   GH_TOKEN             for `gh`
#   GITHUB_REPOSITORY    owner/name
#   TAG                  the tag being published; empty when the tag step failed
#   UPLOAD               `steps.upload.outcome`
#   ENDPOINT_OUTCOME     `steps.endpoint.outcome`
#
# Exit status is always 0: the job is already red, and this step only explains.

unknown() {
    echo "::error::this job failed and the release's state could not be read, so it is unknown whether it is still a draft; check it, and re-run if it is public: gh api graphql -f query='query(\$o:String!,\$r:String!,\$t:String!){repository(owner:\$o,name:\$r){release(tagName:\$t){isDraft isLatest}}}' -f o=${GITHUB_REPOSITORY%/*} -f r=${GITHUB_REPOSITORY#*/} -f t=${TAG:-<tag>}"
    exit 0
}

[[ -n "${TAG:-}" ]] || unknown
# A missing tag reads as an empty line with exit 0, so only the exact strings
# `true` and `false` count as a read. The `$o` `$r` `$t` in the query are
# GraphQL variables, not shell ones.
# shellcheck disable=SC2016
state="$(gh api graphql \
    -f query='query($o:String!,$r:String!,$t:String!){repository(owner:$o,name:$r){release(tagName:$t){isDraft isLatest}}}' \
    -f o="${GITHUB_REPOSITORY%/*}" -f r="${GITHUB_REPOSITORY#*/}" -f t="$TAG" \
    --jq '.data.repository.release | [.isDraft, .isLatest] | @tsv')" || unknown
is_draft=""
is_latest=""
IFS=$'\t' read -r is_draft is_latest <<< "$state" || true
case "$is_draft" in
    true) exit 0 ;;
    false) ;;
    *) unknown ;;
esac
# The feed clause is worded from each step's own outcome: the endpoint check
# is named only when it failed, not inferred from the upload having succeeded.
if [[ "${UPLOAD:-}" == "success" ]]; then
    if [[ "${ENDPOINT_OUTCOME:-}" == "failure" ]]; then
        feed="latest.json was uploaded but the endpoint check did not confirm it"
    else
        feed="latest.json was uploaded"
    fi
else
    feed="latest.json was not uploaded"
fi
case "$is_latest" in
    true)
        echo "::error::the release is now PUBLIC and holds releases/latest, and $feed, so installed cockpits' update checks fail until the feed is uploaded and served" ;;
    false)
        echo "::error::the release is now PUBLIC; $feed; releases/latest was not moved to this release; this step did not check what it serves" ;;
    *)
        echo "::error::the release is now PUBLIC; $feed; whether it holds releases/latest is unknown (its isLatest could not be read)" ;;
esac
echo "Use 'Re-run failed jobs' on this run: the publish step finds the release already public and does nothing, then the manifest is rebuilt and uploaded." >&2
