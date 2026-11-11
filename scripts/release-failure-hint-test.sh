#!/usr/bin/env bash
set -euo pipefail

# Proves scripts/release-failure-hint.sh says what it claims to say (#756).
#
# The script is the body of publish-feed.yml's last step, which runs only after
# a failed release run: no PR exercises it, so this is its only coverage before
# the next cut. It runs the REAL script (the bytes the step runs) against a
# stub `gh` on PATH, the way scripts/agent-feed-guard-test.sh does. No `jq`
# needed: the stub plays the part of `gh --jq` and prints the tsv line itself.
# Bash 3.2-clean: CI runs it under macOS stock /bin/bash as well as bash 5.
# Run by ci.yml's helper legs, by `./dev test`, and by hand:
#
#   scripts/release-failure-hint-test.sh
#
# THE STUB IS ARGUMENT-SENSITIVE: it answers only `gh api graphql` carrying the
# exact query string, the exact `--jq` filter and the owner, name and tag
# variables for the repo and tag under test, and exits 2 on any other argument,
# field or filter. Every call's argv lands in $GH_SHIM_CALLS. The unknown
# message's copyable query is asserted verbatim too.
#
# What it holds: no tag (unknown, never calls gh); an unreadable state (gh
# fails, an empty reply, a garbled draft field: each unknown); a draft (prints
# nothing); public and latest, public and not latest, public with an
# unreadable isLatest; and for each public case the feed clause with the upload
# succeeding and the endpoint check succeeding / failing / skipped, and with
# the upload failing. The copyable command in the unknown message ends the
# line, with nothing after it. The command the workflow's inline fallback
# prints matches the script's, escapes included.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
HINT="$SCRIPT_DIR/release-failure-hint.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

REPO="Acme/solador"
TAG="v2026.11.99"
# The exact GraphQL query and `--jq` filter the script must send. They are
# written out here, not read from the script, so a changed query or filter
# fails the stub rather than being matched by it.
# shellcheck disable=SC2016
QUERY='query($o:String!,$r:String!,$t:String!){repository(owner:$o,name:$r){release(tagName:$t){isDraft isLatest}}}'
JQ='.data.repository.release | [.isDraft, .isLatest] | @tsv'

# --- The stub `gh` ----------------------------------------------------------
#   GH_SHIM_REPO   owner/name the script must ask about
#   GH_SHIM_TAG    the tag it must ask about
#   GH_SHIM_REPLY  printed verbatim (what `--jq` would have produced)
#   GH_SHIM_FAIL   `gh` fails with this text
mkdir -p "$work/ghshim"
cat > "$work/ghshim/gh" <<'SHIM'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${GH_SHIM_CALLS:-/dev/null}"
if [ "${1:-}" != api ] || [ "${2:-}" != graphql ]; then
    echo "gh shim: unexpected call: $*" >&2
    exit 2
fi
shift 2
saw_query=0 saw_o=0 saw_r=0 saw_t=0 saw_jq=0
while [ $# -gt 0 ]; do
    case "$1" in
        -f)
            case "$2" in
                "query=$GH_SHIM_QUERY") saw_query=1 ;;
                o="${GH_SHIM_REPO%/*}") saw_o=1 ;;
                r="${GH_SHIM_REPO#*/}") saw_r=1 ;;
                t="$GH_SHIM_TAG") saw_t=1 ;;
                *) echo "gh shim: unexpected field: $2" >&2; exit 2 ;;
            esac
            shift 2 ;;
        --jq)
            [ "$2" = "$GH_SHIM_JQ" ] || { echo "gh shim: unexpected --jq filter: $2" >&2; exit 2; }
            saw_jq=1
            shift 2 ;;
        *) echo "gh shim: unexpected argument: $1" >&2; exit 2 ;;
    esac
done
if [ "$saw_query$saw_o$saw_r$saw_t$saw_jq" != 11111 ]; then
    echo "gh shim: the query or one of its variables is missing" >&2
    exit 2
fi
if [ -n "${GH_SHIM_FAIL:-}" ]; then echo "$GH_SHIM_FAIL" >&2; exit 1; fi
printf '%s\n' "${GH_SHIM_REPLY-}"
SHIM
chmod +x "$work/ghshim/gh"

pass=0
fail=0

