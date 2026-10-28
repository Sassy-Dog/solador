#!/usr/bin/env bash
set -euo pipefail

# Fails when any chain of tracked AGENTS.md/CLAUDE.md files exceeds the org's
# agent-guidance budget, 32,768 bytes (#638). A chain is the root AGENTS.md plus
# every tracked AGENTS.md and CLAUDE.md in the directories down to one working
# directory; the budget is Codex's default `project_doc_max_bytes`. Codex reads
# the files root first, cuts the one that crosses the budget at that byte with
# nothing above a log-level warning, and drops every deeper file, so an
# over-budget guide is a silently truncated one. Bytes are counted raw
# (`wc -c`), HTML comments included.
#
#   scripts/agent-guide-size-guard.sh
#   REPO_DIR=/some/repo scripts/agent-guide-size-guard.sh   (the override is how
#                                                            agent-guide-size-guard-test.sh
#                                                            points it at a fixture)
#
# Fails closed: no tracked root AGENTS.md, a file that cannot be read or
# measured, and a git listing that cannot be taken are failures, never a pass.
# Run by ci.yml's `secrets-guard` job on every PR and merge group, and by
# `./dev lint`.
#
# Not measured: an AGENTS.override.md (Codex reads it in place of AGENTS.md) and
# untracked files. A tracked CLAUDE.md is counted in its directory's chain
# beside AGENTS.md (it should hold only the `@AGENTS.md` import, a few bytes):
# its own bytes are counted, but `@path` imports are NOT followed, so an import
# of a further file is not measured. Depth belongs in docs/*.md, named from
# AGENTS.md by a "read before changing X" pointer, never behind an `@path`
# import.

BUDGET=32768

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
ROOT_DIR="$( cd "$SCRIPT_DIR/.." && pwd )"
REPO_DIR="${REPO_DIR:-$ROOT_DIR}"

fail() {
    echo "::error::$1"
    exit 1
}

listing="$(mktemp)"
trap 'rm -f "$listing"' EXIT

if ! git -C "$REPO_DIR" ls-files -z -- 'AGENTS.md' ':(glob)**/AGENTS.md' 'CLAUDE.md' ':(glob)**/CLAUDE.md' >"$listing" 2>&1; then
    fail "git ls-files failed in $REPO_DIR, so the tracked AGENTS.md/CLAUDE.md files could not be listed: $(tr '\0' ' ' <"$listing")"
fi

files=()
while IFS= read -r -d '' path; do
    files+=("$path")
done <"$listing"

has_root=0
for path in ${files[@]+"${files[@]}"}; do
    if [[ "$path" == AGENTS.md ]]; then has_root=1; fi
done
if [[ $has_root -eq 0 ]]; then
    fail "no tracked root AGENTS.md in $REPO_DIR; this repo must have one, and the budget check must not pass vacuously"
fi

# size_of PATH: sets SIZE to the raw byte count, or fails. Called directly, never
# in a command substitution: `fail` must exit THIS shell, not a subshell.
SIZE=0
size_of() {
    local n
    if [[ ! -f "$REPO_DIR/$1" || ! -r "$REPO_DIR/$1" ]]; then
        fail "$1 is tracked but cannot be read, so its size could not be taken"
    fi
    if ! n="$(wc -c <"$REPO_DIR/$1")"; then
        fail "wc -c failed on $1"
    fi
    n="${n//[[:space:]]/}"
    if [[ ! "$n" =~ ^[0-9]+$ ]]; then
        fail "wc -c gave a non-numeric size for $1: '$n'"
    fi
    SIZE="$n"
}

# One chain per tracked guide file: its own directory and every ancestor.
worst=0
for leaf in "${files[@]}"; do
    dir="$(dirname "$leaf")"
    total=0
    names=()
    while :; do
        if [[ "$dir" == . ]]; then pre=""; else pre="$dir/"; fi
        for path in "${files[@]}"; do
            if [[ "$path" == "${pre}AGENTS.md" || "$path" == "${pre}CLAUDE.md" ]]; then
                size_of "$path"
                total=$((total + SIZE))
                names+=("$path")
            fi
        done
        if [[ "$dir" == . ]]; then break; fi
        dir="$(dirname "$dir")"
    done
    if [[ $total -gt $worst ]]; then worst=$total; fi
    if [[ $total -gt $BUDGET ]]; then
        fail "the guide chain ending at $leaf is $total bytes (${names[*]}), over the $BUDGET-byte budget; Codex cuts the file that crosses it and drops every deeper one. Move depth into docs/*.md and name it from AGENTS.md."
    fi
done
echo "Every AGENTS.md/CLAUDE.md chain is within $BUDGET bytes (largest: $worst)."
