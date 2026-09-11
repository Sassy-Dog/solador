#!/usr/bin/env bash
set -euo pipefail

# The secrets guard (#307, widened by #391): which workflow jobs may read a
# secret. Run by ci.yml's `secrets-guard` job on every PR, and by `./dev lint`.
#
#   scripts/secrets-guard.sh                  scan .github/workflows
#   WORKFLOWS_DIR=some/dir scripts/secrets-guard.sh
#                                             scan a copy — what
#                                             secrets-guard-test.sh does to
#                                             prove the negative cases
#
# The rule: a secret may be read ONLY by a job that declares `environment: prd`
# — every credential-holding job in release.yml (allowed wholesale, by file),
# and exactly one job in publish-feed.yml, `agent-feed`, which signs
# `agent-latest.json` when a release is published. Nothing else. The desktop
# feed job beside it stays credential-free, and so does every other workflow.
#
# Why a gate and not a comment: Solador is public, so a fork's pull request
# runs ci.yml, and a secret reference added there resolves to the EMPTY STRING
# rather than erroring — the job that "needed" it fails for some
# unrelated-looking reason, or silently does the wrong thing. Catching it on
# the PR that adds it is the only cheap moment.
#
# Why the publish-feed.yml allowance is by JOB, not by file: a secret reference
# in its desktop job, in a new job, in `agent-feed` with its `environment: prd`
# line removed, or above `jobs:` altogether, must each fail. The job-key
# tracking below therefore has to see EVERY two-space key line under `jobs:`
# — a key carrying a trailing comment (`  leak:  # …`), written quoted, or
# holding a one-line flow mapping is a job too, and the first cut of this
# guard, which recognised only bare keys, attributed such a job's secret to
# whatever job came before it.
#
# Why it lives in scripts/ rather than inline in ci.yml: so the same bytes
# CI runs can be pointed at a mutated copy of the workflows and shown to
# refuse each shape above (secrets-guard-test.sh), rather than trusted to.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
ROOT_DIR="$( cd "$SCRIPT_DIR/.." && pwd )"
WORKFLOWS_DIR="${WORKFLOWS_DIR:-$ROOT_DIR/.github/workflows}"

allow="release.yml"
scoped="publish-feed.yml"
scoped_job="agent-feed"

# The `secrets` context, as a word, in any spelling GitHub accepts inside an
# expression — `secrets.X`, `secrets['X']`, `toJSON(secrets)`,
# `format('{0}', secrets.X)` — matched CASE-INSENSITIVELY, because GitHub
# resolves context names that way (`Secrets.X` reads the secret). The awk
# below applies it only while inside `${{ … }}`, tracking an expression that
# opens on one line and closes on a later one, so splitting the opener from
# the word does not slip past. Written with bracket expressions so the literal
# `${{` never appears in this file, and so awk receives every pattern through
# `-v` intact: `-v` strips a backslash escape, turning a `\$` into a bare `$`
# anchor that matches nothing.
word='(^|[^a-z0-9_])secrets([^a-z0-9_]|$)'
opener='[$][{][{]'
closer='[}][}]'
# `secrets: inherit` on a reusable-workflow call — anywhere on the line (a
# job written as a flow mapping puts it mid-line), with the key or the value
# quoted or not, or with the value on a later line (a trailing comment or
# blank lines in between).
inherit='(^|[^a-z0-9_])secrets["'"'"']?:[[:space:]]*["'"'"']?inherit'
inherit_key='(^|[^a-z0-9_])secrets["'"'"']?:[[:space:]]*(#.*)?$'
inherit_val='^[[:space:]]*["'"'"']?inherit["'"'"']?[[:space:]]*(#.*)?$'
skippable='^[[:space:]]*(#.*)?$'

