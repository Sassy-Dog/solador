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
# (build_release_binary refusing to fall back), plus the source-level
# invariants that no runtime test can reach — and, since #392, the install
# flow itself, run end to end against a temporary HOME with every host command
# stubbed (curl, systemctl, launchctl, uname, sw_vers, …), and, since #393,
# scripts/agent-standby-key.sh against a file-backed `doppler` stub. Nothing
# here talks to a host or a network. What is NOT stubbed is the signature verifier: the
# tamper-rejection cases run the real `minisign` against fixtures signed with a
# throwaway key, and report themselves as SKIPPED when it is not installed,
# because a stubbed verifier that always says yes would make those cases prove
# nothing. A real launchd bootstrap under a throwaway label is opt-in
# (SOLADOR_DEPLOY_TEST_LAUNCHD=1, macOS only); everything else is hermetic.
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

# Hermetic: all of these are read by the code under test and must not leak in
# from whatever shell invoked the suite.
unset CARGO_TARGET_DIR
unset VERIFY_HEALTH_ATTEMPTS
unset SOLADOR_AGENT_RELEASE SOLADOR_AGENT_BIND SOLADOR_AGENT_PORT SOLADOR_AGENT_LAUNCHD_LABEL
unset STUB_UNAME_S STUB_UNAME_M STUB_SW_VERS STUB_CURL_REDIRECT STUB_CURL_BODY
unset STUB_LAUNCHCTL_DOMAIN_EXIT STUB_LAUNCHCTL_LOADED_EXIT STUB_LAUNCHCTL_BOOTSTRAP_EXIT
unset STUB_LAUNCHCTL_DISABLED_LABEL STUB_LAUNCHCTL_DISABLED_WORD STUB_PLUTIL_EXIT
unset STUB_SYSTEMCTL_USER_EXIT STUB_TAILSCALE_IP STUB_CURL_FIXTURES
unset STUB_SYSTEMCTL_TIMER_EXIT STUB_SYSTEMCTL_IS_ENABLED STUB_PLUTIL_FAIL_MATCH STUB_PROBE_EXIT
unset STUB_DATE_EPOCH STUB_DATE_EXIT STUB_WAKETIME_SEC STUB_BOOTTIME_SEC STUB_SYSCTL_EXIT STUB_SYSCTL_ARGV

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

# A skip whose only cause is "minisign is not installed". Locally that is a
# loud SKIP; in CI, where the job installs minisign on purpose, it is a FAIL
# (SOLADOR_DEPLOY_TEST_REQUIRE_MINISIGN=1) — otherwise deleting the apt step
# would turn the load-bearing tamper cases into skips and the job green.
skip_needs_minisign() {
    if [ "${SOLADOR_DEPLOY_TEST_REQUIRE_MINISIGN:-}" = "1" ]; then
        fail "$1" "a usable minisign is required here (SOLADOR_DEPLOY_TEST_REQUIRE_MINISIGN=1): $MINISIGN_SKIP_REASON"
    else
        skip "$1" "$MINISIGN_SKIP_REASON"
    fi
}

# Print a file's permission bits as three octal digits, on either stat.
# GNU first: `stat -f` is GNU's --file-system flag, so trying the BSD form
# first prints a filesystem block and exits 1, and the fallback's answer is
# appended to that block rather than replacing it. BSD's `stat -c` prints
# nothing and exits 1, so this order is the one that works on both.
file_mode() {
    stat -c '%a' "$1" 2>/dev/null || stat -f '%Lp' "$1"
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

# assert_curl_authenticated_via_stdin <who> <token>
# The bearer token must reach curl as a `-K -` config line on stdin — which
# the curl stub records with a `[config] ` prefix — and must appear on NO argv
# line. Reverting the transport to `-H "Authorization: Bearer …"` (or dropping
# the header) fails here; a grep for the header alone could not tell the two
# apart, which is exactly how the first review found this assertion vacuous.
assert_curl_authenticated_via_stdin() {
    local who="$1" token="$2"
    if grep -qF -- "[config] header = \"Authorization: Bearer $token\"" "$STUB_CURL_ARGV"; then
        pass "$who authenticates the probe through curl's stdin config"
    else
        fail "$who authenticates the probe through curl's stdin config" \
            "no [config] header line with the token reached curl"
    fi
    if grep -v '^\[config\] ' "$STUB_CURL_ARGV" | grep -qF -- "$token"; then
        fail "$who keeps the token off curl's argv" "the token appeared on an argv line"
    elif grep -v '^\[config\] ' "$STUB_CURL_ARGV" | grep -qi 'authorization'; then
        fail "$who keeps the token off curl's argv" "an Authorization header appeared on an argv line"
    else
        pass "$who keeps the token off curl's argv"
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
# The opt-in launchd smoke registers the throwaway services it bootstraps
# here (space-separated `gui/<uid>/<label>` ids), so an interrupted run —
# Ctrl-C mid-poll — does not leave a KeepAlive job respawning a binary from
# a $TMP that is about to be removed.
SMOKE_SERVICES=""
cleanup() {
    local svc
    for svc in $SMOKE_SERVICES; do
        launchctl bootout "$svc" >/dev/null 2>&1 || true
    done
    rm -rf "$TMP"
}
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

# curl, three shapes, told apart by their flags:
#   -w '%{url_effective}'  the latest-release redirect: prints STUB_CURL_REDIRECT
#                          (fails like a 404 when it is empty)
#   -o <dest> <url>        an asset download: copies STUB_CURL_FIXTURES/<basename
#                          of url> to <dest>, or fails like `curl -f` on a 404
#   anything else          the health probe: prints STUB_CURL_BODY, or exits
#                          non-zero like `curl -f` when there is no body
# Every invocation is appended to STUB_CURL_ARGV when that is set.
cat > "$STUBS/curl" <<'STUB'
#!/usr/bin/env bash
if [ -n "${STUB_CURL_ARGV:-}" ]; then
    printf '%s\n' "$*" >> "$STUB_CURL_ARGV"
fi
dest=""
url=""
want_effective=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o) dest="$2"; shift ;;
        -w) case "$2" in *url_effective*) want_effective=true ;; esac; shift ;;
        -K)
            # A config file. `-` is stdin, which is how verify_health passes
            # the Authorization header; record its lines beside the argv so
            # the "authenticates" assertions can see what real curl would.
            if [ "$2" = "-" ] && [ -n "${STUB_CURL_ARGV:-}" ]; then
                sed 's/^/[config] /' >> "$STUB_CURL_ARGV"
            fi
            shift
            ;;
        -H | --proto | --retry) shift ;;
        -*) ;;
        *) url="$1" ;;
    esac
    shift
done
if [ "$want_effective" = true ]; then
    [ -n "${STUB_CURL_REDIRECT:-}" ] || exit 22
    printf '%s' "$STUB_CURL_REDIRECT"
    exit 0
fi
if [ -n "$dest" ] && [ "$dest" != "/dev/null" ]; then
    src="${STUB_CURL_FIXTURES:-/nonexistent}/$(basename "$url")"
    if [ -f "$src" ]; then
        cp "$src" "$dest"
        exit 0
    fi
    echo "curl: (22) The requested URL returned error: 404" >&2
    exit 22
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

# The host commands install.sh drives. Each one records its argv when asked
# and otherwise answers what the tests tell it to; none of them touches the
# machine this suite runs on.
cat > "$STUBS/uname" <<'STUB'
#!/usr/bin/env bash
case "${1:-}" in
    -s) printf '%s\n' "${STUB_UNAME_S:-Linux}" ;;
    -m) printf '%s\n' "${STUB_UNAME_M:-x86_64}" ;;
    *) printf '%s\n' "${STUB_UNAME_S:-Linux}" ;;
esac
STUB

cat > "$STUBS/sw_vers" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "${STUB_SW_VERS-15.6}"
STUB

# systemctl: `--user show-environment` (the reachability preflight) answers
# STUB_SYSTEMCTL_USER_EXIT; `is-enabled` prints STUB_SYSTEMCTL_IS_ENABLED
# (default `enabled`, exit 1 for anything else, like the real one); enabling
# the update timer answers STUB_SYSTEMCTL_TIMER_EXIT; everything else
# succeeds.
cat > "$STUBS/systemctl" <<'STUB'
#!/usr/bin/env bash
if [ -n "${STUB_SYSTEMCTL_ARGV:-}" ]; then
    printf '%s\n' "$*" >> "$STUB_SYSTEMCTL_ARGV"
fi
case "${2:-}" in
    show-environment) exit "${STUB_SYSTEMCTL_USER_EXIT:-0}" ;;
    is-enabled)
        printf '%s\n' "${STUB_SYSTEMCTL_IS_ENABLED:-enabled}"
        [ "${STUB_SYSTEMCTL_IS_ENABLED:-enabled}" = "enabled" ] && exit 0
        exit 1
        ;;
esac
case "$*" in
    *"solador-agent-update.timer"*) exit "${STUB_SYSTEMCTL_TIMER_EXIT:-0}" ;;
esac
exit 0
STUB

cat > "$STUBS/loginctl" <<'STUB'
#!/usr/bin/env bash
if [ -n "${STUB_SYSTEMCTL_ARGV:-}" ]; then
    printf 'loginctl %s\n' "$*" >> "$STUB_SYSTEMCTL_ARGV"
fi
exit 0
STUB

# launchctl: `print gui/<uid>` answers STUB_LAUNCHCTL_DOMAIN_EXIT (the "is
# there a login session" check); `print gui/<uid>/<label>` answers
# STUB_LAUNCHCTL_LOADED_EXIT (is the service already loaded); `bootstrap`
# answers STUB_LAUNCHCTL_BOOTSTRAP_EXIT.
cat > "$STUBS/launchctl" <<'STUB'
#!/usr/bin/env bash
if [ -n "${STUB_LAUNCHCTL_ARGV:-}" ]; then
    printf '%s\n' "$*" >> "$STUB_LAUNCHCTL_ARGV"
