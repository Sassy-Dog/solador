#!/bin/bash
#
# launchd launcher for the Solador metrics agent (macOS, #392).
#
# Installed by deploy/install.sh as ~/.local/bin/solador-agent-launchd and named
# in app.solador.agent.plist's ProgramArguments as:
#
#   <launcher> <path-to-solador-agent> <path-to-solador-agent.env> <log file>
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

if [ "$#" -ne 3 ]; then
    log "usage: $0 <solador-agent binary> <env file> <log file>"
    exit 2
fi

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
