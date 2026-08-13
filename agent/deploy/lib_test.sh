#!/usr/bin/env bash
#
# Tests for agent/deploy/lib.sh — the helpers install.sh and redeploy.sh share.
#
#   bash agent/deploy/lib_test.sh
#
# Why this exists (#269): agent/deploy/ is the agent's only path onto a host,
# and until now it had no coverage at all — CI never executed these scripts, not
# even a syntax check. #264 folded agent/ into the root workspace, moving
# cargo's output to <workspace>/target/; both deploy scripts kept looking in
# agent/target/release/ and *every* deploy died. `./dev lint`, `./dev test` and
# all three required checks stayed green the whole time (#268).
#
# Scope: the pure helpers, plus the one behavior whose *failure* is the point
# (build_release_binary refusing to fall back), plus three source-level
# invariants that no runtime test can reach. Nothing here talks to a host —
# cargo, curl and sleep are stubbed. End-to-end install against a real host
# stays deliberately out of scope; verify_health covers that at runtime.
#
# Dependency-free on purpose: bash + the coreutils the deploy scripts already
# need. No bats, no jq. Runs on macOS (bash 3.2) and on the Linux CI runner.
#
# A plain `shellcheck` (no -S) reports SC2016 and SC2030/SC2031 here. Both are
# info-level, below the `-S warning` gate, and both are the intent rather than
# an oversight: the single-quoted strings are literal source-text needles that
# must *not* expand, and every PATH/env change is deliberately scoped to the
# subshell that is the isolation between one test and the next.

# Not `set -e`: a failed assertion is recorded and the run continues, so one
# broken helper cannot hide the state of every other one.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=agent/deploy/lib.sh
source "$SCRIPT_DIR/lib.sh"

# Hermetic: both of these are read by the code under test and must not leak in
# from whatever shell invoked the suite.
unset CARGO_TARGET_DIR
unset VERIFY_HEALTH_ATTEMPTS

# ---- harness ----------------------------------------------------------------

PASSED=0
FAILED=0
SKIPPED=0

pass() {
    PASSED=$((PASSED + 1))
    printf 'ok    %s\n' "$1"
}

# fail <name> [detail...]
fail() {
    local name="$1"
    shift
    FAILED=$((FAILED + 1))
    printf 'FAIL  %s\n' "$name"
    if [ "$#" -gt 0 ]; then
        local detail
        for detail in "$@"; do
            printf '        %s\n' "$detail"
        done
    fi
}

# A skipped test asserted nothing. Say so loudly rather than letting it read as
# a pass — the summary repeats the count at the end.
skip() {
    SKIPPED=$((SKIPPED + 1))
    printf 'SKIP  %s (%s)\n' "$1" "$2"
}

assert_eq() {
    local name="$1" want="$2" got="$3"
    if [ "$want" = "$got" ]; then
        pass "$name"
    else
        fail "$name" "want: [$want]" "got:  [$got]"
    fi
}

assert_empty() {
    local name="$1" got="$2"
    if [ -z "$got" ]; then
        pass "$name"
    else
        fail "$name" "want: <empty>" "got:  [$got]"
    fi
}

# assert_output_has <name> <haystack> <needle>  — substring, no regex.
assert_output_has() {
    local name="$1" haystack="$2" needle="$3"
    case "$haystack" in
        *"$needle"*) pass "$name" ;;
        *) fail "$name" "expected the output to mention: [$needle]" ;;
    esac
}

assert_file_has() {
    local name="$1" file="$2" needle="$3"
    if grep -qF -- "$needle" "$file" 2>/dev/null; then
        pass "$name"
    else
        fail "$name" "expected $file to contain: [$needle]"
    fi
}

# Print the 1-based line number of the first line containing the literal
# needle; print nothing when it is absent.
line_of() {
    grep -nF -- "$2" "$1" 2>/dev/null | head -n1 | cut -d: -f1
}

