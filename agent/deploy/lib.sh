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

# ---- release artifacts (#392, #490) -------------------------------------------
# The published agent binaries live on the agent's OWN releases, one tag per
# release, `agent-vYYYY.M.N` (#472) — a release train of its own, apart from the
# cockpit's `v*` tags. They are FOUND through the signed feed on the permanent
# `agent-latest` release, never through GitHub's `/releases/latest` (which names
# the cockpit's newest release and carries no agent assets). Releases cut before
# the split, `vYYYY.M.N` tags with agent assets (the last combined release is
# named by LAST_COMBINED_AGENT_RELEASE in scripts/config.sh), are still
# installable by pinning one explicitly. Every asset name below is constructed
# to match what `scripts/build-agent.sh` produced and the agent release workflow
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

# Is $1 a CalVer as the mints emit it? A four-digit year, a NON-padded month 1-12
# and a patch of at least 1: `2026.9.8`, never `2026.09.8`, `2026.9.0`, a `v`
# prefix, or a `+dev` source-build suffix. A bash regex over the WHOLE string
# (not grep's per-line match), so a newline cannot let a second line ride along.
is_calver_version() {
    local re='^[1-9][0-9]{3}\.([1-9]|1[0-2])\.[1-9][0-9]*$'
    [[ "$1" =~ $re ]]
}

# A release tag is `agent-v` + a CalVer (`agent-vYYYY.M.N`, the agent's own
# release train, #472) or, for a release cut before that split, `v` + a CalVer
# (`vYYYY.M.N`, docs/VERSIONING.md) — and nothing else is accepted: not a branch
# name, not a commit, not `agent-latest` (the rolling feed release, which holds
# no binaries), not the `releases` landing page a redirect can hand back. The
# download URL is built from this string, so it is validated before it is ever
# interpolated.
validate_release_tag() {
    local tag="$1" version
    # Refuse control characters outright before the pattern is consulted: a
    # newline inside the value must not be able to ride along into a URL.
    case "$tag" in
        *[[:cntrl:]]*)
            echo "ERROR: release tag contains a control character; refusing it." >&2
            return 1
            ;;
    esac
    case "$tag" in
        agent-v*) version="${tag#agent-v}" ;;
        v*) version="${tag#v}" ;;
        *) version="" ;;
    esac
    if [ -n "$version" ] && is_calver_version "$version"; then
        return 0
    fi
    echo "ERROR: '$tag' is not a release tag (expected agent-vYYYY.M.N, e.g. agent-v2026.11.1, or a legacy vYYYY.M.N that carries agent assets, e.g. v2026.9.8)." >&2
    return 1
}

# The version a (validated) release tag names: the tag without its prefix. The
# prefix is the only difference between the two tag kinds, so one function reads
# both and the asset name is built the same way for either.
release_version_of_tag() {
    local tag="$1"
    case "$tag" in
        agent-v*) printf '%s\n' "${tag#agent-v}" ;;
        *) printf '%s\n' "${tag#v}" ;;
    esac
}

# ---- the signed feed: how the default path finds the latest agent release ------
# The agent's releases are found through `agent-latest.json` on the permanent
# `agent-latest` release (docs/AGENT-DISTRIBUTION.md §2): a document signed under
# the committed key that names the version of the newest published agent
# release. It replaces `/releases/latest`, which names the COCKPIT's newest
# release and carries no agent assets.
AGENT_FEED_RELEASE="agent-latest"
AGENT_FEED_ASSET="agent-latest.json"

# Read the `version` out of a feed whose signature has ALREADY VERIFIED, and
# print it — or print nothing and fail. NEVER call this on bytes that have not
# passed `verify_agent_signature`: the whole point of the order is that the
# first thing read out of the download is read out of bytes the committed key
# vouches for.
#
# The feed is the producer's own pretty-printed document
# (`crates/updatefeed::agent`'s `Feed::to_bytes`: two-space indent, `version`
# first), so `version` is the one line shaped `  "version": "…",` at that
# indent — no JSON parser, no jq. Anything else (no such line, two of them, a
# version that is not a strict CalVer) is a refusal, not a guess: a feed this
# parser cannot read is a feed this checkout predates, and "fails closed" is
# the only safe answer there. `agent/src/update.rs` does the full decode.
agent_feed_version() {
    local file="$1" found version
    [ -f "$file" ] || { echo "ERROR: feed $file not found; nothing to read." >&2; return 1; }
    found="$(sed -n 's/^  "version": "\([^"]*\)",$/\1/p' "$file")"
    case "$found" in
        "")
            echo "ERROR: the verified feed has no top-level \"version\" this installer can read." >&2
            echo "       Refusing to guess; pin a release with SOLADOR_AGENT_RELEASE=agent-vYYYY.M.N." >&2
            return 1
            ;;
        *$'\n'*)
            echo "ERROR: the verified feed names more than one top-level version; refusing to pick one." >&2
            return 1
            ;;
    esac
    version="$found"
    if ! is_calver_version "$version"; then
        echo "ERROR: the verified feed's version '$version' is not a CalVer (YYYY.M.N); refusing it." >&2
        echo "       It is signed, but nothing here would accept it as a release to download." >&2
        return 1
    fi
    printf '%s\n' "$version"
}

