#!/usr/bin/env bash
#
# Tests for agent/deploy/lib.sh — the helpers install.sh and redeploy.sh share
# — and, since #434, for agent/deploy/bootstrap.sh, the checkout-free path
# onto install.sh.
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
# scripts/agent-standby-key.sh against a file-backed `doppler` stub. Since
# #434, bootstrap.sh is driven the same way — a stubbed curl serving a local
# tarball keyed by the ref's own basename, so no change to the stub was
# needed to add it. Nothing here talks to a host or a network. What is NOT
# stubbed is the signature verifier: the
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
unset STUB_MONO_NOW_US STUB_RESUME_US STUB_GUARD_SYSTEMCTL_ARGV GUARD_HOME GUARD_NO_INVOCATION GUARD_BASH_ENV
unset STUB_SYSTEMD_VERSION
unset STUB_SYSTEMCTL_DISABLE_EXIT STUB_SYSTEMCTL_STOP_EXIT STUB_LAUNCHCTL_BOOTOUT_EXIT
unset STUB_SYSTEMCTL_PROBE_LOCK STUB_SYSTEMCTL_PROBE_RESULT
unset STUB_LAUNCHCTL_PROBE_LOCK STUB_LAUNCHCTL_PROBE_RESULT

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

# A skip whose only cause is "jq is not installed". jq is deliberately kept
# off the PATH every other bootstrap.sh case runs under (TOOLBIN never
# symlinks it), so the sed fallback is what those cases exercise; this is the
# one place that puts a REAL jq back in front of a bootstrap.sh run, to cover
# the jq branch itself — loudly SKIPped, not silently, on a machine that has
# none. Unlike minisign, CI is not asked to install jq on purpose (both
# hosted runner images already carry one), so there is no
# SOLADOR_DEPLOY_TEST_REQUIRE_JQ escalation to go with it.
skip_needs_jq() {
    skip "$1" "jq not installed"
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
# STUB_SYSTEMCTL_USER_EXIT; `--user show -p Version --value` prints
# STUB_SYSTEMD_VERSION verbatim (default `256.11-1.stub`, the running
# manager's property shape), which the --enable-timer preflight reads for
# its ExecCondition= floor (#411); `is-enabled` prints
# STUB_SYSTEMCTL_IS_ENABLED (default `enabled`, exit 1 for anything else,
# like the real one); enabling the update timer answers
# STUB_SYSTEMCTL_TIMER_EXIT; everything else succeeds.
#
# `stop` and `disable` are --uninstall's own calls, gated by install.sh
# SOLELY on whether the unit's own FILE exists on disk (#455: a prior
# revision also asked the running manager's own state — `is-active`,
# falling back to `list-units --all` — so a re-run could still find and
# stop a unit whose file an earlier exit-4 run had already removed; every
# review round on that logic found a new Blocking problem in it, so it was
# backed out rather than shipped, and is tracked at #455 instead). The stub
# has no unit-name tracking of its own any more: `stop` and `disable`
# simply act on whatever unit install.sh actually names, below.
cat > "$STUBS/systemctl" <<'STUB'
#!/usr/bin/env bash
if [ -n "${STUB_SYSTEMCTL_ARGV:-}" ]; then
    printf '%s\n' "$*" >> "$STUB_SYSTEMCTL_ARGV"
fi
case "$*" in
    "--user show -p Version --value")
        printf '%s\n' "${STUB_SYSTEMD_VERSION-256.11-1.stub}"
        exit 0
        ;;
esac
case "${2:-}" in
    show-environment) exit "${STUB_SYSTEMCTL_USER_EXIT:-0}" ;;
    is-enabled)
        printf '%s\n' "${STUB_SYSTEMCTL_IS_ENABLED:-enabled}"
        [ "${STUB_SYSTEMCTL_IS_ENABLED:-enabled}" = "enabled" ] && exit 0
        exit 1
        ;;
    # --uninstall's own `stop` call (#454 round-4 review's own follow-up:
    # stop and disable are two separate calls, never a combined
    # `disable --now`; install.sh gates both on the unit's own FILE existing
    # — #455). This is the FIRST mutating call stop_and_disable_linux_unit
    # makes on a gated unit, so it is also where the continuous-lock-hold
    # proof (STUB_SYSTEMCTL_PROBE_LOCK, below) now attaches: it
    # independently attempts a real, non-blocking flock() on the named lock
    # file — `flock(1)` where the CURRENT PATH has one (the synthetic
    # `TOOLBIN_FAKEFLOCK`, standing in for a real one no host running this
    # suite has), else the identical question through the stock `perl`'s
    # Fcntl flock, the same fallback install.sh's own lock acquisition uses
    # — and writes "busy" or "free" to STUB_SYSTEMCTL_PROBE_RESULT: a
    # genuine, independent process actually contending for the SAME
    # flock() this uninstall is meant to be holding throughout, not merely
    # a file that exists. `stop` always succeeds here (install.sh only ever
    # calls it once it has already gated the unit on its FILE, so the stub
    # needs no unit-name tracking of its own to be realistic) unless
    # STUB_SYSTEMCTL_STOP_EXIT forces a failure — independent of
    # STUB_SYSTEMCTL_DISABLE_EXIT below, proving the two calls fail (and
    # are reported) independently is the point of having split them.
    stop)
        if [ -n "${STUB_SYSTEMCTL_PROBE_LOCK:-}" ]; then
            if command -v flock >/dev/null 2>&1; then
                if flock -n "$STUB_SYSTEMCTL_PROBE_LOCK" true 2>/dev/null; then
                    printf 'free\n' > "${STUB_SYSTEMCTL_PROBE_RESULT:-/dev/null}"
                else
                    printf 'busy\n' > "${STUB_SYSTEMCTL_PROBE_RESULT:-/dev/null}"
                fi
            elif command -v perl >/dev/null 2>&1; then
                if perl -MFcntl=:flock -e '
                    open(my $fh, "+>>", $ARGV[0]) or exit 2;
                    exit(flock($fh, LOCK_EX | LOCK_NB) ? 0 : 1);
                ' "$STUB_SYSTEMCTL_PROBE_LOCK"; then
                    printf 'free\n' > "${STUB_SYSTEMCTL_PROBE_RESULT:-/dev/null}"
                else
                    printf 'busy\n' > "${STUB_SYSTEMCTL_PROBE_RESULT:-/dev/null}"
                fi
            fi
        fi
        if [ -n "${STUB_SYSTEMCTL_STOP_EXIT:-}" ] && [ "$STUB_SYSTEMCTL_STOP_EXIT" != "0" ]; then
            exit "$STUB_SYSTEMCTL_STOP_EXIT"
        fi
        exit 0
        ;;
    # --uninstall's own `disable` call — no `--now` any more; `stop`, above,
    # is the separate call that covers it. Real systemd's `disable` needs
    # the unit's own FILE to know which enablement symlinks to remove, and
    # fails "Unit file <u> does not exist" (`do_unit_file_disable`'s own
    # -ENOENT) without one, before it ever reaches a stop it no longer even
    # asks for — mirrored here against the stub's own $HOME. install.sh
    # gates `disable` the same way it gates `stop` (#455): both run only
    # when the unit FILE exists, so this stub's "does not exist" branch is
    # not reachable through install.sh's own calls any more; it stays here
    # because it is what real systemd does, and so an unset
    # STUB_SYSTEMCTL_DISABLE_EXIT does not silently start meaning something
    # different.
    disable)
        # $3 is the unit name — UNLESS this is the old `--now` form (no
        # caller in install.sh uses it any more, but the stub still answers
        # it the way real systemd does, for robustness): then $3 is
        # literally "--now" and the unit name is $4.
        disable_unit="$3"
        [ "$disable_unit" = "--now" ] && disable_unit="$4"
        if [ ! -f "$HOME/.config/systemd/user/$disable_unit" ]; then
            echo "Failed to disable unit: Unit file $disable_unit does not exist." >&2
            exit 1
        fi
        if [ -n "${STUB_SYSTEMCTL_DISABLE_EXIT:-}" ] && [ "$STUB_SYSTEMCTL_DISABLE_EXIT" != "0" ]; then
            exit "$STUB_SYSTEMCTL_DISABLE_EXIT"
        fi
        exit 0
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
    # --uninstall's own bootout calls (#439): STUB_LAUNCHCTL_BOOTOUT_EXIT fails
    # both the metrics and the update-label bootout — the login-session check
    # (print gui/<uid>, above) already answered, so this is the narrower
    # "manager reachable, one stop request still refused" case.
    # STUB_LAUNCHCTL_PROBE_LOCK is the macOS analogue of
    # STUB_SYSTEMCTL_PROBE_LOCK above — see its own comment.
    bootout)
        if [ -n "${STUB_LAUNCHCTL_PROBE_LOCK:-}" ]; then
            if command -v flock >/dev/null 2>&1; then
                if flock -n "$STUB_LAUNCHCTL_PROBE_LOCK" true 2>/dev/null; then
                    printf 'free\n' > "${STUB_LAUNCHCTL_PROBE_RESULT:-/dev/null}"
                else
                    printf 'busy\n' > "${STUB_LAUNCHCTL_PROBE_RESULT:-/dev/null}"
                fi
            elif command -v perl >/dev/null 2>&1; then
                if perl -MFcntl=:flock -e '
                    open(my $fh, "+>>", $ARGV[0]) or exit 2;
                    exit(flock($fh, LOCK_EX | LOCK_NB) ? 0 : 1);
                ' "$STUB_LAUNCHCTL_PROBE_LOCK"; then
                    printf 'free\n' > "${STUB_LAUNCHCTL_PROBE_RESULT:-/dev/null}"
                else
                    printf 'busy\n' > "${STUB_LAUNCHCTL_PROBE_RESULT:-/dev/null}"
                fi
            fi
        fi
        if [ -n "${STUB_LAUNCHCTL_BOOTOUT_EXIT:-}" ] && [ "$STUB_LAUNCHCTL_BOOTOUT_EXIT" != "0" ]; then
            exit "$STUB_LAUNCHCTL_BOOTOUT_EXIT"
        fi
        exit 0
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
# The two manager reads the Linux guard makes (#411), in the same clock
# directory so they are on PATH only for the guard cases and never shadow
# the install cases' systemctl. Each prints its variable's value — the
# activation time STUB_MONO_NOW_US from the user manager, the last resume
# STUB_RESUME_US from the system manager, both CLOCK_MONOTONIC in µs the way
# `systemctl show --value` prints them — or, when that value is the word
# FAIL, fails the way the real one does with no bus to reach. Any other
# invocation is an error: the guard has no business asking the manager
# anything else, and the argv log STUB_GUARD_SYSTEMCTL_ARGV is what proves it.
cat > "$STUBS_CLOCK/systemctl" <<'STUB'
#!/usr/bin/env bash
if [ -n "${STUB_GUARD_SYSTEMCTL_ARGV:-}" ]; then
    printf '%s\n' "$*" >> "$STUB_GUARD_SYSTEMCTL_ARGV"
fi
case "$*" in
    "--user show -p InactiveExitTimestampMonotonic --value "*)
        if [ "${STUB_MONO_NOW_US:-}" = "FAIL" ]; then
            echo "Failed to connect to user scope bus via local transport: No such file or directory" >&2
            exit 1
        fi
        printf '%s\n' "${STUB_MONO_NOW_US:-0}"
        ;;
    "show -p InactiveEnterTimestampMonotonic --value sleep.target")
        if [ "${STUB_RESUME_US:-}" = "FAIL" ]; then
            echo "Failed to connect to system scope bus via local transport: No such file or directory" >&2
            exit 1
        fi
        printf '%s\n' "${STUB_RESUME_US:-0}"
        ;;
    *)
        echo "stub systemctl (guard): unexpected invocation: $*" >&2
        exit 1
        ;;
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
# install.sh's own perl fallback for the update lock (run_uninstall, #439) is
# only exercised on a host with no flock(1) on PATH — which, since flock is
# now in TOOLBIN above (so install.sh's own restricted-PATH check sees it
# when the host has one), is no longer "any host running this suite".
# TOOLBIN_NOFLOCK is TOOLBIN minus flock, so the uninstall tests can force
# that fallback branch deterministically regardless of what the running host
# happens to have — real perl, asking the kernel a genuine flock() question
# on fd 9, is in the master list below too: without it, every --uninstall
# test run on a host with no flock(1) at all (stock macOS, including the CI
# runner `./dev test` uses for this suite) would find NEITHER flock(1) nor
# perl on PATH and refuse (busy) unconditionally, failing an uninstall the
# fixtures below expect to succeed.
# TOOLBIN_NOFLOCK_NOPERL further removes perl, for the one tier install.sh
# itself refuses on: neither tool on PATH, so it cannot tell whether a
# transaction is running and fails toward busy before anything changes.
TOOLBIN_NOFLOCK="$TMP/toolbin-noflock"
TOOLBIN_NOFLOCK_NOPERL="$TMP/toolbin-noflock-noperl"
mkdir -p "$TOOLBIN" "$TOOLBIN_NOVERIFIER" "$TOOLBIN_NOFLOCK" "$TOOLBIN_NOFLOCK_NOPERL"
for tool in awk sed grep cut head tr od cat mkdir rm cp mv install chmod mktemp cmp \
            id dirname basename ls seq openssl env sh bash date sort tar find gzip base64 \
            flock perl mkfifo sleep; do
    real="$(command -v "$tool" 2>/dev/null || true)"
    if [ -n "$real" ]; then
        ln -s "$real" "$TOOLBIN/$tool"
        ln -s "$real" "$TOOLBIN_NOVERIFIER/$tool"
        [ "$tool" = "flock" ] || ln -s "$real" "$TOOLBIN_NOFLOCK/$tool"
        [ "$tool" = "flock" ] || [ "$tool" = "perl" ] || ln -s "$real" "$TOOLBIN_NOFLOCK_NOPERL/$tool"
    fi
done

