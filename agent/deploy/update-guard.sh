#!/usr/bin/env bash
#
# ExecCondition= guard for the Solador metrics agent's unattended update
# oneshot on Linux (#411): the systemd counterpart of run-agent.sh's update
# mode (#394), holding the same cadence decision — DAILY, NO CATCH-UP — in
# front of solador-agent-update.service.
#
# Installed by deploy/install.sh --enable-timer as
# ~/.local/bin/solador-agent-update-guard (copied out of the checkout, like
# the macOS launcher, so deleting the clone does not break the job) and named
# by the rendered solador-agent-update.service as
#
#   ExecCondition=<this file> %n
#
# so the user manager runs it before ExecStart on EVERY activation — the
# daily timer firing, a `systemctl --user start` by hand, and any firing the
# manager delivers for a reason of its own — and reads its exit status as
# (systemd.service(5), ExecCondition=; each observed on a real user manager,
# see the evidence block below):
#
#   0    run `solador-agent update`.
#   1    DISCARD this firing. ExecStart is skipped, the unit ends inactive
#        with Result=exec-condition ("Skipped due to 'exec-condition'" in the
#        journal; not in `systemctl --user --failed`) and the reason is one
#        line in the journal. Tomorrow's firing resolves it by itself: this
#        is the deliberate no-op the cadence describes.
#   255  HOLD. ExecStart is skipped and the unit FAILS — Result=exit-code,
#        listed by `systemctl --user --failed` until its next activation or
#        a `reset-failed` — because an input the decision needs could not be
#        read, or the stamp could not be written. A hold recurs every day
#        until what it names is fixed, and a green unit over that would be a
#        fabricated state; the journal line names what to fix. This is the
#        macOS launcher's exit 6, in the manager's own vocabulary. The inputs
#        that can hold are the ones the launcher holds on — the clock, this
#        activation's time, the stamp, HOME — plus the kernel's suspend
#        counter when it is present but unreadable, and every usage error;
#        and NOT the last resume, which
#        is advisory (below): a hold over a property of the host that the
#        operator cannot change is not fail-closed, it is a job that never
#        runs, and the daily red unit teaches the operator to stop reading
#        `--failed`, which is when a real hold stops being seen.
#
# Three more facts of that contract shape what this script may exit with.
# First, ExecCondition= honours SuccessExitStatus= too: an exit matching it
# RUNS ExecStart (observed — a condition exit of 4, the oneshot's
# SuccessExitStatus, ran the updater). So DISCARD_EXIT and HOLD_EXIT must
# never be a value that line names, and lib_test.sh asserts they are not.
# Second, the guard NOT BEING THERE is the one status the mapping gets wrong:
# the manager's exec failure is exit 203, inside the skip range, so a unit
# whose guard was deleted would skip every firing forever with
# Result=exec-condition (observed). That is why the unit also carries
# AssertFileIsExecutable= on this path — a failed assertion is an error line
# in the journal and a failed `start`, though systemd.unit(5) is explicit
# that it changes no unit state, so it is NOT in `--failed` either — why
# install.sh installs this file BEFORE it renders the unit, and why the
# documented removal takes the units out before this file. Every usage
# error below is a HOLD for the same reason: a unit that reaches this script
# with the wrong shape is a unit somebody edited, and a skip would read as a
# quiet day. Third, and the reason for the EXIT trap below: `set -e` ends a
# script with the failing command's own status, which is 1 for almost
# everything — DISCARD's value, inside the skip range — so a bug that killed
# this script half-way would read as a quiet day, forever. Every deliberate
# exit therefore goes through `finish`, and an exit that did not is turned
# into a HOLD by the trap, with the status it died with in the line.
#
# Why a monotonic timer still needs a guard. solador-agent-update.timer is
# OnActiveSec=24h + OnUnitActiveSec=24h, and CLOCK_MONOTONIC pauses through
# suspend, so in ordinary operation a wake finds no elapsed deadline. A source
# trace of systemd's timer.c (#411) found the exception: a `daemon-reload`
# after the timer's first day (every install.sh re-run does one) re-bases the
# one-shot OnActiveSec= and leaves it enabled, and the CLOCK_REALTIME change a
# resume delivers then recomputes that deadline from the timer's original
# activation — in the past — so the next resume fires the job once, seconds
# after wake. Not observed on a real suspend-capable manager; nil on a server
# that never sleeps; real on an opted-in laptop. A check fired at wake is
# exactly the coalesced catch-up the decision forbids, and on a machine whose
# network is not back yet it is also a failed attempt that costs the day. So
# the guard applies the launcher's two rules to every firing, however the
# manager arrived at it:
#
#   1. Not within WAKE_SETTLE_SECS of the last resume — or of the boot, on a
#      boot that has not slept. A firing delivered because a deadline was
#      recomputed at wake arrives seconds after it; a scheduled one almost
#      never does, and the one that does is discarded, as the decision says.
#   2. Not within MIN_INTERVAL_SECS of the last attempt, recorded in a
#      one-line stamp of the macOS launcher's format at the same path
#      (~/.config/solador-agent-update.last-attempt; this guard writes it
#      mode 0600, which the launcher does not). Belt to the
#      first rule's braces: two firings closer together than the interval —
#      a re-enable, a manual start, a manager behaviour this trace missed —
#      run `update` once. The stamp is written BEFORE the attempt, so a
#      failed update is not retried until tomorrow either.
#
# Where "seconds since the last resume" comes from, and the evidence that the
# reading works unprivileged on a real kernel (Fedora CoreOS 41, kernel
# 6.12.13, systemd 256, uid 501 with its own user manager, in a podman
# machine VM — which proves the READING and cannot prove a suspend):
#
#   * The moment of this activation, CLOCK_MONOTONIC in microseconds:
#         systemctl --user show -p InactiveExitTimestampMonotonic --value %n
#     The user manager stamps it when the unit leaves inactive, before it
#     spawns ExecCondition=: read from inside the condition it was 116500920
#     against a /proc/uptime of 116.52 read a moment later. Nothing else
#     unprivileged yields CLOCK_MONOTONIC — /proc/uptime is CLOCK_BOOTTIME
#     (it counts through suspend, which is the whole difference), the
#     /proc/timer_list dump is root-only, and `date` has no monotonic clock.
#   * The moment of the last resume, on the same clock — WHEN THE MANAGER
#     STILL HAS IT, which on a stock distribution it mostly does not:
#         systemctl show -p InactiveEnterTimestampMonotonic --value sleep.target
#     A read-only property fetch from the SYSTEM manager over the system bus,
#     which every user may make (no root, no polkit prompt; observed as uid
#     501). Every systemd sleep path — suspend, hibernate, hybrid-sleep,
#     suspend-then-hibernate, from `systemctl suspend`, logind's lid
#     handling or a desktop's power menu — pulls in sleep.target and, being
#     StopWhenUnneeded=, stops it once systemd-*.service returns, i.e. after
#     the resume. While that unit object is still loaded, the timestamp is
#     the resume. But nothing else references sleep.target on stock
#     Debian/Ubuntu/Fedora (`list-dependencies --reverse --all` is empty),
#     so the manager garbage-collects the object once the cycle ends, and a
#     later `show` loads it fresh from disk and prints 0 — measured on the
#     systemd 256 VM above: a transient unit with Wants=sleep.target reached
#     and stopped the target at monotonic 28446.57 s by the journal's own
#     account, and `show` read InactiveEnterTimestampMonotonic=0 in the same
#     second (and a firing seconds after wake races that collection). 0 is also
#     what a boot that has not slept prints (observed), and what a masked or
#     absent sleep.target prints (a not-found unit prints 0 and exits 0;
#     observed). So this reading is ADVISORY: used when it is there, and
#     otherwise declared unavailable — never held on, because "unavailable"
#     is the normal state of every laptop after its first suspend, and a
#     guard that holds on the normal state is a job that never runs.
#   * The kernel's own count of completed suspends this boot:
#         /sys/power/suspend_stats/success      (mode 0444; kernel >= 5.4)
#     is what tells the two zeros apart: a zero from the manager with a zero
#     (or absent) counter is a boot that has not slept, and the settle rule
#     counts from the boot; a zero from the manager with a counter above
#     zero is a resume the manager no longer has, and the guard says so and
#     falls through to the interval rule. Never a stand-in for the
#     timestamp — a counter says that a suspend happened, not when, and a
#     laptop that sleeps nightly would discard every daily firing on it.
#     Absent (no CONFIG_PM_SLEEP, or a kernel older than 5.4) it is not
#     consulted; present and unreadable it is a hold, because that is a
#     file the kernel keeps and the line names it.
#
#   What the fall-through costs, and why that is the right price: the
#   recorded decision — daily, no catch-up — is carried entirely by the
#   interval rule, which needs no resume at all. The settle rule only adds
#   "not while the network is still coming back after a wake"; without it,
#   a firing that lands in that window is one attempt on a bad day, which
#   the stamp then charges to that day. Nothing is staged by it — `update`
#   verifies before it writes a byte.
#   * The wall clock, `date +%s`, for the interval rule and the stamp: the
#     same clock, the same stamp format and path, and the same 23 h as the
#     macOS launcher, so the two guards answer the same question the same
#     way. Every value read here goes into arithmetic as `10#…`: a
#     hand-edited stamp of `09` is otherwise an octal error that ends the
#     script under `set -e` with status 1 — a clean skip with no log line.
#
# Out, deliberately: the system journal (`journalctl -b -u
# systemd-suspend.service` would survive the collection above, but it is
# readable only in the systemd-journal/adm/wheel groups — which a dedicated
# service user on a headless host is not in — so it would be the same
# job-that-never-runs with a different journal line, over four sleep units)
# and logind's D-Bus (PrepareForSleep is a signal, which needs a listener,
# and there is no "last resume" property to read).
#
# What this reads and does NOT do: no env file (the updater reads it by
# itself; the token never enters this process), no export, no request, no
# touch of solador-agent.service. It resolves `bash`, `systemctl`, `date`
# and the coreutils it uses on the unit's PATH, which the oneshot pins to the
# system directories (the macOS updater plist's decision, #394: not
# ~/.local/bin — the directory the installer writes to — in front of a
# process that decides whether a binary is renamed over the service), and
# reads $HOME for the stamp, as the updater reads it for the env file.
# SOLADOR_AGENT_UPDATE_GUARD_ROOT is the test harness's seam: when set, the
# sysfs counter is read under that prefix instead of /, so the suite never
# reads this machine's kernel. The oneshot UnsetEnvironment=s it, so under
# the unit it is never set; it is not a privilege boundary either way — the
# environment here is the user's own manager's.
#
# Hand-inspecting on a host:
#   systemctl --user status solador-agent-update.service    # "Skipped due to 'exec-condition'" is a discard; "failed" + a HELD line below is a hold; "failed" + an updater exit 3 means check solador-agent.service
#   journalctl --user -u solador-agent-update -g 'solador-agent-update-guard:' -n 20   # this guard's lines
#   systemctl --user reset-failed solador-agent-update.service   # after fixing what a hold named
#   cat ~/.config/solador-agent-update.last-attempt          # epoch seconds of the last attempt
# An operator who wants a check NOW runs `solador-agent update` directly,
# which this guard does not cover (a `systemctl --user start` of the oneshot
# goes through it).

