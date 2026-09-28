#!/usr/bin/env bash
#
# Install the Solador metrics agent from a signed release binary (#392).
#
# Usage:
#   ./deploy/install.sh                     # download, verify, install, start, verify
#   ./deploy/install.sh --enable-timer      # ...and opt in to a daily unattended update check
#   ./deploy/install.sh --migrate-from-opt  # re-point an existing /opt install (Linux)
#   ./deploy/install.sh --uninstall         # remove THIS USER's install (env file kept)
#   ./deploy/install.sh --uninstall --purge # ...and delete the env file (it holds the token) too
#   ./deploy/install.sh --help
#
# Environment:
#   SOLADOR_AGENT_RELEASE=vYYYY.M.N   install this release instead of the latest
#   SOLADOR_AGENT_BIND / _PORT        as documented in agent/README.md
#
# What it does:
#   1. Detects the platform and maps it onto one of the four published
#      targets (x86_64 / aarch64, Linux musl / macOS). Anything else refuses.
#   2. Resolves the latest published release (or SOLADOR_AGENT_RELEASE) and
#      downloads that release's raw binary plus its detached .minisig into a
#      private staging directory. No Rust toolchain is involved.
#   3. Verifies the signature with the stock `minisign` under the public key
#      shipped with this checkout, agent/release-signing-key.pub. Only a
#      verified binary is ever made executable or asked its `--version`.
#   4. Installs it, user-owned, at ~/.local/bin/solador-agent — staged beside
#      the live path and renamed over it, so a running agent is never
#      overwritten in place; the displaced binary is kept as .prev.
#   5. Writes ~/.config/solador-agent.env with the bearer token (prompted
#      without echo, or reused), the bind address and the port, mode 0600.
#   6. Installs and starts the service: a systemd user unit on Linux, a
#      LaunchAgent (gui/<uid>) on macOS, each rendered with the actual paths.
#   7. Verifies /v1/health, authenticated, reports the verified binary's own
#      version. A running service alone proves nothing about which binary.
#   8. ONLY with --enable-timer (#394): installs a separate, daily unattended
#      update job — a systemd user timer + oneshot on Linux (plus the
#      oneshot's ExecCondition= guard, ~/.local/bin/solador-agent-update-guard,
#      #411), a second LaunchAgent (<label>.update) on macOS — that runs the
#      installed `solador-agent update`. Off by default: a default install
#      creates no updater job and makes no update check. Daily, no catch-up:
#      the first check is a day after enabling, a missed one is discarded,
#      never made up at wake or login. A re-run WITHOUT the flag leaves an
#      earlier opt-in exactly as it is; it is revoked by the disable/remove
#      commands in agent/README.md, or removed along with everything else
#      by --uninstall (below).
#
# Exit status: 0 installed and serving (and, with the flag, scheduled); 1 the
# install failed or was refused, nothing is serving that this run put there;
# 2 usage; 3 the metrics service IS installed and serving but the
# --enable-timer opt-in failed — the "Done" block above the error is true.
#
# --uninstall (#439): removes, for the INVOKING USER ONLY, everything a
# default install (or an opted-in --enable-timer) put on disk — disables and
# stops both service-manager jobs, removes both unit/plist pairs, the binary
# and its .prev/.new/.update.lock/.rollback-displaced siblings, the macOS
# launcher, the Linux guard, and the update stamp. The env file (the token)
# is KEPT and named in the output unless --purge is also given. Refuses as
# root (same reason --enable-timer does) or when the service manager itself
# is unreachable (same reachability check the install path makes — a
# sudo -u/su session, say), never runs loginctl disable-linger, and refuses,
# untouched, while solador-agent update/rollback holds <bin>.update.lock — an
# uninstall mid-transaction would race that transaction's own binary swap.
# Idempotent: a second run finds nothing left and says so. Exit status: 0
# uninstalled (or already clean — a second run is a no-op) — earned only when
# every service-manager call the reachability check let through actually
# succeeded; 1 refused before anything changed (root, the manager
# unreachable, the lock, a hostile SOLADOR_AGENT_LAUNCHD_LABEL, an
# unsupported platform); 2 usage (--purge without --uninstall, or --uninstall
# combined with --migrate-from-opt or --enable-timer — the two do not
# combine); 4 every file was still removed (best-effort), but at least one
# reachable manager refused a specific stop request — verify by hand that
# nothing is still running before trusting this host is clean.
#
# Re-running is safe: it reuses the token, replaces the binary and restarts.
# Nothing here uses sudo. redeploy.sh remains the from-source path for our own
# Linux hosts and is unchanged.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=agent/deploy/lib.sh
source "$SCRIPT_DIR/lib.sh"

BIN_NAME="solador-agent"
RELEASE_REPO_URL="https://github.com/Sassy-Dog/solador"
# Shipped with this checkout, beside deploy/. Never downloaded, never
# overridable: see verify_agent_signature in lib.sh.
SIGNING_PUBKEY="$SCRIPT_DIR/../release-signing-key.pub"

ENV_FILE="$HOME/.config/${BIN_NAME}.env"
# User-owned, no sudo — the owner decision recorded on #392. /opt is no longer
# a destination; an existing /opt install is detected below and migrated only
# on request.
INSTALL_DIR="$HOME/.local/bin"
DEST_BIN="$INSTALL_DIR/$BIN_NAME"

# Linux: the systemd user unit.
UNIT_SRC="$SCRIPT_DIR/${BIN_NAME}.service"
UNIT_DST="$HOME/.config/systemd/user/${BIN_NAME}.service"

# macOS: the LaunchAgent, its launcher, and where launchd sends the agent's
# stderr. The label and the gui/<uid> domain are the service identity #393's
# restart contract consumes; change them together with app.solador.agent.plist
# and agent/README.md or not at all. The label override exists for the test
# harness, which bootstraps a throwaway service beside any real one.
LAUNCHD_LABEL="${SOLADOR_AGENT_LAUNCHD_LABEL:-app.solador.agent}"
PLIST_SRC="$SCRIPT_DIR/app.solador.agent.plist"
PLIST_DST="$HOME/Library/LaunchAgents/${LAUNCHD_LABEL}.plist"
LAUNCHER_SRC="$SCRIPT_DIR/run-agent.sh"
LAUNCHER_DST="$INSTALL_DIR/${BIN_NAME}-launchd"
LOG_FILE="$HOME/Library/Logs/${BIN_NAME}.log"

# The opt-in unattended update job (#394): its own units and its own label,
# never a property of the metrics service. On Linux a user timer + oneshot;
# on macOS a second LaunchAgent, <metrics label>.update, so the updater is
# always the metrics label's sibling — including under the harness's
# throwaway label — and `launchctl kickstart -k` of one never touches the
# other. Its log is separate too: `tail` of one file answers "did the check
# run last night" without the metrics agent's lines in between.
UPDATE_NAME="${BIN_NAME}-update"
UPDATE_UNIT_SRC="$SCRIPT_DIR/${UPDATE_NAME}.service"
UPDATE_UNIT_DST="$HOME/.config/systemd/user/${UPDATE_NAME}.service"
UPDATE_TIMER_SRC="$SCRIPT_DIR/${UPDATE_NAME}.timer"
UPDATE_TIMER_DST="$HOME/.config/systemd/user/${UPDATE_NAME}.timer"
# The oneshot's ExecCondition= guard (#411): the Linux stand-in for the
# launcher's update mode, copied out of the checkout beside the binary so
# deleting the clone does not break the job. Linux only — on macOS the
# launcher is the guard. ExecCondition= itself exists since systemd 243; an
# older manager logs "Unknown key" and runs the updater UNGUARDED, which is
# not the opt-in that was asked for, so the opt-in is refused below that.
GUARD_SRC="$SCRIPT_DIR/update-guard.sh"
GUARD_DST="$INSTALL_DIR/${UPDATE_NAME}-guard"
SYSTEMD_MIN_FOR_GUARD=243
UPDATE_LABEL="${LAUNCHD_LABEL}.update"
UPDATE_PLIST_SRC="$SCRIPT_DIR/app.solador.agent.update.plist"
UPDATE_PLIST_DST="$HOME/Library/LaunchAgents/${UPDATE_LABEL}.plist"
UPDATE_LOG_FILE="$HOME/Library/Logs/${UPDATE_NAME}.log"
# The no-catch-up stamp both guards write (update-guard.sh on Linux,
# run-agent.sh's update mode on macOS) — one path, one name, on both
# platforms; --uninstall is the one thing here that reads it by name rather
# than writing it.
UPDATE_STAMP="$HOME/.config/${UPDATE_NAME}.last-attempt"