fi
case "${1:-}" in
    print)
        case "${2:-}" in
            gui/*/*) exit "${STUB_LAUNCHCTL_LOADED_EXIT:-113}" ;;
            *) exit "${STUB_LAUNCHCTL_DOMAIN_EXIT:-0}" ;;
        esac
        ;;
    print-disabled)
        # launchd's override records; STUB_LAUNCHCTL_DISABLED_LABEL names a
        # service a legacy `unload -w` left disabled, and the word after `=>`
        # is `disabled` (Ventura+) or `true` (Big Sur / Monterey). Written as
        # separate lines so a `| grep -q` reader can take SIGPIPE.
        printf '\tdisabled services = {\n'
        printf '\t\t"com.example.other" => disabled\n'
        [ -n "${STUB_LAUNCHCTL_DISABLED_LABEL:-}" ] && printf '\t\t"%s" => %s\n' "$STUB_LAUNCHCTL_DISABLED_LABEL" "${STUB_LAUNCHCTL_DISABLED_WORD:-disabled}"
        printf '\t\t"com.example.another" => disabled\n'
        printf '\t}\n'
        ;;
    bootstrap)
        if [ "${STUB_LAUNCHCTL_BOOTSTRAP_EXIT:-0}" != 0 ]; then
            echo "Bootstrap failed: 5: Input/output error (stubbed)" >&2
        fi
        exit "${STUB_LAUNCHCTL_BOOTSTRAP_EXIT:-0}"
        ;;
    *) exit 0 ;;
esac
STUB

# plutil: real where it exists (macOS), so the rendered plist is genuinely
# linted there; a yes-man on Linux, which has no plutil to lint with.
# STUB_PLUTIL_EXIT forces a lint verdict, for the failure path;
# STUB_PLUTIL_FAIL_MATCH fails only a lint whose argv contains that string,
# so the updater plist can fail its lint while the metrics plist passes.
cat > "$STUBS/plutil" <<'STUB'
#!/usr/bin/env bash
if [ -n "${STUB_PLUTIL_EXIT:-}" ]; then
    exit "$STUB_PLUTIL_EXIT"
fi
if [ -n "${STUB_PLUTIL_FAIL_MATCH:-}" ]; then
    case "$*" in
        *"$STUB_PLUTIL_FAIL_MATCH"*) exit 1 ;;
    esac
fi
if [ -x /usr/bin/plutil ]; then
    exec /usr/bin/plutil "$@"
fi
exit 0
STUB

# The two clocks the launcher's update-mode guard reads (#394), in their own
# directory so they are on PATH only for the launcher cases that need them.
# `date +%s` answers STUB_DATE_EPOCH (anything else — the launcher's own log
# timestamps — goes to the real date); `sysctl -n kern.waketime` and
# `kern.boottime` print launchd's `{ sec = N, usec = M } …` shape from
# STUB_WAKETIME_SEC / STUB_BOOTTIME_SEC, or fail with STUB_SYSCTL_EXIT, and
# record every call in STUB_SYSCTL_ARGV so a test can prove the metrics path
# never asked.
STUBS_CLOCK="$TMP/stubs-clock"
mkdir -p "$STUBS_CLOCK"
REAL_DATE="$(command -v date)"
cat > "$STUBS_CLOCK/date" <<STUB
#!/usr/bin/env bash
if [ "\${1:-}" = "+%s" ] && [ -n "\${STUB_DATE_EXIT:-}" ]; then
    exit "\$STUB_DATE_EXIT"
fi
if [ "\${1:-}" = "+%s" ] && [ -n "\${STUB_DATE_EPOCH:-}" ]; then
    printf '%s\\n' "\$STUB_DATE_EPOCH"
    exit 0
fi
exec "$REAL_DATE" "\$@"
STUB
cat > "$STUBS_CLOCK/sysctl" <<'STUB'
#!/usr/bin/env bash
if [ -n "${STUB_SYSCTL_ARGV:-}" ]; then
    printf '%s\n' "$*" >> "$STUB_SYSCTL_ARGV"
fi
if [ -n "${STUB_SYSCTL_EXIT:-}" ]; then
    exit "$STUB_SYSCTL_EXIT"
fi
case "${2:-}" in
    kern.waketime) printf '{ sec = %s, usec = 550946 } Fri Sep 11 16:17:31 2026\n' "${STUB_WAKETIME_SEC:-0}" ;;
    kern.boottime) printf '{ sec = %s, usec = 442275 } Wed Aug 26 17:39:31 2026\n' "${STUB_BOOTTIME_SEC:-0}" ;;
    *) exit 1 ;;
esac
STUB
chmod +x "$STUBS_CLOCK"/*


cat > "$STUBS/tailscale" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "${STUB_TAILSCALE_IP:-100.64.0.9}"
STUB

chmod +x "$STUBS"/*

# A PATH for the install-flow tests that holds ONLY the stubs above plus a
# fixed list of real utilities, symlinked in — so "minisign is not installed"
# is a PATH this suite constructs rather than a machine state it hopes for,
# and the stubs cannot be shadowed by a real systemctl or launchctl. Built
# once; the tests pick TOOLBIN (real minisign linked in when the machine has
# one) or TOOLBIN_NOVERIFIER.
TOOLBIN="$TMP/toolbin"
TOOLBIN_NOVERIFIER="$TMP/toolbin-noverifier"
mkdir -p "$TOOLBIN" "$TOOLBIN_NOVERIFIER"
for tool in awk sed grep cut head tr od cat mkdir rm cp mv install chmod mktemp cmp \
            id dirname basename ls seq openssl env sh bash date sort; do
    real="$(command -v "$tool" 2>/dev/null || true)"
    if [ -n "$real" ]; then
        ln -s "$real" "$TOOLBIN/$tool"
        ln -s "$real" "$TOOLBIN_NOVERIFIER/$tool"
    fi
done
# The throwaway keypair the signature cases sign with. Generated up front so
# "minisign is usable" is one fact decided once: present, AND new enough for
# `-W` (unencrypted keys, minisign ≥ 0.11 — what Debian 12 and Ubuntu 24.04
# package; Ubuntu 22.04 and Debian 11 ship no minisign at all, and an older
# build from elsewhere lands here). Either shortfall is a SKIP with the
# reason, never a FAIL blamed on the code under test.
TEST_KEY_DIR="$TMP/keys"
HAVE_MINISIGN=false
MINISIGN_SKIP_REASON="minisign not installed (brew/apt/dnf install minisign)"
if command -v minisign >/dev/null 2>&1; then
    ln -s "$(command -v minisign)" "$TOOLBIN/minisign"
    mkdir -p "$TEST_KEY_DIR"
    if minisign -G -W -f -p "$TEST_KEY_DIR/a.pub" -s "$TEST_KEY_DIR/a.key" >/dev/null 2>&1 \
        && minisign -G -W -f -p "$TEST_KEY_DIR/b.pub" -s "$TEST_KEY_DIR/b.key" >/dev/null 2>&1; then
        HAVE_MINISIGN=true
    else
        MINISIGN_SKIP_REASON="$(minisign -v 2>&1 | head -n1) cannot generate an unencrypted key (-W needs minisign >= 0.11)"
    fi
fi

# The bypass: a "verifier" that accepts everything. Used ONCE, to show that
# the tamper-rejection case below is minisign's doing and not some other
# failure that happened to land first — the proof docs/AGENT-DISTRIBUTION.md's
# Testing section requires of the load-bearing test. It answers `-v` the way
# the real one does because verify_agent_signature checks that the tool on
# PATH identifies itself as minisign — the point of the bypass is to defeat
# the signature check, not the identity check.
STUBS_BYPASS="$TMP/stubs-bypass"
mkdir -p "$STUBS_BYPASS"
cat > "$STUBS_BYPASS/minisign" <<'STUB'
#!/usr/bin/env bash
[ "${1:-}" = "-v" ] && echo "minisign 0.0-accept-everything-stub"
exit 0
STUB
chmod +x "$STUBS_BYPASS/minisign"

# A PATH for the install cases that never reach signature verification —
# argument refusal, platform preflight, release resolution. They must not
# depend on a real minisign being installed, so they carry the stub as a
# stand-in that satisfies the preflight's presence check and nothing else.
NOVERIFY_PATH="$STUBS_BYPASS:$STUBS:$TOOLBIN_NOVERIFIER"

# The same stubs with no `tailscale` (and no `ip`): a host with nothing to
# detect a bind address from.
mkdir -p "$TMP/stubs-no-tailscale"
for stub in "$STUBS"/*; do
    [ "$(basename "$stub")" = "tailscale" ] && continue
    ln -s "$stub" "$TMP/stubs-no-tailscale/$(basename "$stub")"
done

# Something executable that is not the real agent. The deploy scripts only ever
# test these for -x and hand the path to `install`, so a shell stub is enough.
make_fake_binary() {
    printf '#!/bin/sh\nexit 0\n' > "$1"
    chmod +x "$1"
}

# ---- binary_version ---------------------------------------------------------

# Write a stub "agent binary" that answers --version the way the real one does.
# `printf '%s\n'` and nothing else: the contract is one line carrying the
# version and no decoration.
stub_agent_binary() {
    local path="$1" version="$2"
    cat > "$path" <<STUB
#!/bin/sh
if [ "\$1" = "--version" ]; then
    printf '%s\n' '$version'
    exit 0
fi
echo "stub agent: unexpected args: \$*" >&2
exit 2
STUB
    chmod +x "$path"
}

test_binary_version() {
    local bin

    bin="$TMP/agent-ok"
    stub_agent_binary "$bin" "2026.9.3"
    assert_eq "binary_version reads --version out of the artifact" \
        "2026.9.3" "$(binary_version "$bin")"

    # The version this asserts against /v1/health is the CalVer the build
    # compiled in, NOT agent/Cargo.toml's number. Those diverged at #390 and the
    # manifest one names no release, so reading the manifest here would compare
    # the wrong number and blame the agent for the mismatch.
    bin="$TMP/agent-calver"
    stub_agent_binary "$bin" "2026.12.41"
    assert_eq "binary_version returns the CalVer, not a crate semver" \
        "2026.12.41" "$(binary_version "$bin")"

    # Trailing whitespace or a stray CR (a binary built on a checkout with
    # autocrlf, say) must not become part of the string the health check
    # compares — it would fail with two identical-looking versions on screen.
    bin="$TMP/agent-crlf"
    cat > "$bin" <<'STUB'
#!/bin/sh
printf '2026.9.3 
'
STUB
    chmod +x "$bin"
    assert_eq "binary_version strips whitespace and CR" \
        "2026.9.3" "$(binary_version "$bin")"

    # More than one line is not a contract violation worth dying over — take the
    # first and move on. Written with parameter expansion rather than `head -n1`
    # for a reason this case also guards: under `set -o pipefail`, `head`
    # closing the pipe early can hand the producer a SIGPIPE and take the whole
    # deploy down over a version string that parsed perfectly well.
    bin="$TMP/agent-chatty"
    cat > "$bin" <<'STUB'
#!/bin/sh
printf '2026.9.3\nbuilt from abc1234\nand a third line\n'
STUB
    chmod +x "$bin"
    assert_eq "binary_version takes the first line without a broken pipe" \
        "2026.9.3" "$(binary_version "$bin")"

    # FAIL CLOSED. A binary that cannot name itself exits non-zero and prints
    # nothing; if this returned success with an empty string, verify_health
    # would take the empty form — "just come back online" — and a deploy that
    # never proved which binary is serving would report success.
    bin="$TMP/agent-unversioned"
    cat > "$bin" <<'STUB'
#!/bin/sh
echo "solador-agent: this build carries no version." >&2
exit 1
STUB
    chmod +x "$bin"
    binary_version "$bin" >/dev/null 2>&1
    assert_eq "binary_version fails when the binary carries no version" "1" "$?"

    bin="$TMP/agent-silent"
    printf '#!/bin/sh\nexit 0\n' > "$bin"
    chmod +x "$bin"
    binary_version "$bin" >/dev/null 2>&1
    assert_eq "binary_version fails when --version prints nothing" "1" "$?"

    binary_version "$TMP/does-not-exist" >/dev/null 2>&1
    assert_eq "binary_version fails on a missing binary" "1" "$?"

    # The real binary, whichever profile this machine happens to have built.
    # Debug counts: this asserts the CONTRACT (`--version` prints one dotted
    # line, or refuses), and the contract does not vary by profile — while the
    # CI job that runs this suite builds debug, so insisting on release would
    # make the one case reading a real artifact a permanent skip.
    #
    # BOTH outcomes are correct and which one applies is not this suite's to
    # decide. CI checks out shallow, so the binary there genuinely carries no
    # version and `binary_version` MUST fail; a full checkout produces a
    # version and it must parse. Asserting either one unconditionally would
    # fail a correct build for being built somewhere else — so assert that the
    # two agree with each other instead.
    local real td
    td="$(target_dir "$SCRIPT_DIR/.." 2>/dev/null)"
    real=""
    for profile in release debug; do
        if [ -n "$td" ] && [ -x "$td/$profile/solador-agent" ]; then
            real="$td/$profile/solador-agent"
            break
        fi
    done
    if [ -n "$real" ]; then
        local got rc
        got="$(binary_version "$real" 2>/dev/null)"
        rc=$?
        if [ "$rc" -eq 0 ]; then
            case "$got" in
                [0-9]*.[0-9]*.[0-9]*)
                    pass "binary_version reads the real binary (got $got)"
                    ;;
                *)
                    fail "binary_version reads the real binary" \
                        "want: a dotted version on one line" "got:  [$got]"
                    ;;
            esac
        elif "$real" --version >/dev/null 2>&1; then
            fail "binary_version agrees with the binary it asked" \
                "binary_version refused, but $real --version succeeded"
        else
            pass "binary_version fails closed on a real binary that carries no version (shallow checkout)"
        fi
    else
        skip "binary_version reads the real binary" \
            "no solador-agent built under ${td:-<no target dir>} (cargo build -p solador-agent)"
    fi

    # Both callers must treat the failure as fatal. Neither may fall through to
    # verify_health with an empty expectation. install.sh's side is observed at
    # runtime by test_install_linux_flow ("aborts when the verified binary
    # carries no version"); redeploy.sh has no hermetic runner, so its side
    # stays a source-level assertion.
    assert_file_has "redeploy.sh aborts when the built binary has no version" \
        "$SCRIPT_DIR/redeploy.sh" 'target_version="$(binary_version "$built_bin")" || exit 1'
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
    # The header reaches curl through `-K -` (a config file on stdin) and never
    # on argv, which is world-readable via /proc/<pid>/cmdline. The stub logs
    # stdin config lines with a `[config] ` prefix, so this can tell the two
    # apart: the config line must carry the header, and no argv line may.
    assert_curl_authenticated_via_stdin "verify_health" "$token"

    # The escaping the config syntax needs — `"` and `\` inside the value —
    # asserted byte for byte on a token that has both, plus a quoted-and-CRLF
    # file that env_value must read the way the launcher does.
    # The raw token is tok"q\b — one double quote, one backslash — stored
    # quoted, on a CRLF line.
    printf 'SOLADOR_AGENT_TOKEN="tok"q\\b"\r\nSOLADOR_AGENT_BIND=127.0.0.1\nSOLADOR_AGENT_PORT=7878\n' > "$TMP/quoted.env"
    : > "$STUB_CURL_ARGV"
    (
        PATH="$STUBS:$PATH"
        export VERIFY_HEALTH_ATTEMPTS=1
        verify_health "$TMP/quoted.env" "0.4.0"
    ) >/dev/null 2>&1
    assert_eq "verify_health reads a quoted, CRLF token like the launcher does" "0" "$?"
    assert_file_has "the token's quote and backslash are escaped for curl's config" \
        "$STUB_CURL_ARGV" '[config] header = "Authorization: Bearer tok\"q\\b"'
    # Bounded per attempt: without these a blackholed tailnet bind waits out
    # the OS SYN timeout fifteen times over (18–30 minutes) with every test
    # green.
    if grep -qE -- '--connect-timeout 2 --max-time 5' "$STUB_CURL_ARGV"; then
        pass "verify_health bounds every probe with connect and total timeouts"
    else
        fail "verify_health bounds every probe with connect and total timeouts" \
            "no --connect-timeout/--max-time on curl's argv"
    fi

    # The exit-code hints an operator reads on the failure line.
    assert_output_has "curl_exit_hint names a refused connection" "$(curl_exit_hint 7)" "nothing listening"
    assert_output_has "curl_exit_hint names a timeout" "$(curl_exit_hint 28)" "timed out"
    assert_output_has "curl_exit_hint names an HTTP refusal" "$(curl_exit_hint 22)" "401"
    assert_output_has "curl_exit_hint names a dropped connection" "$(curl_exit_hint 56)" "check its log"
    assert_output_has "curl_exit_hint falls back to the manual" "$(curl_exit_hint 99)" "curl(1)"

    # The CalVer comparison the re-run downgrade guard uses.
    calver_newer 2026.10.1 2026.9.30 && pass "calver_newer compares fields numerically" \
        || fail "calver_newer compares fields numerically" "2026.10.1 should be newer than 2026.9.30"
    calver_newer 2026.9.8 2026.9.8 && fail "calver_newer is strict" "equal versions compared as newer" \
        || pass "calver_newer is strict"
    calver_newer 2026.9.8 2026.9.9 && fail "calver_newer orders patch numbers" "9.8 compared as newer than 9.9" \
        || pass "calver_newer orders patch numbers"
    calver_newer 2027.1.1 2026.12.99 && pass "calver_newer orders years first" \
        || fail "calver_newer orders years first" "2027.1.1 should be newer than 2026.12.99"
    calver_newer "" 2026.9.8 && fail "calver_newer treats an unparseable version as not newer" "empty compared as newer" \
        || pass "calver_newer treats an unparseable version as not newer"
    calver_newer 2026.9 2026.9.8 && fail "calver_newer needs three fields" "two fields compared as newer" \
        || pass "calver_newer needs three fields"

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

    # The rollback form, which asserts only that the agent came back: any answer
    # at all is the contract — and an answer without a version reports
    # "unknown", not a number. (`.prev` could be asked with `binary_version`
    # since #390; see lib.sh's note on why rollback deliberately does not.)
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

# ---- release-artifact helpers (#392) ----------------------------------------

test_agent_target_for() {
    # All four published triples, from the strings uname actually prints —
    # including macOS's arm64 for what Linux and the triple call aarch64, and
    # the amd64 some container images report for x86_64.
    assert_eq "agent_target_for maps Linux x86_64" \
        "x86_64-unknown-linux-musl" "$(agent_target_for Linux x86_64)"
    assert_eq "agent_target_for maps Linux aarch64" \
        "aarch64-unknown-linux-musl" "$(agent_target_for Linux aarch64)"
    assert_eq "agent_target_for maps Darwin arm64 (Apple Silicon)" \
        "aarch64-apple-darwin" "$(agent_target_for Darwin arm64)"
    assert_eq "agent_target_for maps Darwin x86_64 (Intel)" \
        "x86_64-apple-darwin" "$(agent_target_for Darwin x86_64)"
    assert_eq "agent_target_for normalises amd64 to x86_64" \
        "x86_64-unknown-linux-musl" "$(agent_target_for Linux amd64)"
    assert_eq "agent_target_for normalises Linux arm64 to aarch64" \
        "aarch64-unknown-linux-musl" "$(agent_target_for Linux arm64)"

    # Fail closed: nothing is published for these, and "nearest match" is how
    # a binary that cannot run here gets installed as a service.
    agent_target_for FreeBSD x86_64 >/dev/null 2>&1
    assert_eq "agent_target_for refuses an unsupported OS" "1" "$?"
    agent_target_for Linux riscv64 >/dev/null 2>&1
    assert_eq "agent_target_for refuses an unsupported architecture" "1" "$?"
    agent_target_for Windows_NT x86_64 >/dev/null 2>&1
    assert_eq "agent_target_for refuses Windows" "1" "$?"
    assert_empty "agent_target_for prints nothing for an unsupported platform" \
        "$(agent_target_for Linux i686 2>/dev/null)"

    assert_eq "agent_asset_name matches what build-agent.sh names the artifact" \
        "solador-agent-2026.9.8-aarch64-apple-darwin" \
        "$(agent_asset_name 2026.9.8 aarch64-apple-darwin)"
}

# The installer re-derives what scripts/config.sh and scripts/build-agent.sh
# define — the four triples, the asset name, the macOS floor — and the two
# are joined by nothing but this test. Sourced in a subshell: config.sh
# exports a dozen variables this suite has no use for.
# Prints one line per disagreement, nothing when they agree. A function rather
# than an inline `$( … )` because bash 3.2 cannot parse a `case` pattern's
# unbalanced `)` inside a command substitution.
release_contract_report() {
    local config="$1" triple got name floor
    # shellcheck source=scripts/config.sh
    . "$config" >/dev/null 2>&1
    for triple in $AGENT_LINUX_TARGETS $AGENT_MACOS_TARGETS; do
        case "$triple" in
            x86_64-unknown-linux-musl) got="$(agent_target_for Linux x86_64)" ;;
            aarch64-unknown-linux-musl) got="$(agent_target_for Linux aarch64)" ;;
            aarch64-apple-darwin) got="$(agent_target_for Darwin arm64)" ;;
            x86_64-apple-darwin) got="$(agent_target_for Darwin x86_64)" ;;
            *) got="<no mapping in agent_target_for>" ;;
        esac
        [ "$got" = "$triple" ] || printf 'triple %s -> %s\n' "$triple" "$got"
        name="$(agent_asset_name 2026.9.8 "$triple")"
        [ "$name" = "$AGENT_PACKAGE-2026.9.8-$triple" ] || printf 'asset %s\n' "$name"
    done
    floor="${AGENT_MACOS_MIN_VERSION%%.*}"
    grep -q "\"\$MACOS_MAJOR\" -lt $floor" "$SCRIPT_DIR/install.sh" \
        || printf 'macOS floor: install.sh does not compare against %s\n' "$floor"
}

test_release_contract_matches_config() {
    local config="$SCRIPT_DIR/../../scripts/config.sh"
    if [ ! -f "$config" ]; then
        fail "the installer's release contract matches scripts/config.sh" "no $config"
        return
    fi
    local report
    report="$( release_contract_report "$config" )"
    assert_empty "the installer's release contract matches scripts/config.sh" "$report"

    # The asset NAME has no home in config.sh — build-agent.sh composes it and
    # release.yml reads it back — so bind agent_asset_name to both producers'
    # spelling. A rename upstream fails here rather than 404ing every install.
    assert_file_has "agent_asset_name matches how build-agent.sh names the artifact" \
        "$SCRIPT_DIR/../../scripts/build-agent.sh" '$AGENT_PACKAGE-$MARKETING_VERSION-$triple'
    assert_file_has "agent_asset_name matches how release.yml attaches the artifact" \
        "$SCRIPT_DIR/../../.github/workflows/release.yml" 'solador-agent-$VERSION-$t'
    assert_eq "agent_asset_name composes package-version-triple" \
        "solador-agent-2026.9.8-x86_64-unknown-linux-musl" \
        "$(agent_asset_name 2026.9.8 x86_64-unknown-linux-musl)"
}

test_validate_release_tag() {
    validate_release_tag v2026.9.8 >/dev/null 2>&1
    assert_eq "validate_release_tag accepts a CalVer tag" "0" "$?"
    validate_release_tag v2026.12.141 >/dev/null 2>&1
    assert_eq "validate_release_tag accepts a two-digit month" "0" "$?"

    # The tag is interpolated into a URL, so anything that is not exactly a
    # tag is refused before it gets there.
    local bad
    for bad in 2026.9.8 v2026.9 v2026.9.8.1 main "v2026.9.8/../x" "" "v2026.9.8 " "V2026.9.8"; do
        validate_release_tag "$bad" >/dev/null 2>&1
        assert_eq "validate_release_tag refuses [$bad]" "1" "$?"
    done
    # grep matches per line: a second line hiding behind a valid first one
    # must not ride along into the URL.
    validate_release_tag "$(printf 'v2026.9.8\nevil')" >/dev/null 2>&1
    assert_eq "validate_release_tag refuses a tag with an embedded newline" "1" "$?"
    validate_release_tag "$(printf 'v2026.9.8\t')" >/dev/null 2>&1
    assert_eq "validate_release_tag refuses a tag with a control character" "1" "$?"
}

test_resolve_latest_release_tag() {
    local repo="https://github.com/Sassy-Dog/solador" got

    got="$(
        PATH="$STUBS:$PATH"
        export STUB_CURL_REDIRECT="$repo/releases/tag/v2026.9.8"
        resolve_latest_release_tag "$repo" 2>"$STDERR"
    )"
    assert_eq "resolve_latest_release_tag reads the tag off the /releases/latest redirect" \
        "v2026.9.8" "$got"

    # A repo with no releases lands on the listing page, not a tag. That is
    # not a version and must not become one.
    (
        PATH="$STUBS:$PATH"
        export STUB_CURL_REDIRECT="$repo/releases"
        resolve_latest_release_tag "$repo"
    ) >/dev/null 2>&1
    assert_eq "resolve_latest_release_tag refuses a redirect that is not a tag" "1" "$?"

    (
        PATH="$STUBS:$PATH"
        export STUB_CURL_REDIRECT="$repo/releases/tag/not-a-version"
        resolve_latest_release_tag "$repo"
    ) >/dev/null 2>&1
    assert_eq "resolve_latest_release_tag refuses a tag that is not CalVer" "1" "$?"

    (
        PATH="$STUBS:$PATH"
        export STUB_CURL_REDIRECT=""
        resolve_latest_release_tag "$repo"
    ) >/dev/null 2>&1
    assert_eq "resolve_latest_release_tag fails when the request fails" "1" "$?"
}

test_service_rendering() {
    local tpl got

    # An ordinary path is rendered bare, because redeploy.sh reads ExecStart
    # back with `awk '{print $1}'` and every host installed so far has one.
    assert_eq "systemd_exec_path leaves an ordinary path unquoted" \
        "/home/u/.local/bin/solador-agent" \
        "$(systemd_exec_path /home/u/.local/bin/solador-agent)"
    # Whitespace is quoted — that is what makes a HOME with a space work
    # without anyone editing the unit.
    assert_eq "systemd_exec_path double-quotes a path with spaces" \
        '"/home/some user/.local/bin/solador-agent"' \
        "$(systemd_exec_path "/home/some user/.local/bin/solador-agent")"

    # And the characters that mean something to systemd even inside quotes are
    # refused rather than rendered into a unit that starts the wrong thing.
    local bad
    for bad in '/home/pct%h/x' '/home/dollar$X/x' '/home/q"uote/x' '/home/back\slash/x' 'relative/path'; do
        systemd_exec_path "$bad" >/dev/null 2>&1
        assert_eq "systemd_exec_path refuses [$bad]" "1" "$?"
    done
    check_install_path "/Users/Some Name" >/dev/null 2>&1
    assert_eq "check_install_path accepts a HOME with spaces" "0" "$?"

    assert_eq "xml_escape escapes the five XML specials" \
        "/Users/A &amp; B &lt;&apos;&quot;x&quot;&gt;" \
        "$(xml_escape "/Users/A & B <'\"x\">")"

    # Rendering is literal: `&` and `\` in a home directory must land in the
    # file as themselves, which a sed replacement would not do.
    tpl="$TMP/render.tpl"
    printf 'ExecStart=@BIN@\nEnvironmentFile=@ENV@\n' > "$tpl"
    got="$(render_template "$tpl" "@BIN@" '/Users/A & B/x' "@ENV@" 'back\slash')"
    assert_eq "render_template substitutes placeholders literally" \
        "$(printf 'ExecStart=/Users/A & B/x\nEnvironmentFile=back\\slash')" "$got"
}

# ---- verify_agent_signature (real minisign) ---------------------------------
#
# THE load-bearing test of #392 (docs/AGENT-DISTRIBUTION.md, "Testing"): a
# tampered binary must fail, and the failure must be shown to be the
# verifier's. So these use the real minisign and a throwaway keypair; a
# machine without minisign gets loud SKIPs, never a stub that says yes.

# sign_fixture <file> <seckey>: writes <file>.minisig the way build-agent.sh
# does (trusted comment = the asset name). The keys live in TEST_KEY_DIR,
# generated at setup.
sign_fixture() {
    minisign -S -W -s "$2" -m "$1" -x "$1.minisig" -t "$(basename "$1")" \
        -c "solador-agent release signature" </dev/null >/dev/null 2>&1
}

test_verify_agent_signature() {
    if [ "$HAVE_MINISIGN" != true ]; then
        skip_needs_minisign "verify_agent_signature accepts a genuine signature"
        skip_needs_minisign "verify_agent_signature rejects modified bytes"
        skip_needs_minisign "verify_agent_signature rejects a signature from another key"
        return
    fi

    local f="$TMP/sig-fixture"
    printf '#!/bin/sh\necho 2026.9.8\n' > "$f"
    sign_fixture "$f" "$TEST_KEY_DIR/a.key"

    verify_agent_signature "$f" "$f.minisig" "$TEST_KEY_DIR/a.pub" >/dev/null 2>&1
    assert_eq "verify_agent_signature accepts a genuine signature" "0" "$?"

    # One byte changed after signing.
    printf '#!/bin/sh\necho 2026.9.9\n' > "$f"
    verify_agent_signature "$f" "$f.minisig" "$TEST_KEY_DIR/a.pub" >"$STDERR" 2>&1
    assert_eq "verify_agent_signature rejects modified bytes" "1" "$?"
    assert_file_has "the rejection says so in capitals" "$STDERR" "SIGNATURE VERIFICATION FAILED"
    printf '#!/bin/sh\necho 2026.9.8\n' > "$f"

    verify_agent_signature "$f" "$f.minisig" "$TEST_KEY_DIR/b.pub" >/dev/null 2>&1
    assert_eq "verify_agent_signature rejects a signature from another key" "1" "$?"

    # Fail closed on every missing piece: no signature, no key, no verifier.
    verify_agent_signature "$f" "$TMP/no-such.minisig" "$TEST_KEY_DIR/a.pub" >/dev/null 2>&1
    assert_eq "verify_agent_signature fails without a signature file" "1" "$?"
    verify_agent_signature "$f" "$f.minisig" "$TMP/no-such.pub" >/dev/null 2>&1
    assert_eq "verify_agent_signature fails without the public key" "1" "$?"
    (
        PATH="$TOOLBIN_NOVERIFIER"
        verify_agent_signature "$f" "$f.minisig" "$TEST_KEY_DIR/a.pub"
    ) >"$STDERR" 2>&1
    assert_eq "verify_agent_signature fails when minisign is not on PATH" "1" "$?"
    assert_file_has "the missing verifier is named, with how to get it" "$STDERR" "minisign not found"

    # Something that answers to the name but is not minisign — rsign, whose
    # `-V` prints a version and exits 0 — is refused before it is asked.
    local impostor="$TMP/impostor"
    mkdir -p "$impostor"
    printf '#!/usr/bin/env bash\ncase "$1" in -v) echo "rsign 0.6.6";; esac\nexit 0\n' > "$impostor/minisign"
    chmod +x "$impostor/minisign"
    (
        PATH="$impostor:$TOOLBIN_NOVERIFIER"
        verify_agent_signature "$f" "$f.minisig" "$TEST_KEY_DIR/a.pub"
    ) >"$STDERR" 2>&1
    assert_eq "verify_agent_signature refuses a verifier that does not identify as minisign" "1" "$?"
    assert_file_has "the impostor is named" "$STDERR" "does not identify itself as minisign"
}

# ---- the install flow (#392) ------------------------------------------------
#
# install.sh, run for real against a temporary HOME, from a copy of the
# checkout layout (agent/deploy/* beside agent/release-signing-key.pub) that
# carries the THROWAWAY public key — so the production key-resolution path is
# the one exercised, with no override in the installer for a test to reach
# for. Every host command is a stub; the verifier is real.

CHECKOUT="$TMP/checkout"
FIXTURES="$TMP/fixtures"

# Lay out a copy of agent/deploy plus the given public key as
# agent/release-signing-key.pub.
make_checkout() {
    local pubkey="$1"
    rm -rf "$CHECKOUT"
    mkdir -p "$CHECKOUT/agent/deploy"
    cp "$SCRIPT_DIR/install.sh" "$SCRIPT_DIR/lib.sh" "$SCRIPT_DIR/run-agent.sh" \
       "$SCRIPT_DIR/solador-agent.service" "$SCRIPT_DIR/app.solador.agent.plist" \
       "$SCRIPT_DIR/solador-agent-update.service" "$SCRIPT_DIR/solador-agent-update.timer" \
       "$SCRIPT_DIR/app.solador.agent.update.plist" \
       "$CHECKOUT/agent/deploy/"
    cp "$pubkey" "$CHECKOUT/agent/release-signing-key.pub"
}

# make_fixture <version> <triple> <seckey|-> [exec-marker]
# Writes FIXTURES/solador-agent-<version>-<triple> — a stub agent that answers
# --version, that records having been EXECUTED by touching <exec-marker>
# (the canary the rejection cases assert on), and that appends its argv to
# AGENT_ARGV (so "no update check was made" is asserted on what the binary
# was actually asked, not on curl's argv) — plus its .minisig, unless the
# key is "-".
AGENT_ARGV="$TMP/agent-argv"
make_fixture() {
    local version="$1" triple="$2" seckey="$3" marker="${4:-}" f
    mkdir -p "$FIXTURES"
    f="$FIXTURES/$(agent_asset_name "$version" "$triple")"
    cat > "$f" <<STUB
#!/bin/sh
[ -n "$marker" ] && : > "$marker"
printf '%s\\n' "\$*" >> "$AGENT_ARGV"
if [ "\$1" = "--version" ]; then
    printf '%s\n' '$version'
    exit 0
fi
exit 0
STUB
    rm -f "$f.minisig"
    if [ "$seckey" != "-" ]; then
        sign_fixture "$f" "$seckey"
    fi
    printf '%s\n' "$f"
}

# run_install <home> [args...]: runs the copied install.sh with HOME set,
# stdin from INSTALL_STDIN (a token, or nothing), PATH = stubs + the tool set
# in INSTALL_PATH, and the STUB_* variables the caller exported. Output
# (both streams) lands in $INSTALL_OUT; the exit status is returned AND kept
# in INSTALL_STATUS, for assertions that read the output first.
INSTALL_OUT="$TMP/install.out"
INSTALL_STDIN=""
INSTALL_PATH=""
INSTALL_SCRIPT=""
INSTALL_STATUS=""
run_install() {
    local home="$1"
    shift
    (
        export HOME="$home"
        export PATH="${INSTALL_PATH:-$STUBS:$TOOLBIN}"
        export VERIFY_HEALTH_ATTEMPTS=1
        export STUB_CURL_FIXTURES="$FIXTURES"
        unset XDG_CACHE_HOME
        export USER="${USER:-tester}"
        # INSTALL_UMASK: the operator's umask is not ours to assume; a case
        # runs under 002 to prove the env file and plist modes are explicit.
        umask "${INSTALL_UMASK:-022}"
        printf '%s' "$INSTALL_STDIN" | "$BASH" "${INSTALL_SCRIPT:-$CHECKOUT/agent/deploy/install.sh}" "$@"
    ) >"$INSTALL_OUT" 2>&1
    INSTALL_STATUS=$?
    return "$INSTALL_STATUS"
}

# Did the installer ask systemd to CHANGE anything? The read-only preflight
# probe (`systemctl --user show-environment`) is not a mutation and must not
# count as one, or every "changes nothing" assertion would fail on it.
systemctl_mutated() {
    grep -qE "(daemon-reload|enable|restart|stop|disable|start)" "$STUB_SYSTEMCTL_ARGV" 2>/dev/null
}

# Does any unattended-update job exist under this HOME — the Linux timer or
# oneshot, or a macOS updater plist (under any label)? The default install's
# contract (#394) is that the answer is no, and this is the one place that
# spells out what "no" covers.
updater_installed() {
    local home="$1"
    [ -e "$home/.config/systemd/user/solador-agent-update.timer" ] && return 0
    [ -e "$home/.config/systemd/user/solador-agent-update.service" ] && return 0
    ls "$home/Library/LaunchAgents"/*.update.plist >/dev/null 2>&1 && return 0
    return 1
}

# Did the installer ask a service manager to CHANGE anything about the
# updater? A default install and a no-flag re-run must not — no enable, no
# disable, no start, no bootstrap, no bootout, no kickstart — or "off by
# default" and "an earlier opt-in is left alone" are claims about source
# text rather than behaviour. Read-only queries (`is-enabled`, `print`) are
# how the summary line reports the job's state and are not changes.
manager_changed_updater() {
    # Captured, then matched — not `grep | grep -q`, which can take SIGPIPE
    # under pipefail (the flake install.sh's own comments record).
    local lines
    lines="$(grep -E '^--user (enable|disable|start|stop|restart)' "$STUB_SYSTEMCTL_ARGV" 2>/dev/null || true)"
    case "$lines" in *solador-agent-update*) return 0 ;; esac
    lines="$(grep -E '^(bootstrap|bootout|enable|disable|kickstart) ' "$STUB_LAUNCHCTL_ARGV" 2>/dev/null || true)"
    case "$lines" in *.update*) return 0 ;; esac
    return 1
}

# Was the installed agent ever asked to `update`? The default install, the
# no-flag re-run and the opt-in itself must never make an update check; the
# fixture binary records every argv it is invoked with.
agent_asked_to_update() {
    grep -qx 'update' "$AGENT_ARGV" 2>/dev/null
}

# Reset the argv logs the assertions read.
reset_argv_logs() {
    export STUB_CURL_ARGV="$TMP/curl-argv"
    export STUB_SYSTEMCTL_ARGV="$TMP/systemctl-argv"
    export STUB_LAUNCHCTL_ARGV="$TMP/launchctl-argv"
    : > "$STUB_CURL_ARGV"
    : > "$STUB_SYSTEMCTL_ARGV"
    : > "$STUB_LAUNCHCTL_ARGV"
    : > "$AGENT_ARGV"
}

# assert_untouched <name> <home>: no env file, no binary, no unit, no plist,
# and no service-manager call — the state a refusal must leave behind.
assert_untouched() {
    local name="$1" home="$2" problems=""
    [ -e "$home/.config/solador-agent.env" ] && problems="$problems env-file"
    [ -e "$home/.local/bin/solador-agent" ] && problems="$problems binary"
    [ -e "$home/.config/systemd/user/solador-agent.service" ] && problems="$problems unit"
    [ -e "$home/Library/LaunchAgents/app.solador.agent.plist" ] && problems="$problems plist"
    updater_installed "$home" && problems="$problems updater"
    systemctl_mutated && problems="$problems systemctl-was-called"
    grep -qE '^(bootstrap|bootout)' "$STUB_LAUNCHCTL_ARGV" 2>/dev/null && problems="$problems launchctl-was-called"
    if [ -z "$problems" ]; then
        pass "$name"
    else
        fail "$name" "installed state changed:$problems"
    fi
}

test_install_arguments() {
    local home="$TMP/home-args"
    mkdir -p "$home"
    INSTALL_PATH="$NOVERIFY_PATH"
    make_checkout "$SCRIPT_DIR/../release-signing-key.pub"
    reset_argv_logs

    # No parser existed before #392; the reason one exists now is that an
    # unknown argument is refused rather than ignored into a default install.
    run_install "$home" --frobnicate
    assert_eq "install.sh refuses an unknown argument" "2" "$?"
    assert_untouched "an unknown argument changes nothing" "$home"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "an unknown argument downloads nothing" "curl was invoked"
    else
        pass "an unknown argument downloads nothing"
    fi

    # #394's flag is accepted — and an unknown argument beside it is still
    # refused before anything happens, in either order. "Unknown options
    # fail before installation" has to hold for the run that opts in, or the
    # opt-in is what a typo turns into.
    reset_argv_logs
    run_install "$home" --enable-timer --frobnicate
    assert_eq "install.sh refuses an unknown argument beside --enable-timer" "2" "$?"
    assert_untouched "an unknown argument beside --enable-timer changes nothing" "$home"
    reset_argv_logs
    run_install "$home" --frobnicate --enable-timer
    assert_eq "install.sh refuses an unknown argument ahead of --enable-timer" "2" "$?"
    assert_untouched "an unknown argument ahead of --enable-timer changes nothing" "$home"
    assert_output_has "the unknown-argument refusal lists --enable-timer as a known one" \
        "$(cat "$INSTALL_OUT")" "[--enable-timer]"

    run_install "$home" --help
    assert_eq "install.sh --help exits 0" "0" "$?"
    assert_output_has "--help prints the usage" "$(cat "$INSTALL_OUT")" "Usage:"
    assert_untouched "--help changes nothing" "$home"
    INSTALL_PATH=""
}

test_install_preflight() {
    local home="$TMP/home-preflight"
    mkdir -p "$home"
    INSTALL_PATH="$NOVERIFY_PATH"
    make_checkout "$SCRIPT_DIR/../release-signing-key.pub"

    # Unsupported platforms refuse before anything is fetched.
    reset_argv_logs
    STUB_UNAME_S=FreeBSD STUB_UNAME_M=x86_64 run_install "$home"
    assert_eq "install.sh refuses an unsupported OS" "1" "$?"
    assert_untouched "an unsupported OS changes nothing" "$home"
    reset_argv_logs
    STUB_UNAME_S=Linux STUB_UNAME_M=riscv64 run_install "$home"
    assert_eq "install.sh refuses an unsupported architecture" "1" "$?"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "an unsupported platform downloads nothing" "curl was invoked"
    else
        pass "an unsupported platform downloads nothing"
    fi

    # The documented macOS floor is 11.0.
    reset_argv_logs
    STUB_UNAME_S=Darwin STUB_UNAME_M=arm64 STUB_SW_VERS=10.15.7 run_install "$home"
    assert_eq "install.sh refuses macOS below 11" "1" "$?"
    assert_output_has "the macOS refusal names the floor" "$(cat "$INSTALL_OUT")" "11.0"
    reset_argv_logs
    STUB_UNAME_S=Darwin STUB_UNAME_M=arm64 STUB_SW_VERS="" run_install "$home"
    assert_eq "install.sh refuses when the macOS version cannot be read" "1" "$?"
    assert_output_has "the unreadable-version refusal says so" "$(cat "$INSTALL_OUT")" "could not read the macOS version"
    reset_argv_logs
    STUB_UNAME_S=Darwin STUB_UNAME_M=arm64 STUB_SW_VERS="Version 15" run_install "$home"
    assert_eq "install.sh refuses a non-numeric macOS version" "1" "$?"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "a macOS version refusal downloads nothing" "curl was invoked"
    else
        pass "a macOS version refusal downloads nothing"
    fi

    # No login session: a LaunchAgent has nowhere to go, and launchctl's own
    # message for that names nothing.
    reset_argv_logs
    STUB_UNAME_S=Darwin STUB_UNAME_M=arm64 STUB_LAUNCHCTL_DOMAIN_EXIT=125 run_install "$home"
    assert_eq "install.sh refuses when the gui launchd domain is unavailable" "1" "$?"
    assert_output_has "the missing-session refusal says what to do" "$(cat "$INSTALL_OUT")" "login session"
    assert_untouched "a missing login session changes nothing" "$home"

    # No verifier: refuse, say how to get one, touch nothing. Never install it.
    reset_argv_logs
    INSTALL_PATH="$STUBS:$TOOLBIN_NOVERIFIER" run_install "$home"
    assert_eq "install.sh refuses without minisign" "1" "$?"
    assert_output_has "the missing-verifier refusal says how to install it" "$(cat "$INSTALL_OUT")" "brew install minisign"
    assert_untouched "a missing verifier changes nothing" "$home"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "a missing verifier downloads nothing" "curl was invoked"
    else
        pass "a missing verifier downloads nothing"
    fi

    # No public key in the checkout: an incomplete distribution cannot verify.
    rm "$CHECKOUT/agent/release-signing-key.pub"
    reset_argv_logs
    run_install "$home"
    assert_eq "install.sh refuses without the committed public key" "1" "$?"
    assert_untouched "a missing public key changes nothing" "$home"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "a missing public key refuses before any download" "curl was invoked"
    else
        pass "a missing public key refuses before any download"
    fi

    # --migrate-from-opt is a Linux/systemd concept.
    make_checkout "$SCRIPT_DIR/../release-signing-key.pub"
    reset_argv_logs
    STUB_UNAME_S=Darwin STUB_UNAME_M=arm64 run_install "$home" --migrate-from-opt
    assert_eq "install.sh refuses --migrate-from-opt on macOS" "2" "$?"

    # The launchd label is a filename and a rendered value; a test seam is
    # not a place for a path or a placeholder.
    local bad_label
    for bad_label in "../x" "x@BINARY@y" ".hidden" "a b"; do
        reset_argv_logs
        SOLADOR_AGENT_LAUNCHD_LABEL="$bad_label" STUB_UNAME_S=Darwin STUB_UNAME_M=arm64 run_install "$home"
        assert_eq "install.sh refuses SOLADOR_AGENT_LAUNCHD_LABEL [$bad_label]" "1" "$?"
        assert_untouched "a refused label changes nothing [$bad_label]" "$home"
    done

    # Linux: the user manager must be reachable (a real session, not sudo -u
    # or su), and finding that out at daemon-reload would be a half-install.
    reset_argv_logs
    STUB_SYSTEMCTL_USER_EXIT=1 run_install "$home"
    assert_eq "install.sh refuses when systemctl --user is unreachable" "1" "$?"
    assert_output_has "the unreachable-manager refusal says what to do" "$(cat "$INSTALL_OUT")" "XDG_RUNTIME_DIR"
    assert_untouched "an unreachable user manager changes nothing" "$home"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "an unreachable user manager downloads nothing" "curl was invoked"
    else
        pass "an unreachable user manager downloads nothing"
    fi
    INSTALL_PATH=""
}

test_install_release_resolution() {
    local home="$TMP/home-release" repo="https://github.com/Sassy-Dog/solador"
    mkdir -p "$home"
    INSTALL_PATH="$NOVERIFY_PATH"
    make_checkout "$SCRIPT_DIR/../release-signing-key.pub"
    rm -rf "$FIXTURES"

    # The latest published release predates #390 (as v2026.9.3 does): the
    # redirect resolves, the asset is missing. That is a hard failure naming
    # the tag — not a source build, not another version.
    reset_argv_logs
    STUB_CURL_REDIRECT="$repo/releases/tag/v2026.9.3" run_install "$home"
    assert_eq "install.sh fails when the latest release has no agent asset" "1" "$?"
    assert_output_has "the missing-asset failure names the tag" "$(cat "$INSTALL_OUT")" "v2026.9.3"
    assert_output_has "the missing-asset failure names the asset" "$(cat "$INSTALL_OUT")" \
        "solador-agent-2026.9.3-x86_64-unknown-linux-musl"
    assert_output_has "the missing-asset failure says there is no source fallback" "$(cat "$INSTALL_OUT")" \
        "does not fall back to building from source"
    assert_untouched "a missing asset changes nothing" "$home"
    if grep -q "cargo" "$INSTALL_OUT"; then
        fail "install.sh never mentions cargo" "it did"
    else
        pass "install.sh never mentions cargo"
    fi

    # The redirect is not a tag (a repo with no releases).
    reset_argv_logs
    STUB_CURL_REDIRECT="$repo/releases" run_install "$home"
    assert_eq "install.sh fails when no release can be resolved" "1" "$?"
    assert_untouched "an unresolvable release changes nothing" "$home"

    # A pin is honoured verbatim — and validated.
    reset_argv_logs
    SOLADOR_AGENT_RELEASE="main" run_install "$home"
    assert_eq "install.sh refuses a SOLADOR_AGENT_RELEASE that is not a tag" "1" "$?"
    assert_output_has "the refused pin is named as not a tag" "$(cat "$INSTALL_OUT")" "is not a release tag"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "a refused pin is never interpolated into a request" "curl was invoked"
    else
        pass "a refused pin is never interpolated into a request"
    fi
    reset_argv_logs
    SOLADOR_AGENT_RELEASE="v2026.9.8" run_install "$home"
    assert_eq "install.sh fails when the pinned release has no asset" "1" "$?"
    assert_file_has "the pinned tag is what was requested" "$STUB_CURL_ARGV" \
        "$repo/releases/download/v2026.9.8/solador-agent-2026.9.8-x86_64-unknown-linux-musl"
    # Asserted against a log that DID record a request, so it proves the pin
    # skipped discovery rather than that nothing ran.
    if grep -q "releases/latest" "$STUB_CURL_ARGV"; then
        fail "a pinned release does not consult /releases/latest" "it did"
    else
        pass "a pinned release does not consult /releases/latest"
    fi

    # An explicit bind and port reach the env file and the health probe: the
    # only paths a LAN/VPN host (no Tailscale) has. Only meaningful past the
    # download, which needs a signed fixture.
    if [ "$HAVE_MINISIGN" = true ]; then
        make_checkout "$TEST_KEY_DIR/a.pub"
        rm -rf "$FIXTURES"
        make_fixture 2026.9.8 x86_64-unknown-linux-musl "$TEST_KEY_DIR/a.key" >/dev/null
        rm -rf "$home"
        mkdir -p "$home"
        reset_argv_logs
        INSTALL_PATH="$STUBS:$TOOLBIN" STUB_CURL_BODY='{"status":"ok","version":"2026.9.8"}' INSTALL_STDIN="
" SOLADOR_AGENT_RELEASE="v2026.9.8" SOLADOR_AGENT_BIND="192.168.1.20" SOLADOR_AGENT_PORT="9000" run_install "$home"
        assert_eq "install.sh honours an explicit bind and port end to end" "0" "$?"
        assert_output_has "an explicit SOLADOR_AGENT_BIND is reported as such" "$(cat "$INSTALL_OUT")" \
            "Binding to 192.168.1.20 (SOLADOR_AGENT_BIND)"
        assert_file_has "the explicit bind reaches the env file" "$home/.config/solador-agent.env" "SOLADOR_AGENT_BIND=192.168.1.20"
        assert_file_has "the explicit port reaches the env file" "$home/.config/solador-agent.env" "SOLADOR_AGENT_PORT=9000"
        assert_file_has "the health probe dials the explicit bind and port" "$STUB_CURL_ARGV" "http://192.168.1.20:9000/v1/health"
        make_checkout "$SCRIPT_DIR/../release-signing-key.pub"
        rm -rf "$FIXTURES"
    else
        skip_needs_minisign "install.sh honours an explicit bind and port end to end"
    fi

    # No Tailscale, no SOLADOR_AGENT_BIND, no existing file: refused in
    # preflight, before a download (never mind a token prompt).
    rm -rf "$home"
    mkdir -p "$home"
    reset_argv_logs
    INSTALL_PATH="$STUBS_BYPASS:$TMP/stubs-no-tailscale:$TOOLBIN_NOVERIFIER" \
        SOLADOR_AGENT_RELEASE="v2026.9.8" run_install "$home"
    assert_eq "install.sh refuses without a bind address" "1" "$?"
    assert_output_has "the bind refusal names SOLADOR_AGENT_BIND" "$(cat "$INSTALL_OUT")" "SOLADOR_AGENT_BIND"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "the bind refusal downloads nothing" "curl was invoked"
    else
        pass "the bind refusal downloads nothing"
    fi
    INSTALL_PATH=""
}

test_install_signature_gate() {
    local home="$TMP/home-sig" marker="$TMP/executed-marker" f
    mkdir -p "$home"
    if [ "$HAVE_MINISIGN" != true ]; then
        skip_needs_minisign "install.sh rejects a tampered binary before executing it"
        skip_needs_minisign "install.sh rejects a binary signed by another key"
        skip_needs_minisign "install.sh rejects a binary with no signature asset"
        skip_needs_minisign "the tamper rejection is minisign's (bypassing the verifier lets it through)"
        skip_needs_minisign "install.sh from the real checkout uses agent/release-signing-key.pub"
        return
    fi
    make_checkout "$TEST_KEY_DIR/a.pub"
    export SOLADOR_AGENT_RELEASE="v2026.9.8"

    # Tampered: signed, then one byte changed. The fixture touches $marker if
    # it is ever executed — which a rejected candidate must never be.
    rm -rf "$FIXTURES"
    f="$(make_fixture 2026.9.8 x86_64-unknown-linux-musl "$TEST_KEY_DIR/a.key" "$marker")"
    printf '\n# tampered\n' >> "$f"
    rm -f "$marker"
    reset_argv_logs
    run_install "$home"
    assert_eq "install.sh rejects a tampered binary before executing it" "1" "$?"
    assert_output_has "the tamper rejection is named as one" "$(cat "$INSTALL_OUT")" "SIGNATURE VERIFICATION FAILED"
    if [ -e "$marker" ]; then
        fail "a rejected candidate is never executed" "the tampered fixture ran (--version was called)"
    else
        pass "a rejected candidate is never executed"
    fi
    assert_untouched "a rejected signature changes nothing" "$home"
    if ls "$home/.cache"/solador-agent-install.* >/dev/null 2>&1; then
        fail "staging is cleaned up after a rejection" "a staging directory was left under $home/.cache"
    else
        pass "staging is cleaned up after a rejection"
    fi

    # THE PROOF THAT THE CASE ABOVE PROVES SOMETHING. Same tampered fixture,
    # same everything, but the verifier on PATH is a stub that accepts all.
    # If the rejection above were coming from anywhere other than minisign,
    # this run would be rejected too. It is not: the candidate gets executed.
    rm -f "$marker"
    reset_argv_logs
    INSTALL_PATH="$STUBS_BYPASS:$STUBS:$TOOLBIN" run_install "$home"
    if [ -e "$marker" ]; then
        pass "the tamper rejection is minisign's (bypassing the verifier lets it through)"
    else
        fail "the tamper rejection is minisign's (bypassing the verifier lets it through)" \
            "with an accept-all verifier the tampered fixture was still not executed," \
            "so the rejection above was not attributable to signature verification" \
            "$(head -n 20 "$INSTALL_OUT")"
    fi
    # That run installed a tampered binary under a bypassed verifier — into a
    # throwaway HOME. Discard it so the cases below start clean.
    rm -rf "$home"
    mkdir -p "$home"

    # Signed by a key that is not the one this checkout ships.
    rm -rf "$FIXTURES"
    make_fixture 2026.9.8 x86_64-unknown-linux-musl "$TEST_KEY_DIR/b.key" "$marker" >/dev/null
    rm -f "$marker"
    reset_argv_logs
    run_install "$home"
    assert_eq "install.sh rejects a binary signed by another key" "1" "$?"
    if [ -e "$marker" ]; then
        fail "a wrongly-signed candidate is never executed" "the fixture ran"
    else
        pass "a wrongly-signed candidate is never executed"
    fi
    assert_untouched "a wrong key changes nothing" "$home"

    # No .minisig published beside the binary.
    rm -rf "$FIXTURES"
    make_fixture 2026.9.8 x86_64-unknown-linux-musl - "$marker" >/dev/null
    rm -f "$marker"
    reset_argv_logs
    run_install "$home"
    assert_eq "install.sh rejects a binary with no signature asset" "1" "$?"
    assert_output_has "the missing signature is named" "$(cat "$INSTALL_OUT")" ".minisig"
    if [ -e "$marker" ]; then
        fail "an unsigned candidate is never executed" "the fixture ran"
    else
        pass "an unsigned candidate is never executed"
    fi
    assert_untouched "a missing signature changes nothing" "$home"

    # The REAL checkout, the real key: a fixture signed by the throwaway key
    # must be rejected by the installer as it sits in this repository. This is
    # what proves the production path reads agent/release-signing-key.pub and
    # not something a test laid down.
    rm -rf "$FIXTURES"
    make_fixture 2026.9.8 x86_64-unknown-linux-musl "$TEST_KEY_DIR/a.key" "$marker" >/dev/null
    rm -f "$marker"
    reset_argv_logs
    INSTALL_SCRIPT="$SCRIPT_DIR/install.sh" run_install "$home"
    assert_eq "install.sh from the real checkout uses agent/release-signing-key.pub" "1" "$?"
    assert_output_has "the real-key rejection names the committed key" "$(cat "$INSTALL_OUT")" "release-signing-key.pub"
    if [ -e "$marker" ]; then
        fail "the real checkout never executes a candidate the committed key rejects" "the fixture ran"
    else
        pass "the real checkout never executes a candidate the committed key rejects"
    fi

    unset SOLADOR_AGENT_RELEASE
}

test_install_linux_flow() {
    local home="$TMP/home-linux" token env_file unit bin out
    if [ "$HAVE_MINISIGN" != true ]; then
        skip_needs_minisign "install.sh: fresh Linux install"
        skip_needs_minisign "install.sh: repeated install"
        skip_needs_minisign "install.sh: pre-rename handover"
        skip_needs_minisign "install.sh: /opt migration gate"
        return
    fi
    make_checkout "$TEST_KEY_DIR/a.pub"
    rm -rf "$FIXTURES" "$home"
    mkdir -p "$home"
    make_fixture 2026.9.8 x86_64-unknown-linux-musl "$TEST_KEY_DIR/a.key" >/dev/null
    export SOLADOR_AGENT_RELEASE="v2026.9.8"
    export STUB_CURL_BODY='{"status":"ok","hostname":"h","version":"2026.9.8"}'
    export STUB_TAILSCALE_IP="100.64.0.9"

    env_file="$home/.config/solador-agent.env"
    unit="$home/.config/systemd/user/solador-agent.service"
    bin="$home/.local/bin/solador-agent"

    # ---- fresh install: token typed at the hidden prompt ----
    token="tok-MUST-NOT-BE-PRINTED-7a1e"
    reset_argv_logs
    INSTALL_STDIN="$token
" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: fresh Linux install" "0" "$INSTALL_STATUS"
    [ -x "$bin" ] && pass "the binary lands, executable, at ~/.local/bin/solador-agent" \
        || fail "the binary lands, executable, at ~/.local/bin/solador-agent" "$out"
    assert_file_has "the env file carries the typed token" "$env_file" "SOLADOR_AGENT_TOKEN=$token"
    assert_file_has "the env file carries the detected tailnet bind" "$env_file" "SOLADOR_AGENT_BIND=100.64.0.9"
    assert_file_has "the env file carries the default port" "$env_file" "SOLADOR_AGENT_PORT=7878"
    assert_eq "the env file is mode 0600" "600" "$(file_mode "$env_file")"
    case "$out" in
        *"$token"*) fail "install.sh never prints the full token" "the token appeared in its output" ;;
        *) pass "install.sh never prints the full token" ;;
    esac
    assert_output_has "install.sh reports the token's last four characters only" "$out" "...7a1e"
    assert_file_has "the unit's ExecStart is the actual installed path" "$unit" "ExecStart=$bin"
    assert_file_has "the unit reads the env file from %h" "$unit" 'EnvironmentFile=%h/.config/solador-agent.env'
    assert_file_has "the unit is a template no longer" "$unit" "ExecStart=/"
    if grep -q '@SOLADOR_AGENT_BIN@' "$unit"; then
        fail "the placeholder was rendered" "@SOLADOR_AGENT_BIN@ survived into the installed unit"
    else
        pass "the placeholder was rendered"
    fi
    assert_file_has "systemd is reloaded" "$STUB_SYSTEMCTL_ARGV" "--user daemon-reload"
    assert_file_has "the unit is enabled" "$STUB_SYSTEMCTL_ARGV" "--user enable solador-agent"
    assert_file_has "the unit is restarted, not merely started" "$STUB_SYSTEMCTL_ARGV" "--user restart solador-agent"
    assert_file_has "lingering is enabled best-effort" "$STUB_SYSTEMCTL_ARGV" "loginctl enable-linger"
    assert_curl_authenticated_via_stdin "install.sh's health probe" "$token"
    assert_output_has "install.sh reports the verified version" "$out" "2026.9.8 installed and serving"
    if ls "$home/.cache"/solador-agent-install.* >/dev/null 2>&1; then
        fail "staging is cleaned up after success" "a staging directory was left under $home/.cache"
    else
        pass "staging is cleaned up after success"
    fi

    # ---- fresh install, Enter at the prompt: a token is generated ----
    rm -rf "$home"
    mkdir -p "$home"
    reset_argv_logs
    INSTALL_STDIN="
" run_install "$home"
    assert_eq "install.sh generates a token when the prompt is left empty" "0" "$?"
    token="$(grep -E '^SOLADOR_AGENT_TOKEN=' "$env_file" | cut -d= -f2-)"
    if [ "${#token}" -ge 32 ]; then
        pass "the generated token is at least 32 characters"
    else
        fail "the generated token is at least 32 characters" "got ${#token} characters"
    fi

    # ---- repeated install: token reused, binary swapped by rename ----
    # The live binary is hard-linked to a witness. An in-place overwrite
    # writes through the link and changes the witness; a rename over the path
    # leaves the witness holding the old bytes. That is the ETXTBSY contract.
    rm -f "$TMP/witness"
    ln "$bin" "$TMP/witness"
    old_sum="$(cat "$TMP/witness")"
    rm -rf "$FIXTURES"
    make_fixture 2026.9.9 x86_64-unknown-linux-musl "$TEST_KEY_DIR/a.key" >/dev/null
    export SOLADOR_AGENT_RELEASE="v2026.9.9"
    export STUB_CURL_BODY='{"status":"ok","hostname":"h","version":"2026.9.9"}'
    reset_argv_logs
    INSTALL_STDIN="" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: repeated install" "0" "$INSTALL_STATUS"
    assert_output_has "a re-run reuses the stored token without prompting" "$out" "Reusing existing token"
    assert_file_has "the reused token is the one stored" "$env_file" "SOLADOR_AGENT_TOKEN=$token"
    assert_eq "the running binary was not overwritten in place" "$old_sum" "$(cat "$TMP/witness")"
    assert_eq "the displaced binary is kept as .prev" "$old_sum" "$(cat "$bin.prev")"
    if [ -e "$bin.new" ]; then
        fail "no .new is left behind" "$bin.new exists"
    else
        pass "no .new is left behind"
    fi
    assert_eq "the new binary is the one at the live path" "2026.9.9" "$("$bin" --version)"

    # ---- the same bytes again: .prev is the LAST-GOOD anchor and stays ----
    # This is the fix-and-retry the failure text recommends. If the re-run
    # copied the live (possibly bad) binary over .prev, the anchor a rollback
    # restores would be the thing being rolled back.
    reset_argv_logs
    INSTALL_STDIN="" run_install "$home"
    assert_eq "install.sh: re-run with identical bytes" "0" "$?"
    assert_eq "a re-run with identical bytes leaves .prev as the last-good binary" \
        "$old_sum" "$(cat "$bin.prev")"

    # ---- a re-run must not move backwards on the unpinned path ----
    # 2026.9.9 is installed. An unpinned run whose "latest" resolves to
    # 2026.9.8 (an intercepting proxy steering the unsigned redirect, say) is
    # refused; the same downgrade with SOLADOR_AGENT_RELEASE pinned is the
    # operator's explicit choice and goes through.
    rm -rf "$FIXTURES"
    make_fixture 2026.9.8 x86_64-unknown-linux-musl "$TEST_KEY_DIR/a.key" >/dev/null
    export STUB_CURL_REDIRECT="https://github.com/Sassy-Dog/solador/releases/tag/v2026.9.8"
    unset SOLADOR_AGENT_RELEASE
    reset_argv_logs
    INSTALL_STDIN="" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "an unpinned re-run refuses to downgrade" "1" "$INSTALL_STATUS"
    assert_output_has "the downgrade refusal names both versions" "$out" "installed agent is 2026.9.9 and the latest published release is 2026.9.8"
    assert_output_has "the downgrade refusal names the pin that allows it" "$out" "SOLADOR_AGENT_RELEASE=v2026.9.8"
    assert_eq "a refused downgrade leaves the live binary alone" "2026.9.9" "$("$bin" --version)"
    if systemctl_mutated; then
        fail "a refused downgrade never reaches the service manager" "systemctl was called"
    else
        pass "a refused downgrade never reaches the service manager"
    fi
    reset_argv_logs
    export STUB_CURL_BODY='{"status":"ok","hostname":"h","version":"2026.9.8"}'
    SOLADOR_AGENT_RELEASE="v2026.9.8" INSTALL_STDIN="" run_install "$home"
    assert_eq "a pinned re-run may downgrade" "0" "$?"
    assert_eq "the pinned downgrade installed the older binary" "2026.9.8" "$("$bin" --version)"
    unset STUB_CURL_REDIRECT
    # Restore the state the cases below expect.
    rm -rf "$FIXTURES"
    make_fixture 2026.9.9 x86_64-unknown-linux-musl "$TEST_KEY_DIR/a.key" >/dev/null
    export SOLADOR_AGENT_RELEASE="v2026.9.9"
    export STUB_CURL_BODY='{"status":"ok","hostname":"h","version":"2026.9.9"}'
    reset_argv_logs
    INSTALL_STDIN="" run_install "$home"
    assert_eq "install.sh: back on 2026.9.9 for the cases below" "0" "$?"

    # ---- version mismatch: the tag names one version, the binary another ----
    rm -rf "$FIXTURES"
    make_fixture 2026.9.9 x86_64-unknown-linux-musl "$TEST_KEY_DIR/a.key" >/dev/null
    export SOLADOR_AGENT_RELEASE="v2026.9.10"
    # The asset name must match the tag for the download to be found at all,
    # so rename the fixture to the tag's name: same bytes (signed as 2026.9.9),
    # published under the wrong tag.
    mv "$FIXTURES/solador-agent-2026.9.9-x86_64-unknown-linux-musl" \
       "$FIXTURES/solador-agent-2026.9.10-x86_64-unknown-linux-musl"
    mv "$FIXTURES/solador-agent-2026.9.9-x86_64-unknown-linux-musl.minisig" \
       "$FIXTURES/solador-agent-2026.9.10-x86_64-unknown-linux-musl.minisig"
    reset_argv_logs
    run_install "$home"
    assert_eq "install.sh refuses a binary whose version is not its release's" "1" "$?"
    assert_output_has "the version mismatch names both numbers" "$(cat "$INSTALL_OUT")" \
        "reports version 2026.9.9 but was published under v2026.9.10"
    assert_eq "a refused install leaves the live binary alone" "2026.9.9" "$("$bin" --version)"
    if systemctl_mutated; then
        fail "a version mismatch never reaches the service manager" "systemctl was called"
    else
        pass "a version mismatch never reaches the service manager"
    fi

    # ---- a binary that cannot name itself: fail closed, before any restart ----
    rm -rf "$FIXTURES"
    f="$FIXTURES/solador-agent-2026.9.10-x86_64-unknown-linux-musl"
    mkdir -p "$FIXTURES"
    printf '#!/bin/sh\nexit 1\n' > "$f"
    sign_fixture "$f" "$TEST_KEY_DIR/a.key"
    reset_argv_logs
    run_install "$home"
    assert_eq "install.sh aborts when the verified binary carries no version" "1" "$?"
    if systemctl_mutated; then
        fail "an unversioned binary never reaches the service manager" "systemctl was called"
    else
        pass "an unversioned binary never reaches the service manager"
    fi

    # ---- the service came up serving the wrong version: non-zero ----
    rm -rf "$FIXTURES"
    make_fixture 2026.9.10 x86_64-unknown-linux-musl "$TEST_KEY_DIR/a.key" >/dev/null
    export STUB_CURL_BODY='{"status":"ok","hostname":"h","version":"2026.9.9"}'
    reset_argv_logs
    run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh fails when /v1/health serves a version other than the installed one" "1" "$INSTALL_STATUS"
    assert_output_has "the served/installed mismatch is named" "$out" "VERSION MISMATCH"
    assert_output_has "the failure names the previous binary for rollback" "$out" "$bin.prev"
    export STUB_CURL_BODY=""
    reset_argv_logs
    run_install "$home"
    assert_eq "install.sh fails when /v1/health never answers" "1" "$?"

    # ---- pre-rename handover ----
    rm -rf "$home"
    mkdir -p "$home/.config/systemd/user"
    printf '[Service]\nExecStart=/opt/devcanopy-agent/devcanopy-agent\n' > "$home/.config/systemd/user/devcanopy-agent.service"
    printf 'DEVCANOPY_AGENT_TOKEN=legacy-tok-MUST-NOT-BE-PRINTED\nDEVCANOPY_AGENT_BIND=0.0.0.0\n' > "$home/.config/devcanopy-agent.env"
    rm -rf "$FIXTURES"
    make_fixture 2026.9.10 x86_64-unknown-linux-musl "$TEST_KEY_DIR/a.key" >/dev/null
    export STUB_CURL_BODY='{"status":"ok","hostname":"h","version":"2026.9.10"}'
    reset_argv_logs
    INSTALL_STDIN="" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: pre-rename handover" "0" "$INSTALL_STATUS"
    assert_file_has "the legacy token is carried across" "$env_file" "SOLADOR_AGENT_TOKEN=legacy-tok-MUST-NOT-BE-PRINTED"
    assert_file_has "the legacy bind is carried across" "$env_file" "SOLADOR_AGENT_BIND=0.0.0.0"
    case "$out" in
        *"legacy-tok-MUST-NOT-BE-PRINTED"*) fail "the carried-over token is never printed" "it appeared in the output" ;;
        *) pass "the carried-over token is never printed" ;;
    esac
    # Order: the legacy unit is stopped BEFORE the new one is restarted, or
    # the new one crash-loops on EADDRINUSE while the old one keeps serving.
    local stop_line restart_line
    stop_line="$(grep -nF -- '--user stop devcanopy-agent' "$STUB_SYSTEMCTL_ARGV" | head -n1 | cut -d: -f1)"
    restart_line="$(grep -nF -- '--user restart solador-agent' "$STUB_SYSTEMCTL_ARGV" | head -n1 | cut -d: -f1)"
    if [ -n "$stop_line" ] && [ -n "$restart_line" ] && [ "$stop_line" -lt "$restart_line" ]; then
        pass "the legacy unit is stopped before the new one is restarted"
    else
        fail "the legacy unit is stopped before the new one is restarted" \
            "stop at line ${stop_line:-<never>}, restart at line ${restart_line:-<never>}"
    fi
    assert_file_has "the legacy unit is disabled" "$STUB_SYSTEMCTL_ARGV" "--user disable devcanopy-agent"
    [ -f "$home/.config/systemd/user/devcanopy-agent.service" ] \
        && pass "the legacy unit file is left for rollback" \
        || fail "the legacy unit file is left for rollback" "it was removed"
    [ -f "$home/.config/devcanopy-agent.env" ] \
        && pass "the legacy env file is left for rollback" \
        || fail "the legacy env file is left for rollback" "it was removed"

    # ---- /opt migration gate ----
    # An existing unit that starts /opt/…: refuse, before any state change,
    # and print the explicit path. Then take that path.
    rm -rf "$home"
    mkdir -p "$home/.config/systemd/user" "$home/.config"
    # The pre-#392 layout, with the root-owned binary stood in by a file the
    # test can read — what the migration copies in as .prev.
    local opt_bin="$TMP/fake-opt/solador-agent/solador-agent"
    mkdir -p "$(dirname "$opt_bin")"
    printf '#!/bin/sh\necho opt-2026.8.1\n' > "$opt_bin"
    chmod +x "$opt_bin"
    printf '[Service]\nExecStart=%s\nEnvironmentFile=%%h/.config/solador-agent.env\n' "$opt_bin" > "$unit"
    # A fourth, operator-added key: the migration must carry it through.
    printf 'SOLADOR_AGENT_TOKEN=opt-tok-MUST-NOT-BE-PRINTED\nSOLADOR_AGENT_BIND=100.64.0.3\nSOLADOR_AGENT_PORT=7979\nRUST_LOG=debug\n' > "$env_file"
    chmod 600 "$env_file"
    local env_before
    env_before="$(cat "$env_file")"
    reset_argv_logs
    run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: /opt migration gate" "1" "$INSTALL_STATUS"
    assert_output_has "the /opt refusal names the existing path" "$out" "$opt_bin"
    assert_output_has "the /opt refusal names the flag" "$out" "--migrate-from-opt"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "the /opt refusal downloads nothing" "curl was invoked"
    else
        pass "the /opt refusal downloads nothing"
    fi
    if systemctl_mutated; then
        fail "the /opt refusal touches no service" "systemctl was called"
    else
        pass "the /opt refusal touches no service"
    fi
    assert_eq "the /opt refusal leaves the env file byte-for-byte" "$env_before" "$(cat "$env_file")"
    assert_file_has "the /opt refusal leaves the unit pointing at /opt" "$unit" "ExecStart=$opt_bin"
    [ -e "$bin" ] && fail "the /opt refusal installs no binary" "$bin exists" || pass "the /opt refusal installs no binary"

    reset_argv_logs
    run_install "$home" --migrate-from-opt
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh --migrate-from-opt re-points an /opt install" "0" "$INSTALL_STATUS"
    assert_eq "the migration preserves the env file byte-for-byte" "$env_before" "$(cat "$env_file")"
    assert_file_has "the migration re-points ExecStart at the user-owned binary" "$unit" "ExecStart=$bin"
    assert_file_has "the migration carries an operator-added key through" "$env_file" "RUST_LOG=debug"
    assert_file_has "the migration keeps the displaced unit as .prev" "$unit.prev" "ExecStart=$opt_bin"
    assert_eq "the migration seeds .prev from the /opt binary, for rollback" \
        "$(cat "$opt_bin")" "$(cat "$bin.prev" 2>/dev/null)"
    [ -x "$bin" ] && pass "the migration installs the verified binary user-owned" \
        || fail "the migration installs the verified binary user-owned" "$out"
    assert_file_has "the migration restarts the service" "$STUB_SYSTEMCTL_ARGV" "--user restart solador-agent"
    assert_file_has "the migration probes the existing bind and port" "$STUB_CURL_ARGV" "http://100.64.0.3:7979/v1/health"
    if grep -q "sudo" "$STUB_SYSTEMCTL_ARGV" "$STUB_CURL_ARGV"; then
        fail "the migration uses no sudo" "sudo appeared"
    else
        pass "the migration uses no sudo"
    fi
    case "$out" in
        *"opt-tok-MUST-NOT-BE-PRINTED"*) fail "the migration never prints the token" "it appeared" ;;
        *) pass "the migration never prints the token" ;;
    esac

    # ---- a HOME with a space renders a quoted ExecStart ----
    local spaced="$TMP/home with space"
    rm -rf "$spaced"
    mkdir -p "$spaced"
    reset_argv_logs
    INSTALL_STDIN="
" run_install "$spaced"
    assert_eq "install.sh handles a HOME with a space (Linux)" "0" "$?"
    assert_file_has "a spaced path is double-quoted in ExecStart" \
        "$spaced/.config/systemd/user/solador-agent.service" \
        "ExecStart=\"$spaced/.local/bin/solador-agent\""
    # And the re-run reads that quoted ExecStart back as its own destination —
    # not as "some other path, migrate from /opt".
    reset_argv_logs
    INSTALL_STDIN="" run_install "$spaced"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh re-runs over a HOME with a space (Linux)" "0" "$INSTALL_STATUS"
    assert_output_has "the spaced re-run reuses the token" "$out" "Reusing existing token"

    # A HOME neither service format can carry is refused before anything.
    local pct="$TMP/home%pct"
    mkdir -p "$pct"
    reset_argv_logs
    INSTALL_STDIN="" run_install "$pct"
    assert_eq "install.sh refuses a HOME with a % in it" "1" "$?"
    assert_untouched "a refused HOME changes nothing" "$pct"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "a refused HOME downloads nothing" "curl was invoked"
    else
        pass "a refused HOME downloads nothing"
    fi

    unset SOLADOR_AGENT_RELEASE STUB_CURL_BODY STUB_TAILSCALE_IP
}

test_install_macos_flow() {
    local home="$TMP/home mac & co" plist launcher env_file bin out
    if [ "$HAVE_MINISIGN" != true ]; then
        skip_needs_minisign "install.sh: macOS install (stubbed launchctl)"
        return
    fi
    make_checkout "$TEST_KEY_DIR/a.pub"
    rm -rf "$FIXTURES" "$home"
    mkdir -p "$home"
    make_fixture 2026.9.8 aarch64-apple-darwin "$TEST_KEY_DIR/a.key" >/dev/null
    export SOLADOR_AGENT_RELEASE="v2026.9.8"
    export STUB_CURL_BODY='{"status":"ok","hostname":"mac","version":"2026.9.8"}'
    export STUB_UNAME_S=Darwin STUB_UNAME_M=arm64 STUB_SW_VERS=15.6

    env_file="$home/.config/solador-agent.env"
    plist="$home/Library/LaunchAgents/app.solador.agent.plist"
    launcher="$home/.local/bin/solador-agent-launchd"
    bin="$home/.local/bin/solador-agent"

    reset_argv_logs
    # Under umask 002, deliberately: the env file's 0600 and the plist's 0644
    # have to be explicit, not inherited from whatever umask the operator has.
    INSTALL_UMASK=002 INSTALL_STDIN="mac-tok-MUST-NOT-BE-PRINTED
" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: macOS install (stubbed launchctl)" "0" "$INSTALL_STATUS"
    assert_file_has "the macOS install picked the Apple Silicon asset" "$STUB_CURL_ARGV" \
        "solador-agent-2026.9.8-aarch64-apple-darwin"
    # The download itself is hardened: -f (a 404 is a failure, not a saved
    # error page) and --proto '=https' (no redirect off HTTPS).
    if grep -E '^-fsSL --proto =https .* -o ' "$STUB_CURL_ARGV" | grep -q 'aarch64-apple-darwin$'; then
        pass "the download passes -f and --proto '=https'"
    else
        fail "the download passes -f and --proto '=https'" "$(grep -- ' -o ' "$STUB_CURL_ARGV" | head -n2)"
    fi
    [ -x "$bin" ] && pass "macOS: the binary lands at ~/.local/bin/solador-agent" \
        || fail "macOS: the binary lands at ~/.local/bin/solador-agent" "$out"
    [ -x "$launcher" ] && pass "macOS: the launcher is installed beside it" \
        || fail "macOS: the launcher is installed beside it" "$out"
    if [ -L "$launcher" ]; then
        fail "macOS: the launcher is a copy, not a link into the checkout" "$launcher is a symlink"
    else
        assert_eq "macOS: the launcher is a copy, not a link into the checkout" \
            "$(cat "$SCRIPT_DIR/run-agent.sh")" "$(cat "$launcher")"
    fi
    assert_eq "macOS: the env file is mode 0600 even under umask 002" "600" "$(file_mode "$env_file")"
    if [ -e "$env_file.new" ]; then
        fail "macOS: no env file .new is left behind" "$env_file.new exists"
    else
        pass "macOS: no env file .new is left behind"
    fi
    [ -f "$plist" ] && pass "macOS: the LaunchAgent plist is written" \
        || fail "macOS: the LaunchAgent plist is written" "$out"
    assert_file_has "the plist carries the label" "$plist" "<string>app.solador.agent</string>"
    assert_file_has "the plist names the launcher, XML-escaped" "$plist" \
        "<string>$(xml_escape "$launcher")</string>"
    assert_file_has "the plist names the binary, XML-escaped" "$plist" \
        "<string>$(xml_escape "$bin")</string>"
    assert_file_has "the plist names the env file, XML-escaped" "$plist" \
        "<string>$(xml_escape "$env_file")</string>"
    assert_file_has "an ampersand in HOME is escaped" "$plist" "&amp;"
    # PATH is load-bearing: launchd's compiled-in PATH has none of
    # /opt/homebrew/bin, /usr/local/bin or /opt/podman/bin, and an agent that
    # cannot find docker/tart/podman serves an empty container list forever.
    assert_file_has "the plist sets PATH for the container runtimes" "$plist" "<key>PATH</key>"
    assert_file_has "the PATH covers Homebrew" "$plist" "/opt/homebrew/bin"
    assert_file_has "the PATH covers /usr/local/bin (Docker)" "$plist" "/usr/local/bin"
    assert_file_has "the PATH covers /opt/podman/bin" "$plist" "/opt/podman/bin"
    assert_eq "macOS: the plist is mode 0644 even under umask 002" "644" "$(file_mode "$plist")"
    # The plist parsed as a plist, not merely grepped — on Linux the plutil
    # stub cannot lint it, so plistlib stands in where python3 exists.
    if command -v python3 >/dev/null 2>&1; then
        local parsed
        parsed="$(python3 - "$plist" "$launcher" "$bin" "$env_file" "$home/Library/Logs/solador-agent.log" <<'PY' 2>&1
import plistlib, sys
with open(sys.argv[1], "rb") as f:
    d = plistlib.load(f)
ok = (
    d.get("Label") == "app.solador.agent"
    and d.get("ProgramArguments") == [sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5]]
    and d.get("StandardOutPath") == sys.argv[5]
    and d.get("StandardErrorPath") == sys.argv[5]
    and d.get("RunAtLoad") is True
    and d.get("KeepAlive") is True
    and "PATH" in d.get("EnvironmentVariables", {})
    and "ProcessType" not in d
)
print("ok" if ok else "mismatch: " + repr(d))
PY
)"
        assert_eq "the rendered plist parses and carries the expected keys" "ok" "$parsed"
    else
        skip "the rendered plist parses and carries the expected keys" "python3 not on PATH"
    fi
    if grep -q "mac-tok-MUST-NOT-BE-PRINTED" "$plist"; then
        fail "the token is not in the plist" "it is"
    else
        pass "the token is not in the plist"
    fi
    if grep -q '@[A-Z_]*@' "$plist"; then
        fail "every plist placeholder was rendered" "$(grep -o '@[A-Z_]*@' "$plist" | head -n3 | tr '\n' ' ')"
    else
        pass "every plist placeholder was rendered"
    fi
    case "$out" in
        *"mac-tok-MUST-NOT-BE-PRINTED"*) fail "macOS: install.sh never prints the token" "it did" ;;
        *) pass "macOS: install.sh never prints the token" ;;
    esac
    # Anchored: `print gui/<uid>` exactly, not the `print gui/<uid>/<label>`
    # loaded-check that would satisfy a substring match on its own.
    if grep -qx "print gui/$(id -u)" "$STUB_LAUNCHCTL_ARGV"; then
        pass "macOS: the login-session check ran"
    else
        fail "macOS: the login-session check ran" "no bare 'print gui/$(id -u)' on launchctl's argv"
    fi
    assert_file_has "macOS: the plist is bootstrapped into gui/<uid>" "$STUB_LAUNCHCTL_ARGV" \
        "bootstrap gui/$(id -u) $plist"
    if grep -q "^bootout" "$STUB_LAUNCHCTL_ARGV"; then
        fail "macOS: a fresh install does not bootout a service that is not loaded" "it did"
    else
        pass "macOS: a fresh install does not bootout a service that is not loaded"
    fi
    if systemctl_mutated; then
        fail "macOS: systemctl is never called" "it was"
    else
        pass "macOS: systemctl is never called"
    fi
    assert_output_has "macOS: the summary says it is a login-session service" "$out" "does not run before anyone logs in"

    # Re-run over a loaded service: bootout first, then bootstrap.
    reset_argv_logs
    STUB_LAUNCHCTL_LOADED_EXIT=0 INSTALL_STDIN="" run_install "$home"
    assert_eq "macOS: a re-run over a loaded service succeeds" "0" "$?"
    local bootout_line bootstrap_line
    bootout_line="$(grep -n "^bootout gui/$(id -u)/app.solador.agent" "$STUB_LAUNCHCTL_ARGV" | head -n1 | cut -d: -f1)"
    bootstrap_line="$(grep -n "^bootstrap gui/$(id -u) " "$STUB_LAUNCHCTL_ARGV" | head -n1 | cut -d: -f1)"
    if [ -n "$bootout_line" ] && [ -n "$bootstrap_line" ] && [ "$bootout_line" -lt "$bootstrap_line" ]; then
        pass "macOS: a loaded service is booted out before the new plist is bootstrapped"
    else
        fail "macOS: a loaded service is booted out before the new plist is bootstrapped" \
            "bootout at line ${bootout_line:-<never>}, bootstrap at line ${bootstrap_line:-<never>}"
    fi
    # Same bytes as the first install, so no .prev is minted — a .prev that
    # is a copy of the live binary is not a rollback anchor.
    if [ -e "$bin.prev" ]; then
        fail "macOS: a re-run with identical bytes mints no .prev" "$bin.prev exists"
    else
        pass "macOS: a re-run with identical bytes mints no .prev"
    fi
    assert_output_has "macOS: the identical re-run says so" "$(cat "$INSTALL_OUT")" "already these bytes"

    # bootstrap failing is an install failure.
    reset_argv_logs
    STUB_LAUNCHCTL_BOOTSTRAP_EXIT=5 INSTALL_STDIN="" run_install "$home"
    assert_eq "macOS: a failed bootstrap is a failed install" "1" "$?"
    assert_output_has "macOS: launchd's own bootstrap error is relayed" "$(cat "$INSTALL_OUT")" \
        "Bootstrap failed: 5: Input/output error (stubbed)"
    # Retried a bounded number of times, and NOT once more for the error text:
    # a sixth attempt that succeeded would leave the service running behind an
    # exit 1.
    assert_eq "macOS: bootstrap is attempted exactly five times before giving up" \
        "5" "$(grep -c "^bootstrap gui/$(id -u) " "$STUB_LAUNCHCTL_ARGV")"
    if grep -q "^enable " "$STUB_LAUNCHCTL_ARGV"; then
        fail "macOS: a service launchd does not report disabled is not 'enable'd" \
            "launchctl enable was called, which writes a permanent override record"
    else
        pass "macOS: a service launchd does not report disabled is not 'enable'd"
    fi

    # The legacy `launchctl unload -w` leaves a disabled flag that fails every
    # bootstrap with "Service is disabled"; when launchd reports it, clear it.
    reset_argv_logs
    STUB_LAUNCHCTL_DISABLED_LABEL=app.solador.agent INSTALL_STDIN="" run_install "$home"
    assert_eq "macOS: a re-run over a disabled service succeeds" "0" "$?"
    assert_file_has "macOS: a previously disabled service is re-enabled before bootstrap" \
        "$STUB_LAUNCHCTL_ARGV" "enable gui/$(id -u)/app.solador.agent"
    local enable_line bootstrap_line2
    enable_line="$(grep -n "^enable " "$STUB_LAUNCHCTL_ARGV" | head -n1 | cut -d: -f1)"
    bootstrap_line2="$(grep -n "^bootstrap " "$STUB_LAUNCHCTL_ARGV" | head -n1 | cut -d: -f1)"
    if [ -n "$enable_line" ] && [ -n "$bootstrap_line2" ] && [ "$enable_line" -lt "$bootstrap_line2" ]; then
        pass "macOS: the re-enable precedes the bootstrap"
    else
        fail "macOS: the re-enable precedes the bootstrap" \
            "enable at line ${enable_line:-<never>}, bootstrap at line ${bootstrap_line2:-<never>}"
    fi
    # Big Sur / Monterey spell the same state `=> true`.
    reset_argv_logs
    STUB_LAUNCHCTL_DISABLED_LABEL=app.solador.agent STUB_LAUNCHCTL_DISABLED_WORD=true INSTALL_STDIN="" run_install "$home"
    assert_eq "macOS: a re-run over a service disabled in the Big Sur wording succeeds" "0" "$?"
    assert_file_has "macOS: the Big Sur '=> true' wording is recognised as disabled" \
        "$STUB_LAUNCHCTL_ARGV" "enable gui/$(id -u)/app.solador.agent"
    # A label that differs only where the ERE would treat `.` as any character
    # must NOT trigger the re-enable of ours.
    reset_argv_logs
    STUB_LAUNCHCTL_DISABLED_LABEL=app-solador-agent INSTALL_STDIN="" run_install "$home"
    assert_eq "macOS: a re-run beside a look-alike disabled label succeeds" "0" "$?"
    if grep -q "^enable " "$STUB_LAUNCHCTL_ARGV"; then
        fail "macOS: a look-alike disabled label does not re-enable ours" "launchctl enable was called"
    else
        pass "macOS: a look-alike disabled label does not re-enable ours"
    fi
    # The same run, repeated: the pipeline that reads print-disabled must not
    # flake under pipefail (a `| grep -q` took SIGPIPE about once in five).
    local flake_runs=0 flake_hits=0
    while [ "$flake_runs" -lt 8 ]; do
        flake_runs=$((flake_runs + 1))
        reset_argv_logs
        STUB_LAUNCHCTL_DISABLED_LABEL=app.solador.agent INSTALL_STDIN="" run_install "$home"
        grep -q "^enable " "$STUB_LAUNCHCTL_ARGV" && flake_hits=$((flake_hits + 1))
    done
    assert_eq "macOS: the disabled-service check is deterministic across repeated runs" \
        "$flake_runs" "$flake_hits"

    # A plist that fails the lint leaves the PREVIOUS plist in place: it is
    # rendered and linted in staging and only then moved.
    local plist_before
    plist_before="$(cat "$plist")"
    reset_argv_logs
    STUB_PLUTIL_EXIT=1 INSTALL_STDIN="" run_install "$home"
    assert_eq "macOS: a plist that fails the lint is a failed install" "1" "$?"
    assert_eq "macOS: a failed lint leaves the previous plist untouched" "$plist_before" "$(cat "$plist")"
    if grep -q "^bootstrap" "$STUB_LAUNCHCTL_ARGV"; then
        fail "macOS: nothing is bootstrapped after a failed lint" "bootstrap was called"
    else
        pass "macOS: nothing is bootstrapped after a failed lint"
    fi

    unset SOLADOR_AGENT_RELEASE STUB_CURL_BODY STUB_UNAME_S STUB_UNAME_M STUB_SW_VERS
}

# ---- the unattended update job (#394) ----------------------------------------
#
# Off by default, opt-in with --enable-timer, preserved by a re-run without
# it, revoked only by the documented commands — each observed as what the
# installer did and did not do to the temporary HOME and to the stubbed
# service manager, not read out of the script. The metrics service is
# installed and running in every one of these; only the updater varies.

test_install_update_timer_linux() {
    local home="$TMP/home-timer" env_file unit bin out update_unit update_timer
    if [ "$HAVE_MINISIGN" != true ]; then
        skip_needs_minisign "install.sh: --enable-timer (Linux)"
        return
    fi
    make_checkout "$TEST_KEY_DIR/a.pub"
    rm -rf "$FIXTURES" "$home"
    mkdir -p "$home"
    make_fixture 2026.9.8 x86_64-unknown-linux-musl "$TEST_KEY_DIR/a.key" >/dev/null
    export SOLADOR_AGENT_RELEASE="v2026.9.8"
    export STUB_CURL_BODY='{"status":"ok","hostname":"h","version":"2026.9.8"}'
    export STUB_TAILSCALE_IP="100.64.0.9"

    env_file="$home/.config/solador-agent.env"
    unit="$home/.config/systemd/user/solador-agent.service"
    update_unit="$home/.config/systemd/user/solador-agent-update.service"
    update_timer="$home/.config/systemd/user/solador-agent-update.timer"
    bin="$home/.local/bin/solador-agent"

    # ---- a fresh default install: metrics yes, updater no, no check ----
    reset_argv_logs
    INSTALL_STDIN="fresh-tok-MUST-NOT-BE-PRINTED
" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: fresh default install (Linux, no --enable-timer)" "0" "$INSTALL_STATUS"
    assert_file_has "the default install still enables the metrics unit" "$STUB_SYSTEMCTL_ARGV" "--user enable solador-agent"
    assert_file_has "the default install still restarts the metrics unit" "$STUB_SYSTEMCTL_ARGV" "--user restart solador-agent"
    if updater_installed "$home"; then
        fail "a default install writes no updater unit or timer" "$(ls "$home/.config/systemd/user")"
    else
        pass "a default install writes no updater unit or timer"
    fi
    if manager_changed_updater; then
        fail "a default install asks systemd to change nothing about an updater" "$(grep update "$STUB_SYSTEMCTL_ARGV")"
    else
        pass "a default install asks systemd to change nothing about an updater"
    fi
    if agent_asked_to_update; then
        fail "a default install never asks the agent to update" "$(cat "$AGENT_ARGV")"
    else
        pass "a default install never asks the agent to update"
    fi
    assert_file_has "the fixture agent records its argv, so the assertion above is live" "$AGENT_ARGV" "--version"
    assert_output_has "the default install reports unattended updates as off, naming the flag" \
        "$out" "Unattended updates: off (opt in with"

    # ---- opting in: a timer and a oneshot, enabled, and nothing fired ----
    reset_argv_logs
    INSTALL_STDIN="" run_install "$home" --enable-timer
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: --enable-timer (Linux)" "0" "$INSTALL_STATUS"
    [ -f "$update_unit" ] && pass "--enable-timer writes the update oneshot" \
        || fail "--enable-timer writes the update oneshot" "$out"
    [ -f "$update_timer" ] && pass "--enable-timer writes the update timer" \
        || fail "--enable-timer writes the update timer" "$out"
    assert_file_has "the oneshot runs the absolute installed binary with 'update'" \
        "$update_unit" "ExecStart=$bin update"
    assert_file_has "the oneshot is Type=oneshot" "$update_unit" "Type=oneshot"
    assert_file_has "the oneshot treats exit 4 (not newer) as success" "$update_unit" "SuccessExitStatus=4"
    if grep -q '^EnvironmentFile=' "$update_unit"; then
        fail "the oneshot does not load the env file into its own environment" "EnvironmentFile= is in $update_unit"
    else
        pass "the oneshot does not load the env file into its own environment"
    fi
    if grep -q '@SOLADOR_AGENT_BIN@' "$update_unit"; then
        fail "the oneshot's placeholder was rendered" "@SOLADOR_AGENT_BIN@ survived"
    else
        pass "the oneshot's placeholder was rendered"
    fi
    if grep -q 'fresh-tok-MUST-NOT-BE-PRINTED' "$update_unit" "$update_timer"; then
        fail "the token is in neither the oneshot nor the timer" "it is"
    else
        pass "the token is in neither the oneshot nor the timer"
    fi
    # The cadence: monotonic, daily, first firing a day after the timer
    # starts; no realtime OnCalendar (fires on resume when its time passed
    # during sleep), no Persistent (would replay a missed firing), no
    # WakeSystem (would select the clock that runs through suspend).
    assert_file_has "the timer's first firing is 24h after it starts" "$update_timer" "OnActiveSec=24h"
    assert_file_has "the timer then fires 24h after each run" "$update_timer" "OnUnitActiveSec=24h"
    if grep -qE '^(OnCalendar|Persistent|WakeSystem|OnBootSec|OnStartupSec)=' "$update_timer"; then
        fail "the timer uses no realtime, persistent or wake-capable trigger" \
            "$(grep -E '^(OnCalendar|Persistent|WakeSystem|OnBootSec|OnStartupSec)=' "$update_timer")"
    else
        pass "the timer uses no realtime, persistent or wake-capable trigger"
    fi
    assert_eq "the timer is installed verbatim from the template" \
        "$(cat "$SCRIPT_DIR/solador-agent-update.timer")" "$(cat "$update_timer")"
    assert_file_has "the timer is enabled and started" "$STUB_SYSTEMCTL_ARGV" "--user enable --now solador-agent-update.timer"
    if grep -qE '^--user (start|restart) solador-agent-update(\.service)?$' "$STUB_SYSTEMCTL_ARGV"; then
        fail "opting in never starts the oneshot itself (no check on enable)" \
            "$(grep -E 'solador-agent-update(\.service)?$' "$STUB_SYSTEMCTL_ARGV")"
    else
        pass "opting in never starts the oneshot itself (no check on enable)"
    fi
    if grep -qE '^--user restart solador-agent-update.timer$' "$STUB_SYSTEMCTL_ARGV"; then
        fail "opting in enables the timer without restarting it" "restart would reset a running interval"
    else
        pass "opting in enables the timer without restarting it"
    fi
    # Consent is recorded after the metrics install verified, never before.
    assert_before "the timer is enabled only after the metrics service was restarted" \
        "$STUB_SYSTEMCTL_ARGV" "--user restart solador-agent" "enable --now solador-agent-update.timer"
    assert_file_has "the metrics unit is still enabled on the opt-in run" "$STUB_SYSTEMCTL_ARGV" "--user enable solador-agent"
    assert_output_has "the opt-in run says the first check is a day away" "$out" "first check in 24h"
    assert_output_has "the opt-in run names the cadence" "$out" "daily, no catch-up"
    if agent_asked_to_update; then
        fail "opting in never asks the agent to update" "$(cat "$AGENT_ARGV")"
    else
        pass "opting in never asks the agent to update"
    fi
    # The "Done" block precedes the opt-in, so a scripted caller can read
    # that the agent is serving before anything the opt-in says.
    assert_before "the opt-in run reports the verified install before the opt-in" \
        "$INSTALL_OUT" "installed and serving" "Unattended updates: enabled"

    # ---- a re-run WITHOUT the flag preserves the opt-in, byte for byte ----
    local unit_before timer_before
    unit_before="$(cat "$update_unit")"
    timer_before="$(cat "$update_timer")"
    reset_argv_logs
    INSTALL_STDIN="" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: a no-flag re-run over an opted-in host" "0" "$INSTALL_STATUS"
    assert_eq "a no-flag re-run leaves the oneshot as it was" "$unit_before" "$(cat "$update_unit")"
    assert_eq "a no-flag re-run leaves the timer as it was" "$timer_before" "$(cat "$update_timer")"
    if manager_changed_updater; then
        fail "a no-flag re-run asks systemd to change nothing about the updater (no disable, no enable)" \
            "$(grep update "$STUB_SYSTEMCTL_ARGV")"
    else
        pass "a no-flag re-run asks systemd to change nothing about the updater (no disable, no enable)"
    fi
    if agent_asked_to_update; then
        fail "a no-flag re-run never asks the agent to update" "$(cat "$AGENT_ARGV")"
    else
        pass "a no-flag re-run never asks the agent to update"
    fi
    assert_file_has "a no-flag re-run still restarts the metrics unit" "$STUB_SYSTEMCTL_ARGV" "--user restart solador-agent"
    assert_file_has "a no-flag re-run asks systemd whether the timer is enabled" "$STUB_SYSTEMCTL_ARGV" "--user is-enabled solador-agent-update.timer"
    assert_output_has "a no-flag re-run reports the opt-in as enabled, from systemd's answer" "$out" "Unattended updates: enabled (solador-agent-update.timer"
    # The documented pause (`disable --now`, files stay) must not be
    # reported as scheduled on the next no-flag re-run.
    reset_argv_logs
    STUB_SYSTEMCTL_IS_ENABLED=disabled INSTALL_STDIN="" run_install "$home"
    assert_eq "install.sh: a no-flag re-run over a paused timer" "0" "$?"
    assert_output_has "a paused timer is reported as present but not enabled" \
        "$(cat "$INSTALL_OUT")" "present but not enabled (systemd says 'disabled')"
    assert_output_has "a paused timer's report names the re-enable command" \
        "$(cat "$INSTALL_OUT")" "systemctl --user enable --now solador-agent-update.timer"
    if manager_changed_updater; then
        fail "a no-flag re-run does not re-enable a paused timer (consent is the operator's)" "$(grep update "$STUB_SYSTEMCTL_ARGV")"
    else
        pass "a no-flag re-run does not re-enable a paused timer (consent is the operator's)"
    fi

    # ---- a repeated opt-in: still one timer, one enable, nothing fired ----
    reset_argv_logs
    INSTALL_STDIN="" run_install "$home" --enable-timer
    assert_eq "install.sh: a repeated --enable-timer" "0" "$?"
    assert_eq "a repeated opt-in enables the timer exactly once more" \
        "1" "$(grep -c 'enable --now solador-agent-update.timer' "$STUB_SYSTEMCTL_ARGV")"
    assert_eq "a repeated opt-in leaves exactly one timer file" \
        "1" "$(find "$home/.config/systemd/user" -name 'solador-agent-update.timer*' | wc -l | tr -d ' ')"
    if grep -qE '^--user (start|restart) solador-agent-update(\.service)?$' "$STUB_SYSTEMCTL_ARGV"; then
        fail "a repeated opt-in never starts the oneshot either" "it did"
    else
        pass "a repeated opt-in never starts the oneshot either"
    fi

    # ---- the documented removal: only the updater goes ----
    # `systemctl --user disable --now` is stubbed; what the test can observe
    # is that a no-flag re-run after the files are removed does not bring
    # them back.
    rm -f "$update_unit" "$update_timer"
    reset_argv_logs
    INSTALL_STDIN="" run_install "$home"
    assert_eq "install.sh: a no-flag re-run after the updater was removed" "0" "$?"
    if updater_installed "$home"; then
        fail "a no-flag re-run does not re-create a removed updater" "it did"
    else
        pass "a no-flag re-run does not re-create a removed updater"
    fi
    assert_output_has "a no-flag re-run after removal reports the updater as off" \
        "$(cat "$INSTALL_OUT")" "Unattended updates: off"

    # ---- the timer failing to enable is a failed opt-in — exit 3, not 1 ----
    # 1 is "the install failed"; this install succeeded and says so first.
    reset_argv_logs
    STUB_SYSTEMCTL_TIMER_EXIT=1 INSTALL_STDIN="" run_install "$home" --enable-timer
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: a timer that cannot be enabled is a failed opt-in (exit 3)" "3" "$INSTALL_STATUS"
    assert_output_has "the failed opt-in says the metrics service is fine" "$out" "only the"
    assert_output_has "the failed opt-in names the timer to inspect" "$out" "systemctl --user status solador-agent-update.timer"
    assert_before "the failed opt-in still reported the verified install first" \
        "$INSTALL_OUT" "installed and serving" "unattended update job failed"

    # ---- a failed metrics verification records no consent ----
    rm -f "$update_unit" "$update_timer"
    reset_argv_logs
    STUB_CURL_BODY="" INSTALL_STDIN="" run_install "$home" --enable-timer
    assert_eq "install.sh: --enable-timer over a metrics service that never answers" "1" "$?"
    if updater_installed "$home"; then
        fail "no updater is written when the metrics install did not verify" "it was"
    else
        pass "no updater is written when the metrics install did not verify"
    fi
    if manager_changed_updater; then
        fail "no timer is enabled when the metrics install did not verify" "$(grep update "$STUB_SYSTEMCTL_ARGV")"
    else
        pass "no timer is enabled when the metrics install did not verify"
    fi

    # ---- a HOME with a space: the oneshot's ExecStart is quoted ----
    local spaced="$TMP/home timer space"
    rm -rf "$spaced"
    mkdir -p "$spaced"
    reset_argv_logs
    INSTALL_STDIN="
" run_install "$spaced" --enable-timer
    assert_eq "install.sh --enable-timer handles a HOME with a space (Linux)" "0" "$?"
    assert_file_has "a spaced path is double-quoted in the oneshot's ExecStart, then 'update'" \
        "$spaced/.config/systemd/user/solador-agent-update.service" \
        "ExecStart=\"$spaced/.local/bin/solador-agent\" update"

    # ---- an unmigrated /opt install refuses the opt-in, before anything ----
    rm -rf "$home"
    mkdir -p "$home/.config/systemd/user" "$home/.config"
    local opt_bin="$TMP/fake-opt-timer/solador-agent/solador-agent"
    mkdir -p "$(dirname "$opt_bin")"
    printf '#!/bin/sh\necho opt-2026.8.1\n' > "$opt_bin"
    chmod +x "$opt_bin"
    printf '[Service]\nExecStart=%s\nEnvironmentFile=%%h/.config/solador-agent.env\n' "$opt_bin" > "$unit"
    printf 'SOLADOR_AGENT_TOKEN=opt-tok-MUST-NOT-BE-PRINTED\nSOLADOR_AGENT_BIND=100.64.0.3\nSOLADOR_AGENT_PORT=7979\n' > "$env_file"
    chmod 600 "$env_file"
    reset_argv_logs
    run_install "$home" --enable-timer
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: --enable-timer on an unmigrated /opt install is refused" "1" "$INSTALL_STATUS"
    assert_output_has "the /opt opt-in refusal names the combined step" "$out" "--migrate-from-opt --enable-timer"
    if updater_installed "$home"; then
        fail "the /opt opt-in refusal creates no updater" "it did"
    else
        pass "the /opt opt-in refusal creates no updater"
    fi
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "the /opt opt-in refusal downloads nothing" "curl was invoked"
    else
        pass "the /opt opt-in refusal downloads nothing"
    fi
    if systemctl_mutated; then
        fail "the /opt opt-in refusal touches no service" "systemctl was called"
    else
        pass "the /opt opt-in refusal touches no service"
    fi
    # Both flags: migrate, then opt in, in one run.
    export STUB_CURL_BODY='{"status":"ok","hostname":"h","version":"2026.9.8"}'
    reset_argv_logs
    run_install "$home" --migrate-from-opt --enable-timer
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh --migrate-from-opt --enable-timer migrates and opts in" "0" "$INSTALL_STATUS"
    assert_file_has "after the migration the oneshot runs the user-owned binary" "$update_unit" "ExecStart=$bin update"
    assert_file_has "after the migration the metrics unit runs the user-owned binary" "$unit" "ExecStart=$bin"
    assert_file_has "after the migration the timer is enabled" "$STUB_SYSTEMCTL_ARGV" "enable --now solador-agent-update.timer"
    case "$out" in
        *"opt-tok-MUST-NOT-BE-PRINTED"*) fail "the migrate-and-opt-in run never prints the token" "it did" ;;
        *) pass "the migrate-and-opt-in run never prints the token" ;;
    esac

    # ---- an install directory this user cannot write to refuses the opt-in ----
    if [ "$(id -u)" = "0" ]; then
        skip "install.sh: --enable-timer refuses an unwritable install directory" "running as root, which can write anywhere"
    else
        rm -rf "$home"
        mkdir -p "$home/.local/bin"
        chmod 555 "$home/.local/bin"
        reset_argv_logs
        run_install "$home" --enable-timer
        out="$(cat "$INSTALL_OUT")"
        assert_eq "install.sh: --enable-timer refuses an unwritable install directory" "1" "$INSTALL_STATUS"
        assert_output_has "the unwritable refusal names the directory" "$out" "$home/.local/bin is not writable"
        if [ -s "$STUB_CURL_ARGV" ]; then
            fail "the unwritable refusal downloads nothing" "curl was invoked"
        else
            pass "the unwritable refusal downloads nothing"
        fi
        chmod 755 "$home/.local/bin"
        # Without the flag the same directory is not this script's concern
        # here: the install itself fails later on its own terms, and that is
        # #392's behaviour, unchanged.

        # A writable directory holding a binary this user cannot write —
        # the shape a `sudo install` leaves — is refused the same way.
        printf '#!/bin/sh\nexit 0\n' > "$home/.local/bin/solador-agent"
        chmod 555 "$home/.local/bin/solador-agent"
        reset_argv_logs
        run_install "$home" --enable-timer
        out="$(cat "$INSTALL_OUT")"
        assert_eq "install.sh: --enable-timer refuses an unwritable installed binary" "1" "$INSTALL_STATUS"
        assert_output_has "the unwritable-binary refusal names the binary" "$out" "$home/.local/bin/solador-agent is not writable"
        if [ -s "$STUB_CURL_ARGV" ]; then
            fail "the unwritable-binary refusal downloads nothing" "curl was invoked"
        else
            pass "the unwritable-binary refusal downloads nothing"
        fi
        chmod 755 "$home/.local/bin/solador-agent"
    fi

    # ---- as root, the opt-in is refused: the updater will not run as root ----
    local root_stubs="$TMP/stubs-root"
    mkdir -p "$root_stubs"
    cat > "$root_stubs/id" <<STUB
#!/usr/bin/env bash
case "\${1:-}" in
    -u) echo 0 ;;
    *) exec "$(command -v id)" "\$@" ;;
esac
STUB
    chmod +x "$root_stubs/id"
    rm -rf "$home"
    mkdir -p "$home"
    reset_argv_logs
    INSTALL_PATH="$root_stubs:$STUBS:$TOOLBIN" run_install "$home" --enable-timer
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: --enable-timer as root is refused" "1" "$INSTALL_STATUS"
    assert_output_has "the root refusal says why" "$out" "refused as root"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "the root refusal downloads nothing" "curl was invoked"
    else
        pass "the root refusal downloads nothing"
    fi
    INSTALL_PATH=""

    unset SOLADOR_AGENT_RELEASE STUB_CURL_BODY STUB_TAILSCALE_IP
}

test_install_update_timer_macos() {
    local home="$TMP/home mac timer & co" plist update_plist launcher env_file bin out
    if [ "$HAVE_MINISIGN" != true ]; then
        skip_needs_minisign "install.sh: --enable-timer (macOS, stubbed launchctl)"
        return
    fi
    make_checkout "$TEST_KEY_DIR/a.pub"
    rm -rf "$FIXTURES" "$home"
    mkdir -p "$home"
    make_fixture 2026.9.8 aarch64-apple-darwin "$TEST_KEY_DIR/a.key" >/dev/null
    export SOLADOR_AGENT_RELEASE="v2026.9.8"
    export STUB_CURL_BODY='{"status":"ok","hostname":"mac","version":"2026.9.8"}'
    export STUB_UNAME_S=Darwin STUB_UNAME_M=arm64 STUB_SW_VERS=15.6

    env_file="$home/.config/solador-agent.env"
    plist="$home/Library/LaunchAgents/app.solador.agent.plist"
    update_plist="$home/Library/LaunchAgents/app.solador.agent.update.plist"
    launcher="$home/.local/bin/solador-agent-launchd"
    bin="$home/.local/bin/solador-agent"
    local update_log="$home/Library/Logs/solador-agent-update.log"

    # ---- a fresh default install: the metrics LaunchAgent, and no updater ----
    reset_argv_logs
    INSTALL_STDIN="mac-tok-MUST-NOT-BE-PRINTED
" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: fresh default install (macOS, no --enable-timer)" "0" "$INSTALL_STATUS"
    assert_file_has "the default install still bootstraps the metrics LaunchAgent" \
        "$STUB_LAUNCHCTL_ARGV" "bootstrap gui/$(id -u) $plist"
    if updater_installed "$home"; then
        fail "a default macOS install writes no updater plist" "$(ls "$home/Library/LaunchAgents")"
    else
        pass "a default macOS install writes no updater plist"
    fi
    if manager_changed_updater; then
        fail "a default macOS install asks launchd to change nothing about an updater" "$(grep update "$STUB_LAUNCHCTL_ARGV")"
    else
        pass "a default macOS install asks launchd to change nothing about an updater"
    fi
    if agent_asked_to_update; then
        fail "a default macOS install never asks the agent to update" "$(cat "$AGENT_ARGV")"
    else
        pass "a default macOS install never asks the agent to update"
    fi
    assert_output_has "the default macOS install reports unattended updates as off" "$out" "Unattended updates: off"

    # ---- opting in: a second LaunchAgent, loaded, and not run ----
    reset_argv_logs
    INSTALL_UMASK=002 INSTALL_STDIN="" run_install "$home" --enable-timer
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: --enable-timer (macOS, stubbed launchctl)" "0" "$INSTALL_STATUS"
    [ -f "$update_plist" ] && pass "--enable-timer writes the updater plist beside the metrics one" \
        || fail "--enable-timer writes the updater plist beside the metrics one" "$out"
    assert_eq "the updater plist is mode 0644 even under umask 002" "644" "$(file_mode "$update_plist")"
    assert_file_has "the updater plist carries the sibling label" "$update_plist" "<string>app.solador.agent.update</string>"
    assert_file_has "the updater plist names the metrics label it maintains" "$update_plist" \
        "<string>app.solador.agent</string>"
    assert_file_has "the updater plist ends ProgramArguments with the word update" "$update_plist" "<string>update</string>"
    assert_file_has "the updater plist fires on a 24h interval" "$update_plist" "<integer>86400</integer>"
    assert_file_has "the updater plist pins HOME to the install's, XML-escaped" "$update_plist" \
        "<string>$(xml_escape "$home")</string>"
    if grep -qE '<key>(RunAtLoad|KeepAlive|StartCalendarInterval)</key>' "$update_plist"; then
        fail "the updater plist has no RunAtLoad, KeepAlive or StartCalendarInterval" \
            "$(grep -E '<key>(RunAtLoad|KeepAlive|StartCalendarInterval)</key>' "$update_plist")"
    else
        pass "the updater plist has no RunAtLoad, KeepAlive or StartCalendarInterval"
    fi
    if grep -q "mac-tok-MUST-NOT-BE-PRINTED" "$update_plist"; then
        fail "the token is not in the updater plist" "it is"
    else
        pass "the token is not in the updater plist"
    fi
    if grep -q '@[A-Z_]*@' "$update_plist"; then
        fail "every updater plist placeholder was rendered" "$(grep -o '@[A-Z_]*@' "$update_plist" | head -n3 | tr '\n' ' ')"
    else
        pass "every updater plist placeholder was rendered"
    fi
    if command -v python3 >/dev/null 2>&1; then
        local parsed
        parsed="$(python3 - "$update_plist" "$launcher" "$bin" "$env_file" "$update_log" "$home" <<'PY' 2>&1
import plistlib, sys
with open(sys.argv[1], "rb") as f:
    d = plistlib.load(f)
env = d.get("EnvironmentVariables", {})
ok = (
    d.get("Label") == "app.solador.agent.update"
    and d.get("ProgramArguments") == [sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5], "update"]
    and d.get("StandardOutPath") == sys.argv[5]
    and d.get("StandardErrorPath") == sys.argv[5]
    and d.get("StartInterval") == 86400
    and "RunAtLoad" not in d
    and "KeepAlive" not in d
    and "StartCalendarInterval" not in d
    and d.get("ProcessType") == "Background"
    and env.get("PATH") == "/usr/bin:/bin:/usr/sbin:/sbin"
    and env.get("HOME") == sys.argv[6]
    and env.get("SOLADOR_AGENT_LAUNCHD_LABEL") == "app.solador.agent"
    and "SOLADOR_AGENT_TOKEN" not in env
)
print("ok" if ok else "mismatch: " + repr(d))
PY
)"
        assert_eq "the rendered updater plist parses and carries exactly the expected keys" "ok" "$parsed"
    else
        skip "the rendered updater plist parses and carries exactly the expected keys" "python3 not on PATH"
    fi
    assert_file_has "the updater plist is bootstrapped into gui/<uid>" "$STUB_LAUNCHCTL_ARGV" \
        "bootstrap gui/$(id -u) $update_plist"
    if grep -q "kickstart" "$STUB_LAUNCHCTL_ARGV"; then
        fail "opting in never kickstarts the updater (no check on enable)" "$(grep kickstart "$STUB_LAUNCHCTL_ARGV")"
    else
        pass "opting in never kickstarts the updater (no check on enable)"
    fi
    if grep -q "^bootout gui/$(id -u)/app.solador.agent.update" "$STUB_LAUNCHCTL_ARGV"; then
        fail "a first opt-in does not bootout an updater that is not loaded" "it did"
    else
        pass "a first opt-in does not bootout an updater that is not loaded"
    fi
    assert_before "the updater is bootstrapped only after the metrics LaunchAgent was" \
        "$STUB_LAUNCHCTL_ARGV" "bootstrap gui/$(id -u) $plist" "bootstrap gui/$(id -u) $update_plist"
    assert_output_has "the macOS opt-in run says the first check is a day away" "$out" "first check in 24h"
    # The metrics plist is untouched by the opt-in beyond its own re-render.
    assert_file_has "the metrics plist still names only the metrics label" "$plist" "<string>app.solador.agent</string>"
    if grep -q "<string>update</string>" "$plist"; then
        fail "the metrics plist did not become the updater" "it carries the update argument"
    else
        pass "the metrics plist did not become the updater"
    fi

    # ---- a re-run WITHOUT the flag: the updater is left exactly as it is ----
    local update_before
    update_before="$(cat "$update_plist")"
    reset_argv_logs
    STUB_LAUNCHCTL_LOADED_EXIT=0 INSTALL_STDIN="" run_install "$home"
    assert_eq "install.sh: a no-flag macOS re-run over an opted-in host" "0" "$?"
    assert_eq "a no-flag macOS re-run leaves the updater plist byte for byte" "$update_before" "$(cat "$update_plist")"
    if manager_changed_updater; then
        fail "a no-flag macOS re-run asks launchd to change nothing about the updater" "$(grep update "$STUB_LAUNCHCTL_ARGV")"
    else
        pass "a no-flag macOS re-run asks launchd to change nothing about the updater"
    fi
    if agent_asked_to_update; then
        fail "a no-flag macOS re-run never asks the agent to update" "$(cat "$AGENT_ARGV")"
    else
        pass "a no-flag macOS re-run never asks the agent to update"
    fi
    assert_file_has "a no-flag macOS re-run still re-bootstraps the metrics LaunchAgent" \
        "$STUB_LAUNCHCTL_ARGV" "bootstrap gui/$(id -u) $plist"
    assert_output_has "a no-flag macOS re-run reports the opt-in as loaded, from launchd's answer" \
        "$(cat "$INSTALL_OUT")" "Unattended updates: loaded (gui/$(id -u)/app.solador.agent.update"
    # The documented pause (`bootout`, plist stays) is reported as such, and
    # not re-loaded by a no-flag re-run.
    reset_argv_logs
    INSTALL_STDIN="" run_install "$home"
    assert_eq "install.sh: a no-flag macOS re-run over a booted-out updater" "0" "$?"
    assert_output_has "a booted-out updater is reported as present but not loaded" \
        "$(cat "$INSTALL_OUT")" "is present but not loaded"
    if manager_changed_updater; then
        fail "a no-flag macOS re-run does not re-load a booted-out updater" "$(grep update "$STUB_LAUNCHCTL_ARGV")"
    else
        pass "a no-flag macOS re-run does not re-load a booted-out updater"
    fi

    # ---- a repeated opt-in over a loaded updater: bootout, then bootstrap ----
    reset_argv_logs
    STUB_LAUNCHCTL_LOADED_EXIT=0 INSTALL_STDIN="" run_install "$home" --enable-timer
    assert_eq "install.sh: a repeated macOS --enable-timer" "0" "$?"
    assert_before "a repeated opt-in boots the loaded updater out before bootstrapping it again" \
        "$STUB_LAUNCHCTL_ARGV" "bootout gui/$(id -u)/app.solador.agent.update" "bootstrap gui/$(id -u) $update_plist"
    assert_eq "a repeated opt-in bootstraps the updater exactly once" \
        "1" "$(grep -c "^bootstrap gui/$(id -u) $update_plist" "$STUB_LAUNCHCTL_ARGV")"
    assert_eq "a repeated opt-in leaves exactly one updater plist" \
        "1" "$(find "$home/Library/LaunchAgents" -name '*.update.plist*' | wc -l | tr -d ' ')"

    # ---- the documented removal, then a no-flag re-run: it stays gone ----
    rm -f "$update_plist"
    reset_argv_logs
    STUB_LAUNCHCTL_LOADED_EXIT=0 INSTALL_STDIN="" run_install "$home"
    assert_eq "install.sh: a no-flag macOS re-run after the updater was removed" "0" "$?"
    if updater_installed "$home"; then
        fail "a no-flag macOS re-run does not re-create a removed updater" "it did"
    else
        pass "a no-flag macOS re-run does not re-create a removed updater"
    fi

    # ---- the updater plist failing its lint: the metrics half is intact ----
    reset_argv_logs
    STUB_PLUTIL_FAIL_MATCH="app.solador.agent.update.plist" INSTALL_STDIN="" run_install "$home" --enable-timer
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: an updater plist that fails the lint is a failed opt-in (exit 3)" "3" "$INSTALL_STATUS"
    assert_before "the failed macOS opt-in still reported the verified install first" \
        "$INSTALL_OUT" "installed and serving" "unattended update job failed"
    assert_file_has "a failed updater lint still bootstrapped the metrics LaunchAgent first" \
        "$STUB_LAUNCHCTL_ARGV" "bootstrap gui/$(id -u) $plist"
    if grep -q "^bootstrap gui/$(id -u) $update_plist" "$STUB_LAUNCHCTL_ARGV"; then
        fail "nothing is bootstrapped for an updater plist that failed the lint" "it was"
    else
        pass "nothing is bootstrapped for an updater plist that failed the lint"
    fi
    [ -e "$update_plist" ] && fail "a failed updater lint leaves no updater plist behind" "$update_plist exists" \
        || pass "a failed updater lint leaves no updater plist behind"
    assert_output_has "the failed opt-in says only the update job failed" "$out" "only the"

    # ---- a failed metrics verification records no consent (macOS) ----
    reset_argv_logs
    STUB_CURL_BODY="" INSTALL_STDIN="" run_install "$home" --enable-timer
    assert_eq "install.sh: --enable-timer over a macOS service that never answers" "1" "$?"
    if updater_installed "$home"; then
        fail "no updater plist is written when the macOS metrics install did not verify" "it was"
    else
        pass "no updater plist is written when the macOS metrics install did not verify"
    fi

    # ---- the throwaway-label seam: the updater is always the metrics label's sibling ----
    export STUB_CURL_BODY='{"status":"ok","hostname":"mac","version":"2026.9.8"}'
    reset_argv_logs
    SOLADOR_AGENT_LAUNCHD_LABEL=app.solador.agent.deploytest INSTALL_STDIN="" run_install "$home" --enable-timer
    assert_eq "install.sh: --enable-timer under an overridden label" "0" "$?"
    local override_plist="$home/Library/LaunchAgents/app.solador.agent.deploytest.update.plist"
    [ -f "$override_plist" ] && pass "the updater under an overridden label is <label>.update" \
        || fail "the updater under an overridden label is <label>.update" "$(ls "$home/Library/LaunchAgents")"
    assert_file_has "the overridden updater names the overridden metrics label" "$override_plist" \
        "<string>app.solador.agent.deploytest</string>"
    assert_file_has "the overridden updater is bootstrapped" "$STUB_LAUNCHCTL_ARGV" "bootstrap gui/$(id -u) $override_plist"

    unset SOLADOR_AGENT_RELEASE STUB_CURL_BODY STUB_UNAME_S STUB_UNAME_M STUB_SW_VERS
}

# The launcher, on its own: it must export exactly the documented keys, never
# evaluate the file as shell, and exec the binary. Run under a temporary HOME
# so nothing it does can touch the invoking user's own ~/Library.
test_launchd_launcher() {
    local launcher="$SCRIPT_DIR/run-agent.sh" env_file="$TMP/launcher.env" probe="$TMP/launcher-probe" out
    local lhome="$TMP/launcher-home" log
    mkdir -p "$lhome/Library/Logs"
    log="$lhome/Library/Logs/solador-agent.log"
    cat > "$probe" <<'STUB'
#!/bin/sh
printf 'TOKEN=%s\nBIND=%s\nPORT=%s\nLOG=%s\nEVIL=%s\n' \
    "${SOLADOR_AGENT_TOKEN:-<unset>}" "${SOLADOR_AGENT_BIND:-<unset>}" \
    "${SOLADOR_AGENT_PORT:-<unset>}" "${RUST_LOG:-<unset>}" "${EVIL:-<unset>}"
STUB
    chmod +x "$probe"
    # run_launcher [args...]: the launcher under the temp HOME; stdout to
    # $INSTALL_OUT, stderr to $STDERR, status returned.
    run_launcher() {
        ( export HOME="$lhome"; "$BASH" "$launcher" "$@" ) >"$INSTALL_OUT" 2>"$STDERR"
    }

    # A token with shell metacharacters, a comment, a blank, and a line that
    # is not a documented key.
    printf 'SOLADOR_AGENT_TOKEN=abc$(touch %s)`id`;x\n# comment\n\nSOLADOR_AGENT_BIND=127.0.0.1\nSOLADOR_AGENT_PORT=7878\nRUST_LOG=debug\nEVIL=1\n' \
        "$TMP/launcher-evaluated" > "$env_file"
    rm -f "$TMP/launcher-evaluated"
    run_launcher "$probe" "$env_file" "$log"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "the launcher execs the binary with the token exported" \
        "$(printf 'TOKEN=abc$(touch %s)`id`;x\nBIND=127.0.0.1\nPORT=7878\nLOG=debug\nEVIL=<unset>' "$TMP/launcher-evaluated")" "$out"
    if [ -e "$TMP/launcher-evaluated" ]; then
        fail "the launcher never evaluates the env file as shell" "a \$(…) in the token was executed"
    else
        pass "the launcher never evaluates the env file as shell"
    fi
    assert_file_has "the launcher reports an unrecognised line by number" "$STDERR" "unrecognised line 7"
    if grep -q "EVIL" "$STDERR"; then
        fail "the launcher never echoes the file's contents" "a line's text reached stderr"
    else
        pass "the launcher never echoes the file's contents"
    fi

    # Values are read the way systemd's EnvironmentFile= reads them: a
    # hand-rotated `KEY="value"`, a trailing space, and a CRLF line must reach
    # the agent as the bare value, or the token 401s on macOS only.
    printf 'SOLADOR_AGENT_TOKEN="quoted tok"\r\nSOLADOR_AGENT_BIND= 10.0.0.1 \nSOLADOR_AGENT_PORT=\x277979\x27\n' > "$env_file"
    run_launcher "$probe" "$env_file" "$log"
    assert_eq "the launcher strips quotes, whitespace and CR like EnvironmentFile=" \
        "$(printf 'TOKEN=quoted tok\nBIND=10.0.0.1\nPORT=7979\nLOG=<unset>\nEVIL=<unset>')" "$(cat "$INSTALL_OUT")"
    # And env_value — the installer's and verify_health's reader — agrees with
    # the launcher on every one of those.
    assert_eq "env_value reads the quoted CRLF token the way the launcher does" \
        "quoted tok" "$(env_value "$env_file" SOLADOR_AGENT_TOKEN)"
    assert_eq "env_value trims whitespace the way the launcher does" \
        "10.0.0.1" "$(env_value "$env_file" SOLADOR_AGENT_BIND)"
    assert_eq "env_value strips single quotes the way the launcher does" \
        "7979" "$(env_value "$env_file" SOLADOR_AGENT_PORT)"
    assert_empty "env_value prints nothing for an absent key" "$(env_value "$env_file" RUST_LOG)"
    assert_empty "env_value prints nothing for an absent file" "$(env_value "$TMP/no-such.env" SOLADOR_AGENT_TOKEN)"
    printf 'SOLADOR_AGENT_TOKENX=nope\n' > "$TMP/prefix.env"
    assert_empty "env_value does not match a key by prefix" "$(env_value "$TMP/prefix.env" SOLADOR_AGENT_TOKEN)"
    # Last wins, like EnvironmentFile= and the launcher's export loop: a token
    # rotated by appending a line is the token the probe verifies.
    printf 'SOLADOR_AGENT_TOKEN=old\nSOLADOR_AGENT_BIND=127.0.0.1\nSOLADOR_AGENT_TOKEN=new\n' > "$TMP/dup.env"
    assert_eq "env_value takes the LAST occurrence of a key" "new" "$(env_value "$TMP/dup.env" SOLADOR_AGENT_TOKEN)"
    cat > "$probe" <<'STUB'
#!/bin/sh
printf 'TOKEN=%s\n' "${SOLADOR_AGENT_TOKEN:-<unset>}"
STUB
    run_launcher "$probe" "$TMP/dup.env" "$log"
    assert_eq "the launcher takes the LAST occurrence of a key, agreeing with env_value" \
        "TOKEN=new" "$(cat "$INSTALL_OUT")"
    cat > "$probe" <<'STUB'
#!/bin/sh
printf 'TOKEN=%s\nBIND=%s\nPORT=%s\nLOG=%s\nEVIL=%s\n' \
    "${SOLADOR_AGENT_TOKEN:-<unset>}" "${SOLADOR_AGENT_BIND:-<unset>}" \
    "${SOLADOR_AGENT_PORT:-<unset>}" "${RUST_LOG:-<unset>}" "${EVIL:-<unset>}"
STUB

    run_launcher "$probe" "$TMP/no-such.env" "$log" || true
    assert_output_has "the launcher's diagnostics are timestamped" "$(cat "$STDERR")" "Z solador-agent-launchd:"
    run_launcher "$probe" "$TMP/no-such.env" "$log"
    assert_eq "the launcher fails when the env file is missing" "1" "$?"
    run_launcher "$TMP/no-such-bin" "$env_file" "$log"
    assert_eq "the launcher fails when the binary is missing" "1" "$?"
    run_launcher "$probe" "$env_file"
    assert_eq "the launcher refuses the wrong number of arguments" "2" "$?"

    # ~/.docker/bin joins PATH: Docker Desktop's no-admin install puts docker
    # only there, and the plist cannot name $HOME.
    cat > "$probe" <<'STUB'
#!/bin/sh
printf '%s\n' "$PATH"
STUB
    run_launcher "$probe" "$env_file" "$log"
    assert_output_has "the launcher appends ~/.docker/bin to PATH" "$(cat "$INSTALL_OUT")" "$lhome/.docker/bin"

    # Log rotation, with launchd's own arrangement reproduced: the log is
    # opened for append BEFORE the launcher starts and handed to it as
    # stdout/stderr. A rename alone would leave every line of this run in
    # `.1`; the launcher must reopen the streams so the agent's output lands
    # in the fresh file.
    cat > "$probe" <<'STUB'
#!/bin/sh
echo "agent-output-marker"
STUB
    if command -v dd >/dev/null 2>&1; then
        dd if=/dev/zero of="$log" bs=1024 count=1 seek=10241 >/dev/null 2>&1   # 10 MB + 1 KB, sparse
        ( export HOME="$lhome"; "$BASH" "$launcher" "$probe" "$env_file" "$log" ) >>"$log" 2>&1
        assert_eq "the launcher rotates a log over the cap" "0" "$?"
        if [ -f "$log.1" ]; then
            pass "the oversized log became .1"
        else
            fail "the oversized log became .1" "no $log.1"
        fi
        assert_file_has "after rotation the agent's output lands in the fresh log" "$log" "agent-output-marker"
        assert_file_has "after rotation the rotation notice lands in the fresh log" "$log" "rotated the previous log"
        if grep -q "agent-output-marker" "$log.1" 2>/dev/null; then
            fail "nothing from this run lands in .1" "the agent's output went to the renamed file"
        else
            pass "nothing from this run lands in .1"
        fi
        rm -f "$log" "$log.1"
        : > "$log"
        ( export HOME="$lhome"; "$BASH" "$launcher" "$probe" "$env_file" "$log" ) >>"$log" 2>&1
        if [ -f "$log.1" ]; then
            fail "a small log is not rotated" "$log.1 appeared"
        else
            pass "a small log is not rotated"
        fi
        # The launcher's OWN refusals are what fill the log under KeepAlive
        # (a missing env file every 3 s), so rotation must run before them.
        rm -f "$log" "$log.1"
        dd if=/dev/zero of="$log" bs=1024 count=1 seek=10241 >/dev/null 2>&1
        ( export HOME="$lhome"; "$BASH" "$launcher" "$probe" "$TMP/no-such.env" "$log" ) >>"$log" 2>&1 || true
        if [ -f "$log.1" ]; then
            pass "an oversized log is rotated even when the launcher then refuses to start"
        else
            fail "an oversized log is rotated even when the launcher then refuses to start" "no $log.1"
        fi
        assert_file_has "the refusal lands in the fresh log" "$log" "not found; re-run deploy/install.sh"
        rm -f "$log" "$log.1"
    else
        skip "the launcher rotates a log over the cap" "no dd"
    fi

    # The launcher's allow-list is a copy of the keys the agent reads. Bind
    # the two: every SOLADOR_AGENT_* the Rust source names must be in the
    # launcher, or a key added to the agent reaches Linux (EnvironmentFile=
    # passes everything) and is silently dropped on macOS.
    #
    # One named exception, and it is a positive list so the next key still
    # trips this: SOLADOR_AGENT_LAUNCHD_LABEL is read by `solador-agent
    # update`/`rollback` (#393) from the MAINTENANCE command's own
    # environment — the same test seam install.sh honours, so a throwaway
    # LaunchAgent can be updated beside a real one — and never from the env
    # file. The metrics service does not read it, so the launcher has
    # nothing to export.
    local agent_keys launcher_keys
    agent_keys="$(grep -rhoE 'SOLADOR_AGENT_[A-Z_]+' "$SCRIPT_DIR/../src" | sort -u \
        | grep -vx 'SOLADOR_AGENT_LAUNCHD_LABEL' | tr '\n' ' ')"
    launcher_keys="$(grep -oE 'SOLADOR_AGENT_[A-Z_]+=\*' "$launcher" | sed 's/=\*$//' | sort -u | tr '\n' ' ')"
    assert_eq "the launcher allow-lists every SOLADOR_AGENT_* key the agent reads" \
        "$agent_keys" "$launcher_keys"
}

# The launcher in update mode (#394): forwards exactly `update` to the binary,
# exports nothing from the env file, and stands between launchd and the
# updater as the no-catch-up guard — with the clock, the wake time and the
# boot time all stubbed, so "a firing seconds after wake" and "a second
# firing inside the interval" are states the test sets rather than waits
# for. The decision on #394 forbids a check made at wake to recover a missed
# interval; launchd.plist(5) says a StartInterval firing that falls during
# sleep is missed, and this guard is what holds either way.
test_launchd_launcher_update() {
    local launcher="$SCRIPT_DIR/run-agent.sh" env_file="$TMP/launcher-update.env" probe="$TMP/launcher-update-probe"
    local lhome="$TMP/launcher-update-home" log stamp ran
    mkdir -p "$lhome/Library/Logs"
    log="$lhome/Library/Logs/solador-agent-update.log"
    stamp="$TMP/solador-agent-update.last-attempt"
    ran="$TMP/launcher-update-ran"
    printf 'SOLADOR_AGENT_TOKEN=upd-tok-MUST-NOT-BE-EXPORTED\nSOLADOR_AGENT_BIND=127.0.0.1\nSOLADOR_AGENT_PORT=7878\n' > "$env_file"
    # The probe records that it ran, with what, and whether the env file's
    # keys reached its environment; its exit status is STUB_PROBE_EXIT.
    cat > "$probe" <<STUB
#!/bin/sh
printf 'ARGS=%s\\nTOKEN=%s\\nBIND=%s\\n' "\$*" "\${SOLADOR_AGENT_TOKEN:-<unset>}" "\${SOLADOR_AGENT_BIND:-<unset>}" > "$ran"
exit "\${STUB_PROBE_EXIT:-0}"
STUB
    chmod +x "$probe"
    export STUB_SYSCTL_ARGV="$TMP/sysctl-argv"
    # run_update_launcher [args...]: the launcher with the clock stubs ahead
    # of PATH; stdout to $INSTALL_OUT, stderr to $STDERR, status returned.
    run_update_launcher() {
        rm -f "$ran"
        ( export HOME="$lhome"; export PATH="$STUBS_CLOCK:$PATH"; "$BASH" "$launcher" "$@" ) >"$INSTALL_OUT" 2>"$STDERR"
    }
    local now=1789237294   # an arbitrary epoch second
    export STUB_DATE_EPOCH="$now"

    # ---- a scheduled firing well after wake, no earlier attempt: it runs ----
    rm -f "$stamp"
    export STUB_WAKETIME_SEC=$((now - 86400)) STUB_BOOTTIME_SEC=$((now - 200000))
    run_update_launcher "$probe" "$env_file" "$log" update
    assert_eq "update mode: a firing a day after wake, with no earlier attempt, runs the updater" "0" "$?"
    assert_eq "update mode: the binary is invoked with exactly 'update' and nothing from the env file" \
        "$(printf 'ARGS=update\nTOKEN=<unset>\nBIND=<unset>')" "$(cat "$ran" 2>/dev/null)"
    assert_eq "update mode: the attempt is stamped with the clock's time" "$now" "$(cat "$stamp" 2>/dev/null)"
    assert_output_has "update mode: the run is logged with its clock context" "$(cat "$STDERR")" "update: running $probe update (last wake 86400s ago)"
    [ -e "$stamp.new" ] && fail "update mode: no stamp .new is left behind" "$stamp.new exists" \
        || pass "update mode: no stamp .new is left behind"

    # ---- the same firing again (a coalesced or duplicate one): discarded ----
    run_update_launcher "$probe" "$env_file" "$log" update
    assert_eq "update mode: a second firing inside the interval exits 0" "0" "$?"
    [ -e "$ran" ] && fail "update mode: a second firing inside the interval never runs the updater" "the probe ran" \
        || pass "update mode: a second firing inside the interval never runs the updater"
    assert_output_has "update mode: the discarded firing says why" "$(cat "$STDERR")" "last attempt was 0s ago"
    assert_eq "update mode: a discarded firing does not move the stamp" "$now" "$(cat "$stamp")"

    # ---- the interval boundary: 23h less a second holds, 23h runs ----
    printf '%s\n' "$((now - 82799))" > "$stamp"
    run_update_launcher "$probe" "$env_file" "$log" update
    [ -e "$ran" ] && fail "update mode: 82799s since the last attempt is inside the interval" "the probe ran" \
        || pass "update mode: 82799s since the last attempt is inside the interval"
    printf '%s\n' "$((now - 82800))" > "$stamp"
    run_update_launcher "$probe" "$env_file" "$log" update
    [ -e "$ran" ] && pass "update mode: 82800s since the last attempt runs the updater" \
        || fail "update mode: 82800s since the last attempt runs the updater" "$(cat "$STDERR")"

    # ---- a firing seconds after wake, a day since the last attempt: the
    # wake-time catch-up the decision forbids, and it is discarded ----
    printf '%s\n' "$((now - 90000))" > "$stamp"
    export STUB_WAKETIME_SEC=$((now - 30))
    run_update_launcher "$probe" "$env_file" "$log" update
    assert_eq "update mode: a firing 30s after wake exits 0" "0" "$?"
    [ -e "$ran" ] && fail "update mode: a firing 30s after wake never runs the updater (no catch-up at wake)" "the probe ran" \
        || pass "update mode: a firing 30s after wake never runs the updater (no catch-up at wake)"
    assert_output_has "update mode: the wake-time refusal names the cadence" "$(cat "$STDERR")" "woke or booted 30s ago"
    assert_eq "update mode: a wake-time refusal does not move the stamp" "$((now - 90000))" "$(cat "$stamp")"
    # 299s is still "just woke"; 300s is not.
    export STUB_WAKETIME_SEC=$((now - 299))
    run_update_launcher "$probe" "$env_file" "$log" update
    [ -e "$ran" ] && fail "update mode: 299s after wake is still a wake-time firing" "the probe ran" \
        || pass "update mode: 299s after wake is still a wake-time firing"
    export STUB_WAKETIME_SEC=$((now - 300))
    run_update_launcher "$probe" "$env_file" "$log" update
    [ -e "$ran" ] && pass "update mode: 300s after wake is a scheduled firing" \
        || fail "update mode: 300s after wake is a scheduled firing" "$(cat "$STDERR")"

    # ---- a firing seconds after boot (a wake time older than the boot) ----
    printf '%s\n' "$((now - 90000))" > "$stamp"
    export STUB_WAKETIME_SEC=$((now - 90000)) STUB_BOOTTIME_SEC=$((now - 30))
    run_update_launcher "$probe" "$env_file" "$log" update
    [ -e "$ran" ] && fail "update mode: a firing 30s after boot never runs the updater" "the probe ran" \
        || pass "update mode: a firing 30s after boot never runs the updater"
    export STUB_BOOTTIME_SEC=$((now - 200000))

    # ---- the clocks the guard cannot read HOLD the check: exit 6, a code the
    # updater never uses, so a permanent hold cannot read as a good day ----
    printf '%s\n' "$((now - 90000))" > "$stamp"
    STUB_SYSCTL_EXIT=1 run_update_launcher "$probe" "$env_file" "$log" update
    assert_eq "update mode: an unreadable wake time exits 6 (held)" "6" "$?"
    [ -e "$ran" ] && fail "update mode: an unreadable wake time holds the check rather than running it" "the probe ran" \
        || pass "update mode: an unreadable wake time holds the check rather than running it"
    assert_output_has "update mode: the held check names the clock it could not read" "$(cat "$STDERR")" "kern.waketime"
    STUB_DATE_EPOCH="not-a-number" run_update_launcher "$probe" "$env_file" "$log" update
    assert_eq "update mode: a clock that prints garbage exits 6 (held)" "6" "$?"
    [ -e "$ran" ] && fail "update mode: an unreadable clock holds the check" "the probe ran" \
        || pass "update mode: an unreadable clock holds the check"
    # A `date` that FAILS (rather than prints garbage) must reach the same
    # hold, not die under `set -e` with no log line.
    STUB_DATE_EXIT=1 run_update_launcher "$probe" "$env_file" "$log" update
    assert_eq "update mode: a clock that fails exits 6 (held), not set -e's 1" "6" "$?"
    assert_output_has "update mode: a clock that fails is logged as a hold" "$(cat "$STDERR")" "cannot read the clock"
    printf 'garbage\n' > "$stamp"
    run_update_launcher "$probe" "$env_file" "$log" update
    assert_eq "update mode: an unreadable stamp exits 6 (held)" "6" "$?"
    [ -e "$ran" ] && fail "update mode: an unreadable stamp holds the check" "the probe ran" \
        || pass "update mode: an unreadable stamp holds the check"
    assert_output_has "update mode: the held check names the stamp file to remove" "$(cat "$STDERR")" "Remove that file"
    assert_eq "update mode: a held check does not rewrite the stamp" "garbage" "$(cat "$stamp")"
    # A clock that moved backwards past the last attempt is discarded (exit
    # 0): tomorrow's firing resolves it by itself.
    printf '%s\n' "$((now + 100))" > "$stamp"
    run_update_launcher "$probe" "$env_file" "$log" update
    assert_eq "update mode: a clock behind the last attempt exits 0 (discarded)" "0" "$?"
    [ -e "$ran" ] && fail "update mode: a clock behind the last attempt holds the check" "the probe ran" \
        || pass "update mode: a clock behind the last attempt holds the check"
    assert_output_has "update mode: the backwards clock is named" "$(cat "$STDERR")" "moved backwards"
    # A stamp that cannot be written holds too: an attempt that cannot be
    # recorded is the retry loop the stamp exists to prevent.
    if [ "$(id -u)" = "0" ]; then
        skip "update mode: an unwritable stamp directory exits 6 (held)" "running as root, which can write anywhere"
    else
        local ro_dir="$TMP/launcher-update-ro"
        mkdir -p "$ro_dir"
        cp "$env_file" "$ro_dir/solador-agent.env"
        chmod 555 "$ro_dir"
        run_update_launcher "$probe" "$ro_dir/solador-agent.env" "$log" update
        assert_eq "update mode: an unwritable stamp directory exits 6 (held)" "6" "$?"
        [ -e "$ran" ] && fail "update mode: an unwritable stamp holds the check" "the probe ran" \
            || pass "update mode: an unwritable stamp holds the check"
        assert_output_has "update mode: the unwritable stamp is named" "$(cat "$STDERR")" "could not write $ro_dir/solador-agent-update.last-attempt"
        [ -e "$ro_dir/solador-agent-update.last-attempt.new" ] && fail "update mode: no stamp .new is left after a failed write" "it is" \
            || pass "update mode: no stamp .new is left after a failed write"
        chmod 755 "$ro_dir"
    fi

    # ---- the updater's exit status is launchd's, not a wrapper's ----
    rm -f "$stamp"
    STUB_PROBE_EXIT=4 run_update_launcher "$probe" "$env_file" "$log" update
    assert_eq "update mode: the updater's exit 4 (not newer) is the launcher's exit status" "4" "$?"
    rm -f "$stamp"
    STUB_PROBE_EXIT=5 run_update_launcher "$probe" "$env_file" "$log" update
    assert_eq "update mode: the updater's exit 5 (recovered) is the launcher's exit status" "5" "$?"
    # ...and a failed attempt is still an attempt: no retry inside the interval.
    run_update_launcher "$probe" "$env_file" "$log" update
    [ -e "$ran" ] && fail "update mode: a failed attempt is not retried inside the interval" "the probe ran" \
        || pass "update mode: a failed attempt is not retried inside the interval"

    # ---- the fourth argument is an allow-list of one word ----
    rm -f "$stamp"
    run_update_launcher "$probe" "$env_file" "$log" rollback
    assert_eq "the launcher refuses a fourth argument that is not 'update'" "2" "$?"
    [ -e "$ran" ] && fail "a refused fourth argument runs nothing" "the probe ran" || pass "a refused fourth argument runs nothing"
    [ -e "$stamp" ] && fail "a refused fourth argument stamps nothing" "$stamp exists" || pass "a refused fourth argument stamps nothing"
    run_update_launcher "$probe" "$env_file" "$log" update extra
    assert_eq "the launcher refuses a fifth argument" "2" "$?"
    run_update_launcher "$probe" "$TMP/no-such.env" "$log" update
    assert_eq "update mode: a missing env file is refused before the guard" "1" "$?"
    [ -e "$stamp" ] && fail "update mode: a missing env file stamps nothing" "$stamp exists" || pass "update mode: a missing env file stamps nothing"

    # ---- the metrics path is untouched by any of this ----
    : > "$STUB_SYSCTL_ARGV"
    rm -f "$stamp"
    cat > "$probe" <<STUB
#!/bin/sh
printf 'ARGS=%s\\nTOKEN=%s\\n' "\$*" "\${SOLADOR_AGENT_TOKEN:-<unset>}" > "$ran"
STUB
    run_update_launcher "$probe" "$env_file" "$log"
    assert_eq "the metrics path (three arguments) still runs the binary" "0" "$?"
    assert_eq "the metrics path still exports the env file and passes no argument" \
        "$(printf 'ARGS=\nTOKEN=upd-tok-MUST-NOT-BE-EXPORTED')" "$(cat "$ran" 2>/dev/null)"
    [ -s "$STUB_SYSCTL_ARGV" ] && fail "the metrics path never reads the wake or boot time" "$(cat "$STUB_SYSCTL_ARGV")" \
        || pass "the metrics path never reads the wake or boot time"
    [ -e "$stamp" ] && fail "the metrics path never writes the update stamp" "$stamp exists" \
        || pass "the metrics path never writes the update stamp"

    unset STUB_DATE_EPOCH STUB_WAKETIME_SEC STUB_BOOTTIME_SEC STUB_SYSCTL_ARGV
}

# A REAL launchd bootstrap, opt-in: the whole installer against a temporary
# HOME, a throwaway label, the real launchctl, the real plutil, the real
# launcher, and the real curl for the health probe — with the download curl
# still stubbed to serve the locally built agent (signed with the throwaway
# key). This is the platform smoke #392 asks for, kept out of the default run
# because it bootstraps a service into the invoking user's session.
#
# Since #394 it continues into the updater: the same install re-run with
# --enable-timer, the sibling job observed loaded and NOT run, fired by hand
# once (a read-only `update` against the real feed under the throwaway
# label, which finds nothing newer or no network — never a swap), fired a
# second time to watch the guard discard it, then removed with the
# documented commands while the metrics service keeps serving.
test_launchd_smoke() {
    local name="install.sh bootstraps a real LaunchAgent (SOLADOR_DEPLOY_TEST_LAUNCHD=1)"
    if [ "${SOLADOR_DEPLOY_TEST_LAUNCHD:-}" != "1" ]; then
        skip "$name" "opt-in; set SOLADOR_DEPLOY_TEST_LAUNCHD=1 on macOS"
        return
    fi
    if [ "$(uname -s)" != "Darwin" ]; then
        skip "$name" "macOS only"
        return
    fi
    if [ "$HAVE_MINISIGN" != true ]; then
        skip_needs_minisign "$name"
        return
    fi
    if ! launchctl print "gui/$(id -u)" >/dev/null 2>&1; then
        skip "$name" "no gui launchd domain for this user"
        return
    fi
    local td real=""
    td="$(target_dir "$SCRIPT_DIR/.." 2>/dev/null)"
    for profile in release debug; do
        if [ -n "$td" ] && [ -x "$td/$profile/solador-agent" ]; then
            real="$td/$profile/solador-agent"
            break
        fi
    done
    if [ -z "$real" ]; then
        skip "$name" "no solador-agent built under ${td:-<no target dir>} (cargo build -p solador-agent)"
        return
    fi
    local version
    if ! version="$(binary_version "$real" 2>/dev/null)"; then
        skip "$name" "$real carries no version (shallow checkout)"
        return
    fi
    make_checkout "$TEST_KEY_DIR/a.pub"

    local home="$TMP/home launchd" label="app.solador.agent.deploytest.$$" triple f port=17878
    local service
    service="gui/$(id -u)/$label"
    rm -rf "$home" "$FIXTURES"
    mkdir -p "$home" "$FIXTURES"
    triple="$(agent_target_for "$(uname -s)" "$(uname -m)")"
    f="$FIXTURES/$(agent_asset_name "$version" "$triple")"
    cp "$real" "$f"
    sign_fixture "$f" "$TEST_KEY_DIR/a.key"

    # Real curl for the health probe; the stub only for the download.
    local smoke_stubs="$TMP/stubs-launchd"
    mkdir -p "$smoke_stubs"
    cat > "$smoke_stubs/curl" <<STUB
#!/usr/bin/env bash
case " \$* " in
    *" -o "*) exec "$STUBS/curl" "\$@" ;;
    *) exec "$(command -v curl)" "\$@" ;;
esac
STUB
    chmod +x "$smoke_stubs/curl"

    # smoke_install [args...]: the installer for real under the temp HOME.
    smoke_install() {
        (
            export HOME="$home"
            # The real uname, sw_vers, launchctl, plutil and sleep, behind the
            # download stub: this is the one test where the host is the point.
            export PATH="$smoke_stubs:$TOOLBIN:/usr/bin:/bin:/usr/sbin:/sbin"
            export STUB_CURL_FIXTURES="$FIXTURES"
            export SOLADOR_AGENT_RELEASE="v$version"
            export SOLADOR_AGENT_LAUNCHD_LABEL="$label"
            export SOLADOR_AGENT_BIND="127.0.0.1"
            export SOLADOR_AGENT_PORT="$port"
            printf '%s' "$INSTALL_STDIN" | "$BASH" "$CHECKOUT/agent/deploy/install.sh" "$@"
        ) >"$INSTALL_OUT" 2>&1
    }
    # launchd_field <service> <field>: one `field = value` line of launchctl print.
    launchd_field() {
        launchctl print "$1" 2>/dev/null | sed -n "s/^[[:space:]]*$2 = //p" | head -n1
    }

    local status out update_service update_plist update_log stamp
    update_service="$service.update"
    update_plist="$home/Library/LaunchAgents/$label.update.plist"
    update_log="$home/Library/Logs/solador-agent-update.log"
    stamp="$home/.config/solador-agent-update.last-attempt"
    SMOKE_SERVICES="$service $update_service"
    INSTALL_STDIN='smoke-tok-MUST-NOT-BE-PRINTED
' smoke_install
    status=$?
    out="$(cat "$INSTALL_OUT")"

    assert_eq "$name" "0" "$status"
    if launchctl print "$service" >/dev/null 2>&1; then
        pass "the real LaunchAgent was loaded in gui/$(id -u) after install"
    else
        fail "the real LaunchAgent was loaded in gui/$(id -u) after install" "$out"
    fi
    assert_output_has "the real agent served its own version over an authenticated /v1/health" \
        "$out" "Health OK: agent reports version $version"
    case "$out" in
        *"smoke-tok-MUST-NOT-BE-PRINTED"*) fail "the launchd smoke never prints the token" "it did" ;;
        *) pass "the launchd smoke never prints the token" ;;
    esac
    [ -f "$home/Library/Logs/solador-agent.log" ] \
        && pass "the agent's log landed under ~/Library/Logs" \
        || fail "the agent's log landed under ~/Library/Logs" "no log file"
    # A default install loads no updater — observed from launchd, not the script.
    if launchctl print "$update_service" >/dev/null 2>&1; then
        fail "a default install loads no updater LaunchAgent" "$update_service is loaded"
    else
        pass "a default install loads no updater LaunchAgent"
    fi

    # ---- opt in, for real ----
    INSTALL_STDIN='' smoke_install --enable-timer
    status=$?
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh --enable-timer bootstraps a real updater LaunchAgent" "0" "$status"
    # The opt-in re-run re-bootstraps the metrics service like any re-run
    # (a new pid); from here on that pid must not change again.
    local metrics_pid
    metrics_pid="$(launchd_field "$service" pid)"
    if launchctl print "$update_service" >/dev/null 2>&1; then
        pass "the updater is loaded in gui/$(id -u) as the metrics label's .update sibling"
    else
        fail "the updater is loaded in gui/$(id -u) as the metrics label's .update sibling" "$out"
    fi
    # Loaded is not run: launchd's own counters say it has never started.
    assert_eq "the updater has not run on load (launchd reports zero runs)" "0" "$(launchd_field "$update_service" runs)"
    [ -e "$stamp" ] && fail "no attempt is stamped on enable" "$stamp exists" || pass "no attempt is stamped on enable"

    # ---- fire it by hand: the launcher runs the real `solador-agent update` ----
    # A read-only run: the throwaway install holds this checkout's own build,
    # which the published feed is never newer than (exit 4), or the host has
    # no route to github.com (exit 1). Neither swaps a binary or restarts the
    # metrics service, and that is asserted rather than assumed.
    #
    # The real guard applies to this firing too: within five minutes of the
    # Mac's last wake it would discard the check (exit 0), which is the
    # guard working, not the updater. Wait that window out first.
    local woke_at settle
    woke_at="$(sysctl -n kern.waketime 2>/dev/null | sed -n 's/^{ sec = \([0-9][0-9]*\),.*/\1/p')"
    if [ -n "$woke_at" ]; then
        settle=$((300 - ($(date +%s) - woke_at)))
        if [ "$settle" -gt 0 ]; then
            printf 'note  the Mac woke %ss ago; waiting %ss for the guard'"'"'s wake-settle window\n' "$((300 - settle))" "$settle"
            /bin/sleep "$settle"
        fi
    fi
    launchctl kickstart "$update_service" >/dev/null 2>&1
    local waited=0 exit_code=""
    while [ "$waited" -lt 90 ]; do
        exit_code="$(launchd_field "$update_service" "last exit code")"
        case "$exit_code" in
            "" | "(never exited)") ;;
            *) break ;;
        esac
        /bin/sleep 1
        waited=$((waited + 1))
    done
    case "$exit_code" in
        1 | 4) pass "a hand-fired updater ran 'solador-agent update' and exited $exit_code (not newer, or no network)" ;;
        "" | "(never exited)") fail "a hand-fired updater ran and exited" "no exit after ${waited}s: $(tail -n 20 "$update_log" 2>/dev/null)" ;;
        *) fail "a hand-fired updater exited 1 or 4, never a swap" "exit $exit_code: $(tail -n 20 "$update_log" 2>/dev/null)" ;;
    esac
    assert_file_has "the launcher logged the update-mode run to the updater's own log" \
        "$update_log" "solador-agent-launchd: update: running $home/.local/bin/solador-agent update"
    if grep -qE '^(==> Latest published release:|ERROR:)' "$update_log" 2>/dev/null; then
        pass "the real updater resolved the throwaway install and reached its first report line"
    else
        fail "the real updater resolved the throwaway install and reached its first report line" "$(tail -n 20 "$update_log" 2>/dev/null)"
    fi
    if grep -q "smoke-tok-MUST-NOT-BE-PRINTED" "$update_log" 2>/dev/null; then
        fail "the updater's log never carries the token" "it does"
    else
        pass "the updater's log never carries the token"
    fi
    [ -f "$stamp" ] && pass "the hand-fired attempt was stamped" || fail "the hand-fired attempt was stamped" "no $stamp"
    [ -e "$home/.local/bin/solador-agent.prev" ] && fail "the read-only run minted no .prev" "$home/.local/bin/solador-agent.prev exists" \
        || pass "the read-only run minted no .prev"
    assert_eq "the metrics service was not restarted by the read-only run" "$metrics_pid" "$(launchd_field "$service" pid)"

    # ---- fire it again: the guard discards a second firing inside the interval ----
    # launchd counts the run at the kickstart and spawns it after its 10 s
    # throttle (measured on a throwaway job), so "it ran" is the launcher's
    # own line landing in the log, not the counter moving.
    local lines_before
    lines_before="$(grep -c 'solador-agent-launchd: update:' "$update_log" 2>/dev/null || echo 0)"
    launchctl kickstart "$update_service" >/dev/null 2>&1
    waited=0
    while [ "$waited" -lt 40 ] \
        && [ "$(grep -c 'solador-agent-launchd: update:' "$update_log" 2>/dev/null || echo 0)" = "$lines_before" ]; do
        /bin/sleep 1
        waited=$((waited + 1))
    done
    /bin/sleep 1
    waited=0
    while [ "$waited" -lt 30 ] && [ "$(launchd_field "$update_service" state)" = "running" ]; do
        /bin/sleep 1
        waited=$((waited + 1))
    done
    if grep -q "not running this check" "$update_log" 2>/dev/null; then
        pass "a second firing inside the interval is discarded by the launcher's guard"
    else
        fail "a second firing inside the interval is discarded by the launcher's guard" \
            "launcher lines before/after: $lines_before/$(grep -c 'solador-agent-launchd: update:' "$update_log" 2>/dev/null)" \
            "$(tail -n 12 "$update_log" 2>/dev/null)"
    fi
    assert_eq "the discarded firing did not run the updater (one 'running' line, not two)" \
        "1" "$(grep -c 'solador-agent-launchd: update: running' "$update_log" 2>/dev/null)"
    assert_eq "the discarded firing exits 0" "0" "$(launchd_field "$update_service" "last exit code")"
    assert_eq "the metrics service was not restarted by the discarded firing" "$metrics_pid" "$(launchd_field "$service" pid)"

    # ---- the documented removal: only the updater goes ----
    launchctl bootout "$update_service" >/dev/null 2>&1 || true
    rm -f "$update_plist"
    if launchctl print "$update_service" >/dev/null 2>&1; then
        fail "the documented removal unloads the updater" "$update_service is still loaded"
    else
        pass "the documented removal unloads the updater"
    fi
    if launchctl print "$service" >/dev/null 2>&1; then
        pass "removing the updater leaves the metrics service loaded"
    else
        fail "removing the updater leaves the metrics service loaded" "$service is gone"
    fi
    assert_eq "removing the updater leaves the metrics service running, same pid" "$metrics_pid" "$(launchd_field "$service" pid)"

    # Whatever happened, take the throwaway services down again.
    launchctl bootout "$update_service" >/dev/null 2>&1 || true
    launchctl bootout "$service" >/dev/null 2>&1 || true
    if launchctl print "$service" >/dev/null 2>&1; then
        fail "the throwaway service was unloaded again" "$service is still loaded — launchctl bootout $service"
    else
        pass "the throwaway service was unloaded again"
    fi
    if launchctl print "$update_service" >/dev/null 2>&1; then
        fail "the throwaway updater was unloaded again" "$update_service is still loaded — launchctl bootout $update_service"
    else
        pass "the throwaway updater was unloaded again"
    fi
}