set -eu

HOLD_EXIT=255
DISCARD_EXIT=1
WAKE_SETTLE_SECS=300
MIN_INTERVAL_SECS=82800   # 23 h: a full day less the drift a timer firing can carry
SLEEP_UNIT="sleep.target"
SUSPEND_COUNTER="${SOLADOR_AGENT_UPDATE_GUARD_ROOT:-}/sys/power/suspend_stats/success"

# One line per decision, to stdout, which the unit's journal keeps with its
# own timestamp; the prefix is what `journalctl` readers grep for, and the
# word after it — RUN, DISCARD, NOTE, HELD — is what they count. `|| :`
# because this runs under `set -e` on the way to an exit: a stdout that
# cannot be written (journald gone, IgnoreSIGPIPE= is the default) would
# otherwise end `hold` at the printf with the builtin's status 1 — the
# discard — before it reached `finish`.
log() {
    printf 'solador-agent-update-guard: %s\n' "$*" || :
}

# Every deliberate exit sets `decided` first; the trap turns any other end
# of this script — a `set -e` death, a bug — into a HOLD carrying the status
# it died with, because that status (1 for nearly everything) is otherwise
# read by the manager as a decided discard.
decided=0
finish() {
    decided=1
    exit "$1"
}
on_exit() {
    local status=$?
    if [ "$decided" -ne 1 ]; then
        log "HELD — the guard ended with status $status before reaching a decision (a bug, or a command it needs failing where nothing expected it); a skip nobody decided would read as a quiet day. Exit $HOLD_EXIT: the unit is failed until its next activation or a reset-failed."
        decided=1
        exit "$HOLD_EXIT"
    fi
}
trap on_exit EXIT

