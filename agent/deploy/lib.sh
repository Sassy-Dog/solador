#!/usr/bin/env bash
#
# Shared helpers for the Solador agent deploy scripts (install.sh, redeploy.sh).
#
# Sourced, never executed on its own. Nothing in here prints the bearer token.
#
# The reason this file exists: both scripts must answer the same question after
# restarting the unit — "is the version now being *served* the version we just
# installed?" A unit that is `active (running)` proves only that *a* binary is
# up, not that it is this one. Any failure that leaves the old binary in place
# (a swap into the wrong prefix, a stale ExecStart, a future script regression)
# looks identical to success unless the served version is compared to the
# artifact.
#
# Since #392 the two scripts get their artifact from different places —
# install.sh downloads a published, minisigned release binary; redeploy.sh
# still builds from source — and the helpers below are grouped accordingly.
# The version-and-health half is shared by both.

# ---- release artifacts (#392) ----------------------------------------------
# The published agent binaries live on the same GitHub Release the cockpit
# ships on, one tag for both products (#390). Every asset name below is
# constructed to match what `scripts/build-agent.sh` produced and `release.yml`
# attached; install.sh names the repository.

# Map this host's `uname -s` / `uname -m` onto one of the four published
# triples, and print it. FAILS on anything else: an unsupported platform must
# not "nearly match" its way to a binary that will not run here.
#
# The arm64 / aarch64 split is real, not cosmetic — macOS's uname says arm64,
# Linux's says aarch64, and the published triples say aarch64 on both. amd64
# is what some BSD-derived userlands and container images report for x86_64.
agent_target_for() {
    local os="$1" arch="$2" arch_part os_part
    case "$arch" in
        x86_64 | amd64) arch_part="x86_64" ;;
        arm64 | aarch64) arch_part="aarch64" ;;
        *)
            echo "ERROR: unsupported architecture '$arch' (published: x86_64, aarch64/arm64)." >&2
            return 1
            ;;
    esac
    case "$os" in
        Linux) os_part="unknown-linux-musl" ;;
        Darwin) os_part="apple-darwin" ;;
        *)
            echo "ERROR: unsupported operating system '$os' (published: Linux, Darwin)." >&2
            return 1
            ;;
    esac
    printf '%s-%s\n' "$arch_part" "$os_part"
}

# Print the published asset name for a version and a triple. One function so
# the binary and its detached signature can never be named by two different
# rules: the signature is always "<asset>.minisig".
agent_asset_name() {
    local version="$1" triple="$2"
    printf 'solador-agent-%s-%s\n' "$version" "$triple"
}

# A release tag is `v` + the repo's CalVer (`vYYYY.M.N`, docs/VERSIONING.md),
# and nothing else is accepted — not a branch name, not a commit, not the
# `releases` landing page a redirect can hand back when a repo has no releases.
# The download URL is built from this string, so it is validated before it is
# ever interpolated.
validate_release_tag() {
    local tag="$1"
    # grep matches per LINE, so a newline inside the value would let a second
    # line ride along on the first one's match. Refuse control characters
    # outright before the pattern is consulted.
    case "$tag" in
        *[[:cntrl:]]*)
            echo "ERROR: release tag contains a control character; refusing it." >&2
            return 1
            ;;
    esac
    if printf '%s' "$tag" | grep -qE '^v[0-9]{4}\.[0-9]{1,2}\.[0-9]+$'; then
        return 0
    fi
    echo "ERROR: '$tag' is not a release tag (expected vYYYY.M.N, e.g. v2026.9.8)." >&2
    return 1
}