# TOOLBIN_FAKEFLOCK: TOOLBIN_NOFLOCK plus a SYNTHETIC `flock(1)`, backed by
# perl's Fcntl flock, supporting only the two invocation forms install.sh
# itself actually uses: the path form (`flock -n <path> <command>`, this
# suite's own STUB_SYSTEMCTL_PROBE_LOCK/STUB_LAUNCHCTL_PROBE_LOCK probes,
# above) and the fd-only form (`flock -n <fd>`, run_uninstall's own
# continuous hold — `man flock`'s EXAMPLES idiom for locking the CALLER's
# own open fd in place). This exists because stock macOS — including the CI
# runner `./dev test` runs this suite on — ships no real flock(1) at all,
# which would otherwise leave the flock(1) tier of that continuous hold
# entirely unexercised by this suite outside of Linux CI. A real
# `flock -n <fd>` genuinely locks the OPEN FILE DESCRIPTION, not a
# per-process table, so a lock taken this way persists for as long as the
# CALLING shell's own fd on that path stays open — exactly what the
# fd-only form is for, and exactly what this reproduces with perl. Present
# only when perl is (SOLADOR_DEPLOY_TEST-style SKIP, not a failure,
# otherwise).
TOOLBIN_FAKEFLOCK="$TMP/toolbin-fakeflock"
HAVE_FAKEFLOCK=false
if command -v perl >/dev/null 2>&1; then
    mkdir -p "$TOOLBIN_FAKEFLOCK"
    for f in "$TOOLBIN_NOFLOCK"/*; do
        ln -s "$f" "$TOOLBIN_FAKEFLOCK/$(basename "$f")"
    done
    cat > "$TOOLBIN_FAKEFLOCK/flock" <<'STUB'
#!/usr/bin/env bash
rest=()
for a in "$@"; do
    case "$a" in
        -n | -x | -s | -u) ;;
        *) rest+=("$a") ;;
    esac
done
target="${rest[0]:-}"
case "$target" in
    '' | *[!0-9]*)
        # Path form: open/create it, lock it, then exec the command (or, with
        # none, just report the result) — the PROBE_LOCK stubs' own shape.
        cmd=("${rest[@]:1}")
        if [ "${#cmd[@]}" -eq 0 ]; then
            perl -MFcntl=:flock -e '
                open(my $fh, "+>>", $ARGV[0]) or exit 1;
                exit(flock($fh, LOCK_EX | LOCK_NB) ? 0 : 1);
            ' "$target"
            exit $?
        fi
        perl -MFcntl=:flock -e '
            my $path = shift @ARGV;
            open(my $fh, "+>>", $path) or exit 1;
            exit 1 unless flock($fh, LOCK_EX | LOCK_NB);
            exec { $ARGV[0] } @ARGV;
        ' "$target" "${cmd[@]}"
        exit $?
        ;;
    *)
        # fd-only form: lock the CALLER's already-open fd in place — "<&="
        # reuses that exact fd rather than dup()ing a new one, so the lock
        # this acquires is the SAME open file description the caller's own
        # fd refers to, and survives this (sub)process exiting.
        perl -MFcntl=:flock -e '
            open(my $fh, "<&=", $ARGV[0]) or exit 1;
            exit(flock($fh, LOCK_EX | LOCK_NB) ? 0 : 1);
        ' "$target"
        exit $?
        ;;
esac
STUB
    chmod +x "$TOOLBIN_FAKEFLOCK/flock"
    HAVE_FAKEFLOCK=true
fi
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

# jq is deliberately absent from TOOLBIN (see its own comment above), so every
# bootstrap.sh --ref case elsewhere in this file drives the no-jq sed
# fallback. JQ_DIR is a real jq, symlinked in ONLY for the case that
# deliberately exercises the jq branch of verify_ref_reachable_from_main —
# added to a run's PATH, never to TOOLBIN itself.
JQ_DIR="$TMP/jq-bin"
mkdir -p "$JQ_DIR"
HAVE_JQ=false
if command -v jq >/dev/null 2>&1; then
    ln -s "$(command -v jq)" "$JQ_DIR/jq"
    HAVE_JQ=true
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

    assert_eq "health_url does not double-bracket an already-bracketed IPv6 literal" \
        "http://[fd7a::1]:7878/v1/health" "$(health_url "[fd7a::1]" "7878" "0")"
    assert_eq "health_url unbrackets a bracketed IPv4" \
        "http://100.64.0.9:7878/v1/health" "$(health_url "[100.64.0.9]" "7878" "0")"
    assert_eq "health_url unbrackets a bracketed hostname" \
        "http://host:7878/v1/health" "$(health_url "[host]" "7878" "0")"
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

    # SOLADOR_AGENT_TLS=1 (#447): https://, across the same bind shapes —
    # mirrors agent/src/update.rs's own `health_url_uses_https_when_tls_is_on`
    # so the two implementations cannot silently diverge on what "on" means.
    assert_eq "health_url dials https:// when tls=1" \
        "https://100.87.202.125:7878/v1/health" "$(health_url "100.87.202.125" "7878" "1")"
    assert_eq "health_url still probes loopback for a wildcard bind, over https" \
        "https://127.0.0.1:7878/v1/health" "$(health_url "0.0.0.0" "7878" "1")"
    assert_eq "health_url still brackets an IPv6 literal, over https" \
        "https://[fd7a:115c:a1e0::1]:7878/v1/health" "$(health_url "fd7a:115c:a1e0::1" "7878" "1")"
    # Anything other than the literal "1" is http://, never inferred.
    assert_eq "health_url treats an empty tls argument as http://" \
        "http://127.0.0.1:7878/v1/health" "$(health_url "127.0.0.1" "7878" "")"
    assert_eq "health_url treats tls=0 as http://" \
        "http://127.0.0.1:7878/v1/health" "$(health_url "127.0.0.1" "7878" "0")"
    assert_eq "health_url treats an unrecognized tls value as http://" \
        "http://127.0.0.1:7878/v1/health" "$(health_url "127.0.0.1" "7878" "true")"
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
    assert_output_has "curl_exit_hint names the DER/PEM cacert failure (#447)" "$(curl_exit_hint 77)" "PEM, not DER"
    assert_output_has "curl_exit_hint names a failed TLS handshake" "$(curl_exit_hint 35)" "TLS handshake failed"
    assert_output_has "curl_exit_hint names a certificate mismatch" "$(curl_exit_hint 60)" "does not match the pinned"
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

    # ---- SOLADOR_AGENT_TLS=1 (#447) ----
    export STUB_CURL_BODY='{"status":"ok","hostname":"ubu-3xdv","version":"0.4.0"}'
    local tls_dir tls_env
    tls_dir="$TMP/tlsconfig"
    tls_env="$tls_dir/solador-agent.env"
    mkdir -p "$tls_dir"
    printf 'SOLADOR_AGENT_TOKEN=%s\nSOLADOR_AGENT_BIND=127.0.0.1\nSOLADOR_AGENT_PORT=7878\nSOLADOR_AGENT_TLS=1\n' \
        "$token" > "$tls_env"

    # No certificate at all: refused, and curl is never even invoked — the
    # missing-cert check short-circuits before a request could leak anything.
    : > "$STUB_CURL_ARGV"
    out="$(
        PATH="$STUBS:$PATH"
        export VERIFY_HEALTH_ATTEMPTS=2
        verify_health "$tls_env" "0.4.0" 2>&1
    )"
    status=$?
    assert_eq "verify_health refuses SOLADOR_AGENT_TLS=1 with no certificate" "1" "$status"
    assert_output_has "the refusal names why" "$out" "never started with TLS on"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "no certificate: verify_health never invokes curl" "curl was invoked anyway"
    else
        pass "no certificate: verify_health never invokes curl"
    fi

    # The certificate appearing PARTWAY through the retry loop — the
    # fresh-install startup race the existence check moved inside the loop
    # to survive (#447 review) — still succeeds: once found, the probe
    # dials https:// and pins it via cacert. This needs REAL per-attempt
    # timing to race a background writer against, so it uses a narrower
    # PATH than the rest of this function: just the curl stub, deliberately
    # WITHOUT $STUBS's own `sleep` (instant, for every other test's speed —
    # see its own stub, "keeps even that second off the clock" — which
    # would let all attempts finish before this test's background writer's
    # real 0.3s ever elapses).
    local tls_stub_dir
    tls_stub_dir="$TMP/tls-stubs"
    mkdir -p "$tls_stub_dir"
    ln -sf "$STUBS/curl" "$tls_stub_dir/curl"
    : > "$STUB_CURL_ARGV"
    rm -f "$tls_dir/solador-agent.tls.crt"
    (
        sleep 0.3
        printf 'FAKE-PEM-FOR-THIS-SHELL-LEVEL-TEST' > "$tls_dir/solador-agent.tls.crt"
    ) &
    out="$(
        PATH="$tls_stub_dir:$PATH"
        export VERIFY_HEALTH_ATTEMPTS=5
        verify_health "$tls_env" "0.4.0" 2>&1
    )"
    status=$?
    wait
    assert_eq "verify_health survives the certificate appearing mid-retry" "0" "$status"
    # Verified as `localhost` (in every certificate's baseline SAN list) while
    # connecting to the bind address (#449): the bind can change after the
    # certificate's SAN list was fixed.
    assert_file_has "once found, the probe dials https:// as localhost" "$STUB_CURL_ARGV" \
        "https://localhost:7878/v1/health"
    assert_file_has "once found, the probe still connects to the bind address" "$STUB_CURL_ARGV" \
        '[config] connect-to = "localhost:7878:127.0.0.1:7878"'
    assert_file_has "once found, the probe pins via cacert" "$STUB_CURL_ARGV" \
        "[config] cacert = \"$tls_dir/solador-agent.tls.crt\""

    # Every bind form's probe URL and connect target — the rows of
    # agent/src/update.rs's `probe_target_matches_lib_sh_for_every_bind_form`,
    # which must agree with these (#449, #462). A wildcard is dialled at
    # loopback with no connect-to; every other bind, a DNS name included, is
    # probed as `localhost` and connected to the bind. A bracketed non-IPv6
    # bind is connected to unbracketed.
    local row bind_v want_url want_connect
    for row in \
        "|https://127.0.0.1:7878/v1/health|" \
        "0.0.0.0|https://127.0.0.1:7878/v1/health|" \
        "::|https://[::1]:7878/v1/health|" \
        "[::]|https://[::1]:7878/v1/health|" \
        "100.64.0.9|https://localhost:7878/v1/health|localhost:7878:100.64.0.9:7878" \
        "fd7a::1|https://localhost:7878/v1/health|localhost:7878:[fd7a::1]:7878" \
        "[fd7a::1]|https://localhost:7878/v1/health|localhost:7878:[fd7a::1]:7878" \
        "[100.64.0.9]|https://localhost:7878/v1/health|localhost:7878:100.64.0.9:7878" \
        "[host]|https://localhost:7878/v1/health|localhost:7878:host:7878" \
        "host.tailnet.ts.net|https://localhost:7878/v1/health|localhost:7878:host.tailnet.ts.net:7878"; do
        bind_v="${row%%|*}"
        want_url="${row#*|}"
        want_connect="${want_url#*|}"
        want_url="${want_url%%|*}"
        printf 'SOLADOR_AGENT_TOKEN=%s\nSOLADOR_AGENT_BIND=%s\nSOLADOR_AGENT_PORT=7878\nSOLADOR_AGENT_TLS=1\n' \
            "$token" "$bind_v" > "$tls_env"
        : > "$STUB_CURL_ARGV"
        (
            PATH="$STUBS:$PATH"
            export VERIFY_HEALTH_ATTEMPTS=1
            verify_health "$tls_env" "0.4.0" >/dev/null 2>&1
        )
        assert_file_has "bind '$bind_v': the probe URL" "$STUB_CURL_ARGV" "$want_url"
        if [ -n "$want_connect" ]; then
            assert_file_has "bind '$bind_v': the connect target" "$STUB_CURL_ARGV" \
                "[config] connect-to = \"$want_connect\""
        elif grep -q "connect-to" "$STUB_CURL_ARGV"; then
            fail "bind '$bind_v': a wildcard has no connect target" "$(grep connect-to "$STUB_CURL_ARGV")"
        else
            pass "bind '$bind_v': a wildcard has no connect target"
        fi
    done

    # The TLS=0 half of the same table (#462): the URL is the target, nothing
    # is redirected and nothing is pinned, for every bind form — the plain rows
    # of `probe_target_matches_lib_sh_for_every_bind_form`.
    for row in \
        "|http://127.0.0.1:7878/v1/health" \
        "0.0.0.0|http://127.0.0.1:7878/v1/health" \
        "::|http://[::1]:7878/v1/health" \
        "[::]|http://[::1]:7878/v1/health" \
        "100.64.0.9|http://100.64.0.9:7878/v1/health" \
        "fd7a::1|http://[fd7a::1]:7878/v1/health" \
        "[fd7a::1]|http://[fd7a::1]:7878/v1/health" \
        "[100.64.0.9]|http://100.64.0.9:7878/v1/health" \
        "[host]|http://host:7878/v1/health" \
        "host.tailnet.ts.net|http://host.tailnet.ts.net:7878/v1/health"; do
        bind_v="${row%%|*}"
        want_url="${row#*|}"
        printf 'SOLADOR_AGENT_TOKEN=%s\nSOLADOR_AGENT_BIND=%s\nSOLADOR_AGENT_PORT=7878\nSOLADOR_AGENT_TLS=0\n' \
            "$token" "$bind_v" > "$tls_env"
        : > "$STUB_CURL_ARGV"
        (
            PATH="$STUBS:$PATH"
            export VERIFY_HEALTH_ATTEMPTS=1
            verify_health "$tls_env" "0.4.0" >/dev/null 2>&1
        )
        assert_file_has "TLS=0, bind '$bind_v': the probe URL" "$STUB_CURL_ARGV" "$want_url"
        if grep -q "connect-to\|cacert" "$STUB_CURL_ARGV"; then
            fail "TLS=0, bind '$bind_v': nothing is redirected or pinned" "$(cat "$STUB_CURL_ARGV")"
        else
            pass "TLS=0, bind '$bind_v': nothing is redirected or pinned"
        fi
    done

    rm -rf "$tls_dir"
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

# ---- simulating a held update lock (#439) -------------------------------------
#
# The lock `solador-agent update`/`rollback` hold for their lifetime
# (agent/src/update.rs's TransactionLock) is never removed once created, so
# the file's mere existence proves nothing about whether it is held RIGHT
# NOW — that is what install.sh's --uninstall (run_uninstall, lib.sh's
# comment on the lock has the full design) needs to know before it removes
# anything, and what it takes and holds itself: a real flock(1) where the
# host has one (Linux, typically); the stock `perl`'s Fcntl flock asking the
# SAME kernel question on the SAME already-open fd where flock(1) is absent
# (stock macOS ships neither `flock(1)` — TransactionLock is flock()-based
# too, confirmed by reading update.rs's own comments, so this is never a
# different question from the one a real transaction would meet); and,
# where neither tool exists at all, a refusal before anything changes,
# since there is no way left to ask for certain and guessing "free" is the
# wrong direction.

# hold_fake_lock <path>: makes install.sh's own lock acquisition see <path>
# as busy, however it decides that — a real, held flock(1) when this host
# has one, a real held lock via perl otherwise — and does not return until
# it can prove the lock is actually held, so a caller never races its own
# setup. release_fake_lock undoes it. FAKE_LOCK_PID is empty when nothing
# was backgrounded (neither tool is on PATH at all — see the plain
# `: > "$lock"` branch below, which relies on install.sh's own "neither
# tool" tier refusing any existing lock file unconditionally rather than on
# holding a real lock itself).
#
# Both real holders block on opening a FIFO nobody writes to, rather than
# `sleep`: a plain `sleep` would fork a child that INHERITS the locked file
# descriptor (a bare `exec N>file` carries no CLOEXEC, and perl's own open()
# does not set it either), and flock(2)'s lock lives as long as ANY
# reference to that open file description does, sleep's included — the exact
# gotcha agent/src/update.rs's TransactionLock documents. Killing only the
# holder would leave that orphaned child quietly holding the lock forever,
# and every later "is it released" assertion would read busy. Blocking on
# the FIFO's open() instead means neither holder forks anything while the
# lock is held, so killing it is the whole story. The perl holder signals
# "I actually hold it now" by writing an ack file rather than by this
# function re-probing the lock itself — probing WITH perl would be exactly
# the operation under test, and probing WITH flock(1) is not available
# (that is why this branch exists).
FAKE_LOCK_PID=""
FAKE_LOCK_FIFO=""
FAKE_LOCK_ACK=""
hold_fake_lock() {
    local lock="$1"
    mkdir -p "$(dirname "$lock")"
    if command -v flock >/dev/null 2>&1 && command -v mkfifo >/dev/null 2>&1; then
        : > "$lock"
        FAKE_LOCK_FIFO="$TMP/fake-lock-fifo.$$"
        rm -f "$FAKE_LOCK_FIFO"
        mkfifo "$FAKE_LOCK_FIFO"
        (
            exec 9>"$lock"
            flock -x 9
            exec <"$FAKE_LOCK_FIFO"
            read -r _unused
        ) &
        FAKE_LOCK_PID=$!
        local waited=0
        while flock -n "$lock" true 2>/dev/null; do
            waited=$((waited + 1))
            [ "$waited" -ge 50 ] && break
            sleep 0.1
        done
    elif command -v perl >/dev/null 2>&1 && command -v mkfifo >/dev/null 2>&1; then
        : > "$lock"
        FAKE_LOCK_FIFO="$TMP/fake-lock-fifo.$$"
        FAKE_LOCK_ACK="$TMP/fake-lock-ack.$$"
        rm -f "$FAKE_LOCK_FIFO" "$FAKE_LOCK_ACK"
        mkfifo "$FAKE_LOCK_FIFO"
        perl -MFcntl=:flock -e '
            my ($lock, $fifo, $ack) = @ARGV;
            open(my $fh, "+<", $lock) or die "open $lock: $!";
            flock($fh, LOCK_EX) or die "flock $lock: $!";
            open(my $a, ">", $ack) or die "open $ack: $!";
            print $a "held\n";
            close $a;
            open(my $f, "<", $fifo) or die "open $fifo: $!";
            my $line = <$f>;
        ' "$lock" "$FAKE_LOCK_FIFO" "$FAKE_LOCK_ACK" &
        FAKE_LOCK_PID=$!
        local waited=0
        while [ ! -e "$FAKE_LOCK_ACK" ]; do
            waited=$((waited + 1))
            [ "$waited" -ge 100 ] && break
            sleep 0.05
        done
    else
        # Neither tool exists: install.sh's own "neither tool" tier refuses
        # (busy) unconditionally, since it has no way left to check, so a
        # plain touch already produces the state this function promises —
        # nothing is actually locked, and nothing needs backgrounding.
        : > "$lock"
        FAKE_LOCK_PID=""
    fi
}
release_fake_lock() {
    local lock="$1"
    if [ -n "$FAKE_LOCK_PID" ]; then
        kill "$FAKE_LOCK_PID" 2>/dev/null || true
        wait "$FAKE_LOCK_PID" 2>/dev/null || true
        FAKE_LOCK_PID=""
    fi
    if [ -n "$FAKE_LOCK_FIFO" ]; then
        rm -f "$FAKE_LOCK_FIFO"
        FAKE_LOCK_FIFO=""
    fi
    if [ -n "$FAKE_LOCK_ACK" ]; then
        rm -f "$FAKE_LOCK_ACK"
        FAKE_LOCK_ACK=""
    fi
    rm -f "$lock"
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

# The agent/deploy/* files a real install.sh needs beside it — one list, so
# make_checkout (a plain directory, for the existing install-flow tests) and
# make_bootstrap_checkout_archive (the same files tarred up, for #434's
# bootstrap.sh) can never drift into copying two different sets.
DEPLOY_SOURCE_FILES="install.sh lib.sh run-agent.sh solador-agent.service \
    app.solador.agent.plist solador-agent-update.service \
    solador-agent-update.timer app.solador.agent.update.plist update-guard.sh"

# Lay out a copy of agent/deploy plus the given public key as
# agent/release-signing-key.pub.
make_checkout() {
    local pubkey="$1" f
    rm -rf "$CHECKOUT"
    mkdir -p "$CHECKOUT/agent/deploy"
    for f in $DEPLOY_SOURCE_FILES; do
        cp "$SCRIPT_DIR/$f" "$CHECKOUT/agent/deploy/"
    done
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
# The fingerprint every fixture's `tls-fingerprint` prints (#447): a fixed,
# obviously-fake value — the fixture is a shell stub with no real
# certificate behind it, and install.sh's Done block only ever reads this
# back and prints it, never parses it.
FIXTURE_TLS_FINGERPRINT="AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99"
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
if [ "\$1" = "tls-fingerprint" ]; then
    printf '%s\n' '$FIXTURE_TLS_FINGERPRINT'
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

# make_pre_tls_fixture <version> <triple> <seckey|->: like make_fixture, but
# `tls-fingerprint` is an UNRECOGNIZED argument (exit 2), matching a real
# agent published before #447 — main.rs's parse_args refuses anything it
# does not name, and this subcommand did not exist yet. install.sh's
# capability probe (`STAGED_BIN_SUPPORTS_TLS`) is what this fixture exists
# to exercise: a fresh install staging THIS binary must not default
# SOLADOR_AGENT_TLS to 1.
make_pre_tls_fixture() {
    local version="$1" triple="$2" seckey="$3" f
    mkdir -p "$FIXTURES"
    f="$FIXTURES/$(agent_asset_name "$version" "$triple")"
    cat > "$f" <<STUB
#!/bin/sh
printf '%s\\n' "\$*" >> "$AGENT_ARGV"
if [ "\$1" = "--version" ]; then
    printf '%s\n' '$version'
    exit 0
fi
echo "solador-agent: unrecognized argument '\$1'" >&2
exit 2
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
# in INSTALL_STATUS, for assertions that read the output first. INSTALL_TLS
# (#447, default "0") sets SOLADOR_AGENT_TLS for the run; "unset" leaves it
# unset so install.sh's own default (on for a fresh install) decides.
INSTALL_OUT="$TMP/install.out"
INSTALL_STDIN=""
INSTALL_PATH=""
INSTALL_SCRIPT=""
INSTALL_STATUS=""
INSTALL_TLS=""
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
        # SOLADOR_AGENT_TLS (#447) defaults to off for this harness: the
        # fixture "agent" is a shell stub with no real TLS server behind it,
        # so every pre-#447 scenario below keeps exercising the plain-HTTP
        # path it was written against, via install.sh's own precedence (a
        # pre-set SOLADOR_AGENT_TLS wins outright, ahead of "fresh install
        # defaults to on"). A TLS-specific scenario sets INSTALL_TLS=unset
        # to let that default actually fire, or INSTALL_TLS=1 to force it
        # on for a re-run — either way it also fakes the certificate
        # `verify_health` then requires; see the TLS section below.
        if [ "${INSTALL_TLS:-0}" = "unset" ]; then
            unset SOLADOR_AGENT_TLS
        else
            export SOLADOR_AGENT_TLS="${INSTALL_TLS:-0}"
        fi
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
# oneshot, the oneshot's guard (#411), or a macOS updater plist (under any
# label)? The default install's contract (#394) is that the answer is no,
# and this is the one place that spells out what "no" covers.
updater_installed() {
    local home="$1"
    [ -e "$home/.config/systemd/user/solador-agent-update.timer" ] && return 0
    [ -e "$home/.config/systemd/user/solador-agent-update.service" ] && return 0
    [ -e "$home/.local/bin/solador-agent-update-guard" ] && return 0
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
# no update-transaction lock, no ~/.local/bin directory at all, and no
# service-manager call — the state a refusal must leave behind. The lock
# file and the bare directory are their own checks (#454 round-4 review),
# not folded into "binary": an --uninstall refusal used to open (and
# thereby create) <bin>.update.lock, and on a host with no install
# directory yet, ~/.local/bin itself, before ever checking whether it
# could actually do anything — leaving both behind despite claiming
# "Nothing has been changed".
assert_untouched() {
    local name="$1" home="$2" problems=""
    [ -e "$home/.config/solador-agent.env" ] && problems="$problems env-file"
    [ -e "$home/.local/bin/solador-agent" ] && problems="$problems binary"
    [ -e "$home/.local/bin/solador-agent.update.lock" ] && problems="$problems lock-file"
    [ -d "$home/.local/bin" ] && problems="$problems local-bin-dir"
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

# ---- bootstrap.sh (#434) ------------------------------------------------
#
# bootstrap.sh downloads a repository ARCHIVE (codeload.github.com), not a
# directory, so its own tests package fixtures as tarballs rather than
# plain directories: the stub curl's `-o <dest> <url>` mode already copies
# STUB_CURL_FIXTURES/<basename of url> to <dest> for any URL, and a
# codeload URL's basename is just the ref (e.g. "main", or a 40-hex sha) —
# so pointing STUB_CURL_FIXTURES at the same $FIXTURES directory the
# install-flow tests already use, and dropping an archive there named after
# the ref, needs no change to the curl stub at all.

BOOTSTRAP_SCRIPT="$SCRIPT_DIR/bootstrap.sh"
BOOTSTRAP_OUT="$TMP/bootstrap.out"
BOOTSTRAP_STATUS=""
BOOTSTRAP_PATH=""
run_bootstrap() {
    local home="$1"
    shift
    (
        export HOME="$home"
        export PATH="${BOOTSTRAP_PATH:-$STUBS:$TOOLBIN}"
        export VERIFY_HEALTH_ATTEMPTS=1
        export STUB_CURL_FIXTURES="$FIXTURES"
        unset XDG_CACHE_HOME
        export USER="${USER:-tester}"
        umask 022
        "$BASH" "$BOOTSTRAP_SCRIPT" "$@"
    ) >"$BOOTSTRAP_OUT" 2>&1
    BOOTSTRAP_STATUS=$?
    return "$BOOTSTRAP_STATUS"
}

# The three files the fake install.sh below writes, all OUTSIDE bootstrap's
# own staging directory (which is gone by the time run_bootstrap returns) —
# its argv, a full listing of the extracted tree, and the staged directory's
# own path, captured from INSIDE the run so the test can assert it is gone
# afterward.
BOOTSTRAP_FAKE_ARGV="$TMP/bootstrap-fake-argv"
BOOTSTRAP_FAKE_LISTING="$TMP/bootstrap-fake-listing"
BOOTSTRAP_FAKE_STAGE="$TMP/bootstrap-fake-stage"

# make_bootstrap_archive <ref>: an archive laid out the way codeload.github.com
# lays one out (solador-<ref>/...), carrying a FAKE agent/deploy/install.sh
# (records its own argv and its tree, never touches a real service) plus the
# signing key(s) — and, critically, files OUTSIDE agent/deploy and outside
# the .pub names (a crate, a workflow, a second file under agent/ that is
# not the key) that must NOT survive bootstrap.sh's extraction.
make_bootstrap_archive() {
    local ref="$1" src="$TMP/bootstrap-src" dir
    rm -rf "$src"
    dir="$src/solador-$ref"
    mkdir -p "$dir/agent/deploy" "$dir/crates/decoy" "$dir/.github/workflows"
    cat > "$dir/agent/deploy/install.sh" <<STUB
#!/bin/sh
printf '%s\n' "\$*" > "$BOOTSTRAP_FAKE_ARGV"
( cd "\$(dirname "\$0")/../.." && find . -type f | sort ) > "$BOOTSTRAP_FAKE_LISTING"
( cd "\$(dirname "\$0")/../.." && pwd ) > "$BOOTSTRAP_FAKE_STAGE"
exit "\${FAKE_INSTALL_EXIT:-0}"
STUB
    chmod +x "$dir/agent/deploy/install.sh"
    echo "lib" > "$dir/agent/deploy/lib.sh"
    echo "pubkey" > "$dir/agent/release-signing-key.pub"
    echo "next-pubkey" > "$dir/agent/release-signing-key-next.pub"
    echo "cargo" > "$dir/agent/Cargo.toml"
    echo "decoy" > "$dir/crates/decoy/lib.rs"
    echo "workflow" > "$dir/.github/workflows/ci.yml"
    mkdir -p "$FIXTURES"
    ( cd "$src" && tar -czf "$FIXTURES/$ref" "solador-$ref" )
}

# A real codeload.github.com pax GLOBAL header record — captured verbatim
# (base64) from a live `main` archive during #434's development, not
# synthesised: a valid tar header's checksum is over the whole 512-byte
# block, and reproducing that by hand invites a checksum bug a synthetic
# fixture would never exercise. Its `comment=` VALUE lives in the data
# block that follows this header, never inside the header itself (whose
# name is always the 18 bytes "pax_global_header" and whose size is always
# 52 for a 40-hex-character sha, so the header itself needs no per-test
# edits) — replaying these exact bytes ahead of an otherwise-ordinary tar
# is what a real archive's layout actually is.
PAX_GLOBAL_HEADER_B64="cGF4X2dsb2JhbF9oZWFkZXIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAADAwMDA2NjYAMDAwMDAwMAAwMDAwMDAwADAwMDAwMDAwMDY0ADE1MjU2NTM1NDMxADAwMTQ1MjMAZwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB1c3RhcgAwMHJvb3QAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAcm9vdAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAwMDAwMDAwADAwMDAwMDAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="

# make_bootstrap_archive_with_pax_comment <ref> <embedded-sha>: like
# make_bootstrap_archive, but the archive genuinely carries a pax GLOBAL
# header whose `comment=` is <embedded-sha> — the shape bootstrap.sh's
# best-effort resolved-commit readout (bootstrap.sh's `if [ "$ref" = main
# ]` block) actually parses, unlike the plain-`tar` fixture above, which
# carries no such header and only exercises the "main" fallback.
make_bootstrap_archive_with_pax_comment() {
    local ref="$1" embedded="$2" src="$TMP/bootstrap-pax-src" dir work
    rm -rf "$src"
    dir="$src/solador-$ref"
    mkdir -p "$dir/agent/deploy"
    cat > "$dir/agent/deploy/install.sh" <<STUB
#!/bin/sh
printf '%s\n' "\$*" > "$BOOTSTRAP_FAKE_ARGV"
exit "\${FAKE_INSTALL_EXIT:-0}"
STUB
    chmod +x "$dir/agent/deploy/install.sh"
    echo "pubkey" > "$dir/agent/release-signing-key.pub"

    work="$TMP/bootstrap-pax-work"
    rm -rf "$work"
    mkdir -p "$work"
    printf '%s' "$PAX_GLOBAL_HEADER_B64" | base64 -d > "$work/header.bin"
    printf '52 comment=%s\n' "$embedded" > "$work/data-raw.bin"
    head -c 460 /dev/zero > "$work/data-pad.bin"
    cat "$work/data-raw.bin" "$work/data-pad.bin" > "$work/data.bin"
    ( cd "$src" && tar -cf "$work/plain.tar" "solador-$ref" )
    cat "$work/header.bin" "$work/data.bin" "$work/plain.tar" > "$work/full.tar"
    mkdir -p "$FIXTURES"
    gzip -c "$work/full.tar" > "$FIXTURES/$ref"
}

# make_bootstrap_checkout_archive <ref> <pubkey>: the REAL agent/deploy/*
# (DEPLOY_SOURCE_FILES — the same files make_checkout copies) tarred up as
# codeload would serve them, carrying <pubkey> as agent/release-signing-key.pub.
# For the one case that must run the real install.sh, and through it the
# real minisign gate, end to end through bootstrap.sh.
make_bootstrap_checkout_archive() {
    local ref="$1" pubkey="$2" src="$TMP/bootstrap-checkout-src" dir f
    rm -rf "$src"
    dir="$src/solador-$ref"
    mkdir -p "$dir/agent/deploy"
    for f in $DEPLOY_SOURCE_FILES; do
        cp "$SCRIPT_DIR/$f" "$dir/agent/deploy/"
    done
    cp "$pubkey" "$dir/agent/release-signing-key.pub"
    mkdir -p "$FIXTURES"
    ( cd "$src" && tar -czf "$FIXTURES/$ref" "solador-$ref" )
}

test_bootstrap_extraction_and_passthrough() {
    local home="$TMP/home-bootstrap" listing argv

    mkdir -p "$home"
    reset_argv_logs
    rm -f "$BOOTSTRAP_FAKE_ARGV" "$BOOTSTRAP_FAKE_LISTING" "$BOOTSTRAP_FAKE_STAGE"

    # --help is bootstrap's own, never reaching the network.
    run_bootstrap "$home" --help
    assert_eq "bootstrap.sh --help exits 0" "0" "$BOOTSTRAP_STATUS"
    assert_output_has "--help prints the usage" "$(cat "$BOOTSTRAP_OUT")" "Usage:"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "--help downloads nothing" "curl was invoked"
    else
        pass "--help downloads nothing"
    fi

    # A bad --ref is refused before any download, the same way.
    reset_argv_logs
    run_bootstrap "$home" --ref not-a-sha
    assert_eq "bootstrap.sh refuses a --ref that is not 'main' or a 40-hex sha" "2" "$BOOTSTRAP_STATUS"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "a bad --ref downloads nothing" "curl was invoked"
    else
        pass "a bad --ref downloads nothing"
    fi

    # No pass-through arguments: the fake install.sh must see an empty argv.
    reset_argv_logs
    rm -f "$BOOTSTRAP_FAKE_ARGV"
    make_bootstrap_archive main
    run_bootstrap "$home"
    assert_eq "bootstrap.sh (default ref, no flags) exits 0" "0" "$BOOTSTRAP_STATUS"
    argv="$(cat "$BOOTSTRAP_FAKE_ARGV" 2>/dev/null || true)"
    assert_empty "install.sh receives no arguments when bootstrap.sh is given none" "$argv"
    assert_output_has "bootstrap.sh prints the resolved commit" "$(cat "$BOOTSTRAP_OUT")" "commit main"

    # --ref is consumed by bootstrap.sh; everything else passes through, in
    # order, unchanged — including flags bootstrap.sh itself knows nothing
    # about (its job is not to duplicate install.sh's own argument parser).
    reset_argv_logs
    rm -f "$BOOTSTRAP_FAKE_ARGV" "$BOOTSTRAP_FAKE_LISTING" "$BOOTSTRAP_FAKE_STAGE"
    run_bootstrap "$home" --ref main --enable-timer --migrate-from-opt
    assert_eq "bootstrap.sh with pass-through arguments exits 0" "0" "$BOOTSTRAP_STATUS"
    argv="$(cat "$BOOTSTRAP_FAKE_ARGV" 2>/dev/null || true)"
    assert_eq "install.sh receives exactly the pass-through arguments, --ref stripped" \
        "--enable-timer --migrate-from-opt" "$argv"

    # --uninstall [--purge] passes through exactly the same way (#439) — no
    # special case in bootstrap.sh's own argument loop for it.
    reset_argv_logs
    rm -f "$BOOTSTRAP_FAKE_ARGV" "$BOOTSTRAP_FAKE_LISTING" "$BOOTSTRAP_FAKE_STAGE"
    run_bootstrap "$home" --uninstall --purge
    assert_eq "bootstrap.sh passes --uninstall --purge through" "0" "$BOOTSTRAP_STATUS"
    argv="$(cat "$BOOTSTRAP_FAKE_ARGV" 2>/dev/null || true)"
    assert_eq "install.sh receives --uninstall --purge unchanged" "--uninstall --purge" "$argv"

    # Extraction is restricted to agent/deploy/* and the signing key(s) —
    # nothing else the archive carries reaches disk.
    listing="$(cat "$BOOTSTRAP_FAKE_LISTING" 2>/dev/null || true)"
    assert_output_has "the extracted tree carries install.sh" "$listing" "agent/deploy/install.sh"
    assert_output_has "the extracted tree carries lib.sh" "$listing" "agent/deploy/lib.sh"
    assert_output_has "the extracted tree carries the signing key" "$listing" "agent/release-signing-key.pub"
    assert_output_has "the extracted tree carries the standby signing key" "$listing" "agent/release-signing-key-next.pub"
    case "$listing" in
        *crates*) fail "bootstrap.sh does not extract crates/" "found it in the extracted tree" ;;
        *) pass "bootstrap.sh does not extract crates/" ;;
    esac
    case "$listing" in
        *.github*) fail "bootstrap.sh does not extract .github/" "found it in the extracted tree" ;;
        *) pass "bootstrap.sh does not extract .github/" ;;
    esac
    case "$listing" in
        *Cargo.toml*) fail "bootstrap.sh does not extract agent/Cargo.toml (only deploy/ and the key(s))" "found it in the extracted tree" ;;
        *) pass "bootstrap.sh does not extract agent/Cargo.toml (only deploy/ and the key(s))" ;;
    esac

    # The staged directory the fake install.sh reported (agent/deploy/../..,
    # i.e. the extracted solador-<ref> root, itself inside bootstrap's own
    # mktemp -d) must be GONE now that bootstrap.sh has exited.
    local stage_dir
    stage_dir="$(cat "$BOOTSTRAP_FAKE_STAGE" 2>/dev/null || true)"
    if [ -n "$stage_dir" ] && [ ! -e "$stage_dir" ]; then
        pass "bootstrap.sh removes its staging directory on exit"
    else
        fail "bootstrap.sh removes its staging directory on exit" "still present: ${stage_dir:-<unknown>}"
    fi
}

test_bootstrap_help_when_piped() {
    local home="$TMP/home-bootstrap-piped-help" out status
    mkdir -p "$home"

    # usage() reads its own header comment out of $0 by line range — but
    # piped in ("cat bootstrap.sh | bash -s -- --help"), $0 is bash itself,
    # not this file, and under set -e an awk failure to open it would kill
    # the run before `exit 0` is ever reached. The canned fallback text is
    # what keeps this exiting 0 either way. Deliberately the bare word
    # "bash", resolved through PATH — not $BASH's own absolute path, which
    # IS a real file and would make `[ -f "$0" ]` true for the wrong reason,
    # missing the case entirely. argv[0] is the literal text used to invoke
    # a command found via PATH, not its resolved path, which is what makes
    # $0 the word "bash" here — the same as a real `curl ... | bash`.
    reset_argv_logs
    (
        # A fresh, empty cwd: `[ -f "$0" ]` resolves the bare word "bash"
        # against the CURRENT directory, and this must stay false on
        # anyone's machine, not merely on one that has no ./bash today.
        cd "$home" || exit 1
        export HOME="$home"
        export PATH="$STUBS:$TOOLBIN"
        cat "$BOOTSTRAP_SCRIPT" | bash -s -- --help
    ) >"$BOOTSTRAP_OUT" 2>&1
    status=$?
    out="$(cat "$BOOTSTRAP_OUT")"
    assert_eq "bootstrap.sh --help exits 0 even when piped (\$0 is not a real file)" "0" "$status"
    assert_output_has "the piped --help still prints Usage" "$out" "Usage:"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "the piped --help downloads nothing" "curl was invoked"
    else
        pass "the piped --help downloads nothing"
    fi
}

test_bootstrap_resolves_commit_from_pax_header() {
    local home="$TMP/home-bootstrap-pax" embedded="0123456789abcdef0123456789abcdef01234567"
    mkdir -p "$home"

    # The "prints the resolved commit" case in the passthrough test above
    # exercises only the FALLBACK (a fixture with no pax global header, so
    # $resolved stays the literal word "main"). This one carries a real pax
    # global header, so it is the one that actually exercises
    # bootstrap.sh's `gzip | head | grep` readout.
    reset_argv_logs
    rm -f "$BOOTSTRAP_FAKE_ARGV"
    make_bootstrap_archive_with_pax_comment main "$embedded"
    run_bootstrap "$home"
    assert_eq "bootstrap.sh exits 0 against an archive carrying a pax global header" "0" "$BOOTSTRAP_STATUS"
    assert_output_has "bootstrap.sh reads the resolved commit from the archive's pax global header" \
        "$(cat "$BOOTSTRAP_OUT")" "commit $embedded"
}

test_bootstrap_truncated_runs_nothing() {
    local start call_line cut truncated

    start="$(line_of "$BOOTSTRAP_SCRIPT" 'bootstrap_main() {')"
    call_line="$(line_of "$BOOTSTRAP_SCRIPT" 'bootstrap_main "$@"')"
    if [ -z "$start" ] || [ -z "$call_line" ]; then
        fail "bootstrap.sh has the expected single-function shape" \
            "could not find bootstrap_main() { and its final call on their own lines"
        return
    fi
    cut=$(( (start + call_line) / 2 ))

    truncated="$TMP/bootstrap-truncated.sh"
    head -n "$cut" "$BOOTSTRAP_SCRIPT" > "$truncated"
    chmod +x "$truncated"

    reset_argv_logs
    (
        export HOME="$TMP/home-bootstrap-truncated"
        mkdir -p "$HOME"
        export PATH="$STUBS:$TOOLBIN"
        export STUB_CURL_FIXTURES="$FIXTURES"
        "$BASH" "$truncated" --ref main >"$BOOTSTRAP_OUT" 2>&1
    )
    BOOTSTRAP_STATUS=$?
    if [ "$BOOTSTRAP_STATUS" -eq 0 ]; then
        fail "a bootstrap.sh truncated mid-function does not exit 0" "it exited 0"
    else
        pass "a bootstrap.sh truncated mid-function exits non-zero (a syntax error, never a partial run)"
    fi
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "a truncated bootstrap.sh downloads nothing" "curl was invoked"
    else
        pass "a truncated bootstrap.sh downloads nothing"
    fi
}

test_bootstrap_refuses_root() {
    local home="$TMP/home-bootstrap-root" root_stubs="$TMP/stubs-bootstrap-root" out
    mkdir -p "$home" "$root_stubs"
    cat > "$root_stubs/id" <<STUB
#!/usr/bin/env bash
case "\${1:-}" in
    -u) echo 0 ;;
    *) exec "$(command -v id)" "\$@" ;;
esac
STUB
    chmod +x "$root_stubs/id"

    reset_argv_logs
    BOOTSTRAP_PATH="$root_stubs:$STUBS:$TOOLBIN" run_bootstrap "$home"
    out="$(cat "$BOOTSTRAP_OUT")"
    assert_eq "bootstrap.sh refuses to run as root" "1" "$BOOTSTRAP_STATUS"
    assert_output_has "the root refusal says why" "$out" "refusing to run as root"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "the root refusal downloads nothing" "curl was invoked"
    else
        pass "the root refusal downloads nothing"
    fi
    BOOTSTRAP_PATH=""
}

test_bootstrap_validate_ref() {
    local home="$TMP/home-bootstrap-validate-ref"
    mkdir -p "$home"

    # A bare --ref with nothing after it (the last token on the line) must
    # be refused as usage, not read $ref past the end of "$@" or silently
    # keep the "main" default.
    reset_argv_logs
    run_bootstrap "$home" --ref
    assert_eq "a bare --ref with no value exits 2" "2" "$BOOTSTRAP_STATUS"
    assert_output_has "the bare --ref refusal says why" "$(cat "$BOOTSTRAP_OUT")" "needs a value"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "a bare --ref downloads nothing" "curl was invoked"
    else
        pass "a bare --ref downloads nothing"
    fi

    # A control character in --ref must never reach the URL bootstrap.sh
    # builds by interpolation — refused the same shape validate_release_tag
    # refuses one in lib.sh (lib_test.sh's own test_validate_release_tag).
    reset_argv_logs
    run_bootstrap "$home" --ref "$(printf 'deadbeefdeadbeefdeadbeefdeadbeefdeadbee\tf')"
    assert_eq "bootstrap.sh refuses a --ref with a control character" "2" "$BOOTSTRAP_STATUS"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "a --ref with a control character downloads nothing" "curl was invoked"
    else
        pass "a --ref with a control character downloads nothing"
    fi

    # `--ref=<sha>` (the single-token form) must be recognised too, not
    # silently fall through to install_args, which would leave $ref at its
    # "main" default — the reachability check skipped and main downloaded
    # instead of the pin the operator asked for, with install.sh's own
    # "unknown argument '--ref=<sha>'" refusal the only visible symptom.
    reset_argv_logs
    run_bootstrap "$home" --ref=not-a-sha
    assert_eq "bootstrap.sh refuses --ref=<value> the same as --ref <value>" "2" "$BOOTSTRAP_STATUS"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "a bad --ref=<value> downloads nothing" "curl was invoked"
    else
        pass "a bad --ref=<value> downloads nothing"
    fi

    local ref="deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
    reset_argv_logs
    rm -f "$BOOTSTRAP_FAKE_ARGV"
    make_bootstrap_archive "$ref"
    STUB_CURL_BODY='{"status":"identical","ahead_by":0,"behind_by":0}' \
        run_bootstrap "$home" --ref="$ref"
    assert_eq "bootstrap.sh accepts and acts on --ref=<sha> like --ref <sha>" "0" "$BOOTSTRAP_STATUS"
    if grep -q "api.github.com" "$STUB_CURL_ARGV" 2>/dev/null; then
        pass "--ref=<sha> reaches the reachability check (main's shortcut was not taken)"
    else
        fail "--ref=<sha> reaches the reachability check (main's shortcut was not taken)" \
            "api.github.com was never requested — \$ref likely stayed \"main\""
    fi
}

test_bootstrap_ref_must_be_reachable_from_main() {
    local home="$TMP/home-bootstrap-ref" ref="deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
    mkdir -p "$home"

    # codeload.github.com will archive ANY commit this repo holds, merged or
    # not — an open pull request's head among them. --ref only trusts a
    # commit main's own history already contains, checked against GitHub's
    # compare API before any download: `diverged` (an unmerged branch) and
    # `behind` (main lacks commits the ref has) are both refused.
    reset_argv_logs
    STUB_CURL_BODY='{"status":"diverged","ahead_by":3,"behind_by":5}' \
        run_bootstrap "$home" --ref "$ref"
    assert_eq "bootstrap.sh refuses a --ref GitHub reports as diverged from main" "1" "$BOOTSTRAP_STATUS"
    assert_output_has "the diverged refusal says why" "$(cat "$BOOTSTRAP_OUT")" "not reachable from main"
    if grep -q "codeload" "$STUB_CURL_ARGV" 2>/dev/null; then
        fail "a --ref refused for reachability downloads no archive" "codeload.github.com was requested"
    else
        pass "a --ref refused for reachability downloads no archive"
    fi

    reset_argv_logs
    STUB_CURL_BODY='{"status":"behind","ahead_by":0,"behind_by":2}' \
        run_bootstrap "$home" --ref "$ref"
    assert_eq "bootstrap.sh refuses a --ref GitHub reports as behind main" "1" "$BOOTSTRAP_STATUS"
    assert_output_has "the behind refusal says why" "$(cat "$BOOTSTRAP_OUT")" "not reachable from main"
    if grep -q "codeload" "$STUB_CURL_ARGV" 2>/dev/null; then
        fail "a --ref refused as behind downloads no archive" "codeload.github.com was requested"
    else
        pass "a --ref refused as behind downloads no archive"
    fi

    # An unreachable/failed compare check fails closed: an unverifiable ref
    # is never treated as a verified one.
    reset_argv_logs
    STUB_CURL_BODY="" \
        run_bootstrap "$home" --ref "$ref"
    assert_eq "bootstrap.sh refuses a --ref it could not verify against main" "1" "$BOOTSTRAP_STATUS"
    assert_output_has "the unverifiable-ref refusal says why" "$(cat "$BOOTSTRAP_OUT")" "could not verify"

    # `ahead` (main contains the ref plus more commits) and `identical`
    # (the ref IS main) both mean the ref is part of main's history, and are
    # accepted — the archive download and extraction proceed normally.
    reset_argv_logs
    rm -f "$BOOTSTRAP_FAKE_ARGV"
    make_bootstrap_archive "$ref"
    STUB_CURL_BODY='{"status":"ahead","ahead_by":3,"behind_by":0}' \
        run_bootstrap "$home" --ref "$ref"
    assert_eq "bootstrap.sh accepts a --ref GitHub reports as an ancestor of main (ahead)" "0" "$BOOTSTRAP_STATUS"

    reset_argv_logs
    rm -f "$BOOTSTRAP_FAKE_ARGV"
    STUB_CURL_BODY='{"status":"identical","ahead_by":0,"behind_by":0}' \
        run_bootstrap "$home" --ref "$ref"
    assert_eq "bootstrap.sh accepts a --ref GitHub reports as identical to main" "0" "$BOOTSTRAP_STATUS"

    # jq is deliberately NOT on the restricted PATH this harness builds
    # (TOOLBIN never symlinks it), so every case above already drives the
    # no-jq sed fallback. This one is the shape that fallback has to get
    # right: a COMPACT (single-line, no jq available to pretty-print it)
    # response whose "files" array carries its own per-file "status" ---
    # "modified" here --- after the real, top-level one. An unanchored
    # greedy match would walk past the real field and read the file's
    # instead; the fix truncates before "files"/"commits" so only the
    # top-level field is ever in play.
    reset_argv_logs
    rm -f "$BOOTSTRAP_FAKE_ARGV"
    STUB_CURL_BODY='{"url":"x","status":"identical","ahead_by":0,"behind_by":0,"commits":[],"files":[{"filename":"a","status":"modified"}]}' \
        run_bootstrap "$home" --ref "$ref"
    assert_eq "bootstrap.sh reads the top-level status, not a later per-file one, from compact JSON" \
        "0" "$BOOTSTRAP_STATUS"

    # main itself never consults the compare API at all — the common path
    # costs no extra round trip.
    reset_argv_logs
    rm -f "$BOOTSTRAP_FAKE_ARGV"
    make_bootstrap_archive main
    run_bootstrap "$home"
    assert_eq "the default ref (main) still exits 0 with no compare-API stub set" "0" "$BOOTSTRAP_STATUS"
    if grep -q "api.github.com" "$STUB_CURL_ARGV" 2>/dev/null; then
        fail "the default ref never consults the compare API" "api.github.com was requested"
    else
        pass "the default ref never consults the compare API"
    fi
}

# Every case above runs with jq off the PATH, so none of them ever exercise
# the `command -v jq` branch of verify_ref_reachable_from_main — only its
# sed fallback. This is the one test that puts a real jq back in front of
# bootstrap.sh (JQ_DIR, never TOOLBIN itself) and drives the same four
# statuses through it.
test_bootstrap_ref_reachable_via_jq() {
    local home="$TMP/home-bootstrap-ref-jq" ref="deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
    mkdir -p "$home"
    if [ "$HAVE_JQ" != true ]; then
        skip_needs_jq "bootstrap.sh (jq present) refuses a --ref GitHub reports as diverged from main"
        skip_needs_jq "bootstrap.sh (jq present) refuses a --ref GitHub reports as behind main"
        skip_needs_jq "bootstrap.sh (jq present) accepts a --ref GitHub reports as an ancestor of main (ahead)"
        skip_needs_jq "bootstrap.sh (jq present) accepts a --ref GitHub reports as identical to main"
        return
    fi

    reset_argv_logs
    BOOTSTRAP_PATH="$JQ_DIR:$STUBS:$TOOLBIN" \
    STUB_CURL_BODY='{"status":"diverged","ahead_by":3,"behind_by":5}' \
        run_bootstrap "$home" --ref "$ref"
    assert_eq "bootstrap.sh (jq present) refuses a --ref GitHub reports as diverged from main" \
        "1" "$BOOTSTRAP_STATUS"

    reset_argv_logs
    BOOTSTRAP_PATH="$JQ_DIR:$STUBS:$TOOLBIN" \
    STUB_CURL_BODY='{"status":"behind","ahead_by":0,"behind_by":2}' \
        run_bootstrap "$home" --ref "$ref"
    assert_eq "bootstrap.sh (jq present) refuses a --ref GitHub reports as behind main" \
        "1" "$BOOTSTRAP_STATUS"

    reset_argv_logs
    rm -f "$BOOTSTRAP_FAKE_ARGV"
    make_bootstrap_archive "$ref"
    BOOTSTRAP_PATH="$JQ_DIR:$STUBS:$TOOLBIN" \
    STUB_CURL_BODY='{"status":"ahead","ahead_by":3,"behind_by":0}' \
        run_bootstrap "$home" --ref "$ref"
    assert_eq "bootstrap.sh (jq present) accepts a --ref GitHub reports as an ancestor of main (ahead)" \
        "0" "$BOOTSTRAP_STATUS"

    reset_argv_logs
    rm -f "$BOOTSTRAP_FAKE_ARGV"
    BOOTSTRAP_PATH="$JQ_DIR:$STUBS:$TOOLBIN" \
    STUB_CURL_BODY='{"status":"identical","ahead_by":0,"behind_by":0}' \
        run_bootstrap "$home" --ref "$ref"
    assert_eq "bootstrap.sh (jq present) accepts a --ref GitHub reports as identical to main" \
        "0" "$BOOTSTRAP_STATUS"

    BOOTSTRAP_PATH=""
}

# The no-jq sed fallback's own comment used to claim that truncating at
# "commits"/"files" makes the top-level "status" the only field an
# unanchored match can ever reach — true for a compact, single-line response,
# but sed truncates per LINE, so a pretty-printed (multi-line) one needs its
# own coverage.
test_bootstrap_ref_reachable_pretty_printed() {
    local home="$TMP/home-bootstrap-ref-pretty" ref="deadbeefdeadbeefdeadbeefdeadbeefdeadbeef" body
    mkdir -p "$home"

    # The ordinary shape, pretty-printed: top-level "status" in GitHub's own
    # position, before "commits"/"files". Must read the same as the compact
    # case above.
    body='{
  "url": "https://api.github.com/repos/Sassy-Dog/solador/compare/'"$ref"'...main",
  "status": "identical",
  "ahead_by": 0,
  "behind_by": 0,
  "commits": [],
  "files": [
    {
      "filename": "agent/deploy/install.sh",
      "status": "modified"
    }
  ]
}'
    reset_argv_logs
    rm -f "$BOOTSTRAP_FAKE_ARGV"
    make_bootstrap_archive "$ref"
    STUB_CURL_BODY="$body" run_bootstrap "$home" --ref "$ref"
    assert_eq "bootstrap.sh reads the top-level status from a pretty-printed (multi-line) response" \
        "0" "$BOOTSTRAP_STATUS"

    # The case the old comment overclaimed protection against: "files"
    # (carrying a per-file status of "identical", chosen to spoof an accept)
    # placed BEFORE the real top-level "status", which is "diverged" and
    # must be refused. sed's truncation runs line by line: on a MULTI-LINE
    # response it blanks only the "files" line itself, so a per-file
    # "status" on a later line — untouched by that truncation — would
    # reach the second sed and, since it now sits earlier in the stream
    # than the real field, win the `head -n1` race. A spoofed accept here
    # is a regression back to the per-line version of the fallback; this
    # must stay refused.
    body='{
  "url": "https://api.github.com/repos/Sassy-Dog/solador/compare/'"$ref"'...main",
  "files": [
    {
      "filename": "agent/deploy/install.sh",
      "status": "identical"
    }
  ],
  "status": "diverged",
  "ahead_by": 3,
  "behind_by": 5
}'
    reset_argv_logs
    STUB_CURL_BODY="$body" run_bootstrap "$home" --ref "$ref"
    assert_eq "bootstrap.sh refuses a pretty-printed response whose real status is diverged, even though an earlier per-file status spells 'identical'" \
        "1" "$BOOTSTRAP_STATUS"
    assert_output_has "the refusal names the real reason, not a spoofed accept" \
        "$(cat "$BOOTSTRAP_OUT")" "not reachable from main"
    if grep -q "codeload" "$STUB_CURL_ARGV" 2>/dev/null; then
        fail "the spoof attempt downloads no archive" "codeload.github.com was requested"
    else
        pass "the spoof attempt downloads no archive"
    fi
}

test_bootstrap_failure_paths() {
    local home="$TMP/home-bootstrap-failures"
    mkdir -p "$home"

    # A download that fails outright (no fixture at the ref's key) leaves
    # nothing extracted and install.sh never runs.
    reset_argv_logs
    rm -rf "$FIXTURES"
    run_bootstrap "$home"
    assert_eq "bootstrap.sh refuses when the archive cannot be downloaded" "1" "$BOOTSTRAP_STATUS"
    assert_output_has "the download failure is named" "$(cat "$BOOTSTRAP_OUT")" "could not download"

    # An archive that does not have the expected agent/deploy layout at all
    # (extraction matches nothing) is refused, not silently ignored.
    reset_argv_logs
    rm -rf "$FIXTURES"
    mkdir -p "$FIXTURES"
    (
        d="$TMP/bootstrap-empty-src/solador-main"
        rm -rf "$TMP/bootstrap-empty-src"
        mkdir -p "$d/crates/decoy"
        echo "nothing here" > "$d/crates/decoy/lib.rs"
        cd "$TMP/bootstrap-empty-src" && tar -czf "$FIXTURES/main" solador-main
    )
    run_bootstrap "$home"
    assert_eq "bootstrap.sh refuses an archive with no agent/deploy layout" "1" "$BOOTSTRAP_STATUS"
    assert_output_has "the empty-extraction failure is named" "$(cat "$BOOTSTRAP_OUT")" "could not extract"

    # An archive that has agent/deploy/* but no install.sh inside it is
    # refused just as loudly, after extraction succeeds but before anything
    # is run.
    reset_argv_logs
    rm -rf "$FIXTURES"
    mkdir -p "$FIXTURES"
    (
        d="$TMP/bootstrap-noinstall-src/solador-main"
        rm -rf "$TMP/bootstrap-noinstall-src"
        mkdir -p "$d/agent/deploy"
        echo "lib" > "$d/agent/deploy/lib.sh"
        echo "pubkey" > "$d/agent/release-signing-key.pub"
        cd "$TMP/bootstrap-noinstall-src" && tar -czf "$FIXTURES/main" solador-main
    )
    run_bootstrap "$home"
    assert_eq "bootstrap.sh refuses an archive whose agent/deploy carries no install.sh" "1" "$BOOTSTRAP_STATUS"
    assert_output_has "the missing-install.sh failure is named" "$(cat "$BOOTSTRAP_OUT")" "not found after extraction"

    # Cleanup is unconditional: none of the three refusals above should have
    # left a staging directory behind.
    if ls "$home/.cache"/solador-agent-bootstrap.* >/dev/null 2>&1; then
        fail "every refusal above still cleaned up its staging directory" "a staging directory was left under $home/.cache"
    else
        pass "every refusal above still cleaned up its staging directory"
    fi
}

# The three failure paths above are all bootstrap.sh's OWN refusals —
# nothing here yet proves that a FAILURE install.sh reports on its own (a
# preflight refusal, an --enable-timer opt-in that did not take, ...) comes
# back out of bootstrap.sh unchanged, rather than being swallowed or
# flattened to 1. FAKE_INSTALL_EXIT (make_bootstrap_archive's fake
# install.sh) is what lets this test set that exit status without a real
# installer run.
test_bootstrap_passes_through_install_exit_status() {
    local home="$TMP/home-bootstrap-exit-status"
    mkdir -p "$home"

    reset_argv_logs
    rm -f "$BOOTSTRAP_FAKE_ARGV" "$BOOTSTRAP_FAKE_STAGE"
    make_bootstrap_archive main
    FAKE_INSTALL_EXIT=3 run_bootstrap "$home"
    assert_eq "bootstrap.sh passes install.sh's own non-zero exit status through unchanged" \
        "3" "$BOOTSTRAP_STATUS"

    # Cleanup does not depend on install.sh's own exit status either — the
    # staging directory is removed whether the run underneath it succeeded
    # or not.
    local stage_dir
    stage_dir="$(cat "$BOOTSTRAP_FAKE_STAGE" 2>/dev/null || true)"
    if [ -n "$stage_dir" ] && [ ! -e "$stage_dir" ]; then
        pass "the staging directory is removed even when install.sh exits non-zero"
    else
        fail "the staging directory is removed even when install.sh exits non-zero" \
            "still present: ${stage_dir:-<unknown>}"
    fi
}

test_bootstrap_signature_gate() {
    local home="$TMP/home-bootstrap-sig" ref="deadbeefdeadbeefdeadbeefdeadbeefdeadbeef" marker="$TMP/bootstrap-sig-marker"
    mkdir -p "$home"
    if [ "$HAVE_MINISIGN" != true ]; then
        skip_needs_minisign "a wrong key delivered via the bootstrap archive is rejected by install.sh's real minisign gate"
        skip_needs_minisign "the rejection above is minisign's (an accept-all verifier lets the same archive through)"
        return
    fi

    # The archive carries key B; the release binary it will try to install
    # is signed with key A — the same key-mismatch shape
    # test_install_signature_gate proves against a checkout on disk, proven
    # here against a key that arrived through the bootstrap archive instead.
    rm -rf "$FIXTURES"
    make_bootstrap_checkout_archive "$ref" "$TEST_KEY_DIR/b.pub"
    make_fixture 2026.9.8 x86_64-unknown-linux-musl "$TEST_KEY_DIR/a.key" "$marker" >/dev/null
    rm -f "$marker"
    reset_argv_logs
    export SOLADOR_AGENT_RELEASE="v2026.9.8"
    # A pinned --ref now needs GitHub's compare API to confirm it is
    # reachable from main before anything downloads; stub it "identical" so
    # this test exercises the signature gate, not that earlier check (which
    # has its own dedicated test below).
    STUB_CURL_BODY='{"status":"identical","ahead_by":0,"behind_by":0}' \
        run_bootstrap "$home" --ref "$ref"
    assert_eq "install.sh, reached through bootstrap.sh, rejects a candidate signed under a key that does not match the one the archive delivered" \
        "1" "$BOOTSTRAP_STATUS"
    if [ -e "$marker" ]; then
        fail "the mismatched-key candidate is never executed, even when the wrong key arrives via the bootstrap archive" \
            "the fixture ran (--version was called)"
    else
        pass "the mismatched-key candidate is never executed, even when the wrong key arrives via the bootstrap archive"
    fi

    # THE PROOF. Same archive, same wrong key, same fixture — only the
    # verifier on PATH changes. If the rejection above were not minisign's
    # doing, this run would be rejected too; it is not.
    rm -f "$marker"
    reset_argv_logs
    STUB_CURL_BODY='{"status":"identical","ahead_by":0,"behind_by":0}' \
    BOOTSTRAP_PATH="$STUBS_BYPASS:$STUBS:$TOOLBIN" run_bootstrap "$home" --ref "$ref"
    if [ -e "$marker" ]; then
        pass "the rejection above is minisign's (an accept-all verifier lets the same archive through)"
    else
        fail "the rejection above is minisign's (an accept-all verifier lets the same archive through)" \
            "with an accept-all verifier the mismatched-key candidate still did not run" \
            "$(head -n 20 "$BOOTSTRAP_OUT")"
    fi
    BOOTSTRAP_PATH=""
    unset SOLADOR_AGENT_RELEASE
}

# A re-run hint printed by the REAL install.sh, reached through bootstrap.sh,
# must name `bash bootstrap.sh ...` — never $0, which by the time anyone
# could read and act on a hint is a path under bootstrap.sh's own (already
# removed) staging directory. The "no bind address" preflight refusal is the
# earliest hint site reachable without a signed release fixture, so it is
# the one driven here, the same way test_bootstrap_signature_gate reaches a
# real install.sh: no Tailscale on PATH, no SOLADOR_AGENT_BIND set, and
# SOLADOR_AGENT_TLS=0 (#449: with TLS on there is no refusal to reach).
test_bootstrap_rerun_hint_names_bootstrap() {
    local home="$TMP/home-bootstrap-rerun-hint" out
    mkdir -p "$home"

    rm -rf "$FIXTURES"
    make_bootstrap_checkout_archive main "$SCRIPT_DIR/../release-signing-key.pub"
    reset_argv_logs
    SOLADOR_AGENT_TLS=0 BOOTSTRAP_PATH="$TMP/stubs-no-tailscale:$TOOLBIN" run_bootstrap "$home"
    out="$(cat "$BOOTSTRAP_OUT")"
    assert_eq "install.sh (reached through bootstrap.sh) still refuses without a bind address" \
        "1" "$BOOTSTRAP_STATUS"
    assert_output_has "the re-run hint names bootstrap.sh, not \$0" "$out" "bash bootstrap.sh"
    case "$out" in
        *.cache/solador-agent-bootstrap*)
            fail "the re-run hint does not name the (already-deleted) staged install.sh" \
                "found a ~/.cache/solador-agent-bootstrap.* path in the output"
            ;;
        *) pass "the re-run hint does not name the (already-deleted) staged install.sh" ;;
    esac
    BOOTSTRAP_PATH=""
}

# install.sh --uninstall never touches minisign/curl/Tailscale (it is
# dispatched before any of that preflight), so — unlike the install-flow
# tests — this needs no HAVE_MINISIGN gate to run the REAL install.sh
# through bootstrap.sh end to end.
test_bootstrap_uninstall_rerun_hint() {
    local home="$TMP/home-bootstrap-uninstall-hint" env_file out
    mkdir -p "$home/.config"
    env_file="$home/.config/solador-agent.env"
    : > "$env_file"

    rm -rf "$FIXTURES"
    make_bootstrap_checkout_archive main "$SCRIPT_DIR/../release-signing-key.pub"
    reset_argv_logs
    run_bootstrap "$home" --uninstall
    out="$(cat "$BOOTSTRAP_OUT")"
    assert_eq "install.sh --uninstall (reached through bootstrap.sh) exits 0" "0" "$BOOTSTRAP_STATUS"
    assert_output_has "the kept-env-file hint names bootstrap.sh, not \$0" "$out" "bash bootstrap.sh"
    assert_output_has "the kept-env-file hint still names --uninstall --purge" "$out" "--uninstall --purge"
    case "$out" in
        *.cache/solador-agent-bootstrap*)
            fail "the uninstall hint does not name the (already-deleted) staged install.sh" \
                "found a ~/.cache/solador-agent-bootstrap.* path in the output"
            ;;
        *) pass "the uninstall hint does not name the (already-deleted) staged install.sh" ;;
    esac
    [ -e "$env_file" ] && pass "the env file survives an uninstall reached through bootstrap.sh" \
        || fail "the env file survives an uninstall reached through bootstrap.sh" "$env_file is gone"
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
    assert_file_has "the unit tells the agent where its TLS files live (#457)" "$unit" 'Environment=SOLADOR_AGENT_CONFIG_DIR=%h/.config'
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

    # ---- the pre-rename handover is NOT a fresh install: TLS stays off (#447 review round 2) ----
    # Run with INSTALL_TLS=unset (rather than this harness's own
    # SOLADOR_AGENT_TLS=0 default) so install.sh's real FRESH_INSTALL /
    # LEGACY_ENV_FILE precedence is what actually decides this, the same way
    # test_install_tls's "pre-#447 env file" case does. $ENV_FILE has never
    # existed under the new name, so without the LEGACY_ENV_FILE check this
    # host reads as fresh and gets SOLADOR_AGENT_TLS=1 — breaking the exact
    # cockpit host the handover exists to keep working: the cockpit dials it over
    # plain HTTP (it was never paired), so an agent that suddenly speaks TLS reads
    # as unreachable there until the operator pairs it (#448).
    rm -rf "$home"
    mkdir -p "$home/.config/systemd/user"
    printf '[Service]\nExecStart=/opt/devcanopy-agent/devcanopy-agent\n' > "$home/.config/systemd/user/devcanopy-agent.service"
    printf 'DEVCANOPY_AGENT_TOKEN=legacy-tok-tls-MUST-NOT-BE-PRINTED\nDEVCANOPY_AGENT_BIND=0.0.0.0\n' \
        > "$home/.config/devcanopy-agent.env"
    reset_argv_logs
    INSTALL_TLS=unset INSTALL_STDIN="" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: the pre-rename handover succeeds" "0" "$INSTALL_STATUS"
    assert_file_has "the handover does NOT turn TLS on" "$env_file" "SOLADOR_AGENT_TLS=0"
    assert_output_has "the run does not call this a fresh install" "$out" "TLS: off"
    case "$out" in
        *"TLS: on (fresh install)"*)
            fail "the handover is not treated as a fresh install" "the output said so" ;;
        *) pass "the handover is not treated as a fresh install" ;;
    esac

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
    # The migration otherwise preserves the env file byte-for-byte, but this
    # one gains a line it never had: SOLADOR_AGENT_TLS (#447) is a fourth
    # key install.sh now always writes (0 here — this harness's default,
    # since the fixture agent has no real TLS server behind it), so a
    # pre-#447 file (hand-crafted above with only the original three) picks
    # it up on its first re-run through the new install.sh, right where the
    # write order puts it: after the three original owned keys, before the
    # operator-added one that was already carried through unowned.
    assert_eq "the migration preserves the env file otherwise byte-for-byte, plus the new TLS key" \
        "SOLADOR_AGENT_TOKEN=opt-tok-MUST-NOT-BE-PRINTED
SOLADOR_AGENT_BIND=100.64.0.3
SOLADOR_AGENT_PORT=7979
SOLADOR_AGENT_TLS=0
RUST_LOG=debug" \
        "$(cat "$env_file")"
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

# ---- TLS (#447) ---------------------------------------------------------------
# The fixture "agent" is a shell stub (make_fixture) with no real TLS server
# behind it, and the stub systemctl/launchctl never actually run it as a
# live process — so nothing in this harness generates a real certificate.
# Every scenario below pre-seeds $home/.config/solador-agent.tls.crt itself, standing in
# for what the real Rust agent would have written on its own first start,
# before the health probe (also stubbed — STUB_CURL_BODY, not a real TLS
# handshake) needs it to exist. What IS real and asserted here is
# install.sh's own logic: which value SOLADOR_AGENT_TLS gets and why
# (TLS_SOURCE, in the "==> TLS:" line), that the probe switches to https://
# and to `cacert = "…solador-agent.tls.crt"` on curl's stdin config, and that the Done
# block reads the fingerprint back through `tls-fingerprint` rather than
# generating or parsing anything itself.
test_install_tls() {
    local home="$TMP/home-tls" env_file bin out
    if [ "$HAVE_MINISIGN" != true ]; then
        skip_needs_minisign "install.sh: TLS defaults on for a fresh install"
        skip_needs_minisign "install.sh: a no-flag re-run keeps TLS off"
        skip_needs_minisign "install.sh: --enable-tls turns an existing off install on"
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
    bin="$home/.local/bin/solador-agent"

    # ---- a fresh install defaults SOLADOR_AGENT_TLS=1: https://, cacert, and the fingerprint in the Done block ----
    mkdir -p "$home/.config"
    printf 'FAKE-DER-BYTES' > "$home/.config/solador-agent.tls.crt"
    reset_argv_logs
    INSTALL_TLS=unset INSTALL_STDIN="tok-tls-fresh
" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: TLS defaults on for a fresh install" "0" "$INSTALL_STATUS"
    assert_file_has "the env file carries SOLADOR_AGENT_TLS=1" "$env_file" "SOLADOR_AGENT_TLS=1"
    assert_output_has "the run reports TLS on, sourced from a fresh install" "$out" "TLS: on (fresh install)"
    assert_file_has "the health probe dials https:// as localhost" "$STUB_CURL_ARGV" "https://localhost:7878/v1/health"
    assert_file_has "the health probe connects to the tailnet bind" "$STUB_CURL_ARGV" \
        '[config] connect-to = "localhost:7878:100.64.0.9:7878"'
    assert_file_has "the health probe pins the certificate via cacert" "$STUB_CURL_ARGV" \
        "[config] cacert = \"$home/.config/solador-agent.tls.crt\""
    assert_output_has "the Done block prints the fingerprint" "$out" "$FIXTURE_TLS_FINGERPRINT"
    assert_output_has "the Done block warns against deleting the key/cert" "$out" "Never delete"
    [ -x "$bin" ] || fail "the binary is installed before the fingerprint is read" "$out"

    # ---- a no-flag re-run keeps an existing TLS=1, unchanged ----
    reset_argv_logs
    INSTALL_TLS=unset INSTALL_STDIN="" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: a no-flag re-run over a TLS-on install" "0" "$INSTALL_STATUS"
    assert_file_has "TLS stays on across a no-flag re-run" "$env_file" "SOLADOR_AGENT_TLS=1"
    assert_output_has "the re-run names the env file as the source" "$out" \
        "TLS: on (kept from the existing env file)"

    # ---- an install with SOLADOR_AGENT_TLS=0 pre-set stays off across a no-flag re-run ----
    rm -rf "$home"
    mkdir -p "$home"
    reset_argv_logs
    INSTALL_STDIN="tok-tls-off
" run_install "$home"
    assert_eq "install.sh: a fresh install with SOLADOR_AGENT_TLS=0 pre-set" "0" "$INSTALL_STATUS"
    assert_file_has "TLS is off, per the pre-set env var, even though this install is fresh" \
        "$env_file" "SOLADOR_AGENT_TLS=0"

    reset_argv_logs
    INSTALL_TLS=unset INSTALL_STDIN="" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: a no-flag re-run over a TLS-off install" "0" "$INSTALL_STATUS"
    assert_file_has "a no-flag re-run does not turn TLS on" "$env_file" "SOLADOR_AGENT_TLS=0"
    assert_output_has "the re-run says how to opt in" "$out" "re-run with --enable-tls"

    # ---- only --enable-tls turns an existing off install on ----
    printf 'FAKE-DER-BYTES' > "$home/.config/solador-agent.tls.crt"
    reset_argv_logs
    INSTALL_TLS=unset INSTALL_STDIN="" run_install "$home" --enable-tls
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: --enable-tls turns an existing off install on" "0" "$INSTALL_STATUS"
    assert_file_has "--enable-tls flips SOLADOR_AGENT_TLS to 1" "$env_file" "SOLADOR_AGENT_TLS=1"
    assert_output_has "the run names --enable-tls as the source" "$out" "TLS: on (--enable-tls)"

    # ---- the most common upgrade path: an env file from BEFORE #447 (no
    # SOLADOR_AGENT_TLS line at all — hand-crafted, the way the /opt
    # migration fixture above is, rather than produced by a run_install
    # call, since every run_install call in THIS file's own present already
    # writes the key) ----
    rm -rf "$home"
    mkdir -p "$home/.config"
    printf 'SOLADOR_AGENT_TOKEN=pre-447-tok\nSOLADOR_AGENT_BIND=127.0.0.1\nSOLADOR_AGENT_PORT=7878\n' \
        > "$env_file"
    reset_argv_logs
    INSTALL_TLS=unset INSTALL_STDIN="" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: a no-flag re-run over a pre-#447 env file succeeds" "0" "$INSTALL_STATUS"
    assert_file_has "a pre-#447 env file's absent key resolves to off" "$env_file" "SOLADOR_AGENT_TLS=0"
    assert_output_has "the run names the unset existing choice, not a fresh install" "$out" \
        "TLS: off (kept from the existing env file (was unset)"

    unset SOLADOR_AGENT_RELEASE STUB_CURL_BODY STUB_TAILSCALE_IP
}

# The bind default once TLS is on (#449). A host with no Tailscale, no
# SOLADOR_AGENT_BIND and no existing env file used to be refused; with TLS on
# the token is encrypted and the certificate pinned, so it binds all
# interfaces instead — and says so, by name, in the summary. With TLS off the
# refusal is unchanged (the existing "refuses without a bind address" case).
test_install_tls_no_tailnet_bind() {
    local home="$TMP/home-tls-no-tailnet" env_file out env_before
    if [ "$HAVE_MINISIGN" != true ]; then
        skip_needs_minisign "install.sh: a fresh TLS install with no Tailscale binds all interfaces"
        skip_needs_minisign "install.sh: the summary names the interface and says all interfaces"
        skip_needs_minisign "install.sh: a TLS install with no Tailscale still refuses with TLS off"
        return
    fi
    make_checkout "$TEST_KEY_DIR/a.pub"
    rm -rf "$FIXTURES" "$home"
    mkdir -p "$home/.config"
    make_fixture 2026.9.8 x86_64-unknown-linux-musl "$TEST_KEY_DIR/a.key" >/dev/null
    export SOLADOR_AGENT_RELEASE="v2026.9.8"
    export STUB_CURL_BODY='{"status":"ok","hostname":"h","version":"2026.9.8"}'
    unset STUB_TAILSCALE_IP
    env_file="$home/.config/solador-agent.env"
    printf 'FAKE-DER-BYTES' > "$home/.config/solador-agent.tls.crt"

    # ---- fresh, TLS on, no Tailscale: not refused, binds every interface ----
    reset_argv_logs
    INSTALL_PATH="$TMP/stubs-no-tailscale:$TOOLBIN" INSTALL_TLS=unset INSTALL_STDIN="tok-no-tailnet
" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: a fresh TLS install with no Tailscale binds all interfaces" "0" "$INSTALL_STATUS"
    assert_file_has "the env file carries the all-interfaces bind" "$env_file" "SOLADOR_AGENT_BIND=0.0.0.0"
    assert_file_has "the env file carries SOLADOR_AGENT_TLS=1" "$env_file" "SOLADOR_AGENT_TLS=1"
    assert_output_has "the run names the bind and why" "$out" \
        "Binding to 0.0.0.0 (all interfaces: no Tailscale IP detected, and TLS is on)"
    assert_output_has "the run warns that every network can reach it" "$out" "EVERY network this host is on"
    assert_output_has "the run tells the operator to firewall or narrow the bind" "$out" "firewall port 7878"
    assert_output_has "the Done summary names the bind as all interfaces" "$out" \
        "Bind:    0.0.0.0:7878 — ALL interfaces"
    # A wildcard is not dialable: the probe goes to loopback, which the
    # certificate's baseline SAN list always carries.
    assert_file_has "the health probe dials loopback over https" "$STUB_CURL_ARGV" "https://127.0.0.1:7878/v1/health"
    if grep -q "connect-to" "$STUB_CURL_ARGV"; then
        fail "a wildcard bind needs no connect-to (loopback is dialled directly)" "$(grep connect-to "$STUB_CURL_ARGV")"
    else
        pass "a wildcard bind needs no connect-to (loopback is dialled directly)"
    fi
    case "$out" in
        *"tok-no-tailnet"*) fail "install.sh never prints the full token" "the token appeared in its output" ;;
        *) pass "install.sh never prints the full token" ;;
    esac

    # ---- a re-run resolves the bind again (it is provisional, #449) ----
    reset_argv_logs
    INSTALL_PATH="$TMP/stubs-no-tailscale:$TOOLBIN" INSTALL_TLS=unset INSTALL_STDIN="" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: a re-run over the no-tailnet TLS install" "0" "$INSTALL_STATUS"
    assert_output_has "the re-run reports the bind it resolved, not one it kept" "$out" \
        "Binding to 0.0.0.0 (all interfaces: no Tailscale IP detected, and TLS is on)"

    # ---- a Tailscale IP still wins over all interfaces when TLS is on ----
    rm -rf "$home"
    mkdir -p "$home/.config"
    printf 'FAKE-DER-BYTES' > "$home/.config/solador-agent.tls.crt"
    reset_argv_logs
    STUB_TAILSCALE_IP="100.64.0.9" INSTALL_TLS=unset INSTALL_STDIN="tok-tailnet
" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: a TLS install with Tailscale" "0" "$INSTALL_STATUS"
    assert_file_has "the tailnet IP is still the default bind" "$env_file" "SOLADOR_AGENT_BIND=100.64.0.9"
    assert_output_has "the summary says that interface only" "$out" "Bind:    100.64.0.9:7878 — that interface only"

    # ---- TLS off, no Tailscale: refused, exactly as before, before a download ----
    rm -rf "$home"
    mkdir -p "$home"
    reset_argv_logs
    INSTALL_PATH="$TMP/stubs-no-tailscale:$TOOLBIN" INSTALL_TLS=0 INSTALL_STDIN="" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: no Tailscale and TLS off is still refused" "1" "$INSTALL_STATUS"
    assert_output_has "the refusal names SOLADOR_AGENT_BIND and the TLS way out" "$out" "SOLADOR_AGENT_TLS=1"
    if [ -e "$home/.local/bin/solador-agent" ] || [ -e "$env_file" ]; then
        fail "the refusal changes nothing" "the binary or env file exists"
    else
        pass "the refusal changes nothing"
    fi
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "the TLS-off refusal happens before a download" "curl was invoked: $(head -n1 "$STUB_CURL_ARGV")"
    else
        pass "the TLS-off refusal happens before a download"
    fi

    # ---- a bind chosen for TLS is provisional: set aside on every re-run (#449) ----
    # The env file carries 0.0.0.0 only because this script picked it for a TLS
    # host with no tailnet, and it said so with SOLADOR_AGENT_BIND_AUTO=1. A
    # re-run does not keep it: it resolves the bind again from scratch.
    rm -rf "$home"
    mkdir -p "$home/.config"
    printf 'FAKE-DER-BYTES' > "$home/.config/solador-agent.tls.crt"
    reset_argv_logs
    INSTALL_PATH="$TMP/stubs-no-tailscale:$TOOLBIN" INSTALL_TLS=unset INSTALL_STDIN="tok-auto
" run_install "$home"
    assert_eq "install.sh: no-tailnet TLS install (for the re-runs below)" "0" "$INSTALL_STATUS"
    assert_file_has "the auto-chosen bind is marked as such" "$env_file" "SOLADOR_AGENT_BIND_AUTO=1"
    env_before="$(cat "$env_file")"

    # ...TLS turned off, still no tailnet: refused, nothing changed, nothing
    # downloaded, and the remedy names BOTH keys (removing only the marker would
    # leave a bare 0.0.0.0 that the agent honours as an explicit bind).
    reset_argv_logs
    INSTALL_PATH="$TMP/stubs-no-tailscale:$TOOLBIN" INSTALL_TLS=0 INSTALL_STDIN="" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: a TLS-off re-run over an auto-chosen wildcard is refused" "1" "$INSTALL_STATUS"
    assert_output_has "the refusal says the bind was chosen by an earlier install" "$out" \
        "was chosen by an earlier install"
    assert_output_has "the refusal names BOTH keys to remove" "$out" \
        "remove BOTH"
    assert_output_has "the refusal names SOLADOR_AGENT_BIND_AUTO" "$out" "SOLADOR_AGENT_BIND_AUTO"
    assert_eq "the refused re-run leaves the env file unchanged" "$env_before" "$(cat "$env_file")"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "the refused re-run downloads nothing" "curl was invoked: $(head -n1 "$STUB_CURL_ARGV")"
    else
        pass "the refused re-run downloads nothing"
    fi

    # ...TLS stays on, still no tailnet: the wildcard is chosen again, and the
    # marker is REWRITTEN (a re-run that dropped it would turn a provisional
    # bind into an explicit one).
    reset_argv_logs
    INSTALL_PATH="$TMP/stubs-no-tailscale:$TOOLBIN" INSTALL_TLS=unset INSTALL_STDIN="" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: a TLS re-run over an auto-chosen wildcard" "0" "$INSTALL_STATUS"
    assert_output_has "the re-run resolves the bind again rather than keeping it" "$out" \
        "Binding to 0.0.0.0 (all interfaces: no Tailscale IP detected, and TLS is on)"
    assert_file_has "the auto marker is rewritten" "$env_file" "SOLADOR_AGENT_BIND_AUTO=1"
    assert_eq "the marker is written exactly once" "1" "$(grep -c '^SOLADOR_AGENT_BIND_AUTO=' "$env_file")"

    # ...a tailnet appears later, TLS on: the bind MOVES to the tailnet IP, the
    # marker goes with the wildcard it described, and the health probe (over
    # TLS) verifies the certificate as `localhost` while connecting to the new
    # bind, because the certificate's SAN list was fixed before that address.
    reset_argv_logs
    STUB_TAILSCALE_IP="100.64.0.9" INSTALL_TLS=unset INSTALL_STDIN="" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: a tailnet appearing after an auto wildcard (TLS on)" "0" "$INSTALL_STATUS"
    assert_file_has "the bind moves to the tailnet IP" "$env_file" "SOLADOR_AGENT_BIND=100.64.0.9"
    if grep -q "SOLADOR_AGENT_BIND_AUTO\|^SOLADOR_AGENT_BIND=0.0.0.0" "$env_file"; then
        fail "the wildcard and its marker are gone" "$(cat "$env_file")"
    else
        pass "the wildcard and its marker are gone"
    fi
    assert_output_has "the summary says that interface only" "$out" "Bind:    100.64.0.9:7878 — that interface only"
    assert_file_has "the probe verifies the certificate as localhost" "$STUB_CURL_ARGV" \
        "https://localhost:7878/v1/health"
    assert_file_has "the probe connects to the bind address" "$STUB_CURL_ARGV" \
        '[config] connect-to = "localhost:7878:100.64.0.9:7878"'
    unset STUB_TAILSCALE_IP

    # ...and with TLS turned off and a tailnet now up: the bind is the tailnet
    # IP too (plain HTTP on the tailnet only), never the kept wildcard.
    rm -rf "$home"
    mkdir -p "$home/.config"
    printf 'FAKE-DER-BYTES' > "$home/.config/solador-agent.tls.crt"
    reset_argv_logs
    INSTALL_PATH="$TMP/stubs-no-tailscale:$TOOLBIN" INSTALL_TLS=unset INSTALL_STDIN="tok-auto2
" run_install "$home"
    assert_file_has "the auto-chosen bind is marked as such (second host)" "$env_file" "SOLADOR_AGENT_BIND_AUTO=1"
    reset_argv_logs
    STUB_TAILSCALE_IP="100.64.0.9" INSTALL_TLS=0 INSTALL_STDIN="" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: a tailnet appearing after an auto wildcard (TLS off)" "0" "$INSTALL_STATUS"
    assert_file_has "TLS off: the bind is the tailnet IP" "$env_file" "SOLADOR_AGENT_BIND=100.64.0.9"
    assert_file_has "TLS off is recorded" "$env_file" "SOLADOR_AGENT_TLS=0"
    if grep -q "SOLADOR_AGENT_BIND_AUTO\|0\.0\.0\.0" "$env_file"; then
        fail "TLS off with a tailnet: no wildcard and no marker survive" "$(cat "$env_file")"
    else
        pass "TLS off with a tailnet: no wildcard and no marker survive"
    fi
    assert_output_has "the summary says that interface only, over plain HTTP on the tailnet" "$out" \
        "Bind:    100.64.0.9:7878 — that interface only"
    unset STUB_TAILSCALE_IP

    # An operator who names the address gets plain HTTP on that one interface,
    # and the marker goes with the bind it described.
    rm -rf "$home"
    mkdir -p "$home/.config"
    printf 'FAKE-DER-BYTES' > "$home/.config/solador-agent.tls.crt"
    reset_argv_logs
    INSTALL_PATH="$TMP/stubs-no-tailscale:$TOOLBIN" INSTALL_TLS=unset INSTALL_STDIN="tok-auto3
" run_install "$home"
    reset_argv_logs
    SOLADOR_AGENT_BIND="192.168.1.20" INSTALL_PATH="$TMP/stubs-no-tailscale:$TOOLBIN" INSTALL_TLS=0 \
        INSTALL_STDIN="" run_install "$home"
    assert_eq "install.sh: a TLS-off re-run with an explicit bind" "0" "$INSTALL_STATUS"
    assert_file_has "the explicit bind replaces the auto one" "$env_file" "SOLADOR_AGENT_BIND=192.168.1.20"
    if grep -q "SOLADOR_AGENT_BIND_AUTO" "$env_file"; then
        fail "the auto marker is dropped with the bind it described" "still in the env file"
    else
        pass "the auto marker is dropped with the bind it described"
    fi

    # ---- negative control: a wildcard WITHOUT the marker is the operator's choice ----
    # The documented plain-HTTP opt-in: honoured as it always was, so the
    # marker-driven set-aside above is proven to be the marker's doing — and the
    # summary says loudly what it is.
    rm -rf "$home"
    mkdir -p "$home/.config"
    printf 'SOLADOR_AGENT_TOKEN=explicit-tok\nSOLADOR_AGENT_BIND=0.0.0.0\nSOLADOR_AGENT_TLS=0\n' > "$env_file"
    reset_argv_logs
    INSTALL_PATH="$TMP/stubs-no-tailscale:$TOOLBIN" INSTALL_TLS=0 INSTALL_STDIN="" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: an explicit wildcard with TLS off is honoured" "0" "$INSTALL_STATUS"
    assert_file_has "the explicit wildcard is kept" "$env_file" "SOLADOR_AGENT_BIND=0.0.0.0"
    if grep -q "SOLADOR_AGENT_BIND_AUTO" "$env_file"; then
        fail "an explicit wildcard is never marked automatic" "the marker was written"
    else
        pass "an explicit wildcard is never marked automatic"
    fi
    assert_output_has "the run says it is kept from the env file" "$out" \
        "Binding to 0.0.0.0 (kept from the existing env file)"
    assert_output_has "the run warns loudly: plain HTTP on every interface" "$out" \
        "PLAIN HTTP ON EVERY INTERFACE"
    assert_output_has "the Done summary says plain HTTP on all interfaces" "$out" \
        "Bind:    0.0.0.0:7878 — ALL interfaces, PLAIN HTTP"

    # ---- an env file with a token but no bind, and no Tailscale ----
    rm -rf "$home"
    mkdir -p "$home/.config"
    printf 'SOLADOR_AGENT_TOKEN=nobind-tok\nSOLADOR_AGENT_TLS=0\n' > "$env_file"
    reset_argv_logs
    INSTALL_PATH="$TMP/stubs-no-tailscale:$TOOLBIN" INSTALL_TLS=unset INSTALL_STDIN="" run_install "$home"
    assert_eq "install.sh: existing TLS=0 and no bind is refused" "1" "$INSTALL_STATUS"
    out="$(cat "$INSTALL_OUT")"
    assert_output_has "the existing-TLS=0 refusal says no tailnet was found" "$out" \
        "could not detect a Tailscale IP for SOLADOR_AGENT_BIND, and TLS is off"
    assert_output_has "the existing-TLS=0 refusal names the TLS way out" "$out" "SOLADOR_AGENT_TLS=1"
    if [ -s "$STUB_CURL_ARGV" ]; then
        fail "the early refusal downloads nothing" "curl was invoked"
    else
        pass "the early refusal downloads nothing"
    fi
    printf 'FAKE-DER-BYTES' > "$home/.config/solador-agent.tls.crt"
    printf 'SOLADOR_AGENT_TOKEN=nobind-tok\nSOLADOR_AGENT_TLS=1\n' > "$env_file"
    reset_argv_logs
    INSTALL_PATH="$TMP/stubs-no-tailscale:$TOOLBIN" INSTALL_TLS=unset INSTALL_STDIN="" run_install "$home"
    assert_eq "install.sh: existing TLS=1 and no bind proceeds" "0" "$INSTALL_STATUS"
    assert_file_has "existing TLS=1 gets the all-interfaces bind" "$env_file" "SOLADOR_AGENT_BIND=0.0.0.0"
    printf 'SOLADOR_AGENT_TOKEN=nobind-tok\nSOLADOR_AGENT_TLS=0\n' > "$env_file"
    reset_argv_logs
    INSTALL_PATH="$TMP/stubs-no-tailscale:$TOOLBIN" INSTALL_TLS=unset INSTALL_STDIN="" run_install "$home" --enable-tls
    assert_eq "install.sh: --enable-tls with no bind proceeds" "0" "$INSTALL_STATUS"
    assert_file_has "--enable-tls gets the all-interfaces bind" "$env_file" "SOLADOR_AGENT_BIND=0.0.0.0"

    # ---- an explicit bind wins over the all-interfaces default ----
    rm -rf "$home"
    mkdir -p "$home/.config"
    printf 'FAKE-DER-BYTES' > "$home/.config/solador-agent.tls.crt"
    reset_argv_logs
    SOLADOR_AGENT_BIND="192.168.1.20" INSTALL_PATH="$TMP/stubs-no-tailscale:$TOOLBIN" INSTALL_TLS=unset \
        INSTALL_STDIN="tok-explicit
" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: an explicit bind with TLS on and no Tailscale" "0" "$INSTALL_STATUS"
    assert_file_has "the explicit bind reaches the env file" "$env_file" "SOLADOR_AGENT_BIND=192.168.1.20"
    assert_output_has "the summary says that interface only" "$out" "Bind:    192.168.1.20:7878 — that interface only"

    # ---- fresh, no Tailscale, staged binary predates TLS: refused after staging ----
    rm -rf "$FIXTURES" "$home"
    mkdir -p "$home"
    make_pre_tls_fixture 2026.9.5 x86_64-unknown-linux-musl "$TEST_KEY_DIR/a.key" >/dev/null
    SOLADOR_AGENT_RELEASE="v2026.9.5" reset_argv_logs
    SOLADOR_AGENT_RELEASE="v2026.9.5" INSTALL_PATH="$TMP/stubs-no-tailscale:$TOOLBIN" INSTALL_TLS=unset \
        INSTALL_STDIN="tok-late
" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: a no-tailnet fresh install onto a pre-TLS binary is refused" "1" "$INSTALL_STATUS"
    assert_output_has "the late refusal is printed" "$out" "TLS is off"
    # AFTER staging: the release was downloaded and its signature verified, the
    # candidate binary was run, and TLS was decided — that is what makes this the
    # late refusal and not the early one (which downloads nothing).
    assert_output_has "the late refusal came after the release was verified" "$out" "==> Verified "
    assert_output_has "the late refusal came after TLS was decided" "$out" "==> TLS: off"
    if [ -s "$STUB_CURL_ARGV" ]; then
        pass "the late refusal came after a download"
    else
        fail "the late refusal came after a download" "curl was never invoked"
    fi
    if [ -e "$env_file" ] || [ -e "$home/.local/bin/solador-agent" ]; then
        fail "the late refusal writes no env file and installs no binary" "one exists"
    else
        pass "the late refusal writes no env file and installs no binary"
    fi
    case "$out" in
        *"Binding to 0.0.0.0"*) fail "the late refusal never binds 0.0.0.0" "it did" ;;
        *) pass "the late refusal never binds 0.0.0.0" ;;
    esac

    # ---- an EXPLICIT SOLADOR_AGENT_TLS=1 against that same pre-TLS release ----
    # The staged binary would ignore the variable and serve plain HTTP, so the
    # wildcard fallback ("TLS is on") must not be taken on the strength of a
    # setting the binary cannot honour: refused after staging, nothing written.
    rm -rf "$home"
    mkdir -p "$home"
    reset_argv_logs
    SOLADOR_AGENT_RELEASE="v2026.9.5" INSTALL_PATH="$TMP/stubs-no-tailscale:$TOOLBIN" INSTALL_TLS=1 \
        INSTALL_STDIN="tok-late2
" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: explicit TLS=1, no tailnet, pre-TLS binary is refused" "1" "$INSTALL_STATUS"
    assert_output_has "the refusal came after staging" "$out" "==> Verified "
    assert_output_has "the refusal names the pre-TLS binary" "$out" "predates #447 and"
    assert_output_has "the refusal offers dropping the explicit TLS=1" "$out" "drop SOLADOR_AGENT_TLS=1"
    case "$out" in
        *"turn TLS on (SOLADOR_AGENT_TLS=1"* | *"serve HTTPS (SOLADOR_AGENT_TLS=1"*)
            fail "the refusal does not advise setting the TLS=1 the operator already set" "it did" ;;
        *) pass "the refusal does not advise setting the TLS=1 the operator already set" ;;
    esac
    if [ -e "$env_file" ] || [ -e "$home/.local/bin/solador-agent" ]; then
        fail "explicit TLS=1 against a pre-TLS binary binds nothing and installs nothing" "one exists"
    else
        pass "explicit TLS=1 against a pre-TLS binary binds nothing and installs nothing"
    fi
    case "$out" in
        *"Binding to 0.0.0.0"*) fail "explicit TLS=1 against a pre-TLS binary never binds 0.0.0.0" "it did" ;;
        *) pass "explicit TLS=1 against a pre-TLS binary never binds 0.0.0.0" ;;
    esac

    INSTALL_PATH=""
    unset SOLADOR_AGENT_RELEASE STUB_CURL_BODY
}

# A release published before #447 answers `--version` but refuses
# `tls-fingerprint` as an unrecognized argument (exit 2) — install.sh's
# capability probe reads exactly that, so defaulting TLS on for a fresh
# install must depend on the STAGED bytes, never on this script's own
# revision.
test_install_tls_capability_gate() {
    local home="$TMP/home-tls-capability" env_file bin out
    if [ "$HAVE_MINISIGN" != true ]; then
        skip_needs_minisign "install.sh: a fresh install does not default TLS on when the staged binary predates it"
        skip_needs_minisign "install.sh: --enable-tls is refused against a binary that predates it"
        return
    fi
    make_checkout "$TEST_KEY_DIR/a.pub"
    rm -rf "$FIXTURES" "$home"
    mkdir -p "$home"
    make_pre_tls_fixture 2026.9.5 x86_64-unknown-linux-musl "$TEST_KEY_DIR/a.key" >/dev/null
    export SOLADOR_AGENT_RELEASE="v2026.9.5"
    export STUB_CURL_BODY='{"status":"ok","hostname":"h","version":"2026.9.5"}'
    export STUB_TAILSCALE_IP="100.64.0.9"

    env_file="$home/.config/solador-agent.env"
    bin="$home/.local/bin/solador-agent"

    # ---- a fresh install still succeeds, but TLS stays off ----
    reset_argv_logs
    INSTALL_TLS=unset INSTALL_STDIN="tok-pre-tls
" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: a fresh install with a pre-#447 binary still succeeds" "0" "$INSTALL_STATUS"
    [ -x "$bin" ] || fail "the binary is installed" "$out"
    assert_file_has "TLS stays off: the staged binary cannot serve it" "$env_file" "SOLADOR_AGENT_TLS=0"
    assert_output_has "the run explains why, naming #447" "$out" "predates #447"

    # ---- --enable-tls against the same binary is refused, not silently ignored ----
    # A RE-run (the env file from the fresh install above already exists),
    # not another fresh install: on a fresh install --enable-tls is always
    # redundant with the (capability-gated) default, so it takes a second
    # run to reach the --enable-tls branch of install.sh's own precedence at
    # all — the same reason a real operator would only pass the flag once
    # they already have a host installed and want to turn TLS on.
    ENV_BEFORE="$(cat "$env_file")"
    reset_argv_logs
    INSTALL_TLS=unset INSTALL_STDIN="" run_install "$home" --enable-tls
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: --enable-tls is refused against a binary that predates it" "1" "$INSTALL_STATUS"
    assert_output_has "the refusal names the reason" "$out" "does not support TLS"
    assert_eq "a refused --enable-tls leaves the env file byte-for-byte" "$ENV_BEFORE" "$(cat "$env_file")"

    # ---- a re-run with an EXISTING SOLADOR_AGENT_TLS=1 is refused too, not
    # silently kept on (#447 review round 2). Simulates SOLADOR_AGENT_RELEASE
    # pinned back to a release before #447 — the exact downgrade install.sh's
    # own error messages recommend for other problems — on a host that
    # already had TLS on: the "kept from the existing env file" branch must
    # not skip the capability check just because no flag was given.
    printf 'SOLADOR_AGENT_TOKEN=tok-existing-tls\nSOLADOR_AGENT_BIND=127.0.0.1\nSOLADOR_AGENT_PORT=7878\nSOLADOR_AGENT_TLS=1\n' \
        > "$env_file"
    ENV_BEFORE="$(cat "$env_file")"
    reset_argv_logs
    INSTALL_TLS=unset INSTALL_STDIN="" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: a re-run with an existing TLS=1 is refused against a binary that predates it" \
        "1" "$INSTALL_STATUS"
    assert_output_has "the refusal names the reason" "$out" "predates #447 and does not support TLS"
    assert_output_has "the refusal names the SOLADOR_AGENT_TLS=0 override" "$out" "SOLADOR_AGENT_TLS=0"
    assert_eq "the refusal leaves the env file byte-for-byte" "$ENV_BEFORE" "$(cat "$env_file")"

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
    local home="$TMP/home-timer" env_file unit bin out update_unit update_timer guard
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
    guard="$home/.local/bin/solador-agent-update-guard"

    # ---- a fresh default install: metrics yes, updater no, no check ----
    reset_argv_logs
    INSTALL_STDIN="fresh-tok-MUST-NOT-BE-PRINTED
" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: fresh default install (Linux, no --enable-timer)" "0" "$INSTALL_STATUS"
    assert_file_has "the default install still enables the metrics unit" "$STUB_SYSTEMCTL_ARGV" "--user enable solador-agent"
    assert_file_has "the default install still restarts the metrics unit" "$STUB_SYSTEMCTL_ARGV" "--user restart solador-agent"
    if updater_installed "$home"; then
        fail "a default install writes no updater unit, timer or guard" "$(ls "$home/.config/systemd/user" "$home/.local/bin")"
    else
        pass "a default install writes no updater unit, timer or guard"
    fi
    [ -e "$guard" ] && fail "a default install installs no guard" "$guard exists" \
        || pass "a default install installs no guard"
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
    if grep -q '@SOLADOR_AGENT_' "$update_unit"; then
        fail "the oneshot's placeholders were all rendered" "$(grep '@SOLADOR_AGENT_' "$update_unit")"
    else
        pass "the oneshot's placeholders were all rendered"
    fi
    # The guard (#411): installed with the opt-in, executable, byte-for-byte
    # the checkout's script, and named by the oneshot twice — as the
    # ExecCondition= (Exec-quoted, with %n so it learns its own unit) and as
    # the AssertFileIsExecutable= (a bare path) that makes a deleted guard an
    # assertion failure rather than a silent daily skip.
    [ -x "$guard" ] && pass "--enable-timer installs the guard, executable" \
        || fail "--enable-timer installs the guard, executable" "$(ls -la "$home/.local/bin" 2>&1)"
    assert_eq "the installed guard is mode 0755" "755" "$(file_mode "$guard" 2>/dev/null)"
    if cmp -s "$SCRIPT_DIR/update-guard.sh" "$guard"; then
        pass "the installed guard is the checkout's update-guard.sh, byte for byte"
    else
        fail "the installed guard is the checkout's update-guard.sh, byte for byte" "it differs"
    fi
    assert_file_has "the oneshot runs the installed guard as its ExecCondition, handing it %n" \
        "$update_unit" "ExecCondition=$guard %n"
    assert_file_has "the oneshot asserts the guard is executable before every start" \
        "$update_unit" "AssertFileIsExecutable=$guard"
    assert_before "the guard's ExecCondition precedes ExecStart in the oneshot" \
        "$update_unit" "ExecCondition=" "ExecStart="
    assert_output_has "the opt-in run names the guard" "$out" "guarded by $guard"
    # The guard is installed BEFORE the oneshot that names it is written —
    # a unit whose ExecCondition binary is absent skips every firing as exec
    # failure 203, with Result=success — so the order is asserted on the
    # opt-in function's own body, the way redeploy.sh's swap order is.
    local optin_body="$TMP/install-optin-linux.sh"
    extract_function "$CHECKOUT/agent/deploy/install.sh" "install_update_timer_linux" > "$optin_body"
    assert_before "install.sh installs the guard before it renders the oneshot" \
        "$optin_body" 'mv -f "$GUARD_DST.new" "$GUARD_DST"' 'render_template "$UPDATE_UNIT_SRC"'
    assert_before "install.sh stages the guard beside the live path before renaming it over" \
        "$optin_body" 'install -m 0755 "$GUARD_SRC" "$GUARD_DST.new"' 'mv -f "$GUARD_DST.new" "$GUARD_DST"'
    [ -e "$guard.new" ] && fail "install.sh leaves no guard .new behind" "$guard.new exists" \
        || pass "install.sh leaves no guard .new behind"
    assert_before "install.sh renders the oneshot before it reloads the manager" \
        "$optin_body" 'render_template "$UPDATE_UNIT_SRC"' 'systemctl --user daemon-reload'
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
    local unit_before timer_before guard_before
    unit_before="$(cat "$update_unit")"
    timer_before="$(cat "$update_timer")"
    guard_before="$(cat "$guard")"
    reset_argv_logs
    INSTALL_STDIN="" run_install "$home"
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: a no-flag re-run over an opted-in host" "0" "$INSTALL_STATUS"
    assert_eq "a no-flag re-run leaves the oneshot as it was" "$unit_before" "$(cat "$update_unit")"
    assert_eq "a no-flag re-run leaves the timer as it was" "$timer_before" "$(cat "$update_timer")"
    assert_eq "a no-flag re-run leaves the guard as it was" "$guard_before" "$(cat "$guard")"
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
    assert_output_has "a no-flag re-run reports the opt-in as enabled and guarded, from systemd's answer" "$out" "Unattended updates: enabled, guarded by $guard (solador-agent-update.timer"
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
    # is that a no-flag re-run after the files are removed — the two units
    # and the guard, the three the README's remove command names — does not
    # bring any of them back.
    rm -f "$update_unit" "$update_timer" "$guard"
    reset_argv_logs
    INSTALL_STDIN="" run_install "$home"
    assert_eq "install.sh: a no-flag re-run after the updater was removed" "0" "$?"
    if updater_installed "$home"; then
        fail "a no-flag re-run does not re-create a removed updater (units or guard)" "$(ls "$home/.config/systemd/user" "$home/.local/bin")"
    else
        pass "a no-flag re-run does not re-create a removed updater (units or guard)"
    fi
    [ -e "$bin" ] && pass "removing the updater leaves the metrics binary in place" \
        || fail "removing the updater leaves the metrics binary in place" "$bin is gone"
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
    rm -f "$update_unit" "$update_timer" "$guard"
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
    assert_file_has "a spaced guard path is double-quoted in the oneshot's ExecCondition, then %n" \
        "$spaced/.config/systemd/user/solador-agent-update.service" \
        "ExecCondition=\"$spaced/.local/bin/solador-agent-update-guard\" %n"
    assert_file_has "a spaced guard path is bare in the oneshot's AssertFileIsExecutable" \
        "$spaced/.config/systemd/user/solador-agent-update.service" \
        "AssertFileIsExecutable=$spaced/.local/bin/solador-agent-update-guard"
    [ -x "$spaced/.local/bin/solador-agent-update-guard" ] && pass "the guard is installed under a spaced HOME" \
        || fail "the guard is installed under a spaced HOME" "not executable or absent"

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

    # ---- a manager too old for ExecCondition= refuses the opt-in (#411) ----
    # systemd < 243 logs "Unknown key 'ExecCondition'" and runs the updater
    # unguarded — not the opt-in that was asked for. The version is the
    # RUNNING user manager's `Version` property (not the client's
    # `--version`), in the shapes real managers print: a Fedora/Debian
    # `NNN.x-y…`, a bare `219` (RHEL 7), and an unreadable one. Refused
    # before a byte is downloaded; a default install on the same manager is
    # untouched by the gate, and 243 itself is accepted.
    rm -rf "$home"
    mkdir -p "$home"
    reset_argv_logs
    STUB_SYSTEMD_VERSION="242.4-4ubuntu1" run_install "$home" --enable-timer
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: --enable-timer on systemd 242 is refused" "1" "$INSTALL_STATUS"
    assert_output_has "the old-systemd refusal names the version and the floor" "$out" "systemd 242 is older than 243"
    assert_output_has "the old-systemd refusal says what the guard would have been" "$out" "ExecCondition="
    assert_file_has "the gate asks the running manager, not the client" "$STUB_SYSTEMCTL_ARGV" "--user show -p Version --value"
    assert_untouched "the old-systemd refusal changes nothing" "$home"
    reset_argv_logs
    STUB_SYSTEMD_VERSION="219" run_install "$home" --enable-timer
    assert_eq "install.sh: --enable-timer on a bare '219' is refused" "1" "$?"
    assert_output_has "the bare-version refusal parsed it" "$(cat "$INSTALL_OUT")" "systemd 219 is older than 243"
    reset_argv_logs
    STUB_SYSTEMD_VERSION="" run_install "$home" --enable-timer
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh: --enable-timer on a systemd whose version cannot be read is refused" "1" "$INSTALL_STATUS"
    assert_output_has "the unreadable-version refusal quotes what systemctl said" "$out" "cannot read the running systemd version"
    assert_untouched "the unreadable-version refusal changes nothing" "$home"
    reset_argv_logs
    STUB_SYSTEMD_VERSION="242.4-4ubuntu1" INSTALL_STDIN="gate-tok-MUST-NOT-BE-PRINTED
" run_install "$home"
    assert_eq "install.sh: a default install on systemd 242 is not gated" "0" "$?"
    if updater_installed "$home"; then
        fail "the ungated default install still writes no updater" "it did"
    else
        pass "the ungated default install still writes no updater"
    fi
    reset_argv_logs
    STUB_SYSTEMD_VERSION="243.11-1~deb10u1" INSTALL_STDIN="" run_install "$home" --enable-timer
    assert_eq "install.sh: --enable-timer on systemd 243 (the floor) proceeds" "0" "$?"
    [ -x "$guard" ] && pass "systemd 243 gets the guard" || fail "systemd 243 gets the guard" "$(cat "$INSTALL_OUT")"
    assert_file_has "the oneshot pins PATH to the system directories" "$update_unit" "Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
    assert_file_has "the oneshot unsets the guard's test seam, and bash's own startup-file seam" "$update_unit" "UnsetEnvironment=SOLADOR_AGENT_UPDATE_GUARD_ROOT BASH_ENV ENV"
    assert_output_has "an opted-in, guarded host is reported as guarded" "$(cat "$INSTALL_OUT")" "enabled, guarded by $guard"

    # ---- a pre-#411 opt-in (no ExecCondition=) is reported as UNGUARDED ----
    # The no-flag re-run leaves it exactly as it is, so the summary must
    # not let it read as a guarded host; the flag is how the guard arrives.
    printf '[Unit]\nDescription=pre-#411 oneshot\n\n[Service]\nType=oneshot\nExecStart=%s update\nSuccessExitStatus=4\n' "$bin" > "$update_unit"
    rm -f "$guard"
    reset_argv_logs
    INSTALL_STDIN="" run_install "$home"
    assert_eq "install.sh: a no-flag re-run over a pre-#411 opt-in" "0" "$?"
    assert_output_has "a pre-#411 opt-in is reported as enabled but UNGUARDED" "$(cat "$INSTALL_OUT")" "enabled but UNGUARDED"
    assert_output_has "the UNGUARDED report names the re-run that fixes it" "$(cat "$INSTALL_OUT")" "re-run with --enable-timer to install the guard"
    if grep -q '^ExecCondition=' "$update_unit"; then
        fail "a no-flag re-run does not retrofit the guard into a pre-#411 unit" "it did"
    else
        pass "a no-flag re-run does not retrofit the guard into a pre-#411 unit"
    fi
    reset_argv_logs
    INSTALL_STDIN="" run_install "$home" --enable-timer
    assert_eq "install.sh: --enable-timer over a pre-#411 opt-in" "0" "$?"
    assert_file_has "the flagged re-run retrofits the guard" "$update_unit" "ExecCondition=$guard %n"
    assert_output_has "the retrofitted host is reported as guarded" "$(cat "$INSTALL_OUT")" "enabled, guarded by $guard"

    # ---- a #411 unit whose guard file is gone is the OPPOSITE state ----
    # AssertFileIsExecutable= fails every start, so nothing runs — not
    # "unguarded", and the summary must not say a firing at wake would
    # run. Only the guard is removed; the unit still names it.
    rm -f "$guard"
    reset_argv_logs
    INSTALL_STDIN="" run_install "$home"
    assert_eq "install.sh: a no-flag re-run over a unit whose guard is missing" "0" "$?"
    assert_output_has "a missing guard is reported as MISSING, not unguarded" "$(cat "$INSTALL_OUT")" "its guard $guard is MISSING"
    assert_output_has "the MISSING report says no check runs" "$(cat "$INSTALL_OUT")" "every start fails the unit's assertion and no check runs"
    if grep -q 'UNGUARDED' "$INSTALL_OUT"; then
        fail "a missing guard is not reported as UNGUARDED" "it is"
    else
        pass "a missing guard is not reported as UNGUARDED"
    fi
    reset_argv_logs
    INSTALL_STDIN="" run_install "$home" --enable-timer
    assert_eq "install.sh: --enable-timer reinstalls a missing guard" "0" "$?"
    [ -x "$guard" ] && pass "the flagged re-run reinstalls the missing guard" || fail "the flagged re-run reinstalls the missing guard" "$(cat "$INSTALL_OUT")"

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

    # SOLADOR_AGENT_CONFIG_DIR (#447): where SOLADOR_AGENT_TLS=1 finds or
    # creates solador-agent.tls.key/solador-agent.tls.crt. The launcher
    # exports it derived from $env_file itself (dirname), never read from a
    # line inside the file — so
    # main.rs's tls_config_dir() need not fall back to $HOME, which can
    # differ from install.sh's HOME under launchd (see run-agent.sh's own
    # header comment).
    cat > "$probe" <<'STUB'
#!/bin/sh
printf 'CONFIG_DIR=%s\n' "${SOLADOR_AGENT_CONFIG_DIR:-<unset>}"
STUB
    run_launcher "$probe" "$env_file" "$log"
    assert_eq "the launcher exports SOLADOR_AGENT_CONFIG_DIR as the env file's own directory" \
        "CONFIG_DIR=$(dirname "$env_file")" "$(cat "$INSTALL_OUT")"

    # The launcher's allow-list is a copy of the keys the agent reads. Bind
    # the two: every SOLADOR_AGENT_* the Rust source names must be in the
    # launcher, or a key added to the agent reaches Linux (EnvironmentFile=
    # passes everything) and is silently dropped on macOS.
    #
    # Two named exceptions, and it is a positive list so the next key still
    # trips this. SOLADOR_AGENT_LAUNCHD_LABEL is read by `solador-agent
    # update`/`rollback` (#393) from the MAINTENANCE command's own
    # environment — the same test seam install.sh honours, so a throwaway
    # LaunchAgent can be updated beside a real one — and never from the env
    # file. SOLADOR_AGENT_CONFIG_DIR (#447, above) is exported by the
    # launcher itself, derived from $env_file's own path — never read from a
    # line IN the file — so it carries no `KEY=*` case pattern for this grep
    # to find; checked separately just above instead. The metrics service
    # does not read either from a line of the file, so the launcher's
    # case-statement has nothing to export for them.
    #
    # And ONE the other way round (#449): SOLADOR_AGENT_BIND_AUTO is written by
    # install.sh, never read by the agent, so the launcher names it (a silent
    # arm, so it is a recognised line and is not logged as "unrecognised") and
    # the Rust source does not read it. Excluded from the launcher's side, and
    # from the agent's side only as a NAME in a message (the TLS-off wildcard
    # warning tells an operator what to remove): an env::var/var_os read of it,
    # or a `.get(`/`.remove(`/`.contains_key(` call or match arm naming it as a
    # string literal, is asserted absent right below — as is `map["…"]` indexing
    # and a `const`/`static` binding the name to a constant (the
    # agent/src/metrics.rs pattern), through which a read would go. A read of a
    # constant is caught at the constant's definition, so a caller that holds
    # only the constant's name is covered.
    local agent_keys launcher_keys
    # SOLADOR_AGENT_TEST_* are test-harness switches read only by #[cfg(test)]
    # code (SOLADOR_AGENT_TEST_REQUIRE_NONLOOPBACK): not service configuration.
    agent_keys="$(grep -rhoE 'SOLADOR_AGENT_[A-Z_]+' "$SCRIPT_DIR/../src" | sort -u | grep -v '^SOLADOR_AGENT_TEST_' \
        | grep -vx -e 'SOLADOR_AGENT_LAUNCHD_LABEL' -e 'SOLADOR_AGENT_CONFIG_DIR' -e 'SOLADOR_AGENT_BIND_AUTO' | tr '\n' ' ')"
    launcher_keys="$(grep -oE 'SOLADOR_AGENT_[A-Z_]+=\*' "$launcher" | sed 's/=\*$//' | sort -u \
        | grep -vx 'SOLADOR_AGENT_BIND_AUTO' | tr '\n' ' ')"
    # Any line naming the key that is not inside a string that is only a
    # message: a read looks like var("..."), var_os("..."), .get("...") or a
    # match arm. Flag every non-comment occurrence that has one of those shapes.
    if grep -rnE '(var|var_os|get|remove|contains_key)\(\s*"SOLADOR_AGENT_BIND_AUTO"|"SOLADOR_AGENT_BIND_AUTO"\s*=>|\[\s*"SOLADOR_AGENT_BIND_AUTO"\s*\]|(const|static)[^=]*=\s*"SOLADOR_AGENT_BIND_AUTO"' "$SCRIPT_DIR/../src" | grep -q .; then
        fail "the agent source does not read SOLADOR_AGENT_BIND_AUTO" \
            "$(grep -rnE '(var|var_os|get|remove|contains_key)\(\s*"SOLADOR_AGENT_BIND_AUTO"|"SOLADOR_AGENT_BIND_AUTO"\s*=>|\[\s*"SOLADOR_AGENT_BIND_AUTO"\s*\]|(const|static)[^=]*=\s*"SOLADOR_AGENT_BIND_AUTO"' "$SCRIPT_DIR/../src")"
    else
        pass "the agent source does not read SOLADOR_AGENT_BIND_AUTO"
    fi
    assert_eq "the launcher allow-lists every SOLADOR_AGENT_* key the agent reads" \
        "$agent_keys" "$launcher_keys"

    # The marker is a silent arm (#449): an auto-bound host's env file carries
    # SOLADOR_AGENT_BIND_AUTO=1, and the launcher must neither log "ignoring
    # unrecognised line" about it on every start nor export it. A truly unknown
    # key on the next line still IS logged, so the arm is not a blanket
    # silencer.
    cat > "$probe" <<'STUB'
#!/bin/sh
printf 'AUTO=%s\n' "${SOLADOR_AGENT_BIND_AUTO:-<unset>}"
STUB
    printf 'SOLADOR_AGENT_TOKEN=tok\nSOLADOR_AGENT_BIND=0.0.0.0\nSOLADOR_AGENT_TLS=1\nSOLADOR_AGENT_BIND_AUTO=1\nSOLADOR_AGENT_BOGUS=1\n' > "$env_file"
    run_launcher "$probe" "$env_file" "$log"
    assert_eq "the launcher does not export the auto-bind marker" "AUTO=<unset>" "$(cat "$INSTALL_OUT")"
    if grep -q "unrecognised line 4" "$STDERR"; then
        fail "the auto-bind marker is a recognised line" "the launcher logged it as unrecognised"
    else
        pass "the auto-bind marker is a recognised line"
    fi
    assert_file_has "an unknown key IS still logged as unrecognised" "$STDERR" "unrecognised line 5"
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

# The Linux guard (#411): solador-agent-update.service's ExecCondition=, run
# directly with every input it reads stubbed — the wall clock, the two
# manager reads (this activation's time from the user manager, the last
# resume from the system manager), the kernel's suspend counter under the
# override root, HOME and INVOCATION_ID — so "a firing seconds after
# resume", "a boot that has not slept", "a laptop that slept last night" and
# "a manager that cannot be reached" are states the test sets rather than
# waits for. Its exit status is systemd's ExecCondition contract: 0 runs
# ExecStart, 1 skips it cleanly (unit inactive, not failed), 255 fails the
# unit. Each of those three was observed on a real user manager while the
# guard was designed (systemd 256; the guard's header records the readings);
# what no test here observes is a suspend, which no machine this suite runs
# on can be asked for. The macOS launcher's cases above are the model, and
# the two guards' boundaries are the same numbers on purpose.
test_update_guard_linux() {
    local guard="$SCRIPT_DIR/update-guard.sh" ghome="$TMP/guard-home" root="$TMP/guard-root"
    local stamp counter unit=solador-agent-update.service out
    rm -rf "$ghome" "$root"
    mkdir -p "$ghome/.config"
    stamp="$ghome/.config/solador-agent-update.last-attempt"
    counter="$root/sys/power/suspend_stats/success"
    mkdir -p "$(dirname "$counter")"
    export STUB_GUARD_SYSTEMCTL_ARGV="$TMP/guard-systemctl-argv"
    # run_guard [args...]: the guard as the user manager would start it —
    # HOME and INVOCATION_ID set, the clock and manager stubs ahead of PATH,
    # the sysfs root overridden — under umask 022, so a 0600 stamp is the
    # guard's doing. Both streams to $INSTALL_OUT; the status is returned.
    # GUARD_NO_INVOCATION=1 leaves INVOCATION_ID unset; GUARD_HOME overrides
    # HOME (the word "unset" unsets it); GUARD_BASH_ENV names a file bash
    # sources before the script (BASH_ENV, bash(1)) — the one way to make the
    # guard die somewhere it did not plan for without a hook in the guard.
    run_guard() {
        : > "$STUB_GUARD_SYSTEMCTL_ARGV"
        (
            export SOLADOR_AGENT_UPDATE_GUARD_ROOT="$root"
            export PATH="$STUBS_CLOCK:$PATH"
            if [ -n "${GUARD_BASH_ENV:-}" ]; then
                export BASH_ENV="$GUARD_BASH_ENV"
            else
                unset BASH_ENV
            fi
            if [ "${GUARD_NO_INVOCATION:-}" = 1 ]; then
                unset INVOCATION_ID
            else
                export INVOCATION_ID="53c52280aec6457493cabd9c3b09eb66"
            fi
            case "${GUARD_HOME-}" in
                '') export HOME="$ghome" ;;
                unset) unset HOME ;;
                *) export HOME="$GUARD_HOME" ;;
            esac
            umask 022
            "$BASH" "$guard" "$@"
        ) >"$INSTALL_OUT" 2>&1
    }
    # The guard's whole conversation with the manager: exactly the two
    # read-only `show` lines, never a start, stop, restart, enable or reload.
    assert_guard_only_read() {
        local name="$1" lines
        lines="$(grep -cve '^\(--user \)\?show -p ' "$STUB_GUARD_SYSTEMCTL_ARGV" 2>/dev/null || true)"
        if [ "${lines:-0}" -eq 0 ] && [ "$(wc -l < "$STUB_GUARD_SYSTEMCTL_ARGV" | tr -d ' ')" -eq 2 ]; then
            pass "$name"
        else
            fail "$name" "$(cat "$STUB_GUARD_SYSTEMCTL_ARGV")"
        fi
    }
    local now=1789237294   # an arbitrary epoch second
    local day_us=86400000000
    unset STUB_MONO_NOW_US STUB_RESUME_US STUB_DATE_EXIT GUARD_HOME GUARD_NO_INVOCATION GUARD_BASH_ENV
    export STUB_DATE_EPOCH="$now"

    # ---- a host that has never slept, first firing, no earlier attempt: it runs ----
    # Up two and a half days on CLOCK_MONOTONIC, the manager reporting no
    # sleep.target cycle and the kernel counting no suspend.
    printf '0\n' > "$counter"
    export STUB_MONO_NOW_US=$((216000 * 1000000)) STUB_RESUME_US=0
    rm -f "$stamp"
    run_guard "$unit"
    assert_eq "guard: a scheduled firing on a boot that has not slept, no earlier attempt, exits 0 (run)" "0" "$?"
    assert_eq "guard: the attempt is stamped with the clock's time" "$now" "$(cat "$stamp" 2>/dev/null)"
    assert_eq "guard: the stamp is mode 0600 under umask 022" "600" "$(file_mode "$stamp" 2>/dev/null)"
    assert_output_has "guard: the run line says no suspend was counted this boot" "$(cat "$INSTALL_OUT")" "RUN — letting $unit run: no suspend counted this boot, up 216000s"
    assert_output_has "guard: the run line names the unit it lets run" "$(cat "$INSTALL_OUT")" "letting $unit run"
    [ -e "$stamp.new" ] && fail "guard: no stamp .new is left behind" "$stamp.new exists" \
        || pass "guard: no stamp .new is left behind"
    assert_guard_only_read "guard: the run path asks the manager for exactly its two read-only properties"
    assert_file_has "guard: this activation's time is read from the user manager for %n" \
        "$STUB_GUARD_SYSTEMCTL_ARGV" "--user show -p InactiveExitTimestampMonotonic --value $unit"
    assert_file_has "guard: the last resume is read from the system manager's sleep.target" \
        "$STUB_GUARD_SYSTEMCTL_ARGV" "show -p InactiveEnterTimestampMonotonic --value sleep.target"

    # ---- the same firing again (a duplicate or coalesced one): discarded ----
    run_guard "$unit"
    assert_eq "guard: a second firing inside the interval exits 1 (skip)" "1" "$?"
    assert_output_has "guard: the discarded firing says why" "$(cat "$INSTALL_OUT")" "last attempt was 0s ago"
    assert_eq "guard: a discarded firing does not move the stamp" "$now" "$(cat "$stamp")"

    # ---- the interval boundary: 23h less a second skips, 23h runs ----
    printf '%s\n' "$((now - 82799))" > "$stamp"
    run_guard "$unit"
    assert_eq "guard: 82799s since the last attempt is inside the interval (exit 1)" "1" "$?"
    printf '%s\n' "$((now - 82800))" > "$stamp"
    run_guard "$unit"
    assert_eq "guard: 82800s since the last attempt runs (exit 0)" "0" "$?"
    assert_output_has "guard: the run line carries the last attempt's age" "$(cat "$INSTALL_OUT")" "last attempt 82800s ago"

    # ---- a firing seconds after a resume, a day since the last attempt: the
    # wake-time catch-up the decision forbids, and it is skipped ----
    printf '%s\n' "$((now - 90000))" > "$stamp"
    printf '3\n' > "$counter"
    export STUB_RESUME_US=$((STUB_MONO_NOW_US - 30 * 1000000))
    run_guard "$unit"
    assert_eq "guard: a firing 30s after resume exits 1 (skip)" "1" "$?"
    assert_output_has "guard: the wake-time skip names the cadence and the threshold" "$(cat "$INSTALL_OUT")" "DISCARD — the system resumed 30s ago; a check within 300s of a wake"
    assert_output_has "guard: the wake-time skip says the next firing is a day away" "$(cat "$INSTALL_OUT")" "next scheduled firing is a day away"
    assert_eq "guard: a wake-time skip does not move the stamp" "$((now - 90000))" "$(cat "$stamp")"
    # 299s is still "just resumed"; 300s is not.
    export STUB_RESUME_US=$((STUB_MONO_NOW_US - 299 * 1000000))
    run_guard "$unit"
    assert_eq "guard: 299s after resume is still a wake-time firing (exit 1)" "1" "$?"
    export STUB_RESUME_US=$((STUB_MONO_NOW_US - 300 * 1000000))
    run_guard "$unit"
    assert_eq "guard: 300s after resume is a scheduled firing (exit 0)" "0" "$?"
    assert_output_has "guard: the run line carries the resume's age" "$(cat "$INSTALL_OUT")" "last resume 300s ago"

    # ---- a laptop that slept last night and has been up since: it runs ----
    printf '%s\n' "$((now - 90000))" > "$stamp"
    export STUB_RESUME_US=$((STUB_MONO_NOW_US - 8 * 3600 * 1000000))
    run_guard "$unit"
    assert_eq "guard: a firing eight hours after last night's resume runs (exit 0)" "0" "$?"
    assert_output_has "guard: a suspend the kernel counted and the manager recorded is not a hold" \
        "$(cat "$INSTALL_OUT")" "last resume 28800s ago"

    # ---- a firing seconds after boot (no resume this boot) ----
    printf '%s\n' "$((now - 90000))" > "$stamp"
    printf '0\n' > "$counter"
    export STUB_RESUME_US=0 STUB_MONO_NOW_US=$((30 * 1000000))
    run_guard "$unit"
    assert_eq "guard: a firing 30s after boot exits 1 (skip)" "1" "$?"
    assert_output_has "guard: the boot-time skip says so, with the threshold" "$(cat "$INSTALL_OUT")" "DISCARD — the system booted 30s ago and the kernel counts no suspend since; a check within 300s of a boot"
    export STUB_MONO_NOW_US=$((300 * 1000000))
    run_guard "$unit"
    assert_eq "guard: 300s after boot is a scheduled firing (exit 0)" "0" "$?"
    export STUB_MONO_NOW_US=$((216000 * 1000000))

    # ---- a clock that moved backwards past the last attempt: skipped ----
    printf '%s\n' "$((now + 100))" > "$stamp"
    run_guard "$unit"
    assert_eq "guard: a clock behind the last attempt exits 1 (skip)" "1" "$?"
    assert_output_has "guard: the backwards clock is named" "$(cat "$INSTALL_OUT")" "moved backwards"
    assert_eq "guard: a backwards-clock skip does not move the stamp" "$((now + 100))" "$(cat "$stamp")"
    # A stamp a whole interval in the future would discard every firing
    # until the calendar reached it: one interval ahead still discards (the
    # clock catches up within a day), one second more is a hold naming the file.
    printf '%s\n' "$((now + 82800))" > "$stamp"
    run_guard "$unit"
    assert_eq "guard: a stamp exactly one interval ahead still exits 1 (skip)" "1" "$?"
    printf '%s\n' "$((now + 82801))" > "$stamp"
    run_guard "$unit"
    assert_eq "guard: a stamp more than one interval ahead exits 255 (hold)" "255" "$?"
    assert_output_has "guard: the far-future stamp is named with its lead" "$(cat "$INSTALL_OUT")" "82801s in the future"
    assert_output_has "guard: the far-future stamp hold says to remove the file" "$(cat "$INSTALL_OUT")" "Remove that file"
    assert_eq "guard: the far-future stamp hold does not rewrite the stamp" "$((now + 82801))" "$(cat "$stamp")"

    # ---- a hand-edited stamp: leading zeros, padding, CRLF are read as decimal ----
    # `09` is an octal error in bash arithmetic, and under `set -e` that
    # ends the script with status 1 — a clean skip with no log line, the
    # one shape the guard must never produce. Verified under /bin/bash 3.2
    # before the `10#` prefix went in.
    printf '0%s\n' "$((now - 90000))" > "$stamp"
    run_guard "$unit"
    assert_eq "guard: a stamp with a leading zero is read as decimal and runs (exit 0)" "0" "$?"
    printf '  %s\r\n' "$((now - 90000))" > "$stamp"
    run_guard "$unit"
    assert_eq "guard: a padded, CRLF stamp is read and runs (exit 0)" "0" "$?"
    : > "$stamp"
    run_guard "$unit"
    assert_eq "guard: an empty stamp exits 255 (hold)" "255" "$?"
    assert_output_has "guard: an empty stamp is named as unreadable" "$(cat "$INSTALL_OUT")" "does not hold a timestamp"

    # ---- a hibernate the s2ram counter does not count: resume recorded, counter 0 ----
    printf '%s\n' "$((now - 90000))" > "$stamp"
    printf '0\n' > "$counter"
    export STUB_RESUME_US=$((STUB_MONO_NOW_US - 8 * 3600 * 1000000))
    run_guard "$unit"
    assert_eq "guard: a recorded resume with a zero counter is not a contradiction (exit 0)" "0" "$?"
    export STUB_RESUME_US=0

    # ---- a kernel without the counter (no CONFIG_PM_SLEEP, or < 5.4) is not a hold ----
    rm -f "$counter"
    printf '%s\n' "$((now - 90000))" > "$stamp"
    run_guard "$unit"
    assert_eq "guard: an absent suspend counter is not consulted (exit 0)" "0" "$?"
    assert_output_has "guard: with no counter the settle rule counts from the boot, and says so" "$(cat "$INSTALL_OUT")" "no suspend counted this boot, up 216000s"
    export STUB_MONO_NOW_US=$((30 * 1000000))
    run_guard "$unit"
    assert_eq "guard: with no counter a firing 30s after boot is still discarded (exit 1)" "1" "$?"
    export STUB_MONO_NOW_US=$((216000 * 1000000))
    printf '0\n' > "$counter"

    # ---- a server that never sleeps: three consecutive days, three runs, no hold ----
    # The acceptance criterion in one loop: with the manager reporting no
    # resume and the kernel counting none, each day's firing runs, and the
    # discard/hold paths are never taken.
    rm -f "$stamp"
    local _day status held=0 ran=0 clock="$now" mono="$STUB_MONO_NOW_US"
    for _day in 1 2 3; do
        STUB_DATE_EPOCH="$clock" STUB_MONO_NOW_US="$mono" run_guard "$unit"
        status=$?
        [ "$status" -eq 0 ] && ran=$((ran + 1))
        [ "$status" -eq 255 ] && held=$((held + 1))
        clock=$((clock + 86400))
        mono=$((mono + day_us))
    done
    assert_eq "guard: a host that never sleeps runs on each of three consecutive days" "3" "$ran"
    assert_eq "guard: a host that never sleeps never holds" "0" "$held"
    assert_eq "guard: the stamp follows the last of those attempts" "$((now + 2 * 86400))" "$(cat "$stamp")"

    # ---- the last resume is ADVISORY: a host that cannot place it runs on
    # the interval rule alone, says so once, and never holds ----
    # The case the review of #416 found: on stock distributions the system
    # manager garbage-collects sleep.target after every cycle (nothing else
    # references it), so after a laptop's first suspend `show` reads 0 while
    # the kernel counts the suspend. That host was holding on every daily
    # firing until reboot; it must run.
    printf '%s\n' "$((now - 90000))" > "$stamp"
    printf '2\n' > "$counter"
    STUB_RESUME_US=0 run_guard "$unit"
    assert_eq "guard: suspends the kernel counted that the manager no longer has: exit 0 (run)" "0" "$?"
    assert_output_has "guard: the forgotten resume is logged as the manager forgetting, not as a contradiction" \
        "$(cat "$INSTALL_OUT")" "the kernel counts 2 completed suspend(s) this boot but the system manager reads 0 for sleep.target"
    assert_output_has "guard: the forgotten resume names the rule that governs instead" "$(cat "$INSTALL_OUT")" "NOTE — the last resume is not available on this host (the kernel counts"
    assert_output_has "guard: the forgotten resume is a NOTE, not a fault" "$(cat "$INSTALL_OUT")" "the 23 h interval rule alone governs this firing"
    assert_output_has "guard: the run line says the resume was not available" "$(cat "$INSTALL_OUT")" "RUN — letting $unit run: last resume not available on this host, last attempt 90000s ago"
    assert_eq "guard: the forgotten-resume run logs exactly one NOTE about it" "1" "$(grep -c 'NOTE — ' "$INSTALL_OUT")"
    assert_eq "guard: the forgotten-resume run stamps the attempt" "$now" "$(cat "$stamp")"
    # And a second firing inside the interval on that host is still discarded
    # — the interval rule is what carries the cadence there.
    STUB_RESUME_US=0 run_guard "$unit"
    assert_eq "guard: on that host a second firing inside the interval still exits 1 (skip)" "1" "$?"
    # Three days on a laptop that slept once and whose manager forgot: runs
    # each day, never holds — the mirror of the never-sleeps loop above.
    rm -f "$stamp"
    held=0; ran=0; clock="$now"; mono="$STUB_MONO_NOW_US"
    local noted=0
    for _day in 1 2 3; do
        STUB_DATE_EPOCH="$clock" STUB_MONO_NOW_US="$mono" STUB_RESUME_US=0 run_guard "$unit"
        status=$?
        [ "$status" -eq 0 ] && ran=$((ran + 1))
        [ "$status" -eq 255 ] && held=$((held + 1))
        [ "$(grep -c 'NOTE — ' "$INSTALL_OUT")" -eq 1 ] && noted=$((noted + 1))
        clock=$((clock + 86400))
        mono=$((mono + day_us))
    done
    assert_eq "guard: a laptop whose manager forgot its resume runs on each of three consecutive days" "3" "$ran"
    assert_eq "guard: a laptop whose manager forgot its resume never holds" "0" "$held"
    assert_eq "guard: each of those days logs exactly one NOTE" "3" "$noted"
    printf '0\n' > "$counter"
    # The other ways the reading can be unavailable: the same fall-through.
    printf '%s\n' "$((now - 90000))" > "$stamp"
    STUB_RESUME_US=FAIL run_guard "$unit"
    assert_eq "guard: an unreachable system manager exits 0 (run) on the interval rule" "0" "$?"
    assert_output_has "guard: the unreachable system manager is logged with its own words" "$(cat "$INSTALL_OUT")" "could not be asked (systemctl show -p InactiveEnterTimestampMonotonic sleep.target failed: 'Failed to connect to system scope bus"
    assert_eq "guard: the unreachable-system-manager run stamps the attempt" "$now" "$(cat "$stamp")"
    printf '%s\n' "$((now - 90000))" > "$stamp"
    STUB_RESUME_US=garbage run_guard "$unit"
    assert_eq "guard: a resume time that is not a number exits 0 (run) on the interval rule" "0" "$?"
    assert_output_has "guard: the unparseable resume is logged with the manager's answer" "$(cat "$INSTALL_OUT")" "answered 'garbage'"
    printf '%s\n' "$((now - 90000))" > "$stamp"
    STUB_RESUME_US=$((STUB_MONO_NOW_US + 1000000)) run_guard "$unit"
    assert_eq "guard: a resume later than this activation exits 0 (run) on the interval rule" "0" "$?"
    assert_output_has "guard: the later-than-activation reading is logged as unusable" "$(cat "$INSTALL_OUT")" "later than this activation"
    # A resume the manager DOES still have is not affected by any of this:
    # the settle rule above already proved 30 s / 299 s / 300 s.

    # ---- HOLDS: exit 255, the unit fails, the reason names the input ----
    printf '%s\n' "$((now - 90000))" > "$stamp"
    STUB_MONO_NOW_US=FAIL run_guard "$unit"
    assert_eq "guard: an unreachable user manager exits 255 (hold)" "255" "$?"
    assert_output_has "guard: the held check names the user manager's read" "$(cat "$INSTALL_OUT")" "cannot read this activation's time from the user manager"
    assert_output_has "guard: the held check carries the manager's own error text" "$(cat "$INSTALL_OUT")" "failed: 'Failed to connect to user scope bus"
    assert_eq "guard: the user-manager hold does not move the stamp" "$((now - 90000))" "$(cat "$stamp")"
    assert_output_has "guard: a hold says how long the unit stays failed" "$(cat "$INSTALL_OUT")" "failed until its next activation or a reset-failed"
    STUB_MONO_NOW_US=0 run_guard "$unit"
    assert_eq "guard: a manager that does not show the unit activating (0) exits 255 (hold)" "255" "$?"
    assert_eq "guard: the not-activating hold does not move the stamp" "$((now - 90000))" "$(cat "$stamp")"
    STUB_MONO_NOW_US=not-a-number run_guard "$unit"
    assert_eq "guard: an activation time that is not a number exits 255 (hold)" "255" "$?"
    STUB_DATE_EXIT=1 run_guard "$unit"
    assert_eq "guard: a clock that fails exits 255 (hold), not set -e's 1" "255" "$?"
    assert_output_has "guard: a clock that fails is logged as a hold" "$(cat "$INSTALL_OUT")" "cannot read the clock"
    STUB_DATE_EPOCH="not-a-number" run_guard "$unit"
    assert_eq "guard: a clock that prints garbage exits 255 (hold)" "255" "$?"
    assert_eq "guard: the clock holds do not move the stamp" "$((now - 90000))" "$(cat "$stamp")"
    # The kernel's counter present but unreadable is still a hold: a file
    # the kernel keeps, named in the line, not a property of the host.
    printf 'garbage\n' > "$counter"
    run_guard "$unit"
    assert_eq "guard: a counter that exists but cannot be read exits 255 (hold)" "255" "$?"
    assert_output_has "guard: the unreadable counter is named" "$(cat "$INSTALL_OUT")" "cannot read $counter"
    assert_eq "guard: the unreadable-counter hold does not move the stamp" "$((now - 90000))" "$(cat "$stamp")"
    printf '0\n' > "$counter"
    # An end the guard did not decide — a `set -e` death — is a HOLD, not the
    # discard its status would otherwise read as. BASH_ENV makes a script
    # variable readonly before the guard starts, so its own assignment to it
    # is fatal half-way through, after the manager reads.
    printf 'readonly since_resume=poisoned\n' > "$TMP/guard-poison.env"
    GUARD_BASH_ENV="$TMP/guard-poison.env" run_guard "$unit"
    assert_eq "guard: a death the guard did not decide exits 255 (hold), not set -e's status" "255" "$?"
    assert_output_has "guard: the undecided death is logged as a HELD line" "$(cat "$INSTALL_OUT")" "HELD — the guard ended with status"
    assert_output_has "guard: the undecided death says why a skip would be wrong" "$(cat "$INSTALL_OUT")" "a skip nobody decided would read as a quiet day"
    assert_eq "guard: the undecided death does not move the stamp" "$((now - 90000))" "$(cat "$stamp")"
    # A hold with nowhere to write its line — journald gone, stdout closed —
    # is still a hold: `log` may not abort `hold` before `finish`, or the
    # printf builtin's 1 (the discard) would be the exit.
    local closed_rc=0
    ( export SOLADOR_AGENT_UPDATE_GUARD_ROOT="$root" PATH="$STUBS_CLOCK:$PATH" HOME="$ghome" INVOCATION_ID="53c52280aec6457493cabd9c3b09eb66"
      "$BASH" "$guard" 2>/dev/null >&- ) || closed_rc=$?
    assert_eq "guard: a hold with stdout closed still exits 255, not printf's 1" "255" "$closed_rc"
    closed_rc=0
    ( export SOLADOR_AGENT_UPDATE_GUARD_ROOT="$root" PATH="$STUBS_CLOCK:$PATH" HOME="$ghome" INVOCATION_ID="53c52280aec6457493cabd9c3b09eb66"
      export BASH_ENV="$TMP/guard-poison.env"
      "$BASH" "$guard" "$unit" 2>/dev/null >&- ) || closed_rc=$?
    assert_eq "guard: an undecided death with stdout closed still exits 255" "255" "$closed_rc"
    # The stamp.
    printf 'garbage\n' > "$stamp"
    run_guard "$unit"
    assert_eq "guard: an unreadable stamp exits 255 (hold)" "255" "$?"
    assert_output_has "guard: the held check names the stamp file to remove" "$(cat "$INSTALL_OUT")" "Remove that file"
    assert_eq "guard: a held check does not rewrite the stamp" "garbage" "$(cat "$stamp")"
    if [ "$(id -u)" = "0" ]; then
        skip "guard: an unwritable stamp directory exits 255 (hold)" "running as root, which can write anywhere"
    else
        local ro_home="$TMP/guard-home-ro"
        rm -rf "$ro_home"
        mkdir -p "$ro_home/.config"
        chmod 555 "$ro_home/.config"
        GUARD_HOME="$ro_home" run_guard "$unit"
        assert_eq "guard: an unwritable stamp directory exits 255 (hold)" "255" "$?"
        assert_output_has "guard: the unwritable stamp is named" "$(cat "$INSTALL_OUT")" "could not write $ro_home/.config/solador-agent-update.last-attempt ("
        assert_output_has "guard: the unwritable stamp hold carries the OS's own reason" "$(cat "$INSTALL_OUT")" "Permission denied)"
        assert_output_has "guard: the unwritable stamp hold says what to check" "$(cat "$INSTALL_OUT")" "Check that $ro_home/.config is writable by"
        [ -e "$ro_home/.config/solador-agent-update.last-attempt.new" ] && fail "guard: no stamp .new is left after a failed write" "it is" \
            || pass "guard: no stamp .new is left after a failed write"
        chmod 755 "$ro_home/.config"
    fi

    # ---- the shape of the invocation: every usage error is a HOLD, because
    # a unit that reaches the guard with the wrong shape is a unit somebody
    # edited, and a skip (1–254) would read as a quiet day ----
    printf '%s\n' "$((now - 90000))" > "$stamp"
    GUARD_NO_INVOCATION=1 run_guard "$unit"
    assert_eq "guard: no INVOCATION_ID (not under a unit) exits 255 (hold)" "255" "$?"
    assert_output_has "guard: the no-unit hold names the variable and the manual command" "$(cat "$INSTALL_OUT")" "INVOCATION_ID"
    assert_output_has "guard: the no-unit hold points at solador-agent update" "$(cat "$INSTALL_OUT")" "solador-agent update"
    run_guard
    assert_eq "guard: no argument exits 255 (hold)" "255" "$?"
    assert_output_has "guard: the no-argument hold is a usage line" "$(cat "$INSTALL_OUT")" "usage:"
    run_guard "$unit" extra
    assert_eq "guard: a second argument exits 255 (hold)" "255" "$?"
    run_guard "solador-agent-update.timer"
    assert_eq "guard: an argument that is not a .service exits 255 (hold)" "255" "$?"
    run_guard "evil; rm -rf.service"
    assert_eq "guard: a unit name with shell metacharacters exits 255 (hold)" "255" "$?"
    assert_output_has "guard: the bad unit name is refused before any manager read" "$(cat "$INSTALL_OUT")" "carries a character"
    [ -s "$STUB_GUARD_SYSTEMCTL_ARGV" ] && fail "guard: a refused unit name reaches no systemctl" "$(cat "$STUB_GUARD_SYSTEMCTL_ARGV")" \
        || pass "guard: a refused unit name reaches no systemctl"
    GUARD_HOME='unset' run_guard "$unit"
    assert_eq "guard: no HOME exits 255 (hold)" "255" "$?"
    GUARD_HOME="$TMP/no-such-home" run_guard "$unit"
    assert_eq "guard: a HOME that is not a directory exits 255 (hold)" "255" "$?"
    assert_eq "guard: none of the refusals moved the stamp" "$((now - 90000))" "$(cat "$stamp")"

    # ---- the guard's exit codes stay out of the oneshot's SuccessExitStatus= ----
    # ExecCondition= honours that line too: a condition exit matching it
    # RUNS ExecStart (observed on systemd 256 — a condition exit of 4 ran
    # the updater). So the two codes the guard exits with, read out of the
    # script, must not be among the values the unit template names; a
    # future `SuccessExitStatus=1 4` would turn every discard into a run.
    local discard_exit hold_exit success_statuses code
    discard_exit="$(sed -n 's/^DISCARD_EXIT=\([0-9][0-9]*\).*/\1/p' "$guard")"
    hold_exit="$(sed -n 's/^HOLD_EXIT=\([0-9][0-9]*\).*/\1/p' "$guard")"
    success_statuses="$(sed -n 's/^SuccessExitStatus=//p' "$SCRIPT_DIR/solador-agent-update.service")"
    assert_eq "guard: DISCARD_EXIT is 1 (inside ExecCondition's skip range)" "1" "$discard_exit"
    assert_eq "guard: HOLD_EXIT is 255 (the one code that fails the unit)" "255" "$hold_exit"
    # The trap that turns an undecided end into a hold is armed, and the only
    # bare `exit`s in the script are the two inside finish and the trap
    # itself — every other exit goes through finish, or the trap would fire
    # on a decided one.
    assert_eq "guard: the EXIT trap is armed" "1" "$(grep -c '^trap on_exit EXIT$' "$guard")"
    assert_eq "guard: only finish and the trap exit directly" "2" "$(grep -c '^[[:space:]]*exit ' "$guard")"
    assert_eq "the oneshot names SuccessExitStatus= exactly once" "1" "$(grep -c '^SuccessExitStatus=' "$SCRIPT_DIR/solador-agent-update.service")"
    for code in $success_statuses; do
        if [ "$code" = "$discard_exit" ] || [ "$code" = "$hold_exit" ]; then
            fail "the oneshot's SuccessExitStatus= ($success_statuses) shares no value with the guard's exits" \
                "$code would make ExecCondition run the updater"
            success_statuses=""
            break
        fi
    done
    [ -n "$success_statuses" ] && pass "the oneshot's SuccessExitStatus= ($success_statuses) shares no value with the guard's exits"

    unset STUB_DATE_EPOCH STUB_MONO_NOW_US STUB_RESUME_US STUB_GUARD_SYSTEMCTL_ARGV
    unset STUB_DATE_EXIT GUARD_HOME GUARD_NO_INVOCATION GUARD_BASH_ENV
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

