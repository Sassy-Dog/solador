#!/usr/bin/env bash
# Pins #551: `notarize_and_staple` must classify a failed `notarytool submit
# --wait` by Apple's submission STATUS, never by the exit code. A wait that
# ran out is "still in progress", not "rejected"; a rejection prints Apple's
# log; a submit that never produced an id says the submission did not happen;
# only Accepted goes on to staple. Every non-Accepted path exits non-zero.
#
# build.sh is a program, not a library, so the function is lifted out of it
# with awk (as signing-identity-test.sh does). `xcrun` and `spctl` are shell
# functions simulating notarytool's real output for each outcome; nothing is
# submitted to Apple and no credential is read. Compatible with macOS
# /bin/bash 3.2.
#
# Negative control: NOTARIZE_SOURCE=<file> lifts the function from another
# copy of build.sh. Run against the pre-#551 exit-code-only function, this
# suite must FAIL.
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"

SOURCE="${NOTARIZE_SOURCE:-$SCRIPT_DIR/build.sh}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

awk '/^notarize_and_staple\(\) \{/ { on = 1 } on { print } on && /^\}/ { exit }' "$SOURCE" > "$work/under-test.sh"
grep -q '^notarize_and_staple() {' "$work/under-test.sh" || { echo "could not lift notarize_and_staple out of $SOURCE" >&2; exit 1; }
# shellcheck source=/dev/null
source "$work/under-test.sh"

failures=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; failures=$((failures + 1)); }

ID="b2c0c410-a6d8-45e8-b62c-e1171597b599"
CALLS="$work/calls"
SCENARIO=""

# The stub toolchain. SCENARIO picks what `notarytool` does.
xcrun() {
    echo "xcrun $*" >> "$CALLS"
    case "$1 ${2:-}" in
        "notarytool submit")
            case "$SCENARIO" in
                zeroinvalid)
                    printf 'Submission ID received\n  id: %s\nProcessing complete\n  id: %s\n  status: Invalid\n' "$ID" "$ID"
                    return 0 ;;
                accepted)
                    printf 'Submission ID received\n  id: %s\nUpload successful\nWaiting for processing to complete.\nCurrent status: Accepted...\nProcessing complete\n  id: %s\n  status: Accepted\n' "$ID" "$ID"
                    return 0 ;;
                noid)
                    printf 'Error: Could not resolve host: appstoreconnect.apple.com\n'
                    return 1 ;;
                *)
                    printf 'Submission ID received\n  id: %s\nUpload successful\nWaiting for processing to complete.\nWait timeout is set to 3600.0 second(s).\nCurrent status: In Progress...........\nTimeout of 3600 second(s) was reached before processing completed.\n  id: %s\n' "$ID" "$ID"
                    return 1 ;;
            esac ;;
        "notarytool info")
            case "$SCENARIO" in
                timeout) printf 'Successfully received submission info\n  createdDate: 2026-10-06T22:34:53.000Z\n  id: %s\n  name: Solador.dmg\n  status: In Progress\n' "$ID" ;;
                zeroinvalid|invalid) printf 'Successfully received submission info\n  id: %s\n  status: Invalid\n' "$ID" ;;
                rejected) printf 'Successfully received submission info\n  id: %s\n  status: Rejected\n' "$ID" ;;
                logfail) printf 'Successfully received submission info\n  id: %s\n  status: Invalid\n' "$ID" ;;
                unknown) printf 'Successfully received submission info\n  id: %s\n  status: In Review\n' "$ID" ;;
                race)    printf 'Successfully received submission info\n  id: %s\n  status: Accepted\n' "$ID" ;;
                infofail) printf 'Error: network down\n'; return 1 ;;
            esac ;;
        "notarytool log")
            [[ "$SCENARIO" == logfail ]] && return 1
            printf '{"issues":[{"message":"The signature of the binary is invalid."}]}\n' ;;
        "stapler staple"|"stapler validate") return 0 ;;
    esac
}
spctl() { echo "spctl $*" >> "$CALLS"; }

export APPLE_ASC_KEY_ID=KEYID APPLE_ASC_ISSUER_ID=ISSUER
APPLE_ASC_KEY_BASE64="$(printf 'fake-key' | base64)"
export APPLE_ASC_KEY_BASE64