# assert_before <name> <file> <needle-that-must-come-first> <needle-after>
assert_before() {
    local name="$1" file="$2" first="$3" second="$4" line_first line_second
    line_first="$(line_of "$file" "$first")"
    line_second="$(line_of "$file" "$second")"
    if [ -z "$line_first" ] || [ -z "$line_second" ]; then
        fail "$name" \
            "could not locate both markers in $file" \
            "[$first] -> ${line_first:-<not found>}" \
            "[$second] -> ${line_second:-<not found>}"
        return
    fi
    if [ "$line_first" -lt "$line_second" ]; then
        pass "$name"
    else
        fail "$name" \
            "[$first] is at line $line_first" \
            "[$second] is at line $line_second" \
            "the first must come first"
    fi
}

# Print the body of a shell function from a script, so an ordering assertion
# can be scoped to a single code path. redeploy.sh's rollback path stages and
# renames the same way deploy does, so a file-wide line comparison would happily
# compare a marker in one function against a marker in the other.
extract_function() {
    awk -v name="$2" '
        $0 == name "() {" { inside = 1 }
        inside { print }
        inside && /^\}/ { exit }
    ' "$1"
}

# Walk up from a directory looking for a Cargo.toml. Used as a precondition
# check, not an assertion: if the temp dir turns out to sit inside somebody's
# cargo workspace, the "no workspace here" test would assert nothing, and that
# is worth reporting rather than passing.
ancestor_has_manifest() {
    local dir="$1"
    while [ -n "$dir" ] && [ "$dir" != "/" ]; do
        if [ -f "$dir/Cargo.toml" ]; then
            return 0
        fi
        dir="$(dirname "$dir")"
    done
    [ -f "/Cargo.toml" ]
}

# ---- fixtures ---------------------------------------------------------------

TMP="$(mktemp -d "${TMPDIR:-/tmp}/solador-deploy-test.XXXXXX")" || exit 1
# Physical path: cargo reports the canonical cwd, and on macOS $TMPDIR is a
# symlink (/var -> /private/var), so a literal comparison would fail on a
# difference that is not one.
TMP="$(cd "$TMP" && pwd -P)"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

STUBS="$TMP/stubs"
STDERR="$TMP/stderr"
mkdir -p "$STUBS"

# cargo: `build` reports whatever STUB_CARGO_BUILD_EXIT says and writes nothing
# (the tests place — or deliberately do not place — the binary themselves);
# `locate-project` answers with STUB_CARGO_WORKSPACE_MANIFEST, or fails when it
# is empty. Every invocation is appended to STUB_CARGO_ARGV when that is set.
cat > "$STUBS/cargo" <<'STUB'
#!/usr/bin/env bash
if [ -n "${STUB_CARGO_ARGV:-}" ]; then
    printf '%s\n' "$*" >> "$STUB_CARGO_ARGV"
fi
case "${1:-}" in
    build)
        exit "${STUB_CARGO_BUILD_EXIT:-0}"
        ;;
    locate-project)
        [ -n "${STUB_CARGO_WORKSPACE_MANIFEST:-}" ] || exit 1
        printf '%s\n' "$STUB_CARGO_WORKSPACE_MANIFEST"
        ;;
    *)
        exit 1
        ;;
esac
STUB

# curl: prints STUB_CURL_BODY, or exits non-zero like `curl -f` on an HTTP
# error when there is no body to serve.
cat > "$STUBS/curl" <<'STUB'
#!/usr/bin/env bash
if [ -n "${STUB_CURL_ARGV:-}" ]; then
    printf '%s\n' "$*" >> "$STUB_CURL_ARGV"
fi
[ -n "${STUB_CURL_BODY:-}" ] || exit 22
printf '%s' "$STUB_CURL_BODY"
STUB

# sleep: verify_health polls once a second. The failure paths are exercised
# with VERIFY_HEALTH_ATTEMPTS=1, and this keeps even that second off the clock.
cat > "$STUBS/sleep" <<'STUB'
#!/usr/bin/env bash
exit 0
STUB

chmod +x "$STUBS/cargo" "$STUBS/curl" "$STUBS/sleep"