# The pre-rename install. Everything below that mentions these exists to hand a
# host over from the old agent without the operator noticing anything except a
# version bump.
LEGACY_BIN_NAME="devcanopy-agent"
LEGACY_ENV_FILE="$HOME/.config/${LEGACY_BIN_NAME}.env"
LEGACY_UNIT="$HOME/.config/systemd/user/${LEGACY_BIN_NAME}.service"

# ---- arguments ---------------------------------------------------------------
# There was no argument parser before #392, and the point of adding one is
# that an argument this script does not understand is REFUSED, never ignored
# into a silent default install.
usage() {
    # The header comment, found rather than hardcoded by line range.
    awk 'NR > 2 && /^#/ { sub(/^# ?/, ""); print; next } NR > 2 { exit }' "$0"
}

MIGRATE_FROM_OPT=false
# Consent, and nothing else, turns the updater on. false here means "do not
# touch unattended updating either way": neither create it nor revoke an
# earlier opt-in.
ENABLE_TIMER=false
# --uninstall (#439): the mirror of the install this script otherwise
# performs. --purge only ever modifies what --uninstall does (it is refused
# alone, below) — it is never a standalone mode.
UNINSTALL=false
PURGE=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --migrate-from-opt) MIGRATE_FROM_OPT=true ;;
        --enable-timer) ENABLE_TIMER=true ;;
        --uninstall) UNINSTALL=true ;;
        --purge) PURGE=true ;;
        -h | --help | help)
            usage
            exit 0
            ;;
        *)
            echo "ERROR: unknown argument '$1'. Use: [--enable-timer] [--migrate-from-opt] [--uninstall [--purge]] [--help]" >&2
            exit 2
            ;;
    esac
    shift
done

# Every "re-run this" hint below prints $RERUN_CMD, not $0 directly. Run
# straight from a checkout, $0 IS the command to re-run. Run through
# bootstrap.sh (#434), $0 is a path under ITS staging directory —
# ~/.cache/solador-agent-bootstrap.* — which is gone (bootstrap.sh's own EXIT
# trap removes it) by the time anyone could act on a hint built from it.
# bootstrap.sh exports SOLADOR_AGENT_BOOTSTRAP=1 immediately before running
# this script for exactly this: a hint here names the command that will
# still exist next time.
RERUN_CMD="$0"
if [ "${SOLADOR_AGENT_BOOTSTRAP:-}" = "1" ]; then
    RERUN_CMD="bash bootstrap.sh [--ref <sha>]"
fi

# --purge only ever modifies --uninstall; --uninstall does not combine with a
# flag that installs or repoints something. Refused here, before OS detection
# even runs, the same "usage error before anything happens" contract every
# other argument gets.
if [ "$PURGE" = true ] && [ "$UNINSTALL" != true ]; then
    echo "ERROR: --purge only applies together with --uninstall (it deletes the env file" >&2
    echo "       that --uninstall would otherwise keep). Use: $RERUN_CMD --uninstall --purge" >&2
    exit 2
fi
if [ "$UNINSTALL" = true ] && { [ "$MIGRATE_FROM_OPT" = true ] || [ "$ENABLE_TIMER" = true ]; }; then
    echo "ERROR: --uninstall does not combine with --migrate-from-opt or --enable-timer —" >&2
    echo "       it removes an install; it does not create or repoint one." >&2
    exit 2
fi

# The label becomes a filename under ~/Library/LaunchAgents and a rendered
# plist value; it is a test seam, not a place for a path or a placeholder.
# Validated HERE — before --uninstall's own dispatch below, not only inside
# the install-only preflight further down — because run_uninstall derives
# PLIST_DST/UPDATE_PLIST_DST from it too and `rm -f`s whatever that resolves
# to: an unvalidated "../x" would let SOLADOR_AGENT_LAUNCHD_LABEL steer an
# uninstall's own deletions outside ~/Library/LaunchAgents. UPDATE_LABEL
# ("${LAUNCHD_LABEL}.update") needs no separate check: a label safe by this
# rule stays safe with ".update" appended.
case "$LAUNCHD_LABEL" in
    [A-Za-z0-9]*) ;;
    *) echo "ERROR: SOLADOR_AGENT_LAUNCHD_LABEL '$LAUNCHD_LABEL' must start with a letter or digit." >&2; exit 1 ;;
esac
case "$LAUNCHD_LABEL" in
    *[!A-Za-z0-9._-]*) echo "ERROR: SOLADOR_AGENT_LAUNCHD_LABEL '$LAUNCHD_LABEL' may contain only letters, digits, '.', '_' and '-'." >&2; exit 1 ;;
esac

# ---- uninstall (#439) ---------------------------------------------------------
# The mirror of everything above: for the INVOKING USER ONLY, remove what a
# default install (or an opted-in --enable-timer) put on disk. Every path
# used below (ENV_FILE, DEST_BIN, UNIT_DST, PLIST_DST, ...) is already defined
# — they are plain $HOME-relative strings set unconditionally near the top of
# this script, before argument parsing even runs.
#
# The env file is the one thing kept by default: it holds the bearer token,
# and #439's own motivating case (moving the agent to another Unix user) is
# install-as-the-new-user, re-pair the token in the cockpit, THEN
# uninstall-with-purge as the old user — so the token is never read back out
# of a file by hand, and a plain --uninstall (any other reason to remove an
# install) does not delete a credential the operator may still want.
UNINSTALL_REMOVED=false

# uninstall_remove <path> <description>: rm -f, and note it in both the
# summary printed to the operator and UNINSTALL_REMOVED — the fact "a second
# run is a no-op" is read from, rather than an exit code guessed at.
uninstall_remove() {
    local path="$1" desc="$2"
    if [ -e "$path" ] || [ -L "$path" ]; then
        rm -f "$path"
        UNINSTALL_REMOVED=true
        echo "    removed: $desc ($path)"
    fi
}