hold() {
    log "HELD — $* Exit $HOLD_EXIT: the unit is failed until its next activation or a reset-failed."
    finish "$HOLD_EXIT"
}

is_count() {
    case "${1:-}" in
        '' | *[!0-9]*) return 1 ;;
    esac
}

# ---- the shape of the invocation ----------------------------------------------
if [ "$#" -ne 1 ]; then
    hold "usage: $0 <unit name> — systemd passes %n; got $# argument(s). Run \`solador-agent update\` to check by hand."
fi
unit="$1"
case "$unit" in
    *.service) ;;
    *) hold "the argument '$unit' is not a .service unit name; expected %n of solador-agent-update.service." ;;
esac
case "$unit" in
    *[!A-Za-z0-9_.@:-]*) hold "the unit name '$unit' carries a character systemd would not put in %n." ;;
esac
if [ -z "${INVOCATION_ID:-}" ]; then
    hold "not running under a systemd unit (INVOCATION_ID is unset), so the activation time it needs does not exist. Run \`solador-agent update\` to check by hand."
fi
if [ -z "${HOME:-}" ] || [ ! -d "$HOME" ]; then
    hold "HOME is '${HOME:-<unset>}', which is not a directory; the stamp lives under it."
fi
stamp_file="$HOME/.config/solador-agent-update.last-attempt"

