#!/usr/bin/env bash
set -euo pipefail

# Color output functions
color_red() { printf "\033[31m%s\033[0m\n" "$1"; }
color_green() { printf "\033[32m%s\033[0m\n" "$1"; }
color_yellow() { printf "\033[33m%s\033[0m\n" "$1"; }
color_blue() { printf "\033[34m%s\033[0m\n" "$1"; }
color_cyan() { printf "\033[36m%s\033[0m\n" "$1"; }
color_gray() { printf "\033[90m%s\033[0m\n" "$1"; }

# Logging functions with emoji
log_info() { echo "$(color_blue "ℹ️  $1")"; }
log_success() { echo "$(color_green "✅ $1")"; }
# log_warning and log_error go to STDERR, deliberately (#474). A function whose
# stdout is captured (`x="$(fn)" || exit 1`) would otherwise swallow its own
# refusal into the variable and exit with no message at all. Info, success and
# debug stay on stdout: they are progress, and losing one costs nothing.
log_warning() { echo "$(color_yellow "⚠️  $1")" >&2; }
log_error() { echo "$(color_red "❌ $1")" >&2; }
log_debug() { 
    if [[ "${DEBUG:-0}" == "1" ]]; then
        echo "$(color_gray "🔍 $1")"
    fi
}

# Check if command exists
command_exists() {
    command -v "$1" >/dev/null 2>&1
}

# `security find-identity` gives codesign's SHA-1 identity. Resolve the
# certificate by that exact hash before evaluating code-signing trust: renewals
# may reuse a common name, so a name lookup can validate one certificate and
# sign another.
apple_certificate_is_usable() {
    local candidate_id="$1"
    local certificate verification_error attempt

    if ! certificate="$(
        security find-certificate -a -Z -p 2>/dev/null |
            awk -v candidate_id="$candidate_id" '
                /^SHA-1 hash:/ {
                    wanted = ($3 == candidate_id)
                    in_certificate = 0
                }
                wanted && /^-----BEGIN CERTIFICATE-----$/ {
                    in_certificate = 1
                }
                in_certificate {
                    print
                }
                in_certificate && /^-----END CERTIFICATE-----$/ {
                    in_certificate = 0
                    wanted = 0
                }
            '
    )"; then
        printf 'Could not read signing certificates from the Keychain.\n' >&2
        return 1
    fi
    if [[ -z "$certificate" ]]; then
        printf 'Certificate %s was not found in the Keychain.\n' "$candidate_id" >&2
        return 1
    fi

    # A transient OCSP/trust-service failure is not proof of revocation. Retry
    # once, still requiring a positive response for the exact same certificate.
    # Keep the final diagnostic so an outage is distinguishable from revocation.
    for attempt in 1 2; do
        if verification_error="$(printf '%s\n' "$certificate" |
            security verify-cert -c /dev/stdin -p codeSign -R ocsp -R require 2>&1)"; then
            return 0
        fi
        case "$verification_error" in
            *CSSMERR_TP_CERT_REVOKED*|*CSSMERR_TP_CERT_EXPIRED*) break ;;
        esac
        if [[ "$attempt" -eq 1 ]]; then
            sleep 1
        fi
    done
    printf '%s\n' "${verification_error:-Certificate verification failed without a diagnostic.}" >&2
    return 1
}

# The self-signed identity `./dev run` signs with when no Apple Development
# certificate is installed. What Keychain ACLs bind to is the designated
# requirement, and for this identity that is `identifier "solador-app" and
# certificate leaf = H"<sha1>"` — the same on every rebuild, so "Always Allow"
# survives a relink the way it does under an Apple certificate. Ad-hoc signing
# binds to the cdhash instead, which every relink changes.
#
# It is never trusted, and does not need to be: codesign signs with an
# untrusted identity, and the ACL match is a requirement check, not a trust
# evaluation. Leaving it untrusted means nothing on this machine will accept
# it as anything but a stable name for a local build.
LOCAL_SIGNING_IDENTITY="Solador Local Development"

