#!/bin/sh
#
# The remote half of `solador-app host add --ssh` (crates/hostsetup/src/remote.rs).
#
# The command embeds this file, sends it on ssh's stdin and runs it there as
#
#   ssh -o BatchMode=yes -o StrictHostKeyChecking=yes -o ConnectTimeout=15 \
#       -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -T -- <target> /bin/sh -s -- <mode> [flags]
#
# so the host's login shell only ever parses fixed words, and nothing is left
# on the host beyond what a mode deliberately writes. It runs as the ssh user
# and never asks for sudo: a step only root can take is printed, not taken.
#
# Modes:
#   discover   key=value lines describing this user's agent install. Never the
#              token: only whether there is one.
#   rebind     agent/README.md "Reaching a Tailscale host from the LAN too",
#              steps 2 and 3: SOLADOR_AGENT_BIND=0.0.0.0 in the env file
#              (rewritten 0600 as its owner; only the BIND and BIND_AUTO lines
#              go, the token and every other line are kept), then a restart.
#              Refused while TLS is off (in the env file or, for a release that
#              predates it, in the binary) or the host is held to its tailnet.
#              Prints previous_bind= and previous_bind_auto= first and
#              rewritten=yes once the file is replaced. A system daemon only
#              root can restart: exit 3, and the command says which step.
#   install    bootstrap.sh from main, run with stdin closed, so a fresh install
#              generates its own token on the host. Flags: [--ref <40-hex sha>]
#              [--enable-tls] [--bind-all] (the last sets SOLADOR_AGENT_BIND=0.0.0.0
#              in the installer's environment, which it writes as an explicit bind).
#   collect    The one mode that prints the token, as a single token= line, for
#              the command to capture; it is never echoed on its side.
#
# Exit: 0 done, 1 refused or failed (the reason on stderr), 2 usage, 3 a root
# step is owed (the system daemon's restart).

set -u
# A non-login shell on macOS has no Homebrew on PATH, which is where minisign
# lives. Appended, never prepended: whatever the host already resolves wins.
PATH="$PATH:/opt/homebrew/bin:/usr/local/bin"
export PATH

# Where agent/deploy/install.sh puts an agent. agent/deploy/lib_test.sh holds
# each of these lines to install.sh's own, word for word.
LABEL=app.solador.agent
UNIT=solador-agent.service
ENV_FILE="$HOME/.config/solador-agent.env"
BIN="$HOME/.local/bin/solador-agent"
UNIT_FILE="$HOME/.config/systemd/user/$UNIT"
AGENT_PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
DAEMON_PLIST="/Library/LaunchDaemons/$LABEL.plist"
TAILSCALE_APP="/Applications/Tailscale.app/Contents/MacOS/Tailscale"
BOOTSTRAP_URL=https://raw.githubusercontent.com/Sassy-Dog/solador/main/agent/deploy/bootstrap.sh

die() {
    echo "$*" >&2
    exit 1
}

# One key's value, read the way agent/deploy/lib.sh's env_value reads it (and so
# the way the service does): the LAST matching line, a trailing CR dropped,
# surrounding whitespace trimmed, one matching pair of quotes removed. Never
# sourced.
env_value() {
    [ -f "$ENV_FILE" ] || return 0
    awk -v k="$1=" -v q="'" '
        index($0, k) == 1 { v = substr($0, length(k) + 1); found = 1 }
        END {
            if (!found) exit
            sub(/\r$/, "", v); sub(/^[ \t]+/, "", v); sub(/[ \t]+$/, "", v)
            f = substr(v, 1, 1); l = substr(v, length(v), 1)
            if (length(v) >= 2 && f == l && (f == "\"" || f == q)) v = substr(v, 2, length(v) - 2)
            print v
        }' "$ENV_FILE"
}

# The UserName a system daemon's plist names, read from the XML the installer
# renders (and copies into place unchanged).
daemon_user() {
    [ -r "$DAEMON_PLIST" ] || return 0
    awk '/<key>UserName<\/key>/ { getline; gsub(/.*<string>|<\/string>.*/, ""); print; exit }' "$DAEMON_PLIST"
}

# Which user a running agent belongs to, when one is running at all. Read as a
# uid and then named, because procps cuts its user column to 8 characters
# (sassydog-agents reads "sassydo+"), which is not a user anyone can ssh as.
running_as() {
    uid=$(ps -A -o uid= -o comm= 2>/dev/null |
        awk '{ n = $2; sub(/.*\//, "", n); if (n == "solador-agent") { print $1; exit } }')
    [ -n "$uid" ] || return 0
    id -un "$uid" 2>/dev/null ||
        getent passwd "$uid" 2>/dev/null | cut -d: -f1 | grep . ||
        echo "uid $uid"
}