# ---- install.sh --uninstall (#439) -------------------------------------------
#
# The mirror of the install flow above, on the same stubbed managers: install
# with --enable-timer (so both the metrics job and the updater exist), then
# --uninstall and assert every installer file is gone and both jobs were
# disabled/stopped; the env file survives without --purge and is gone with
# it; a second --uninstall is a no-op that exits 0 and asks the manager for
# nothing; root is refused; the transaction lock refuses an uninstall while
# it is held.

test_uninstall_linux() {
    local home="$TMP/home-uninstall-linux" env_file unit update_unit update_timer bin guard out
    local tls_key tls_cert
    if [ "$HAVE_MINISIGN" != true ]; then
        skip_needs_minisign "install.sh --uninstall (Linux)"
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
    tls_key="$home/.config/solador-agent.tls.key"
    tls_cert="$home/.config/solador-agent.tls.crt"
    bin="$home/.local/bin/solador-agent"
    guard="$home/.local/bin/solador-agent-update-guard"

    reset_argv_logs
    INSTALL_STDIN="uninstall-tok-MUST-NOT-BE-PRINTED
" run_install "$home" --enable-timer
    assert_eq "install.sh --enable-timer succeeds, setting up the uninstall fixture (Linux)" "0" "$INSTALL_STATUS"
    # A migration remnant and the update-only siblings a real host can carry,
    # seeded by hand so removing them is actually exercised — without this,
    # ".prev is gone" and ".rollback-displaced is gone" below would be true
    # by vacuous absence (a fresh install never creates either) rather than
    # by anything this run actually removed.
    cp "$unit" "$unit.prev"
    cp "$bin" "$bin.prev"
    : > "$bin.new"
    : > "$bin.rollback-displaced"
    mkdir -p "$(dirname "$home/.config/solador-agent-update.last-attempt")"
    printf '1234567890\n' > "$home/.config/solador-agent-update.last-attempt"
    # A TLS keypair (#447), seeded by hand for the same reason: this install
    # ran with TLS off (the harness default), so nothing here would create
    # one on its own, and "survives without --purge, gone with it" needs a
    # real file to make either half of that claim about.
    printf 'FAKE-KEY-BYTES' > "$tls_key"
    printf 'FAKE-CERT-BYTES' > "$tls_cert"

    # ---- refuses while the transaction lock is held ----
    hold_fake_lock "$bin.update.lock"
    reset_argv_logs
    run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh --uninstall refuses while the update lock is held" "1" "$INSTALL_STATUS"
    assert_output_has "the lock refusal names the lock file" "$out" "$bin.update.lock"
    [ -e "$unit" ] && pass "a lock-refused uninstall leaves the unit in place" \
        || fail "a lock-refused uninstall leaves the unit in place" "$unit is gone"
    [ -x "$bin" ] && pass "a lock-refused uninstall leaves the binary in place" \
        || fail "a lock-refused uninstall leaves the binary in place" "$bin is gone"
    if systemctl_mutated; then
        fail "a lock-refused uninstall never reaches the service manager" "systemctl was called"
    else
        pass "a lock-refused uninstall never reaches the service manager"
    fi
    # Blocking fix (this round): a busy refusal must NEVER delete the lock
    # file — deleting it while a concurrent holder's flock() is still on it
    # is exactly how two processes come to hold "the" lock at once
    # (agent/src/update.rs:1349-1351). hold_fake_lock's own genuinely
    # competing holder is what makes this file "concurrently created/held"
    # from install.sh's own point of view, not merely pre-existing on disk;
    # checked BEFORE release_fake_lock's own cleanup removes it.
    [ -e "$bin.update.lock" ] && pass "a busy refusal leaves the concurrently held lock file in place" \
        || fail "a busy refusal leaves the concurrently held lock file in place" "$bin.update.lock is gone"
    release_fake_lock "$bin.update.lock"
    # A leftover lock file from a transaction that already finished — the
    # ordinary case (agent/src/update.rs's TransactionLock never removes its
    # file). Named with a pid that is certainly not running, NOT $$: this
    # process's own pid is alive by definition and would read as held under
    # the fallback (no-flock) implementation, defeating the point of this
    # step.
    printf 'pid=999999999 since=1\n' > "$bin.update.lock"

    # ---- the real uninstall ----
    reset_argv_logs
    run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh --uninstall succeeds (Linux)" "0" "$INSTALL_STATUS"
    # Exact-line (grep -x), not a substring match: stop and disable are now
    # separate calls, each against the unit's FULL name — never a bare one
    # (#454 round-4 review's own follow-up).
    if grep -qxF -- "--user stop solador-agent.service" "$STUB_SYSTEMCTL_ARGV"; then
        pass "the metrics service is stopped"
    else
        fail "the metrics service is stopped" "no exact '--user stop solador-agent.service' line"
    fi
    if grep -qxF -- "--user disable solador-agent.service" "$STUB_SYSTEMCTL_ARGV"; then
        pass "the metrics service is disabled"
    else
        fail "the metrics service is disabled" "no exact '--user disable solador-agent.service' line"
    fi
    assert_file_has "the update timer is stopped" "$STUB_SYSTEMCTL_ARGV" "--user stop solador-agent-update.timer"
    assert_file_has "the update timer is disabled" "$STUB_SYSTEMCTL_ARGV" "--user disable solador-agent-update.timer"
    assert_file_has "systemd is reloaded" "$STUB_SYSTEMCTL_ARGV" "--user daemon-reload"
    # #439 nit: reset-failed clears any "failed" state a disable/stop that
    # reported an error can leave behind, for every unit --uninstall knows
    # about — even ones this particular run never touched.
    assert_file_has "reset-failed is run for every unit --uninstall knows about" "$STUB_SYSTEMCTL_ARGV" \
        "--user reset-failed solador-agent.service solador-agent-update.service solador-agent-update.timer devcanopy-agent.service"
    if [ -e "$unit" ] || [ -e "$unit.prev" ] || [ -e "$update_unit" ] || [ -e "$update_timer" ] \
        || [ -e "$guard" ] || [ -e "$bin" ] || [ -e "$bin.prev" ] || [ -e "$bin.new" ] \
        || [ -e "$bin.rollback-displaced" ] \
        || [ -e "$bin.update.lock" ] || [ -e "$home/.config/solador-agent-update.last-attempt" ]; then
        fail "install.sh --uninstall removes every installer file (Linux)" "some installer file is still present"
    else
        pass "install.sh --uninstall removes every installer file (Linux)"
    fi
    case "$out" in
        *"uninstall-tok-MUST-NOT-BE-PRINTED"*) fail "install.sh --uninstall never prints the token" "it appeared in the output" ;;
        *) pass "install.sh --uninstall never prints the token" ;;
    esac
    [ -e "$env_file" ] && pass "the env file survives without --purge" \
        || fail "the env file survives without --purge" "$env_file is gone"
    assert_output_has "the kept env file is named in the output" "$out" "$env_file"
    assert_output_has "the kept env file's removal is spelled out" "$out" "--uninstall --purge"
    # The TLS key/certificate (#447) follow the same rule as the env file:
    # kept without --purge, named in the output.
    if [ -e "$tls_key" ] && [ -e "$tls_cert" ]; then
        pass "the TLS key and certificate survive without --purge"
    else
        fail "the TLS key and certificate survive without --purge" "$tls_key or $tls_cert is gone"
    fi
    assert_output_has "the kept TLS key/certificate are named in the output" "$out" "$tls_key, $tls_cert"
    assert_output_has "install.sh --uninstall reports success" "$out" "uninstalled for"
    if grep -q "disable-linger" "$STUB_SYSTEMCTL_ARGV"; then
        fail "install.sh --uninstall never disables linger" "loginctl disable-linger was called"
    else
        pass "install.sh --uninstall never disables linger"
    fi

    # ---- a second uninstall is a no-op ----
    reset_argv_logs
    run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    assert_eq "a second install.sh --uninstall is a no-op that exits 0 (Linux)" "0" "$INSTALL_STATUS"
    assert_output_has "the no-op run says nothing was installed" "$out" "Nothing installed"
    if systemctl_mutated; then
        fail "a no-op uninstall asks the service manager for nothing" "systemctl was called"
    else
        pass "a no-op uninstall asks the service manager for nothing"
    fi
    [ -e "$env_file" ] && pass "a no-op uninstall still leaves the env file" \
        || fail "a no-op uninstall still leaves the env file" "$env_file is gone"
    if [ -e "$tls_key" ] && [ -e "$tls_cert" ]; then
        pass "a no-op uninstall still leaves the TLS key and certificate"
    else
        fail "a no-op uninstall still leaves the TLS key and certificate" "$tls_key or $tls_cert is gone"
    fi

    # ---- --purge also removes the env file ----
    reset_argv_logs
    run_install "$home" --uninstall --purge
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh --uninstall --purge succeeds" "0" "$INSTALL_STATUS"
    [ -e "$env_file" ] && fail "--purge removes the env file" "$env_file still exists" \
        || pass "--purge removes the env file"
    assert_output_has "the purge names the env file removed" "$out" "$env_file"
    # --purge removes the TLS key/certificate too (#447): re-pairing means a
    # fresh certificate on the agent's next start.
    if [ -e "$tls_key" ] || [ -e "$tls_cert" ]; then
        fail "--purge removes the TLS key and certificate" "$tls_key or $tls_cert still exists"
    else
        pass "--purge removes the TLS key and certificate"
    fi
    assert_output_has "the purge names the TLS key removed" "$out" "$tls_key"
    assert_output_has "the purge names the TLS certificate removed" "$out" "$tls_cert"

    # ---- idempotent again, even with --purge ----
    reset_argv_logs
    run_install "$home" --uninstall --purge
    assert_eq "a repeated --uninstall --purge is still a no-op that exits 0" "0" "$?"

    # ---- a fresh install, for the two manager-related cases below ----
    reset_argv_logs
    INSTALL_STDIN="uninstall-manager-tok-MUST-NOT-BE-PRINTED