run_uninstall() {
    # Same refusal, and the same reason, as --enable-timer's: the install is
    # user-owned, so removing it is too.
    if [ "$(id -u)" = "0" ]; then
        echo "ERROR: --uninstall refuses to run as root. The agent installs and runs" >&2
        echo "       user-owned, with no sudo anywhere in this chain, and uninstall only" >&2
        echo "       ever touches the invoking user's own files. Run this as the user the" >&2
        echo "       agent is installed as." >&2
        return 1
    fi

    # The same service-manager reachability the install path refuses on
    # (preflight, below) before touching anything — a `sudo -u`/`su` session,
    # or one with no XDG_RUNTIME_DIR, cannot ask systemd to stop anything, and
    # discovering that mid-uninstall (after the unit file is already gone) is
    # exactly the half-changed state this check exists to prevent: the files
    # say "removed" while a manager none of this could reach may still be
    # running the process.
    case "$OS" in
        Linux)
            if ! systemctl --user show-environment >/dev/null 2>&1; then
                echo "ERROR: cannot reach this user's systemd manager (systemctl --user)." >&2
                echo "       Run --uninstall from a real login session for this user (not via" >&2
                echo "       sudo -u or su), or set XDG_RUNTIME_DIR=/run/user/\$(id -u). Nothing" >&2
                echo "       has been changed." >&2
                return 1
            fi
            ;;
        Darwin)
            if ! launchctl print "gui/$(id -u)" >/dev/null 2>&1; then
                echo "ERROR: no launchd gui domain for uid $(id -u) — this user has no login session." >&2
                echo "       --uninstall needs the same login session the agent runs in; log in at" >&2
                echo "       the console (or via Screen Sharing) as this user and re-run. Nothing" >&2
                echo "       has been changed." >&2
                return 1
            fi
            ;;
    esac

    # The transaction lock solador-agent update/rollback hold for their
    # lifetime (agent/src/update.rs, #393). It is never removed once created,
    # so its mere presence proves nothing — update_lock_busy (lib.sh) is what
    # asks whether it is actually HELD right now. Checked before anything else
    # touches disk: uninstalling mid-transaction would race that transaction's
    # own binary swap, the same race install.sh itself must not run either.
    local lock_file="$DEST_BIN.update.lock"
    if update_lock_busy "$lock_file"; then
        echo "ERROR: $lock_file is held — an update or rollback is in progress." >&2
        echo "       Uninstalling now would race that transaction's own binary swap." >&2
        echo "       Wait for it to finish (or fail) and re-run." >&2
        return 1
    fi

    echo "==> Uninstalling $BIN_NAME for $(id -un) ($OS)"

    # Set when a service-manager call the reachability check above should
    # have let succeed reports an error anyway — a narrower, rarer case than
    # "the manager was unreachable" (already refused above), but one where
    # this run cannot promise the live process actually stopped even though
    # its files are all still removed below (best-effort, per this script's
    # own --uninstall contract). Read at the end to pick the summary line and
    # the exit status: 0 ("Done") is a claim only a manager that agreed to
    # every stop request has earned.
    local MANAGER_CALL_FAILED=false

    case "$OS" in
        Linux)
            if [ -f "$UNIT_DST" ]; then
                # The line printed reflects what actually happened, not what
                # was attempted: a failed disable/stop is reported as such,
                # never as "stopped".
                if systemctl --user disable --now "$BIN_NAME" 2>/dev/null; then
                    echo "    stopped and disabled $BIN_NAME.service"
                else
                    echo "    (systemctl --user disable --now $BIN_NAME reported an error;" >&2
                    echo "     could not confirm it is stopped — removing its files anyway)" >&2
                    MANAGER_CALL_FAILED=true
                fi
                UNINSTALL_REMOVED=true
            fi
            if [ -f "$UPDATE_TIMER_DST" ]; then
                if systemctl --user disable --now "$UPDATE_NAME.timer" 2>/dev/null; then
                    echo "    stopped and disabled $UPDATE_NAME.timer"
                else
                    echo "    (systemctl --user disable --now $UPDATE_NAME.timer reported an error;" >&2
                    echo "     could not confirm it is stopped — removing its files anyway)" >&2
                    MANAGER_CALL_FAILED=true
                fi
                UNINSTALL_REMOVED=true
            fi
            uninstall_remove "$UNIT_DST" "systemd unit"
            uninstall_remove "$UNIT_DST.prev" "systemd unit (previous, from a migration)"
            uninstall_remove "$UPDATE_UNIT_DST" "update oneshot unit"
            uninstall_remove "$UPDATE_TIMER_DST" "update timer"
            # Only when something above actually changed: a no-op second run
            # asks systemd for nothing, the same "no manager calls" contract
            # every other refusal/no-op path in this script keeps.
            if [ "$UNINSTALL_REMOVED" = true ]; then
                systemctl --user daemon-reload 2>/dev/null || true
            fi
            uninstall_remove "$GUARD_DST" "update guard"
            ;;
        Darwin)
            local metrics_svc update_svc
            metrics_svc="gui/$(id -u)/$LAUNCHD_LABEL"
            update_svc="gui/$(id -u)/$UPDATE_LABEL"
            if launchctl print "$metrics_svc" >/dev/null 2>&1; then
                if launchctl bootout "$metrics_svc" 2>/dev/null; then
                    echo "    stopped $metrics_svc"
                else
                    echo "    (launchctl bootout $metrics_svc reported an error;" >&2
                    echo "     could not confirm it is stopped — removing its files anyway)" >&2
                    MANAGER_CALL_FAILED=true
                fi
                UNINSTALL_REMOVED=true
            fi
            if launchctl print "$update_svc" >/dev/null 2>&1; then
                if launchctl bootout "$update_svc" 2>/dev/null; then
                    echo "    stopped $update_svc"
                else
                    echo "    (launchctl bootout $update_svc reported an error;" >&2
                    echo "     could not confirm it is stopped — removing its files anyway)" >&2
                    MANAGER_CALL_FAILED=true
                fi
                UNINSTALL_REMOVED=true
            fi
            uninstall_remove "$PLIST_DST" "LaunchAgent plist"
            uninstall_remove "$UPDATE_PLIST_DST" "update LaunchAgent plist"
            uninstall_remove "$LAUNCHER_DST" "launcher"
            ;;
        *)
            echo "ERROR: --uninstall supports Linux (systemd) and macOS (launchd); this is $OS." >&2
            return 1
            ;;
    esac

    # Re-checked, not assumed still true from the single check above: the
    # service-manager calls just made can take long enough for a
    # concurrently started `solador-agent update`/`rollback` to acquire the
    # lock in between. This narrows that window; it does not close it (the
    # manager calls above are not themselves reversible), which is the
    # accepted scope #439 was written against.
    if update_lock_busy "$lock_file"; then
        echo "ERROR: $lock_file is now held — an update or rollback started while this" >&2
        echo "       uninstall was stopping the service. The service and its unit/plist" >&2
        echo "       files are already removed above; leaving the binary and the lock" >&2
        echo "       alone rather than deleting them out from under that transaction." >&2
        echo "       Wait for it to finish (or fail) and re-run to finish removing the binary." >&2
        return 1
    fi

    uninstall_remove "$DEST_BIN" "binary"
    uninstall_remove "$DEST_BIN.prev" "binary (previous)"
    uninstall_remove "$DEST_BIN.new" "binary (staged)"
    # A leftover from a half-done `solador-agent rollback` (agent/src/update.rs):
    # the live binary's only copy at the moment the swap ran, kept until an
    # operator moves it by hand. Once the whole install is going away there is
    # nothing left for it to be a backup of.
    uninstall_remove "$DEST_BIN.rollback-displaced" "binary (rollback-displaced, from a half-done rollback)"
    uninstall_remove "$lock_file" "update transaction lock"
    uninstall_remove "$UPDATE_STAMP" "update stamp"

    if [ "$PURGE" = true ]; then
        uninstall_remove "$ENV_FILE" "env file (--purge: it held the bearer token)"
    elif [ -e "$ENV_FILE" ]; then
        echo "    left:    env file ($ENV_FILE) — holds the bearer token; remove it with:"
        echo "               $RERUN_CMD --uninstall --purge"
    fi

    echo
    if [ "$MANAGER_CALL_FAILED" = true ]; then
        # Every file this run knows about is gone (or was already gone), but
        # "Done: uninstalled" is a claim about the SERVICE, not the files —
        # and a manager that answered `show-environment`/`print gui/<uid>`
        # (the reachability check above) yet still refused a specific stop
        # request is a narrower, rarer thing than unreachable, not a reason
        # to fabricate the stronger claim. Exit 4 is distinct from both 0
        # ("Done", earned) and 1 ("refused, nothing changed" — false here).
        echo "==> $BIN_NAME's files were removed for $(id -un), but at least one" >&2
        echo "    service-manager call above could not confirm the service actually" >&2
        echo "    stopped. Verify by hand that nothing is still running:" >&2
        service_inspect_hint "$LAUNCHD_LABEL" >&2
        return 4
    fi
    if [ "$UNINSTALL_REMOVED" = true ]; then
        echo "==> Done: $BIN_NAME uninstalled for $(id -un)."
    else
        echo "==> Nothing installed for $(id -un); nothing to do."
    fi
    return 0
}

# ---- preflight ---------------------------------------------------------------
# Everything that can refuse, refuses HERE — before a byte is downloaded and
# before any installed state changes. A half-installed host is the outcome
# every check below exists to prevent.

OS="$(uname -s)"
ARCH="$(uname -m)"

# --uninstall is its own path, dispatched before any of the install-only
# preflight below (minisign, the macOS floor, Tailscale, a release download —
# none of it applies to removing files this user's own earlier run created).
# run_uninstall's own return is propagated as-is (0, 1 or 4 — see its header
# comment), never collapsed to a bare 0/1: 4 is a distinct claim from both.
if [ "$UNINSTALL" = true ]; then
    uninstall_status=0
    run_uninstall || uninstall_status=$?
    exit "$uninstall_status"
fi

TRIPLE="$(agent_target_for "$OS" "$ARCH")" || exit 1
echo "==> Platform: $OS $ARCH -> $TRIPLE"

