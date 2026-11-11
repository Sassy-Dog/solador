#!/usr/bin/env bash
set -euo pipefail

# Proves scripts/agent-feed-dispatch.sh behaves as it claims (#757): it
# identifies the dispatched run without guessing, waits for it, and is green only
# when the run's conclusion is read as `success`. Runs the REAL script against a
# stub `gh` on PATH, in the shape of scripts/agent-feed-guard-test.sh: the stub
# answers only the exact argv the script must use and fails loudly on any other,
# so a script that asked the wrong question cannot pass. Each case asserts the
# exit status AND one exact output line. Needs `jq` (which the script itself
# needs); without it this SKIPS, unless
# SOLADOR_AGENT_FEED_DISPATCH_TEST_REQUIRE_JQ=1, which CI sets. Bash 3.2-clean;
# the script runs under "$BASH". Run by ci.yml, `./dev test`, and by hand:
#
#   scripts/agent-feed-dispatch-test.sh

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
SUT="${SOLADOR_FEED_DISPATCH_SUT:-$SCRIPT_DIR/agent-feed-dispatch.sh}"

if ! command -v jq >/dev/null 2>&1; then
    if [[ "${SOLADOR_AGENT_FEED_DISPATCH_TEST_REQUIRE_JQ:-}" == "1" ]]; then
        echo "agent-feed-dispatch-test: jq is required here and is not installed" >&2
        exit 1
    fi
    echo "SKIP agent-feed-dispatch-test: jq is not installed; CI requires it" >&2
    exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
TAG="agent-v2026.11.4"
export GITHUB_REPOSITORY="Acme/solador"
export GITHUB_SERVER_URL="https://example.test"
export SOLADOR_FEED_DISPATCH_SLEEP=0
export SOLADOR_FEED_DISPATCH_ATTEMPTS=3
BASE="https://example.test/Acme/solador/actions/runs"
WFURL="https://example.test/Acme/solador/actions/workflows/publish-agent-feed.yml"

mkdir -p "$work/ghshim"
cat > "$work/ghshim/gh" <<'SHIM'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$GH_SHIM_CALLS"
case "$*" in
    "workflow run publish-agent-feed.yml --ref $GH_SHIM_TAG -f tag=$GH_SHIM_TAG")
        if [ -n "${GH_SHIM_DISPATCH_FAIL:-}" ]; then echo "$GH_SHIM_DISPATCH_FAIL" >&2; exit 1; fi
        printf '%s\n' "${GH_SHIM_DISPATCH_OUT:-}"
        ;;
    "run list --workflow publish-agent-feed.yml --event workflow_dispatch --branch $GH_SHIM_TAG --limit 20 --json databaseId,createdAt")
        printf '%s\n' "${GH_SHIM_LIST:-[]}"
        ;;
    "run watch "*" --interval 15 --exit-status")
        echo "(watching)"
        exit "${GH_SHIM_WATCH_RC:-0}"
        ;;
    "run view "*" --json status,conclusion")
        if [ -n "${GH_SHIM_VIEW_FAIL:-}" ]; then echo "$GH_SHIM_VIEW_FAIL" >&2; exit 1; fi
        printf '%s\n' "${GH_SHIM_VIEW:-}"
        ;;
    *)
        echo "gh shim: unexpected call: $*" >&2
        exit 2
        ;;
esac
SHIM
chmod +x "$work/ghshim/gh"

pass=0
fail=0
out=""
rc=0

# run_sut — run the script with the stub first on PATH; leaves $out and $rc.
run_sut() {
    : > "$work/calls"
    set +e
    out="$(PATH="$work/ghshim:$PATH" GH_SHIM_CALLS="$work/calls" GH_SHIM_TAG="$TAG" "$BASH" "$SUT" "$TAG" 2>&1)"
    rc=$?
    set -e
}

