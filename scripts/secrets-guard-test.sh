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
#
# THE ALLOWANCES ARE PAIRS (#491): `release-agent.yml:publish` and
# `publish-agent-feed.yml:agent-feed`, each a `file:job` that may read a secret
# under a job-level `environment: prd`. The same mutations are therefore run
# against EACH pair, and a refusal for one says nothing about the other: a
# secret in a sibling job, a secret above `jobs:`, the environment line removed
# or moved, the job renamed, a new job, and the scoped file deleted. A pair's
# job name does not carry over to the other pair's file (a `publish` job added
# to publish-agent-feed.yml is not allowed). publish-feed.yml — which held the
# agent's feed job until #491 moved it out — has no allowance left, and the
# cases that used to prove its scoped job now prove that.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
ROOT_DIR="$( cd "$SCRIPT_DIR/.." && pwd )"
GUARD="$SCRIPT_DIR/secrets-guard.sh"
SRC="$ROOT_DIR/.github/workflows"

# The pairs under test: `file|scoped job|a sibling job in the same file`. The
# sibling is a job that exists today in that file and is NOT the scoped one;
# the preflight below asserts it, so a renamed job fails here loudly instead of
# hollowing a case out.
PAIRS=(
    "release-agent.yml|publish|build"
    "publish-agent-feed.yml|agent-feed|agent-eligibility"
)

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