case "$OS" in
    Darwin)
        # The published macOS floor is 11.0 (AGENT_MACOS_MIN_VERSION in
        # scripts/config.sh, read back out of each Mach-O by the build). A
        # binary that will not launch is worse than a refusal that says why.
        MACOS_VERSION="$(sw_vers -productVersion 2>/dev/null || true)"
        MACOS_MAJOR="${MACOS_VERSION%%.*}"
        case "$MACOS_MAJOR" in
            '' | *[!0-9]*)
                echo "ERROR: could not read the macOS version (sw_vers said '${MACOS_VERSION:-<nothing>}')." >&2
                exit 1
                ;;
        esac
        if [ "$MACOS_MAJOR" -lt 11 ]; then
            echo "ERROR: macOS $MACOS_VERSION is below the agent's floor of 11.0 (Big Sur)." >&2
            exit 1
        fi
        command -v launchctl >/dev/null 2>&1 || { echo "ERROR: launchctl not found." >&2; exit 1; }
        command -v plutil >/dev/null 2>&1 || { echo "ERROR: plutil not found (needed to validate the rendered plist)." >&2; exit 1; }
        # A LaunchAgent lives in the user's gui/<uid> domain, which exists only
        # while that user has a login session. Over SSH with nobody logged in
        # at the console there is nothing to bootstrap into, and launchctl's
        # own message for that ("Input/output error") names nothing.
        if ! launchctl print "gui/$(id -u)" >/dev/null 2>&1; then
            echo "ERROR: no launchd gui domain for uid $(id -u) — this user has no login session." >&2
            echo "       A LaunchAgent starts at login and runs inside that session; it is" >&2
            echo "       not boot-without-login coverage. Log in at the console (or via" >&2
            echo "       Screen Sharing) as this user and re-run. Nothing has been changed." >&2
            exit 1
        fi
        [ -f "$PLIST_SRC" ] || { echo "ERROR: $PLIST_SRC not found — this checkout is incomplete." >&2; exit 1; }
        [ -f "$LAUNCHER_SRC" ] || { echo "ERROR: $LAUNCHER_SRC not found — this checkout is incomplete." >&2; exit 1; }
        if [ "$ENABLE_TIMER" = true ]; then
            [ -f "$UPDATE_PLIST_SRC" ] || { echo "ERROR: $UPDATE_PLIST_SRC not found — this checkout is incomplete." >&2; exit 1; }
        fi
        ;;
    Linux)
        command -v systemctl >/dev/null 2>&1 || { echo "ERROR: systemctl not found (Linux + systemd required)." >&2; exit 1; }
        # The Linux analogue of the gui-domain check above: `systemctl --user`
        # needs the user manager, which `sudo -u`, `su` and a session with no
        # XDG_RUNTIME_DIR do not have. Finding that out at `daemon-reload`,
        # after the binary and env file are written, is the half-install
        # this block exists to prevent.
        if ! systemctl --user show-environment >/dev/null 2>&1; then
            echo "ERROR: cannot reach this user's systemd manager (systemctl --user)." >&2
            echo "       Run the installer from a real login session for this user (not via" >&2
            echo "       sudo -u or su), or set XDG_RUNTIME_DIR=/run/user/\$(id -u). Nothing" >&2
            echo "       has been changed." >&2
            exit 1
        fi
        [ -f "$UNIT_SRC" ] || { echo "ERROR: $UNIT_SRC not found — this checkout is incomplete." >&2; exit 1; }
        if [ "$ENABLE_TIMER" = true ]; then
            [ -f "$UPDATE_UNIT_SRC" ] || { echo "ERROR: $UPDATE_UNIT_SRC not found — this checkout is incomplete." >&2; exit 1; }
            [ -f "$UPDATE_TIMER_SRC" ] || { echo "ERROR: $UPDATE_TIMER_SRC not found — this checkout is incomplete." >&2; exit 1; }
            [ -f "$GUARD_SRC" ] || { echo "ERROR: $GUARD_SRC not found — this checkout is incomplete." >&2; exit 1; }
            # The RUNNING user manager's version — `systemctl --user show -p
            # Version` answers from the daemon (`256.11-1.fc41`, `249.11-
            # 0ubuntu3.12`; the leading integer is the version) — not
            # `systemctl --version`, which describes the client on this
            # PATH: a package upgraded without a daemon-reexec, or a
            # toolbox's systemctl, passes that and yields exactly the
            # unguarded opt-in this refuses. Anything unparseable is
            # "cannot tell", and an opt-in whose guard the manager might
            # ignore is not made on a guess.
            SYSTEMD_VERSION="$(systemctl --user show -p Version --value 2>/dev/null | head -n1 | sed -n 's/^\([0-9][0-9]*\).*/\1/p')"
            case "$SYSTEMD_VERSION" in
                '' | *[!0-9]*)
                    echo "ERROR: --enable-timer is refused: cannot read the running systemd version (systemctl --user" >&2
                    echo "       show -p Version said '$(systemctl --user show -p Version --value 2>/dev/null | head -n1)'). The update job's guard" >&2
                    echo "       is an ExecCondition=, which needs systemd >= $SYSTEMD_MIN_FOR_GUARD; an older manager would ignore" >&2
                    echo "       it and run the updater unguarded. Nothing has been changed." >&2
                    exit 1
                    ;;
            esac
            if [ "$SYSTEMD_VERSION" -lt "$SYSTEMD_MIN_FOR_GUARD" ]; then
                echo "ERROR: --enable-timer is refused: systemd $SYSTEMD_VERSION is older than $SYSTEMD_MIN_FOR_GUARD, the first" >&2
                echo "       version with ExecCondition=, which the update job's guard needs. An older manager" >&2
                echo "       would ignore the guard and run the updater unguarded. Nothing has been changed." >&2
                exit 1
            fi
        fi
        ;;
esac

# The opt-in's own refusals, before a byte is downloaded (the owner decision
# on #394: unprivileged ownership). `solador-agent update` refuses to run as
# root and refuses an install directory it cannot write to, so a job created
# past either of these would fail on its first firing and every one after —
# a consent recorded for something that can never happen. Neither check
# applies to a default install, which creates no job.
if [ "$ENABLE_TIMER" = true ]; then
    if [ "$(id -u)" = "0" ]; then
        echo "ERROR: --enable-timer is refused as root: the install is user-owned and the updater" >&2
        echo "       refuses to run as root, so a job created now would fail on every firing." >&2
        echo "       Run the installer as the user the agent should run as. Nothing has been changed." >&2
        exit 1
    fi
    if [ -e "$INSTALL_DIR" ] && [ ! -w "$INSTALL_DIR" ]; then
        echo "ERROR: --enable-timer is refused: $INSTALL_DIR is not writable by $(id -un), and" >&2
        echo "       the updater must be able to stage and rename a binary there. Nothing has been" >&2
        echo "       changed; fix the directory's ownership (no sudo is used here) and re-run." >&2
        exit 1
    fi
    if [ -e "$DEST_BIN" ] && [ ! -w "$DEST_BIN" ]; then
        echo "ERROR: --enable-timer is refused: $DEST_BIN is not writable by $(id -un)." >&2
        echo "       Nothing has been changed; fix its ownership (no sudo is used here) and re-run." >&2
        exit 1
    fi
fi

command -v curl >/dev/null 2>&1 || { echo "ERROR: curl not found (needed to download the release and to verify health)." >&2; exit 1; }
if ! command -v minisign >/dev/null 2>&1; then
    echo "ERROR: minisign not found in PATH — the downloaded binary cannot be verified." >&2
    echo "       Install the stock minisign first:" >&2
    echo "         macOS:                       brew install minisign" >&2
    echo "         Debian 12+ / Ubuntu 24.04+:  sudo apt install minisign" >&2
    echo "         Fedora:                      sudo dnf install minisign" >&2
    echo "         otherwise:                   https://jedisct1.github.io/minisign/" >&2
    echo "       This installer never installs a verifier, a package manager or a" >&2
    echo "       toolchain on your behalf. Nothing has been changed." >&2
    exit 1
fi
# And it must BE minisign (rsign answers `-V` with its version and exit 0):
# refused here, before a download, and again at verification time.
require_real_minisign || exit 1
[ -f "$SIGNING_PUBKEY" ] || {
    echo "ERROR: $SIGNING_PUBKEY not found — this checkout is incomplete." >&2
    echo "       The installer needs the committed public key beside deploy/ to verify a download." >&2
    exit 1
}
for tool in mktemp install chmod mv cp cmp awk sed grep; do
    command -v "$tool" >/dev/null 2>&1 || { echo "ERROR: $tool not found." >&2; exit 1; }
done

# Both service formats carry the install path verbatim; refuse a HOME neither
# can hold before anything is rendered into it. Spaces are fine.
check_install_path "$HOME" || exit 1

# Linux: an existing unit that starts something other than the user-owned
# destination — the pre-#392 /opt layout, most likely. Silently re-pointing it
# is exactly the "silent migration" the owner decision on #392 rules out, so
# stop, before any installed-state change, and say what the explicit path is.
existing_exec_start() {
    [ -f "$UNIT_DST" ] || return 0
    awk '/^ExecStart=/ { sub(/^ExecStart=/, ""); print; exit }' "$UNIT_DST" \
        | sed -e 's/^"//' -e 's/"$//'
}
if [ "$OS" = "Linux" ]; then
    EXISTING_EXEC="$(existing_exec_start)"
    if [ -n "$EXISTING_EXEC" ] && [ "$EXISTING_EXEC" != "$DEST_BIN" ]; then
        if [ "$MIGRATE_FROM_OPT" = true ]; then
            echo "==> Migrating: the existing unit starts $EXISTING_EXEC; regenerating it for $DEST_BIN."
            echo "    The old binary is left where it is (and copied in as $DEST_BIN.prev)."
        else
            echo "ERROR: an existing $BIN_NAME user service starts" >&2
            echo "         $EXISTING_EXEC" >&2
            echo "       and since #392 new installs are user-owned at" >&2
            echo "         $DEST_BIN" >&2
            echo "       Nothing has been changed. To migrate explicitly, re-run with" >&2
            echo "         $RERUN_CMD --migrate-from-opt" >&2
            echo "       which keeps $ENV_FILE (token, bind, port) as it is, installs the" >&2
            echo "       verified binary at $DEST_BIN, regenerates the unit from the template" >&2
            echo "       with that path (the displaced unit is kept as .service.prev), restarts," >&2
            echo "       and verifies /v1/health serves the new version. The old binary is not" >&2
            echo "       touched and no sudo is used; remove it by hand once you are satisfied" >&2
            echo "       (e.g. sudo rm -rf /opt/solador-agent)." >&2
            if [ "$ENABLE_TIMER" = true ]; then
                echo "       --enable-timer needs that migration first: the updater it schedules cannot" >&2
                echo "       replace a binary this user does not own. Both flags together do it in" >&2
                echo "       one run:  $RERUN_CMD --migrate-from-opt --enable-timer" >&2
            fi
            exit 1
        fi
    elif [ "$MIGRATE_FROM_OPT" = true ]; then
        echo "==> --migrate-from-opt: no existing unit points elsewhere; proceeding as a normal install."
    fi