" run_install "$home" --enable-timer
    assert_eq "install.sh --enable-timer succeeds again, for the manager-failure cases (Linux)" "0" "$INSTALL_STATUS"

    # ---- an unreachable systemd user manager refuses --uninstall, untouched ----
    reset_argv_logs
    STUB_SYSTEMCTL_USER_EXIT=1 run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh --uninstall refuses when systemctl --user is unreachable" "1" "$INSTALL_STATUS"
    assert_output_has "the unreachable-manager refusal says so" "$out" "cannot reach this user's systemd manager"
    [ -e "$unit" ] && pass "an unreachable-manager refusal leaves the unit in place" \
        || fail "an unreachable-manager refusal leaves the unit in place" "$unit is gone"
    [ -x "$bin" ] && pass "an unreachable-manager refusal leaves the binary in place" \
        || fail "an unreachable-manager refusal leaves the binary in place" "$bin is gone"
    if grep -qE "stop|disable|daemon-reload" "$STUB_SYSTEMCTL_ARGV"; then
        fail "an unreachable-manager refusal never asks systemd to change anything" "a mutating call was made"
    else
        pass "an unreachable-manager refusal never asks systemd to change anything"
    fi

    # ---- a reachable manager that still refuses to DISABLE the service: exit 4, files still removed ----
    # stop and disable are two separate calls now (#454 round-4 review's own
    # follow-up); STUB_SYSTEMCTL_STOP_EXIT is unset here, so `stop` succeeds
    # and only the `disable` call (gated on the unit file, which this fresh
    # fixture still has) is made to fail.
    reset_argv_logs
    STUB_SYSTEMCTL_DISABLE_EXIT=1 run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh --uninstall exits 4 when a reachable manager refuses to disable the service" "4" "$INSTALL_STATUS"
    assert_output_has "the exit-4 (disable) case says the files were removed anyway" "$out" "files were removed"
    assert_output_has "the exit-4 (disable) case says to verify by hand" "$out" "Verify by hand"
    assert_output_has "the exit-4 (disable) case names the actual unconfirmed claim (auto-start)" "$out" "auto-start"
    # #454 round-5 nit: a failed disable, with a successful stop, must NOT
    # claim the process may still be running — that claim belongs only to a
    # failed stop, below.
    case "$out" in
        *"still running"*)
            fail "the exit-4 (disable) case never claims the process may still be running" \
                "\"still running\" appeared in the output" ;;
        *) pass "the exit-4 (disable) case never claims the process may still be running" ;;
    esac
    if [ -e "$unit" ] || [ -x "$bin" ]; then
        fail "the exit-4 (disable) case still removes every installer file" "some installer file is still present"
    else
        pass "the exit-4 (disable) case still removes every installer file"
    fi
    case "$out" in
        *"uninstall-manager-tok-MUST-NOT-BE-PRINTED"*)
            fail "the exit-4 (disable) case never prints the token" "it appeared in the output" ;;
        *) pass "the exit-4 (disable) case never prints the token" ;;
    esac
    # #454 round-6 nit: by this point the unit's FILE is already removed and
    # `daemon-reload` has already run, so lib.sh's own service_inspect_hint
    # (`systemctl --user cat solador-agent | grep ExecStart`) would answer
    # "No files found" whether or not the process is still running. Linux
    # gets its own hint instead: `systemctl --user status <unit>` for every
    # unit whose disable call actually failed here — a fresh --enable-timer
    # fixture, so all three (metrics, timer, oneshot) failed to disable —
    # plus the `.wants` listing that a dangling enablement symlink leaves
    # behind.
    assert_output_has "the exit-4 (disable) case names the failed unit's status command" \
        "$out" "systemctl --user status solador-agent.service"
    assert_output_has "the exit-4 (disable) case names the dangling-enablement-link listing" \
        "$out" "ls -l ~/.config/systemd/user/*.wants/"

    # ---- a reachable manager that still refuses to STOP the service: exit 4, files still removed ----
    # A fresh fixture (install with --enable-timer) so the unit file exists
    # and `stop` is actually attempted; STUB_SYSTEMCTL_STOP_EXIT fails it
    # independent of STUB_SYSTEMCTL_DISABLE_EXIT — proving the two calls are
    # reported independently was the point of splitting them.
    reset_argv_logs
    INSTALL_STDIN="uninstall-stopfail-tok-MUST-NOT-BE-PRINTED