# Something executable that is not the real agent. The deploy scripts only ever
# test these for -x and hand the path to `install`, so a shell stub is enough.
make_fake_binary() {
    printf '#!/bin/sh\nexit 0\n' > "$1"
    chmod +x "$1"
}

# ---- crate_version ----------------------------------------------------------

test_crate_version() {
    local manifest got

    manifest="$TMP/simple.toml"
    cat > "$manifest" <<'TOML'
[package]
name = "solador-agent"
version = "0.4.0"
edition = "2021"
TOML
    assert_eq "crate_version reads the [package] version" \
        "0.4.0" "$(crate_version "$manifest")"

    # The trap the awk exists for. A bare `grep '^version'` returns the *first*
    # version line in the file, so a manifest that grows a [dependencies.foo]
    # table above [package] would have the deploy assert serde's number against
    # /v1/health — and the mismatch would be blamed on the agent.
    manifest="$TMP/dep-above.toml"
    cat > "$manifest" <<'TOML'
[dependencies.serde]
version = "1.0.999"
features = ["derive"]

[package]
name = "solador-agent"
version = "0.4.0"
TOML
    assert_eq "crate_version ignores a [dependencies.*] version above [package]" \
        "0.4.0" "$(crate_version "$manifest")"

    # ...and the section has to *end* at the next table header, or the same
    # dependency below [package] wins instead.
    manifest="$TMP/dep-below.toml"
    cat > "$manifest" <<'TOML'
[package]
name = "solador-agent"
version = "0.4.0"

[dependencies.serde]
version = "1.0.999"
TOML
    assert_eq "crate_version stops at the next table header" \
        "0.4.0" "$(crate_version "$manifest")"

    # agent/Cargo.toml carries its changelog as a comment block between the
    # header and the value, so "the line after [package]" is not the version.
    manifest="$TMP/commented.toml"
    cat > "$manifest" <<'TOML'
[package]
name = "solador-agent"
# 0.4.0: gpu is measured on hosts with an NVIDIA card (#217).
# 0.3.1: processes[] lists processes only (#211).
version = "0.4.0"  # inline comments are legal TOML too
TOML
    assert_eq "crate_version skips comments between the header and the value" \
        "0.4.0" "$(crate_version "$manifest")"

    # The real manifest, so a restructure that breaks the parser fails here
    # instead of on a host mid-deploy. Asserting the shape, not the number: the
    # number moves every release, the parseability must not.
    got="$(crate_version "$SCRIPT_DIR/../Cargo.toml")"
    case "$got" in
        [0-9]*.[0-9]*.[0-9]*)
            pass "crate_version parses the real agent/Cargo.toml (got $got)"
            ;;
        *)
            fail "crate_version parses the real agent/Cargo.toml" \
                "want: a semver-shaped version" "got:  [$got]"
            ;;
    esac

    crate_version "$TMP/does-not-exist.toml" >/dev/null 2>&1
    assert_eq "crate_version fails on a missing manifest" "1" "$?"

    # Never fabricate: an unreadable version is empty, not a guess. Both deploy
    # scripts guard on that emptiness and refuse to build — which is the only
    # reason returning nothing here is safe.
    manifest="$TMP/no-version.toml"
    printf '[package]\nname = "solador-agent"\n' > "$manifest"
    assert_empty "crate_version prints nothing when [package] has no version" \
        "$(crate_version "$manifest")"
    assert_file_has "install.sh refuses to build without a version" \
        "$SCRIPT_DIR/install.sh" '[ -n "$TARGET_VERSION" ]'
    assert_file_has "redeploy.sh refuses to build without a version" \
        "$SCRIPT_DIR/redeploy.sh" '[ -n "$target_version" ]'
}

# ---- health_url -------------------------------------------------------------