# ---- the clocks ---------------------------------------------------------------
# `|| true` on every read: under `set -e` a command substitution that fails
# ends the script with no log line, and the validation below is what decides.
now="$(date +%s 2>/dev/null || true)"
if ! is_count "$now"; then
    hold "cannot read the clock (date +%s said '${now:-<nothing>}'); not running an update it cannot place in time."
fi

# This activation, CLOCK_MONOTONIC µs, as the user manager recorded it. 0 is
# "the manager does not show $unit activating" — a hand run with
# INVOCATION_ID exported, or a stale unit object — and is a hold, not "now".
# `if x=$(cmd 2>&1)` keeps the command's own words for the hold line without
# a temp file: on success the value is its stdout, on failure its stderr.
if mono_now_us="$(systemctl --user show -p InactiveExitTimestampMonotonic --value "$unit" 2>&1)"; then
    if ! is_count "$mono_now_us" || [ "$((10#$mono_now_us))" -eq 0 ]; then
        hold "cannot read this activation's time from the user manager (systemctl --user show -p InactiveExitTimestampMonotonic $unit said '${mono_now_us:-<nothing>}'); cannot tell a scheduled firing from a wake-time one."
    fi
else
    hold "cannot read this activation's time from the user manager (systemctl --user show -p InactiveExitTimestampMonotonic $unit failed: '${mono_now_us:-<nothing>}'); cannot tell a scheduled firing from a wake-time one."
fi

# The kernel's count of suspends this boot. Read first, because it is what
# tells a manager's 0 apart (see the header); absent it is not consulted.
suspends=""
if [ -e "$SUSPEND_COUNTER" ]; then
    suspends="$(head -n1 "$SUSPEND_COUNTER" 2>/dev/null | tr -d '[:space:]' || true)"
    if ! is_count "$suspends"; then
        hold "cannot read $SUSPEND_COUNTER (got '${suspends:-<nothing>}'); it exists, so the kernel keeps the count, and without it a 0 from the system manager cannot be told from a forgotten resume."
    fi
fi

# The last resume, same clock, from the system manager — ADVISORY. The
# outcome is one of three: `resume_us` set (a resume the manager still
# has, or 0 for a boot that has not slept, told apart by `booted`), or
# empty with `resume_note` saying why the host cannot place its last resume
# in time. Nothing on this path holds; the header says why.
resume_us=""
resume_note=""
booted=0
if resume_raw="$(systemctl show -p InactiveEnterTimestampMonotonic --value "$SLEEP_UNIT" 2>&1)"; then
    if ! is_count "$resume_raw"; then
        resume_note="the system manager answered '${resume_raw:-<nothing>}' for $SLEEP_UNIT's InactiveEnterTimestampMonotonic"
    elif [ "$((10#$resume_raw))" -eq 0 ]; then
        if [ -n "$suspends" ] && [ "$((10#$suspends))" -gt 0 ]; then
            resume_note="the kernel counts $suspends completed suspend(s) this boot but the system manager reads 0 for $SLEEP_UNIT — it forgets that unit's timestamps once a sleep cycle ends"
        else
            resume_us=0
            booted=1
        fi
    elif [ "$((10#$resume_raw))" -gt "$((10#$mono_now_us))" ]; then
        resume_note="the system manager's last resume (${resume_raw}µs) is later than this activation (${mono_now_us}µs) on the same clock, so the reading is not usable"
    else
        resume_us="$resume_raw"
    fi
