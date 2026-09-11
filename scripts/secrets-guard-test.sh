#!/usr/bin/env bash
set -euo pipefail

# Proves scripts/secrets-guard.sh refuses what it claims to refuse.
#
# Dependency-free bash in the shape of agent/deploy/lib_test.sh: copies the
# real workflows into a scratch directory, applies one mutation per case, and
# runs THE SAME guard script CI runs over the copy. Run by ci.yml's
# `secrets-guard` job after the real scan, and by hand:
#
#   scripts/secrets-guard-test.sh
#
# A guard that only ever sees the valid tree is indistinguishable from one
# that passes everything — which is exactly how the first cut of the job-key
# tracking shipped attributing a trailing-comment job's secret to the job
# above it. Every shape below is a shape that was, or could plausibly be,
# written by someone who did not mean to widen the allowance.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
ROOT_DIR="$( cd "$SCRIPT_DIR/.." && pwd )"
GUARD="$SCRIPT_DIR/secrets-guard.sh"
SRC="$ROOT_DIR/.github/workflows"

# The literal a leak looks like. Assembled so this file, too, never carries
# the bare expression opener on one line.
OPEN='${'
OPEN="$OPEN{"
SECRET="$OPEN secrets.SOLADOR_AGENT_SIGNING_PRIVATE_KEY }}"
SECRET_INDEXED="$OPEN secrets['SOLADOR_AGENT_SIGNING_PRIVATE_KEY'] }}"
SECRET_TOJSON="$OPEN toJSON(secrets) }}"
SECRET_FORMAT="$OPEN format('{0}', secrets.SOLADOR_AGENT_SIGNING_PRIVATE_KEY) }}"
SECRET_CASED="$OPEN Secrets.SOLADOR_AGENT_SIGNING_PRIVATE_KEY }}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

pass=0
fail=0