# expect NAME RC LINE — the exit status AND one exact output line.
expect() {
    local name="$1" want_rc="$2" line="$3" ok=1
    [[ "$rc" == "$want_rc" ]] || ok=0
    printf '%s\n' "$out" | grep -qxF -- "$line" || ok=0
    if [[ $ok -eq 1 ]]; then
        echo "ok   $name"
        pass=$((pass + 1))
    else
        echo "FAIL $name: want rc=$want_rc and the line: $line"
        echo "     got rc=$rc, output:"
        printf '%s\n' "$out" | sed 's/^/       /'
        fail=$((fail + 1))
    fi
}

# expect_calls NAME PATTERN N — how many stub calls matched PATTERN.
expect_calls() {
    local name="$1" pattern="$2" want="$3" got
    got="$(grep -c -- "$pattern" "$work/calls" || true)"
    if [[ "$got" == "$want" ]]; then
        echo "ok   $name"
        pass=$((pass + 1))
    else
        echo "FAIL $name: want $want call(s) matching '$pattern', got $got"
        fail=$((fail + 1))
    fi
}

# fresh — a dispatch that prints its run's URL, a watch that passes.
fresh() {
    unset GH_SHIM_DISPATCH_FAIL GH_SHIM_LIST GH_SHIM_WATCH_RC GH_SHIM_VIEW GH_SHIM_VIEW_FAIL
    export GH_SHIM_DISPATCH_OUT="$BASE/4242"
}

NEW='"createdAt":"2999-01-01T00:00:00Z"'
OLD='"createdAt":"2000-01-01T00:00:00Z"'
URL="$BASE/4242"

# --- identifying the run ----------------------------------------------------
fresh
run_sut
expect "the printed URL's id is taken, and a passing watch passes" 0 "The agent feed run passed: $URL"
expect_calls "  the runs are not listed when the URL was printed" '^run list' 0
expect_calls "  the watch is on that id" '^run watch 4242 --interval 15 --exit-status$' 1

fresh; export GH_SHIM_DISPATCH_OUT="" GH_SHIM_LIST="[{\"databaseId\":77,$NEW}]"
run_sut
expect "no URL, exactly one run since the dispatch" 0 "The agent feed run passed: $BASE/77"
expect_calls "  the watch is on the run that was found, not another" '^run watch 77 --interval 15 --exit-status$' 1

fresh; export GH_SHIM_DISPATCH_OUT="" GH_SHIM_LIST="[{\"databaseId\":5,$OLD},{\"databaseId\":78,$NEW}]"
run_sut
expect "no URL, a run from before the dispatch is not counted" 0 "The agent feed run passed: $BASE/78"

fresh; export GH_SHIM_DISPATCH_OUT="" GH_SHIM_LIST="[{\"databaseId\":5,$OLD}]"
run_sut
expect "no URL, no run since the dispatch" 1 "::error::dispatched publish-agent-feed.yml at $TAG but found no run for it; look at $WFURL"
expect_calls "  after exactly the configured attempts" '^run list' 3
expect_calls "  and nothing is watched" '^run watch' 0

fresh; export GH_SHIM_DISPATCH_OUT="" GH_SHIM_LIST="[{\"databaseId\":81,$NEW},{\"databaseId\":82,$NEW}]"
run_sut
expect "no URL, two runs since the dispatch" 1 "::error::more than one publish-agent-feed.yml dispatch run at $TAG since the dispatch; refusing to guess which is this job's; look at $WFURL"
expect "  and each candidate is a full run URL" 1 "$BASE/81"
expect "  (the second too)" 1 "$BASE/82"
expect_calls "  and nothing is watched" '^run watch' 0

fresh; export GH_SHIM_DISPATCH_FAIL="HTTP 403: Resource not accessible by integration"
run_sut
expect "the dispatch fails" 1 "::error::could not dispatch publish-agent-feed.yml at $TAG; look at $WFURL"
expect "  and says why" 1 "HTTP 403: Resource not accessible by integration"
expect_calls "  and nothing is watched" '^run watch' 0

