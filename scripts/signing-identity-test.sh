#!/usr/bin/env bash
# Pins #474: resolve_signing_identity's refusals must reach the terminal even
# though build.sh calls it as `identity="$(resolve_signing_identity)" || exit 1`.
# That substitution captures stdout, so a refusal written to stdout became the
# value of `identity` and was thrown away, leaving a bare `exit 1` after the
# release tag had already been pushed.
#
# build.sh is a program, not a library (it parses arguments and builds on
# source), so the function and its prefix constant are lifted out of it by
# line range rather than sourcing the file. `security` is a stub; no keychain
# is read. Compatible with macOS /bin/bash 3.2.
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"

cat > "$work/bin/security" <<'SHIM'
#!/usr/bin/env bash
[[ "${1:-}" == "find-identity" ]] || exit 2
case "${SIGNING_SCENARIO:-}" in
    none) echo '     0 valid identities found' ;;
    one) echo '  1) AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA "Developer ID Application: Team One (T1)"' ;;
    two)
        echo '  1) AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA "Developer ID Application: Team One (T1)"'
        echo '  2) BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB "Developer ID Application: Team Two (T2)"'
        ;;
esac
SHIM
chmod +x "$work/bin/security"

# The prefix constant and the function, verbatim from build.sh.
{
    grep '^SIGNING_IDENTITY_PREFIX=' "$SCRIPT_DIR/build.sh"
    awk '/^resolve_signing_identity\(\) \{/ { on = 1 } on { print } on && /^\}/ { exit }' "$SCRIPT_DIR/build.sh"
} > "$work/under-test.sh"
grep -q '^resolve_signing_identity() {' "$work/under-test.sh" || {
    echo "could not lift resolve_signing_identity out of build.sh" >&2
    exit 1
}
# shellcheck source=/dev/null
source "$work/under-test.sh"

failures=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; failures=$((failures + 1)); }

# run_case <scenario>: sets RC, OUT (stdout) and ERR (stderr), the way
# build.sh's `$(...)` would see them: stdout captured, stderr passed through.
run_case() {
    RC=0
    OUT="$(PATH="$work/bin:$PATH" SIGNING_SCENARIO="$1" APPLE_SIGNING_IDENTITY="" \
        resolve_signing_identity 2>"$work/stderr")" || RC=$?
    ERR="$(cat "$work/stderr")"
}

expect() { # <label> <haystack> <needle>
    case "$2" in *"$3"*) pass "$1" ;; *) fail "$1 (missing: $3)" ;; esac
}

run_case none
if [[ "$RC" -eq 1 ]]; then pass "no identity: returns 1"; else fail "no identity: returns 1 (got $RC)"; fi
expect "no identity: reason on stderr" "$ERR" "No 'Developer ID Application' certificate in the keychain."
expect "no identity: remedy on stderr" "$ERR" "set APPLE_SIGNING_IDENTITY"
if [[ -z "$OUT" ]]; then pass "no identity: nothing captured on stdout"; else fail "no identity: stdout was swallowed: $OUT"; fi

run_case two
if [[ "$RC" -eq 1 ]]; then pass "ambiguous: returns 1"; else fail "ambiguous: returns 1 (got $RC)"; fi
expect "ambiguous: reason on stderr" "$ERR" "2 'Developer ID Application' certificates match"
expect "ambiguous: identity list on stderr" "$ERR" "Team Two (T2)"
expect "ambiguous: remedy on stderr" "$ERR" "Set APPLE_SIGNING_IDENTITY to the one you mean."
if [[ -z "$OUT" ]]; then pass "ambiguous: nothing captured on stdout"; else fail "ambiguous: stdout was swallowed: $OUT"; fi

run_case one
if [[ "$RC" -eq 0 && "$OUT" == "Developer ID Application: Team One (T1)" && -z "$ERR" ]]; then
    pass "one identity: resolves it, silently"
else
    fail "one identity: rc=$RC out=$OUT err=$ERR"
fi

RC=0
OUT="$(PATH="$work/bin:$PATH" SIGNING_SCENARIO=two APPLE_SIGNING_IDENTITY="Override" \
    resolve_signing_identity 2>"$work/stderr")" || RC=$?
ERR="$(cat "$work/stderr")"
if [[ "$RC" -eq 0 && "$OUT" == "Override" && -z "$ERR" ]]; then pass "APPLE_SIGNING_IDENTITY wins"; else fail "APPLE_SIGNING_IDENTITY wins"; fi

if [[ "$failures" -ne 0 ]]; then
    echo "$failures failure(s)" >&2
    exit 1
fi
echo "signing identity resolution: all passed"
