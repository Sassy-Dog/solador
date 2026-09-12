#!/bin/bash
#
# launchd launcher for the Solador metrics agent (macOS, #392) and, since
# #394, for its opt-in unattended update job.
#
# Installed by deploy/install.sh as ~/.local/bin/solador-agent-launchd and named
# in app.solador.agent.plist's ProgramArguments as:
#
#   <launcher> <path-to-solador-agent> <path-to-solador-agent.env> <log file>
#
# and, only on a host that opted in with --enable-timer, in
# app.solador.agent.update.plist's as:
#
#   <launcher> <path-to-solador-agent> <path-to-solador-agent.env> <log file> update
#
# The fourth argument is a positive allow-list of exactly one word. With it the
# launcher runs `solador-agent update` behind the scheduling guard described
# at the bottom of this file, and does NOT export the env file — the updater
# reads it by itself, under the same rules, and the token then reaches one
# place only. Without it the launcher is what it was: the metrics service's
# environment loader. The two paths share the argument checks and the log
# rotation and nothing else; the update path never touches the metrics
# service and the metrics path never reads a clock or a stamp.
#
# Why a launcher exists at all: systemd has EnvironmentFile=, launchd does not.
# Its EnvironmentVariables key would put the bearer token into the plist —
# a second copy of the secret, in a file launchd reads as the user and that
# `launchctl print` echoes back. So the token stays in the one mode-0600 env
# file, this reads it at every start, and rotating it is still "edit the file,
# restart the service" on both platforms.
#
# What this deliberately does NOT do:
#   * `source` the env file. That evaluates its contents as shell, and the
#     token is user-controlled text. The file is parsed line by line and only
#     the keys the agent documents are exported.
#   * Log the token, or any line of the file. A line it does not recognise is
#     reported by number, never by content.
#   * Depend on the source checkout, or take any PATH it needs from $HOME.
#     Every path arrives as an argument, so deleting the clone that ran
#     install.sh does not stop the agent, and launchd's HOME (the user
#     record's) need not be the HOME the installer ran under. $HOME is read
#     for one thing only: extending PATH for the container CLIs that
#     no-admin installs put under the home directory.
#
# /bin/bash rather than /usr/bin/env bash: macOS ships 3.2 at that path on
# every supported release, and launchd starts this with a minimal PATH.

set -eu

# Timestamped, because the sink is a plain file that KeepAlive can fill with
# the same line every three seconds: `tail -n 50` has to be able to say
# whether a loop is now or last Tuesday. /bin/date is on launchd's PATH.
log() {
    printf '%s solador-agent-launchd: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >&2
}

# Three arguments is the metrics service; four, the last of which must be
# the word `update`, is the updater. Anything else — a different word, a
# fifth argument — is refused, not read as "probably the metrics service":
# a plist that got here with the wrong shape is a plist somebody edited.
mode=agent
case "$#" in
    3) ;;
    4)
        if [ "$4" = "update" ]; then
            mode=update
        else
            log "usage: $0 <solador-agent binary> <env file> <log file> [update] — got '$4', which is not 'update'"
            exit 2
        fi
        ;;
    *)
        log "usage: $0 <solador-agent binary> <env file> <log file> [update]"
        exit 2
        ;;
esac

bin="$1"
env_file="$2"
log_file="$3"

# launchd appends to the log file forever and no rotation exists that does not
# need sudo (newsyslog.d), which #392 rules out. So the launcher rotates once,
# at start, when the file has grown past a cap: the previous file becomes
# `.1` (one generation; the old `.1` is gone). This runs BEFORE the launcher's
# own refusals below, because under KeepAlive + a 3 s throttle those refusals
# — a missing env file, say — are exactly what fills the file.
#
# The rename is not enough on its own. launchd opened the log as this
# process's stdout/stderr BEFORE it started, and a rename follows the inode:
# without reopening, every line of this run — the agent's included — would
# keep landing in `.1`, and `solador-agent.log` would not exist until the next
# spawn. So both streams are reopened on the fresh path, and the agent
# inherits those.
#
# `wc -c`, not `stat`: `stat -f` is BSD's format flag and GNU's --file-system
# flag, and this file's test suite runs on both.
LOG_CAP_BYTES=$((10 * 1024 * 1024))
if [ -f "$log_file" ]; then
    size="$(wc -c < "$log_file" 2>/dev/null | tr -d '[:space:]')"
    case "$size" in
        '' | *[!0-9]*) size=0 ;;
    esac
    if [ "$size" -gt "$LOG_CAP_BYTES" ]; then
        mv -f "$log_file" "$log_file.1" 2>/dev/null || true
        exec >>"$log_file" 2>&1
        log "rotated the previous log (${size} bytes) to $log_file.1"
    fi