# insert_before FILE ANCHOR_REGEX TEXT — the same, ahead of the matching line.
insert_before() {
    local file="$1" anchor="$2" text="$3"
    grep -qE "$anchor" "$file" || { echo "anchor not found in $file: $anchor" >&2; exit 1; }
    awk -v anchor="$anchor" -v ins="$text" '
        BEGIN { gsub(/<NL>/, "\n", ins) }
        !done && $0 ~ anchor { print ins; print; done = 1; next }
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

# --- preflight: the fixtures the cases below lean on ------------------------
# Each pair's file must hold the scoped job and its sibling as two-space job
# keys, and its ONLY `environment: prd` line must sit inside the scoped job —
# or the "environment removed" cases would be removing some other line.
for pair in "${PAIRS[@]}"; do
    IFS='|' read -r file job sibling <<< "$pair"
    for key in "$job" "$sibling"; do
        grep -qxF "  $key:" "$SRC/$file" \
            || { echo "$file has no job '$key' — the cases below no longer test what they claim" >&2; exit 1; }
    done
    owner="$(awk '
        /^  [A-Za-z0-9_-]+:[[:space:]]*$/ { cur = $1; sub(/:$/, "", cur) }
        /^    environment: prd$/ { print cur }
    ' "$SRC/$file")"
    if [[ "$owner" != "$job" ]]; then
        echo "$file's 'environment: prd' line(s) sit in '${owner:-<none>}', not only in '$job' — the cases below no longer test what they claim" >&2
        exit 1
    fi
done

# --- each pair: the allowance is by file AND job ----------------------------
for pair in "${PAIRS[@]}"; do
    IFS='|' read -r file job sibling <<< "$pair"
    wf="$work/wf/$file"
    other_job=""
    other_file=""
    for p2 in "${PAIRS[@]}"; do
        IFS='|' read -r f2 j2 _ <<< "$p2"
        if [[ "$f2" != "$file" ]]; then other_file="$f2"; other_job="$j2"; fi
    done
    echo "--- $file:$job"

    # A secret in a sibling job, in every spelling GitHub resolves.
    for form in "$SECRET" "$SECRET_INDEXED" "$SECRET_TOJSON" "$SECRET_FORMAT" "$SECRET_CASED"; do
        fresh_copy "$work/wf"
        insert_after "$wf" "^  $sibling:\$" "    env:<NL>      LEAK: $form"
        expect refuse "$file: a secret in the sibling job '$sibling' ($form)"
    done

    # The opener on one line and the context word on the next — a YAML plain
    # scalar continues across lines, and GitHub evaluates the joined expression.
    fresh_copy "$work/wf"
    insert_after "$wf" "^  $sibling:\$" "    env:<NL>      LEAK: $OPEN<NL>        secrets.SOLADOR_AGENT_SIGNING_PRIVATE_KEY }}"
    expect refuse "$file: the expression opener and the secrets word on different lines"

    # Above `jobs:`.
    fresh_copy "$work/wf"
    insert_before "$wf" '^jobs:$' "env:<NL>  LEAK: $SECRET<NL>"
    grep -qx 'env:' "$wf" || { echo "multi-line insertion did not split lines" >&2; exit 1; }
    expect refuse "$file: a workflow-level env secret above jobs:"

    # The environment line, removed in every way that is not the one spelling.
    fresh_copy "$work/wf"
    replace_line "$wf" "    environment: prd" "    # environment: prd"
    expect refuse "$file: $job with its environment line commented out"

    fresh_copy "$work/wf"
    replace_line "$wf" "    environment: prd" "    environment:<NL>      name: prd"
    grep -qx '      name: prd' "$wf" || { echo "mapping form did not split lines" >&2; exit 1; }
    expect refuse "$file: environment written as a mapping (the guard requires the one spelling)"

    fresh_copy "$work/wf"
    replace_line "$wf" "    environment: prd" "    environment: staging"
    expect refuse "$file: $job under some other environment"

    fresh_copy "$work/wf"
    replace_line "$wf" "    environment: prd" "        environment: prd"
    expect refuse "$file: environment: prd indented as a step key, not the job's"

    # The environment line moved onto the SIBLING job while the secret stays in
    # the scoped one: the job-scoped condition is what refuses this; a guard
    # that only asked "is there an environment line somewhere" would not.
    fresh_copy "$work/wf"
    replace_line "$wf" "    environment: prd" "    # moved"
    insert_after "$wf" "^  $sibling:\$" "    environment: prd"
    expect refuse "$file: environment: prd on the sibling job, secret still in $job"

    fresh_copy "$work/wf"
    replace_line "$wf" "  $job:" "  $job-2:"
    expect refuse "$file: $job renamed (the allowance is by name)"

    # A new job, even under the protected environment.
    fresh_copy "$work/wf"
    printf '\n  sneaky:\n    runs-on: ubuntu-latest\n    environment: prd\n    steps:\n      - run: echo %s\n' "$SECRET" >> "$wf"
    expect refuse "$file: a new job, even under environment: prd"

    fresh_copy "$work/wf"
    printf '\n  sneaky:  # a job key carrying a trailing comment\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo %s\n' "$SECRET" >> "$wf"
    expect refuse "$file: a new job whose key line carries a trailing comment"

    fresh_copy "$work/wf"
    printf '\n  "sneaky":\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo %s\n' "$SECRET" >> "$wf"
    expect refuse "$file: a new job written with a quoted key"

    fresh_copy "$work/wf"
    printf '\n  sneaky: { runs-on: ubuntu-latest, steps: [ { run: "echo %s" } ] }\n' "$SECRET" >> "$wf"
    expect refuse "$file: a job written as a one-line flow mapping"

    # A job whose name is a PREFIX of the scoped job's: the allowance is the
    # exact word, and a plain substring match would hand it over.
    fresh_copy "$work/wf"
    printf '\n  %s:\n    runs-on: ubuntu-latest\n    environment: prd\n    steps:\n      - run: echo %s\n' "${job%?}" "$SECRET" >> "$wf"
    expect refuse "$file: a job named '${job%?}' (a prefix of '$job') is not allowed"

    # The OTHER pair's job name, added here: the allowance is the pair, not the
    # job's name alone.
    fresh_copy "$work/wf"
    printf '\n  %s:\n    runs-on: ubuntu-latest\n    environment: prd\n    steps:\n      - run: echo %s\n' "$other_job" "$SECRET" >> "$work/wf/$file"
    expect refuse "$file: a '$other_job' job (the other pair's name) is not allowed here"

    # And this pair's job name in the other pair's file.
    fresh_copy "$work/wf"
    printf '\n  %s:\n    runs-on: ubuntu-latest\n    environment: prd\n    steps:\n      - run: echo %s\n' "$job" "$SECRET" >> "$work/wf/$other_file"
    expect refuse "$other_file: gaining a '$job' job ($file's allowed name) is not allowed there"

    # The wrong directory is not a clean directory, per pair.
    fresh_copy "$work/wf"
    rm "$wf"
    expect refuse "a workflows directory without $file"
done

# --- publish-feed.yml: the cockpit's feed holds no allowance any more -------
fresh_copy "$work/wf"
insert_after "$work/wf/publish-feed.yml" '^          NOTES: ' "          LEAK: $SECRET"
expect refuse "a secret in publish-feed.yml's feed job"

fresh_copy "$work/wf"
printf '\n  agent-feed:\n    runs-on: ubuntu-latest\n    environment: prd\n    steps:\n      - run: echo %s\n' "$SECRET" >> "$work/wf/publish-feed.yml"
expect refuse "an agent-feed job back in publish-feed.yml, even under environment: prd"

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

fresh_copy "$work/wf"
insert_after "$work/wf/ci.yml" '^jobs:$' "  leak:<NL>    uses: ./.github/workflows/x.yml<NL>    secrets:  # all of them<NL>      inherit"
expect refuse "secrets: with a trailing comment, then inherit"

fresh_copy "$work/wf"
insert_after "$work/wf/ci.yml" '^jobs:$' "  leak:<NL>    uses: ./.github/workflows/x.yml<NL>    secrets:<NL><NL>      inherit"
expect refuse "secrets: then a blank line, then inherit"

fresh_copy "$work/wf"
insert_after "$work/wf/ci.yml" '^jobs:$' "  leak: { uses: ./.github/workflows/x.yml, secrets: inherit }"
expect refuse "a reusable-workflow call written as a flow mapping with secrets: inherit"

fresh_copy "$work/wf"
insert_after "$work/wf/ci.yml" '^jobs:$' "  leak:<NL>    uses: ./.github/workflows/x.yml<NL>    with:<NL>      secrets: inherit"
expect refuse "secrets: inherit nested one level deeper"

fresh_copy "$work/wf"
insert_after "$work/wf/ci.yml" '^jobs:$' "  leak:<NL>    uses: ./.github/workflows/x.yml<NL>    \"secrets\": inherit"
expect refuse "a quoted secrets key with inherit"

# --- the wrong directory is not a clean directory ----------------------------
rm -rf "$work/wf"
mkdir -p "$work/wf"
expect refuse "an empty workflows directory"

# Every scoped file gone while other workflows remain: still refused, and for
# each pair (the per-pair loop above covers one at a time).
fresh_copy "$work/wf"
for pair in "${PAIRS[@]}"; do
    IFS='|' read -r file _ <<< "$pair"
    rm "$work/wf/$file"
done
expect refuse "a workflows directory without any scoped file"

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