# Resolve the latest *published* agent release and print its tag
# (`agent-v<version>`), reading it from the signed feed:
#
#   1. fetch `agent-latest.json` and its `.minisig` from the `agent-latest`
#      release into STAGE (a private directory the caller owns and removes);
#   2. VERIFY them with the stock `minisign` under the checked-out public key —
#      the same gate the binary passes, `verify_agent_signature`;
#   3. ONLY THEN read `version` out of the verified bytes, strictly.
#
# That order is the point: a feed that fails verification is rejected before a
# byte of it has been interpreted, so a tampered `version` — a host steered to
# a release of the attacker's choosing — is never read at all. What this cannot
# do is stop a REPLAYED older feed, which is validly signed; the caller's
# refusal to move an installed agent backwards covers that (install.sh's
# downgrade check). The binary is then downloaded from that tag and verified on
# its own signature, as before.
resolve_latest_agent_release() {
    local repo="$1" stage="$2" pubkey="$3" feed sig url version
    feed="$stage/$AGENT_FEED_ASSET"
    sig="$feed.minisig"
    url="$repo/releases/download/$AGENT_FEED_RELEASE/$AGENT_FEED_ASSET"
    if ! download_release_asset "$url" "$feed"; then
        echo "ERROR: could not download the agent feed $url" >&2
        echo "       Agent releases are found through that signed feed. Either no agent release" >&2
        echo "       has been published through it yet, or it is not reachable from here." >&2
        echo "       This installer does not fall back to building from source." >&2
        return 1
    fi
    if ! download_release_asset "$url.minisig" "$sig"; then
        echo "ERROR: could not download $url.minisig" >&2
        echo "       The feed is published without its signature; refusing to read an unverifiable one." >&2
        return 1
    fi
    verify_agent_signature "$feed" "$sig" "$pubkey" || return 1
    version="$(agent_feed_version "$feed")" || return 1
    printf 'agent-v%s\n' "$version"
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
    # The launchd domain the service lives in: the invoking user's gui domain
    # for a LaunchAgent, "system" for the --system-daemon LaunchDaemon (#506).
    local domain="${2:-gui/$(id -u)}"
    case "$(uname -s)" in
        Darwin)
            echo "         launchctl print $domain/$label"
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
        # TLS/certificate failures (#447): 77 is what a DER cacert produces —
        # cacert pointed at a file curl's TLS backend could not load
        # (solador-agent.tls.crt must be PEM: every backend here refuses DER;
        # see agent/src/tls.rs). 35/51/60 are the
        # handshake and verification failures the same misconfiguration, or
        # a genuinely mismatched pin, would produce.
        77) echo "could not load the certificate cacert points at — it must be PEM, not DER" ;;
        35) echo "TLS handshake failed — the agent may not actually be speaking TLS on that port" ;;
        60 | 51) echo "certificate verification failed — the served certificate does not match the pinned solador-agent.tls.crt" ;;
        *) echo "see curl(1) EXIT CODES" ;;
    esac
}

