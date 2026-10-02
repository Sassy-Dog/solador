#!/usr/bin/env bash
set -euo pipefail

# Proves scripts/agent-feed-guard.sh refuses what it claims to refuse (#491).
#
# The guard is the logic of publish-agent-feed.yml's `agent-eligibility` job,
# and that workflow can only be exercised by publishing a real agent release —
# the moment a wrong guard freezes a fleet. So the logic is a script, and this
# runs the REAL script (the same bytes the workflow runs) against a stub `gh`
# on PATH, the way scripts/versioning-test.sh stubs `git` and `gh`. Dependency-
# free bash in that file's shape, plus `jq` (which the guard itself needs, and
# which every runner image ships; a Mac older than macOS 15 may not). Without
# `jq` it SKIPS, loudly, the way agent/deploy/lib_test.sh skips without
# `minisign` — unless SOLADOR_AGENT_FEED_GUARD_TEST_REQUIRE_JQ=1, which CI sets
# so that there a missing `jq` is a failure and not a green skip. Bash 3.2-clean: CI runs it
# under macOS stock /bin/bash as well as bash 5 on Linux, and the script under
# test runs under "$BASH", the interpreter this harness was started with. Run by
# ci.yml's `agent-tests` and `rust-workspace` jobs, by `./dev test`, and by hand:
#
#   scripts/agent-feed-guard-test.sh
#
# THE STUB IS ARGUMENT-SENSITIVE, and that is the point of it. It answers only
# for the exact endpoints the guard must ask about (`repos/<repo>/releases`,
# `repos/<repo>/releases/latest`), rejects any flag the guard has no reason to
# pass, and returns ONE PAGE of the release list unless `--paginate` is given. A
# stub that answered the same way to any question would pass a guard that asked
# the wrong one — a guard that forgot `--paginate` would find the newest release
# on page one and say nothing. Every call's argv lands in $GH_SHIM_CALLS, and the
# pagination case reads it.
#
# What it holds:
#   newest   the candidate is the newest (pass); an older candidate (refused,
#            naming the newest); an older candidate with --force (pass, and
#            says what it overrode); a newer release on the SECOND api page
#            (found, so the older candidate is refused) with a control showing
#            the stub really does hide page two without --paginate; the
#            candidate itself on page two (pass); a draft or a prerelease that
#            sorts newer (ignored); a newer cockpit `v*` release (ignored);
#            numeric, not lexical, ordering across a digit-count boundary and
#            across a month and a year (a three-digit patch beside the next
#            month's .1 included); no agent release at all and a candidate
#            missing from the list (refused, --force or not: force replays an
#            older PUBLISHED release, it does not feed an unpublished one); a
#            malformed published tag (reported, not winning, not wedging); a
#            malformed candidate (usage error); an unreadable list, a failing
#            `gh` and a non-JSON reply (each refused, --force or not); a bad
#            flag.
#   latest   a `v*` tag (pass); an `agent-v*` tag (refused, with the
#            `gh release edit <tag> --latest=false` remedy for THAT tag); any
#            other non-`v*` tag (refused); an unreadable or empty answer
#            (refused); a stray argument.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
GUARD="$SCRIPT_DIR/agent-feed-guard.sh"

if ! command -v jq >/dev/null 2>&1; then
    if [[ "${SOLADOR_AGENT_FEED_GUARD_TEST_REQUIRE_JQ:-}" == "1" ]]; then
        echo "agent-feed-guard-test: jq is required here and is not installed (the guard reads the release list with it)" >&2
        exit 1
    fi
    echo "SKIP agent-feed-guard-test: jq is not installed (brew install jq); CI requires it, so this is a skip here and a failure there" >&2
    exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

REPO="Acme/solador"