# Resolve the latest *published* release tag and print it.
#
# GitHub answers `<repo>/releases/latest` with a redirect to
# `<repo>/releases/tag/<tag>` for the newest non-draft, non-prerelease release.
# That redirect is the whole discovery mechanism: no API token, no JSON to
# parse, nothing but curl. A draft is invisible here by construction — its
# assets are not downloadable either — so this can only ever name a release
# whose files a stranger could fetch.
#
# What it cannot do is promise the release *has* agent binaries. The first
# releases after #390 predate them, and the download step is where that is
# discovered — as a hard failure naming the tag, never as a fallback.
resolve_latest_release_tag() {
    local repo="$1" effective tag
    effective="$(curl -fsSL --proto '=https' -o /dev/null -w '%{url_effective}' "$repo/releases/latest")" || {
        echo "ERROR: could not resolve the latest release from $repo/releases/latest." >&2
        return 1
    }
    case "$effective" in
        "$repo/releases/tag/"?*) tag="${effective#"$repo/releases/tag/"}" ;;
        *)
            echo "ERROR: $repo/releases/latest did not land on a release tag (got: ${effective:-<nothing>})." >&2
            return 1
            ;;
    esac
    validate_release_tag "$tag" || return 1
    printf '%s\n' "$tag"
}

# Fetch one release asset to a path. `--proto '=https'` refuses to follow a
# redirect off HTTPS; `-f` turns a 404 into a failure instead of a saved error
# page that would then fail signature verification with a misleading message.
download_release_asset() {
    local url="$1" dest="$2"
    curl -fsSL --proto '=https' --retry 2 -o "$dest" "$url"
}

# Verify a downloaded binary against its detached minisign signature under the
# COMMITTED public key, `agent/release-signing-key.pub`.
#
# This is the security boundary of the whole install (docs/AGENT-DISTRIBUTION.md
# §5): HTTPS authenticates the transport, not the artifact. It is the reference
# C `minisign` — the stock tool anyone can install — and never a verifier
# fetched beside the candidate. FAILS CLOSED on a missing verifier, a missing
# key or a missing signature: each of those is "I cannot prove this is ours",
# and an unprovable binary is not installed.
#
# The public key is the one shipped with this checkout. It is not downloaded,
# and there is no override: a key fetched from the same place as the binary
# proves nothing, and the install path is the one place a stranger's host
# decides whom to trust.
#
# (verify_agent_signature is below; its verifier check comes first.)
#
# The verifier must be present AND must BE minisign, not merely answer to the
# name. The repo's own pinned signer, rsign, takes `-V` as "print the version"
# and exits 0 — so a convenience symlink `minisign -> rsign` on a maintainer's
# machine would be an accept-everything verifier. `minisign -v` prints
# "minisign <version>" and nothing else does. Called in install.sh's preflight
# (so the refusal lands before any download) and again by
# verify_agent_signature (so the check cannot be skipped by a caller).
require_real_minisign() {
    local ident
    if ! command -v minisign >/dev/null 2>&1; then
        echo "ERROR: minisign not found in PATH — cannot verify the downloaded binary." >&2
        echo "       Install the stock minisign (brew install minisign; apt install minisign on" >&2
        echo "       Debian 12+ / Ubuntu 24.04+; dnf install minisign; otherwise" >&2
        echo "       https://jedisct1.github.io/minisign/) and re-run. This installer never" >&2
        echo "       installs a verifier for you." >&2
        return 1
    fi
    ident="$(minisign -v 2>/dev/null | head -n1)" || true
    case "$ident" in
        "minisign "*) ;;
        *)
            echo "ERROR: the 'minisign' on PATH does not identify itself as minisign (got: ${ident:-<nothing>})." >&2
            echo "       Refusing to verify with a tool whose verdict cannot be trusted." >&2
            return 1
            ;;
    esac
}

