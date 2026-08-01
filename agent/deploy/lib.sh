#!/usr/bin/env bash
#
# Shared helpers for the DevCanopy agent deploy scripts (install.sh, redeploy.sh).
#
# Sourced, never executed on its own. Nothing in here prints the bearer token.
#
# The reason this file exists: both scripts must answer the same question after
# restarting the unit — "is the version now being *served* the version we just
# built?" A unit that is `active (running)` proves only that *a* binary is up,
# not that it is this one. Any failure that leaves the old binary in place
# (a swap into the wrong prefix, a stale ExecStart, a future script regression)
# looks identical to success unless the served version is compared to source.

# ---- crate version ----------------------------------------------------------
# Print the `[package]` version from a Cargo.toml.
#
# Section-aware on purpose: a bare `grep '^version'` also matches the version
# inside a `[dependencies.foo]` block, so a manifest that grows one would start
# asserting some dependency's number against the agent's /v1/health.
crate_version() {
    local manifest="$1"
    [ -f "$manifest" ] || return 1
    awk '
        # A new table header ends the [package] section (and may start it).
        /^[[:space:]]*\[/ {
            in_pkg = ($0 ~ /^[[:space:]]*\[package\][[:space:]]*$/)
            next
        }
        in_pkg && /^[[:space:]]*version[[:space:]]*=/ {
            if (match($0, /"[^"]*"/)) {
                print substr($0, RSTART + 1, RLENGTH - 2)
                exit
            }
        }
    ' "$manifest"
}

# ---- health endpoint --------------------------------------------------------
# Build the URL to probe from the agent's configured bind address.
#
# The bind is whatever DEVCANOPY_AGENT_BIND resolved to at install time — a
# tailnet IPv4 by default, but the operator can opt into a wildcard behind a
# firewall. A wildcard is not an address you can dial, so probe loopback there;
# an IPv6 literal needs brackets before it is a legal URL host.
health_url() {
    local bind="${1:-}" port="${2:-7878}" host
    case "$bind" in
        "" | 0.0.0.0) host="127.0.0.1" ;;
        "::" | "[::]") host="[::1]" ;;
        *:*) host="[${bind}]" ;;
        *) host="$bind" ;;
    esac
    printf 'http://%s:%s/v1/health\n' "$host" "${port:-7878}"
}

# Pull the `version` field out of a /v1/health body without assuming jq is
# installed on the host. Prints nothing if the body has no version field.
health_version() {
    printf '%s' "${1:-}" \
        | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p'
}

# ---- health verification ----------------------------------------------------
# Poll /v1/health (bind/port/token read from the env file) until it answers,
# then optionally assert the version it reports.
#
#   verify_health <env-file> <expected-version>   # require an exact match
#   verify_health <env-file> ""                   # only require it to answer
#
# The empty form is for rollback, where the .prev binary's version can't be
# known statically (the agent has no --version flag). Never prints the token.
#
# Polls once a second, 15 times by default; VERIFY_HEALTH_ATTEMPTS overrides that
# (it exists so the failure paths can be exercised without a 15s wait).
verify_health() {
    local env_file="$1" expected_version="${2:-}"
    [ -f "$env_file" ] || { echo "ERROR: env file $env_file not found; cannot verify." >&2; return 1; }

    local token bind port url
    token="$(grep -E '^DEVCANOPY_AGENT_TOKEN=' "$env_file" | head -n1 | cut -d= -f2-)"
    bind="$(grep -E '^DEVCANOPY_AGENT_BIND=' "$env_file" | head -n1 | cut -d= -f2-)"
    port="$(grep -E '^DEVCANOPY_AGENT_PORT=' "$env_file" | head -n1 | cut -d= -f2-)"
    url="$(health_url "$bind" "$port")"

    if [ -z "$token" ]; then
        echo "ERROR: no DEVCANOPY_AGENT_TOKEN in $env_file; cannot verify." >&2
        return 1
    fi

    if [ -n "$expected_version" ]; then
        echo "==> Verifying $url reports version $expected_version ..."
    else
        echo "==> Verifying $url is back online ..."
    fi

    local attempt body got
    body=""
    got=""
    for attempt in $(seq 1 "${VERIFY_HEALTH_ATTEMPTS:-15}"); do
        body="$(curl -fsS -H "Authorization: Bearer ${token}" "$url" 2>/dev/null || true)"
        if [ -n "$body" ]; then
            got="$(health_version "$body")"
            if [ -z "$expected_version" ]; then
                echo "==> Health OK: agent online, reports version ${got:-unknown}"
                return 0
            fi
            if [ "$got" = "$expected_version" ]; then
                echo "==> Health OK: agent reports version $got"
                return 0
            fi
            if [ -n "$got" ]; then
                echo "    attempt $attempt: agent reports $got (want $expected_version), retrying..."
            fi
        fi
        sleep 1
    done

    if [ -z "$expected_version" ]; then
        echo "ERROR: /v1/health did not come back online within timeout." >&2
        echo "       Last response: ${body:-<no response>}" >&2
        return 1
    fi

    if [ -n "$got" ]; then
        # The damning case: the agent is up and healthy, and serving the wrong
        # code. Name both numbers so the operator doesn't have to go find them.
        echo "ERROR: VERSION MISMATCH — the running agent is not the binary just built." >&2
        echo "         served (per $url): $got" >&2
        echo "         built  (per Cargo.toml): $expected_version" >&2
        echo "       A stale binary is still serving. Check that the unit's ExecStart" >&2
        echo "       points at the path the binary was installed to:" >&2
        echo "         systemctl --user cat devcanopy-agent | grep ExecStart" >&2
    else
        echo "ERROR: /v1/health did not report version $expected_version within timeout." >&2
        echo "       Last response: ${body:-<no response>}" >&2
    fi
    return 1
}