" run_install "$home" --enable-timer
    assert_eq "install.sh --enable-timer succeeds again, for the stop-failure case (Linux)" "0" "$INSTALL_STATUS"
    reset_argv_logs
    STUB_SYSTEMCTL_STOP_EXIT=1 run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh --uninstall exits 4 when a reachable manager refuses to stop the service" "4" "$INSTALL_STATUS"
    assert_output_has "the exit-4 (stop) case says the files were removed anyway" "$out" "files were removed"
    assert_output_has "the exit-4 (stop) case says to verify by hand" "$out" "Verify by hand"
    # #454 round-5 nit: a failed stop DOES claim the process may still be
    # running — the mirror of the disable-only assertion above.
    assert_output_has "the exit-4 (stop) case claims the process may still be running" "$out" "still running"
    if [ -e "$unit" ] || [ -x "$bin" ]; then
        fail "the exit-4 (stop) case still removes every installer file" "some installer file is still present"
    else
        pass "the exit-4 (stop) case still removes every installer file"
    fi
    case "$out" in
        *"uninstall-stopfail-tok-MUST-NOT-BE-PRINTED"*)
            fail "the exit-4 (stop) case never prints the token" "it appeared in the output" ;;
        *) pass "the exit-4 (stop) case never prints the token" ;;
    esac
    # #454 round-6 nit (continued): this fresh --enable-timer fixture fails
    # `stop` on all three units it carries, which is also the "two failed
    # units" case — both the metrics unit's and the update timer's own
    # `systemctl --user status` lines must appear, not just the first one
    # found.
    assert_output_has "the exit-4 (stop) case names the failed unit's status command" \
        "$out" "systemctl --user status solador-agent.service"
    assert_output_has "the exit-4 (stop) case names a second failed unit's status command too" \
        "$out" "systemctl --user status solador-agent-update.timer"
    # No disable call failed here (only stop was made to fail), so the
    # dangling-enablement-link listing must NOT appear — it is specific to a
    # failed disable, not printed unconditionally.
    case "$out" in
        *".wants"*)
            fail "the exit-4 (stop) case never names the wants listing (no disable failure)" \
                "\".wants\" appeared in the output" ;;
        *) pass "the exit-4 (stop) case never names the wants listing (no disable failure)" ;;
    esac

    # ---- an unmigrated /opt host: --uninstall says what it leaves behind ----
    # (#439 nit b) — a unit whose ExecStart= names a binary this user does not
    # own (the pre-#392 layout --migrate-from-opt exists to repoint) must not
    # be silently forgotten once the unit that named it is gone.
    reset_argv_logs
    INSTALL_STDIN="foreign-tok-MUST-NOT-BE-PRINTED