test_health_url() {
    assert_eq "health_url dials a tailnet IPv4 as-is" \
        "http://100.87.202.125:7878/v1/health" "$(health_url "100.87.202.125" "7878")"

    # A wildcard is not an address you can dial, so probe loopback instead.
    assert_eq "health_url probes loopback for a 0.0.0.0 bind" \
        "http://127.0.0.1:7878/v1/health" "$(health_url "0.0.0.0" "7878")"
    assert_eq "health_url probes loopback for an empty bind" \
        "http://127.0.0.1:7878/v1/health" "$(health_url "" "7878")"
    assert_eq "health_url probes IPv6 loopback for a :: bind" \
        "http://[::1]:7878/v1/health" "$(health_url "::" "7878")"
    assert_eq "health_url probes IPv6 loopback for a [::] bind" \
        "http://[::1]:7878/v1/health" "$(health_url "[::]" "7878")"

    # An IPv6 literal is not a legal URL host until it is bracketed.
    assert_eq "health_url brackets an IPv6 literal" \
        "http://[fd7a:115c:a1e0::1]:7878/v1/health" "$(health_url "fd7a:115c:a1e0::1" "7878")"

    assert_eq "health_url dials a hostname as-is" \
        "http://ubu-3xdv:7878/v1/health" "$(health_url "ubu-3xdv" "7878")"

    # verify_health greps the port out of the env file, so a file written
    # before SOLADOR_AGENT_PORT existed hands over an empty string, not an
    # absent argument.
    assert_eq "health_url falls back to 7878 for an empty port" \
        "http://127.0.0.1:7878/v1/health" "$(health_url "127.0.0.1" "")"
    assert_eq "health_url honors a custom port" \
        "http://127.0.0.1:9999/v1/health" "$(health_url "127.0.0.1" "9999")"
    assert_eq "health_url defaults both arguments" \
        "http://127.0.0.1:7878/v1/health" "$(health_url)"
}

# ---- health_version ---------------------------------------------------------

test_health_version() {
    assert_eq "health_version reads the version out of a /v1/health body" \
        "0.4.0" \
        "$(health_version '{"status":"ok","hostname":"ubu-3xdv","version":"0.4.0","samplerStale":false}')"

    assert_eq "health_version tolerates whitespace around the colon" \
        "0.4.0" "$(health_version '{ "version" : "0.4.0" }')"

    # Never fabricate: a body that does not carry a version yields nothing, and
    # verify_health renders that as "unknown" rather than as a match.
    assert_empty "health_version prints nothing when the key is absent" \
        "$(health_version '{"status":"ok","hostname":"ubu-3xdv"}')"
    assert_empty "health_version prints nothing for a null version" \
        "$(health_version '{"status":"ok","version":null}')"
    assert_empty "health_version prints nothing for an empty body" \
        "$(health_version "")"
    assert_empty "health_version prints nothing with no argument" \
        "$(health_version)"
}

# ---- target_dir -------------------------------------------------------------

test_target_dir() {
    local workspace crate got real_ws

    workspace="$TMP/ws"
    crate="$workspace/agent"
    mkdir -p "$crate"
    : > "$workspace/Cargo.toml"

    # The CARGO_TARGET_DIR branch short-circuits before cargo is consulted. The
    # stub is rigged to fail locate-project, so an answer here can only have
    # come from the environment.
    export STUB_CARGO_WORKSPACE_MANIFEST=""
    got="$(
        PATH="$STUBS:$PATH"
        export CARGO_TARGET_DIR="$TMP/elsewhere"
        target_dir "$crate" 2>"$STDERR"
    )"
    assert_eq "target_dir honors CARGO_TARGET_DIR without consulting cargo" \
        "$TMP/elsewhere" "$got"

    # #268 itself: the answer is the *workspace* target dir. The crate-local
    # agent/target/ is the wrong one twice over — cargo stopped writing to it
    # when #264 landed, and on any host installed before that it still holds a
    # binary of the right name from the last standalone build.
    export STUB_CARGO_WORKSPACE_MANIFEST="$workspace/Cargo.toml"
    got="$(
        PATH="$STUBS:$PATH"
        target_dir "$crate" 2>"$STDERR"
    )"
    assert_eq "target_dir resolves the workspace target dir, not the crate's (#268)" \
        "$workspace/target" "$got"
    if [ "$got" = "$crate/target" ]; then
        fail "target_dir must never answer with the crate-local target dir" \
            "got the pre-#264 path: [$got]"
    else
        pass "target_dir must never answer with the crate-local target dir"
    fi

    # Not in a workspace: fail, so the caller cannot go looking for a binary.
    export STUB_CARGO_WORKSPACE_MANIFEST=""
    (
        PATH="$STUBS:$PATH"
        target_dir "$crate"
    ) >/dev/null 2>&1
    assert_eq "target_dir fails when cargo cannot locate a workspace" "1" "$?"

    # ...and the same two questions against the real cargo, because the stub
    # only proves this code handles the answer it was told to expect.
    if command -v cargo >/dev/null 2>&1; then
        real_ws="$TMP/realws"
        mkdir -p "$real_ws/member/src"
        cat > "$real_ws/Cargo.toml" <<'TOML'
