#!/usr/bin/env bash
# generated-by: ai-agent-skills:refresh-sassydog-hooks | template: sassydog-post-edit | template-version: 1
#
# PostToolUse (Edit|Write) formatter/linter dispatcher. Reads the hook event
# JSON on stdin, extracts the edited file's path, routes by extension.
#
# Contract:
#   - formatters fix in place, silently                    -> exit 0
#   - linters with UNFIXABLE findings print them to stderr -> exit 2
#     (the harness feeds stderr back to Claude for an immediate fix)
#   - anything unexpected (no path, missing tool, no route) -> exit 0, never block
set -uo pipefail

payload=$(cat)
file=$(printf '%s' "$payload" | jq -r '.tool_input.file_path // empty' 2>/dev/null)
[ -z "$file" ] && exit 0
[ -f "$file" ] || exit 0

lint_fail() {  # $1 = tool label, $2 = findings
    {
        echo "$1 findings in $file (fix these now):"
        echo "$2" | head -20
    } >&2
    exit 2
}

case "$file" in
    *.sh)
        command -v shellcheck >/dev/null 2>&1 || exit 0
        if ! out=$(shellcheck -S warning "$file" 2>&1); then
            lint_fail "shellcheck" "$out"
        fi
        ;;
    *.rs)
        command -v rustfmt >/dev/null 2>&1 || exit 0
        rustfmt "$file" >/dev/null 2>&1 || true
        ;;
esac

exit 0
