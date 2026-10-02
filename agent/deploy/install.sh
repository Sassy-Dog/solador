#!/usr/bin/env bash
#
# Install the Solador metrics agent from a signed release binary (#392).
#
# Usage:
#   ./deploy/install.sh                     # download, verify, install, start, verify
#   ./deploy/install.sh --enable-timer      # ...and opt in to a daily unattended update check
#   ./deploy/install.sh --enable-tls        # ...and turn SOLADOR_AGENT_TLS on for an EXISTING install
#   ./deploy/install.sh --migrate-from-opt  # re-point an existing /opt install (Linux)
#   ./deploy/install.sh --uninstall         # remove THIS USER's install (env file, TLS key/cert kept)
#   ./deploy/install.sh --uninstall --purge # ...and delete the env file + TLS key/cert too
#   ./deploy/install.sh --help
#
# Environment:
#   SOLADOR_AGENT_RELEASE=agent-vYYYY.M.N   install this release instead of the
#                                     latest; also accepts a legacy vYYYY.M.N tag
#                                     (a release cut before the agent had its own
#                                     train) that carries agent assets
#   SOLADOR_AGENT_BIND / _PORT        as documented in agent/README.md
#   SOLADOR_AGENT_TLS=0|1             pin the TLS opt-in (#447) outright, overriding both
#                                     the fresh-install default and --enable-tls; as
#                                     documented in agent/README.md's TLS section
#
# What it does:
#   1. Detects the platform and maps it onto one of the four published
#      targets (x86_64 / aarch64, Linux musl / macOS). Anything else refuses.
#   2. Resolves the release — by default from the signed feed
#      `agent-latest.json` on the permanent `agent-latest` release (#472;
#      `/releases/latest` is the COCKPIT's newest release and carries no agent
#      assets), or SOLADOR_AGENT_RELEASE — and downloads that release's raw
#      binary (`agent-v<version>`) plus its detached .minisig into a private
#      staging directory. No Rust toolchain is involved. The feed is verified
#      under the same key and the same stock `minisign` BEFORE its `version` is
#      read (#490): a tampered feed is rejected unread, and a `version` that is
#      not a strict CalVer is refused. A re-run never moves an installed RELEASE
#      backwards on this path (a replayed, older feed is validly signed); an
#      installed `+dev` source build is not a CalVer and is deliberately exempt —
#      this script is how such a host moves onto a published release.
#   3. Verifies the binary's signature with the stock `minisign` under the
#      public key shipped with this checkout, agent/release-signing-key.pub.
#      Only a verified binary is ever made executable or asked its `--version`.
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
# default install (or an opted-in --enable-timer) put on disk. On Linux,
# where a unit's own FILE still exists, it stops it then disables it (and
# the pre-rename unit, if a handed-over host still has one). Stop and
# disable are two separate systemctl calls: systemd's `disable` needs the
# unit's FILE and fails without one before it ever reaches a `--now` stop, so
# a combined `disable --now` on a unit whose file is already gone would never
# stop anything. Two calls also keep a stop failure and a disable failure
# from being reported as the same claim. On
# macOS, gated on `launchctl print` rather than a file check, it stops the
# service with `launchctl bootout`; nothing is disabled — removing the plist
# is what disables it, and the only override install writes is a
# `launchctl enable`, which is inert once the plist is gone. Either way this
# removes both unit/plist pairs, the binary and its
# .prev/.new/.update.lock/.rollback-displaced siblings, the macOS launcher,
# the Linux guard, and the update stamp. The env file (the token) is KEPT and
# named in the output unless --purge is also given, which also removes the
# pre-rename devcanopy-agent.env (install copied its token out of that file
# and never deleted it). The TLS key/certificate (#447, solador-agent.tls.key
# / solador-agent.tls.crt beside the env file, namespaced the same way the
# env file itself is) follow the SAME rule: KEPT — a re-install as this
# user reuses them, so every cockpit that has pinned the fingerprint keeps
# working — unless --purge is given, which removes them too (re-pairing is
# then a fresh certificate on the next start). A binary an existing unit/plist names OUTSIDE
# ~/.local/bin — an unmigrated /opt host, most likely — is named as "left
# behind" together with its actual remedy (this user cannot delete it; its
# owner can) rather than silently ignored or pointed at a flag that does not
# apply post-uninstall.
#
# RE-RUN AFTER EXIT 4 (#463, part of #455): on Linux a unit counts as present
# when its unit FILE exists OR the user manager still holds it (see
# linux_unit_present), so a re-run after an exit 4 — whose first run already
# deleted the files — still finds and stops a unit the manager is running,
# rather than reporting "Nothing installed". Only `stop` is gated on the
# manager's word; `disable` needs the file. (macOS has always asked launchd
# directly, via `launchctl print gui/$(id -u)/<label>`.)
#
# Refusals run in this order and each is untouched:
# root (same reason --enable-timer refuses it); then an unsupported platform
# (this script supports Linux/systemd and macOS/launchd, named as such);
# then the service manager being unreachable (same reachability check the
# install path makes — a sudo -u/su session, say); then the update
# transaction lock (<bin>.update.lock) — this whole section is skipped
# outright when ~/.local/bin ($DEST_BIN's own directory) does not exist at
# all, since nothing can possibly be installed under a directory that is
# not there. Where it does exist, this OPENS the lock file (creating it if missing,
# NEVER truncating one that exists — agent/src/update.rs's own
# create(true).truncate(false)) and takes a non-blocking exclusive flock on
# it — flock(1) where it is on PATH, else the stock perl's Fcntl flock on
# that same already-open file descriptor. Where NEITHER tool is on PATH there
# is no way left to ask the kernel whether a transaction is running, and
# guessing "free" is the wrong direction, so this refuses (busy) before
# anything changes, the same as every other preflight refusal. Once held, a
# note (pid=<pid> since=<epoch>, the same shape agent/src/update.rs writes)
# is written into the file so a racing update/rollback's own busy message
# names THIS uninstall rather than a stale previous holder.
#
# LOCK LIFETIME: the hold starts before the first stop/bootout, so a
# transaction starting while this run stops the service and removes its
# unit/plist meets it as busy (exit 75, its own code). It is released
# (`exec 9>&-`) after the binary, its siblings and the lock file itself are
# removed, and BEFORE the update stamp and, with --purge, the env-file,
# legacy-env and TLS removals. The lock file is therefore unlinked while
# still held, which agent/src/update.rs warns is how two processes can come
# to hold "the" lock at once (flock() locks the open file description, not
# the path). Nothing runs between the unlink and the release, and on a clean
# run the unit/plist and the binary are already gone, so a transaction
# starting afterwards has no install to resolve and nothing to swap. On an
# exit-6 run one of them may remain; the work after the release (stamp,
# --purge removals) is then unguarded. A busy refusal, or a perl reopen failure,
# never deletes the file. On a refusal path only a failed open deletes one,
# and only when this run created it.
#
# EXIT STATUS of --uninstall (canonical: every other document points here):
#   0  uninstalled, or already clean (a second run is a no-op). Earned only
#      when every service-manager call the reachability check let through
#      succeeded and every file that should be gone is.
#   1  refused: root, an unsupported platform, the manager unreachable, the
#      update lock held or uncheckable (neither flock(1) nor perl, the lock
#      file could not be opened, or perl could not reopen fd 9), a hostile
#      SOLADOR_AGENT_LAUNCHD_LABEL. Nothing is changed, except that an empty
#      lock file this run created may remain after a perl reopen failure.
#   2  usage: an unknown argument, --purge without --uninstall, or
#      --uninstall combined with --migrate-from-opt or --enable-timer.
#   4  the uninstall could not confirm the service is gone: a reachable
#      manager refused a specific stop or disable request, or a unit's state
#      could not be read. Every file this run found was still removed
#      (best-effort); the message says so only when some file or a held unit
#      was removed, and says none were on an otherwise clean host whose
#      state read failed.
#      Verify by hand. The message names which: a failed stop may mean the
#      process is still running; a failed disable alone does not; an
#      unreadable state means no stop or disable request was made for that
#      unit at all.
#   6  a file that should have been removable could not be deleted (a
#      read-only parent directory, an immutable file, or similar). The
#      service and other files may already be gone; fix it and re-run. 6 wins
#      over 4 when both apply, since "a file provably could not be removed"
#      is the more severe claim.
# (Exit 75 is not this script's: it is what a racing `update`/`rollback`
# reports when it meets this run's lock.)
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
# The self-signed keypair SOLADOR_AGENT_TLS=1 serves (#447), generated once
# by the agent itself on its first start — never by this script — and kept
# for the host's lifetime beside the env file. Namespaced the same way the
# env file itself is (${BIN_NAME}.tls.key / ${BIN_NAME}.tls.crt), not bare
# tls.key/tls.crt in an XDG root every app shares. Named here only so
# --uninstall can KEEP them (like the env file) or, with --purge, remove
# them; this script never reads or writes their contents.
TLS_KEY_FILE="$HOME/.config/${BIN_NAME}.tls.key"
TLS_CERT_FILE="$HOME/.config/${BIN_NAME}.tls.crt"
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
# --enable-tls (#447): explicit consent to flip an EXISTING env file's
# SOLADOR_AGENT_TLS choice to on. A fresh install (no env file yet) writes
# SOLADOR_AGENT_TLS=1 regardless of this flag — see the env-file section
# below — so this only ever matters on a re-run.
ENABLE_TLS=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --migrate-from-opt) MIGRATE_FROM_OPT=true ;;
        --enable-timer) ENABLE_TIMER=true ;;
        --uninstall) UNINSTALL=true ;;
        --purge) PURGE=true ;;
        --enable-tls) ENABLE_TLS=true ;;
        -h | --help | help)
            usage
            exit 0
            ;;
        *)
            echo "ERROR: unknown argument '$1'. Use: [--enable-timer] [--enable-tls] [--migrate-from-opt] [--uninstall [--purge]] [--help]" >&2
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
# Set the moment any `rm -f` below fails on a path that still exists
# afterwards — a read-only parent directory (`chmod 555`) or an immutable
# file (`chflags uchg` on macOS) both leave `rm -f` reporting failure rather
# than quietly doing nothing, so the outcome is checked rather than assumed:
# a failed removal must never be reported as a successful one (a "removed:"
# line, "==> Done", exit 0). Read at the end, same as
# MANAGER_STOP_FAILED/MANAGER_DISABLE_FAILED below: it is a DIFFERENT, more
# severe case (a file that should be gone is provably still there, not
# merely unconfirmed), so it gets its own exit status rather than folding
# into 4's.
UNINSTALL_FAILED=false

