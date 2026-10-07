#!/usr/bin/env bash
set -euo pipefail

# Proves scripts/shellcheck-pin-guard.sh refuses what it claims to refuse
# (#548). Runs THE SAME guard against stub `shellcheck` executables made in a
# temp dir, never a real binary. Run by ci.yml's `secrets-guard` job and by
# `./dev lint`. A guard that has only ever seen the pinned version is
# indistinguishable from one that passes everything.
#
# GUARD overrides the guard under test. The test runs itself once against an
# always-passing guard and requires that run to FAIL, so its refusal cases are
# proven able to go red on every run, not just documented.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
GUARD="${GUARD:-$SCRIPT_DIR/shellcheck-pin-guard.sh}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

if [[ -z "${SHELLCHECK_PIN_GUARD_SELF_CHECK:-}" ]]; then
    printf '#!/bin/sh\nexit 0\n' > "$work/always-passes"
    chmod +x "$work/always-passes"
    if GUARD="$work/always-passes" SHELLCHECK_PIN_GUARD_SELF_CHECK=1 "${BASH_SOURCE[0]}" > /dev/null 2>&1; then
        echo "FAIL this test passed against a guard that accepts everything, so it proves nothing"
        exit 1
    fi
    echo "ok   the test fails against an always-passing guard"
fi

# The guard's own pin-reading expression, so the two cannot disagree.
pin="$(sed -n 's/^export SHELLCHECK_VERSION="\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)"[[:space:]]*$/\1/p' "$SCRIPT_DIR/config.sh")"
[[ -n "$pin" && "$pin" != *$'\n'* ]] || { echo "no pin in config.sh" >&2; exit 1; }

pass=0
fail=0

record() {
    if [[ "$2" -eq 1 ]]; then
        pass=$((pass + 1))
        echo "ok   $1"
    else
        fail=$((fail + 1))
        echo "FAIL $1 (exit $3)"
        echo "$4" | sed 's/^/     /'
    fi
}

# stub NAME BODY: an executable whose whole behaviour is BODY.
stub() {
    printf '#!/bin/sh\n%s\n' "$2" > "$work/$1"
    chmod +x "$work/$1"
}

# report VERSION: a --version body as the real one prints it.
report() {
    printf "echo 'ShellCheck - shell script analysis tool'; echo 'version: %s'; echo 'license: GNU General Public License, version 3'" "$1"
}

# run_case NAME EXPECT(pass|fail) SHELLCHECK_PATH [OUTPUT_REGEX...]
run_case() {
    local name="$1" expect="$2" bin="$3" out rc=0 ok=1 re
    shift 3
    out="$(SHELLCHECK="$bin" "$GUARD" 2>&1)" || rc=$?
    if [[ "$expect" == pass && $rc -ne 0 ]]; then ok=0; fi
    if [[ "$expect" == fail && $rc -eq 0 ]]; then ok=0; fi
    for re in "$@"; do
        grep -qE -- "$re" <<< "$out" || ok=0
    done
    record "$name" "$ok" "$rc" "$out"
}

stub pinned "$(report "$pin")"
run_case "the pinned version passes" pass "$work/pinned" "$pin"

stub older "$(report 0.9.0)"
run_case "an older version fails naming both versions and the install route" fail "$work/older" \
    '0\.9\.0' "$pin" 'brew install shellcheck'

stub newer "$(report 99.0.0)"
run_case "a newer version fails" fail "$work/newer" '99\.0\.0' "$pin"

stub patch "$(report "${pin%.*}.99")"
run_case "a different patch fails (the match is exact)" fail "$work/patch" "${pin%.*}\\.99" 'pins'

stub garbage "echo 'hello world'"
run_case "output with no version line fails" fail "$work/garbage" 'did not report a version'

stub silent "exit 0"
run_case "no output fails" fail "$work/silent" 'did not report a version'

stub broken "echo 'boom' >&2; exit 3"
run_case "a binary that errors fails" fail "$work/broken" 'could not run'

run_case "a missing binary fails" fail "$work/does-not-exist" 'could not run'

# A config with no pin line fails, even with a matching binary.
grep -v '^export SHELLCHECK_VERSION=' "$SCRIPT_DIR/config.sh" > "$work/config-nopin.sh"
rc=0
out="$(CONFIG_FILE="$work/config-nopin.sh" SHELLCHECK="$work/pinned" "$GUARD" 2>&1)" || rc=$?
ok=0
if [[ $rc -ne 0 ]] && grep -q 'expected exactly one line' <<< "$out"; then ok=1; fi
record "a missing pin line fails" "$ok" "$rc" "$out"

echo "shellcheck-pin-guard: $pass passed, $fail failed"
[[ $fail -eq 0 ]]