# Prints the SHA-1 of the one local signing identity, or nothing when there
# is none. Two is refused rather than resolved by picking the first: whichever
# one was not picked is what the existing ACLs may be bound to.
#
# Without -v: an untrusted identity appears only in the "Matching" section.
# A trusted one appears in both, hence the sort -u.
local_signing_identity() {
    local ids count
    ids="$(security find-identity -p codesigning 2>/dev/null |
        awk -v name="\"$LOCAL_SIGNING_IDENTITY\"" 'index($0, name) { print $2 }' |
        sort -u)"
    count="$(printf '%s' "$ids" | grep -c . || true)"
    if [[ "$count" -gt 1 ]]; then
        printf 'Found %s "%s" identities in the Keychain; delete all but one in Keychain Access.\n' \
            "$count" "$LOCAL_SIGNING_IDENTITY" >&2
        return 1
    fi
    [[ -z "$ids" ]] || printf '%s\n' "$ids"
}

# Creates the local signing identity in the default (login) Keychain.
#
# /usr/bin/openssl, not whatever is first on PATH: macOS's LibreSSL writes a
# PKCS#12 `security import` reads, and Homebrew's OpenSSL 3 defaults to a
# cipher it cannot. The key exists on disk only inside a private temp dir for
# the length of the import, removed by the subshell's EXIT trap even when the
# run is interrupted.
#
# -T lets codesign use the key without a prompt. That also means any process
# running as this user can sign with it, and so satisfy the dev build's
# designated requirement and read what was granted "Always Allow". That is the
# same exposure an Xcode-managed Apple Development key already has, and it
# needs code execution as this user to exploit.
#
# Tool output is captured rather than discarded, and printed on failure, so a
# creation that keeps failing says why instead of degrading to ad-hoc quietly.
create_local_signing_identity() {
    local dir passphrase output status=0
    dir="$(mktemp -d)" || return 1
    passphrase="$(/usr/bin/openssl rand -hex 16)" || { rm -rf "$dir"; return 1; }
    output="$(
        exec 2>&1
        trap 'rm -rf "$dir"' EXIT
        trap 'exit 130' INT TERM
        umask 077
        /usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
            -subj "/CN=$LOCAL_SIGNING_IDENTITY" \
            -addext "basicConstraints=critical,CA:false" \
            -addext "keyUsage=critical,digitalSignature" \
            -addext "extendedKeyUsage=critical,codeSigning" \
            -keyout "$dir/key.pem" -out "$dir/cert.pem" &&
            /usr/bin/openssl pkcs12 -export -name "$LOCAL_SIGNING_IDENTITY" \
                -inkey "$dir/key.pem" -in "$dir/cert.pem" \
                -passout "pass:$passphrase" -out "$dir/identity.p12" &&
            security import "$dir/identity.p12" -P "$passphrase" -T /usr/bin/codesign
    )" || status=$?
    rm -rf "$dir"
    if [[ "$status" -ne 0 ]]; then
        printf 'Could not create the "%s" signing identity:\n%s\n' \
            "$LOCAL_SIGNING_IDENTITY" "$output" >&2
    fi
    return "$status"
}

# Ensure we're in the project root
ensure_project_root() {
    if [[ ! -f "Cargo.toml" || ! -d "app/src-tauri" ]]; then
        log_error "Not in Solador project root directory"
        exit 1
    fi
}

# Check for clean git working tree
ensure_clean_working_tree() {
    if ! git diff --quiet || ! git diff --staged --quiet; then
        log_error "Working tree is not clean. Please commit or stash changes."
        exit 1
    fi
}

# Get current git branch
get_current_branch() {
    git rev-parse --abbrev-ref HEAD
}

# Check if on main branch
ensure_main_branch() {
    local current_branch
    current_branch=$(get_current_branch)
    if [[ "$current_branch" != "main" ]]; then
        log_error "Not on main branch (current: $current_branch)"
        exit 1
    fi
}

# Versioning lives in dedicated single-source scripts (docs/VERSIONING.md, org
# Versioning spec §3) — NOT here:
#   scripts/get-version-info.sh   marketing CalVer + the §4 release mint (--tag)
#   scripts/get-build-number.sh   build number (total commit count, --at <ref>)
# The old semver helpers (parse_version / increment_version) and the inline
# build-number counter were removed with the CalVer adoption (issue #98).

# Export functions for use in other scripts
export -f color_red color_green color_yellow color_blue color_cyan color_gray
export -f log_info log_success log_warning log_error log_debug
export -f apple_certificate_is_usable
export -f local_signing_identity create_local_signing_identity
export -f get_current_branch ensure_main_branch