# ---- scripts/agent-standby-key.sh (#393 §A) -----------------------------------
#
# The custody script, run for real against a copy of the checkout with
# `doppler`, `gh` and `cargo` stubbed (a file-backed secret store, fixed name
# lists, a recorder) and `rsign` stubbed as a thin wrapper over the REAL
# `minisign` — so the keypair generated, the value "uploaded", the value
# "retrieved" and the custody proof are real cryptography, and only the
# network is fake. No production key is anywhere near this; every key is
# minted into the temp dir and dies with it. What is asserted is what the
# script promises: nothing secret on argv, in the output or left on disk;
# a re-run that reuses rather than rotates; every half-state and every
# misplacement refused before anything changes.

STANDBY_CHECKOUT="$TMP/standby-checkout"
STANDBY_STORE="$TMP/doppler-store"
STANDBY_OUT="$TMP/standby.out"
STANDBY_TMPDIR="$TMP/standby-tmpdir"

make_standby_stubs() {
    local dir="$TMP/stubs-standby"
    rm -rf "$dir"
    mkdir -p "$dir"

    # rsign: the pinned signer's three verbs, translated onto the stock
    # minisign. `--version` answers the pin so require_rsign passes.
    cat > "$dir/rsign" <<'STUB'
#!/usr/bin/env bash
if [ -n "${STUB_RSIGN_ARGV:-}" ]; then
    printf '%s\n' "$*" >> "$STUB_RSIGN_ARGV"
fi
case "${1:-}" in
    --version) printf 'rsign2 %s\n' "${RSIGN_VERSION:-0.0.0}"; exit 0 ;;