# fresh_copy DIR — a clean copy of the real workflows.
fresh_copy() {
    rm -rf "$1"
    mkdir -p "$1"
    cp "$SRC"/*.yml "$1"/
}

# insert_after FILE ANCHOR_REGEX TEXT — append TEXT after the first line
# matching ANCHOR_REGEX; fails loudly if the anchor is absent, because a
# mutation that did nothing would make its case vacuous. `<NL>` in TEXT
# becomes a newline: a real one in an awk `-v` value is a syntax error on
# macOS awk, and whether `\n` is expanded there differs between awks.
insert_after() {
    local file="$1" anchor="$2" text="$3"
    grep -qE "$anchor" "$file" || { echo "anchor not found in $file: $anchor" >&2; exit 1; }
    awk -v anchor="$anchor" -v ins="$text" '
        BEGIN { gsub(/<NL>/, "\n", ins) }
        !done && $0 ~ anchor { print; print ins; done = 1; next }
        { print }
    ' "$file" > "$file.tmp" && mv "$file.tmp" "$file"
}

# replace_line FILE EXACT_LINE TEXT — replace the first exact match; `<NL>`
# in TEXT becomes a newline, as in insert_after.
replace_line() {
    local file="$1" from="$2" to="$3"
    grep -qxF -- "$from" "$file" || { echo "line not found in $file: $from" >&2; exit 1; }
    awk -v from="$from" -v to="$to" '
        BEGIN { gsub(/<NL>/, "\n", to) }
        !done && $0 == from { print to; done = 1; next }
        { print }
    ' "$file" > "$file.tmp" && mv "$file.tmp" "$file"
}

# expect VERDICT LABEL — run the guard over $work/wf and compare. A refusal
# is exit 1 WITH a `::error` line: any other non-zero exit is the guard
# crashing (an unbound variable, a broken awk), which must not score as a
# refusal — a guard that dies on every input would otherwise pass every
# negative case here.
expect() {
    local verdict="$1" label="$2" out rc=0
    out="$(WORKFLOWS_DIR="$work/wf" bash "$GUARD" 2>&1)" || rc=$?
    local ok=false
    case "$verdict" in
        pass)   [[ $rc -eq 0 ]] && ok=true ;;
        refuse) [[ $rc -eq 1 ]] && grep -q '^::error' <<< "$out" && ok=true ;;
    esac
    if $ok; then
        echo "ok   $label ($verdict)"
        pass=$((pass + 1))
    else
        echo "FAIL $label: expected $verdict, exit $rc"
        echo "$out" | sed 's/^/     /'
        fail=$((fail + 1))
    fi
}

# --- the valid tree ---------------------------------------------------------
fresh_copy "$work/wf"
expect pass "the real workflows"

# The append cases below (`printf >>`) put a new job AFTER the last one in
# publish-feed.yml, and they prove what they claim only if that last job is
# `agent-feed` — a stray appended after some other job would be reported as
# stray for a different reason. Asserted here so a reordering of the workflow
# does not silently hollow those cases out.
last_job="$(awk '/^  [A-Za-z0-9_-]+:[[:space:]]*$/ { j = $1 } END { sub(/:$/, "", j); print j }' "$SRC/publish-feed.yml")"
if [[ "$last_job" != "agent-feed" ]]; then
    echo "publish-feed.yml's last job is '$last_job', not agent-feed — the append cases below no longer test what they claim" >&2
    exit 1
fi

# --- publish-feed.yml: the allowance is by job -----------------------------
fresh_copy "$work/wf"
insert_after "$work/wf/publish-feed.yml" '^          NOTES: ' "          LEAK: $SECRET"
expect refuse "a secret in the desktop feed job"

fresh_copy "$work/wf"
insert_after "$work/wf/publish-feed.yml" '^          NOTES: ' "          LEAK: $SECRET_INDEXED"
expect refuse "a secret via secrets['X'] in the desktop feed job"

fresh_copy "$work/wf"
insert_after "$work/wf/publish-feed.yml" '^          NOTES: ' "          LEAK: $SECRET_TOJSON"
expect refuse "toJSON(secrets) in the desktop feed job"

fresh_copy "$work/wf"
insert_after "$work/wf/publish-feed.yml" '^          NOTES: ' "          LEAK: $SECRET_FORMAT"
expect refuse "format('{0}', secrets.X) in the desktop feed job"

fresh_copy "$work/wf"
insert_after "$work/wf/publish-feed.yml" '^          NOTES: ' "          LEAK: $SECRET_CASED"
expect refuse "Secrets.X (GitHub resolves contexts case-insensitively)"

fresh_copy "$work/wf"
printf '\n  sneaky: { runs-on: ubuntu-latest, steps: [ { run: "echo %s" } ] }\n' "$SECRET" >> "$work/wf/publish-feed.yml"
expect refuse "a job written as a one-line flow mapping"

fresh_copy "$work/wf"
replace_line "$work/wf/publish-feed.yml" "    environment: prd" "    # environment: prd"
expect refuse "agent-feed with its environment line commented out"

fresh_copy "$work/wf"
replace_line "$work/wf/publish-feed.yml" "    environment: prd" "    environment:<NL>      name: prd"
grep -qx '      name: prd' "$work/wf/publish-feed.yml" || { echo "mapping form did not split lines" >&2; exit 1; }
expect refuse "environment written as a mapping (the guard requires the one spelling)"

fresh_copy "$work/wf"
replace_line "$work/wf/publish-feed.yml" "    environment: prd" "    environment: staging"
expect refuse "agent-feed under some other environment"

# The environment line moved onto the DESKTOP job while the secret stays in
# agent-feed: the job-scoped condition (`cur == job`) is what refuses this;
# a guard that only asked "is there an environment line somewhere" would not.
fresh_copy "$work/wf"
replace_line "$work/wf/publish-feed.yml" "    environment: prd" "    # moved"
insert_after "$work/wf/publish-feed.yml" '^  feed:$' "    environment: prd"
expect refuse "environment: prd on the desktop job, secret still in agent-feed"

fresh_copy "$work/wf"
replace_line "$work/wf/publish-feed.yml" "    environment: prd" "        environment: prd"
expect refuse "environment: prd indented as a step key, not the job's"

fresh_copy "$work/wf"
replace_line "$work/wf/publish-feed.yml" "  agent-feed:" "  agent-feed-2:"
expect refuse "agent-feed renamed (the allowance is by name)"

fresh_copy "$work/wf"
printf '\n  sneaky:\n    runs-on: ubuntu-latest\n    environment: prd\n    steps:\n      - run: echo %s\n' "$SECRET" >> "$work/wf/publish-feed.yml"
expect refuse "a new job in publish-feed.yml, even under environment: prd"

fresh_copy "$work/wf"
printf '\n  sneaky:  # a job key carrying a trailing comment\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo %s\n' "$SECRET" >> "$work/wf/publish-feed.yml"
expect refuse "a new job whose key line carries a trailing comment"

fresh_copy "$work/wf"
printf '\n  "sneaky":\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo %s\n' "$SECRET" >> "$work/wf/publish-feed.yml"
expect refuse "a new job written with a quoted key"

fresh_copy "$work/wf"
insert_after "$work/wf/publish-feed.yml" '^permissions:$' "env:<NL>  LEAK: $SECRET"
grep -qx 'env:' "$work/wf/publish-feed.yml" || { echo "multi-line insertion did not split lines" >&2; exit 1; }
expect refuse "a workflow-level env secret above jobs:"

# The opener on one line and the context word on the next — a YAML plain
# scalar continues across lines, and GitHub evaluates the joined expression.
fresh_copy "$work/wf"
insert_after "$work/wf/publish-feed.yml" '^          NOTES: ' "          LEAK: $OPEN<NL>            secrets.SOLADOR_AGENT_SIGNING_PRIVATE_KEY }}"
expect refuse "the expression opener and the secrets word on different lines"

# --- other workflows: no allowance at all -----------------------------------
fresh_copy "$work/wf"
printf 'name: x\non: push\njobs:\n  j:\n    runs-on: ubuntu-latest\n    environment: prd\n    steps:\n      - run: echo %s\n' "$SECRET" > "$work/wf/new.yml"
expect refuse "a new workflow file, even under environment: prd"

fresh_copy "$work/wf"
printf 'name: x\non: push\njobs:\n  j:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo %s\n' "$SECRET" > "$work/wf/new.yaml"
expect refuse "a new workflow file with the .yaml spelling"

fresh_copy "$work/wf"
insert_after "$work/wf/ci.yml" '^      - name: Format check$' "        env:<NL>          LEAK: $SECRET"
expect refuse "a secret in ci.yml"

fresh_copy "$work/wf"
insert_after "$work/wf/ci.yml" '^jobs:$' "  leak:<NL>    uses: ./.github/workflows/x.yml<NL>    secrets: inherit"
expect refuse "secrets: inherit in ci.yml"

fresh_copy "$work/wf"
insert_after "$work/wf/ci.yml" '^jobs:$' "  leak:<NL>    uses: ./.github/workflows/x.yml<NL>    secrets: \"inherit\""
expect refuse "secrets: \"inherit\" (quoted) in ci.yml"

fresh_copy "$work/wf"
insert_after "$work/wf/ci.yml" '^jobs:$' "  leak:<NL>    uses: ./.github/workflows/x.yml<NL>    secrets:<NL>      inherit"
expect refuse "secrets: with inherit on the next line, in ci.yml"

# --- the wrong directory is not a clean directory ----------------------------
rm -rf "$work/wf"
mkdir -p "$work/wf"
expect refuse "an empty workflows directory"

fresh_copy "$work/wf"
rm "$work/wf/publish-feed.yml"
expect refuse "a workflows directory without publish-feed.yml"

# --- release.yml stays allowed wholesale ------------------------------------
fresh_copy "$work/wf"
printf '\n  extra:\n    runs-on: ubuntu-latest\n    environment: prd\n    steps:\n      - run: echo %s\n' "$SECRET" >> "$work/wf/release.yml"
expect pass "release.yml gaining another secret reference"

echo
if [[ $fail -eq 0 ]]; then
    echo "secrets-guard: $pass case(s) behaved"
else
    echo "secrets-guard: $fail of $((pass + fail)) case(s) MISBEHAVED" >&2
    exit 1
fi