# --- The stub `gh` ----------------------------------------------------------
#   GH_SHIM_REPO       the repository the guard must ask about
#   GH_SHIM_PAGE1/2    files holding one page of the release list each; page 2
#                      is served only to a `--paginate` request
#   GH_SHIM_LIST_FAIL  `releases` fails with this text
#   GH_SHIM_LATEST     file holding the `releases/latest` body
#   GH_SHIM_LATEST_FAIL  `releases/latest` fails with this text
#   GH_SHIM_RAW        served verbatim for `releases` (a reply that is not JSON)
mkdir -p "$work/ghshim"
cat > "$work/ghshim/gh" <<'SHIM'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${GH_SHIM_CALLS:-/dev/null}"
if [ "${1:-}" != api ]; then
    echo "gh shim: unexpected call: $*" >&2
    exit 2
fi
shift
paginate=0
endpoint=""
for a in "$@"; do
    case "$a" in
        --paginate) paginate=1 ;;
        -*) echo "gh shim: unexpected flag: $a" >&2; exit 2 ;;
        *) endpoint="$a" ;;
    esac
done
case "$endpoint" in
    "repos/$GH_SHIM_REPO/releases")
        if [ -n "${GH_SHIM_LIST_FAIL:-}" ]; then echo "$GH_SHIM_LIST_FAIL" >&2; exit 1; fi
        if [ -n "${GH_SHIM_RAW:-}" ]; then printf '%s\n' "$GH_SHIM_RAW"; exit 0; fi
        cat "$GH_SHIM_PAGE1"
        if [ "$paginate" = 1 ] && [ -n "${GH_SHIM_PAGE2:-}" ]; then cat "$GH_SHIM_PAGE2"; fi
        ;;
    "repos/$GH_SHIM_REPO/releases/latest")
        if [ -n "${GH_SHIM_LATEST_FAIL:-}" ]; then echo "$GH_SHIM_LATEST_FAIL" >&2; exit 1; fi
        cat "$GH_SHIM_LATEST"
        ;;
    *)
        echo "gh shim: unexpected endpoint: $endpoint" >&2
        exit 2
        ;;
esac
SHIM
chmod +x "$work/ghshim/gh"

# page FILE SPEC… — write one page of the release list. SPEC is
# `tag` (published), `tag:draft` or `tag:pre`, the three states the guard tells
# apart; the objects carry the keys GitHub's API does and nothing the guard
# reads besides.
page() {
    local file="$1" spec tag state sep=""
    shift
    printf '[' > "$file"
    for spec in "$@"; do
        tag="${spec%%:*}"
        state="${spec#*:}"
        [[ "$state" != "$spec" ]] || state=published
        case "$state" in
            published) printf '%s{"tag_name":"%s","draft":false,"prerelease":false}' "$sep" "$tag" >> "$file" ;;
            draft)     printf '%s{"tag_name":"%s","draft":true,"prerelease":false}' "$sep" "$tag" >> "$file" ;;
            pre)       printf '%s{"tag_name":"%s","draft":false,"prerelease":true}' "$sep" "$tag" >> "$file" ;;
            *) echo "page: unknown state $state" >&2; exit 1 ;;
        esac
        sep=","
    done
    printf ']\n' >> "$file"
}

latest_body() { printf '{"tag_name":"%s","draft":false,"prerelease":false}\n' "$1" > "$work/latest.json"; }

pass=0
fail=0

# run_guard ARGS… — run the guard under this harness's interpreter with the
# stub first on PATH. Sets $out (stdout + stderr) and $rc.
out=""
rc=0
run_guard() {
    rc=0
    out="$(PATH="$work/ghshim:$PATH" GITHUB_REPOSITORY="$REPO" GH_SHIM_REPO="$REPO" \
        GH_SHIM_CALLS="$work/gh.calls" GH_SHIM_PAGE1="$work/page1.json" \
        GH_SHIM_PAGE2="${GH_SHIM_PAGE2:-}" GH_SHIM_LATEST="$work/latest.json" \
        "${BASH:-bash}" "$GUARD" "$@" 2>&1)" || rc=$?
}