# LINUX_UNIT_HINTS: a space-separated list of "<unit>=<kind>" records, one
# per systemd unit whose `stop`, `disable` or state read failed during THIS
# run's own pass (Linux only — see stop_and_disable_linux_unit, below,
# which appends to it; <kind> is "stop", "disable", "both" or "unread"). Read only by
# run_uninstall's own Linux exit-4 hint, further down, to name exactly which
# `systemctl --user status <unit>` calls are worth the operator's time — by
# then this run has already removed the unit file and run `daemon-reload`,
# so a blanket hint naming a fixed unit (what `service_inspect_hint` in
# lib.sh does, unchanged, for redeploy.sh's own use) answers "No files
# found" whether or not the process is still running, and names only the
# metrics unit regardless of which one actually failed.
#
# A plain string, not an array: bash 3.2 (the macOS system /bin/bash this
# repo's own tests run under, and `set -u`) makes bash arrays a poor
# foundation here, and no systemd unit name can ever contain a space, so
# splitting this back apart on whitespace is safe. A script-global, like
# UNINSTALL_REMOVED/UNINSTALL_FAILED above — not a `local` — for the same
# reason: stop_and_disable_linux_unit is a function this one calls, visible
# to it under bash's own dynamic scoping.
LINUX_UNIT_HINTS=""

# uninstall_remove <path> <description>: rm -f, and note the OUTCOME — not
# just the attempt — in the summary printed to the operator and in
# UNINSTALL_REMOVED/UNINSTALL_FAILED. `rm -f` suppresses "no such file", but
# still fails (and still prints its own reason to stderr, ahead of the line
# below) on a permission or immutability error, and that failure must never
# be read as success: a second `-e`/`-L` check after the `rm -f` call is what
# tells "removed" apart from "rm said it worked but the path is somehow still
# there" (belt-and-braces; `rm -f`'s own exit status already catches every
# case observed in practice).
uninstall_remove() {
    local path="$1" desc="$2"
    if [ -e "$path" ] || [ -L "$path" ]; then
        if rm -f "$path" 9>&- && ! { [ -e "$path" ] || [ -L "$path" ]; }; then
            UNINSTALL_REMOVED=true
            echo "    removed: $desc ($path)"
        else
            echo "ERROR: FAILED to remove: $desc ($path)" >&2
            UNINSTALL_FAILED=true
        fi
    fi
}

# unowned_service_binary: the executable path a PRE-EXISTING unit/plist
# names, read BEFORE --uninstall removes that unit/plist — never assumed to
# be $DEST_BIN. An unmigrated /opt host's unit still names
# /opt/solador-agent/solador-agent (root-owned; #392's migration is
# explicit, never automatic on its own), and --uninstall deleting that unit
# must not go quiet about the binary it named: the mirror-of-install claims
# it removed everything IT put there, and a root-owned binary this run never
# touched is not that. Prints nothing when there is no existing unit/plist,
# it cannot be read, or it already names $DEST_BIN. Read-only.
unowned_service_binary() {
    local path=""
    case "$OS" in
        Linux)
            [ -f "$UNIT_DST" ] || return 0
            path="$(awk '/^ExecStart=/ { sub(/^ExecStart=/, ""); print; exit }' "$UNIT_DST" 9>&- \
                | sed -e 's/^"//' -e 's/"$//' 9>&-)"
            ;;
        Darwin)
            [ -f "$PLIST_DST" ] || return 0
            # ProgramArguments is [launcher, binary, env file, log file]
            # (stage_launch_agent_plist, below) — the SECOND <string>, never
            # the first (the launcher, resolved and removed on its own path
            # as $LAUNCHER_DST). A plain scan, not `plutil -extract … raw`
            # (macOS 12+ only, and it is the OPERATOR's shell this runs
            # under, not the agent's 11.0 floor): every path this plist can
            # hold already passed check_install_path (no ", \, $, % or
            # control character), so the handful of entities xml_escape can
            # produce are the only ones ever to undo.
            path="$(awk '
                /<key>ProgramArguments<\/key>/ { want = 1; next }
                want && /<string>/ {
                    n++
                    if (n == 2) {
                        line = $0
                        sub(/^[ \t]*<string>/, "", line)
                        sub(/<\/string>[ \t]*$/, "", line)
                        print line
                        exit
                    }
                    next
                }
                want && /<\/array>/ { exit }
            ' "$PLIST_DST" 9>&-)"
            path="$(printf '%s' "$path" | sed -e 's/&lt;/</g' -e 's/&gt;/>/g' -e "s/&apos;/'/g" -e 's/&quot;/"/g' -e 's/&amp;/\&/g' 9>&-)"
            ;;
    esac
    if [ -n "$path" ] && [ "$path" != "$DEST_BIN" ]; then
        printf '%s\n' "$path"
    fi
    # Explicit, never the `&&` chain's own status: the common case (a unit
    # that already names $DEST_BIN, or none at all) makes the condition
    # above false, and this function's job is read-only reporting, not a
    # pass/fail signal — a caller under `set -e` must survive it either way.
    return 0
}