fi

if [ ! -x "$bin" ]; then
    log "$bin is not an executable binary."
    exit 1
fi
if [ ! -f "$env_file" ]; then
    log "$env_file not found; re-run deploy/install.sh."
    exit 1
fi

# ---- the update job (#394) --------------------------------------------------
# Everything below this block is the metrics path and is not reached in
# update mode; everything inside it is never reached by the metrics path.
#
# The cadence decision on #394 is DAILY, NO CATCH-UP: one check per 24 h
# while the session is up, never one fired at wake, login or boot to make up
# for an interval that fell during sleep, and no coalescing of missed
# firings into one late run. The plist's StartInterval gives the 24 h;
# launchd.plist(5) says a StartInterval firing that falls during sleep is
# missed, and StartCalendarInterval — the key that coalesces missed firings
# into one run at wake — is deliberately not used. This guard is what makes
# the property hold rather than the man page, and it is applied to every
# firing, however launchd arrived at it:
#
#   1. Not within WAKE_SETTLE_SECS of the last wake or the boot (kern.waketime
#      / kern.boottime, whichever is later). A firing launchd delivers because
#      an interval elapsed while the lid was closed arrives seconds after the
#      wake; a firing at a random point in the day almost never does, and the
#      one that does is discarded, as the decision says — tomorrow's is a day
#      away.
#   2. Not within MIN_INTERVAL_SECS of the last attempt, recorded in a stamp
#      beside the env file. Belt to the first rule's braces: a re-bootstrap,
#      a manual `launchctl kickstart`, or any launchd behaviour that delivers
#      two firings closer together than the interval, runs `update` once.
#      The stamp is written BEFORE the attempt, so a failed update is not
#      retried until tomorrow either — no rapid retry loop.
#
# Two kinds of not-running, told apart by exit status because `launchctl
# print` shows nothing else. A DISCARDED firing (too soon after a wake or
# boot, inside the interval, or a clock that went backwards past the stamp
# — all of which tomorrow's firing resolves by itself) is exit 0: the
# deliberate no-op the cadence describes. A HELD check — the clock, the
# wake time or the stamp unreadable, or the stamp unwritable — is exit
# HOLD_EXIT, distinct from every code `solador-agent update` uses (0, 1, 3,
# 4, 5, 75): it does not resolve by itself, so a green "last exit code = 0"
# over it every day would be a fabricated state. Both write one line saying
# why. An operator who wants a check NOW runs `solador-agent update`
# directly, which this guard does not cover.
#
# Hand-inspecting: `launchctl print gui/$(id -u)/app.solador.agent.update`
# shows the last exit status; the log file this job writes to has the
# updater's own lines, or this launcher's one-line reason for not running it.
if [ "$mode" = "update" ]; then
    HOLD_EXIT=6
    WAKE_SETTLE_SECS=300
    MIN_INTERVAL_SECS=82800   # 23 h: a full day less the drift a StartInterval firing can carry
    stamp_file="$(dirname "$env_file")/solador-agent-update.last-attempt"

    is_epoch() {
        case "${1:-}" in
            '' | *[!0-9]*) return 1 ;;
        esac
    }

    # `{ sec = 1789165051, usec = 550946 } Fri Sep 11 16:17:31 2026` is what
    # sysctl prints for both keys; the first integer after `sec =` is the
    # value. Anything else is "unreadable", never "zero".
    sysctl_epoch() {
        sysctl -n "$1" 2>/dev/null | sed -n 's/^{ sec = \([0-9][0-9]*\),.*/\1/p' | head -n1
    }

    # `|| true` on each read: under `set -e` a command substitution that fails
    # ends the script with no log line, and is_epoch is the one that decides.
    now="$(date +%s 2>/dev/null || true)"
    if ! is_epoch "$now"; then
        log "update: HELD — cannot read the clock (date +%s said '${now:-<nothing>}'); not running an update it cannot place in time. Exit $HOLD_EXIT."
        exit "$HOLD_EXIT"
    fi
    woke="$(sysctl_epoch kern.waketime || true)"
    booted="$(sysctl_epoch kern.boottime || true)"
    if ! is_epoch "$woke" || ! is_epoch "$booted"; then
        log "update: HELD — cannot read kern.waketime/kern.boottime (got '${woke:-<nothing>}' / '${booted:-<nothing>}'); cannot tell a scheduled firing from a wake-time one, so not running this check. Exit $HOLD_EXIT."
        exit "$HOLD_EXIT"
    fi
    [ "$booted" -gt "$woke" ] && woke="$booted"
    since_wake=$((now - woke))
    if [ "$since_wake" -lt "$WAKE_SETTLE_SECS" ]; then
        log "update: the system woke or booted ${since_wake}s ago; a check this close to a wake is a missed interval being made up, which the daily/no-catch-up cadence discards. Not running it; the next scheduled firing is a day away."
        exit 0
    fi
    last=""
    since_last=""
    if [ -e "$stamp_file" ]; then
        last="$(head -n1 "$stamp_file" 2>/dev/null | tr -d '[:space:]')"
        if ! is_epoch "$last"; then
            log "update: HELD — $stamp_file does not hold a timestamp; cannot tell when the last attempt was, so not running this check. Remove that file to resume unattended checks. Exit $HOLD_EXIT."
            exit "$HOLD_EXIT"
        fi
        since_last=$((now - last))
        if [ "$since_last" -lt 0 ]; then
            log "update: the clock has moved backwards since the last attempt (recorded $last, now $now); not running this check."
            exit 0
        fi
        if [ "$since_last" -lt "$MIN_INTERVAL_SECS" ]; then
            log "update: the last attempt was ${since_last}s ago and the interval is ${MIN_INTERVAL_SECS}s; not running this check (a second firing inside the interval is a made-up one, and the cadence discards it)."
            exit 0
        fi
    fi
    # Stamped before the attempt, through a sibling and a rename: a failed
    # update is still an attempt, and a crash between the two writes must
    # not leave a half-written file that holds every later run.
    if ! printf '%s\n' "$now" > "$stamp_file.new" 2>/dev/null || ! mv -f "$stamp_file.new" "$stamp_file" 2>/dev/null; then
        log "update: HELD — could not write $stamp_file; not running an update whose attempt could not be recorded. Exit $HOLD_EXIT."
        rm -f "$stamp_file.new" 2>/dev/null || true
        exit "$HOLD_EXIT"
    fi
    log "update: running $bin update (last wake ${since_wake}s ago${last:+, last attempt ${since_last}s ago})"
    # exec, so the exit status launchd records is the updater's own — 0, 1,
    # 3, 4, 5 or 75, exactly as agent/README.md documents them — and not a
    # wrapper's paraphrase of it.
    exec "$bin" update