# expect VERDICT LABEL [REGEX…] — a `pass` is exit 0; a `refuse` is exit 1 WITH
# an `::error` line (any other non-zero exit is the guard crashing — an unbound
# variable, a broken jq — and must not score as a refusal, or a guard that dies
# on every input would pass every negative case); a `usage` is exit 2. Each
# REGEX must also match the output: the reason has to be the RIGHT one.
expect() {
    local verdict="$1" label="$2" ok=false re
    shift 2
    case "$verdict" in
        pass)   [[ $rc -eq 0 ]] && ok=true ;;
        refuse) [[ $rc -eq 1 ]] && grep -q '^::error' <<< "$out" && ok=true ;;
        usage)  [[ $rc -eq 2 ]] && ok=true ;;
    esac
    if $ok; then
        for re in "$@"; do
            if ! grep -Eq -- "$re" <<< "$out"; then
                ok=false
                label="$label (output lacks /$re/)"
            fi
        done
    fi
    if $ok; then
        echo "ok   $label ($verdict)"
        pass=$((pass + 1))
    else
        echo "FAIL $label: expected $verdict, exit $rc"
        echo "$out" | sed 's/^/     /'
        fail=$((fail + 1))
    fi
}

# expect_absent REGEX LABEL — the output of the last run must NOT match.
expect_absent() {
    if grep -Eq -- "$1" <<< "$out"; then
        echo "FAIL $2: output matches /$1/"
        echo "$out" | sed 's/^/     /'
        fail=$((fail + 1))
    else
        echo "ok   $2"
        pass=$((pass + 1))
    fi
}

# --- newest: the candidate is, or is not, the newest published agent release -
unset GH_SHIM_PAGE2 GH_SHIM_LIST_FAIL GH_SHIM_RAW GH_SHIM_LATEST_FAIL

page "$work/page1.json" agent-v2026.10.15 v2026.10.14 agent-v2026.10.12 agent-latest:pre
run_guard newest agent-v2026.10.15
expect pass "the candidate is the newest" 'is the newest published'

run_guard newest agent-v2026.10.12
expect refuse "an older candidate" 'agent-v2026\.10\.15 is' 'freeze guard' 'force=true'

run_guard newest agent-v2026.10.12 --force
expect pass "an older candidate with --force" 'forced past the freeze guard' 'agent-v2026\.10\.15'

# Re-running the publish at the CURRENT tag after a failed upload is the
# recovery path the design depends on: it must not need --force.
run_guard newest agent-v2026.10.15
expect pass "re-running at the current tag (the recovery path)"

# --- the release list is read completely ------------------------------------
# A cockpit release list is long and newest-created-first, so a newer agent
# release can sit beyond the first page. Page one holds only older agent releases.
page "$work/page1.json" v2026.10.14 v2026.10.13 agent-v2026.10.12 v2026.10.11
page "$work/page2.json" agent-v2026.10.15 v2026.10.10
export GH_SHIM_PAGE2="$work/page2.json"

: > "$work/gh.calls"
run_guard newest agent-v2026.10.12
expect refuse "a newer agent release beyond the first page is found" 'agent-v2026\.10\.15 is'
grep -Eq '^api --paginate repos/Acme/solador/releases$' "$work/gh.calls" \
    && { echo "ok   the guard asked for the whole list with --paginate"; pass=$((pass + 1)); } \
    || { echo "FAIL the guard did not pass --paginate; calls were:"; sed 's/^/     /' "$work/gh.calls"; fail=$((fail + 1)); }

# Negative control on the FIXTURE: without --paginate the stub serves page one
# only, so a guard that forgot the flag would see agent-v2026.10.12 as the newest
# and pass the older candidate. Shown here, so the case above refuses because
# the guard read page two and not because the fixture never hid it.
rc=0
out="$(PATH="$work/ghshim:$PATH" GH_SHIM_REPO="$REPO" GH_SHIM_PAGE1="$work/page1.json" \
    GH_SHIM_PAGE2="$work/page2.json" gh api "repos/$REPO/releases")" || rc=$?
if [[ $rc -eq 0 ]] && ! grep -q 'agent-v2026.10.15' <<< "$out" && grep -q 'agent-v2026.10.12' <<< "$out"; then
    echo "ok   control: the stub hides page two from a request without --paginate"
    pass=$((pass + 1))
else
    echo "FAIL control: the stub served page two without --paginate (the pagination case proves nothing)"
    fail=$((fail + 1))
fi

