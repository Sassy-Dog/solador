#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"

cat > "$work/bin/security" <<'SHIM'
#!/usr/bin/env bash
set -euo pipefail

emit_revoked() {
    cat <<'CERTIFICATE'
SHA-256 hash: REVOKED256
SHA-1 hash: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
-----BEGIN CERTIFICATE-----
SAME-NAME: Apple Development: Fixture
REVOKED-CERTIFICATE
-----END CERTIFICATE-----
CERTIFICATE
}

emit_valid() {
    cat <<'CERTIFICATE'
SHA-256 hash: VALID256
SHA-1 hash: BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB
-----BEGIN CERTIFICATE-----
SAME-NAME: Apple Development: Fixture
VALID-CERTIFICATE
-----END CERTIFICATE-----
CERTIFICATE
}

case "${1:-}" in
    find-certificate)
        [[ "$*" == "find-certificate -a -Z -p" ]] || exit 2
        if [[ "${CERT_ORDER:-revoked-first}" == "valid-first" ]]; then
            emit_valid
            emit_revoked
        else
            emit_revoked
            emit_valid
        fi
        ;;
    verify-cert)
        [[ "$*" == "verify-cert -c /dev/stdin -p codeSign -R ocsp -R require" ]] ||
            exit 2
        certificate="$(cat)"
        case "$certificate" in
            *VALID-CERTIFICATE*) exit 0 ;;
            *REVOKED-CERTIFICATE*) exit 1 ;;
            *) exit 2 ;;
        esac
        ;;
    *)
        exit 2
        ;;
esac
SHIM
chmod +x "$work/bin/security"

pass=0
fail=0

expect_success() {
    local name="$1" order="$2" identity="$3"
    if CERT_ORDER="$order" PATH="$work/bin:$PATH" \
        apple_certificate_is_usable "$identity"; then
        printf 'PASS: %s\n' "$name"
        pass=$((pass + 1))
    else
        printf 'FAIL: %s (expected success)\n' "$name" >&2
        fail=$((fail + 1))
    fi
}

expect_failure() {
    local name="$1" order="$2" identity="$3"
    if CERT_ORDER="$order" PATH="$work/bin:$PATH" \
        apple_certificate_is_usable "$identity"; then
        printf 'FAIL: %s (expected failure)\n' "$name" >&2
        fail=$((fail + 1))
    else
        printf 'PASS: %s\n' "$name"
        pass=$((pass + 1))
    fi
}

REVOKED_ID="AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
VALID_ID="BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"
UNKNOWN_ID="CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC"

expect_failure "revoked exact identity is rejected before same-name renewal" \
    revoked-first "$REVOKED_ID"
expect_success "same-name renewal is accepted after revoked identity" \
    revoked-first "$VALID_ID"
expect_success "valid exact identity is accepted before same-name revoked cert" \
    valid-first "$VALID_ID"
expect_failure "revoked exact identity is rejected after same-name renewal" \
    valid-first "$REVOKED_ID"
expect_failure "identity absent from certificate enumeration is rejected" \
    revoked-first "$UNKNOWN_ID"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