fi

# The keys agent/README.md documents, and no others. An allow-list: a key the
# agent does not read is not exported into its process for having appeared in
# the file. Values are read the way systemd's EnvironmentFile= reads them —
# a trailing CR, surrounding whitespace and one matching pair of quotes are
# stripped, and the last occurrence of a key wins — so a token rotated by
# hand with `KEY="value"` works on both platforms rather than 401ing on this
# one with nothing in the log to say why.
lineno=0
while IFS= read -r line || [ -n "$line" ]; do
    lineno=$((lineno + 1))
    line="${line%$'\r'}"
    case "$line" in
        '' | '#'*)
            ;;
        SOLADOR_AGENT_TOKEN=* | SOLADOR_AGENT_BIND=* | SOLADOR_AGENT_PORT=* | \
        SOLADOR_AGENT_SKIP_FSTYPES=* | RUST_LOG=*)
            key="${line%%=*}"
            value="${line#*=}"
            # Trim surrounding whitespace.
            value="${value#"${value%%[![:space:]]*}"}"
            value="${value%"${value##*[![:space:]]}"}"
            # One matching pair of quotes, double or single.
            case "$value" in
                \"*\") value="${value#\"}"; value="${value%\"}" ;;
                \'*\') value="${value#\'}"; value="${value%\'}" ;;
            esac
            export "$key=$value"
            ;;
        *)
            log "ignoring unrecognised line $lineno of $env_file"
            ;;
    esac
done < "$env_file"

# The container CLIs that no-admin installs put under the home directory —
# Docker Desktop (~/.docker/bin), OrbStack (~/.orbstack/bin), Rancher Desktop
# (~/.rd/bin) — which the plist cannot name because launchd does not expand
# $HOME. The only use of $HOME in this file.
if [ -n "${HOME:-}" ]; then
    export PATH="$PATH:$HOME/.docker/bin:$HOME/.orbstack/bin:$HOME/.rd/bin"
fi

# A dated launch boundary, so a crash loop of the agent's own undated fatal
# lines in the log can be read against the clock.
log "starting $bin (launcher pid $$)"

# exec, so the pid launchd tracks is the agent's own and KeepAlive restarts the
# agent rather than a dead wrapper.
exec "$bin"