elif [ "$MIGRATE_FROM_OPT" = true ]; then
    echo "ERROR: --migrate-from-opt applies to a Linux systemd install; there is no /opt layout on $OS." >&2
    exit 2
fi

# ---- bind address and port ---------------------------------------------------
# Resolved HERE, in preflight, because a host with no Tailscale and no
# SOLADOR_AGENT_BIND is refused — and that refusal must land before a download,
# a signature check and a token prompt, not after them.
# Default to the host's Tailscale IP so the agent only listens on the tailnet,
# never the public NIC. Honor a pre-set SOLADOR_AGENT_BIND (e.g. to opt into
# 0.0.0.0 behind a firewall) and reuse an existing value from the env file.
# Every env-file read goes through env_value (lib.sh): the same CR/whitespace/
# quote stripping the launcher and systemd apply, so what is verified is what
# the service actually starts with.
EXISTING_BIND="$(env_value "$ENV_FILE" SOLADOR_AGENT_BIND)"
# ...and the same carry-over, so a host that deliberately opted out of the
# tailnet default keeps its choice instead of silently reverting to detection.
if [ -z "$EXISTING_BIND" ]; then
    EXISTING_BIND="$(env_value "$LEGACY_ENV_FILE" DEVCANOPY_AGENT_BIND)"
fi

detect_tailscale_ip() {
    # Prefer the tailscale CLI; then the CLI the Mac app bundles rather than
    # putting on PATH; then scan the tailscale0 interface (Linux).
    if command -v tailscale >/dev/null 2>&1; then
        tailscale ip -4 2>/dev/null | head -n1 && return 0
    fi
    if [ "$OS" = "Darwin" ] && [ -x "/Applications/Tailscale.app/Contents/MacOS/Tailscale" ]; then
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale" ip -4 2>/dev/null | head -n1 && return 0
    fi
    if command -v ip >/dev/null 2>&1; then
        ip -4 -o addr show tailscale0 2>/dev/null \
            | grep -oE 'inet [0-9.]+' | awk '{print $2}' | head -n1 && return 0
    fi
    return 1
}

if [ -n "${SOLADOR_AGENT_BIND:-}" ]; then
    BIND="$SOLADOR_AGENT_BIND"
    BIND_SOURCE="SOLADOR_AGENT_BIND"
elif [ -n "$EXISTING_BIND" ]; then
    BIND="$EXISTING_BIND"
    BIND_SOURCE="kept from the existing env file"
else
    BIND="$(detect_tailscale_ip || true)"
    BIND_SOURCE="detected Tailscale IP"
fi
if [ -z "$BIND" ]; then
    echo "ERROR: could not detect a Tailscale IP for SOLADOR_AGENT_BIND." >&2
    echo "       Bring up Tailscale, or set SOLADOR_AGENT_BIND explicitly" >&2
    echo "       (e.g. SOLADOR_AGENT_BIND=0.0.0.0 $RERUN_CMD — only behind a firewall)." >&2
    exit 1
fi
echo "==> Binding to $BIND ($BIND_SOURCE)"

# Same precedence as the bind: an explicit SOLADOR_AGENT_PORT, then the port
# the env file already carries, then the default. Reading the existing value
# is what keeps a re-run (and the /opt migration) from resetting a host that
# chose another port back to 7878 — and the health probe below dials the port
# this file says, so a reset would also make verification look at the wrong
# socket.
EXISTING_PORT="$(env_value "$ENV_FILE" SOLADOR_AGENT_PORT)"
PORT="${SOLADOR_AGENT_PORT:-${EXISTING_PORT:-7878}}"

# ---- resolve the release -----------------------------------------------------
if [ -n "${SOLADOR_AGENT_RELEASE:-}" ]; then
    TAG="$SOLADOR_AGENT_RELEASE"
    validate_release_tag "$TAG" || exit 1
    echo "==> Release: $TAG (pinned by SOLADOR_AGENT_RELEASE)"
else
    TAG="$(resolve_latest_release_tag "$RELEASE_REPO_URL")" || exit 1
    echo "==> Release: $TAG (latest published)"
fi
RELEASE_VERSION="${TAG#v}"
ASSET="$(agent_asset_name "$RELEASE_VERSION" "$TRIPLE")"
ASSET_URL="$RELEASE_REPO_URL/releases/download/$TAG/$ASSET"

# ---- stage: download and verify ----------------------------------------------
# A private directory under $HOME rather than /tmp: it is 0700 from birth
# (umask), and a /tmp mounted noexec — common on hardened hosts — would make
# the verified binary's own `--version` fail in a way that reads as "carries
# no version". (The atomic step is the later .new → live rename inside the
# install directory; staging is copied there with `install`, so it need not
# share a filesystem with anything.)
STAGE_ROOT="${XDG_CACHE_HOME:-$HOME/.cache}"
mkdir -p "$STAGE_ROOT"
STAGE="$(umask 077 && mktemp -d "$STAGE_ROOT/${BIN_NAME}-install.XXXXXX")" || {
    echo "ERROR: could not create a staging directory under $STAGE_ROOT." >&2
    exit 1
}
# On every exit path — a rejected signature must not leave the rejected bytes
# lying around any more than a successful install leaves the verified ones.
cleanup_stage() { rm -rf "$STAGE"; }
trap cleanup_stage EXIT

STAGED_BIN="$STAGE/$ASSET"
STAGED_SIG="$STAGE/$ASSET.minisig"

echo "==> Downloading $ASSET"
if ! download_release_asset "$ASSET_URL" "$STAGED_BIN"; then
    echo "ERROR: could not download $ASSET_URL" >&2
    echo "       Release $TAG has no $ASSET, or it is not reachable. A release cut" >&2
    echo "       before #390 (v2026.9.3 and earlier) publishes no agent binaries at" >&2
    echo "       all — this installer does not fall back to building from source." >&2
    echo "       Pin a release that has them:  SOLADOR_AGENT_RELEASE=v<version> $RERUN_CMD" >&2
    exit 1
fi
if ! download_release_asset "$ASSET_URL.minisig" "$STAGED_SIG"; then
    echo "ERROR: could not download $ASSET_URL.minisig" >&2
    echo "       Release $TAG publishes $ASSET without a signature; refusing to install" >&2
    echo "       an unverifiable binary." >&2
    exit 1
fi

echo "==> Verifying the signature under $(basename "$SIGNING_PUBKEY")"
verify_agent_signature "$STAGED_BIN" "$STAGED_SIG" "$SIGNING_PUBKEY" || exit 1

# ONLY NOW is the candidate executable, and only now is it run. A release asset
# carries no unix mode, so this is also the chmod a hand install needs.
chmod 0755 "$STAGED_BIN"
TARGET_VERSION="$(binary_version "$STAGED_BIN")" || exit 1
if [ "$TARGET_VERSION" != "$RELEASE_VERSION" ]; then
    echo "ERROR: the verified binary reports version $TARGET_VERSION but was published under $TAG." >&2
    echo "       The asset does not carry the version its release names; refusing to" >&2
    echo "       install something the health check could never confirm." >&2
    exit 1
fi
echo "==> Verified $ASSET ($TARGET_VERSION)"