[workspace]
resolver = "2"
members = ["member"]
TOML
        cat > "$real_ws/member/Cargo.toml" <<'TOML'
[package]
name = "solador-deploy-test-member"
version = "0.0.0"
edition = "2021"
TOML
        : > "$real_ws/member/src/lib.rs"
        got="$(target_dir "$real_ws/member" 2>"$STDERR")"
        assert_eq "target_dir agrees with the real cargo locate-project" \
            "$real_ws/target" "$got"

        mkdir -p "$TMP/nows"
        if ancestor_has_manifest "$TMP/nows"; then
            fail "target_dir fails outside a workspace (real cargo)" \
                "precondition unmet: an ancestor of $TMP/nows carries a Cargo.toml," \
                "so cargo would resolve that workspace and this would assert nothing"
        else
            target_dir "$TMP/nows" >/dev/null 2>&1
            assert_eq "target_dir fails outside a workspace (real cargo)" "1" "$?"
        fi
    else
        skip "target_dir agrees with the real cargo locate-project" "cargo not on PATH"
        skip "target_dir fails outside a workspace (real cargo)" "cargo not on PATH"
    fi

    unset STUB_CARGO_WORKSPACE_MANIFEST
}

# ---- build_release_binary ---------------------------------------------------

test_build_release_binary() {
    local workspace crate stale built custom out status

    workspace="$TMP/bws"
    crate="$workspace/agent"
    mkdir -p "$crate/target/release" "$workspace/target/release"
    : > "$workspace/Cargo.toml"

    export STUB_CARGO_WORKSPACE_MANIFEST="$workspace/Cargo.toml"
    export STUB_CARGO_BUILD_EXIT=0
    export STUB_CARGO_ARGV="$TMP/cargo-argv"

    # ---- the #268 lock ----
    # A stale pre-#264 binary sits in the crate-local target dir — the exact
    # state of every host installed before the workspace move — and the
    # workspace target dir has nothing. This is the case a *lenient*
    # implementation gets wrong and still passes a naive "did it find a
    # binary?" test: it finds the stale one, installs it, and the deploy
    # reports success over code that may be several releases old. Refusing is
    # the entire contract.
    stale="$crate/target/release/solador-agent"
    make_fake_binary "$stale"

    : > "$STUB_CARGO_ARGV"
    out="$(
        PATH="$STUBS:$PATH"
        build_release_binary "$crate" "solador-agent" 2>"$STDERR"
    )"
    status=$?
    assert_eq "build_release_binary fails rather than falling back to the crate-local target dir (#268)" \
        "1" "$status"
    assert_empty "build_release_binary prints no path when the build produced none" "$out"
    assert_file_has "the failure names the path it actually looked at" \
        "$STDERR" "$workspace/target/release/solador-agent"
    if grep -qF -- "$stale" "$STDERR"; then
        fail "build_release_binary never offers the stale crate-local binary" \
            "the pre-#264 path appeared in its output"
    else
        pass "build_release_binary never offers the stale crate-local binary"
    fi

    # `-p` is load-bearing, not tidiness: a bare `cargo build` inside the
    # workspace resolves app/src-tauri too, and a headless metrics host has no
    # webkit2gtk and no reason to grow one.
    assert_eq "build_release_binary scopes the build to the agent package" \
        "build --release -p solador-agent" "$(head -n1 "$STUB_CARGO_ARGV")"

    # ---- the happy path ----
    built="$workspace/target/release/solador-agent"
    make_fake_binary "$built"
    out="$(
        PATH="$STUBS:$PATH"
        build_release_binary "$crate" "solador-agent" 2>"$STDERR"
    )"
    status=$?
    assert_eq "build_release_binary succeeds when cargo wrote the binary" "0" "$status"
    assert_eq "build_release_binary prints the workspace target path" "$built" "$out"

    # A file that is not executable is not a binary to install. Skipped under
    # root, where -x is true for everything and the test would assert nothing.
    if [ "$(id -u)" = "0" ]; then
        skip "build_release_binary rejects a non-executable file at the target path" "running as root; -x is always true"
    else
        chmod 0644 "$built"
        out="$(
            PATH="$STUBS:$PATH"
            build_release_binary "$crate" "solador-agent" 2>"$STDERR"
        )"
        status=$?
        assert_eq "build_release_binary rejects a non-executable file at the target path" \
            "1" "$status"
        assert_empty "build_release_binary prints no path for a non-executable file" "$out"
        chmod 0755 "$built"
    fi

    # A failed build must not print a path either — the binary sitting at the
    # target path is now the *previous* build, and installing it would ship
    # exactly the stale code #268 was about.
    export STUB_CARGO_BUILD_EXIT=101
    out="$(
        PATH="$STUBS:$PATH"
        build_release_binary "$crate" "solador-agent" 2>"$STDERR"
    )"
    status=$?
    assert_eq "build_release_binary propagates a cargo build failure" "1" "$status"
    assert_empty "build_release_binary prints no path when cargo failed" "$out"
    export STUB_CARGO_BUILD_EXIT=0

    # An operator with CARGO_TARGET_DIR set is honored end to end.
    custom="$TMP/ctd"
    mkdir -p "$custom/release"
    make_fake_binary "$custom/release/solador-agent"
    out="$(
        PATH="$STUBS:$PATH"
        export CARGO_TARGET_DIR="$custom"
        build_release_binary "$crate" "solador-agent" 2>"$STDERR"
    )"
    assert_eq "build_release_binary follows CARGO_TARGET_DIR" \
        "$custom/release/solador-agent" "$out"

    # No workspace, no answer — and again, no search for something that looks
    # close enough.
    export STUB_CARGO_WORKSPACE_MANIFEST=""
    out="$(
        PATH="$STUBS:$PATH"
        build_release_binary "$crate" "solador-agent" 2>"$STDERR"
    )"
    status=$?
    assert_eq "build_release_binary fails when the target dir cannot be located" "1" "$status"
    assert_empty "build_release_binary prints no path with no target dir" "$out"

    unset STUB_CARGO_WORKSPACE_MANIFEST STUB_CARGO_BUILD_EXIT STUB_CARGO_ARGV
}