# --- the watch ---------------------------------------------------------------
fresh; export GH_SHIM_WATCH_RC=1 GH_SHIM_VIEW='{"status":"completed","conclusion":"failure"}'
run_sut
expect "the watch fails, the run concluded failure" 1 "::error::the agent feed run concluded failure: $URL"
expect "  and the re-run advice is given" 1 "Re-run its failed jobs, or dispatch publish-agent-feed.yml again from $TAG."
expect_calls "  after reading the run once" '^run view 4242 --json status,conclusion$' 1

fresh; export GH_SHIM_WATCH_RC=1 GH_SHIM_VIEW='{"status":"completed","conclusion":"cancelled"}'
run_sut
expect "the watch fails, the run concluded cancelled" 1 "::error::the agent feed run concluded cancelled: $URL"
if printf '%s\n' "$out" | grep -q 'Re-run its failed jobs'; then
    echo "FAIL a cancelled run gets the failed-jobs advice"
    fail=$((fail + 1))
else
    echo "ok   a cancelled run does not get the failed-jobs advice"
    pass=$((pass + 1))
fi

fresh; export GH_SHIM_WATCH_RC=1 GH_SHIM_VIEW='{"status":"completed","conclusion":"skipped"}'
run_sut
expect "the watch fails, the run concluded skipped" 1 "::error::the agent feed run concluded skipped: $URL"

fresh; export GH_SHIM_WATCH_RC=1 GH_SHIM_VIEW='{"status":"completed","conclusion":"timed_out"}'
run_sut
expect "the watch fails, the run concluded timed_out" 1 "::error::the agent feed run concluded timed_out: $URL"

fresh; export GH_SHIM_WATCH_RC=1 GH_SHIM_VIEW='{"status":"completed","conclusion":null}'
run_sut
expect "the watch fails, completed with a null conclusion" 1 "::error::the agent feed run concluded with no conclusion: $URL"

fresh; export GH_SHIM_WATCH_RC=1 GH_SHIM_VIEW='{"status":"completed","conclusion":""}'
run_sut
expect "the watch fails, completed with an empty conclusion" 1 "::error::the agent feed run concluded with no conclusion: $URL"

fresh; export GH_SHIM_DISPATCH_OUT="" GH_SHIM_LIST="[{\"databaseId\":79,$NEW}]" GH_SHIM_WATCH_RC=1 GH_SHIM_VIEW='{"status":"completed","conclusion":"failure"}'
run_sut
expect "no URL, the watch fails, the failure names the found run" 1 "::error::the agent feed run concluded failure: $BASE/79"
expect_calls "  and the view is on that run" '^run view 79 --json status,conclusion$' 1

fresh; export GH_SHIM_WATCH_RC=1 GH_SHIM_VIEW='{"status":"in_progress","conclusion":null}'
run_sut
expect "the watch fails, the run is still in_progress" 1 "::error::could not watch the agent feed run to completion (status in_progress): check $URL"

fresh; export GH_SHIM_WATCH_RC=1 GH_SHIM_VIEW_FAIL="HTTP 502: Bad Gateway"
run_sut
expect "the watch fails and the read fails too" 1 "::error::the watch failed and the run's state could not be read, so it is unknown: $URL"

fresh; export GH_SHIM_WATCH_RC=1 GH_SHIM_VIEW='not json'
run_sut
expect "the watch fails and the read is not JSON" 1 "::error::the watch failed and the run's state could not be read, so it is unknown: $URL"

fresh; export GH_SHIM_WATCH_RC=1 GH_SHIM_VIEW='{"status":"completed","conclusion":"success"}'
run_sut
expect "the watch fails but the read says completed/success: the job passes" 0 "::notice::gh run watch could not follow the agent feed run, but the run concluded success: $URL"

echo
if [[ $fail -eq 0 ]]; then
    echo "agent-feed-dispatch: $pass case(s) behaved"
else
    echo "agent-feed-dispatch: $fail of $((pass + fail)) case(s) MISBEHAVED" >&2
    exit 1
fi