# The tailnet address the installer would bind, found the way install.sh's
# detect_tailscale_ip finds it: the first of the tailscale CLI on PATH, the CLI
# the Mac app bundles, and the tailscale0 interface that answers. A CLI that
# exits non-zero (logged out) is no answer, because install.sh runs under
# pipefail; one that succeeds with nothing to say is, and ends the search.
# This script cannot rely on pipefail in /bin/sh, so it keeps the status by
# capturing the output first. agent/deploy/lib_test.sh runs the two side by
# side.
tailnet_ip() {
    if command -v tailscale >/dev/null 2>&1 && ip4="$(tailscale ip -4 2>/dev/null)"; then
        printf '%s\n' "$ip4" | head -n1
        return 0
    fi
    if [ "$(uname -s)" = "Darwin" ] && [ -x "$TAILSCALE_APP" ] &&
        ip4="$("$TAILSCALE_APP" ip -4 2>/dev/null)"; then
        printf '%s\n' "$ip4" | head -n1
        return 0
    fi
    if command -v ip >/dev/null 2>&1; then
        ip -4 -o addr show tailscale0 2>/dev/null |
            grep -oE 'inet [0-9.]+' | awk '{print $2}' | head -n1
    fi
}

# One line of `key=value`, the value cut to its first line.
kv() {
    printf '%s=%s\n' "$1" "$(printf '%s' "$2" | head -n 1 | tr -d '\r')"
}

have() {
    if command -v "$1" >/dev/null 2>&1; then kv "have_$1" yes; else kv "have_$1" no; fi
}

discover() {
    os=$(uname -s)
    kv os "$os"
    kv hostname "$(hostname 2>/dev/null)"
    kv user "$(id -un)"
    if [ -f "$ENV_FILE" ]; then kv env present; else kv env absent; fi
    kv tls "$(env_value SOLADOR_AGENT_TLS)"
    kv bind "$(env_value SOLADOR_AGENT_BIND)"
    kv bind_auto "$(env_value SOLADOR_AGENT_BIND_AUTO)"
    kv require_tailnet "$(env_value SOLADOR_AGENT_REQUIRE_TAILNET)"
    kv port "$(env_value SOLADOR_AGENT_PORT)"
    if [ -n "$(env_value SOLADOR_AGENT_TOKEN)" ]; then kv token present; else kv token absent; fi
    if [ -x "$BIN" ]; then
        kv bin present
        kv version "$("$BIN" --version 2>/dev/null)"
        # The certificate exists once a TLS agent has started; exit 2 is a
        # release from before TLS, which does not know the subcommand.
        fingerprint=$("$BIN" tls-fingerprint 2>/dev/null)
        case $? in
            0) kv fingerprint "$fingerprint" ;;
            2) kv fingerprint_status unsupported ;;
            *) kv fingerprint_status missing ;;
        esac
    else
        kv bin absent
    fi
    case "$os" in
        Linux)
            if [ -f "$UNIT_FILE" ]; then kv service systemd-user; else kv service none; fi
            ;;
        Darwin)
            if [ -f "$AGENT_PLIST" ]; then
                kv service launch-agent
            elif [ -n "$(daemon_user)" ] && [ "$(daemon_user)" = "$(id -un)" ]; then
                kv service launch-daemon
            else
                kv service none
            fi
            kv daemon_user "$(daemon_user)"
            if launchctl print "gui/$(id -u)" >/dev/null 2>&1; then kv gui yes; else kv gui no; fi
            ;;
    esac
    kv running_as "$(running_as)"
    kv tailnet_ip "$(tailnet_ip)"
    have minisign
    have curl
    have bash
}

restart() {
    case "$(uname -s)" in
        Linux)
            systemctl --user restart "$UNIT" || die "systemctl --user restart $UNIT failed"
            ;;
        Darwin)
            if [ -f "$AGENT_PLIST" ]; then
                launchctl kickstart -k "gui/$(id -u)/$LABEL" || die "launchctl kickstart gui/$(id -u)/$LABEL failed"
            else
                # A LaunchDaemon belongs to the system domain: only root can
                # restart it, so the step is handed back rather than taken.
                exit 3
            fi
            ;;
        *) die "unsupported platform: $(uname -s)" ;;
    esac
}

