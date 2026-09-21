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
            revoked-first)
                echo '  1) AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA "Apple Development: Fixture" (CSSMERR_TP_CERT_REVOKED)'
                echo '  2) BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB "Apple Development: Fixture"'
                ;;
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
        previous_attempts=0
        if [[ -n "${VERIFY_RECORD:-}" ]]; then
            if [[ -f "$VERIFY_RECORD" ]]; then
                previous_attempts="$(wc -l < "$VERIFY_RECORD")"
            fi
            printf 'verify\n' >> "$VERIFY_RECORD"
        fi
        case "$certificate" in
            *VALID-CERTIFICATE*)
                case "${RUN_SCENARIO:-}" in
                    verification-unavailable)
                        echo 'OCSP responder unavailable (fixture)' >&2
                        exit 1
                        ;;
                    verification-recovered)
                        if [[ "$previous_attempts" -eq 0 ]]; then
                            echo 'OCSP responder unavailable (fixture)' >&2
                            exit 1
                        fi
                        ;;
                    expired)
                        echo 'Cert Verify Result: CSSMERR_TP_CERT_EXPIRED'
                        exit 1
                        ;;
                esac
                exit 0
                ;;
            *REVOKED-CERTIFICATE*)
                echo 'Cert Verify Result: CSSMERR_TP_CERT_REVOKED'
                exit 1
                ;;
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
        apple_certificate_is_usable "$identity" > "$work/verification.log" 2>&1; then
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
        apple_certificate_is_usable "$identity" > "$work/verification.log" 2>&1; then
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
# Recovery must still verify and sign the exact identity before launching.
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
printf '%s\n' "$*" >> "$SIGN_RECORD"
[[ "${RUN_SCENARIO:-}" != sign-failed ]]
SHIM
cat > "$work/bin/sleep" <<'SHIM'
#!/usr/bin/env bash
[[ "$*" == 1 ]]
SHIM
chmod +x "$work/bin/"* "$work/project/target/debug/solador-app"
for scenario in untrusted sign-failed trusted no-identity verification-unavailable verification-recovered revoked-first expired; do
    launch_record="$work/launched-$scenario"
    verify_record="$work/verified-$scenario"
    sign_record="$work/signed-$scenario"
    touch "$verify_record" "$sign_record"
    run_status=0
    RUN_SCENARIO="$scenario" LAUNCH_RECORD="$launch_record" VERIFY_RECORD="$verify_record" \
        SIGN_RECORD="$sign_record" DEVELOPMENT_TEAM="" PATH="$work/bin:$PATH" \
        bash "$work/project/scripts/run.sh" > "$work/run-$scenario.log" 2>&1 || run_status=$?
    if [[ "$scenario" == untrusted || "$scenario" == sign-failed ||
          "$scenario" == verification-unavailable || "$scenario" == expired ]]; then
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

    expected_attempts=1
    diagnostic=""
    case "$scenario" in
        no-identity) expected_attempts=0 ;;
        verification-recovered|revoked-first) expected_attempts=2 ;;
        verification-unavailable)
            expected_attempts=2
            diagnostic='OCSP responder unavailable (fixture)'
            ;;
        untrusted) diagnostic=CSSMERR_TP_CERT_REVOKED ;;
        expired) diagnostic=CSSMERR_TP_CERT_EXPIRED ;;
    esac
    if [[ "$(wc -l < "$verify_record")" -eq "$expected_attempts" ]] &&
        { [[ -z "$diagnostic" ]] || grep -Fq "$diagnostic" "$work/run-$scenario.log"; }; then
        printf 'PASS: %s bounds verification attempts and preserves errors\n' "$scenario"
        pass=$((pass + 1))
    else
        printf 'FAIL: %s verification attempts or diagnostic\n' "$scenario" >&2
        fail=$((fail + 1))
    fi

    case "$scenario" in
        trusted|verification-recovered|revoked-first|sign-failed)
            if grep -Fq -- "--force --identifier solador-app --sign $VALID_ID " "$sign_record"; then
                printf 'PASS: %s signs the verified identity with the stable identifier\n' "$scenario"
                pass=$((pass + 1))
            else
                printf 'FAIL: %s did not sign the verified identity\n' "$scenario" >&2
                fail=$((fail + 1))
            fi
            ;;
        *)
            if [[ ! -s "$sign_record" ]]; then
                printf 'PASS: %s does not sign without a verified identity\n' "$scenario"
                pass=$((pass + 1))
            else
                printf 'FAIL: %s signed without a verified identity\n' "$scenario" >&2
                fail=$((fail + 1))
            fi
            ;;
    esac
done

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
