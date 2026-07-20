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

# Prefer the CI-pinned swiftformat that ./dev lint/format cache under
# build/lint-tools/ — keeps hook output identical to the lint gate. Hooks must
# stay instant, so never download here; fall back to a PATH install, and exit
# clean with neither (the pre-push lint gate still catches formatting).
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SWIFTFORMAT=$(ls "$ROOT"/build/lint-tools/swiftformat-*/swiftformat 2>/dev/null | sort -V | tail -1)
[[ -n "$SWIFTFORMAT" && -x "$SWIFTFORMAT" ]] || SWIFTFORMAT=$(command -v swiftformat || true)
[[ -n "$SWIFTFORMAT" ]] || exit 0

file=$(printf '%s' "$payload" | jq -r '.tool_input.file_path // empty')
[[ -n "$file" && "$file" == *.swift && -f "$file" ]] || exit 0

# Mirror the excluded paths in .swiftlint.yml / .swiftformat.
case "$file" in
    */build/*|*/.build/*|*/Scripts/*|*/Assets.xcassets/*) exit 0 ;;
esac

"$SWIFTFORMAT" "$file" >/dev/null 2>&1 || true
exit 0