# left_behind_hint <path>: the actual remedy for a binary this run found
# named by an existing unit/plist but never installed and cannot delete —
# never "see --migrate-from-opt": post-uninstall, that flag either
# reinstalls fresh on Linux or is refused outright on macOS (see the
# --uninstall/--migrate-from-opt usage check above), and neither touches a
# leftover binary, so it is not a remedy.
# The /opt layout this repo's own migration path creates is named
# explicitly, with the same `sudo rm -rf /opt/solador-agent` the migration
# step's own error text already suggests (existing_exec_start's caller,
# below); anything else just names the path and says whose job it is.
left_behind_hint() {
    local path="$1"
    case "$path" in
        /opt/solador-agent/*)
            echo "    left behind: $path"
            echo "                 this user cannot delete it; its owner can, e.g.:"
            echo "                   sudo rm -rf /opt/solador-agent"
            ;;
        *)
            echo "    left behind: $path"
            echo "                 this user cannot delete it; remove it as its owner."
            ;;
    esac
}

# linux_unit_present <full-unit-name>: does the running user manager still
# hold this unit? Exit 0 present, 1 absent, 2 the state could not be read.
# Written against five systemd behaviours (#463; source citations are
# v256):
#  - the name is always the FULL name: `list-units` does not append
#    `.service` (systemctl-list-units.c:275), so a bare `solador-agent`
#    matches nothing;
#  - the state is read with `show -p LoadState -p ActiveState <unit>`, never
#    by parsing `list-units` columns, and each property is read by KEY, so
#    the order systemd prints them in cannot matter;
#  - present means `LoadState != not-found` OR `ActiveState != inactive`.
#    `list-units --all` also lists fileless units something merely
#    references (a dangling *.wants link, this repo's own
#    `After=solador-agent.service` in the update oneshot, an operator's
#    unit); those read `not-found` + `inactive`, and `stop` on one fails
#    "Unit ... not loaded." (dbus-unit.c:1811-1814), which systemctl reports
#    as exit 5 — counting them present would make a clean host exit 4
#    forever;
#  - a fileless oneshot left `failed` reads `not-found` + `failed`: present,
#    and `stop` works on it;
#  - a unit the manager never loaded is not an error: `show` exits 0 with
#    `LoadState=not-found`, `ActiveState=inactive` — absent.
# A `show` that fails, or prints neither property, is exit 2: not evidence
# of absence, so the caller says so instead of reporting "Nothing installed".
linux_unit_present() {
    local unit="$1" props line load="" active=""
    if ! props="$(systemctl --user show -p LoadState -p ActiveState "$unit" 9>&- 2>/dev/null)"; then
        return 2
    fi
    while IFS= read -r line; do
        case "$line" in
            LoadState=*) load="${line#LoadState=}" ;;
            ActiveState=*) active="${line#ActiveState=}" ;;
        esac
    done <<EOF_PROPS
$props
EOF_PROPS
    if [ -z "$load" ] || [ -z "$active" ]; then
        return 2
    fi
    if [ "$load" != "not-found" ] || [ "$active" != "inactive" ]; then
        return 0
    fi
    return 1
}

# stop_and_disable_linux_unit <full-unit-name> <file> <label>: stop and
# disable are two separate systemctl calls, never a combined `disable --now`.
# Real systemd's `do_unit_file_disable`
# (`src/shared/install.c`) returns -ENOENT for a unit file that is not
# there, and `disable`'s own CLI path (`systemctl-enable.c`) fails at
# "Failed to %s unit" BEFORE it ever reaches the `--now` stop — so a
# combined call on a unit whose file is already gone never stops anything,
# it just fails outright. Splitting the two calls also means a stop failure
# and a disable failure are never reported as the same claim
# (MANAGER_STOP_FAILED vs MANAGER_DISABLE_FAILED, below): only a failed
# `stop` means the process itself may still be running.
#
# The two calls are gated differently (#463, part of #455):
#  - `stop` runs when the unit's FILE exists OR the user manager still holds
#    the unit (linux_unit_present, above). The second half is what lets a
#    re-run converge after an exit 4: that run's best-effort removal has
#    already deleted the file, and the manager may still be running the unit.
#  - `disable` runs only while the FILE exists, because without one real
#    systemd fails it ENOENT. A fileless unit's enablement symlinks are not
#    something `disable` can find; `daemon-reload` and `reset-failed` below
#    are what clear the rest.
# The file-exists path never asks the manager, so a normal uninstall makes
# exactly the calls it always did.
#
# A failed `stop` sets MANAGER_STOP_FAILED; a failed `disable` (it only ever
# runs once the file is confirmed to exist) sets MANAGER_DISABLE_FAILED.
# Either means exit 4, as does a state that could not be read, which sets
# MANAGER_STATE_UNREAD and a `<unit>=unread` hint instead (#475): no request
# was made, so it is not a refused stop. Sets
# UNINSTALL_REMOVED whenever the unit's file exists or the manager held it.
# UNINSTALL_REMOVED is a script-global, set after the argument-parsing loop
# in the `---- uninstall (#439) ----` section above — it is not a `local`,
# unlike MANAGER_STOP_FAILED/MANAGER_DISABLE_FAILED, which ARE
# run_uninstall's own locals; all three are visible here, unshadowed, under
# bash's dynamic scoping (verified: a `local` in a calling function is
# visible to a function it calls).
#
# Either failure also appends "<unit>=<kind>" to LINUX_UNIT_HINTS (another
# script-global, declared beside UNINSTALL_REMOVED above), so the exit-4
# path below can name exactly which unit(s) to inspect by hand, and how —
# by the time it runs, this same unit's FILE is already gone and
# `daemon-reload` has already run, so a generic `systemctl --user cat
# <fixed-unit>` hint answers "No files found" regardless of whether the
# process is still running, and never names anything but the metrics unit.
stop_and_disable_linux_unit() {
    local unit="$1" file="$2" label="$3"
    local this_stop_failed=false this_disable_failed=false this_state_unread=false
    local has_file=false held=false present_rc=1
    if [ -f "$file" ]; then
        has_file=true
    else
        linux_unit_present "$unit" && present_rc=0 || present_rc=$?
        case "$present_rc" in
            0) held=true ;;
            2)
                echo "    (systemctl --user show $unit: could not read the state of $label;" >&2
                echo "     cannot confirm it is stopped)" >&2
                # A DIFFERENT claim from a refused stop: no stop or disable
                # request was made, so MANAGER_STOP_FAILED stays untouched.
                MANAGER_STATE_UNREAD=true
                this_state_unread=true
                ;;
        esac
    fi
    if [ "$has_file" = true ] || [ "$held" = true ]; then
        if systemctl --user stop "$unit" 9>&- 2>/dev/null; then
            echo "    stopped $label"
        else
            echo "    (systemctl --user stop $unit reported an error;" >&2
            if [ "$has_file" = true ]; then
                echo "     could not confirm it is stopped — removing its files anyway)" >&2
            else
                echo "     could not confirm it is stopped)" >&2
            fi
            MANAGER_STOP_FAILED=true
            this_stop_failed=true
        fi
        UNINSTALL_REMOVED=true
    fi
    if [ "$has_file" = true ]; then
        if systemctl --user disable "$unit" 9>&- 2>/dev/null; then
            echo "    disabled $label"
        else
            echo "    (systemctl --user disable $unit reported an error;" >&2
            echo "     could not confirm its enablement is cleared — removing its files anyway)" >&2
            MANAGER_DISABLE_FAILED=true
            this_disable_failed=true
        fi
    fi
    if [ "$this_stop_failed" = true ] && [ "$this_disable_failed" = true ]; then
        LINUX_UNIT_HINTS="$LINUX_UNIT_HINTS $unit=both"
    elif [ "$this_stop_failed" = true ]; then
        LINUX_UNIT_HINTS="$LINUX_UNIT_HINTS $unit=stop"
    elif [ "$this_disable_failed" = true ]; then
        LINUX_UNIT_HINTS="$LINUX_UNIT_HINTS $unit=disable"
    elif [ "$this_state_unread" = true ]; then
        LINUX_UNIT_HINTS="$LINUX_UNIT_HINTS $unit=unread"
    fi
}

# linux_uninstall_hint: the Linux exit-4 "verify by hand" text, reading
# LINUX_UNIT_HINTS (above) rather than `lib.sh`'s own `service_inspect_hint`
# — that one is deliberately left unchanged, since redeploy.sh still uses
# it and it is correct there: redeploy leaves the unit's file in place.
# --uninstall does not: by the time run_uninstall reaches its exit-4 path
# the failed unit's FILE is already removed and `systemctl --user
# daemon-reload` has already run, so `systemctl --user cat solador-agent`
# (service_inspect_hint's own Linux line) answers "No files found" whether
# or not the process is still running, and names only the metrics unit
# regardless of which of up to four actually failed.
#
# One `systemctl --user status <unit>` line per unit LINUX_UNIT_HINTS
# recorded (order of first failure, the order stop_and_disable_linux_unit's
# own callers run in) — `status`, not `cat`, because a unit whose FILE is
# gone still has a live status the manager can report (running, dead,
# failed) and `cat` cannot. Any recorded disable failure — alone or beside
# a stop failure — also gets the one command that finds what a `disable`
# systemd could not confirm actually leaves behind: a dangling enablement
# symlink under one of the *.wants directories, printed once, not once per
# unit, since it is not itself a per-unit listing.
linux_uninstall_hint() {
    local record hint_unit hint_kind hint_seen_disable=false
    for record in $LINUX_UNIT_HINTS; do
        hint_unit="${record%=*}"
        hint_kind="${record##*=}"
        echo "         systemctl --user status $hint_unit" >&2
        case "$hint_kind" in
            disable | both) hint_seen_disable=true ;;
        esac
    done
    if [ "$hint_seen_disable" = true ]; then
        echo "         ls -l ~/.config/systemd/user/*.wants/" >&2
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

    # An unsupported platform is refused HERE — before the manager-reachability
    # check below, and before the lock section further down touches anything:
    # that section creates a lock file (and, on a host with no install
    # directory yet, would create the directory too), which an "unsupported
    # platform (e.g. FreeBSD)" refusal claiming "Nothing has been changed"
    # must not leave behind. Checked first, so nothing below ever runs for an
    # OS this does not name.
    case "$OS" in
        Linux | Darwin) ;;
        *)
            echo "ERROR: --uninstall supports Linux (systemd) and macOS (launchd); this is $OS." >&2
            return 1
            ;;
    esac

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
    # lifetime (agent/src/update.rs, #393). update/rollback never remove it
    # once created (only this uninstall does, below), so its mere presence
    # proves nothing about whether it is HELD right now.
    # Taken (and held) before anything else touches disk: uninstalling
    # mid-transaction would race that transaction's own binary swap, the same
    # race install.sh itself must not run either.
    #
    # `exec 9>>"$lock_file"` opens fd 9 on it — creating the file if it is
    # missing, and NEVER truncating one that already exists, the same
    # create(true).truncate(false) agent/src/update.rs's own
    # TransactionLock::try_acquire opens it with, so a note a genuine
    # transaction already wrote there survives until THIS run actually takes
    # the lock, below. (Its parent directory — $INSTALL_DIR — is never
    # created here: a host where it does not exist yet has nothing installed
    # and nothing to lock, so the whole section below is skipped instead.)
    #
    # A non-blocking exclusive flock is then taken on fd 9, and HELD until the
    # `exec 9>&-` near the end of this function (after the lock file's own
    # removal, before the update stamp and the --purge removals) — never a
    # one-shot check — with `flock -n 9`
    # where flock(1) is on PATH: `man flock`'s own EXAMPLES idiom for locking
    # the CALLER's own already-open fd in place (a numeric-fd-only invocation
    # locks that fd itself, and the lock persists for as long as the fd stays
    # open, which here is until this function releases it at the very end).
    # Where flock(1) is not on PATH (stock macOS ships none), the stock
    # perl's Fcntl flock asks the SAME kernel question on the SAME fd:
    # `open(my $fh,"<&=",9)` reopens fd 9 by number without dup()ing a new
    # file description, so the flock it takes is the SAME open file
    # description bash's fd 9 refers to, and — confirmed empirically against
    # a genuinely competing process before this was trusted, not merely
    # assumed from `perlfunc` — it persists after that perl process exits,
    # for as long as bash's own fd 9 stays open. This is the exact primitive
    # `TransactionLock` itself uses (`std::fs::File::try_lock`, which is
    # `flock()` on Unix, never `fcntl()`/`F_SETLK` — confirmed by reading its
    # own comments on the module: a lock surviving `fork()` until `exec()` is
    # `flock()`'s open-file-description semantics, not `fcntl()`'s
    # per-process one), so an `update`/`rollback` arriving in the window this
    # uninstall spends stopping the service and removing its unit/plist meets
    # the SAME contention a real transaction would, reported as *busy* (exit
    # 75) by its own code — never a race this process needs to re-check
    # afterwards, on either platform.
    #
    # Where NEITHER tool is on PATH there is no way left to ask the kernel
    # whether a transaction is running, and guessing "free" is the one wrong
    # answer (an uninstall, or a fresh transaction, racing one this host
    # cannot see) — so this fails toward busy: refused before anything
    # changes, the same direction every other preflight refusal above fails.
    #
    # Both checks below run BEFORE any `mkdir`/`exec`: opening (and thereby
    # creating) the lock file before asking whether a lock tool exists would
    # make "neither flock(1) nor perl" exit 1 "Nothing has been changed"
    # while leaving that lock file (and, on a host with no install directory
    # yet, the directory itself) behind. The install directory not existing at
    # all is the stronger case: nothing can possibly be installed under it,
    # so the whole lock section — tool check, mkdir, exec, note — is skipped
    # rather than materializing $INSTALL_DIR just to find it empty; the
    # removal switch below still runs and still correctly reports "nothing
    # installed" for a host in that state (or removes a stray unit/plist an
    # unmigrated /opt host can carry even with no local install directory).
    local lock_file="$DEST_BIN.update.lock"
    # $INSTALL_DIR (set above, near $DEST_BIN's own definition) IS the
    # directory of $DEST_BIN — reused rather than re-derived with `dirname`.
    #
    # created_lock records whether THIS run's own open is what brought the
    # file into existence — decided by an ATOMIC create-if-missing, before
    # `exec 9>>` below (which would otherwise silently create the same file
    # and leave no way to tell) ever touches it. `( set -C; : > "$lock_file"
    # )` — noclobber — refuses to write through a path that already exists,
    # so success is proof of authorship even against a second process
    # racing this exact instant, and failure is equally atomic proof this
    # run did NOT create it. On a host where nothing was ever installed (no
    # unit/plist, no binary, and no lock file either — a genuine
    # "nothing to do" --uninstall), creating this file ourselves purely to
    # check for contention must not itself count as "installed state" this
    # run removed; a lock a real transaction already left behind is
    # different — cleaning THAT up is real work, reported like any other
    # removal (lock_pre_existed, its exact inverse, is what the
    # successful-completion accounting further down reads for that).
    #
    # On every REFUSAL path created_lock is the ONLY thing that may delete
    # this file, and even that comes with its own limit: never on a busy
    # result, and never when this run could not actually verify the lock is
    # free. (The end-of-run removal below is the one deliberate unlink of a
    # held lock; see the header's LOCK LIFETIME for why it is safe there.)
    # TransactionLock in agent/src/update.rs is explicit that unlinking a
    # lock file while another process holds a lock on it is how two processes come to
    # hold "the" lock at once — flock() locks the open file description, not
    # the path, so a second process that later opens the SAME NAME after
    # this one unlinks it opens a DIFFERENT inode and neither is actually
    # contending with the other any more. A busy refusal below — the file IS
    # held, by anyone, whether or not this run created it — therefore
    # deletes nothing at all, and neither does perl's own "could not reopen
    # fd 9" branch further down: that failure proves nothing about whether
    # the lock is actually free, so it is held to the same rule as a
    # genuinely busy result rather than assumed safe to clean up. Only the
    # narrower branch where `exec 9>>` itself fails to open the file — a
    # permission or I/O error, never a locking outcome — may still delete
    # it, and only when created_lock proves this run is the one that made
    # it.
    local created_lock=false
    local lock_pre_existed=false
    if [ -d "$INSTALL_DIR" ]; then
        local lock_tool=""
        if command -v flock >/dev/null 2>&1; then
            lock_tool=flock
        elif command -v perl >/dev/null 2>&1; then
            lock_tool=perl
        else
            echo "ERROR: cannot check the update lock ($lock_file) — neither flock(1) nor perl" >&2
            echo "       is on PATH, so this cannot tell whether an update or rollback is" >&2
            echo "       running right now. Refusing rather than guessing free. Install one of" >&2
            echo "       them (flock(1) ships in util-linux on Linux; perl ships with macOS)" >&2
            echo "       and re-run. Nothing has been changed." >&2
            return 1
        fi

        # $INSTALL_DIR already exists (checked above), so no mkdir is needed
        # before this create — unlike the parent directory, which a
        # genuinely fresh host would not yet have.
        if ( set -C; : > "$lock_file" ) 2>/dev/null; then
            created_lock=true
        else
            lock_pre_existed=true
        fi

        exec 9>>"$lock_file" || {
            echo "ERROR: could not open $lock_file to take the update lock." >&2
            [ "$created_lock" = true ] && rm -f "$lock_file" 9>&- 2>/dev/null || true
            return 1
        }
        if [ "$lock_tool" = flock ]; then
            if ! flock -n 9; then
                echo "ERROR: $lock_file is held — an update or rollback is in progress." >&2
                echo "       Uninstalling now would race that transaction's own binary swap." >&2
                echo "       Wait for it to finish (or fail) and re-run." >&2
                exec 9>&-
                # NEVER deleted on a busy result — see the comment above this
                # whole section: unlinking a held lock file is how two
                # processes end up holding "the" lock at once.
                return 1
            fi
        else
            local perl_lock_status=0
            perl -MFcntl=:flock -e '
                open(my $fh, "<&=", 9) or exit 2;
                exit(flock($fh, LOCK_EX | LOCK_NB) ? 0 : 1);
            ' || perl_lock_status=$?
            case "$perl_lock_status" in
                0) ;; # acquired — held on fd 9 for the rest of this function
                2)
                    echo "ERROR: cannot check the update lock ($lock_file) — perl could not" >&2
                    echo "       reopen its already-open file descriptor. Refusing rather than" >&2
                    echo "       guessing free." >&2
                    # NEVER deleted, even when created_lock is true — this
                    # run could not actually verify the lock is free, so it
                    # is held to the same rule as the genuinely busy branch,
                    # below. That means the "nothing changed" claim is only
                    # true when this run did not create the file: when it
                    # did, the empty file its own atomic create left behind
                    # is a real, if inert, change — said below rather than
                    # papered over. A LATER run that reaches this same
                    # section finds the file already present (created_lock
                    # false, this time) and, once it gets past whatever
                    # tripped this check, removes it: run_uninstall's own
                    # cleanup at the end always calls uninstall_remove on
                    # $lock_file once lock_pre_existed is true.
                    if [ "$created_lock" = true ]; then
                        echo "       Nothing else has been changed; an empty lock file may remain" >&2
                        echo "       at $lock_file; a later run that gets past this check removes it." >&2
                    else
                        echo "       Nothing has been changed." >&2
                    fi
                    exec 9>&-
                    return 1
                    ;;
                *)
                    echo "ERROR: $lock_file is held — an update or rollback is in progress." >&2
                    echo "       Uninstalling now would race that transaction's own binary swap." >&2
                    echo "       Wait for it to finish (or fail) and re-run." >&2
                    exec 9>&-
                    # NEVER deleted on a busy result — see the flock branch's
                    # own comment, above.
                    return 1
                    ;;
            esac
        fi
        # Held: leave a note for the NEXT update/rollback's own busy message to
        # read — "pid=<pid> since=<epoch>", the same shape agent/src/update.rs
        # writes — so it names THIS uninstall rather than a stale previous
        # holder. A fresh `>` write is safe here, unlike the flock/perl calls
        # above: opening a file for writing never itself attempts a lock, and
        # nothing else can hold $lock_file while this process does. Best effort,
        # like update.rs's own note: a lock whose note could not be written is
        # still a lock.
        printf 'pid=%s since=%s\n' "$$" "$(date +%s 9>&-)" > "$lock_file" 2>/dev/null || true
    fi

    echo "==> Uninstalling $BIN_NAME for $(id -un 9>&-) ($OS)"

    # Set when a service-manager call the reachability check above should
    # have let succeed reports an error anyway — a narrower, rarer case than
    # "the manager was unreachable" (already refused above). Two separate
    # flags, not one: a failed `stop` (or, on macOS, `bootout`) means this
    # run cannot promise the live process actually stopped; a failed
    # `disable` while `stop` succeeded means only its future auto-start (at
    # next login/boot) is unconfirmed — a DIFFERENT, weaker claim, and
    # conflating the two would either overstate a disable-only failure ("may
    # still be running" when it is not) or understate a stop failure. Read
    # at the end to pick the summary line and the exit status: 0 ("Done") is
    # a claim only a manager that agreed to every stop AND disable request
    # has earned.
    local MANAGER_STOP_FAILED=false
    local MANAGER_DISABLE_FAILED=false
    # A unit's state could not be read (Linux `systemctl --user show` failed
    # or printed no properties): no stop or disable was attempted. Kept apart
    # from the two flags above because it is neither claim, and because on a
    # clean host nothing was removed either.
    local MANAGER_STATE_UNREAD=false
    # Read BEFORE the unit/plist is removed below — unowned_service_binary
    # needs the file that is about to be deleted.
    local foreign_bin
    foreign_bin="$(unowned_service_binary)"

    # No `*)` arm here: $OS is already Linux or Darwin, refused at the very
    # top of this function otherwise — a dead arm, untestable because nothing
    # can reach it, is worse than none.
    case "$OS" in
        Linux)
            # Each of the four units below is stopped when its unit FILE exists
            # or the manager still holds it, and disabled only while the
            # FILE exists (#463 — see stop_and_disable_linux_unit's own
            # comment). Every argument here is the unit's FULL
            # name (e.g. "solador-agent.service", never bare
            # "solador-agent"): `list-units` never appends `.service`, and
            # the update TIMER needs it besides — a bare
            # "solador-agent-update" resolves to the oneshot ".service",
            # never the ".timer".
            stop_and_disable_linux_unit "$BIN_NAME.service" "$UNIT_DST" "$BIN_NAME.service"
            stop_and_disable_linux_unit "$UPDATE_NAME.timer" "$UPDATE_TIMER_DST" "$UPDATE_NAME.timer"
            stop_and_disable_linux_unit "$UPDATE_NAME.service" "$UPDATE_UNIT_DST" "$UPDATE_NAME.service"
            # The pre-rename unit: a host handed over (#392) without ever
            # having run a fresh install afterwards can still carry it
            # alongside the current one. Stopped and disabled the same way,
            # and named in the same daemon-reload/reset-failed below, so a
            # handed-over host ends up exactly as clean as one that was
            # always solador-agent.
            stop_and_disable_linux_unit "$LEGACY_BIN_NAME.service" "$LEGACY_UNIT" "$LEGACY_BIN_NAME.service (pre-rename)"
            uninstall_remove "$UNIT_DST" "systemd unit"
            uninstall_remove "$UNIT_DST.prev" "systemd unit (previous, from a migration)"
            uninstall_remove "$UPDATE_UNIT_DST" "update oneshot unit"
            uninstall_remove "$UPDATE_TIMER_DST" "update timer"
            uninstall_remove "$LEGACY_UNIT" "systemd unit (pre-rename)"
            # Only when something above actually changed: a no-op second run
            # makes no MUTATING manager call (its read-only `show` state
            # reads above change nothing).
            if [ "$UNINSTALL_REMOVED" = true ]; then
                systemctl --user daemon-reload 9>&- 2>/dev/null || true
                # Clears the "failed" state a disable/stop that reported an
                # error above can leave behind on any of these units —
                # reset-failed takes unit names, never paths, and tolerates
                # one that was never loaded or never existed.
                systemctl --user reset-failed "$BIN_NAME.service" "$UPDATE_NAME.service" \
                    "$UPDATE_NAME.timer" "$LEGACY_BIN_NAME.service" 9>&- 2>/dev/null || true
            fi
            uninstall_remove "$GUARD_DST" "update guard"
            if [ -n "$foreign_bin" ]; then
                left_behind_hint "$foreign_bin"
            fi
            ;;
        Darwin)
            local metrics_svc update_svc
            metrics_svc="gui/$(id -u 9>&-)/$LAUNCHD_LABEL"
            update_svc="gui/$(id -u 9>&-)/$UPDATE_LABEL"
            if launchctl print "$metrics_svc" 9>&- >/dev/null 2>&1; then
                if launchctl bootout "$metrics_svc" 9>&- 2>/dev/null; then
                    echo "    stopped $metrics_svc"
                else
                    echo "    (launchctl bootout $metrics_svc reported an error;" >&2
                    echo "     could not confirm it is stopped — removing its files anyway)" >&2
                    MANAGER_STOP_FAILED=true
                fi
                UNINSTALL_REMOVED=true
            fi
            if launchctl print "$update_svc" 9>&- >/dev/null 2>&1; then
                if launchctl bootout "$update_svc" 9>&- 2>/dev/null; then
                    echo "    stopped $update_svc"
                else
                    echo "    (launchctl bootout $update_svc reported an error;" >&2
                    echo "     could not confirm it is stopped — removing its files anyway)" >&2
                    MANAGER_STOP_FAILED=true
                fi
                UNINSTALL_REMOVED=true
            fi
            uninstall_remove "$PLIST_DST" "LaunchAgent plist"
            uninstall_remove "$UPDATE_PLIST_DST" "update LaunchAgent plist"
            uninstall_remove "$LAUNCHER_DST" "launcher"
            if [ -n "$foreign_bin" ]; then
                left_behind_hint "$foreign_bin"
            fi
            ;;
    esac

    # No second lock check is needed here: this process has held the SAME
    # flock (or refused before starting) since before the service-manager
    # calls above, so a transaction attempting to start during them met THIS
    # run's hold as busy on its own terms (exit 75) rather than racing
    # anything.
    uninstall_remove "$DEST_BIN" "binary"
    uninstall_remove "$DEST_BIN.prev" "binary (previous)"
    uninstall_remove "$DEST_BIN.new" "binary (staged)"
    # A leftover from a half-done `solador-agent rollback` (agent/src/update.rs):
    # the live binary's only copy at the moment the swap ran, kept until an
    # operator moves it by hand. Once the whole install is going away there is
    # nothing left for it to be a backup of.
    uninstall_remove "$DEST_BIN.rollback-displaced" "binary (rollback-displaced, from a half-done rollback)"
    # A lock file THIS run created purely to check for contention (nothing
    # pre-existed, and nothing else above needed removing either) is not
    # "installed state" — deleted quietly, own failure and all, so it never
    # turns a genuine no-op into a false "Done" or a false exit 6. One that
    # pre-existed (a real transaction's leftover) or that this run is
    # already reporting as a real uninstall goes through the same accounted
    # path as everything else.
    if [ "$lock_pre_existed" = true ] || [ "$UNINSTALL_REMOVED" = true ]; then
        uninstall_remove "$lock_file" "update transaction lock"
    else
        rm -f "$lock_file" 9>&- 2>/dev/null || true
    fi
    # Release the hold taken above. The lock file was just unlinked while
    # still held: nothing runs before the release below, and on a clean run the
    # unit/plist and the binary are already gone, so a transaction starting
    # now has no install to resolve and nothing to swap. Also harmless if $lock_file's own removal just failed
    # (uninstall_remove's own FAILED path, above): the fd, and the flock on
    # it, are independent of the directory entry. The update stamp and the
    # --purge removals below run after the release.
    exec 9>&-
    uninstall_remove "$UPDATE_STAMP" "update stamp"

    if [ "$PURGE" = true ]; then
        uninstall_remove "$ENV_FILE" "env file (--purge: it held the bearer token)"
        # The pre-rename install (#392's own handover) copied its token OUT of
        # this file and into $ENV_FILE, but never deleted it — so a $ENV_FILE
        # purge that stopped there would still leave the same secret sitting
        # under an account nothing runs any more, in the one file --purge
        # exists to be certain is gone.
        uninstall_remove "$LEGACY_ENV_FILE" "legacy env file (--purge: install copied its token out and never deleted it)"
        # The self-signed TLS keypair (#447), the same "credential this
        # script never generated and does not lightly discard" reasoning as
        # the env file's token: plain --uninstall keeps it (a re-install as
        # this user reuses it, byte for byte, and every cockpit that has
        # pinned its fingerprint keeps working), --purge removes it because
        # purging IS "re-pair from scratch" — a fresh certificate next start.
        uninstall_remove "$TLS_KEY_FILE" "TLS key (--purge: re-pairing needs a fresh certificate)"
        uninstall_remove "$TLS_CERT_FILE" "TLS certificate (--purge: re-pairing needs a fresh certificate)"
    else
        if [ -e "$ENV_FILE" ]; then
            echo "    left:    env file ($ENV_FILE) — holds the bearer token; remove it with:"
            echo "               $RERUN_CMD --uninstall --purge"
        fi
        if [ -e "$TLS_KEY_FILE" ] || [ -e "$TLS_CERT_FILE" ]; then
            echo "    left:    TLS key/certificate ($TLS_KEY_FILE, $TLS_CERT_FILE) — a re-install as"
            echo "               this user reuses them; every cockpit that has pinned the"
            echo "               fingerprint keeps working. --purge also removes them (re-pairing"
            echo "               then generates a fresh certificate on the next start):"
            echo "               $RERUN_CMD --uninstall --purge"
        fi
    fi

    echo
    if [ "$UNINSTALL_FAILED" = true ]; then
        # A more severe, and different, claim than MANAGER_STOP_FAILED/
        # MANAGER_DISABLE_FAILED below: something that SHOULD be an ordinary
        # `rm -f` provably did not
        # happen (see uninstall_remove's own comment — the FAILED lines
        # above this name which path and why), so this is not "Done" and not
        # "refused, nothing changed" either, since the service was likely
        # already stopped and other files already removed above. Exit 6 is
        # its own code rather than folding into 4's: 4 promises every file found
        # WAS removed and only a manager call is unconfirmed; that promise
        # is false here.
        echo "==> $BIN_NAME's uninstall for $(id -un 9>&-) did NOT finish: see the FAILED line(s)" >&2
        echo "    above. Whatever else is listed as \"removed\" or \"stopped\" above this is" >&2
        echo "    genuinely gone; fix the reported problem (a read-only parent directory, an" >&2
        echo "    immutable file, or similar) and re-run to finish." >&2
        return 6
    fi
    if [ "$MANAGER_STOP_FAILED" = true ] || [ "$MANAGER_DISABLE_FAILED" = true ] ||
        [ "$MANAGER_STATE_UNREAD" = true ]; then
        # "Done: uninstalled" is a claim about the SERVICE, not the files, and
        # a manager that answered `show-environment`/`print gui/<uid>` (the
        # reachability check above) yet refused a specific stop or disable
        # request, or could not report a unit's state, is a narrower, rarer
        # thing than unreachable. Exit 4 is distinct from both 0 ("Done",
        # earned) and 1 ("refused, nothing changed" — false here when files
        # went). The message states only what happened: "files were removed"
        # only when UNINSTALL_REMOVED says some were (a state read failing on
        # a clean host removed nothing), and three separate claims: a failed
        # `stop` means the process itself may still be running; a failed
        # `disable` with a successful `stop` means only its future auto-start
        # is unconfirmed; an unread state means no request was made at all.
        if [ "$MANAGER_STOP_FAILED" = true ] || [ "$MANAGER_DISABLE_FAILED" = true ]; then
            if [ "$UNINSTALL_REMOVED" = true ]; then
                echo "==> $BIN_NAME's files were removed for $(id -un 9>&-), but at least one" >&2
            else
                echo "==> $BIN_NAME's uninstall for $(id -un 9>&-) removed no files, and at least one" >&2
            fi
            echo "    service-manager call above could not confirm a stop or disable request" >&2
            echo "    actually succeeded." >&2
        fi
        if [ "$MANAGER_STATE_UNREAD" = true ]; then
            if [ "$MANAGER_STOP_FAILED" = true ] || [ "$MANAGER_DISABLE_FAILED" = true ]; then
                echo "    Also, the service manager could not report the state of at least one" >&2
                echo "    unit, so no stop or disable request was made for it." >&2
            else
                if [ "$UNINSTALL_REMOVED" = true ]; then
                    echo "==> $BIN_NAME's files were removed for $(id -un 9>&-), but the service" >&2
                else
                    echo "==> $BIN_NAME's uninstall for $(id -un 9>&-) removed no files, and the service" >&2
                fi
                echo "    manager could not report the state of at least one unit, so it cannot" >&2
                echo "    be confirmed stopped. No stop or disable request was made for it." >&2
            fi
        fi
        if [ "$MANAGER_STOP_FAILED" = true ] || [ "$MANAGER_STATE_UNREAD" = true ]; then
            echo "    Verify by hand that nothing is still running:" >&2
        else
            echo "    The service itself was told to stop; only its future auto-start (at" >&2
            echo "    next login/boot) is unconfirmed. Verify by hand:" >&2
        fi
        if [ "$OS" = "Linux" ]; then
            linux_uninstall_hint
        else
            service_inspect_hint "$LAUNCHD_LABEL" 9>&- >&2
        fi
        return 4
    fi
    if [ "$UNINSTALL_REMOVED" = true ]; then
        echo "==> Done: $BIN_NAME uninstalled for $(id -un 9>&-)."
    else
        echo "==> Nothing installed for $(id -un 9>&-); nothing to do."
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
# run_uninstall's own return is propagated as-is (0, 1, 4 or 6 — see the
# "EXIT STATUS of --uninstall" table in this file's header), never collapsed to a bare 0/1: 4 and 6 are each a
# distinct claim from both, and from each other.
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
# The order is the same as solador-agent's own resolve_bind_host (#449): an
# explicit SOLADOR_AGENT_BIND, then the value the env file already carries,
# then the host's Tailscale IP. A host with none of the three is decided
# below, once TLS is: with TLS on the token is encrypted and the certificate
# pinned, so the answer is all interfaces (0.0.0.0) rather than a refusal;
# with TLS off the tailnet is the only thing protecting the token on the wire,
# and the host is refused. The refusal still lands before a download, a
# signature check and a token prompt whenever TLS is known to be off by then.
# Every env-file read goes through env_value (lib.sh): the same CR/whitespace/
# quote stripping the launcher and systemd apply, so what is verified is what
# the service actually starts with.
EXISTING_BIND="$(env_value "$ENV_FILE" SOLADOR_AGENT_BIND)"
# SOLADOR_AGENT_BIND_AUTO=1 (#449) is written beside an all-interfaces bind THIS
# script chose because TLS was on and there was no tailnet. It marks that bind
# as *provisional*: on every re-run it is set aside and the bind is resolved
# again from scratch (a tailnet IP if one is up now; all interfaces only while
# TLS is on; otherwise the plain-HTTP refusal), so it never survives TLS being
# turned off and never pins a host to the wildcard after Tailscale arrives. A
# bind WITHOUT the marker is an operator's explicit choice and is honoured as it
# always was, plain HTTP included. The agent and the launcher ignore the key.
EXISTING_BIND_AUTO="$(env_value "$ENV_FILE" SOLADOR_AGENT_BIND_AUTO)"
BIND_AUTO=false
AUTO_BIND_SET_ASIDE=""
# ...and the same carry-over, so a host that deliberately opted out of the
# tailnet default keeps its choice instead of silently reverting to detection.
if [ -z "$EXISTING_BIND" ]; then
    EXISTING_BIND="$(env_value "$LEGACY_ENV_FILE" DEVCANOPY_AGENT_BIND)"
elif [ "$EXISTING_BIND_AUTO" = "1" ]; then
    case "$EXISTING_BIND" in
        0.0.0.0 | "::" | "[::]")
            AUTO_BIND_SET_ASIDE="$EXISTING_BIND"
            EXISTING_BIND=""
            ;;
    esac
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
    if [ -n "$AUTO_BIND_SET_ASIDE" ] && [ -n "$BIND" ]; then
        BIND_SOURCE="detected Tailscale IP; the all-interfaces bind an earlier install chose is replaced"
    fi
fi
# Empty here means "no explicit bind, no existing bind, no tailnet": resolved
# once TLS is decided (the block after the "TLS:" line, below).

# An IPv6 zone id (fe80::1%en0) is refused whether or not TLS is on (#476), and
# here, before a download or a token prompt, whichever of the three sources the
# bind came from. `update`'s health probe cannot dial it (the TLS probe has no
# place for the zone; the plain URL does not parse), so a host installed on one
# would be refused at its first `update` or `rollback` — and the agent itself
# refuses to start on it. Same words as the agent's FATAL
# (agent/src/update.rs's `zone_id_refusal`, plus a link-local note).
case "$BIND" in
    *%*)
        echo "ERROR: SOLADOR_AGENT_BIND '$BIND' carries an IPv6 zone id, which the health probe" >&2
        echo "       cannot dial; bind the address without the zone (or a name that resolves to it)." >&2
        echo "       A link-local address is reachable only from its own link, so no other network's" >&2
        echo "       cockpit could use it anyway. Nothing has been changed." >&2
        exit 1
        ;;
esac

# The refusal for a host with no bind address and no TLS. One place, so the
# early (pre-download) and late (post-staging) callers say the same thing.
refuse_no_bind() {
    echo "ERROR: could not detect a Tailscale IP for SOLADOR_AGENT_BIND, and TLS is off," >&2
    echo "       so the bearer token would cross the network in the clear." >&2
    echo "       Bring up Tailscale, set SOLADOR_AGENT_BIND to the address the cockpit" >&2
    echo "       dials, or serve HTTPS (SOLADOR_AGENT_TLS=1 $RERUN_CMD), which binds" >&2
    echo "       all interfaces when there is no tailnet." >&2
    if [ -n "$AUTO_BIND_SET_ASIDE" ]; then
        echo "       The env file's SOLADOR_AGENT_BIND=$AUTO_BIND_SET_ASIDE was chosen by an earlier install (marked" >&2
        echo "       SOLADOR_AGENT_BIND_AUTO=1) for TLS with no tailnet, so it is not kept over plain" >&2
        echo "       HTTP. If you turn TLS off by editing $ENV_FILE yourself, remove BOTH" >&2
        echo "       SOLADOR_AGENT_BIND and SOLADOR_AGENT_BIND_AUTO from it, or set SOLADOR_AGENT_BIND" >&2
        echo "       to one address: the agent honours a bind it finds there, wildcard included." >&2
    fi
    echo "       Nothing has been changed." >&2
    exit 1
}

# Same precedence as the bind: an explicit SOLADOR_AGENT_PORT, then the port
# the env file already carries, then the default. Reading the existing value
# is what keeps a re-run (and the /opt migration) from resetting a host that
# chose another port back to 7878 — and the health probe below dials the port
# this file says, so a reset would also make verification look at the wrong
# socket.
EXISTING_PORT="$(env_value "$ENV_FILE" SOLADOR_AGENT_PORT)"
PORT="${SOLADOR_AGENT_PORT:-${EXISTING_PORT:-7878}}"

# ---- TLS opt-in (#447): freshness/existing-choice is decided now, capability is not --------
# A FRESH install — no env file existed before this run — writes
# SOLADOR_AGENT_TLS=1: HTTPS is the default for a host nobody has configured
# yet. A re-run (the env file already existing, however it got here) keeps
# whatever choice is already recorded, touched only by the explicit
# --enable-tls flag — there is no --disable-tls; turning TLS back off, like
# rotating the token, is an env-file edit an operator makes by hand, never
# something a re-run does on its own.
#
# What is decided HERE is only freshness and the existing env file's choice —
# BOTH of which are readable before any download. The actual TLS_VALUE this
# run writes is NOT decided until the release is staged and verified, below:
# it also depends on whether the STAGED BINARY understands
# SOLADOR_AGENT_TLS at all, which can only be asked of the verified bytes,
# never assumed from this script's own git history. A release cut before
# #447 (or one pinned via SOLADOR_AGENT_RELEASE to before it) silently
# ignores the env var and serves plain HTTP regardless of what this file
# says — so defaulting a fresh install to TLS=1 against such a binary would
# write a setting the agent cannot honour, and the health check below would
# fail confusingly (dialing https:// against a plain-HTTP server) rather
# than reporting the real cause.
#
# "Fresh" also checks the LEGACY env file (#447 review round 2): the
# pre-rename handover (below, "Found a pre-rename install") carries an
# existing host's token across specifically so its cockpit pairing survives
# the rename — $ENV_FILE has never existed under the new name, so without
# this check FRESH_INSTALL would read true and silently turn TLS on under
# that same host, breaking the exact pairing the handover exists to keep.
FRESH_INSTALL=true
if [ -f "$ENV_FILE" ] || [ -f "$LEGACY_ENV_FILE" ]; then
    FRESH_INSTALL=false
fi
EXISTING_TLS="$(env_value "$ENV_FILE" SOLADOR_AGENT_TLS)"

# Early refusal (#449): with no bind address at all, TLS is the only way this
# run can proceed. Whether TLS will be on is not final until the staged binary
# has been asked about it, but the cases where it cannot be are known now —
# SOLADOR_AGENT_TLS set to something other than 1, or an existing env file
# whose TLS choice is not 1 and no --enable-tls — so refuse those before any
# download, exactly as before. The remaining case (a fresh install whose
# staged binary predates TLS) is refused after staging by the same function.
if [ -z "$BIND" ]; then
    if [ -n "${SOLADOR_AGENT_TLS:-}" ]; then
        [ "$SOLADOR_AGENT_TLS" = "1" ] || refuse_no_bind
    elif [ "$FRESH_INSTALL" = false ] && [ "$ENABLE_TLS" != true ] && [ "$EXISTING_TLS" != "1" ]; then
        refuse_no_bind
    fi
fi

# A pin is validated before anything is created or fetched: a refused one must
# leave nothing behind, and is never interpolated into a request.
if [ -n "${SOLADOR_AGENT_RELEASE:-}" ]; then
    TAG="$SOLADOR_AGENT_RELEASE"
    validate_release_tag "$TAG" || exit 1
fi

# ---- stage: a private directory ----------------------------------------------
# A private directory under $HOME rather than /tmp: it is 0700 from birth
# (umask), and a /tmp mounted noexec — common on hardened hosts — would make
# the verified binary's own `--version` fail in a way that reads as "carries
# no version". (The atomic step is the later .new → live rename inside the
# install directory; staging is copied there with `install`, so it need not
# share a filesystem with anything.) It holds the feed too (#490), which is
# why it exists before the release is resolved.
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

# ---- resolve the release -----------------------------------------------------
# Pinned: an `agent-v*` tag, or a legacy `v*` tag with agent assets — validated
# above and used as written, with no feed involved. Otherwise the signed feed on
# the `agent-latest` release names the version (verified, THEN read — see
# lib.sh's resolve_latest_agent_release), and the binary comes from
# `agent-v<version>`.
if [ -n "${SOLADOR_AGENT_RELEASE:-}" ]; then
    echo "==> Release: $TAG (pinned by SOLADOR_AGENT_RELEASE)"
else
    echo "==> Reading the signed agent feed (the $AGENT_FEED_RELEASE release)"
    TAG="$(resolve_latest_agent_release "$RELEASE_REPO_URL" "$STAGE" "$SIGNING_PUBKEY")" || {
        echo "       Nothing has been changed. To install a release without the feed, pin one:" >&2
        echo "       SOLADOR_AGENT_RELEASE=agent-v<version> $RERUN_CMD" >&2
        echo "       (or a legacy v<version> tag that carries agent assets, for a release cut" >&2
        echo "       before the agent had its own train)." >&2
        exit 1
    }
    # The resolver's own output is the tag, and the tag is built from a version it
    # verified as a CalVer; validating it again is cheap and keeps the invariant
    # "nothing reaches a URL that validate_release_tag has not seen" local.
    validate_release_tag "$TAG" || exit 1
    echo "==> Release: $TAG (latest published, from the signed feed)"
fi
RELEASE_VERSION="$(release_version_of_tag "$TAG")"
ASSET="$(agent_asset_name "$RELEASE_VERSION" "$TRIPLE")"
ASSET_URL="$RELEASE_REPO_URL/releases/download/$TAG/$ASSET"

# ---- stage: download and verify ----------------------------------------------
STAGED_BIN="$STAGE/$ASSET"
STAGED_SIG="$STAGE/$ASSET.minisig"

echo "==> Downloading $ASSET"
if ! download_release_asset "$ASSET_URL" "$STAGED_BIN"; then
    echo "ERROR: could not download $ASSET_URL" >&2
    echo "       Release $TAG has no $ASSET, or it is not reachable." >&2
    echo "       Releases are found through the signed agent-latest feed now, and an agent release" >&2
    echo "       carries its binaries on an agent-v<version> tag; a release cut before the agent had" >&2
    echo "       its own train (v2026.9.3 and earlier predate agent binaries altogether) publishes" >&2
    echo "       none. This installer does not fall back to building from source." >&2
    echo "       Pin a release that has them:  SOLADOR_AGENT_RELEASE=agent-v<version> $RERUN_CMD" >&2
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

# ---- TLS opt-in (#447): NOW capability is known ------------------------------
# `tls-fingerprint` is refused as an unrecognized argument, exit 2, by any
# agent published before this feature — see parse_args in agent/src/main.rs,
# which dispatches it before the token check, the same as --version. Exit 0
# (fingerprint printed) or 1 (no certificate yet, but the subcommand IS
# recognized) both mean the opposite: this binary knows what
# SOLADOR_AGENT_TLS means. This is the only way to ask the VERIFIED bytes
# rather than assume this script's own git history.
STAGED_TLS_PROBE_RC=0
"$STAGED_BIN" tls-fingerprint >/dev/null 2>&1 || STAGED_TLS_PROBE_RC=$?
STAGED_BIN_SUPPORTS_TLS=true
[ "$STAGED_TLS_PROBE_RC" -eq 2 ] && STAGED_BIN_SUPPORTS_TLS=false

if [ -n "${SOLADOR_AGENT_TLS:-}" ]; then
    # The explicit override always wins, even against a binary that will
    # silently ignore it — the same "you asked for this" precedent
    # SOLADOR_AGENT_BIND/_PORT already set. The health check below still
    # fails informatively if it turns out not to work.
    TLS_VALUE="$SOLADOR_AGENT_TLS"
    TLS_SOURCE="SOLADOR_AGENT_TLS"
elif [ "$FRESH_INSTALL" = true ]; then
    if [ "$STAGED_BIN_SUPPORTS_TLS" = true ]; then
        TLS_VALUE=1
        TLS_SOURCE="fresh install"
    else
        TLS_VALUE=0
        TLS_SOURCE="fresh install, but $ASSET ($TARGET_VERSION) predates #447 and cannot serve TLS"
    fi
elif [ "$ENABLE_TLS" = true ]; then
    if [ "$STAGED_BIN_SUPPORTS_TLS" = true ]; then
        TLS_VALUE=1
        TLS_SOURCE="--enable-tls"
    else
        echo "ERROR: --enable-tls was given, but $ASSET ($TARGET_VERSION) predates #447 and" >&2
        echo "       does not support TLS (solador-agent tls-fingerprint is not a recognized" >&2
        echo "       command on this binary). Pin a release that has it"                       >&2
        echo "       (SOLADOR_AGENT_RELEASE=agent-v<version> $RERUN_CMD --enable-tls), or drop" >&2
        echo "       --enable-tls. Nothing has been changed." >&2
        exit 1
    fi
elif [ -n "$EXISTING_TLS" ]; then
    # Same capability check as --enable-tls, and for the same reason (#447
    # review round 2): a re-run pinning SOLADOR_AGENT_RELEASE to a release
    # before #447 — the exact downgrade this script's own error message
    # below recommends for other problems — must not keep re-writing
    # SOLADOR_AGENT_TLS=1 against a binary that will silently ignore it.
    # Refused before anything changes, like every other capability refusal
    # here; the operator's already-generated solador-agent.tls.crt is
    # untouched either way.
    if [ "$EXISTING_TLS" = "1" ] && [ "$STAGED_BIN_SUPPORTS_TLS" != true ]; then
        echo "ERROR: the env file already has SOLADOR_AGENT_TLS=1, but $ASSET ($TARGET_VERSION)" >&2
        echo "       predates #447 and does not support TLS (solador-agent tls-fingerprint is" >&2
        echo "       not a recognized command on this binary). Pin a release that has it, or" >&2
        echo "       turn TLS off explicitly:  SOLADOR_AGENT_TLS=0 $RERUN_CMD" >&2
        echo "       Nothing has been changed." >&2
        exit 1
    fi
    TLS_VALUE="$EXISTING_TLS"
    TLS_SOURCE="kept from the existing env file"
else
    TLS_VALUE=0
    TLS_SOURCE="kept from the existing env file (was unset)"
fi
if [ "$TLS_VALUE" = "1" ]; then
    echo "==> TLS: on ($TLS_SOURCE)"
else
    echo "==> TLS: off ($TLS_SOURCE; re-run with --enable-tls to turn it on)"
fi

# The host with no bind address, now that TLS is decided (#449). TLS on: all
# interfaces, and BIND_SOURCE says why. TLS off: the refusal, as before.
# It takes a TLS the staged binary can actually serve: an explicit
# SOLADOR_AGENT_TLS=1 against a release that predates TLS is served as plain HTTP.
if [ -z "$BIND" ]; then
    if [ "$TLS_VALUE" = "1" ] && [ "$STAGED_BIN_SUPPORTS_TLS" != true ]; then
        # TLS was asked for and cannot be served: telling the operator to set
        # SOLADOR_AGENT_TLS=1 would be circular. Name the binary, as the
        # sibling refusals above do.
        echo "ERROR: SOLADOR_AGENT_TLS=1 was given, but $ASSET ($TARGET_VERSION) predates #447 and" >&2
        echo "       does not support TLS (solador-agent tls-fingerprint is not a recognized" >&2
        echo "       command on this binary), and no Tailscale IP was detected for" >&2
        echo "       SOLADOR_AGENT_BIND — so there is nothing safe to bind. Pin a release that has" >&2
        echo "       TLS (SOLADOR_AGENT_RELEASE=agent-v<version> $RERUN_CMD), bring up Tailscale, set" >&2
        echo "       SOLADOR_AGENT_BIND to the address the cockpit dials, or drop SOLADOR_AGENT_TLS=1" >&2
        echo "       (or set it to 0) and set SOLADOR_AGENT_BIND, which is then a plain-HTTP install." >&2
        echo "       Nothing has been changed." >&2
        exit 1
    fi
    if [ "$TLS_VALUE" != "1" ]; then
        refuse_no_bind
    fi
    BIND="0.0.0.0"
    BIND_SOURCE="all interfaces: no Tailscale IP detected, and TLS is on"
    BIND_AUTO=true
fi
case "$BIND" in
    0.0.0.0 | "::" | "[::]") BIND_ALL_INTERFACES=true ;;
    *) BIND_ALL_INTERFACES=false ;;
esac
echo "==> Binding to $BIND ($BIND_SOURCE)"
if [ "$BIND_ALL_INTERFACES" = true ]; then
    if [ "$TLS_VALUE" = "1" ] && [ "$STAGED_BIN_SUPPORTS_TLS" = true ]; then
        echo "    The agent will be reachable on EVERY network this host is on, including any"
        echo "    public address. The bearer token and the pinned TLS certificate protect it;"
        echo "    firewall port ${PORT} or set SOLADOR_AGENT_BIND to one interface on a host with"
        echo "    a public address."
    else
        echo "    WARNING: PLAIN HTTP ON EVERY INTERFACE. This bind is an explicit choice"
        echo "    ($BIND_SOURCE) and is honoured, but the bearer token crosses every network"
        echo "    this host is on in the clear. The cockpit will not send it here over plain"
        echo "    HTTP unless the address it dials is loopback or Tailscale. Turn TLS on"
        echo "    (--enable-tls), or set SOLADOR_AGENT_BIND to one address."
    fi
fi

# A re-run is the update path, and a feed is a signed document, not a freshness
# guarantee (docs/AGENT-DISTRIBUTION.md §6): an intercepting proxy, or a stale
# cache, can replay an OLDER feed that is still validly signed, steering the
# install to an older, validly signed release. A FRESH install has nothing to
# compare against; a re-run does — the binary already serving. So a re-run
# refuses to move backwards unless the operator pinned the release explicitly,
# which is the one legitimate reason to. (An installed `+dev` source build is
# not a CalVer, so nothing is "newer" than it and this check does not apply.)
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
# What is carried over from the previous file is an ALLOW-LIST (#496), not
# "everything this script does not own". On Linux the unit loads the file
# wholesale (EnvironmentFile=), so a line carried through verbatim, such as an
# LD_PRELOAD= planted by anything that could reach the file, would survive
# every re-run and land in the agent's process. Kept: blank lines and comments
# (operators annotate the file), and the documented keys this script does not
# itself rewrite. Every other line is dropped and its KEY (never its value)
# is named on stderr, once. The install proceeds: a refusal would strand a
# host whose file carries a harmless stray key, and the point is only that the
# key stops reaching the agent.
#
# The two lists below are, together, exactly run-agent.sh's allow-list (the
# macOS launcher). That script is installed standalone and cannot source this
# one, so lib_test.sh asserts the parity.
ENV_CARRIED_KEYS="SOLADOR_AGENT_SKIP_FSTYPES RUST_LOG"
ENV_OWNED_KEYS="SOLADOR_AGENT_TOKEN SOLADOR_AGENT_BIND SOLADOR_AGENT_PORT SOLADOR_AGENT_TLS SOLADOR_AGENT_BIND_AUTO"

# carry_env_lines <file>: the lines of <file> that survive a re-run, on stdout;
# one warning per dropped key on stderr.
carry_env_lines() {
    local file="$1" line trimmed key k keep why dropped=" " lineno=0
    while IFS= read -r line || [ -n "$line" ]; do
        lineno=$((lineno + 1))
        # One trailing CR is a CRLF line ending (the launcher strips it too).
        # Any other CR is a line break to systemd, whose EnvironmentFile=
        # parser ends a line on "\n" and on "\r": `RUST_LOG=x<CR>LD_PRELOAD=y`
        # is one line here and two settings there, so it is dropped whole, as
        # is a comment hiding one.
        line="${line%$'\r'}"
        case "$line" in
            *$'\r'*)
                echo "WARNING: dropped line $lineno from $file: it contains a carriage return" >&2
                continue
                ;;
        esac
        # systemd trims leading whitespace before it reads a line, so a
        # whitespace-led `  KEY=value` is a setting there: judge the trimmed
        # form, and keep a setting only when it is in the form the launcher
        # accepts (a key at column 0).
        trimmed="${line#"${line%%[![:space:]]*}"}"
        case "$trimmed" in
            '' | '#'* | ';'*)
                printf '%s\n' "$line"
                continue
                ;;
        esac
        key="${line%%=*}"
        keep=false
        if [ "$key" != "$line" ]; then
            for k in $ENV_CARRIED_KEYS; do
                [ "$key" = "$k" ] && keep=true
            done
        fi
        if [ "$keep" = true ]; then
            printf '%s\n' "$line"
            continue
        fi
        # Name the key only, and only when it looks like one: anything else
        # (no `=`, empty, odd characters) could be a value, so it is named by
        # line.
        key="${trimmed%%=*}"
        case "$key" in
            "$trimmed" | '' | *[!A-Za-z0-9_]* | [0-9]*) key="(line $lineno, not a KEY=value setting)" ;;
        esac
        why="it is not a documented agent setting (on Linux, set it in a unit drop-in: systemctl --user edit solador-agent)"
        # The installer rewrites its own keys at column 0: an indented copy
        # is not "dropped", it is superseded.
        for k in $ENV_OWNED_KEYS; do
            [ "$key" = "$k" ] && continue 2
        done
        for k in $ENV_CARRIED_KEYS; do
            [ "$key" = "$k" ] && why="it is indented, and the agent reads settings only at column 0 (re-add it there)"
        done
        case "$dropped" in
            *" $key "*) ;;
            *)
                dropped="$dropped$key "
                echo "WARNING: dropped $key from $file: $why" >&2
                ;;
        esac
    done < "$file"
    return 0
}

ENV_NEW="$ENV_FILE.new"
(
    umask 077
    {
        printf 'SOLADOR_AGENT_TOKEN=%s\n' "$TOKEN"
        printf 'SOLADOR_AGENT_BIND=%s\n' "$BIND"
        printf 'SOLADOR_AGENT_PORT=%s\n' "$PORT"
        printf 'SOLADOR_AGENT_TLS=%s\n' "$TLS_VALUE"
        if [ "$BIND_AUTO" = true ]; then
            printf 'SOLADOR_AGENT_BIND_AUTO=1\n'
        fi
        if [ -f "$ENV_FILE" ]; then
            carry_env_lines "$ENV_FILE"
        fi
    } > "$ENV_NEW"
)
chmod 600 "$ENV_NEW"
mv -f "$ENV_NEW" "$ENV_FILE"
echo "==> Wrote $ENV_FILE (token + bind + port + tls, mode 600; documented keys and comments kept)"

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
if [ "$BIND_ALL_INTERFACES" = true ] && [ "$TLS_VALUE" != "1" ]; then
    echo "    Bind:    $BIND:$PORT — ALL interfaces, PLAIN HTTP ($BIND_SOURCE)"
elif [ "$BIND_ALL_INTERFACES" = true ]; then
    echo "    Bind:    $BIND:$PORT — ALL interfaces ($BIND_SOURCE)"
else
    echo "    Bind:    $BIND:$PORT — that interface only ($BIND_SOURCE)"
fi
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

# ---- TLS fingerprint (#447) -----------------------------------------------------
# Printed only when TLS is actually on for this install. By now verify_health
# above has already confirmed the service serving — over HTTPS, when TLS is
# on — so the certificate it generated on that first start is already there;
# this reads it (`solador-agent tls-fingerprint` never generates one, see
# agent/src/tls.rs) rather than reimplementing the SHA-256/DER read in shell.
if [ "$TLS_VALUE" = "1" ]; then
    if TLS_FINGERPRINT="$("$DEST_BIN" tls-fingerprint 2>&1)"; then
        echo "    TLS: on — certificate fingerprint:"
        echo "      $TLS_FINGERPRINT"
        echo "      To pair this host: in Solador, Settings > Connections, add (or edit) the host,"
        echo "      press \"Check certificate\", compare the fingerprint it shows with the one above,"
        echo "      and press \"Trust\" only if they match. Nothing is pinned until you do."
        echo "      Never delete $TLS_KEY_FILE / $TLS_CERT_FILE unless you mean to re-pair —"
        echo "      a new certificate makes every cockpit that paired this host report"
        echo "      \"certificate changed\" until it is re-paired."
        echo "      A Solador cockpit older than certificate pairing (#448) cannot dial an HTTPS"
        echo "      agent and reads this host as unreachable: update the cockpit, or edit"
        echo "      $ENV_FILE, set SOLADOR_AGENT_TLS=0, and restart the service."
        if [ "$BIND_ALL_INTERFACES" = true ]; then
            echo "      Plain HTTP on all interfaces sends the token in the clear: in that file ALSO"
            echo "      remove BOTH SOLADOR_AGENT_BIND and SOLADOR_AGENT_BIND_AUTO (so the agent"
            echo "      finds the tailnet address itself), or set SOLADOR_AGENT_BIND to the one address"
            echo "      the cockpit dials. The agent honours a bind it finds there, wildcard included."
        fi
    else
        echo "    TLS: on, but the fingerprint could not be read (this should not happen right" >&2
        echo "       after a verified HTTPS health check): $TLS_FINGERPRINT" >&2
        echo "       Inspect by hand:  $DEST_BIN tls-fingerprint" >&2
        echo "       Pairing in Solador needs it: Settings > Connections > \"Check certificate\"" >&2
        echo "       shows the fingerprint to compare with that command's output." >&2
    fi
else
    echo "    TLS: off (a fresh install turns this on by default; re-run with --enable-tls to opt in)"
fi

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
if [ "$TLS_VALUE" = "1" ]; then
    echo "  printf 'header = \"Authorization: Bearer %s\"\\ncacert = \"%s\"\\n' \"\$(grep '^SOLADOR_AGENT_TOKEN=' \"$ENV_FILE\" | cut -d= -f2- | sed -e 's/\\\\/\\\\\\\\/g' -e 's/\"/\\\\\"/g')\" \"$TLS_CERT_FILE\" | curl -sS -K - \"$(health_url "$BIND" "$PORT" "$TLS_VALUE")\""
else
    echo "  printf 'header = \"Authorization: Bearer %s\"\\n' \"\$(grep '^SOLADOR_AGENT_TOKEN=' \"$ENV_FILE\" | cut -d= -f2- | sed -e 's/\\\\/\\\\\\\\/g' -e 's/\"/\\\\\"/g')\" | curl -sS -K - \"$(health_url "$BIND" "$PORT" "$TLS_VALUE")\""
fi
echo
echo "Bearer token (give this to Solador): stored in $ENV_FILE (mode 600)."
echo "  Last 4 chars: ...${TOKEN: -4}   — read the full value with:"
echo "  grep '^SOLADOR_AGENT_TOKEN=' \"$ENV_FILE\" | cut -d= -f2-"
echo "To rotate it: edit that file, then restart the service."