# run_case <scenario>: the function exits, so it runs in a subshell.
rc=0
run_case() {
    SCENARIO="$1"; : > "$CALLS"
    rc=0
    ( notarize_and_staple "$work/Solador.dmg" ) > "$work/out" 2>&1 || rc=$?
}
called() { grep -qF -- "$1" "$CALLS"; }
said() { grep -qF -- "$1" "$work/out"; }
# check <label> <command...>: pass when the command succeeds.
check() {
    local label="$1"; shift
    if "$@"; then pass "$label"; else fail "$label"; sed 's/^/      | /' "$work/out"; fi
}
exited_zero() { [[ "$rc" -eq 0 ]]; }
exited_nonzero() { [[ "$rc" -ne 0 ]]; }
never_said() { ! said "$1"; }
never_called() { ! called "$1"; }
staple_chain() { called "stapler staple" && called "stapler validate" && called "spctl -a -vvv --type install"; }
log_for_id() { called "notarytool log $ID" && said "signature of the binary is invalid"; }

# --- Accepted: staples, validates, assesses; exits 0; waits 60m.
run_case accepted
check "accepted: exits 0" exited_zero
check "accepted: waits 60 minutes" called "--wait --timeout 60m"
check "accepted: staples, validates and assesses" staple_chain

# --- Timed out: still in progress, never "rejected", resume path, no log.
run_case timeout
check "timeout: exits non-zero" exited_nonzero
check "timeout: says still in progress" said "still in progress at Apple"
check "timeout: never says rejected" never_said "was rejected"
check "timeout: names the submission id" said "$ID"
check "timeout: gives the notarytool wait resume path" said "notarytool wait $ID"
check "timeout: then staple" said "xcrun stapler staple"
check "timeout: never suggests notarytool log" never_said "notarytool log"
check "timeout: never fetches the log" never_called "notarytool log"
check "timeout: does not staple" never_called "stapler staple"

# --- Rejected: says so, prints Apple's log, non-zero.
run_case invalid
check "invalid: exits non-zero" exited_nonzero
check "invalid: says rejected" said "was rejected by Apple"
check "invalid: prints Apple's log" log_for_id
check "invalid: does not staple" never_called "stapler staple"

# --- Rejected status, a failed log fetch, an exit-0 Invalid, an unknown status.
run_case rejected
check "rejected: exits non-zero" exited_nonzero
check "rejected: says rejected" said "was rejected by Apple"
run_case logfail
check "logfail: exits non-zero" exited_nonzero
check "logfail: names the log command" said "xcrun notarytool log $ID"
run_case zeroinvalid
check "zeroinvalid: exits non-zero despite exit 0" exited_nonzero
check "zeroinvalid: says rejected" said "was rejected by Apple"
check "zeroinvalid: does not staple" never_called "stapler staple"
run_case unknown
check "unknown: exits non-zero" exited_nonzero
check "unknown: says unrecognised or unreadable" said "unrecognised or unreadable"
check "unknown: never says rejected" never_said "was rejected"

# --- No id: the submission did not happen, notarytool's error shown.
run_case noid
check "noid: exits non-zero" exited_nonzero
check "noid: says the submission did not happen" said "submission did not happen"
check "noid: shows notarytool's own error" said "Could not resolve host"
check "noid: never says rejected" never_said "was rejected"
check "noid: asks Apple nothing further" never_called "notarytool info"

# --- Race: non-zero exit but Apple says Accepted -> carry on to staple.
run_case race
check "race: exits 0" exited_zero
check "race: staples" called "stapler staple"

# --- Status unreadable: fail closed, without calling it a rejection.
run_case infofail
check "infofail: exits non-zero" exited_nonzero
check "infofail: says the status is unrecognised or unreadable" said "unrecognised or unreadable"
check "infofail: never says rejected" never_said "was rejected"
check "infofail: does not staple" never_called "stapler staple"

if (( failures )); then
    echo "notarize outcome: $failures failure(s)" >&2
    exit 1
fi
echo "notarize outcome: all passed"