# run_hint NAME TAG UPLOAD ENDPOINT REPLY [FAIL] — run the script, leaving
# stdout in $work/out, stderr in $work/err and the status in $status.
run_hint() {
    local tag="$1" upload="$2" endpoint="$3" reply="$4" failtext="${5:-}"
    : > "$work/calls"
    status=0
    env -i PATH="$work/ghshim:$PATH" HOME="$work" \
        GH_SHIM_REPO="$REPO" GH_SHIM_TAG="$TAG" GH_SHIM_REPLY="$reply" GH_SHIM_FAIL="$failtext" \
        GH_SHIM_CALLS="$work/calls" GH_SHIM_QUERY="$QUERY" GH_SHIM_JQ="$JQ" \
        GH_TOKEN=x GITHUB_REPOSITORY="$REPO" TAG="$tag" UPLOAD="$upload" ENDPOINT_OUTCOME="$endpoint" \
        "$BASH" "$HINT" > "$work/out" 2> "$work/err" || status=$?
}

ok() { pass=$((pass + 1)); echo "ok   - $1"; }
bad() { fail=$((fail + 1)); echo "FAIL - $1" >&2; [ -z "${2:-}" ] || echo "       $2" >&2; }

# expect_out NAME EXACT — stdout is exactly EXACT (one line), status 0.
expect_out() {
    local got
    got="$(cat "$work/out")"
    if [ "$status" = 0 ] && [ "$got" = "$2" ]; then ok "$1"; else bad "$1" "status=$status, stdout: $got"; fi
}
expect_err_has() {
    if grep -qF -- "$2" "$work/err"; then ok "$1"; else bad "$1" "stderr lacks: $2 (got: $(cat "$work/err"))"; fi
}
expect_no_gh_call() {
    if [ ! -s "$work/calls" ]; then ok "$1"; else bad "$1" "gh was called: $(cat "$work/calls")"; fi
}

REMEDY="Use 'Re-run failed jobs' on this run: the publish step finds the release already public and does nothing, then the manifest is rebuilt and uploaded."

UNKNOWN_HEAD="::error::this job failed and the release's state could not be read, so it is unknown whether it is still a draft; check it, and re-run if it is public: gh api graphql -f query='$QUERY' -f o=Acme -f r=solador -f t="

# expect_unknown NAME [TAG] — exactly the unknown message, ending in the
# copyable command (query verbatim), and exit status 0.
expect_unknown() {
    local got
    got="$(cat "$work/out")"
    if [ "$status" = 0 ] && [ "$got" = "$UNKNOWN_HEAD${2:-$TAG}" ]; then ok "$1"; else bad "$1" "status=$status, stdout: $got"; fi
}

# --- No tag -----------------------------------------------------------------
run_hint "" failure skipped "false	true"
expect_no_gh_call "no tag: gh is not asked"
expect_unknown "no tag: unknown, the command names a <tag> placeholder, last on the line" "<tag>"

# --- An unreadable state ------------------------------------------------------
run_hint "$TAG" success failure "" "HTTP 502"
expect_unknown "gh fails: unknown"
run_hint "$TAG" success failure ""
expect_unknown "empty reply: unknown"
run_hint "$TAG" success failure "	"
expect_unknown "a lone TAB (a missing release: [null,null] | @tsv): unknown"
run_hint "$TAG" success failure "maybe	true"
expect_unknown "a garbled isDraft: unknown"
run_hint "$TAG" success failure "null	null"
expect_unknown "a null isDraft: unknown"

# --- A draft ------------------------------------------------------------------
for latest in true false ""; do
    run_hint "$TAG" failure skipped "true	$latest"
    expect_out "draft (isLatest='$latest'): prints nothing" ""
    if [ ! -s "$work/err" ]; then ok "draft (isLatest='$latest'): nothing on stderr"; else bad "draft stderr" "$(cat "$work/err")"; fi
done

# --- Public: the feed clause across the upload and the endpoint check ---------
UPLOADED_UNCONFIRMED="latest.json was uploaded but the endpoint check did not confirm it"
UPLOADED_ONLY="latest.json was uploaded"
NOT_UPLOADED="latest.json was not uploaded"