# ---- binary version ---------------------------------------------------------
# Ask a built agent binary what version it is, and print it.
#
# Read out of the ARTIFACT, never out of a manifest. Since #390 the agent's
# version is derived once by scripts/get-version-info.sh and compiled in by
# agent/build.rs — since #490 it is the agent's OWN version (a release's
# `YYYY.M.N`, or a source build's `<base>+dev.<k>.g<sha>`, which is what a
# from-source redeploy verifies), pinned by AGENT_MARKETING_VERSION in a release
# build and never by the cockpit's MARKETING_VERSION. `agent/Cargo.toml`'s
# `[package] version` is a
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
        echo "       A binary built outside a full git checkout (a shallow clone, an" >&2
        echo "       unpacked source archive, or a clone without the agent's base tag —" >&2
        echo "       git fetch --tags) carries no version; see docs/VERSIONING.md." >&2
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
# tailnet IPv4 when there is one, all interfaces (a wildcard) for a TLS host with
# no tailnet (#449), or whatever address the operator set explicitly. A wildcard
# is not an address you can dial, so probe loopback there; an IPv6 literal needs
# brackets before it is a legal URL host.
#
# The optional third argument is SOLADOR_AGENT_TLS's value (#447): exactly
# "1" means https://, anything else (including absent) means http:// — never
# inferred from the port. agent/src/update.rs's `probe_target` carries the same
# rows — this URL, and for TLS `verify_health`'s `connect_line` — and its table
# test plus lib_test.sh's `verify_health` table (`TLS=0` and `TLS=1`, the same
# rows) pin the two to each other for every bind form: "", 0.0.0.0, ::, [::],
# IPv4, bare and bracketed IPv6, bracketed non-IPv6 (`[100.64.0.9]`, `[host]`)
# and a name. A bracket pair is stripped first (`bind_bare`) and only an IPv6
# literal is bracketed again, so `[fd7a::1]` is never double-bracketed and
# `[100.64.0.9]` dials as `100.64.0.9` — brackets are not legal around a non-IPv6
# host. An IPv6 zone id (`fe80::1%en0`) has no row: it is refused whether or not
# TLS is on (#476) — by the agent at start, by `install.sh` before anything is
# downloaded, and by `update`/`rollback` before anything changes — so none of
# these functions is ever handed one by a supported path.
bind_bare() {
    local bind="${1:-}"
    case "$bind" in
        \[*\]) bind="${bind#\[}"; bind="${bind%\]}" ;;
    esac
    printf '%s\n' "$bind"
}

health_url() {
    local bind="${1:-}" port="${2:-7878}" tls="${3:-}" host scheme
    bind="$(bind_bare "$bind")"
    case "$bind" in
        "" | 0.0.0.0) host="127.0.0.1" ;;
        "::") host="[::1]" ;;
        *:*) host="[${bind}]" ;;
        *) host="$bind" ;;
    esac
    scheme="http"
    [ "$tls" = "1" ] && scheme="https"
    printf '%s://%s:%s/v1/health\n' "$scheme" "$host" "${port:-7878}"
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