esac
verb="$1"; shift
pub=""; sec=""; sig=""; tc=""; uc=""; file=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        -p) pub="$2"; shift ;;
        -s) sec="$2"; shift ;;
        -x) sig="$2"; shift ;;
        -t) tc="$2"; shift ;;
        -c) uc="$2"; shift ;;
        -W | -f | -q) ;;
        *) file="$1" ;;
    esac
    shift
done
case "$verb" in
    generate) exec minisign -G -W -f -p "$pub" -s "$sec" -c "$uc" ;;
    sign) exec minisign -S -W -s "$sec" -x "$sig" -t "$tc" -c "$uc" -m "$file" ;;
    verify) exec minisign -Vq -p "$pub" -x "$sig" -m "$file" ;;
    *) echo "rsign stub: unknown verb $verb" >&2; exit 2 ;;
esac
STUB

    # doppler: a directory per config under STUB_DOPPLER_STORE, a file per
    # secret. Only the invocations the script makes are understood; anything
    # else is a failure, so a new call shape cannot pass unnoticed.
    cat > "$dir/doppler" <<'STUB'
#!/usr/bin/env bash
if [ -n "${STUB_DOPPLER_ARGV:-}" ]; then
    printf '%s\n' "$*" >> "$STUB_DOPPLER_ARGV"
