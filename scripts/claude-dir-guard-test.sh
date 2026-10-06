#!/usr/bin/env bash
set -euo pipefail

# Proves scripts/claude-dir-guard.sh refuses what it claims to refuse. Runs
# THE SAME guard over throwaway repositories whose index holds one mutation
# each. Run by ci.yml's `secrets-guard` job and by `./dev lint`.
# A guard that has only ever seen a valid tree is indistinguishable from one
# that passes everything.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
GUARD="$SCRIPT_DIR/claude-dir-guard.sh"

# An inherited GIT_DIR (a `git rebase -x` from a linked worktree exports one)
# would point every fixture's `git init`/`git add -f` below at the REAL
# checkout, staging fake settings and hooks into its index. Drop the whole
# family before the first git call (as versioning-test.sh does); a control
# below proves the fixture's git dir is its own.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_OBJECT_DIRECTORY

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

pass=0
fail=0
n=0

record() {
    # record NAME OK(1|0) RC OUT
    if [[ "$2" -eq 1 ]]; then
        pass=$((pass + 1))
        echo "ok   $1"
    else
        fail=$((fail + 1))
        echo "FAIL $1 (exit $3)"
        echo "$4" | sed 's/^/     /'
    fi
}

# fresh_repo: a new empty repository holding one allowed config file, so
# every refusal below is refused NEXT TO a legitimate entry.
fresh_repo() {
    n=$((n + 1))
    repo="$work/repo$n"
    mkdir -p "$repo/.claude/sassy-dog"
    git -C "$repo" init -q
    printf -- '---\nmerge_queue: true\n---\n' >"$repo/.claude/sassy-dog/take-it.md"
    git -C "$repo" add -f .claude/sassy-dog/take-it.md
}

# track PATH [symlink-target]: create PATH in $repo and force it into the index,
# then assert it is really tracked, so no case can pass vacuously.
track() {
    local path="$1" target="${2:-}"
    mkdir -p "$repo/$(dirname "$path")"
    if [[ -n "$target" ]]; then
        ln -s "$target" "$repo/$path"
    else
        echo '{}' >"$repo/$path"
    fi
    git -C "$repo" add -f -- "$path"
    git -C "$repo" ls-files --error-unmatch -- "$path" >/dev/null 2>&1 \
        || { echo "mutation failed: $path is not tracked" >&2; exit 1; }
}

# expect NAME pass|fail [OUTPUT_REGEX...]
expect() {
    local name="$1" want="$2" out rc=0 ok=1 re
    shift 2
    out="$(REPO_DIR="$repo" "$GUARD" 2>&1)" || rc=$?
    if [[ "$want" == pass && $rc -ne 0 ]]; then ok=0; fi
    if [[ "$want" == fail && $rc -eq 0 ]]; then ok=0; fi
    for re in "$@"; do
        grep -qE -- "$re" <<<"$out" || ok=0
    done
    record "$name" "$ok" "$rc" "$out"
}

# The committed tree must pass.
repo="$(cd "$SCRIPT_DIR/.." && pwd)"
expect "the committed tree passes" pass

# The allowance itself, and a repository with no .claude at all.
fresh_repo
# Asked in git's own terms, like versioning-test.sh: from a repository's top
# level `--git-dir` answers `.git` unless an inherited GIT_DIR overrides it.
case "$( cd "$repo" && git rev-parse --git-dir )" in
    .git) record "fixtures use their own git dir, not an inherited GIT_DIR" 1 0 "" ;;
    *) record "fixtures use their own git dir, not an inherited GIT_DIR" 0 1 "$( cd "$repo" && git rev-parse --git-dir )"; exit 1 ;;
esac
expect "one config file passes" pass '1 file\(s\)'
n=$((n + 1)); repo="$work/repo$n"; mkdir -p "$repo"; git -C "$repo" init -q
expect "a repository with nothing named .claude passes" pass