" run_install "$home"
    assert_eq "install.sh succeeds, setting up the foreign-binary fixture (Linux)" "0" "$INSTALL_STATUS"
    awk '/^ExecStart=/ { print "ExecStart=/opt/solador-agent/solador-agent"; next } { print }' \
        "$unit" > "$unit.tmp" && mv "$unit.tmp" "$unit"
    reset_argv_logs
    run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh --uninstall still succeeds on an unmigrated host (Linux)" "0" "$INSTALL_STATUS"
    assert_output_has "the foreign binary is named as left behind" "$out" \
        "left behind: /opt/solador-agent/solador-agent"
    assert_output_has "the /opt case names the actual remedy" "$out" "sudo rm -rf /opt/solador-agent"
    case "$out" in
        *"not owned by this user; see --migrate-from-opt"*)
            fail "the left-behind hint no longer points at --migrate-from-opt" \
                "the old, unchecked wording is still printed" ;;
        *) pass "the left-behind hint no longer points at --migrate-from-opt" ;;
    esac
    [ -e "$unit" ] && fail "the unit naming a foreign binary is still removed" "$unit is still present" \
        || pass "the unit naming a foreign binary is still removed"

    # ---- the pre-rename unit is also stopped, disabled and removed, and ----
    # ---- named in the same daemon-reload/reset-failed sweep (#439 nit)  ----
    reset_argv_logs
    INSTALL_STDIN="legacy-unit-tok-MUST-NOT-BE-PRINTED