# The candidate itself on page two, the older agent release on page one.
page "$work/page1.json" v2026.10.14 v2026.10.13 agent-v2026.10.12
page "$work/page2.json" agent-v2026.10.15
run_guard newest agent-v2026.10.15
expect pass "the candidate is the newest and is on the second page"
unset GH_SHIM_PAGE2

# --- only a published, stable, agent-v* release counts ----------------------
page "$work/page1.json" agent-v2026.10.16:draft agent-v2026.10.15 agent-v2026.10.12
run_guard newest agent-v2026.10.15
expect pass "a draft that sorts newer is ignored"

page "$work/page1.json" agent-v2026.10.16:pre agent-v2026.10.15
run_guard newest agent-v2026.10.15
expect pass "a prerelease that sorts newer is ignored"

page "$work/page1.json" agent-latest:pre agent-v2026.10.15
run_guard newest agent-v2026.10.15
expect pass "the rolling agent-latest prerelease is ignored"

page "$work/page1.json" v2099.1.1 agent-v2026.10.15
run_guard newest agent-v2026.10.15
expect pass "a newer cockpit v* release is not an agent release"
expect_absent '::warning' "a cockpit release is filtered out, not reported as a malformed agent tag"

# A draft or prerelease is not a candidate that can be "the newest published".
page "$work/page1.json" agent-v2026.10.16:draft agent-v2026.10.15
run_guard newest agent-v2026.10.16
expect refuse "a draft candidate is not among the published releases" 'not among'

page "$work/page1.json" agent-v2026.10.16:pre agent-v2026.10.15
run_guard newest agent-v2026.10.16
expect refuse "a prerelease candidate is not among the published releases" 'not among'

# --- ordering is CalVer, numerically per component --------------------------
page "$work/page1.json" agent-v2026.10.10 agent-v2026.10.9
run_guard newest agent-v2026.10.9
expect refuse "numeric, not lexical: .9 is older than .10" 'agent-v2026\.10\.10 is'
run_guard newest agent-v2026.10.10
expect pass "numeric, not lexical: .10 is the newest"

page "$work/page1.json" agent-v2026.9.99 agent-v2026.10.1 agent-v2026.2.500
run_guard newest agent-v2026.10.1
expect pass "a later month outranks a higher patch in an earlier one"
run_guard newest agent-v2026.9.99
expect refuse "an earlier month is older whatever its patch" 'agent-v2026\.10\.1 is'

# A patch can have three or four digits (N counts releases in a month, not
# commits, but nothing bounds it). These pin the weight a month carries: a key
# that gave the month fewer digits than that would rank .9.150 above .10.1.
page "$work/page1.json" agent-v2026.9.150 agent-v2026.10.1
run_guard newest agent-v2026.10.1
expect pass "a three-digit patch does not outrank the next month's .1"
run_guard newest agent-v2026.9.150
expect refuse "...and is older than the next month's .1" 'agent-v2026\.10\.1 is'

page "$work/page1.json" agent-v2026.1.1200 agent-v2026.2.1
run_guard newest agent-v2026.2.1
expect pass "a four-digit patch does not outrank the next month's .1"
run_guard newest agent-v2026.1.1200
expect refuse "...and is older than the next month's .1 (four digits)" 'agent-v2026\.2\.1 is'

page "$work/page1.json" agent-v2027.1.1 agent-v2026.12.400
run_guard newest agent-v2027.1.1
expect pass "a later year outranks a later month"
run_guard newest agent-v2026.12.400
expect refuse "an earlier year is older" 'agent-v2027\.1\.1 is'

# --- a list that cannot vouch for the candidate -----------------------------
page "$work/page1.json" v2026.10.14 v2026.10.13
run_guard newest agent-v2026.10.15
expect refuse "no agent release at all: the candidate is not among them" 'not among'
# --force is for REPLAYING an older published release. It never feeds a release
# that is not published, and the freeze remedy text would be the wrong advice.
run_guard newest agent-v2026.10.15 --force
expect refuse "no agent release at all, even forced" 'not among'
expect_absent 'would replace a newer' "...and it does not give the freeze remedy for a candidate that is not published"

