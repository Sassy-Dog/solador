#!/usr/bin/env bash
set -euo pipefail

# Proves scripts/tauri-cli-pin-guard.sh refuses what it claims to refuse
# (#485). Runs THE SAME guard over mutated copies of the real config and
# lockfile. Run by ci.yml's `secrets-guard` job and by `./dev lint`.
# A guard that has only ever seen a valid tree is indistinguishable from one
# that passes everything.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
ROOT_DIR="$( cd "$SCRIPT_DIR/.." && pwd )"
GUARD="$SCRIPT_DIR/tauri-cli-pin-guard.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

pass=0
fail=0

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

# run_case NAME EXPECT(pass|fail) PIN LOCKED [OUTPUT_REGEX...]
# Copies the real files, then rewrites the pin and the tauri stanza's version
# (empty argument = leave as is). A mutation that changed nothing is an error:
# it would make the case vacuous.
run_case() {
    local name="$1" expect="$2" pin="$3" locked="$4"
    shift 4
    local cfg="$work/config.sh" lock="$work/Cargo.lock" out rc=0 ok=1 re
    cp "$SCRIPT_DIR/config.sh" "$cfg"
    cp "$ROOT_DIR/Cargo.lock" "$lock"
    if [[ -n "$pin" ]]; then
        sed "s/^export TAURI_CLI_VERSION=.*/export TAURI_CLI_VERSION=\"$pin\"/" "$cfg" > "$cfg.tmp" && mv "$cfg.tmp" "$cfg"
        grep -q "TAURI_CLI_VERSION=\"$pin\"" "$cfg" || { echo "mutation failed: $name" >&2; exit 1; }
    fi
    if [[ -n "$locked" ]]; then
        awk -v v="$locked" '
            /^name = "tauri"$/ { hit = 1; print; next }
            hit && /^version = / { print "version = \"" v "\""; hit = 0; next }
            { hit = 0; print }
        ' "$lock" > "$lock.tmp" && mv "$lock.tmp" "$lock"
        # Re-read through the `tauri` stanza only: sibling crates share versions.
        [[ "$(awk '/^name = "tauri"$/ { w = 1; next } w && /^version = / { gsub(/^version = "|"$/, ""); print; w = 0; next } { w = 0 }' "$lock")" == "$locked" ]] \
            || { echo "mutation failed: $name" >&2; exit 1; }
    fi
    out="$(CONFIG_FILE="$cfg" LOCK_FILE="$lock" "$GUARD" 2>&1)" || rc=$?
    if [[ "$expect" == pass && $rc -ne 0 ]]; then ok=0; fi
    if [[ "$expect" == fail && $rc -eq 0 ]]; then ok=0; fi
    for re in "$@"; do
        grep -qE -- "$re" <<< "$out" || ok=0
    done
    record "$name" "$ok" "$rc" "$out"
}

# The committed tree must pass.
run_case "the committed tree passes" pass "" ""
# A patch-only difference passes, in both directions.
run_case "patch-only difference (CLI behind) passes" pass "2.11.4" "2.11.5"
run_case "patch-only difference (CLI ahead) passes" pass "2.11.5" "2.11.4"
# A minor mismatch fails and names both versions and the file to edit.
run_case "a minor mismatch fails naming both versions and config.sh" fail "2.11.4" "2.12.1" \
    '2\.11\.4' '2\.12\.1' 'different minor trains' 'Edit TAURI_CLI_VERSION in scripts/config\.sh'
run_case "the CLI ahead by a minor fails" fail "2.13.0" "2.12.1" '2\.13\.0' '2\.12\.1'
run_case "a major mismatch fails" fail "3.12.1" "2.12.1" '3\.12\.1' '2\.12\.1'

cfg="$work/config.sh"
lock="$work/Cargo.lock"

# tauri-build must not stand in for tauri: a lock with no exact `tauri` stanza
# fails rather than reading a sibling's version.
cp "$SCRIPT_DIR/config.sh" "$cfg"
sed 's/^name = "tauri"$/name = "tauri-renamed"/' "$ROOT_DIR/Cargo.lock" > "$lock"
rc=0
out="$(CONFIG_FILE="$cfg" LOCK_FILE="$lock" "$GUARD" 2>&1)" || rc=$?
ok=0
if [[ $rc -ne 0 ]] && grep -q "tauri' package stanza" <<< "$out"; then ok=1; fi
record "no exact tauri stanza fails (siblings are not read)" "$ok" "$rc" "$out"

# A config with no pin line fails.
grep -v '^export TAURI_CLI_VERSION=' "$SCRIPT_DIR/config.sh" > "$cfg"
cp "$ROOT_DIR/Cargo.lock" "$lock"
rc=0
out="$(CONFIG_FILE="$cfg" LOCK_FILE="$lock" "$GUARD" 2>&1)" || rc=$?
ok=0
if [[ $rc -ne 0 ]] && grep -q 'expected exactly one line' <<< "$out"; then ok=1; fi
record "a missing pin line fails" "$ok" "$rc" "$out"

echo "tauri-cli-pin-guard: $pass passed, $fail failed"
[[ $fail -eq 0 ]]