" run_install "$home"
    assert_eq "install.sh succeeds, setting up the legacy-unit fixture (Linux)" "0" "$INSTALL_STATUS"
    local legacy_unit="$home/.config/systemd/user/devcanopy-agent.service"
    : > "$legacy_unit"
    reset_argv_logs
    run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh --uninstall succeeds with a pre-rename unit present (Linux)" "0" "$INSTALL_STATUS"
    assert_file_has "the pre-rename unit is stopped" "$STUB_SYSTEMCTL_ARGV" \
        "--user stop devcanopy-agent.service"
    assert_file_has "the pre-rename unit is disabled" "$STUB_SYSTEMCTL_ARGV" \
        "--user disable devcanopy-agent.service"
    [ -e "$legacy_unit" ] && fail "the pre-rename unit file is removed too" "$legacy_unit still exists" \
        || pass "the pre-rename unit file is removed too"

    # ---- an unremovable binary: exit 6, no Done, no "removed:" line for it ----
    # (#439 fix 1) — chmod 555 on the binary's own parent directory (not the
    # file itself: a file's own permission bits do not gate unlink(2), its
    # containing directory's do), the same shape the reviewer reproduced with
    # `chmod 555 ~/.local/bin`. Restored before this function returns either
    # way, so a failure here cannot break cleanup or any later test.
    reset_argv_logs
    INSTALL_STDIN="unremovable-bin-tok-MUST-NOT-BE-PRINTED
" run_install "$home"
    assert_eq "install.sh succeeds, setting up the unremovable-binary fixture (Linux)" "0" "$INSTALL_STATUS"
    # Seeded BEFORE the chmod below: --uninstall's own lock file lives in the
    # same directory as the binary, and opening an EXISTING file for writing
    # needs no directory write permission (only creating or unlinking one
    # does) — so a pre-existing lock file lets the lock acquisition itself
    # succeed under a read-only ~/.local/bin, and this case stays specifically
    # about the binary's own removal failing, not about the lock never being
    # taken at all.
    : > "$bin.update.lock"
    chmod 555 "$(dirname "$bin")"
    reset_argv_logs
    run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    chmod 755 "$(dirname "$bin")"
    assert_eq "install.sh --uninstall exits 6 when the binary cannot be removed (Linux)" "6" "$INSTALL_STATUS"
    case "$out" in
        *"==> Done"*) fail "the exit-6 binary case never prints Done" "it did" ;;
        *) pass "the exit-6 binary case never prints Done" ;;
    esac
    if grep -q "removed: binary (" "$INSTALL_OUT"; then
        fail "the exit-6 binary case prints no removed: line for the binary" "it did"
    else
        pass "the exit-6 binary case prints no removed: line for the binary"
    fi
    assert_output_has "the exit-6 binary case names FAILED to remove and the binary" "$out" \
        "FAILED to remove: binary ($bin)"
    [ -e "$unit" ] && fail "the exit-6 binary case still removes the unit" "$unit is still present" \
        || pass "the exit-6 binary case still removes the unit"
    rm -f "$bin" "$bin.prev" "$bin.new" "$bin.rollback-displaced" "$bin.update.lock"
    reset_argv_logs
    run_install "$home" --uninstall >/dev/null 2>&1
    assert_eq "cleanup: a follow-up uninstall finishes once the directory is writable again (Linux)" "0" "$INSTALL_STATUS"

    # ---- an unremovable env file under --purge: the same exit-6 contract ----
    # (#439 fix 1 / nit a). An ordinary --uninstall (no --purge) runs first so
    # the update stamp — which lives beside the env file in ~/.config — is
    # already gone before ~/.config itself is made read-only; otherwise its
    # removal would ALSO fail here, and this case would no longer be
    # specifically about the env file.
    reset_argv_logs
    INSTALL_STDIN="unremovable-env-tok-MUST-NOT-BE-PRINTED
" run_install "$home"
    assert_eq "install.sh succeeds, setting up the unremovable-env-file fixture (Linux)" "0" "$INSTALL_STATUS"
    reset_argv_logs
    run_install "$home" --uninstall
    assert_eq "the ordinary uninstall ahead of the unremovable-env-file case succeeds (Linux)" "0" "$INSTALL_STATUS"
    [ -e "$home/.config/solador-agent-update.last-attempt" ] && \
        fail "nothing but the env file is left in ~/.config before the chmod" "the update stamp is still there"
    chmod 555 "$(dirname "$env_file")"
    reset_argv_logs
    run_install "$home" --uninstall --purge
    out="$(cat "$INSTALL_OUT")"
    chmod 755 "$(dirname "$env_file")"
    assert_eq "install.sh --uninstall --purge exits 6 when the env file cannot be removed (Linux)" "6" "$INSTALL_STATUS"
    case "$out" in
        *"==> Done"*) fail "the exit-6 env-file case never prints Done" "it did" ;;
        *) pass "the exit-6 env-file case never prints Done" ;;
    esac
    if grep -q "removed: env file (" "$INSTALL_OUT"; then
        fail "the exit-6 env-file case prints no removed: line for it" "it did"
    else
        pass "the exit-6 env-file case prints no removed: line for it"
    fi
    assert_output_has "the exit-6 env-file case names FAILED to remove and the env file" "$out" \
        "FAILED to remove: env file (--purge: it held the bearer token) ($env_file)"
    rm -f "$env_file"

    # ---- the legacy pre-rename env file is purged too (#439 nit a) ----
    # install copies its token OUT of devcanopy-agent.env and never deletes
    # it; --purge must not leave that copy of the same secret behind.
    reset_argv_logs
    INSTALL_STDIN="legacy-purge-tok-MUST-NOT-BE-PRINTED
" run_install "$home"
    assert_eq "install.sh succeeds, setting up the legacy-env-file fixture (Linux)" "0" "$INSTALL_STATUS"
    local legacy_env="$home/.config/devcanopy-agent.env"
    printf 'DEVCANOPY_AGENT_TOKEN=legacy-tok-MUST-NOT-BE-PRINTED\n' > "$legacy_env"
    reset_argv_logs
    run_install "$home" --uninstall --purge
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh --uninstall --purge succeeds with a legacy env file present (Linux)" "0" "$INSTALL_STATUS"
    [ -e "$legacy_env" ] && fail "--purge removes the legacy env file" "$legacy_env still exists" \
        || pass "--purge removes the legacy env file"
    assert_output_has "--purge names the legacy env file removed" "$out" "$legacy_env"
    case "$out" in
        *"legacy-tok-MUST-NOT-BE-PRINTED"*) fail "--purge never prints the legacy token" "it appeared in the output" ;;
        *) pass "--purge never prints the legacy token" ;;
    esac

    # ---- neither flock(1) nor perl: refuses (busy) BEFORE anything changes ----
    # ---- (#439 follow-up review) — there is no exit 5 any more: the        ----
    # ---- previous revision re-checked the lock a second time, right before ----
    # ---- removing the binary, for exactly this tier (no flock(1) on PATH), ----
    # ---- and a transaction starting in that window used to exit 5. Now     ----
    # ---- this tier cannot check the lock AT ALL, so it fails toward busy   ----
    # ---- up front — before the service-manager calls even run — rather     ----
    # ---- than reaching that window in the first place.                     ----
    reset_argv_logs
    INSTALL_STDIN="neithertool-tok-MUST-NOT-BE-PRINTED
" run_install "$home" --enable-timer
    assert_eq "install.sh --enable-timer succeeds, setting up the neither-tool fixture (Linux)" "0" "$INSTALL_STATUS"
    reset_argv_logs
    INSTALL_PATH="$STUBS:$TOOLBIN_NOFLOCK_NOPERL" run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh --uninstall refuses (busy) when neither flock(1) nor perl exists" "1" "$INSTALL_STATUS"
    assert_output_has "the cannot-check refusal names the lock file" "$out" "$bin.update.lock"
    assert_output_has "the cannot-check refusal says it cannot check, not that a transaction is running" "$out" \
        "cannot check the update lock"
    [ -e "$unit" ] && pass "a cannot-check refusal leaves the unit in place" \
        || fail "a cannot-check refusal leaves the unit in place" "$unit is gone"
    [ -x "$bin" ] && pass "a cannot-check refusal leaves the binary in place" \
        || fail "a cannot-check refusal leaves the binary in place" "$bin is gone"
    if systemctl_mutated; then
        fail "a cannot-check refusal never reaches the service manager" "systemctl was called"
    else
        pass "a cannot-check refusal never reaches the service manager"
    fi
    INSTALL_PATH=""
    reset_argv_logs
    run_install "$home" --uninstall
    assert_eq "cleanup: a follow-up uninstall finishes (Linux)" "0" "$INSTALL_STATUS"

    # ---- the continuous hold, proven with a REAL flock(), starting from NO ----
    # ---- pre-existing lock file (#439 follow-up review) — exec 9>>"$lock"  ----
    # ---- must CREATE the file, matching agent/src/update.rs's own          ----
    # ---- create(true).truncate(false), and hold it from before the FIRST   ----
    # ---- service-manager call. Proven in both tiers: TOOLBIN_FAKEFLOCK is  ----
    # ---- a synthetic flock(1) (perl-backed; see its own setup comment)     ----
    # ---- standing in for the real one this dev/CI host may not have, and   ----
    # ---- TOOLBIN_NOFLOCK hides it so the SAME code takes the perl branch.  ----
    if [ "$HAVE_FAKEFLOCK" != true ]; then
        skip "install.sh's continuous flock hold blocks a real competing flock() (flock(1) tier)" "no perl on PATH to back the synthetic flock(1)"
        skip "install.sh's continuous flock hold blocks a real competing flock() (perl tier)" "no perl on PATH to back the synthetic flock(1)"
    else
        reset_argv_logs
        INSTALL_STDIN="fakeflock-tok-MUST-NOT-BE-PRINTED
" run_install "$home" --enable-timer
        assert_eq "install.sh --enable-timer succeeds, setting up the fakeflock fixture (Linux)" "0" "$INSTALL_STATUS"
        rm -f "$bin.update.lock"
        [ -e "$bin.update.lock" ] && fail "the fresh-lock fixture starts with no pre-existing lock file" "$bin.update.lock exists" \
            || pass "the fresh-lock fixture starts with no pre-existing lock file"
        local probe="$TMP/fakeflock-probe-result"
        rm -f "$probe"
        reset_argv_logs
        STUB_SYSTEMCTL_PROBE_LOCK="$bin.update.lock" STUB_SYSTEMCTL_PROBE_RESULT="$probe" \
            INSTALL_PATH="$STUBS:$TOOLBIN_FAKEFLOCK" run_install "$home" --uninstall
        assert_eq "install.sh --uninstall succeeds while holding a real flock() throughout, no pre-existing lock file (flock(1) tier, Linux)" \
            "0" "$INSTALL_STATUS"
        assert_eq "a genuinely competing flock() attempt during the stop call sees the hold as busy (flock(1) tier, Linux)" \
            "busy" "$(cat "$probe" 2>/dev/null)"
        INSTALL_PATH=""

        # ---- the same case again, via the perl tier: flock(1) hidden ----
        reset_argv_logs
        INSTALL_STDIN="fakeflock-perl-tok-MUST-NOT-BE-PRINTED
" run_install "$home" --enable-timer
        assert_eq "install.sh --enable-timer succeeds, setting up the perl-tier fixture (Linux)" "0" "$INSTALL_STATUS"
        rm -f "$bin.update.lock" "$probe"
        reset_argv_logs
        STUB_SYSTEMCTL_PROBE_LOCK="$bin.update.lock" STUB_SYSTEMCTL_PROBE_RESULT="$probe" \
            INSTALL_PATH="$STUBS:$TOOLBIN_NOFLOCK" run_install "$home" --uninstall
        assert_eq "install.sh --uninstall succeeds while holding a real flock() throughout, no pre-existing lock file (perl tier, Linux)" \
            "0" "$INSTALL_STATUS"
        assert_eq "a genuinely competing flock() attempt during the stop call sees the hold as busy (perl tier, Linux)" \
            "busy" "$(cat "$probe" 2>/dev/null)"
        INSTALL_PATH=""
    fi

    unset SOLADOR_AGENT_RELEASE STUB_CURL_BODY STUB_TAILSCALE_IP
}

