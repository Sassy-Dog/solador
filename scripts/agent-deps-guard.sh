#!/usr/bin/env bash
set -euo pipefail

# A deliberate absence, asserted (#391): the agent must not depend on the
# release tooling in crates/updatefeed. Run by ci.yml's `secrets-guard` job on
# every PR, and by `./dev lint`.
#
# The feed's wire contract is prose the two sides agree on, and the agent
# compiles its own keys in (`agent/src/update.rs`, #393) and verifies with
# `minisign-verify` directly; an edge here would put release tooling into a
# daemon that runs unattended on strangers' hosts, and it is the obvious
# shortcut for whoever next touches the consumer. A doc sentence saying "does
# not depend" rots silently; this does not.
#
# The agent's whole tree is listed and searched, rather than `cargo tree -i`
# asked for the inverse path: `-i` exits 101 with an EMPTY stdout when the
# package is absent, which is the same stdout a cargo failure produces — a
# check that read "nothing printed" as "no edge" would pass on a broken
# lockfile. A cargo failure fails this gate instead. `cargo tree` resolves and
# downloads manifests; it compiles nothing, so no system libraries are needed.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
ROOT_DIR="$( cd "$SCRIPT_DIR/.." && pwd )"

# `--target all`: the agent ships for four triples, and a
# `[target.'cfg(…)'.dependencies]` edge that exists only on Darwin would be
# invisible to a host-only tree on the Linux runner.
if ! agent_tree="$(cargo tree --locked --manifest-path "$ROOT_DIR/Cargo.toml" -p solador-agent --target all --prefix none 2>&1)"; then
    echo "::error::cargo tree failed, so the absence could not be asserted: $agent_tree"
    exit 1
fi
if grep -q '^solador-updatefeed ' <<< "$agent_tree"; then
    echo "::error::agent/ resolves solador-updatefeed — the agent must not depend on release tooling (crates/updatefeed is the feed PRODUCER; the consumer compiles its own key in and verifies with minisign-verify directly)"
    exit 1
fi
# The other absence CLAUDE.md names, asserted the same way now that the
# agent has an HTTP stack and a failure enum that make "report a failed
# update to Sentry" the obvious next shortcut (#393): crates/crashreport is
# the ONLY crate carrying the Sentry SDK, and only app/src-tauri may
# depend on it. A daemon on strangers' hosts phones nowhere.
if grep -qE '^(solador-crashreport|sentry|sentry-[a-z]+) ' <<< "$agent_tree"; then
    echo "::error::agent/ resolves crates/crashreport or the Sentry SDK — the agent must never carry crash reporting (CLAUDE.md: only app/src-tauri depends on crashreport)"
    exit 1
fi
echo "agent/ does not resolve solador-updatefeed, solador-crashreport or sentry."
