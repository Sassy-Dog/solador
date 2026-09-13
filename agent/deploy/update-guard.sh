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
#        read, or the stamp could not be written. A hold over a host property
#        (no bus, a masked sleep.target the kernel contradicts) recurs every
#        day, and a green unit over that would be a fabricated state; the
#        journal line names what to fix. This is the macOS launcher's exit 6,
#        in the manager's own vocabulary.
#
# Two more facts of that contract shape what this script may exit with.
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
# quiet day.
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
#   * The moment of the last resume, on the same clock:
#         systemctl show -p InactiveEnterTimestampMonotonic --value sleep.target
#     A read-only property fetch from the SYSTEM manager over the system bus,
#     which every user may make (no root, no polkit prompt; observed as uid
#     501). Every systemd sleep path — suspend, hibernate, hybrid-sleep,
#     suspend-then-hibernate, from `systemctl suspend`, logind's lid
#     handling or a desktop's power menu — pulls in sleep.target and, being
#     StopWhenUnneeded=, stops it once systemd-*.service returns, i.e. after
#     the resume; so this timestamp IS the resume. It is 0 on a boot that has
#     not slept (observed) — and ALSO 0 for a masked or absent sleep.target
#     (a not-found unit prints 0 and exits 0; observed), which is why the
#     next input exists.
#   * The kernel's own count of completed suspends this boot:
#         /sys/power/suspend_stats/success      (mode 0444; kernel >= 5.4)
#     corroborates a zero from the manager: a boot on which the kernel has
#     suspended but the manager recorded no sleep.target cycle is a resume
#     this guard cannot place in time, and it HOLDS rather than reads "never
#     slept" off a masked unit. Never sufficient alone — a counter says that
#     a suspend happened, not when, and a laptop that sleeps nightly would
#     discard every daily firing on it. Absent (no CONFIG_PM_SLEEP, or a
#     kernel older than 5.4) it is not consulted; present and unreadable it
#     is a hold like any other input.
#   * The wall clock, `date +%s`, for the interval rule and the stamp: the
#     same clock, the same stamp format and path, and the same 23 h as the
#     macOS launcher, so the two guards answer the same question the same
#     way. Every value read here goes into arithmetic as `10#…`: a
#     hand-edited stamp of `09` is otherwise an octal error that ends the
#     script under `set -e` with status 1 — a clean skip with no log line.
#
# Out, deliberately: the system journal (readable only in the
# systemd-journal group) and logind's D-Bus (PrepareForSleep is a signal,
# which needs a listener, and there is no "last resume" property to read).
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
# own timestamp; the prefix is what `journalctl` readers grep for.
log() {
    printf 'solador-agent-update-guard: %s\n' "$*"
}

hold() {
    log "HELD — $* Exit $HOLD_EXIT: the unit is failed until its next activation or a reset-failed."
    exit "$HOLD_EXIT"
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
mono_now_us="$(systemctl --user show -p InactiveExitTimestampMonotonic --value "$unit" 2>/dev/null || true)"
if ! is_count "$mono_now_us" || [ "$((10#$mono_now_us))" -eq 0 ]; then
    hold "cannot read this activation's time from the user manager (systemctl --user show -p InactiveExitTimestampMonotonic $unit said '${mono_now_us:-<nothing>}'); cannot tell a scheduled firing from a wake-time one."
fi

# The last resume, same clock, from the system manager. 0 is "this boot has
# not slept", pending the kernel's agreement below.
resume_us="$(systemctl show -p InactiveEnterTimestampMonotonic --value "$SLEEP_UNIT" 2>/dev/null || true)"
if ! is_count "$resume_us"; then
    hold "cannot read the last resume from the system manager (systemctl show -p InactiveEnterTimestampMonotonic $SLEEP_UNIT said '${resume_us:-<nothing>}'); cannot tell a scheduled firing from a wake-time one."
fi

# The kernel's count of suspends this boot corroborates a zero from the
# manager; it never stands in for the timestamp (see the header).
if [ -e "$SUSPEND_COUNTER" ]; then
    suspends="$(head -n1 "$SUSPEND_COUNTER" 2>/dev/null | tr -d '[:space:]' || true)"
    if ! is_count "$suspends"; then
        hold "cannot read $SUSPEND_COUNTER (got '${suspends:-<nothing>}'); it exists, so the kernel keeps the count, and without it a zero from the manager cannot be trusted."
    fi
    if [ "$((10#$suspends))" -gt 0 ] && [ "$((10#$resume_us))" -eq 0 ]; then
        hold "the kernel counts $suspends completed suspend(s) this boot but the system manager recorded no $SLEEP_UNIT cycle (masked, or slept outside systemd?); the last resume cannot be placed in time."
    fi
fi

since_resume=$(( (10#$mono_now_us - 10#$resume_us) / 1000000 ))
if [ "$since_resume" -lt 0 ]; then
    hold "the system manager's last resume (${resume_us}µs) is later than this activation (${mono_now_us}µs) on the same clock; the inputs do not agree."
fi
if [ "$since_resume" -lt "$WAKE_SETTLE_SECS" ]; then
    if [ "$((10#$resume_us))" -eq 0 ]; then
        log "the system booted ${since_resume}s ago; a check this close to a boot is a missed interval being made up, which the daily/no-catch-up cadence discards. Not running it; the next scheduled firing is a day away."
    else
        log "the system resumed ${since_resume}s ago; a check this close to a wake is a missed interval being made up, which the daily/no-catch-up cadence discards. Not running it; the next scheduled firing is a day away."
    fi
    exit "$DISCARD_EXIT"
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
        log "the clock has moved backwards since the last attempt (recorded $last, now $now); not running this check."
        exit "$DISCARD_EXIT"
    fi
    if [ "$since_last" -lt "$MIN_INTERVAL_SECS" ]; then
        log "the last attempt was ${since_last}s ago and the interval is ${MIN_INTERVAL_SECS}s; not running this check (a second firing inside the interval is a made-up one, and the cadence discards it)."
        exit "$DISCARD_EXIT"
    fi
fi

# Stamped before the attempt, through a sibling and a rename, mode 0600 from
# creation (umask in a subshell, so nothing else here inherits it): a failed
# update is still an attempt, and a crash between the two writes must not
# leave a half-written file that holds every later run.
rm -f "$stamp_file.new" 2>/dev/null || true
if ! ( umask 077 && printf '%s\n' "$now" > "$stamp_file.new" ) 2>/dev/null \
    || ! mv -f "$stamp_file.new" "$stamp_file" 2>/dev/null; then
    rm -f "$stamp_file.new" 2>/dev/null || true
    hold "could not write $stamp_file; not running an update whose attempt could not be recorded."
fi
if [ "$((10#$resume_us))" -eq 0 ]; then
    log "letting $unit run: this boot has not slept, up ${since_resume}s${last:+, last attempt ${since_last}s ago}."
else
    log "letting $unit run: last resume ${since_resume}s ago${last:+, last attempt ${since_last}s ago}."
fi
exit 0
