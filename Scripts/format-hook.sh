#!/usr/bin/env bash
# Claude Code PostToolUse hook: auto-format a Swift file right after it's edited.
#
# Reads the hook payload (JSON) on stdin and formats only `.tool_input.file_path`
# when it's a .swift file outside the excluded dirs. Formatting only — the
# pre-push gate (./dev lint) is what catches lint-rule/baseline issues.
#
# Failure-proof by design: every dependency is guarded and the script always
# exits 0, so a missing tool or odd payload can never block an edit.
set -uo pipefail

payload=$(cat)

command -v jq >/dev/null 2>&1 || exit 0
command -v swiftformat >/dev/null 2>&1 || exit 0

file=$(printf '%s' "$payload" | jq -r '.tool_input.file_path // empty')
[[ -n "$file" && "$file" == *.swift && -f "$file" ]] || exit 0

# Mirror the excluded paths in .swiftlint.yml / .swiftformat.
case "$file" in
    */build/*|*/.build/*|*/Scripts/*|*/Assets.xcassets/*) exit 0 ;;
esac

swiftformat "$file" >/dev/null 2>&1 || true
exit 0
