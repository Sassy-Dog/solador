#!/usr/bin/env bash
set -euo pipefail

# Proves scripts/agent-guide-size-guard.sh refuses what it claims to refuse
# (#638). Runs THE SAME guard over throwaway git repositories, one mutation
# each: a boundary pair (exactly 32,768 bytes passes, 32,769 fails), a nested
# AGENTS.md that takes a chain over the budget although each file alone is
# under it, a tree with no AGENTS.md, and a tracked file that cannot be read.
# Run by ci.yml's `secrets-guard` job and by `./dev lint`. A guard that has only
# ever seen a valid tree is indistinguishable from one that passes everything.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
GUARD="$SCRIPT_DIR/agent-guide-size-guard.sh"

# See claude-dir-guard-test.sh: an inherited GIT_DIR would aim the fixtures'
# git calls at the real checkout.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_OBJECT_DIRECTORY

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

pass=0
fail=0
n=0

fresh_repo() {
    n=$((n + 1))
    repo="$work/repo$n"
    mkdir -p "$repo"
    git -C "$repo" init -q
}

# put PATH BYTES: write a file of exactly BYTES bytes and track it.
put() {
    mkdir -p "$repo/$(dirname "$1")"
    head -c "$2" /dev/zero | tr '\0' 'a' >"$repo/$1"
    git -C "$repo" add -f -- "$1"
    local got
    got="$(wc -c <"$repo/$1")"
    got="${got//[[:space:]]/}"
    [[ "$got" -eq "$2" ]] || { echo "mutation failed: $1 is $got bytes, wanted $2" >&2; exit 1; }
}

# expect NAME pass|fail [OUTPUT_REGEX...]
expect() {
    local name="$1" want="$2" out rc=0 ok=1 re
    shift 2
    out="$(REPO_DIR="$repo" "$GUARD" 2>&1)" || rc=$?
    if [[ "$want" == pass && $rc -ne 0 ]]; then ok=0; fi
    if [[ "$want" == fail && $rc -eq 0 ]]; then ok=0; fi
    for re in "$@"; do
        if ! grep -Eq -- "$re" <<<"$out"; then ok=0; fi
    done
    if [[ $ok -eq 1 ]]; then
        pass=$((pass + 1))
        echo "ok   $name"
    else
        fail=$((fail + 1))
        echo "FAIL $name (exit $rc)"
        echo "$out" | sed 's/^/     /'
    fi
}

fresh_repo
put AGENTS.md 1000
expect "a small root passes" pass "within 32768"

fresh_repo
put AGENTS.md 32768
expect "a root of exactly 32768 bytes passes (the boundary)" pass "within 32768"

fresh_repo
put AGENTS.md 32769
expect "a root of 32769 bytes fails" fail "guide chain" "32769 bytes"

fresh_repo
put AGENTS.md 20000
put sub/AGENTS.md 20000
expect "a nested file fails when it takes the chain over, though each is under" fail "chain ending at sub/AGENTS.md" "40000 bytes"

fresh_repo
put AGENTS.md 20000
put sub/AGENTS.md 12768
expect "a chain of exactly 32768 bytes across two files passes" pass "within 32768"

fresh_repo
put AGENTS.md 20000
put a/AGENTS.md 12000
put b/AGENTS.md 12000
expect "sibling subtrees are separate chains and both pass" pass "within 32768"

fresh_repo
put AGENTS.md 32700
put CLAUDE.md 100
expect "a CLAUDE.md that takes the chain over the budget fails" fail "AGENTS.md CLAUDE.md" "32800 bytes"

fresh_repo
put AGENTS.md 1000
put CLAUDE.md 112
expect "a small CLAUDE.md beside AGENTS.md passes" pass "within 32768"

fresh_repo
put README.md 10
expect "a tree with no AGENTS.md fails (no vacuous pass)" fail "no tracked root AGENTS.md"

fresh_repo
put sub/AGENTS.md 10
expect "a nested AGENTS.md without a root still fails" fail "no tracked root AGENTS.md"

fresh_repo
put AGENTS.md 10
rm -f "$repo/AGENTS.md"
expect "a tracked but unreadable (missing) file fails closed" fail "cannot be read"

fresh_repo
put AGENTS.md 10
chmod 000 "$repo/AGENTS.md"
if [[ -r "$repo/AGENTS.md" ]]; then
    echo "skip chmod 000 case (readable anyway, e.g. running as root)"
else
    expect "an unreadable (mode 000) file fails closed" fail "cannot be read"
fi
chmod 644 "$repo/AGENTS.md"

# The guard in a directory that is not a git repository cannot list anything.
n=$((n + 1))
repo="$work/notgit$n"
mkdir -p "$repo"
head -c 10 /dev/zero | tr '\0' 'a' >"$repo/AGENTS.md"
expect "a directory git cannot list fails closed" fail "ls-files failed"

echo "$pass passed, $fail failed"
[[ $fail -eq 0 ]]
