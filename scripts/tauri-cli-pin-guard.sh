#!/usr/bin/env bash
set -euo pipefail

# Fails when the pinned Tauri CLI and the locked `tauri` runtime are on
# different minor trains (#485). Run by ci.yml's `secrets-guard` job on every
# PR, and by `./dev lint`.
#
#   scripts/tauri-cli-pin-guard.sh
#   CONFIG_FILE=a/config.sh LOCK_FILE=a/Cargo.lock scripts/tauri-cli-pin-guard.sh
#                                   (the override is how tauri-cli-pin-guard-test.sh
#                                    points it at a mutated copy)
#
# `TAURI_CLI_VERSION` in scripts/config.sh names the CLI that bundles
# Solador.app and the NSIS installer; Dependabot moves `Cargo.lock`'s `tauri`
# and cannot see a shell variable. #444 moved tauri to 2.12.1 while the pin
# stayed at 2.11.4 and nothing failed until #483 fixed it by hand.
#
# Only major.minor must match, never the patch: `tauri-cli` publishes its own
# patch numbers (when tauri 2.11.5 shipped the newest CLI was 2.11.4).
#
# No cargo and no network: the lockfile is read as text, by its exact
# `name = "tauri"` stanza — `tauri-build`, `tauri-utils` and the rest share the
# prefix and must not be mistaken for it. The config is grepped rather than
# sourced, so no code in it runs.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
ROOT_DIR="$( cd "$SCRIPT_DIR/.." && pwd )"
CONFIG_FILE="${CONFIG_FILE:-$SCRIPT_DIR/config.sh}"
LOCK_FILE="${LOCK_FILE:-$ROOT_DIR/Cargo.lock}"

fail() {
    echo "::error::$1"
    exit 1
}

[[ -f "$CONFIG_FILE" ]] || fail "$CONFIG_FILE not found, so the Tauri CLI pin could not be read"
[[ -f "$LOCK_FILE" ]] || fail "$LOCK_FILE not found, so the locked tauri version could not be read"

pin="$(sed -n 's/^export TAURI_CLI_VERSION="\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)"[[:space:]]*$/\1/p' "$CONFIG_FILE")"
if [[ -z "$pin" || "$pin" == *$'\n'* ]]; then
    fail "scripts/config.sh: expected exactly one line 'export TAURI_CLI_VERSION=\"X.Y.Z\"' in $CONFIG_FILE"
fi

# The stanza is `name = "tauri"` immediately followed by `version = "…"`.
locked="$(awk '
    /^name = "tauri"$/ { want = 1; next }
    want && /^version = "/ { gsub(/^version = "|"$/, ""); print; want = 0; next }
    { want = 0 }
' "$LOCK_FILE")"
if [[ -z "$locked" || "$locked" == *$'\n'* ]]; then
    fail "$LOCK_FILE: expected exactly one 'tauri' package stanza with a version"
fi

minor_of() { echo "${1%.*}"; }

if [[ "$(minor_of "$pin")" != "$(minor_of "$locked")" ]]; then
    fail "TAURI_CLI_VERSION is $pin but Cargo.lock locks tauri $locked — different minor trains. Edit TAURI_CLI_VERSION in scripts/config.sh to the newest published tauri-cli on $(minor_of "$locked").x (only major.minor must match)."
fi
echo "TAURI_CLI_VERSION $pin and Cargo.lock's tauri $locked share major.minor $(minor_of "$locked")."