verify_agent_signature() {
    local bin="$1" sig="$2" pubkey="$3"
    require_real_minisign || return 1
    [ -f "$pubkey" ] || { echo "ERROR: signing public key $pubkey not found — this checkout is incomplete." >&2; return 1; }
    [ -f "$sig" ] || { echo "ERROR: signature file $sig not found; refusing an unsigned binary." >&2; return 1; }
    [ -f "$bin" ] || { echo "ERROR: $bin not found; nothing to verify." >&2; return 1; }
    if ! minisign -Vq -m "$bin" -x "$sig" -p "$pubkey"; then
        echo "ERROR: SIGNATURE VERIFICATION FAILED for $(basename "$bin")." >&2
        echo "       The download does not verify under $pubkey. It has NOT been" >&2
        echo "       executed and nothing has been installed. A corrupted download, a" >&2
        echo "       tampered asset, or a key rotation this checkout predates all land" >&2
        echo "       here; do not work around it." >&2
        return 1
    fi
}

# ---- service rendering (#392) ----------------------------------------------
# The service templates carry `@…@` placeholders and the installer renders the
# ACTUAL paths it chose into them, so nobody edits an ExecStart by hand.
# Rendering with parameter expansion rather than sed: a sed replacement
# reinterprets `&` and `\`, and a home directory is user-supplied text.
#
# Bash 5.2 taught `${var//pat/rep}` the same `&` trick (`patsub_replacement`,
# on by default), and the two escapes that exist for it disagree between
# versions: quoting the replacement is literal on 5.2+ and inserts the quote
# characters themselves on 3.2. Switching the option off — where it exists —
# is the one form both behave under. Measured, not assumed: a HOME with an
# `&` rendered the placeholder's own name into the plist until this landed.
render_template() (
    # A subshell body — `( … )`, not `{ … }` — so the shopt below is scoped
    # to this render and never escapes into the caller's shell.
    local template="$1" content
    shift
    shopt -u patsub_replacement 2>/dev/null || true
    content="$(cat "$template")" || return 1
    while [ "$#" -ge 2 ]; do
        content="${content//"$1"/$2}"
        shift 2
    done
    printf '%s\n' "$content"
)