# ---- verify_health ----------------------------------------------------------

test_verify_health() {
    local env_file token out status

    # Distinctive so the "never printed" assertions below cannot pass by
    # accident. It is written only to files under $TMP, which is removed on exit.
    token="tok-MUST-NOT-BE-PRINTED-9f3c"
    env_file="$TMP/agent.env"
    printf 'SOLADOR_AGENT_TOKEN=%s\nSOLADOR_AGENT_BIND=127.0.0.1\nSOLADOR_AGENT_PORT=7878\n' \
        "$token" > "$env_file"

    export STUB_CURL_ARGV="$TMP/curl-argv"
    : > "$STUB_CURL_ARGV"

    # No env file: refuse. Probing some default would verify a different agent.
    (
        PATH="$STUBS:$PATH"
        export VERIFY_HEALTH_ATTEMPTS=1
        verify_health "$TMP/missing.env" "0.4.0"
    ) >/dev/null 2>&1
    assert_eq "verify_health fails when the env file is absent" "1" "$?"

    # No token: refuse before sending anything. An unauthenticated probe gets a
    # 401 that would read as "not up yet".
    printf 'SOLADOR_AGENT_BIND=127.0.0.1\n' > "$TMP/no-token.env"
    (
        PATH="$STUBS:$PATH"
        export VERIFY_HEALTH_ATTEMPTS=1
        verify_health "$TMP/no-token.env" "0.4.0"
    ) >/dev/null 2>&1
    assert_eq "verify_health fails when the env file carries no token" "1" "$?"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "verify_health sends no request without a token" "curl was invoked anyway"
    else
        pass "verify_health sends no request without a token"
    fi

    # The version being served matches the version just built.
    export STUB_CURL_BODY='{"status":"ok","hostname":"ubu-3xdv","version":"0.4.0"}'
    out="$(
        PATH="$STUBS:$PATH"
        export VERIFY_HEALTH_ATTEMPTS=1
        verify_health "$env_file" "0.4.0" 2>&1
    )"
    status=$?
    assert_eq "verify_health passes when the served version matches" "0" "$status"
    assert_output_has "verify_health reports the version it saw" "$out" "0.4.0"
    case "$out" in
        *"$token"*) fail "verify_health never prints the bearer token" "the token appeared in its output" ;;
        *) pass "verify_health never prints the bearer token" ;;
    esac
    if grep -qF -- "Authorization: Bearer" "$STUB_CURL_ARGV"; then
        pass "verify_health authenticates the probe"
    else
        fail "verify_health authenticates the probe" "no Authorization header reached curl"
    fi

    # The damning case: the unit is up, healthy, and serving the wrong code.
    # Both numbers have to be named or the operator cannot tell this from a
    # slow start.
    export STUB_CURL_BODY='{"status":"ok","hostname":"ubu-3xdv","version":"0.3.1"}'
    out="$(
        PATH="$STUBS:$PATH"
        export VERIFY_HEALTH_ATTEMPTS=1
        verify_health "$env_file" "0.4.0" 2>&1
    )"
    status=$?
    assert_eq "verify_health fails when a stale binary is serving" "1" "$status"
    assert_output_has "the mismatch is named as one" "$out" "VERSION MISMATCH"
    assert_output_has "the mismatch names the served version" "$out" "0.3.1"
    assert_output_has "the mismatch names the built version" "$out" "0.4.0"
    case "$out" in
        *"$token"*) fail "verify_health never prints the token on the failure path" "the token appeared in its output" ;;
        *) pass "verify_health never prints the token on the failure path" ;;
    esac

    # A healthy body with no version is not a match. Never fabricate one.
    export STUB_CURL_BODY='{"status":"ok","hostname":"ubu-3xdv"}'
    (
        PATH="$STUBS:$PATH"
        export VERIFY_HEALTH_ATTEMPTS=1
        verify_health "$env_file" "0.4.0"
    ) >/dev/null 2>&1
    assert_eq "verify_health fails when the body carries no version" "1" "$?"

    # The rollback form. The .prev binary's version cannot be known statically
    # (the agent has no --version flag), so any answer at all is the contract —
    # and an answer without a version reports "unknown", not a number.
    out="$(
        PATH="$STUBS:$PATH"
        export VERIFY_HEALTH_ATTEMPTS=1
        verify_health "$env_file" "" 2>&1
    )"
    status=$?
    assert_eq "verify_health accepts any version on the rollback path" "0" "$status"
    assert_output_has "an unreadable version reports unknown, not a guess" "$out" "unknown"

    export STUB_CURL_BODY='{"status":"ok","version":"0.3.1"}'
    (
        PATH="$STUBS:$PATH"
        export VERIFY_HEALTH_ATTEMPTS=1
        verify_health "$env_file" ""
    ) >/dev/null 2>&1
    assert_eq "verify_health accepts the previous version on the rollback path" "0" "$?"

    # Nothing answering at all.
    export STUB_CURL_BODY=""
    (
        PATH="$STUBS:$PATH"
        export VERIFY_HEALTH_ATTEMPTS=1
        verify_health "$env_file" ""
    ) >/dev/null 2>&1
    assert_eq "verify_health fails when the agent never comes back online" "1" "$?"

    unset STUB_CURL_BODY STUB_CURL_ARGV
}