# A re-run is the update path, and `/releases/latest` is the one unsigned
# link in the chain (docs/AGENT-DISTRIBUTION.md §6): an intercepting proxy
# could steer it to an older, validly signed release. A FRESH install has
# nothing to compare against; a re-run does — the binary already serving.
# So a re-run refuses to move backwards unless the operator pinned the
# release explicitly, which is the one legitimate reason to.
if [ -x "$DEST_BIN" ] && [ -z "${SOLADOR_AGENT_RELEASE:-}" ]; then
    INSTALLED_VERSION="$(binary_version "$DEST_BIN" 2>/dev/null || true)"
    if [ -n "$INSTALLED_VERSION" ] && calver_newer "$INSTALLED_VERSION" "$TARGET_VERSION"; then
        echo "ERROR: the installed agent is $INSTALLED_VERSION and the latest published release is $TARGET_VERSION." >&2
        echo "       Refusing to install an older version over a newer one on the unpinned path." >&2
        echo "       If that downgrade is what you want, say so:  SOLADOR_AGENT_RELEASE=$TAG $RERUN_CMD" >&2
        exit 1
    fi
fi

# ---- hand over from the pre-rename install -----------------------------------
# Must run BEFORE the service block below, and before the token is resolved.
#
# Without this, installing over an old agent fails in a way that looks like
# something else entirely: the new unit binds the same tailnet address and port,
# gets EADDRINUSE, and — with Restart=always — crash-loops every 3s, while the
# OLD agent keeps answering /v1/health with the OLD token. verify_health then
# reports "did not report version within timeout", naming neither the port
# conflict nor the other unit. Monitoring keeps working throughout, so the
# whole thing looks less broken than it is.
if [ "$OS" = "Linux" ] && [ -f "$LEGACY_UNIT" ]; then
    echo "==> Found a pre-rename install ($LEGACY_BIN_NAME). Handing over."
    # Stop first: it holds the port the new unit is about to want.
    systemctl --user stop "$LEGACY_BIN_NAME" 2>/dev/null || true
    systemctl --user disable "$LEGACY_BIN_NAME" 2>/dev/null || true
    echo "    stopped and disabled $LEGACY_BIN_NAME"
fi

# ---- env file (token) --------------------------------------------------------
mkdir -p "$(dirname "$ENV_FILE")"
EXISTING_TOKEN="$(env_value "$ENV_FILE" SOLADOR_AGENT_TOKEN)"
# Carry the old token across. This is the part that matters: the file name AND
# the key both changed, so the read above finds nothing on an old host and the
# script would silently mint a FRESH token — leaving the cockpit's stored
# per-host credential pointing at nothing, with no signal anywhere that the two
# had diverged. A token the operator never sees changing is a token they cannot
# be asked to re-enter.
if [ -z "$EXISTING_TOKEN" ]; then
    EXISTING_TOKEN="$(env_value "$LEGACY_ENV_FILE" DEVCANOPY_AGENT_TOKEN)"
    [ -n "$EXISTING_TOKEN" ] && echo "==> Carried the bearer token over from $LEGACY_ENV_FILE"
fi

if [ -n "$EXISTING_TOKEN" ]; then
    echo "==> Reusing existing token from $ENV_FILE"
    TOKEN="$EXISTING_TOKEN"
else
    # Generate a strong default the user can accept by pressing Enter.
    GEN_TOKEN="$( (openssl rand -hex 32 2>/dev/null) || head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')"
    # -s: do NOT echo the secret to the terminal (no scrollback/transcript
    # leak). That only holds when stdin IS a terminal: under a bare
    # `ssh host ./deploy/install.sh` the local terminal echoes whatever is
    # typed before it ever reaches this read, so say so — and note that a
    # token piped on stdin (unattended installs) is read as-is.
    if [ ! -t 0 ]; then
        echo "    (stdin is not a terminal: a token typed here cannot be hidden — pipe it in," >&2
        echo "     or press Enter to generate one.)" >&2
    fi
    printf "Enter bearer token [press Enter to generate]: "
    read -rs TOKEN || true
    printf '\n'  # read -s swallows the trailing newline; restore it.
    TOKEN="${TOKEN:-$GEN_TOKEN}"
fi

# Written 0600 from the first byte (umask in a subshell, so the service files
# written after it keep the normal mode), to a sibling and renamed into place:
# truncating the live file first would, on a failure between the truncate and
# the write, leave an empty file that the next run reads as "no token" and
# silently mints a fresh one — the very divergence the handover above exists
# to prevent.
#
# Every line that is not one of the three keys this script owns is carried
# through verbatim. SOLADOR_AGENT_SKIP_FSTYPES and RUST_LOG are documented
# keys an operator adds by hand, and a re-run that dropped them would be an
# upgrade that quietly changed the agent's configuration.
ENV_NEW="$ENV_FILE.new"
(
    umask 077
    {
        printf 'SOLADOR_AGENT_TOKEN=%s\n' "$TOKEN"
        printf 'SOLADOR_AGENT_BIND=%s\n' "$BIND"
        printf 'SOLADOR_AGENT_PORT=%s\n' "$PORT"
        if [ -f "$ENV_FILE" ]; then
            grep -vE '^SOLADOR_AGENT_(TOKEN|BIND|PORT)=' "$ENV_FILE" || true
        fi
    } > "$ENV_NEW"
)
chmod 600 "$ENV_NEW"
mv -f "$ENV_NEW" "$ENV_FILE"
echo "==> Wrote $ENV_FILE (token + bind + port, mode 600; other keys kept)"

# ---- install the binary ------------------------------------------------------
# Staged BESIDE the live path and renamed over it, never copied onto it: Linux
# refuses to overwrite a running executable in place (ETXTBSY), and a rename
# is atomic on both platforms. The displaced binary is kept as .prev — on
# Linux that is exactly the anchor `redeploy.sh rollback` restores.
#
# .prev is the LAST-GOOD anchor, so two cases leave it alone or seed it:
#   * Re-running with the same bytes (the fix-and-retry this script recommends
#     after a failed health check) must not copy the failed binary over the
#     good one .prev still holds.
#   * The /opt migration has no binary at the destination yet; the /opt one is
#     what a rollback would want, so it is copied in as .prev — readable
#     without sudo, which is all this needs.
mkdir -p "$INSTALL_DIR"
NEW_BIN="$DEST_BIN.new"
PREV_BIN="$DEST_BIN.prev"
if [ -e "$DEST_BIN" ]; then
    if cmp -s "$STAGED_BIN" "$DEST_BIN"; then
        echo "==> The installed binary is already these bytes; leaving $PREV_BIN as it is."
    else
        echo "==> Preserving current binary as $PREV_BIN"
        cp -p "$DEST_BIN" "$PREV_BIN"
    fi
elif [ "$MIGRATE_FROM_OPT" = true ] && [ -n "${EXISTING_EXEC:-}" ] && [ -r "$EXISTING_EXEC" ]; then
    echo "==> Preserving the migrated-from binary as $PREV_BIN"
    # Plain cp, not -p: the source is root-owned and the copy must be ours.
    cp "$EXISTING_EXEC" "$PREV_BIN"
    chmod 0755 "$PREV_BIN"
fi
install -m 0755 "$STAGED_BIN" "$NEW_BIN"
mv -f "$NEW_BIN" "$DEST_BIN"
echo "==> Binary installed: $DEST_BIN"

# ---- service -------------------------------------------------------------------
# What every failure from here on says: where things are and what to look at.
# Once the binary is installed a failure is not a clean refusal any more, and
# an exit that leaves the operator with launchd's opaque stderr and nothing
# else is the half-install the preflight exists to prevent.
install_failure_epilogue() {
    case "$OS" in
        Linux)
            echo "       Inspect:  systemctl --user status $BIN_NAME" >&2
            echo "                 journalctl --user -u $BIN_NAME -n 50 --no-pager" >&2
            echo "       Unit ExecStart:   $(existing_exec_start || echo '<unreadable>')" >&2
            ;;
        Darwin)
            echo "       Inspect:  launchctl print gui/$(id -u)/$LAUNCHD_LABEL" >&2
            echo "                 tail -n 50 \"$LOG_FILE\"" >&2
            ;;
    esac
    echo "       Installed binary: $DEST_BIN" >&2
    [ -e "$PREV_BIN" ] && echo "       Previous binary:  $PREV_BIN" >&2
    echo "       The token is already stored in $ENV_FILE (mode 600); re-running this" >&2
    echo "       script reuses it, so a fix-and-retry costs you nothing." >&2
}