fi
store="${STUB_DOPPLER_STORE:?}"
group="$1"; verb="${2:-}"
project=""; config=""; name=""; json=false; only_names=false; plain=false; page=1
args=("$@")
i=0
for a in "${args[@]}"; do
    case "$a" in
        --project) project="${args[$((i + 1))]}" ;;
        --config) config="${args[$((i + 1))]}" ;;
        --page) page="${args[$((i + 1))]}" ;;
        --json) json=true ;;
        --only-names) only_names=true ;;
        --plain) plain=true ;;
    esac
    i=$((i + 1))
done
case "$group $verb" in
    "configs get")
        [ -d "$store/$3" ] || exit 1
        printf '{"name":"%s","project":"%s"}\n' "$3" "$project"
        ;;
    "configs logs")
        # STUB_DOPPLER_LOGS_EXIT: an unreadable log. Pages: .logs.json is
        # page 1, .logs.pageN.json page N, anything else is empty.
        [ "${STUB_DOPPLER_LOGS_EXIT:-0}" = 0 ] || exit "$STUB_DOPPLER_LOGS_EXIT"
        if [ "$page" = 1 ]; then f="$store/$config/.logs.json"; else f="$store/$config/.logs.page$page.json"; fi
        if [ -f "$f" ]; then cat "$f"; else echo '[]'; fi
        ;;
    "secrets set")
        name="$3"
        [ -d "$store/$config" ] || exit 1
        cat > "$store/$config/$name"
        ;;
    "secrets get")
        name="$3"
        if [ -n "${STUB_DOPPLER_GET_OVERRIDE:-}" ]; then cat "$STUB_DOPPLER_GET_OVERRIDE"; exit 0; fi
        [ -f "$store/$config/$name" ] || exit 1
        cat "$store/$config/$name"
        ;;
    "secrets "*|"secrets")
        [ "$only_names" = true ] && [ "$json" = true ] || exit 2
        [ -d "$store/$config" ] || exit 1
        printf '{'
        first=true
        for f in "$store/$config"/*; do
            [ -f "$f" ] || continue
            [ "$first" = true ] || printf ','
            printf '"%s":{}' "$(basename "$f")"
            first=false
        done
        printf '}\n'
        ;;
    *) echo "doppler stub: unsupported: $*" >&2; exit 2 ;;
esac
STUB

    # gh: secret-name lists for the three scopes the script checks. It models
    # GitHub's paging the way the real API does it — 30 names a page,
    # alphabetical — so a listing asked for WITHOUT `--paginate` is the first
    # thirty and nothing else. That is the shape the round-2 review measured
    # on the real organisation (64 secrets), and the case that asserts it is
    # what keeps `--paginate` on every call.
    cat > "$dir/gh" <<'STUB'
#!/usr/bin/env bash
if [ -n "${STUB_GH_ARGV:-}" ]; then
    printf '%s\n' "$*" >> "$STUB_GH_ARGV"
fi
paginate=false
case " $* " in *" --paginate "*) paginate=true ;; esac
page() {
    # Sorted, then the first page only unless --paginate was given.
    if [ "$paginate" = true ]; then sort; else sort | head -n 30; fi
}
case "$*" in
    *"/environments/prd/secrets"*)
        [ "${STUB_GH_PRD_EXIT:-0}" = 0 ] || exit "$STUB_GH_PRD_EXIT"
        [ -n "${STUB_GH_PRD_NAMES-SOLADOR_AGENT_SIGNING_PRIVATE_KEY}" ] && printf '%s\n' "${STUB_GH_PRD_NAMES-SOLADOR_AGENT_SIGNING_PRIVATE_KEY}" | page; exit 0 ;;
    *"orgs/"*"/actions/secrets"*)
        [ "${STUB_GH_ORG_EXIT:-0}" = 0 ] || exit "$STUB_GH_ORG_EXIT"
        [ -n "${STUB_GH_ORG_NAMES:-}" ] && printf '%s\n' "$STUB_GH_ORG_NAMES" | page; exit 0 ;;
    *"/actions/secrets"*)
        [ "${STUB_GH_REPO_EXIT:-0}" = 0 ] || exit "$STUB_GH_REPO_EXIT"
        [ -n "${STUB_GH_REPO_NAMES:-}" ] && printf '%s\n' "$STUB_GH_REPO_NAMES" | page; exit 0 ;;
    *) exit 1 ;;
esac
STUB

    # cargo: records its argv and, because a cargo invocation runs
    # third-party build scripts, whether any private key was still on disk
    # under TMPDIR when it ran.
    cat > "$dir/cargo" <<'STUB'
#!/usr/bin/env bash
if [ -n "${STUB_CARGO_ARGV:-}" ]; then
    printf '%s\n' "$*" >> "$STUB_CARGO_ARGV"
    if ls "${TMPDIR:-/nonexistent}"/*/*.key >/dev/null 2>&1; then
        printf 'KEYS_STILL_ON_DISK\n' >> "$STUB_CARGO_ARGV"
    fi
