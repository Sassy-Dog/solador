#!/usr/bin/env bash
set -euo pipefail

# Fails unless the ShellCheck on hand is exactly the version pinned in
# scripts/config.sh (#548). Run by `./dev lint`, and by ci.yml's ShellCheck
# step against the binary it just downloaded.
#
#   scripts/shellcheck-pin-guard.sh
#   SHELLCHECK=/path/to/shellcheck scripts/shellcheck-pin-guard.sh
#   CONFIG_FILE=a/config.sh scripts/shellcheck-pin-guard.sh
#                       (the overrides are how shellcheck-pin-guard-test.sh
#                        points it at stub binaries)
#
# ShellCheck adds checks in minor releases (0.9.0 missed an SC2218 that 0.11.0
# did not, and the reverse is as likely), so the match is exact. The config is
# read as text, never sourced, so no code in it runs. An unreadable answer from
# the binary fails, it is never taken as a match.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
CONFIG_FILE="${CONFIG_FILE:-$SCRIPT_DIR/config.sh}"
SHELLCHECK="${SHELLCHECK:-shellcheck}"

fail() {
    echo "::error::$1" >&2
    exit 1
}

[[ -f "$CONFIG_FILE" ]] || fail "$CONFIG_FILE not found, so the ShellCheck pin could not be read"

pin="$(sed -n 's/^export SHELLCHECK_VERSION="\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)"[[:space:]]*$/\1/p' "$CONFIG_FILE")"
if [[ -z "$pin" || "$pin" == *$'\n'* ]]; then
    fail "expected exactly one line 'export SHELLCHECK_VERSION=\"X.Y.Z\"' in $CONFIG_FILE"
fi

if ! out="$("$SHELLCHECK" --version 2>&1)"; then
    fail "could not run '$SHELLCHECK --version'. Install ShellCheck $pin: 'brew install shellcheck' only if Homebrew still carries $pin (it tracks the latest release); otherwise download the shellcheck-v$pin.darwin.(aarch64|x86_64).tar.xz asset from https://github.com/koalaman/shellcheck/releases/tag/v$pin, extract it to a directory, and put that directory first on PATH or run SHELLCHECK=<dir>/shellcheck ./dev lint."
fi

found="$(sed -n 's/^version:[[:space:]]*\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)[[:space:]]*$/\1/p' <<< "$out")"
if [[ -z "$found" || "$found" == *$'\n'* ]]; then
    fail "'$SHELLCHECK --version' did not report a version line, so it cannot be checked against the pin $pin"
fi

if [[ "$found" != "$pin" ]]; then
    fail "ShellCheck is $found but scripts/config.sh pins $pin, and CI runs $pin. Install $pin: 'brew install shellcheck' only if Homebrew still carries $pin (it tracks the latest release); otherwise download the shellcheck-v$pin.darwin.(aarch64|x86_64).tar.xz asset from https://github.com/koalaman/shellcheck/releases/tag/v$pin, extract it to a directory, and put that directory first on PATH or run SHELLCHECK=<dir>/shellcheck ./dev lint."
fi
echo "ShellCheck $found matches the pin $pin."