# stage_launch_agent_plist <template> <destination> <label> <log file>
# Render a LaunchAgent template with the paths this run chose, lint it, and
# move it into place — the one path both the metrics plist and the updater's
# take, so the two cannot be rendered by different rules. Every placeholder
# either template uses is offered; a template that lacks one is unchanged by
# it. Rendered and linted in staging, then moved: a lint failure must leave
# the previous, valid plist where it was rather than a broken one beside a
# bootout'd service. Asserted out of the file, not assumed from the template:
# a path that broke the XML would otherwise surface as launchctl's
# "Bootstrap failed: 5: Input/output error". 0644 explicitly: launchd refuses
# a group- or world-writable plist in a gui domain, and the operator's umask
# is not ours to assume.
stage_launch_agent_plist() {
    local template="$1" dest="$2" label="$3" log_file="$4" staged
    staged="$STAGE/$(basename "$dest")"
    render_template "$template" \
        "@LABEL@" "$(xml_escape "$label")" \
        "@METRICS_LABEL@" "$(xml_escape "$LAUNCHD_LABEL")" \
        "@LAUNCHER@" "$(xml_escape "$LAUNCHER_DST")" \
        "@BINARY@" "$(xml_escape "$DEST_BIN")" \
        "@ENV_FILE@" "$(xml_escape "$ENV_FILE")" \
        "@LOG_FILE@" "$(xml_escape "$log_file")" \
        "@HOME@" "$(xml_escape "$HOME")" \
        > "$staged"
    if ! plutil -lint -s "$staged"; then
        echo "ERROR: the rendered plist is not valid; $dest was not touched." >&2
        return 1
    fi
    chmod 0644 "$staged"
    mv -f "$staged" "$dest"
}

# bootstrap_launch_agent <label> <plist>
# Load a rendered plist into this user's gui domain, replacing a loaded copy.
# bootout + bootstrap rather than kickstart: kickstart restarts the process
# but keeps the plist launchd already parsed, so a changed path would not
# take effect until the next login. On failure, launchd's own words are
# printed and the caller says what state that leaves.
bootstrap_launch_agent() {
    local label="$1" plist="$2" service disabled_list label_re booted err
    service="gui/$(id -u)/$label"
    if launchctl print "$service" >/dev/null 2>&1; then
        echo "==> Stopping the running $label"
        launchctl bootout "$service" 2>/dev/null || true
    fi
    # A service once disabled (the legacy `launchctl unload -w` idiom leaves
    # that flag behind) fails every bootstrap with "Service is disabled".
    # Cleared only when launchd actually reports it: an unconditional
    # `enable` writes a permanent override record for a label that never had
    # one. `=> disabled` since Ventura; Big Sur and Monterey — inside the
    # agent's 11.0 floor — print `=> true` for the same state.
    #
    # Captured first, then grepped from a here-string — NOT piped straight
    # into `grep -q`: under `set -o pipefail`, grep -q exits on the matching
    # line, the producer's next write takes SIGPIPE, and the pipeline reads
    # as "not disabled" — a real race with launchctl's multi-KB output and a
    # measured 1-in-5 flake in the suite. The label's dots are escaped so
    # `app-solador-agent` cannot satisfy a pattern meant for
    # `app.solador.agent`.
    disabled_list="$(launchctl print-disabled "gui/$(id -u)" 2>/dev/null || true)"
    label_re="${label//./\\.}"
    if grep -qE "\"$label_re\" => (disabled|true)" <<< "$disabled_list"; then
        echo "==> $label was disabled in launchd; re-enabling it"
        launchctl enable "$service"
    fi
    # bootout returns before the service is fully torn down on some releases,
    # and a bootstrap that races it fails with "service already loaded"; a
    # few retries cover that. Each attempt's stderr is kept so the failure
    # can say what launchd said, without a further attempt whose success
    # would then be reported as failure.
    booted=false
    err="$STAGE/bootstrap.err"
    for _ in 1 2 3 4 5; do
        if launchctl bootstrap "gui/$(id -u)" "$plist" 2>"$err"; then
            booted=true
            break
        fi
        sleep 1
    done
    if [ "$booted" != true ]; then
        echo "ERROR: launchctl bootstrap gui/$(id -u) $plist failed:" >&2
        sed 's/^/       /' "$err" >&2
        return 1
    fi
    echo "==> Bootstrapped $service"
}

case "$OS" in
    Linux)
        mkdir -p "$(dirname "$UNIT_DST")"
        EXEC_START="$(systemd_exec_path "$DEST_BIN")" || exit 1
        # The unit is REGENERATED from the template, not edited: an operator's
        # own edits to the file do not survive (drop-ins via `systemctl --user
        # edit` do). The displaced file is kept beside it so nothing is lost.
        if [ -f "$UNIT_DST" ]; then
            cp -p "$UNIT_DST" "$UNIT_DST.prev"
        fi
        render_template "$UNIT_SRC" "@SOLADOR_AGENT_BIN@" "$EXEC_START" > "$UNIT_DST.new"
        mv -f "$UNIT_DST.new" "$UNIT_DST"
        systemctl --user daemon-reload
        systemctl --user enable "$BIN_NAME"
        # `restart` (not `enable --now`) so a re-run actually picks up the new
        # binary — `--now` only starts a stopped unit, it won't restart a
        # running one.
        systemctl --user restart "$BIN_NAME"

        # The old binary, env file and unit file are left on disk on purpose:
        # they cost nothing, and they are the rollback path if the new agent
        # does not come up. Remove them by hand once you are satisfied.

        # Survive logout / start on boot. `$USER` is not guaranteed under a
        # bare `ssh host cmd`, and `set -u` would take the install down over
        # a best-effort step.
        LINGER_USER="${USER:-$(id -un)}"
        if command -v loginctl >/dev/null 2>&1; then
            loginctl enable-linger "$LINGER_USER" 2>/dev/null || \
                echo "    (could not enable-linger; run 'sudo loginctl enable-linger $LINGER_USER' for boot start)"
        fi
        ;;
    Darwin)
        mkdir -p "$(dirname "$PLIST_DST")" "$(dirname "$LOG_FILE")"
        # The launcher is copied out of the checkout, so deleting the clone
        # later does not stop the agent.
        install -m 0755 "$LAUNCHER_SRC" "$LAUNCHER_DST"
        if ! stage_launch_agent_plist "$PLIST_SRC" "$PLIST_DST" "$LAUNCHD_LABEL" "$LOG_FILE"; then
            install_failure_epilogue
            exit 1
        fi
        if ! bootstrap_launch_agent "$LAUNCHD_LABEL" "$PLIST_DST"; then
            echo "       The service is not loaded; the binary and plist are in place." >&2
            install_failure_epilogue
            exit 1
        fi
        ;;
esac

# ---- post-install verification -------------------------------------------------
# A running service only proves *a* binary is up. Assert the version being
# served is the version just verified and installed, so a stale binary can't
# pass for a successful install. The probe dials the bind address written
# above, so it works on a tailnet-only agent without hardcoding loopback.
if ! verify_health "$ENV_FILE" "$TARGET_VERSION" "$LAUNCHD_LABEL"; then
    echo >&2
    echo "ERROR: install verification FAILED — not treating this as a successful install." >&2
    install_failure_epilogue
    exit 1
fi

echo
echo "==> Done: $BIN_NAME $TARGET_VERSION installed and serving."
case "$OS" in
    Linux)
        systemctl --user --no-pager status "$BIN_NAME" || true
        ;;
    Darwin)
        echo "    Service: gui/$(id -u)/$LAUNCHD_LABEL ($PLIST_DST)"
        echo "    Log:     $LOG_FILE"
        echo "    It starts at this user's login; it does not run before anyone logs in."
        ;;
esac

# ---- the unattended update job (#394) ------------------------------------------
# After the metrics service is verified AND reported, never before: a job
# that updates a service this run could not bring up would be consent
# recorded against a broken install, and a caller scripting this must be
# able to read "the agent is installed and serving" above any failure below.
# That is also why a failed opt-in exits OPT_IN_FAILED_EXIT rather than 1: 1
# is "the install failed", and this is not that.
#
# Off by default. The no-flag run touches nothing about the updater: an
# earlier opt-in is left exactly as it is (the files, the enablement, the
# timer's phase), and it is revoked by the documented disable/remove
# commands, or by --uninstall — never by a plain re-run. The summary line
# at the end reports what the service manager says, not what files exist,
# so a paused job (the documented `disable --now` / `bootout`, which leave
# the files) is not reported as scheduled.
#
# The first firing is a day away on both platforms: systemd's OnActiveSec
# starts counting when the timer starts, launchd's StartInterval when the
# job loads, and neither is asked to run the service now. No `systemctl
# start` of the oneshot, no `kickstart`, no network check on enable.
OPT_IN_FAILED_EXIT=3

update_failure_epilogue() {
    echo "       The metrics service is installed and serving $TARGET_VERSION; only the" >&2
    echo "       unattended update job failed (exit $OPT_IN_FAILED_EXIT). Re-run with" >&2
    echo "       --enable-timer once fixed. Installed binary: $DEST_BIN" >&2
}