public_cases() {
    # $1 = isLatest field as read ("true", "false", "" for unreadable)
    local latest="$1" prefix="$2" suffix="$3" name c upload endpoint feed
    # upload:endpoint:kind (kind picks the expected feed clause)
    for c in success:failure:unconfirmed success:success:only success:skipped:only success::only \
             failure:skipped:none failure:failure:none cancelled:skipped:none; do
        upload="${c%%:*}"
        endpoint="${c#*:}"; endpoint="${endpoint%%:*}"
        case "${c##*:}" in
            unconfirmed) feed="$UPLOADED_UNCONFIRMED" ;;
            only) feed="$UPLOADED_ONLY" ;;
            *) feed="$NOT_UPLOADED" ;;
        esac
        name="isLatest='$latest' upload=$upload endpoint=${endpoint:-unset}"
        run_hint "$TAG" "$upload" "$endpoint" "false	$latest"
        expect_out "$name: message" "$prefix$feed$suffix"
        expect_err_has "$name: remedy" "$REMEDY"
    done
}

public_cases true "::error::the release is now PUBLIC and holds releases/latest, and " ", so installed cockpits' update checks fail until the feed is uploaded and served"
public_cases false "::error::the release is now PUBLIC; " "; releases/latest was not moved to this release; this step did not check what it serves"
public_cases "" "::error::the release is now PUBLIC; " "; whether it holds releases/latest is unknown (its isLatest could not be read)"

# --- Only `failure` words the endpoint check as unconfirmed --------------------
run_hint "$TAG" success success "false	true"
if grep -q "endpoint check" "$work/out"; then bad "a passing endpoint check is not named as unconfirmed" "$(cat "$work/out")"; else ok "a passing endpoint check is not named as unconfirmed"; fi

# --- The stub is argument-sensitive: the real script asks the right question ---
run_hint "$TAG" success failure "false	true"
calls="$(cat "$work/calls")"
case "$calls" in
    "api graphql -f query="*" -f o=Acme -f r=solador -f t=$TAG --jq "*) ok "asks GraphQL for the repo and tag under test" ;;
    *) bad "asks GraphQL for the repo and tag under test" "$calls" ;;
esac

# --- The workflow's inline fallback prints the same copyable command ----------
# publish-feed.yml hand-writes it (it runs without the script, which comes from
# a checkout that may have failed), so it is compared here against the pinned
# query and against the script's own tail. PUBLISH_FEED_WORKFLOW points the
# check at another copy of the workflow (the negative control).
WORKFLOW="${PUBLISH_FEED_WORKFLOW:-$SCRIPT_DIR/../.github/workflows/publish-feed.yml}"
wf_line="$(grep -F 'the failure hint could not run' "$WORKFLOW" || true)"
sc_line="$(grep -F "the release's state could not be read" "$HINT" || true)"
# The workflow's echo is double-quoted, so its `$` are written `\$`. The raw
# extract is compared with the pinned query re-escaped, never un-escaped: a
# dropped backslash would print broken GraphQL and must fail here.
wf_query="$(printf '%s\n' "$wf_line" | sed -n "s/.*query='\\(.*\\)' -f o=.*/\\1/p")"
want_query="$(printf '%s\n' "$QUERY" | sed 's/\$/\\$/g')"
wf_tail="$(printf '%s\n' "$wf_line" | sed -n "s/.*query='.*' \\(-f o=.*\\)\"\$/\\1/p")"
sc_tail="$(printf '%s\n' "$sc_line" | sed -n "s/.*query='.*' \\(-f o=.*\\)\"\$/\\1/p")"
if [ -z "$wf_tail" ] || [ -z "$sc_tail" ]; then
    bad "the workflow fallback's copyable command could be read" "wf: $wf_line"
elif [ "$wf_query" != "$want_query" ]; then
    bad "the workflow fallback's query matches the pinned one, escapes included" "workflow: $wf_query  want: $want_query"
elif [ "$wf_tail" != "$sc_tail" ]; then
    bad "the workflow fallback's -f o= -f r= -f t= tail matches the script's" "workflow: $wf_tail  script: $sc_tail"
else
    ok "the workflow fallback's copyable command matches the pinned query and the script's tail"
fi

echo
echo "release-failure-hint-test: $pass passed, $fail failed"
[ "$fail" = 0 ]