page "$work/page1.json" agent-v2026.10.16:draft agent-v2026.10.12
run_guard newest agent-v2026.10.16 --force
expect refuse "a draft candidate, even forced (pulled back to a draft during the approval wait)" 'not among'

page "$work/page1.json"
run_guard newest agent-v2026.10.15
expect refuse "an empty release list" 'not among'

# A published release tagged agent-v… that is not agent-vYYYY.M.N cannot come
# from the mint and cannot be ordered: reported and ignored, never the winner
# and never a reason to stop.
page "$work/page1.json" agent-vnext agent-v2026.10.015 agent-v2026.13.1 agent-v2026.10.15
run_guard newest agent-v2026.10.15
expect pass "malformed published agent-v tags are ignored" '::warning::ignoring the published release .agent-vnext.'

# --- the candidate itself must be well-formed -------------------------------
page "$work/page1.json" agent-v2026.10.15
run_guard newest v2026.10.15
expect usage "a cockpit tag is not an agent candidate" 'not an agent-vYYYY\.M\.N'
run_guard newest agent-v2026.10.015
expect usage "a zero-padded patch is not a candidate"
run_guard newest agent-v2026.13.1
expect usage "month 13 is not a candidate"
run_guard newest agent-latest
expect usage "agent-latest is not a candidate"
run_guard newest agent-v2026.10.15 --forse
expect usage "an unknown flag"
run_guard newest
expect usage "newest without a tag"
run_guard
expect usage "no subcommand"
run_guard publish
expect usage "an unknown subcommand"

# --- what cannot be read is refused, forced or not --------------------------
export GH_SHIM_LIST_FAIL="HTTP 502: Bad Gateway"
run_guard newest agent-v2026.10.15
expect refuse "the release list cannot be read" 'could not list'
run_guard newest agent-v2026.10.15 --force
expect refuse "the release list cannot be read, even forced" 'could not list'
unset GH_SHIM_LIST_FAIL

export GH_SHIM_RAW='<html>not json</html>'
run_guard newest agent-v2026.10.15
expect refuse "a reply that is not JSON" 'not JSON'
run_guard newest agent-v2026.10.15 --force
expect refuse "a reply that is not JSON, even forced" 'not JSON'
unset GH_SHIM_RAW

rc=0
out="$(PATH="$work/ghshim:$PATH" GITHUB_REPOSITORY="" "${BASH:-bash}" "$GUARD" newest agent-v2026.10.15 2>&1)" || rc=$?
expect usage "no repository to ask about" 'GITHUB_REPOSITORY'

# --- latest: the cockpit keeps the releases/latest slot ---------------------
latest_body v2026.10.14
run_guard latest
expect pass "releases/latest names a cockpit v* release" 'v2026\.10\.14'

latest_body agent-v2026.10.15
run_guard latest
expect refuse "releases/latest names an agent-v release" 'agent-v2026\.10\.15' 'gh release edit agent-v2026\.10\.15 --latest=false'

latest_body agent-latest
run_guard latest
expect refuse "releases/latest names the rolling agent-latest" 'gh release edit agent-latest --latest=false'

latest_body nightly
run_guard latest
expect refuse "releases/latest names any other non-v tag" 'gh release edit nightly --latest=false'

printf '{"tag_name":null}\n' > "$work/latest.json"
run_guard latest
expect refuse "releases/latest names no tag" 'names no tag'

printf 'not json\n' > "$work/latest.json"
run_guard latest
expect refuse "releases/latest is not JSON" 'not JSON'

latest_body v2026.10.14
export GH_SHIM_LATEST_FAIL="HTTP 404: Not Found"
run_guard latest
expect refuse "releases/latest cannot be read" 'could not read'
unset GH_SHIM_LATEST_FAIL

run_guard latest extra
expect usage "latest takes no argument"

echo
if [[ $fail -eq 0 ]]; then
    echo "agent-feed-guard: $pass case(s) behaved"
else
    echo "agent-feed-guard: $fail of $((pass + fail)) case(s) MISBEHAVED" >&2
    exit 1
fi
