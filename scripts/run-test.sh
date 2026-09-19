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
    find-identity)
        case "${RUN_SCENARIO:-}" in
            untrusted) echo '  1) AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA "Apple Development: Fixture"' ;;
            no-identity) : ;;
            *) echo '  1) BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB "Apple Development: Fixture"' ;;
        esac
        ;;
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

# Run the real launcher in an isolated fake project. A transient trust-service
# failure must not launch an ad-hoc replacement against somebody's Keychain.
mkdir -p "$work/project/scripts" "$work/project/target/debug" "$work/project/app/src-tauri/icons"
cp "$SCRIPT_DIR/run.sh" "$SCRIPT_DIR/config.sh" "$SCRIPT_DIR/lib.sh" "$work/project/scripts/"
touch "$work/project/app/src-tauri/icons/icon.icns"
cat > "$work/project/target/debug/solador-app" <<'SHIM'
#!/usr/bin/env bash
touch "$LAUNCH_RECORD"
SHIM
cat > "$work/bin/cargo" <<'SHIM'
#!/usr/bin/env bash
exit 0
SHIM
cat > "$work/bin/uname" <<'SHIM'
#!/usr/bin/env bash
echo Darwin
SHIM
cat > "$work/bin/plutil" <<'SHIM'
#!/usr/bin/env bash
case "$2" in
    productName) echo Solador ;;
    identifier) echo app.solador.desktop ;;
    *) echo 14.0 ;;
esac
SHIM
cat > "$work/bin/codesign" <<'SHIM'
#!/usr/bin/env bash
[[ "${RUN_SCENARIO:-}" != sign-failed ]]
SHIM
chmod +x "$work/bin/"* "$work/project/target/debug/solador-app"
for scenario in untrusted sign-failed trusted no-identity; do
    launch_record="$work/launched-$scenario"
    run_status=0
    RUN_SCENARIO="$scenario" LAUNCH_RECORD="$launch_record" DEVELOPMENT_TEAM="" PATH="$work/bin:$PATH" \
        bash "$work/project/scripts/run.sh" > "$work/run-$scenario.log" 2>&1 || run_status=$?
    if [[ "$scenario" == untrusted || "$scenario" == sign-failed ]]; then
        if [[ "$run_status" -ne 0 && ! -f "$launch_record" ]]; then
            printf 'PASS: %s stops before launching\n' "$scenario"
            pass=$((pass + 1))
        else
            printf 'FAIL: %s launched without a trusted signature\n' "$scenario" >&2
            fail=$((fail + 1))
        fi
    elif [[ "$run_status" -eq 0 && -f "$launch_record" ]]; then
        printf 'PASS: %s can launch\n' "$scenario"
        pass=$((pass + 1))
    else
        printf 'FAIL: %s could not launch\n' "$scenario" >&2
        fail=$((fail + 1))
    fi
done

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