# ---- the update transaction lock (#393, #439) --------------------------------
# `<bin>.update.lock` is the flock `solador-agent update`/`rollback` hold for
# their lifetime (agent/src/update.rs's TransactionLock). They create it once
# and never remove it; `install.sh --uninstall` does remove it, while holding
# it (see that script's header, LOCK LIFETIME). Either way the file's mere
# existence proves nothing: it is exactly as present the instant after a
# transaction finishes cleanly as it is while one is running.
#
# There is no standalone "is it busy" checker here: `--uninstall` opens and
# holds the lock itself (via `flock -n` or the equivalent perl one-liner, see
# install.sh's `run_uninstall`) and so asks the kernel directly, which a
# one-shot path-based check cannot do without a window. TransactionLock takes
# the lock with `std::fs::File::try_lock`, which is `flock()` on Unix (not
# `fcntl()`/`F_SETLK`): a lock that survives `fork()` until `exec()`, the
# open-file-description semantics install.sh's own hold asks the same question
# through.

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
# macOS diagnostic names the service that exists rather than the default. The
# optional fourth is the launchd domain that diagnostic names ("system" for the
# --system-daemon LaunchDaemon, #506); it defaults to the invoking user's gui
# domain.
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
    # Only the failure hint reads this (#506): where the service lives.
    local launchd_domain="${4:-gui/$(id -u)}"
    [ -f "$env_file" ] || { echo "ERROR: env file $env_file not found; cannot verify." >&2; return 1; }

    local token bind port tls url
    token="$(env_value "$env_file" SOLADOR_AGENT_TOKEN)"
    bind="$(env_value "$env_file" SOLADOR_AGENT_BIND)"
    port="$(env_value "$env_file" SOLADOR_AGENT_PORT)"
    tls="$(env_value "$env_file" SOLADOR_AGENT_TLS)"
    url="$(health_url "$bind" "$port" "$tls")"

    if [ -z "$token" ]; then
        echo "ERROR: no SOLADOR_AGENT_TOKEN in $env_file; cannot verify." >&2
        return 1
    fi

    # SOLADOR_AGENT_TLS=1 (#447): verify against the certificate beside the
    # env file — solador-agent.tls.crt, the same directory `agent/src/tls.rs`
    # writes it into — never with verification disabled. `cacert` (below, in the SAME
    # -K config curl already reads the Authorization header from — a bash
    # array of extra argv, the more obvious way to make this conditional,
    # is a real portability trap: `"${arr[@]}"` on an EMPTY array throws
    # "unbound variable" under `set -u` on bash 3.2, the interpreter this
    # script runs under on macOS) makes curl trust exactly that one
    # certificate; nothing else (a real CA-signed cert presented instead
    # would fail here too, which is the point: this confirms it is THIS
    # agent, not merely that something answered on the port).
    #
    # The existence check happens INSIDE the retry loop below, not here:
    # on a fresh install this function is often called moments after
    # `systemctl --user restart` / `launchctl kickstart`, and the agent
    # generates solador-agent.tls.crt during its own startup — after reading
    # settings and spawning the sampler, on Linux's Type=simple unit `restart`
    # returns as soon as the process forks, before any of that has run.
    # Checking once, here, would race that startup and report "never started
    # with TLS on" about a service that is about to be fine.
    local cert_file=""
    cert_file="$(dirname "$env_file")/solador-agent.tls.crt"

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
    # escaped in the token first. `cacert_line` rides the same config, on its
    # own line; empty, curl's config parser ignores the blank line.
    local header_line curl_rc
    header_line="$(printf '%s' "$token" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')"
    header_line="header = \"Authorization: Bearer ${header_line}\""

    # Over TLS the certificate is checked as the name `localhost`, which every
    # certificate this agent generates carries, whatever the bind is (#449):
    # the URL names `localhost` and curl is told to CONNECT to the bind address
    # (`connect-to`, in the same -K config). A certificate's SAN list is fixed
    # when it is generated, and the bind can change afterwards — all interfaces
    # to a tailnet address the day Tailscale comes up — so verifying by the
    # bind address would fail a healthy host. Still chain-checked against the
    # one pinned file (`cacert`); nothing is disabled.
    local connect_line="" probe_url="$url"
    if [ "$tls" = "1" ]; then
        local bare_bind
        bare_bind="$(bind_bare "$bind")"
        case "$bare_bind" in
            "" | 0.0.0.0 | "::") ;;
            *)
                local dial_host="$bare_bind"
                case "$bare_bind" in
                    *:*) dial_host="[${bare_bind}]" ;;
                esac
                probe_url="https://localhost:${port:-7878}/v1/health"
                connect_line="connect-to = \"localhost:${port:-7878}:${dial_host}:${port:-7878}\""
                ;;
        esac
    fi

    local attempt body got cacert_line cert_missing
    body=""
    got=""
    curl_rc=0
    cert_missing=false
    # Bounded per attempt: without a connect timeout a blackholed bind (a
    # tailnet address with Tailscale down) waits out the OS SYN timeout —
    # 75 s on macOS, ~2 min on Linux — fifteen times over. `got` is reset per
    # attempt so one early answer from the OLD agent (the bootout race) does
    # not turn fourteen refused probes into "a stale binary is still serving".
    for attempt in $(seq 1 "${VERIFY_HEALTH_ATTEMPTS:-15}"); do
        curl_rc=0
        got=""
        cacert_line=""
        if [ "$tls" = "1" ]; then
            if [ ! -f "$cert_file" ]; then
                # Not yet generated — the agent may still be starting (see
                # above). Consume this attempt like any other unanswered
                # one, rather than a distinct return: only if it is STILL
                # missing once every attempt is spent does that become the
                # reported cause, below.
                cert_missing=true
                sleep 1
                continue
            fi
            cert_missing=false
            cacert_line="cacert = \"$(printf '%s' "$cert_file" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')\""
        fi
        body="$(printf '%s\n%s\n%s\n' "$header_line" "$cacert_line" "$connect_line" | curl -fsS --connect-timeout 2 --max-time 5 -K - "$probe_url" 2>/dev/null)" || curl_rc=$?
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

    if [ "$tls" = "1" ] && [ "$cert_missing" = true ]; then
        echo "ERROR: SOLADOR_AGENT_TLS=1 but $cert_file still does not exist after" >&2
        echo "       ${VERIFY_HEALTH_ATTEMPTS:-15} attempts; the agent generates it on its own" >&2
        echo "       first start, so this means it never started with TLS on." >&2
        return 1
    fi

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
        service_inspect_hint "$launchd_label" "$launchd_domain" >&2
    else
        echo "ERROR: /v1/health did not report version $expected_version within timeout." >&2
        echo "       Last response: ${body:-<no response>}" >&2
        [ "$curl_rc" -ne 0 ] && echo "       Last probe:    curl exit $curl_rc ($(curl_exit_hint "$curl_rc"))" >&2
    fi
    return 1
}