rebind() {
    [ -f "$ENV_FILE" ] || die "no agent env file at $ENV_FILE"
    [ "$(env_value SOLADOR_AGENT_TLS)" = 1 ] ||
        die "TLS is off on this agent: an unpaired host off the tailnet is never sent its token, so enable TLS first (--enable-tls)."
    [ "$(env_value SOLADOR_AGENT_REQUIRE_TAILNET)" = 1 ] &&
        die "SOLADOR_AGENT_REQUIRE_TAILNET=1 holds this agent to its tailnet; that is someone's decision, so it is not removed here."
    # The env file can say TLS=1 beside a release that predates it (exit 2:
    # it does not know the subcommand), which would serve plain HTTP on every
    # interface once rebound.
    if [ -x "$BIN" ]; then
        "$BIN" tls-fingerprint >/dev/null 2>&1
        [ "$?" -ne 2 ] ||
            die "The agent installed here predates TLS, although $ENV_FILE says SOLADOR_AGENT_TLS=1: re-run its installer with --enable-tls first."
    fi
    kv previous_bind "$(env_value SOLADOR_AGENT_BIND)"
    kv previous_bind_auto "$(env_value SOLADOR_AGENT_BIND_AUTO)"
    # BIND_AUTO goes with BIND: it would have the next installer run resolve
    # the bind again, back to the tailnet address. REQUIRE_TAILNET stays,
    # whatever its value: =1 was refused above, and anything else is inert.
    (
        umask 077
        grep -v -E '^SOLADOR_AGENT_(BIND|BIND_AUTO)=' "$ENV_FILE" >"$ENV_FILE.new" &&
            echo SOLADOR_AGENT_BIND=0.0.0.0 >>"$ENV_FILE.new" &&
            mv "$ENV_FILE.new" "$ENV_FILE"
    ) || die "could not rewrite $ENV_FILE"
    kv rewritten yes
    restart
}

install_agent() {
    ref=""
    enable_tls=""
    bind_all=""
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --ref)
                [ "$#" -ge 2 ] || die "install: --ref needs a value"
                case "$2" in
                    *[!0-9a-f]*) die "install: --ref must be a 40-hex commit" ;;
                esac
                [ "${#2}" -eq 40 ] || die "install: --ref must be a 40-hex commit"
                ref=$2
                shift
                ;;
            --enable-tls) enable_tls=1 ;;
            --bind-all) bind_all=1 ;;
            *)
                echo "install: unexpected $1" >&2
                exit 2
                ;;
        esac
        shift
    done
    dir="$HOME/.cache/solador-cli"
    (umask 077 && mkdir -p "$dir" && chmod 700 "$dir") || die "could not create $dir"
    curl -fsSL --connect-timeout 15 --max-time 120 -o "$dir/bootstrap.sh" "$BOOTSTRAP_URL" ||
        die "could not download $BOOTSTRAP_URL"
    set --
    [ -n "$ref" ] && set -- "$@" --ref "$ref"
    [ -n "$enable_tls" ] && set -- "$@" --enable-tls
    # The installer reads these ahead of the env file, and a login shell's rc
    # file may export any of them: left alone, one could remove
    # REQUIRE_TAILNET, pick the bind, or move the service to a label this
    # script does not look for, none of which is any flag's to decide.
    unset SOLADOR_AGENT_BIND SOLADOR_AGENT_PORT SOLADOR_AGENT_REQUIRE_TAILNET \
        SOLADOR_AGENT_TLS SOLADOR_AGENT_RELEASE SOLADOR_AGENT_LAUNCHD_LABEL
    # stdin closed: the installer's token prompt reads nothing and generates one.
    if [ -n "$bind_all" ]; then
        env SOLADOR_AGENT_BIND=0.0.0.0 bash "$dir/bootstrap.sh" "$@" </dev/null
    else
        bash "$dir/bootstrap.sh" "$@" </dev/null
    fi
}

collect() {
    token=$(env_value SOLADOR_AGENT_TOKEN)
    [ -n "$token" ] || die "no SOLADOR_AGENT_TOKEN in $ENV_FILE"
    printf 'token=%s\n' "$token"
}

[ "$#" -ge 1 ] || {
    echo "usage: host.sh discover|rebind|install [flags]|collect" >&2
    exit 2
}
mode=$1
shift
# Every mode runs from inside this one compound command, which the shell reads
# off stdin whole before it starts: by the time any child runs, the script has
# been read to its end, so nothing a child reads from stdin can be script.
case "$mode" in
    discover) discover ;;
    rebind) rebind ;;
    install) install_agent "$@" ;;
    collect) collect ;;
    *)
        echo "unknown mode: $mode" >&2
        exit 2
        ;;
esac