else
    resume_note="the system manager could not be asked (systemctl show -p InactiveEnterTimestampMonotonic $SLEEP_UNIT failed: '${resume_raw:-<nothing>}')"
fi

since_resume=""
if [ -n "$resume_us" ]; then
    since_resume=$(( (10#$mono_now_us - 10#$resume_us) / 1000000 ))
    if [ "$since_resume" -lt "$WAKE_SETTLE_SECS" ]; then
        if [ "$booted" -eq 1 ]; then
            log "DISCARD — the system booted ${since_resume}s ago and the kernel counts no suspend since; a check within ${WAKE_SETTLE_SECS}s of a boot is a missed interval being made up, which the daily/no-catch-up cadence discards. Not running it; the next scheduled firing is a day away."
        else
            log "DISCARD — the system resumed ${since_resume}s ago; a check within ${WAKE_SETTLE_SECS}s of a wake is a missed interval being made up, which the daily/no-catch-up cadence discards. Not running it; the next scheduled firing is a day away."
        fi
        finish "$DISCARD_EXIT"
    fi
else
    log "NOTE — the last resume is not available on this host ($resume_note); the 23 h interval rule alone governs this firing."
fi

# ---- the last attempt ---------------------------------------------------------
last=""
since_last=""
if [ -e "$stamp_file" ]; then
    last="$(head -n1 "$stamp_file" 2>/dev/null | tr -d '[:space:]' || true)"
    if ! is_count "$last"; then
        hold "$stamp_file does not hold a timestamp; cannot tell when the last attempt was, so not running this check. Remove that file to resume unattended checks."
    fi
    since_last=$((10#$now - 10#$last))
    if [ "$since_last" -lt 0 ]; then
        # This guard only ever writes `now`, so a stamp ahead of the clock
        # is a clock that stepped back (NTP, a restored snapshot) or a hand
        # edit. Inside one interval the clock catches up and the discard
        # resolves itself; beyond it the file would discard every firing
        # until the calendar reached it, which is a hold's job to name.
        if [ "$((0 - since_last))" -gt "$MIN_INTERVAL_SECS" ]; then
            hold "$stamp_file records an attempt $((0 - since_last))s in the future (recorded $last, now $now), more than one interval ahead; cannot tell when the last attempt was, so not running this check. Remove that file to resume unattended checks."
        fi
        log "DISCARD — the clock has moved backwards since the last attempt (recorded $last, now $now); not running this check."
        finish "$DISCARD_EXIT"
    fi
    if [ "$since_last" -lt "$MIN_INTERVAL_SECS" ]; then
        log "DISCARD — the last attempt was ${since_last}s ago and the interval is ${MIN_INTERVAL_SECS}s; not running this check (a second firing inside the interval is a made-up one, and the cadence discards it)."
        finish "$DISCARD_EXIT"
    fi
fi

# Stamped before the attempt, through a sibling and a rename, mode 0600 from
# creation (umask in a subshell, so nothing else here inherits it): a failed
# update is still an attempt, and a crash between the two writes must not
# leave a half-written file that holds every later run.
rm -f "$stamp_file.new" 2>/dev/null || true
if ! write_err="$( { ( umask 077 && printf '%s\n' "$now" > "$stamp_file.new" ) && mv -f "$stamp_file.new" "$stamp_file"; } 2>&1 )"; then
    rm -f "$stamp_file.new" 2>/dev/null || true
    hold "could not write $stamp_file (${write_err:-no error text}); not running an update whose attempt could not be recorded. Check that $(dirname "$stamp_file") is writable by $(id -un 2>/dev/null || echo "this user")."
fi
if [ -z "$resume_us" ]; then
    log "RUN — letting $unit run: last resume not available on this host${last:+, last attempt ${since_last}s ago}."
elif [ "$booted" -eq 1 ]; then
    log "RUN — letting $unit run: no suspend counted this boot, up ${since_resume}s${last:+, last attempt ${since_last}s ago}."
else
    log "RUN — letting $unit run: last resume ${since_resume}s ago${last:+, last attempt ${since_last}s ago}."
fi
finish 0