fi
exit "${STUB_CARGO_EXIT:-0}"
STUB
    chmod +x "$dir"/*
    printf '%s\n' "$dir"
}

# A copy of the four scripts the custody script needs, plus the throwaway
# "current" key as agent/release-signing-key.pub.
make_standby_checkout() {
    local current_pub="$1"
    rm -rf "$STANDBY_CHECKOUT"
    mkdir -p "$STANDBY_CHECKOUT/scripts" "$STANDBY_CHECKOUT/agent"
    cp "$SCRIPT_DIR/../../scripts/agent-standby-key.sh" "$SCRIPT_DIR/../../scripts/agent-signing.sh" \
       "$SCRIPT_DIR/../../scripts/lib.sh" "$SCRIPT_DIR/../../scripts/config.sh" "$STANDBY_CHECKOUT/scripts/"
    cp "$current_pub" "$STANDBY_CHECKOUT/agent/release-signing-key.pub"
}

# run_standby [args...]: the script under stubs, output (both streams) to
# STANDBY_OUT, status in STANDBY_STATUS. TMPDIR is a fresh directory so the
# "every temp copy removed" assertion has something to look at.
STANDBY_STATUS=""
run_standby() {
    rm -rf "$STANDBY_TMPDIR"
    mkdir -p "$STANDBY_TMPDIR"
    (
        export PATH="$STANDBY_STUBS:$TOOLBIN"
        export TMPDIR="$STANDBY_TMPDIR"
        export STUB_DOPPLER_STORE="$STANDBY_STORE"
        export STUB_DOPPLER_ARGV="$TMP/doppler-argv"
        export STUB_RSIGN_ARGV="$TMP/rsign-argv"
        export STUB_GH_ARGV="$TMP/gh-argv"
        export STUB_CARGO_ARGV="$TMP/cargo-argv"
        unset DEBUG
        cd "$TMP" || exit 99
        "$BASH" "$STANDBY_CHECKOUT/scripts/agent-standby-key.sh" "$@"
    ) >"$STANDBY_OUT" 2>&1
    STANDBY_STATUS=$?
    : > "$TMP/.standby-ran"
    return "$STANDBY_STATUS"
}

reset_standby_logs() {
    : > "$TMP/doppler-argv"
    : > "$TMP/rsign-argv"
    : > "$TMP/gh-argv"
    : > "$TMP/cargo-argv"
}

test_standby_key_script() {
    local name="agent-standby-key.sh"
    if [ "$HAVE_MINISIGN" != true ]; then
        skip_needs_minisign "$name provisions, uploads and proves custody (real minisign behind an rsign stub)"
        skip_needs_minisign "$name re-run reuses the standby rather than rotating it"
        skip_needs_minisign "$name refuses every half-state and misplacement without changing anything"
        return
    fi
    STANDBY_STUBS="$(make_standby_stubs)"
    local next_pub="$STANDBY_CHECKOUT/agent/release-signing-key-next.pub"
    local secret="$STANDBY_STORE/custody/SOLADOR_AGENT_SIGNING_STANDBY_PRIVATE_KEY"
    local out

    # ---- fresh run -----------------------------------------------------------
    make_standby_checkout "$TEST_KEY_DIR/a.pub"
    rm -rf "$STANDBY_STORE"
    mkdir -p "$STANDBY_STORE/custody" "$STANDBY_STORE/prd"
    : > "$STANDBY_STORE/prd/SOLADOR_AGENT_SIGNING_PRIVATE_KEY"
    reset_standby_logs
    run_standby
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: a fresh run exits 0" "0" "$STANDBY_STATUS"
    assert_output_has "$name: generated with the pinned signer" "$out" "generating a fresh keypair"
    assert_output_has "$name: wrote the public half" "$out" "Wrote agent/release-signing-key-next.pub"
    assert_output_has "$name: uploaded the private half" "$out" "Uploaded SOLADOR_AGENT_SIGNING_STANDBY_PRIVATE_KEY"
    assert_output_has "$name: proved custody with the RETRIEVED value" "$out" "custody proven: a signature made with the RETRIEVED standby"
    assert_output_has "$name: showed the identities are distinct" "$out" "does NOT verify under the current key"
    assert_output_has "$name: showed an unrelated key is rejected" "$out" "unrelated key's signature is rejected"
    assert_output_has "$name: checked the GitHub prd environment by name" "$out" "GitHub prd environment: SOLADOR_AGENT_SIGNING_PRIVATE_KEY present, SOLADOR_AGENT_SIGNING_STANDBY_PRIVATE_KEY absent"
    assert_output_has "$name: ran the updater's trust-set test" "$out" "as two distinct trusted keys"
    if [ -f "$next_pub" ] && [ "$(grep -c '' "$next_pub")" = 2 ] \
        && sed -n '1p' "$next_pub" | grep -qE '^untrusted comment: minisign public key: [0-9A-F]{16} — Solador agent release signing STANDBY key'; then
        pass "$name: the public file has the committed shape (two lines, id on line 1)"
    else
        fail "$name: the public file has the committed shape (two lines, id on line 1)" "$(cat "$next_pub" 2>/dev/null)"
    fi
    if [ "$(sed -n '2p' "$next_pub")" != "$(sed -n '2p' "$TEST_KEY_DIR/a.pub")" ]; then
        pass "$name: the standby is not the current key"
    else
        fail "$name: the standby is not the current key"
    fi
    if [ -f "$secret" ] && [ "$(grep -c '' "$secret")" -ge 2 ] && head -n1 "$secret" | grep -q '^untrusted comment:'; then
        pass "$name: the store holds a minisign secret key"
    else
        fail "$name: the store holds a minisign secret key"
    fi
    # The independent check: the stored private key signs, the committed
    # public file verifies — with nothing from the script in between.
    printf 'independent\n' > "$TMP/standby-indep"
    if minisign -S -W -s "$secret" -m "$TMP/standby-indep" -x "$TMP/standby-indep.minisig" </dev/null >/dev/null 2>&1 \
        && minisign -Vq -m "$TMP/standby-indep" -x "$TMP/standby-indep.minisig" -p "$next_pub" \
        && ! minisign -Vq -m "$TMP/standby-indep" -x "$TMP/standby-indep.minisig" -p "$TEST_KEY_DIR/a.pub" 2>/dev/null; then
        pass "$name: the stored private half and the committed public half are a pair (and not the current key)"
    else
        fail "$name: the stored private half and the committed public half are a pair (and not the current key)"
    fi
    local key_line
    key_line="$(sed -n '2p' "$secret")"
    if [ -n "$key_line" ] && ! grep -qF -- "$key_line" "$STANDBY_OUT" "$TMP/doppler-argv" "$TMP/rsign-argv" "$TMP/gh-argv" "$TMP/cargo-argv"; then
        pass "$name: the private key is in no output and on no argv"
    else
        fail "$name: the private key is in no output and on no argv"
    fi
    if grep -q '^secrets set SOLADOR_AGENT_SIGNING_STANDBY_PRIVATE_KEY --project solador --config custody --silent$' "$TMP/doppler-argv"; then
        pass "$name: the upload named the secret and the custody config on argv, and nothing else"
    else
        fail "$name: the upload named the secret and the custody config on argv, and nothing else" "$(cat "$TMP/doppler-argv")"
    fi
    if [ -z "$(ls -A "$STANDBY_TMPDIR")" ]; then
        pass "$name: every temporary copy was removed"
    else
        fail "$name: every temporary copy was removed" "$(ls -A "$STANDBY_TMPDIR")"
    fi
    assert_file_has "$name: the trust-set test was the one asked for" "$TMP/cargo-argv" \
        "the_compiled_in_trust_set_is_the_committed_key_files_and_they_are_distinct"
    if grep -q 'KEYS_STILL_ON_DISK' "$TMP/cargo-argv"; then
        fail "$name: no private key is on disk when cargo runs" "a .key under TMPDIR outlived the custody proof"
    else
        pass "$name: no private key is on disk when cargo runs"
    fi
    assert_output_has "$name: checked the organisation's secrets too" "$out" "GitHub organisation secrets: SOLADOR_AGENT_SIGNING_STANDBY_PRIVATE_KEY absent"

    # ---- re-run: reuse, never rotate --------------------------------------------
    local before after
    before="$(cat "$secret")"
    reset_standby_logs
    run_standby
    out="$(cat "$STANDBY_OUT")"
    after="$(cat "$secret")"
    assert_eq "$name: a re-run exits 0" "0" "$STANDBY_STATUS"
    assert_output_has "$name: a re-run reuses the existing standby" "$out" "already exists"
    assert_eq "$name: a re-run leaves the stored key as it was" "$before" "$after"
    if grep -q 'next\.key' "$TMP/rsign-argv"; then
        fail "$name: a re-run generates no standby" "$(grep 'next\.key' "$TMP/rsign-argv")"
    else
        pass "$name: a re-run generates no standby"
    fi
    if grep -q '^secrets set' "$TMP/doppler-argv"; then
        fail "$name: a re-run uploads nothing"
    else
        pass "$name: a re-run uploads nothing"
    fi
    assert_output_has "$name: a re-run still proves custody" "$out" "custody proven"

    # ---- half-states: refused, nothing changed --------------------------------
    local kept="$TMP/standby-next.pub.kept"
    cp "$next_pub" "$kept"
    rm -f "$next_pub"
    reset_standby_logs
    run_standby
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: secret without public file is refused" "1" "$STANDBY_STATUS"
    assert_output_has "$name: …and names the recovery" "$out" "refusing to guess"
    [ -f "$next_pub" ] && fail "$name: …and writes no public file" || pass "$name: …and writes no public file"
    assert_eq "$name: …and leaves the stored key alone" "$before" "$(cat "$secret")"

    cp "$kept" "$next_pub"
    mv "$secret" "$TMP/standby-secret.kept"
    reset_standby_logs
    run_standby
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: public file without secret is refused" "1" "$STANDBY_STATUS"
    assert_output_has "$name: …as worthless" "$out" "worthless"
    grep -q '^secrets set' "$TMP/doppler-argv" && fail "$name: …and uploads nothing" || pass "$name: …and uploads nothing"
    mv "$TMP/standby-secret.kept" "$secret"

    # ---- a synced config is refused before generation -----------------------------
    make_standby_checkout "$TEST_KEY_DIR/a.pub"
    rm -rf "$STANDBY_STORE"
    mkdir -p "$STANDBY_STORE/custody"
    printf '[{"text":"Added GitHub, Actions: Sassy-Dog / solador / custody integration","created_at":"2026-09-12T00:00:00Z"},{"text":"Removed GitHub, Actions: old integration","created_at":"2026-09-01T00:00:00Z"}]\n' \
        > "$STANDBY_STORE/custody/.logs.json"
    reset_standby_logs
    run_standby
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: a config with an active sync is refused" "1" "$STANDBY_STATUS"
    assert_output_has "$name: …naming the sync" "$out" "has an active sync"
    [ -f "$next_pub" ] && fail "$name: …before any key is generated" || pass "$name: …before any key is generated"
    grep -q 'generate' "$TMP/rsign-argv" && fail "$name: …rsign was not asked to generate" || pass "$name: …rsign was not asked to generate"
    # The same log with the sync since removed (newest first) is fine.
    printf '[{"text":"Removed GitHub, Actions: Sassy-Dog / solador / custody integration","created_at":"2026-09-12T00:00:00Z"},{"text":"Added GitHub, Actions: Sassy-Dog / solador / custody integration","created_at":"2026-09-01T00:00:00Z"}]\n' \
        > "$STANDBY_STORE/custody/.logs.json"
    reset_standby_logs
    run_standby
    assert_eq "$name: a config whose sync was removed is accepted" "0" "$STANDBY_STATUS"

    # The gate walks EVERY page: an integration event older than the first
    # page of secret edits is still the newest integration event.
    make_standby_checkout "$TEST_KEY_DIR/a.pub"
    rm -rf "$STANDBY_STORE"
    mkdir -p "$STANDBY_STORE/custody"
    {
        printf '['
        for i in $(seq 1 25); do
            [ "$i" -gt 1 ] && printf ','
            printf '{"text":"Updated secret NOISE_%s","created_at":"2026-09-12T00:00:%02dZ"}' "$i" "$((i % 60))"
        done
        printf ']\n'
    } > "$STANDBY_STORE/custody/.logs.json"
    printf '[{"text":"Added GitHub, Actions: Sassy-Dog / solador / custody integration","created_at":"2026-09-01T00:00:00Z"}]\n' \
        > "$STANDBY_STORE/custody/.logs.page2.json"
    reset_standby_logs
    run_standby
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: an Added integration event on the second log page is still refused" "1" "$STANDBY_STATUS"
    assert_output_has "$name: …naming the sync" "$out" "has an active sync"
    grep -q '^secrets set' "$TMP/doppler-argv" && fail "$name: …before any upload" || pass "$name: …before any upload"
    [ -f "$next_pub" ] && fail "$name: …and before any key" || pass "$name: …and before any key"

    # A newest integration event with a verb the script does not classify
    # is refused, not read as clean: the verdict is a positive list.
    printf '[{"text":"Updated GitHub, Actions: Sassy-Dog / solador / custody integration","created_at":"2026-09-12T00:00:00Z"}]\n' \
        > "$STANDBY_STORE/custody/.logs.json"
    rm -f "$STANDBY_STORE/custody/.logs.page2.json"
    reset_standby_logs
    run_standby
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: an integration event the script does not classify is refused" "1" "$STANDBY_STATUS"
    assert_output_has "$name: …and printed" "$out" "does not classify"
    grep -q 'generate' "$TMP/rsign-argv" && fail "$name: …before any key" || pass "$name: …before any key"

    # A log that cannot be read is a refusal, never "no sync".
    rm -f "$STANDBY_STORE/custody/.logs.json" "$STANDBY_STORE/custody/.logs.page2.json"
    reset_standby_logs
    STUB_DOPPLER_LOGS_EXIT=1 run_standby
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: an unreadable audit log is refused" "1" "$STANDBY_STATUS"
    assert_output_has "$name: …rather than assumed clean" "$out" "refusing to assume it syncs nowhere"
    grep -q '^secrets set' "$TMP/doppler-argv" && fail "$name: …with no upload" || pass "$name: …with no upload"

    # ---- prd, a missing config, a misplaced secret, a failed proof --------------
    make_standby_checkout "$TEST_KEY_DIR/a.pub"
    rm -rf "$STANDBY_STORE"
    mkdir -p "$STANDBY_STORE/custody" "$STANDBY_STORE/prd"
    reset_standby_logs
    run_standby --config prd
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: --config prd is refused" "1" "$STANDBY_STATUS"
    assert_output_has "$name: …because prd holds the ACTIVE key" "$out" "holds the ACTIVE key"

    reset_standby_logs
    run_standby --config nowhere
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: a missing config is refused" "1" "$STANDBY_STATUS"
    assert_output_has "$name: …with the command that creates it" "$out" "doppler environments create"
    grep -q 'generate' "$TMP/rsign-argv" && fail "$name: …and generates nothing" || pass "$name: …and generates nothing"

    reset_standby_logs
    STUB_GH_PRD_NAMES=$'SOLADOR_AGENT_SIGNING_PRIVATE_KEY\nSOLADOR_AGENT_SIGNING_STANDBY_PRIVATE_KEY' run_standby
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: the standby appearing in GitHub's prd environment is a failure" "1" "$STANDBY_STATUS"
    assert_output_has "$name: …that says so" "$out" "IS in Sassy-Dog/solador's prd environment"

    # A repository- or organisation-level copy, or a listing that cannot be
    # read, each stop the run after the upload (the check is post-upload by
    # nature — the sync is what would copy it — so the assertion is the exit
    # status and the message, not "no upload").
    make_standby_checkout "$TEST_KEY_DIR/a.pub"
    rm -rf "$STANDBY_STORE"
    mkdir -p "$STANDBY_STORE/custody" "$STANDBY_STORE/prd"
    reset_standby_logs
    STUB_GH_REPO_NAMES="SOLADOR_AGENT_SIGNING_STANDBY_PRIVATE_KEY" run_standby
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: the standby appearing as a repository secret is a failure" "1" "$STANDBY_STATUS"
    assert_output_has "$name: …that says so" "$out" "IS a repository Actions secret"
    reset_standby_logs
    STUB_GH_ORG_NAMES="SOLADOR_AGENT_SIGNING_STANDBY_PRIVATE_KEY" run_standby
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: the standby appearing as an organisation secret is a failure" "1" "$STANDBY_STATUS"
    assert_output_has "$name: …that says so" "$out" "IS an organisation Actions secret"
    reset_standby_logs
    STUB_GH_REPO_EXIT=1 run_standby
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: an unreadable repository secret listing is a failure" "1" "$STANDBY_STATUS"
    assert_output_has "$name: …not an absence" "$out" "refusing to assume the standby is absent"
    reset_standby_logs
    STUB_GH_ORG_EXIT=1 run_standby
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: an unreadable organisation secret listing is a failure" "1" "$STANDBY_STATUS"
    assert_output_has "$name: …not an absence" "$out" "refusing to assume the standby is absent"
    # The standby sorted PAST the first page of thirty: the real organisation
    # holds more than thirty secrets, and a listing that stops at page one
    # would print "absent" for the one name this scan exists to find.
    local filler=""
    for i in $(seq -w 1 30); do
        filler="${filler}AAAA_FILLER_${i}"$'\n'
    done
    reset_standby_logs
    STUB_GH_ORG_NAMES="${filler}SOLADOR_AGENT_SIGNING_STANDBY_PRIVATE_KEY" run_standby
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: the standby on the second page of the organisation listing is still found" "1" "$STANDBY_STATUS"
    assert_output_has "$name: …and refused" "$out" "IS an organisation Actions secret"
    if grep -q '^api ' "$TMP/gh-argv" && ! grep '^api ' "$TMP/gh-argv" | grep -qv -- '--paginate'; then
        pass "$name: every GitHub listing is paginated"
    else
        fail "$name: every GitHub listing is paginated" "$(grep '^api ' "$TMP/gh-argv" | grep -v -- '--paginate')"
    fi
    reset_standby_logs
    STUB_GH_PRD_NAMES="" run_standby
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: an EMPTY prd environment is a failure, not an absence" "1" "$STANDBY_STATUS"
    assert_output_has "$name: …that names the broken sync" "$out" "lists NO secrets"
    reset_standby_logs
    STUB_GH_PRD_EXIT=1 run_standby
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: an unreadable prd listing is a failure" "1" "$STANDBY_STATUS"
    assert_output_has "$name: …not an absence" "$out" "refusing to assume the standby is absent"
    reset_standby_logs
    STUB_CARGO_EXIT=1 run_standby
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: a failing trust-set test is a failure" "1" "$STANDBY_STATUS"
    assert_output_has "$name: …that says not to commit" "$out" "do not commit"

    make_standby_checkout "$TEST_KEY_DIR/a.pub"
    rm -rf "$STANDBY_STORE"
    mkdir -p "$STANDBY_STORE/custody" "$STANDBY_STORE/prd"
    reset_standby_logs
    STUB_DOPPLER_GET_OVERRIDE="$TEST_KEY_DIR/b.key" run_standby
    out="$(cat "$STANDBY_OUT")"
    assert_eq "$name: a retrieved value that is not the committed key's pair fails the proof" "1" "$STANDBY_STATUS"
    assert_output_has "$name: …loudly" "$out" "CUSTODY PROOF FAILED"
    if [ -z "$(ls -A "$STANDBY_TMPDIR")" ]; then
        pass "$name: temporary copies are removed on the failure path too"
    else
        fail "$name: temporary copies are removed on the failure path too" "$(ls -A "$STANDBY_TMPDIR")"
    fi
}

# ---- source-level invariants ------------------------------------------------
#
# These are not reachable at runtime without a real host, and each one breaks
# silently — the deploy keeps reporting success while doing the wrong thing.
# That is exactly the shape of failure this file exists to catch. (The
# install.sh invariants that used to live here — the legacy handover and its
# ordering — are observed at runtime by test_install_linux_flow since #392.)

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
        "$install_sh" 'env_value "$LEGACY_ENV_FILE" DEVCANOPY_AGENT_TOKEN'
    assert_file_has "install.sh still carries the pre-rename bind across" \
        "$install_sh" 'env_value "$LEGACY_ENV_FILE" DEVCANOPY_AGENT_BIND'

    # install.sh no longer builds (#392). If cargo comes back it is a source
    # build sneaking in as a fallback, which is exactly what the download path
    # must never do.
    if grep -qE '(^|[^a-z-])cargo( |$)' "$install_sh"; then
        fail "install.sh never invokes cargo" "a cargo invocation is back in install.sh"
    else
        pass "install.sh never invokes cargo"
    fi
    if grep -v '^[[:space:]]*#' "$install_sh" | grep -qE '(^[[:space:]]*|[;&|][[:space:]]*)sudo '; then
        fail "install.sh never invokes sudo" "a sudo invocation is in install.sh"
    else
        pass "install.sh never invokes sudo"
    fi

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

printf 'agent/deploy/lib.sh + install.sh\n\n'

test_binary_version
test_health_url
test_health_version
test_target_dir
test_build_release_binary
test_verify_health
test_agent_target_for
test_release_contract_matches_config
test_validate_release_tag
test_resolve_latest_release_tag
test_service_rendering
test_verify_agent_signature
test_install_arguments
test_install_preflight
test_install_release_resolution
test_install_signature_gate
test_install_linux_flow
test_install_macos_flow
test_install_update_timer_linux
test_install_update_timer_macos
test_launchd_launcher
test_launchd_launcher_update
test_launchd_smoke
test_standby_key_script
test_deploy_script_invariants

printf '\npassed %d, failed %d, skipped %d\n' "$PASSED" "$FAILED" "$SKIPPED"
if [ "$SKIPPED" -gt 0 ]; then
    printf 'NOTE: %d test(s) asserted nothing — see the SKIP lines above.\n' "$SKIPPED"
fi
if [ "$FAILED" -gt 0 ]; then
    exit 1
fi