# ---- source-level invariants ------------------------------------------------
#
# These three are not reachable at runtime without a real host, and each one
# breaks silently — the deploy keeps reporting success while doing the wrong
# thing. That is exactly the shape of failure this file exists to catch.

test_deploy_script_invariants() {
    local install_sh redeploy_sh deploy_body
    install_sh="$SCRIPT_DIR/install.sh"
    redeploy_sh="$SCRIPT_DIR/redeploy.sh"

    # The pre-rename handover. These names are not ours to tidy: they name what
    # is already sitting on deployed hosts. Rename them and install.sh finds no
    # existing token on an old host, mints a fresh one, and the cockpit's stored
    # per-host credential silently stops matching — with nothing anywhere
    # reporting the divergence.
    assert_file_has "install.sh still knows the pre-rename binary name" \
        "$install_sh" 'LEGACY_BIN_NAME="devcanopy-agent"'
    assert_file_has "install.sh still carries the pre-rename token across" \
        "$install_sh" 'DEVCANOPY_AGENT_TOKEN='
    assert_file_has "install.sh still carries the pre-rename bind across" \
        "$install_sh" 'DEVCANOPY_AGENT_BIND='

    # Order matters as much as presence: the old unit holds the port the new
    # one wants. Stop it after the new unit starts and you get an EADDRINUSE
    # crash-loop every three seconds while the old agent keeps answering
    # /v1/health on the old token — a failure that names neither the port
    # conflict nor the other unit.
    assert_before "install.sh stops the legacy unit before starting the new one" \
        "$install_sh" \
        'systemctl --user stop "$LEGACY_BIN_NAME"' \
        'systemctl --user restart "$BIN_NAME"'

    # The deploy path only. rollback stages and renames too, and asserting
    # across both at once compares markers from two unrelated code paths.
    deploy_body="$TMP/redeploy-do_deploy.sh"
    extract_function "$redeploy_sh" "do_deploy" > "$deploy_body"
    if [ ! -s "$deploy_body" ]; then
        fail "redeploy.sh still defines do_deploy()" \
            "could not extract it from $redeploy_sh — the assertions below would assert nothing"
        return
    fi
    pass "redeploy.sh still defines do_deploy()"

    # The ETXTBSY-safe swap: a running binary cannot be overwritten in place,
    # but the kernel allows a rename over it.
    assert_file_has "redeploy.sh stages the new binary beside the live one" \
        "$deploy_body" '$SUDO install -m 0755 "$built_bin" "$NEW_BIN"'
    assert_file_has "redeploy.sh swaps it in with an atomic rename" \
        "$deploy_body" '$SUDO mv -f "$NEW_BIN" "$INSTALL_PATH"'

    # And .prev has to be taken before the swap, or the rollback anchor is a
    # copy of the binary being rolled back and `rollback` is a no-op.
    assert_before "redeploy.sh preserves .prev before swapping" \
        "$deploy_body" \
        '$SUDO cp -p "$INSTALL_PATH" "$PREV_BIN"' \
        '$SUDO mv -f "$NEW_BIN" "$INSTALL_PATH"'
}

# ---- run --------------------------------------------------------------------

printf 'agent/deploy/lib.sh\n\n'

test_crate_version
test_health_url
test_health_version
test_target_dir
test_build_release_binary
test_verify_health
test_deploy_script_invariants

printf '\npassed %d, failed %d, skipped %d\n' "$PASSED" "$FAILED" "$SKIPPED"
if [ "$SKIPPED" -gt 0 ]; then
    printf 'NOTE: %d test(s) asserted nothing — see the SKIP lines above.\n' "$SKIPPED"
fi
if [ "$FAILED" -gt 0 ]; then
    exit 1
fi
