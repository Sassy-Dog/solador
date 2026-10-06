#!/usr/bin/env bash
set -euo pipefail

# Fails when git tracks anything named .claude other than the sassy-dog
# plugin's config, `.claude/sassy-dog/<name>.md`. Run by ci.yml's
# `secrets-guard` job on every PR and merge group, and by `./dev lint`.
#
#   scripts/claude-dir-guard.sh
#   REPO_DIR=/some/repo scripts/claude-dir-guard.sh   (the override is how
#                                                      claude-dir-guard-test.sh
#                                                      points it at a mutated repo)
#
# `.gitignore` keeps the rest of .claude/ out, but an ignore rule is only a
# default: `git add -f` tracks an ignored path anyway. A tracked
# .claude/settings.json is hooks that run with no trust prompt in
# parent-trusted, `claude -p`/SDK and cloud sessions, so the property is
# asserted on what git TRACKS, not on what .gitignore says. This keeps the
# merge queue from ever landing such a file. It cannot protect a working tree:
# checking a branch out silently overwrites a local ignored copy of the same
# path before CI has run, which is why contributor PRs are checked out into a
# worktree (CLAUDE.md, Security Considerations).
#
# Matched case-insensitively and at any depth: macOS volumes are
# case-insensitive, so a tracked `.CLAUDE/settings.json` lands in `.claude/`;
# Claude Code reads `.claude/` from whichever directory a session starts in,
# so `app/.claude/` counts too; and a tracked symlink named `.claude` would
# make a whole directory of ordinary files the settings. The one allowance is
# exact: `.claude/sassy-dog/<name>.md` at the repository root, one level deep,
# a regular non-executable file — what the plugin reads, and nothing else.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
ROOT_DIR="$( cd "$SCRIPT_DIR/.." && pwd )"
REPO_DIR="${REPO_DIR:-$ROOT_DIR}"

fail() {
    echo "::error::$1"
    exit 1
}

listing="$(mktemp)"
trap 'rm -f "$listing"' EXIT

# `**/.claude` matches a FILE or symlink so named; `**/.claude/**` matches
# everything under such a directory. -s for the mode, -z so no path is quoted
# or split.
if ! git -C "$REPO_DIR" ls-files -s -z -- \
    ':(icase,glob)**/.claude' ':(icase,glob)**/.claude/**' >"$listing" 2>&1; then
    fail "git ls-files failed in $REPO_DIR, so what is tracked under .claude could not be read: $(tr '\0' ' ' <"$listing")"
fi

refused=()
allowed=0
# Each record is "<mode> <object> <stage>\t<path>".
while IFS= read -r -d '' record; do
    mode="${record%% *}"
    path="${record#*$'\t'}"
    case "$path" in
        .claude/sassy-dog/*/*) ;;
        .claude/sassy-dog/*.md)
            if [[ "$mode" == 100644 ]]; then
                allowed=$((allowed + 1))
                continue
            fi
            ;;
    esac
    refused+=("$path (mode $mode)")
done <"$listing"

if [[ ${#refused[@]} -gt 0 ]]; then
    for entry in "${refused[@]}"; do
        echo "::error::tracked outside .claude/sassy-dog/<name>.md: $entry"
    done
    fail "${#refused[@]} tracked path(s) named .claude fall outside the one allowance, .claude/sassy-dog/<name>.md (a regular, non-executable file). Untrack them with 'git rm --cached', keeping any local copy; see the .claude block in .gitignore for why."
fi
echo "Nothing named .claude is tracked except $allowed file(s) under .claude/sassy-dog/."