# Refuse an install prefix that either service format cannot carry safely.
#
# A double quote, a backslash, `$` or `%` each mean something to systemd inside
# an ExecStart= — even a quoted one — and a control character is never a path
# anyone chose. Whitespace is fine and is the case that actually occurs
# ("/Users/Some Name"); it is handled by quoting at render time.
check_install_path() {
    local path="$1"
    case "$path" in
        /*) ;;
        *) echo "ERROR: install prefix '$path' is not an absolute path." >&2; return 1 ;;
    esac
    case "$path" in
        *[[:cntrl:]]* | *'"'* | *'\'* | *'$'* | *'%'*)
            echo "ERROR: install prefix '$path' contains a character (\" \\ \$ % or a control" >&2
            echo "       character) that a systemd ExecStart= or a plist cannot carry safely." >&2
            return 1
            ;;
    esac
}

# Render a binary path for a systemd `ExecStart=` line: as-is when it is made
# of characters systemd reads literally, double-quoted otherwise (whitespace,
# most notably). The unquoted form is preferred rather than always quoting
# because `redeploy.sh` resolves the live binary from this line with a plain
# `awk '{print $1}'` and must keep working on every host installed so far.
systemd_exec_path() {
    local path="$1"
    check_install_path "$path" || return 1
    case "$path" in
        *[!A-Za-z0-9._/@:+,=~-]*) printf '"%s"\n' "$path" ;;
        *) printf '%s\n' "$path" ;;
    esac
}

# Escape a string for a plist <string> element.
xml_escape() {
    printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g"
}

# The service manager's own view of the agent, for a diagnostic that would
# otherwise point a macOS operator at systemctl. Read from `uname`, not from a
# flag, so redeploy.sh (Linux-only) prints exactly what it always did. The
# launchd label is a parameter because install.sh honours an override for it;
# a hint naming a service that does not exist is worse than none.
service_inspect_hint() {
    local label="${1:-app.solador.agent}"
    case "$(uname -s)" in
        Darwin)
            echo "         launchctl print gui/$(id -u)/$label"
            echo "         tail -n 50 ~/Library/Logs/solador-agent.log"
            ;;
        *)
            echo "         systemctl --user cat solador-agent | grep ExecStart"
            ;;
    esac
}

# One line of meaning for the curl exit codes a health probe actually
# produces, so "no response" can say whether nothing was listening, the
# request timed out, or the agent answered with an HTTP error (which -f turns
# into exit 22 — a 401 from an old agent still holding the socket lands here).
curl_exit_hint() {
    case "${1:-0}" in
        7) echo "could not connect — nothing listening at that address/port, or no route to it" ;;
        28) echo "timed out — the address may be unreachable (a tailnet bind with Tailscale down, say)" ;;
        22) echo "HTTP error — the agent answered but refused (a 401 means the token it holds is not this one)" ;;
        52 | 56) echo "the connection was accepted then dropped — the agent is probably crash-looping; check its log" ;;
        6) echo "could not resolve the host" ;;
        *) echo "see curl(1) EXIT CODES" ;;
    esac
}

# ---- binary version ---------------------------------------------------------
# Ask a built agent binary what version it is, and print it.
#
# Read out of the ARTIFACT, never out of a manifest. Since #390 the agent's
# version is the repo's CalVer, derived once by scripts/get-version-info.sh and
# compiled in by agent/build.rs — `agent/Cargo.toml`'s `[package] version` is a
# wire-contract marker that names no release and would now assert the wrong
# number against /v1/health. Asking the binary is also the repo's standing rule
# for version claims (the macOS bundle re-reads its own Info.plist): it is the
# only source that cannot disagree with what is about to be installed.
#
# `--version` prints the version and nothing else, so this needs no parsing.
# It FAILS CLOSED — a binary compiled outside a full git checkout carries no
# version, exits non-zero and prints nothing, and a caller must treat that as
# fatal rather than verifying against an empty string (which /v1/health would
# then "match" by also omitting the key).
binary_version() {
    local bin="$1" out
    [ -x "$bin" ] || { echo "ERROR: $bin is not an executable binary." >&2; return 1; }
    out="$("$bin" --version 2>/dev/null)" || {
        echo "ERROR: $bin --version failed." >&2
        echo "       A binary built outside a full git checkout (a shallow clone, or an" >&2
        echo "       unpacked source archive) carries no version; see docs/VERSIONING.md." >&2
        return 1
    }
    # First line, then all whitespace (a stray CR from a checkout that rewrote
    # line endings would otherwise become part of the string compared against
    # /v1/health, and fail with two identical-looking versions on screen).
    #
    # Parameter expansion rather than `head -n1`: these scripts run under
    # `set -o pipefail`, where `head` closing the pipe early can leave the
    # producer with SIGPIPE and take the whole deploy down over a version
    # string that parsed perfectly well.
    out="${out%%
*}"
    out="$(printf '%s' "$out" | tr -d '[:space:]')"
    [ -n "$out" ] || { echo "ERROR: $bin --version printed nothing." >&2; return 1; }
    printf '%s\n' "$out"
}

# ---- build ------------------------------------------------------------------
# Print the directory cargo writes build output to.
#
# Emphatically NOT "$CRATE_DIR/target". Since agent/ became a member of the root
# workspace (#264) cargo writes to the *workspace* target dir, while the
# crate-local agent/target/ left over from the standalone days sits there still
# holding a binary of the same name from the last pre-#264 build. So the two
# plausible guesses are "the path that no longer receives builds" and "the path
# that contains a stale binary which would install and run perfectly well" —
# which is why this asks cargo instead of guessing, and why the caller must
# treat a failure here as fatal rather than falling back to a search.
target_dir() {
    local crate_dir="$1" ws_manifest
    if [ -n "${CARGO_TARGET_DIR:-}" ]; then
        printf '%s\n' "$CARGO_TARGET_DIR"
        return 0
    fi
    ws_manifest="$(cd "$crate_dir" && cargo locate-project --workspace --message-format plain 2>/dev/null || true)"
    [ -n "$ws_manifest" ] || return 1
    printf '%s/target\n' "$(dirname "$ws_manifest")"
}

# Build the agent in release mode; print the path to the binary on success.
#
# Scoped with -p deliberately: a bare `cargo build` from inside the workspace
# resolves every member including app/src-tauri, which needs webkit2gtk
# libraries a headless metrics host has no reason to carry.
#
# cargo's own output goes to stderr so stdout carries only the path.
build_release_binary() {
    local crate_dir="$1" bin_name="$2" td built
    ( cd "$crate_dir" && cargo build --release -p "$bin_name" >&2 ) || return 1

    if ! td="$(target_dir "$crate_dir")"; then
        echo "ERROR: could not locate cargo's target directory for $crate_dir." >&2
        echo "       Is this still inside a cargo workspace?" >&2
        return 1
    fi

    built="$td/release/$bin_name"
    if [ ! -x "$built" ]; then
        echo "ERROR: build reported success but produced no binary at $built" >&2
        return 1
    fi
    printf '%s\n' "$built"
}

# ---- health endpoint --------------------------------------------------------
# Build the URL to probe from the agent's configured bind address.
#
# The bind is whatever SOLADOR_AGENT_BIND resolved to at install time — a
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

# Is CalVer $1 strictly newer than CalVer $2? Both `YYYY.M.N`; compared field
# by field as integers, so 2026.10.1 beats 2026.9.30. Anything that is not
# three dotted integers compares as "not newer" — this guards a downgrade, and
# an unparseable version must not turn the guard into a refusal of every
# install.
calver_newer() {
    local a="$1" b="$2" a1 a2 a3 b1 b2 b3
    case "$a" in *[!0-9.]* | '' | *..* | .* | *.) return 1 ;; esac
    case "$b" in *[!0-9.]* | '' | *..* | .* | *.) return 1 ;; esac
    IFS=. read -r a1 a2 a3 <<< "$a"
    IFS=. read -r b1 b2 b3 <<< "$b"
    [ -n "$a3" ] && [ -n "$b3" ] || return 1
    if [ "$a1" -ne "$b1" ]; then [ "$a1" -gt "$b1" ]; return; fi
    if [ "$a2" -ne "$b2" ]; then [ "$a2" -gt "$b2" ]; return; fi
    [ "$a3" -gt "$b3" ]
}

# ---- the env file -----------------------------------------------------------
# Read one key's value out of an env file, the way systemd's EnvironmentFile=
# and the macOS launcher (run-agent.sh) read it: first matching line, trailing
# CR dropped, surrounding whitespace trimmed, one matching pair of quotes
# removed. Every reader in these scripts goes through this, so a token an
# operator rotated by hand as `KEY="value"` is the same token to the service
# that starts and to the probe that verifies it. Prints nothing when the key
# is absent.
env_value() {
    local file="$1" key="$2" value
    [ -f "$file" ] || return 0
    # LAST match wins, as it does for systemd's EnvironmentFile= and for the
    # launcher's export loop: a token rotated by appending a line must be the
    # token the probe verifies, or the installer re-writes the old one.
    value="$(awk -v k="$key=" 'index($0, k) == 1 { v = substr($0, length(k) + 1); found = 1 } END { if (found) print v }' "$file")"
    value="${value%$'\r'}"
    value="${value#"${value%%[![:space:]]*}"}"
    value="${value%"${value##*[![:space:]]}"}"
    case "$value" in
        \"*\") value="${value#\"}"; value="${value%\"}" ;;
        \'*\') value="${value#\'}"; value="${value%\'}" ;;
    esac
    printf '%s\n' "$value"
}

# ---- health verification ----------------------------------------------------
# Poll /v1/health (bind/port/token read from the env file) until it answers,
# then optionally assert the version it reports.
#
#   verify_health <env-file> <expected-version> [launchd-label]  # exact match
#   verify_health <env-file> ""                                   # only answer
#
# The optional third argument is the launchd label install.sh derived, so the
# macOS diagnostic names the service that exists rather than the default.
#
# The empty form is for rollback, which asserts only that the agent came back.
# It predates `--version` (#390) and is no longer forced: `.prev` could now be
# asked with `binary_version`. Tightening rollback to a version assertion is a
# behaviour change with its own failure mode (a .prev that answers but does not
# serve), so it stays a deliberate decision rather than a side effect of the
# flag existing. Never prints the token.
#
# Polls once a second, 15 times by default; VERIFY_HEALTH_ATTEMPTS overrides that
# (it exists so the failure paths can be exercised without a 15s wait).
verify_health() {
    local env_file="$1" expected_version="${2:-}" launchd_label="${3:-app.solador.agent}"
    [ -f "$env_file" ] || { echo "ERROR: env file $env_file not found; cannot verify." >&2; return 1; }

    local token bind port url
    token="$(env_value "$env_file" SOLADOR_AGENT_TOKEN)"
    bind="$(env_value "$env_file" SOLADOR_AGENT_BIND)"
    port="$(env_value "$env_file" SOLADOR_AGENT_PORT)"
    url="$(health_url "$bind" "$port")"

    if [ -z "$token" ]; then
        echo "ERROR: no SOLADOR_AGENT_TOKEN in $env_file; cannot verify." >&2
        return 1
    fi

    if [ -n "$expected_version" ]; then
        echo "==> Verifying $url reports version $expected_version ..."
    else
        echo "==> Verifying $url is back online ..."
    fi

    # The Authorization header reaches curl through a config file on stdin
    # (`-K -`), never on its argv: argv is world-readable through
    # /proc/<pid>/cmdline on any Linux without hidepid, and #392 made this the
    # path a stranger runs on a shared server. curl's config syntax quotes
    # values with double quotes and escapes with backslash, so both are
    # escaped in the token first.
    local header_line curl_rc
    header_line="$(printf '%s' "$token" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')"
    header_line="header = \"Authorization: Bearer ${header_line}\""

    local attempt body got
    body=""
    got=""
    curl_rc=0
    # Bounded per attempt: without a connect timeout a blackholed bind (a
    # tailnet address with Tailscale down) waits out the OS SYN timeout —
    # 75 s on macOS, ~2 min on Linux — fifteen times over. `got` is reset per
    # attempt so one early answer from the OLD agent (the bootout race) does
    # not turn fourteen refused probes into "a stale binary is still serving".
    for attempt in $(seq 1 "${VERIFY_HEALTH_ATTEMPTS:-15}"); do
        curl_rc=0
        got=""
        body="$(printf '%s\n' "$header_line" | curl -fsS --connect-timeout 2 --max-time 5 -K - "$url" 2>/dev/null)" || curl_rc=$?
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
        [ "$curl_rc" -ne 0 ] && echo "       Last probe:    curl exit $curl_rc ($(curl_exit_hint "$curl_rc"))" >&2
        return 1
    fi

    if [ -n "$got" ]; then
        # The damning case: the agent is up and healthy, and serving the wrong
        # code. Name both numbers so the operator doesn't have to go find them.
        echo "ERROR: VERSION MISMATCH — the running agent is not the binary just installed." >&2
        echo "         served (per $url): $got" >&2
        echo "         installed (per <binary> --version): $expected_version" >&2
        echo "       A stale binary is still serving. Check that the service starts" >&2
        echo "       the path the binary was installed to:" >&2
        service_inspect_hint "$launchd_label" >&2
    else
        echo "ERROR: /v1/health did not report version $expected_version within timeout." >&2
        echo "       Last response: ${body:-<no response>}" >&2
        [ "$curl_rc" -ne 0 ] && echo "       Last probe:    curl exit $curl_rc ($(curl_exit_hint "$curl_rc"))" >&2
    fi
    return 1
}