# The Linux opt-in as one function, so any step failing lands in the caller's
# `if !` and reaches the epilogue — under `set -e` a bare `mv` or
# `daemon-reload` failing would otherwise end the run with raw stderr and
# no statement that the metrics service is fine.
install_update_timer_linux() {
    # The guard FIRST, and the oneshot only once it is in place (#411). A
    # unit whose ExecCondition= names a binary that is not there skips every
    # firing as exec failure 203 — inside ExecCondition's skip range — with
    # Result=exec-condition and nothing in `--failed` (observed on systemd 256), so
    # this order is what keeps a run interrupted between the two writes from
    # leaving a green, silent job; the unit's AssertFileIsExecutable= on the
    # same path is the second half of that. Copied out of the checkout like
    # the macOS launcher, so deleting the clone later does not stop the job;
    # 0755 explicitly, and asserted back rather than assumed from `install`.
    # Staged beside the live path and renamed over it, like the binary and
    # the units: a firing that lands mid-copy runs the old guard or the new
    # one, never a half-written file (which would be exec failure 203 — a
    # skip, per the comment above).
    rm -f "$GUARD_DST.new"
    install -m 0755 "$GUARD_SRC" "$GUARD_DST.new" || return 1
    mv -f "$GUARD_DST.new" "$GUARD_DST" || { rm -f "$GUARD_DST.new"; return 1; }
    if [ ! -x "$GUARD_DST" ]; then
        echo "ERROR: $GUARD_DST is not executable after install." >&2
        return 1
    fi
    local exec_guard
    exec_guard="$(systemd_exec_path "$GUARD_DST")" || return 1
    # The oneshot carries the same rendered ExecStart as the metrics unit
    # plus the word `update`, the guard on its ExecCondition= (Exec-quoted)
    # and on its AssertFileIsExecutable= (a bare path; Assert lines take no
    # quoting, and check_install_path has already refused every character
    # that would matter); the timer has nothing to render and is copied as
    # it is. All regenerated on every opt-in run: drop-ins survive, edits do
    # not. (No .prev is kept for these — unlike the metrics unit they carry
    # nothing an operator wrote.)
    render_template "$UPDATE_UNIT_SRC" \
        "@SOLADOR_AGENT_BIN@" "$EXEC_START" \
        "@SOLADOR_AGENT_GUARD@" "$exec_guard" \
        "@SOLADOR_AGENT_GUARD_PATH@" "$GUARD_DST" > "$UPDATE_UNIT_DST.new" || return 1
    mv -f "$UPDATE_UNIT_DST.new" "$UPDATE_UNIT_DST" || return 1
    cp "$UPDATE_TIMER_SRC" "$UPDATE_TIMER_DST.new" || return 1
    mv -f "$UPDATE_TIMER_DST.new" "$UPDATE_TIMER_DST" || return 1
    if ! systemctl --user daemon-reload; then
        echo "ERROR: systemctl --user daemon-reload failed after writing $UPDATE_NAME.{service,timer}." >&2
        return 1
    fi
    # `enable --now` on the TIMER, and deliberately not `restart`. A timer
    # that has already fired keeps its OnUnitActiveSec schedule (a repeated
    # opt-in is not a reason to check sooner); one that has not yet fired
    # is re-based by the daemon-reload above to now — a first check LATER
    # than it would have been, never sooner, and never one on enable. A
    # stopped timer starts counting a fresh day from here. The oneshot
    # itself is never started by this script.
    if ! systemctl --user enable --now "$UPDATE_NAME.timer"; then
        echo "ERROR: could not enable $UPDATE_NAME.timer." >&2
        echo "       Inspect:  systemctl --user status $UPDATE_NAME.timer" >&2
        return 1
    fi
}

install_update_agent_macos() {
    mkdir -p "$(dirname "$UPDATE_PLIST_DST")" "$(dirname "$UPDATE_LOG_FILE")" || return 1
    stage_launch_agent_plist "$UPDATE_PLIST_SRC" "$UPDATE_PLIST_DST" "$UPDATE_LABEL" "$UPDATE_LOG_FILE" || return 1
    # A repeated opt-in boots the loaded job out and in again, which
    # restarts its 24 h interval — an update is a day away either way, and
    # a re-rendered plist must be the one launchd holds.
    if ! bootstrap_launch_agent "$UPDATE_LABEL" "$UPDATE_PLIST_DST"; then
        echo "       The update job is not loaded; its plist is in place." >&2
        return 1
    fi
}

if [ "$ENABLE_TIMER" = true ]; then
    case "$OS" in
        Linux)
            if ! install_update_timer_linux; then
                update_failure_epilogue
                exit "$OPT_IN_FAILED_EXIT"
            fi
            echo "==> Unattended updates: enabled ($UPDATE_NAME.timer, daily, no catch-up, guarded by $GUARD_DST; first check in 24h)"
            ;;
        Darwin)
            if ! install_update_agent_macos; then
                update_failure_epilogue
                exit "$OPT_IN_FAILED_EXIT"
            fi
            echo "==> Unattended updates: enabled (gui/$(id -u)/$UPDATE_LABEL, daily, no catch-up; first check in 24h)"
            ;;
    esac
fi

# What the manager says about the job, in three states — enabled, present
# but not scheduled, off — read-only, and the same line on every run.
update_scheduling_summary() {
    local state
    case "$OS" in
        Linux)
            if [ ! -f "$UPDATE_TIMER_DST" ]; then
                echo "    Unattended updates: off (opt in with: $RERUN_CMD --enable-timer)"
                return
            fi
            state="$(systemctl --user is-enabled "$UPDATE_NAME.timer" 2>/dev/null || true)"
            if [ "$state" = "enabled" ]; then
                # A host that opted in before #411 keeps a oneshot with no
                # ExecCondition= — the no-flag re-run leaves it exactly as
                # it is — and it must not read the same as a guarded one:
                # "enabled" is not "guarded", and the re-run with the flag
                # is how the guard arrives.
                if grep -q '^ExecCondition=' "$UPDATE_UNIT_DST" 2>/dev/null; then
                    if [ -x "$GUARD_DST" ]; then
                        echo "    Unattended updates: enabled, guarded by $GUARD_DST ($UPDATE_NAME.timer; systemctl --user list-timers $UPDATE_NAME.timer)"
                    else
                        # The opposite of unguarded: the unit names a guard
                        # that is not there, so its AssertFileIsExecutable=
                        # fails every start and NO check runs, at wake or
                        # otherwise.
                        echo "    Unattended updates: enabled but its guard $GUARD_DST is MISSING ($UPDATE_NAME.timer);"
                        echo "      every start fails the unit's assertion and no check runs — re-run with --enable-timer to reinstall the guard"
                    fi
                else
                    echo "    Unattended updates: enabled but UNGUARDED ($UPDATE_NAME.timer; a pre-#411 unit with no ExecCondition=);"
                    echo "      a firing at wake is not discarded — re-run with --enable-timer to install the guard"
                fi
            else
                echo "    Unattended updates: $UPDATE_NAME.timer is present but not enabled (systemd says '${state:-<nothing>}');"
                echo "      re-enable with:  systemctl --user enable --now $UPDATE_NAME.timer   (or re-run with --enable-timer)"
            fi
            ;;
        Darwin)
            if [ ! -f "$UPDATE_PLIST_DST" ]; then
                echo "    Unattended updates: off (opt in with: $RERUN_CMD --enable-timer)"
                return
            fi
            if launchctl print "gui/$(id -u)/$UPDATE_LABEL" >/dev/null 2>&1; then
                echo "    Unattended updates: loaded (gui/$(id -u)/$UPDATE_LABEL; log $UPDATE_LOG_FILE)"
            else
                echo "    Unattended updates: $UPDATE_PLIST_DST is present but not loaded (a bootout, or launchd disabled it);"
                echo "      it reloads at the next login, or re-run with --enable-timer to load it now"
            fi
            ;;
    esac
}
update_scheduling_summary
echo
echo "Verify locally (the token reaches curl on stdin, not its argv, and the env"
echo "file is read, never sourced — its contents are yours to type and not shell):"
echo "  printf 'header = \"Authorization: Bearer %s\"\\n' \"\$(grep '^SOLADOR_AGENT_TOKEN=' \"$ENV_FILE\" | cut -d= -f2- | sed -e 's/\\\\/\\\\\\\\/g' -e 's/\"/\\\\\"/g')\" | curl -sS -K - \"$(health_url "$BIND" "$PORT")\""
echo
echo "Bearer token (give this to Solador): stored in $ENV_FILE (mode 600)."
echo "  Last 4 chars: ...${TOKEN: -4}   — read the full value with:"
echo "  grep '^SOLADOR_AGENT_TOKEN=' \"$ENV_FILE\" | cut -d= -f2-"
echo "To rotate it: edit that file, then restart the service."