# What the .gitignore block exists to keep out, force-added.
fresh_repo; track .claude/settings.json
expect "a tracked settings.json fails, named" fail '\.claude/settings\.json' 'Untrack them'
fresh_repo; track .claude/settings.local.json
expect "a tracked settings.local.json fails" fail '\.claude/settings\.local\.json'
fresh_repo; track .claude/hooks/sassydog-post-edit.sh
expect "a tracked hook script fails" fail '\.claude/hooks/sassydog-post-edit\.sh'
fresh_repo; track ".claude/hooks/a b.sh"
expect "a path with a space is named whole" fail '\.claude/hooks/a b\.sh'

# Inside the allowance, but not what the plugin reads.
fresh_repo; track .claude/sassy-dog/sub/deep.md
expect "a nested file under sassy-dog/ fails" fail '\.claude/sassy-dog/sub/deep\.md'
fresh_repo; track .claude/sassy-dog/run.sh
expect "a non-markdown file under sassy-dog/ fails" fail '\.claude/sassy-dog/run\.sh'
fresh_repo; track .claude/sassy-dog/send-it.md; git -C "$repo" update-index --chmod=+x .claude/sassy-dog/send-it.md
expect "an executable config file fails" fail 'send-it\.md \(mode 100755\)'
fresh_repo; track .claude/sassy-dog/link.md ../settings.json
expect "a symlinked config file fails" fail 'link\.md \(mode 120000\)'
fresh_repo; track .claude/sassy-dog/send-it.MD
expect "an upper-case .MD extension fails" fail 'send-it\.MD'
fresh_repo
git -C "$repo" update-index --add --cacheinfo "160000,$(printf '%040d' 1),.claude/sassy-dog/vendored.md"
git -C "$repo" ls-files --error-unmatch -- .claude/sassy-dog/vendored.md >/dev/null 2>&1 \
    || { echo "mutation failed: the gitlink is not tracked" >&2; exit 1; }
expect "a gitlink under the allowance fails" fail 'vendored\.md \(mode 160000\)'

# The bypasses a prefix match or a case-sensitive one would miss.
fresh_repo; track .claude/sassy-dog-evil/x.md
expect "a sibling sharing the prefix fails" fail '\.claude/sassy-dog-evil/x\.md'
n=$((n + 1)); repo="$work/repo$n"; mkdir -p "$repo"; git -C "$repo" init -q; track .CLAUDE/settings.json
expect "an upper-case .CLAUDE/ fails" fail '\.CLAUDE/settings\.json'
n=$((n + 1)); repo="$work/repo$n"; mkdir -p "$repo"; git -C "$repo" init -q; track .Claude/sassy-dog/take-it.md
expect "the allowance is exact-case" fail '\.Claude/sassy-dog/take-it\.md'
fresh_repo; track app/.claude/settings.json
expect "a nested .claude/ directory fails" fail 'app/\.claude/settings\.json'
n=$((n + 1)); repo="$work/repo$n"; mkdir -p "$repo/evil"; git -C "$repo" init -q
echo '{}' >"$repo/evil/settings.json"; git -C "$repo" add evil/settings.json; track .claude evil
expect "a symlink named .claude fails" fail '^::error::tracked outside .*: \.claude \(mode 120000\)'
n=$((n + 1)); repo="$work/repo$n"; mkdir -p "$repo/elsewhere"; git -C "$repo" init -q
track .claude/sassy-dog ../elsewhere
expect "sassy-dog/ itself as a symlink fails" fail '\.claude/sassy-dog \(mode 120000\)'
n=$((n + 1)); repo="$work/repo$n"; mkdir -p "$repo"; git -C "$repo" init -q; track .claude
expect "a regular file named .claude fails" fail '^::error::tracked outside .*: \.claude \(mode 100644\)'

# Names that merely contain the word are not .claude.
fresh_repo; track .claudex/y; track docs/x.claude; track notes/claude.md
expect "lookalike names are not refused" pass

# Fail closed when the listing cannot be read at all.
repo="$work/not-a-repo"; mkdir -p "$repo"
expect "a directory that is not a repository fails closed" fail 'could not be read'

echo "claude-dir-guard: $pass passed, $fail failed"
[[ $fail -eq 0 ]]