# #439 review fix: unowned_service_binary's last line used to be an `&&`
# chain — `[ -n "$path" ] && [ "$path" != "$DEST_BIN" ] && printf ...` — so
# it returned 1 in the NORMAL case, where the existing unit/plist already
# names $DEST_BIN. That only survived in run_uninstall because it is called
# as `foreign_bin="$(unowned_service_binary)"` under `run_uninstall || …`,
# which disables errexit for that one statement. Sourcing the function body
# directly and calling it under `set -e`, outside that protection, is what
# proves the fix rather than the caller's happenstance.
test_unowned_service_binary_survives_set_e() {
    local body="$TMP/unowned-service-binary.sh" unit_dst dest_bin result status
    extract_function "$SCRIPT_DIR/install.sh" "unowned_service_binary" > "$body"
    unit_dst="$TMP/unowned-normal-case.service"
    dest_bin="$TMP/unowned-normal-case-bin"
    printf 'ExecStart=%s\n' "$dest_bin" > "$unit_dst"
    result="$(
        set -e
        export OS=Linux
        export UNIT_DST="$unit_dst"
        export PLIST_DST="$TMP/unowned-normal-case-does-not-exist.plist"
        export DEST_BIN="$dest_bin"
        # shellcheck source=/dev/null
        source "$body"
        unowned_service_binary
        printf 'SURVIVED\n'
    )"
    status=$?
    assert_eq "unowned_service_binary survives a bare call under set -e (unit names \$DEST_BIN)" "0" "$status"
    assert_eq "unowned_service_binary prints nothing when the unit already names \$DEST_BIN" "SURVIVED" "$result"
}

test_uninstall_macos() {
    local home="$TMP/home-uninstall-macos" env_file plist update_plist launcher bin out
    local tls_key tls_cert
    if [ "$HAVE_MINISIGN" != true ]; then
        skip_needs_minisign "install.sh --uninstall (macOS)"
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
    tls_key="$home/.config/solador-agent.tls.key"
    tls_cert="$home/.config/solador-agent.tls.crt"

    reset_argv_logs
    INSTALL_STDIN="mac-uninstall-tok-MUST-NOT-BE-PRINTED
" run_install "$home" --enable-timer
    assert_eq "install.sh --enable-timer succeeds, setting up the uninstall fixture (macOS)" "0" "$INSTALL_STATUS"
    # The rollback-anchor siblings a real host can carry, seeded by hand so
    # removing them is actually exercised — without this, ".prev is gone"
    # and ".rollback-displaced is gone" below would be true by vacuous
    # absence (a fresh install never creates either) rather than by
    # anything this run actually removed.
    cp "$bin" "$bin.prev"
    : > "$bin.rollback-displaced"
    # A TLS keypair (#447), seeded by hand for the same reason as the Linux
    # variant: this install ran with TLS off (the harness default), so
    # nothing here would create one on its own, and "survives without
    # --purge, gone with it" needs a real file to make either half of that
    # claim about.
    printf 'FAKE-KEY-BYTES' > "$tls_key"
    printf 'FAKE-CERT-BYTES' > "$tls_cert"

    # ---- the real uninstall: both loaded services must be bootout, both plists removed ----
    reset_argv_logs
    STUB_LAUNCHCTL_LOADED_EXIT=0 run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh --uninstall succeeds (macOS)" "0" "$INSTALL_STATUS"
    assert_file_has "the metrics LaunchAgent is booted out" "$STUB_LAUNCHCTL_ARGV" "bootout gui/$(id -u)/app.solador.agent"
    assert_file_has "the update LaunchAgent is booted out" "$STUB_LAUNCHCTL_ARGV" "bootout gui/$(id -u)/app.solador.agent.update"
    if [ -e "$plist" ] || [ -e "$update_plist" ] || [ -e "$launcher" ] \
        || [ -e "$bin" ] || [ -e "$bin.prev" ] || [ -e "$bin.new" ] || [ -e "$bin.rollback-displaced" ] \
        || [ -e "$bin.update.lock" ] \
        || [ -e "$home/.config/solador-agent-update.last-attempt" ]; then
        fail "install.sh --uninstall removes every installer file (macOS)" "some installer file is still present"
    else
        pass "install.sh --uninstall removes every installer file (macOS)"
    fi
    case "$out" in
        *"mac-uninstall-tok-MUST-NOT-BE-PRINTED"*) fail "macOS: install.sh --uninstall never prints the token" "it appeared in the output" ;;
        *) pass "macOS: install.sh --uninstall never prints the token" ;;
    esac
    [ -e "$env_file" ] && pass "macOS: the env file survives without --purge" \
        || fail "macOS: the env file survives without --purge" "$env_file is gone"
    assert_output_has "macOS: the kept env file is named in the output" "$out" "$env_file"
    # The TLS key/certificate (#447) follow the same rule as the env file:
    # kept without --purge, named in the output — the macOS analogue of the
    # Linux variant's own assertion.
    if [ -e "$tls_key" ] && [ -e "$tls_cert" ]; then
        pass "macOS: the TLS key and certificate survive without --purge"
    else
        fail "macOS: the TLS key and certificate survive without --purge" "$tls_key or $tls_cert is gone"
    fi
    assert_output_has "macOS: the kept TLS key/certificate are named in the output" "$out" "$tls_key, $tls_cert"
    if systemctl_mutated; then
        fail "macOS: install.sh --uninstall never calls systemctl" "it did"
    else
        pass "macOS: install.sh --uninstall never calls systemctl"
    fi

    # ---- a second uninstall is a no-op ----
    reset_argv_logs
    STUB_LAUNCHCTL_LOADED_EXIT=113 run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    assert_eq "a second install.sh --uninstall is a no-op that exits 0 (macOS)" "0" "$INSTALL_STATUS"
    assert_output_has "macOS: the no-op run says nothing was installed" "$out" "Nothing installed"
    if grep -qE "^(bootout|bootstrap)" "$STUB_LAUNCHCTL_ARGV"; then
        fail "macOS: a no-op uninstall asks launchd for nothing" "bootout/bootstrap was called"
    else
        pass "macOS: a no-op uninstall asks launchd for nothing"
    fi

    # ---- --purge removes the env file ----
    reset_argv_logs
    STUB_LAUNCHCTL_LOADED_EXIT=113 run_install "$home" --uninstall --purge
    [ -e "$env_file" ] && fail "macOS: --purge removes the env file" "$env_file still exists" \
        || pass "macOS: --purge removes the env file"
    # --purge removes the TLS key/certificate too (#447), the same as the
    # Linux variant: re-pairing means a fresh certificate on the agent's
    # next start.
    if [ -e "$tls_key" ] || [ -e "$tls_cert" ]; then
        fail "macOS: --purge removes the TLS key and certificate" "$tls_key or $tls_cert still exists"
    else
        pass "macOS: --purge removes the TLS key and certificate"
    fi

    # ---- a fresh install, for the two manager-related cases below ----
    reset_argv_logs
    INSTALL_STDIN="mac-uninstall-manager-tok-MUST-NOT-BE-PRINTED
" run_install "$home" --enable-timer
    assert_eq "install.sh --enable-timer succeeds again, for the manager-failure cases (macOS)" "0" "$INSTALL_STATUS"

    # ---- no launchd gui domain refuses --uninstall, untouched ----
    reset_argv_logs
    STUB_LAUNCHCTL_DOMAIN_EXIT=125 run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    assert_eq "macOS: install.sh --uninstall refuses when there is no gui domain" "1" "$INSTALL_STATUS"
    assert_output_has "the missing-domain refusal says so" "$out" "no launchd gui domain"
    [ -e "$plist" ] && pass "a missing-domain refusal leaves the plist in place" \
        || fail "a missing-domain refusal leaves the plist in place" "$plist is gone"
    [ -x "$bin" ] && pass "a missing-domain refusal leaves the binary in place" \
        || fail "a missing-domain refusal leaves the binary in place" "$bin is gone"
    if grep -qE "^bootout" "$STUB_LAUNCHCTL_ARGV"; then
        fail "a missing-domain refusal never asks launchd to change anything" "bootout was called"
    else
        pass "a missing-domain refusal never asks launchd to change anything"
    fi

    # ---- a reachable manager that still refuses to stop the service: exit 4, files still removed ----
    reset_argv_logs
    STUB_LAUNCHCTL_LOADED_EXIT=0 STUB_LAUNCHCTL_BOOTOUT_EXIT=1 run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    assert_eq "macOS: install.sh --uninstall exits 4 when a reachable manager refuses to stop the service" "4" "$INSTALL_STATUS"
    assert_output_has "macOS: the exit-4 case says the files were removed anyway" "$out" "files were removed"
    assert_output_has "macOS: the exit-4 case says to verify by hand" "$out" "Verify by hand"
    if [ -e "$plist" ] || [ -x "$bin" ]; then
        fail "macOS: the exit-4 case still removes every installer file" "some installer file is still present"
    else
        pass "macOS: the exit-4 case still removes every installer file"
    fi
    case "$out" in
        *"mac-uninstall-manager-tok-MUST-NOT-BE-PRINTED"*)
            fail "macOS: the exit-4 case never prints the token" "it appeared in the output" ;;
        *) pass "macOS: the exit-4 case never prints the token" ;;
    esac

    # ---- a plist naming a binary this user does not own (#439 nit b) ----
    # The macOS analogue of the /opt case: nothing ships a foreign macOS
    # layout today, but unowned_service_binary's plist-reading half is real
    # code, and a hand-edited (or otherwise foreign) ProgramArguments is the
    # only way to exercise it without a genuine root-owned macOS install to
    # migrate from.
    reset_argv_logs
    INSTALL_STDIN="mac-foreign-tok-MUST-NOT-BE-PRINTED
" run_install "$home"
    assert_eq "install.sh succeeds, setting up the foreign-binary fixture (macOS)" "0" "$INSTALL_STATUS"
    awk '
        /<key>ProgramArguments<\/key>/ { want = 1 }
        want && /<string>/ {
            n++
            if (n == 2) {
                print "        <string>/opt/solador-agent/solador-agent</string>"
                next
            }
        }
        want && /<\/array>/ { want = 0 }
        { print }
    ' "$plist" > "$plist.tmp" && mv "$plist.tmp" "$plist"
    reset_argv_logs
    STUB_LAUNCHCTL_LOADED_EXIT=0 run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh --uninstall still succeeds on a foreign-binary plist (macOS)" "0" "$INSTALL_STATUS"
    assert_output_has "macOS: the foreign binary is named as left behind" "$out" \
        "left behind: /opt/solador-agent/solador-agent"
    assert_output_has "macOS: the /opt case names the actual remedy" "$out" "sudo rm -rf /opt/solador-agent"
    case "$out" in
        *"not owned by this user; see --migrate-from-opt"*)
            fail "macOS: the left-behind hint no longer points at --migrate-from-opt" \
                "the old, unchecked wording is still printed" ;;
        *) pass "macOS: the left-behind hint no longer points at --migrate-from-opt" ;;
    esac
    [ -e "$plist" ] && fail "macOS: the plist naming a foreign binary is still removed" "$plist is still present" \
        || pass "macOS: the plist naming a foreign binary is still removed"

    # ---- neither flock(1) nor perl: refuses (busy) BEFORE anything changes ----
    # ---- (#439 follow-up review) — see the Linux test's own comment; the   ----
    # ---- same code runs before the OS-specific stop calls on both          ----
    # ---- platforms, so there is no exit 5 here either.                     ----
    reset_argv_logs
    INSTALL_STDIN="mac-neithertool-tok-MUST-NOT-BE-PRINTED
" run_install "$home" --enable-timer
    assert_eq "install.sh --enable-timer succeeds, setting up the neither-tool fixture (macOS)" "0" "$INSTALL_STATUS"
    reset_argv_logs
    STUB_LAUNCHCTL_LOADED_EXIT=0 INSTALL_PATH="$STUBS:$TOOLBIN_NOFLOCK_NOPERL" run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    assert_eq "macOS: install.sh --uninstall refuses (busy) when neither flock(1) nor perl exists" "1" "$INSTALL_STATUS"
    assert_output_has "macOS: the cannot-check refusal names the lock file" "$out" "$bin.update.lock"
    assert_output_has "macOS: the cannot-check refusal says it cannot check, not that a transaction is running" "$out" \
        "cannot check the update lock"
    [ -e "$plist" ] && pass "a cannot-check refusal leaves the plist in place (macOS)" \
        || fail "a cannot-check refusal leaves the plist in place (macOS)" "$plist is gone"
    [ -x "$bin" ] && pass "a cannot-check refusal leaves the binary in place (macOS)" \
        || fail "a cannot-check refusal leaves the binary in place (macOS)" "$bin is gone"
    if grep -qE "^bootout" "$STUB_LAUNCHCTL_ARGV"; then
        fail "a cannot-check refusal never reaches launchd (macOS)" "bootout was called"
    else
        pass "a cannot-check refusal never reaches launchd (macOS)"
    fi
    INSTALL_PATH=""
    reset_argv_logs
    STUB_LAUNCHCTL_LOADED_EXIT=0 run_install "$home" --uninstall
    assert_eq "cleanup: a follow-up uninstall finishes (macOS)" "0" "$INSTALL_STATUS"

    # ---- the continuous hold, proven with a REAL flock(), starting from NO ----
    # ---- pre-existing lock file (#439 follow-up review) — see the Linux    ----
    # ---- test's own comment. Proven in both tiers.                         ----
    if [ "$HAVE_FAKEFLOCK" != true ]; then
        skip "macOS: install.sh's continuous flock hold blocks a real competing flock() (flock(1) tier)" "no perl on PATH to back the synthetic flock(1)"
        skip "macOS: install.sh's continuous flock hold blocks a real competing flock() (perl tier)" "no perl on PATH to back the synthetic flock(1)"
    else
        reset_argv_logs
        INSTALL_STDIN="mac-fakeflock-tok-MUST-NOT-BE-PRINTED
" run_install "$home" --enable-timer
        assert_eq "install.sh --enable-timer succeeds, setting up the fakeflock fixture (macOS)" "0" "$INSTALL_STATUS"
        rm -f "$bin.update.lock"
        [ -e "$bin.update.lock" ] && fail "the fresh-lock fixture starts with no pre-existing lock file (macOS)" "$bin.update.lock exists" \
            || pass "the fresh-lock fixture starts with no pre-existing lock file (macOS)"
        local probe="$TMP/mac-fakeflock-probe-result"
        rm -f "$probe"
        reset_argv_logs
        STUB_LAUNCHCTL_LOADED_EXIT=0 STUB_LAUNCHCTL_PROBE_LOCK="$bin.update.lock" STUB_LAUNCHCTL_PROBE_RESULT="$probe" \
            INSTALL_PATH="$STUBS:$TOOLBIN_FAKEFLOCK" run_install "$home" --uninstall
        assert_eq "install.sh --uninstall succeeds while holding a real flock() throughout, no pre-existing lock file (flock(1) tier, macOS)" \
            "0" "$INSTALL_STATUS"
        assert_eq "a genuinely competing flock() attempt during the stop call sees the hold as busy (flock(1) tier, macOS)" \
            "busy" "$(cat "$probe" 2>/dev/null)"
        INSTALL_PATH=""

        # ---- the same case again, via the perl tier: flock(1) hidden ----
        reset_argv_logs
        INSTALL_STDIN="mac-fakeflock-perl-tok-MUST-NOT-BE-PRINTED
" run_install "$home" --enable-timer
        assert_eq "install.sh --enable-timer succeeds, setting up the perl-tier fixture (macOS)" "0" "$INSTALL_STATUS"
        rm -f "$bin.update.lock" "$probe"
        reset_argv_logs
        STUB_LAUNCHCTL_LOADED_EXIT=0 STUB_LAUNCHCTL_PROBE_LOCK="$bin.update.lock" STUB_LAUNCHCTL_PROBE_RESULT="$probe" \
            INSTALL_PATH="$STUBS:$TOOLBIN_NOFLOCK" run_install "$home" --uninstall
        assert_eq "install.sh --uninstall succeeds while holding a real flock() throughout, no pre-existing lock file (perl tier, macOS)" \
            "0" "$INSTALL_STATUS"
        assert_eq "a genuinely competing flock() attempt during the stop call sees the hold as busy (perl tier, macOS)" \
            "busy" "$(cat "$probe" 2>/dev/null)"
        INSTALL_PATH=""
    fi

    unset SOLADOR_AGENT_RELEASE STUB_CURL_BODY STUB_UNAME_S STUB_UNAME_M STUB_SW_VERS
}

test_uninstall_arguments_and_refusals() {
    local home="$TMP/home-uninstall-args" out
    mkdir -p "$home"
    INSTALL_PATH="$NOVERIFY_PATH"
    make_checkout "$SCRIPT_DIR/../release-signing-key.pub"

    reset_argv_logs
    run_install "$home" --purge
    assert_eq "install.sh refuses --purge without --uninstall" "2" "$?"
    assert_output_has "the refusal says --purge needs --uninstall" "$(cat "$INSTALL_OUT")" "--purge only applies together with --uninstall"
    assert_untouched "--purge alone changes nothing" "$home"

    reset_argv_logs
    run_install "$home" --uninstall --enable-timer
    assert_eq "install.sh refuses --uninstall with --enable-timer" "2" "$?"
    assert_untouched "--uninstall with --enable-timer changes nothing" "$home"

    reset_argv_logs
    run_install "$home" --uninstall --migrate-from-opt
    assert_eq "install.sh refuses --uninstall with --migrate-from-opt" "2" "$?"
    assert_untouched "--uninstall with --migrate-from-opt changes nothing" "$home"

    # Order independence, the same contract test_install_arguments already
    # holds --enable-timer to.
    reset_argv_logs
    run_install "$home" --enable-timer --uninstall
    assert_eq "install.sh refuses --enable-timer ahead of --uninstall" "2" "$?"

    # ---- as root, --uninstall itself is refused (same reason --enable-timer is) ----
    local root_stubs="$TMP/stubs-uninstall-root"
    mkdir -p "$root_stubs"
    cat > "$root_stubs/id" <<STUB
#!/usr/bin/env bash
case "\${1:-}" in
    -u) echo 0 ;;
    *) exec "$(command -v id)" "\$@" ;;
esac
STUB
    chmod +x "$root_stubs/id"
    reset_argv_logs
    INSTALL_PATH="$root_stubs:$STUBS:$TOOLBIN" run_install "$home" --uninstall
    assert_eq "install.sh --uninstall as root is refused" "1" "$INSTALL_STATUS"
    assert_output_has "the root refusal says why" "$(cat "$INSTALL_OUT")" "refuses to run as root"
    assert_untouched "a root-refused uninstall changes nothing" "$home"
    if systemctl_mutated; then
        fail "a root-refused uninstall never reaches the service manager" "systemctl was called"
    else
        pass "a root-refused uninstall never reaches the service manager"
    fi
    INSTALL_PATH="$NOVERIFY_PATH"

    # ---- an unsupported OS refuses --uninstall too, dispatched before ----
    # ---- the install-only platform table even runs                    ----
    reset_argv_logs
    STUB_UNAME_S=FreeBSD run_install "$home" --uninstall
    assert_eq "install.sh --uninstall refuses an unsupported OS" "1" "$?"
    assert_output_has "the unsupported-OS refusal names the two it supports" "$(cat "$INSTALL_OUT")" "Linux (systemd) and macOS (launchd)"
    assert_untouched "an unsupported-OS uninstall changes nothing" "$home"

    # ---- #454 round-4 review: a genuinely clean host (no ~/.local/bin at ----
    # ---- all — nothing solador-related, or anything else, was ever      ----
    # ---- installed there) must create neither <bin>.update.lock nor the ----
    # ---- directory itself. mkdir -p and exec 9>>"$lock_file" used to run ----
    # ---- unconditionally, before the lock-tool check and before the OS   ----
    # ---- check further down could refuse, so EVERY refusal on a host     ----
    # ---- like this — including the unsupported-OS one just above —      ----
    # ---- left exactly that behind despite "Nothing has been changed".    ----
    # ---- Fixed by skipping the whole lock section outright when          ----
    # ---- $DEST_BIN's own directory does not exist: nothing can possibly  ----
    # ---- be installed under a directory that is not there, so there is   ----
    # ---- nothing to lock and nothing to create just to check.            ----
    reset_argv_logs
    run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh --uninstall on a clean host (no ~/.local/bin at all) exits 0" "0" "$INSTALL_STATUS"
    assert_output_has "a clean-host uninstall says nothing was installed" "$out" "Nothing installed"
    assert_untouched "a clean-host 'nothing installed' run creates neither the lock file nor ~/.local/bin" "$home"

    # ---- the no-tool refusal, on a host where ~/.local/bin EXISTS (an   ----
    # ---- operator's own directory, unrelated to solador, or simply one   ----
    # ---- left over from an earlier full uninstall — install.sh never     ----
    # ---- rmdir's it) but nothing solador-related is in it. This is the   ----
    # ---- case that actually reaches the lock-tool check at all (the      ----
    # ---- clean-host case above skips it outright), and is where the old  ----
    # ---- `mkdir -p`/`exec 9>>`-before-the-check bug is observable: it     ----
    # ---- created <bin>.update.lock — and, on a directory-less host,       ----
    # ---- ~/.local/bin too — before ever asking whether flock(1) or perl   ----
    # ---- exists, so "neither flock(1) nor perl" still left the lock file  ----
    # ---- behind despite exiting 1 "Nothing has been changed".
    mkdir -p "$home/.local/bin"
    reset_argv_logs
    INSTALL_PATH="$STUBS:$TOOLBIN_NOFLOCK_NOPERL" run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    assert_eq "install.sh --uninstall refuses (busy) when neither flock(1) nor perl exists (pre-existing, empty ~/.local/bin)" \
        "1" "$INSTALL_STATUS"
    assert_output_has "the no-tool refusal says it cannot check" "$out" "cannot check the update lock"
    [ -e "$home/.local/bin/solador-agent.update.lock" ] \
        && fail "the no-tool refusal creates no lock file" "$home/.local/bin/solador-agent.update.lock exists" \
        || pass "the no-tool refusal creates no lock file"
    if systemctl_mutated; then
        fail "the no-tool refusal never reaches the service manager" "systemctl was called"
    else
        pass "the no-tool refusal never reaches the service manager"
    fi
    INSTALL_PATH="$NOVERIFY_PATH"

    # The follow-up case the review named explicitly: after the no-tool
    # refusal above left nothing behind, a run WITH a lock tool back on PATH
    # must still say "Nothing installed" — not "Done", which is what it
    # would say if the earlier refusal had left a stray lock file for THIS
    # run to find, remove, and report as something it uninstalled.
    reset_argv_logs
    run_install "$home" --uninstall
    out="$(cat "$INSTALL_OUT")"
    assert_eq "follow-up: install.sh --uninstall with a lock tool available exits 0" "0" "$INSTALL_STATUS"
    assert_output_has "follow-up: the run says nothing was installed, not Done" "$out" "Nothing installed"
    case "$out" in
        *"==> Done"*) fail "follow-up: the run never claims Done" "it did" ;;
        *) pass "follow-up: the run never claims Done" ;;
    esac
    rm -rf "$home/.local"

    # ---- a hostile SOLADOR_AGENT_LAUNCHD_LABEL is refused on --uninstall ----
    # too, not only on a normal install — run_uninstall derives
    # PLIST_DST/UPDATE_PLIST_DST from it and rm -f's whatever that resolves
    # to, so an unvalidated "../x" would let this variable steer an
    # uninstall's deletions outside ~/Library/LaunchAgents.
    local bad_label
    for bad_label in "../x" "x@BINARY@y" ".hidden" "a b"; do
        reset_argv_logs
        SOLADOR_AGENT_LAUNCHD_LABEL="$bad_label" STUB_UNAME_S=Darwin STUB_UNAME_M=arm64 run_install "$home" --uninstall
        assert_eq "install.sh --uninstall refuses SOLADOR_AGENT_LAUNCHD_LABEL [$bad_label]" "1" "$?"
        assert_untouched "a refused label leaves an uninstall changing nothing [$bad_label]" "$home"
    done

    # ---- the traversal case specifically: prove nothing OUTSIDE ----
    # ---- ~/Library/LaunchAgents was touched, not just that the run ----
    # ---- refused. assert_untouched above only checks the FIXED paths ----
    # ---- (app.solador.agent.plist, the env file, ...) — none of them is ----
    # ---- the path "../x" actually resolves to, so a refusal that somehow ----
    # ---- still ran rm -f on that path would pass every assertion above ----
    # ---- while quietly deleting a sentinel one directory above ----
    # ---- LaunchAgents. Seeded and checked with content, not just presence, ----
    # ---- so a refusal that truncated or rewrote the file would still fail ----
    # ---- this rather than reading as "still exists, so untouched".
    local sentinel="$home/Library/x.plist"
    mkdir -p "$(dirname "$sentinel")"
    printf 'sentinel-untouched\n' > "$sentinel"
    reset_argv_logs
    SOLADOR_AGENT_LAUNCHD_LABEL="../x" STUB_UNAME_S=Darwin STUB_UNAME_M=arm64 run_install "$home" --uninstall
    assert_eq "install.sh --uninstall refuses the traversal label [../x] (repeated, for the sentinel check)" "1" "$?"
    if [ -f "$sentinel" ] && [ "$(cat "$sentinel")" = "sentinel-untouched" ]; then
        pass "a traversal label cannot delete or modify a file outside ~/Library/LaunchAgents"
    else
        fail "a traversal label cannot delete or modify a file outside ~/Library/LaunchAgents" \
            "$sentinel was removed or changed"
    fi
    rm -f "$sentinel"

    INSTALL_PATH=""
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
test_uninstall_arguments_and_refusals
test_install_preflight
test_install_release_resolution
test_install_signature_gate
test_bootstrap_extraction_and_passthrough
test_bootstrap_help_when_piped
test_bootstrap_resolves_commit_from_pax_header
test_bootstrap_truncated_runs_nothing
test_bootstrap_refuses_root
test_bootstrap_validate_ref
test_bootstrap_ref_must_be_reachable_from_main
test_bootstrap_ref_reachable_via_jq
test_bootstrap_ref_reachable_pretty_printed
test_bootstrap_failure_paths
test_bootstrap_passes_through_install_exit_status
test_bootstrap_signature_gate
test_bootstrap_rerun_hint_names_bootstrap
test_bootstrap_uninstall_rerun_hint
test_install_linux_flow
test_install_tls
test_install_tls_capability_gate
test_install_tls_no_tailnet_bind
test_install_macos_flow
test_install_update_timer_linux
test_install_update_timer_macos
test_launchd_launcher
test_launchd_launcher_update
test_update_guard_linux
test_launchd_smoke
test_uninstall_linux
test_unowned_service_binary_survives_set_e
test_uninstall_macos
test_standby_key_script
test_deploy_script_invariants

printf '\npassed %d, failed %d, skipped %d\n' "$PASSED" "$FAILED" "$SKIPPED"
if [ "$SKIPPED" -gt 0 ]; then
    printf 'NOTE: %d test(s) asserted nothing — see the SKIP lines above.\n' "$SKIPPED"
fi
if [ "$FAILED" -gt 0 ]; then
    exit 1
fi