# report FILE — one line per finding, tab-separated:
#   secret <line> <job>         a secret reference, and the job it sits in
#                               (`<no job>` above/outside `jobs:`)
#   environment <line> <job>    a job-level `environment: prd`
#
# Every two-space key line under `jobs:` moves `cur` FIRST — to the job's
# name for a plain `name:` line, otherwise to a sentinel — and only then is the
# line tested, so a job written as a one-line flow mapping is both a key line
# and a possible reference. No rule above the test may `next`.
report() {
    awk -v word="$word" -v opener="$opener" -v closer="$closer" \
        -v inherit="$inherit" -v inherit_key="$inherit_key" -v inherit_val="$inherit_val" \
        -v skippable="$skippable" '
        /^jobs:[[:space:]]*$/ { in_jobs = 1; cur = ""; next }
        in_jobs && /^[^[:space:]#]/ { in_jobs = 0; cur = "" }
        in_jobs && /^  [^[:space:]#]/ {
            if ($0 ~ /^  [A-Za-z0-9_-]+:[[:space:]]*$/) { cur = $1; sub(/:$/, "", cur) }
            else { cur = "<unrecognised job key at line " NR ">" }
        }
        {
            line = tolower($0)
            hit = 0
            # Walk the line: text inside an expression (opened here or on an
            # earlier line) is searched for the context word.
            rest = line
            while (length(rest) > 0) {
                if (open) {
                    end = match(rest, closer)
                    body = (end ? substr(rest, 1, end - 1) : rest)
                    if (body ~ word) hit = 1
                    if (!end) break
                    open = 0
                    rest = substr(rest, end + RLENGTH)
                } else {
                    start = match(rest, opener)
                    if (!start) break
                    open = 1
                    rest = substr(rest, start + RLENGTH)
                }
            }
            if (line ~ inherit) hit = 1
            if (pending_inherit && line ~ inherit_val) hit = 1
            # A `secrets:` key with its value still to come stays pending
            # across comment-only and blank lines, and is cleared by the
            # first line that carries anything else.
            if (line ~ inherit_key) pending_inherit = 1
            else if (line !~ skippable) pending_inherit = 0
            if (hit) print "secret\t" NR "\t" (cur == "" ? "<no job>" : cur)
        }
        in_jobs && cur == job && /^    environment:[[:space:]]*prd[[:space:]]*$/ { print "environment\t" NR "\t" cur }
    ' job="$scoped_job" "$1"
}

fail=0
seen_scoped=0
seen_any=0
for wf in "$WORKFLOWS_DIR"/*.yml "$WORKFLOWS_DIR"/*.yaml; do
    [[ -e "$wf" ]] || continue
    seen_any=1
    name="$(basename "$wf")"
    rel=".github/workflows/$name"
    [[ "$name" == "$allow" ]] && continue

    findings="$(report "$wf")"
    secrets="$(awk -F '\t' '$1 == "secret"' <<< "$findings")"

    if [[ "$name" == "$scoped" ]]; then
        seen_scoped=1
        # Every secret reference must sit inside the one allowed job, and that
        # job must declare the protected environment at the job level.
        stray="$(awk -F '\t' -v job="$scoped_job" '$1 == "secret" && $3 != job' <<< "$findings")"
        if [[ -n "$stray" ]]; then
            echo "::error file=$rel::$name references a secret outside its $scoped_job job; only that job may"
            echo "$stray"
            fail=1
        fi
        if [[ -n "$secrets" ]] && ! grep -q $'^environment\t' <<< "$findings"; then
            echo "::error file=$rel::$name's $scoped_job job references secrets but does not carry the job-level line 'environment: prd' (exactly that spelling)"
            echo "The environment is what scopes the credential to a reviewed, v*-tag-only run." >&2
            fail=1
        fi
        continue
    fi

    if [[ -n "$secrets" ]]; then
        echo "::error file=$rel::$name references secrets; only $allow (and $scoped's $scoped_job job) may"
        echo "$secrets"
        fail=1
    fi
done

# A directory with nothing in it is not a clean bill of health: it is the
# wrong directory. Fail closed rather than print the all-clear over nothing.
if [[ $seen_any -eq 0 || $seen_scoped -eq 0 ]]; then
    echo "::error::$WORKFLOWS_DIR holds no $scoped (or no workflows at all) — nothing was checked"
    exit 1
fi

if [[ $fail -ne 0 ]]; then
    echo "A public repo runs ci.yml on fork pull requests." >&2
    echo "If a new workflow genuinely needs credentials, give it its own" >&2
    echo "environment and add it to the allowlist in this script — on purpose." >&2
    exit 1
fi
echo "No workflow outside $allow references a secret, except $scoped's $scoped_job job under environment: prd."
