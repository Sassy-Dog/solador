#!/usr/bin/env bash
#
# Checkout-free bootstrap for the Solador metrics agent (#434).
#
# Usage:
#   curl -fsSLo bootstrap.sh https://raw.githubusercontent.com/Sassy-Dog/solador/main/agent/deploy/bootstrap.sh
#   bash bootstrap.sh [--ref <40-hex sha>] [install.sh flags...]
#
#   The pinned form names the exact commit an operator trusts to install —
#   but bootstrap.sh itself is STILL fetched from /main/, never from the
#   pinned sha's own raw URL, and the pin is passed as --ref:
#   curl -fsSLo bootstrap.sh https://raw.githubusercontent.com/Sassy-Dog/solador/main/agent/deploy/bootstrap.sh
#   bash bootstrap.sh --ref <sha> [install.sh flags...]
#
#   WARNING: a bootstrap.sh fetched from .../<sha>/agent/deploy/bootstrap.sh
#   (rather than from /main/) gets NO protection from the --ref-reachable-
#   from-main check below — that check is code INSIDE this script, so a copy
#   fetched from a sha not already known to be on main is free to run its
#   own version of the check (skip it, or always answer yes) and trust its
#   own key. Only a bootstrap.sh already known to be on main can be trusted
#   to enforce that guarantee at all — fetch it from /main/, always, and pin
#   with --ref, never with the raw URL.
#
# What it does (the accepted proposal on #434 — read the issue's
# "## Decision" section before changing any of this):
#   1. A --ref other than main must be reachable from main (checked against
#      GitHub's compare API, unauthenticated) BEFORE anything is downloaded —
#      codeload.github.com will happily serve an archive for any commit this
#      repository holds, including an open pull request's head, and only a
#      commit main's own history already contains keeps the property below.
#   2. Downloads the repository archive at --ref (default: main) from
#      codeload.github.com, over HTTPS, into a private (0700) staging
#      directory under ~/.cache — never /tmp, for the same reason
#      install.sh's own staging avoids it: a noexec mount there would make
#      running the extracted install.sh fail in a way that looks like
#      something else.
#   3. Extracts ONLY agent/deploy/* and the committed public key(s)
#      (agent/release-signing-key.pub, agent/release-signing-key-next.pub
#      once committed) from that archive. Nothing else it carries — the
#      crates, the app, CI config — is ever written to disk.
#   4. Runs the extracted install.sh, passing every remaining argument
#      through unchanged (--enable-timer, --migrate-from-opt, ...) — except
#      -h/--help/help, which this script answers itself (run install.sh
#      directly, or after this script has extracted it, for its own help).
#      install.sh's own requirements (minisign, systemd/launchd, a bind
#      address) are UNCHANGED; this script itself needs curl, tar, mktemp,
#      id and basename — the same "usual coreutils" assumption install.sh
#      already makes elsewhere in this repo — plus bash. gzip/grep/sed
#      (the best-effort resolved-commit readout) and awk (--help) are used
#      too but never required: their absence degrades, it never blocks an
#      install. The staging directory is removed once install.sh returns,
#      whether it succeeded or not.
#
# Why this keeps the property the earlier "no curl | sh" decision protected
# (agent/README.md's Prerequisites, docs/AGENT-DISTRIBUTION.md §6): the
# public key install.sh verifies a download under must arrive by a path
# other than the download itself, and `main` is the ref this repository's
# ruleset protects. An archive of a commit on `main` — or of a specific
# `--ref <sha>` an operator names, ONLY once step 1 above has confirmed
# main's own history contains it — travels from codeload.github.com over
# the same GitHub HTTPS a `git clone` would use, so the key arrives by the
# same protected path a checkout already gave it. No key is embedded in
# this script, and none is downloaded from anywhere but that archive.
#
# Piping this into a shell is deliberately NOT the documented form (a
# script cut off mid-transfer must run nothing, not whatever arrived) — but
# in case it is piped anyway, everything below lives in ONE function,
# called on the LAST LINE. bash cannot call a function whose closing brace
# it never read, so a truncated transfer downloads nothing and runs nothing.
#
# Exit status: this script's own refusals are 1 (a download/extraction
# failure, a --ref GitHub does not confirm as reachable from main, or
# refusing to run as root) and 2 (a missing or malformed --ref value);
# otherwise install.sh's exit status is passed through unchanged — see its
# own header for the full table, kept there rather than duplicated here so
# the two cannot drift apart.

set -euo pipefail

REPO="Sassy-Dog/solador"
REPO_URL="https://github.com/$REPO"
CODELOAD_URL="https://codeload.github.com/$REPO/tar.gz"
GITHUB_API="https://api.github.com/repos/$REPO"

# Read by usage() only when $0 is not a real file (see below) — a canned
# copy of the header's Usage section, kept short on purpose so there is only
# one place (the header above) that carries the full rationale.
USAGE_FALLBACK_TEXT='Checkout-free bootstrap for the Solador metrics agent (#434).

Usage:
  curl -fsSLo bootstrap.sh https://raw.githubusercontent.com/Sassy-Dog/solador/main/agent/deploy/bootstrap.sh
  bash bootstrap.sh [--ref <40-hex sha>] [install.sh flags...]

  The pinned form names the exact commit an operator trusts to install —
  but bootstrap.sh itself is STILL fetched from /main/, never from the
  pinned sha'"'"'s own raw URL, and the pin is passed as --ref:
  curl -fsSLo bootstrap.sh https://raw.githubusercontent.com/Sassy-Dog/solador/main/agent/deploy/bootstrap.sh
  bash bootstrap.sh --ref <sha> [install.sh flags...]

Run install.sh --help (directly, or after this script has extracted it) for
its own usage.'

usage() {
    # The header comment above, found rather than hardcoded by line range —
    # the same trick install.sh's own usage() uses. That trick needs a real
    # file at $0: piped in ("cat bootstrap.sh | bash -s -- --help"), $0 is
    # the interpreter (bash) rather than this script, and awk has nothing to
    # read there — under `set -e` that failure would kill the run before
    # `exit 0` is ever reached, turning --help into a crash instead of usage
    # text. Fall back to the canned copy above whenever $0 is not a file.
    if [ -f "$0" ]; then
        awk 'NR > 2 && /^#/ { sub(/^# ?/, ""); print; next } NR > 2 { exit }' "$0"
    else
        printf '%s\n' "$USAGE_FALLBACK_TEXT"
    fi
}

# A `--ref` is either the literal word "main" or exactly 40 lowercase hex
# characters — never a branch, a tag, or anything else that could carry a
# newline or a shell metacharacter into a URL this script builds by
# interpolation. Mirrors validate_release_tag's shape in lib.sh, which this
# script cannot source (that file does not exist yet on a clean host — it
# is one of the things being fetched).
validate_ref() {
    local ref="$1"
    case "$ref" in
        *[[:cntrl:]]*)
            echo "ERROR: --ref contains a control character; refusing it." >&2
            return 1
            ;;
    esac
    if [ "$ref" = "main" ]; then
        return 0
    fi
    if printf '%s' "$ref" | grep -qE '^[0-9a-f]{40}$'; then
        return 0
    fi
    echo "ERROR: --ref '$ref' is neither 'main' nor a 40-character lowercase-hex commit SHA." >&2
    return 1
}

# verify_ref_reachable_from_main <ref>
# The property this whole script exists to keep is "the key arrives from
# main, the protected ref" — and that property holds for --ref only if
# <ref> is actually part of main's history. codeload.github.com will
# archive ANY commit this public repository holds, merged or not: an open
# pull request's head is one, and anyone can push a commit there. Without
# this check, publishing "bash bootstrap.sh --ref <that sha>" as "the
# careful, pinned form" would let an attacker's own
# release-signing-key.pub and install.sh stand in for main's, and every
# check downstream (a real minisign verifying a real signature under that
# key) would pass — the chain would be internally consistent and wrong.
#
# Checked against GitHub's compare API (unauthenticated; 60 req/hour is
# ample for one bootstrap run) rather than assumed: comparing <ref> (base)
# against main (head), a `status` of `ahead` (main has additional commits
# on top of <ref>) or `identical` (<ref> IS main) both mean main's history
# already contains <ref>. Anything else — `diverged` (an unmerged branch),
# `behind` (main lacks commits <ref> has), or a request that could not be
# made at all — is refused. Never assumed reachable on a failure to check:
# an unverifiable ref is not a verified one. `per_page=1` asks GitHub to
# keep the commit listing short; the fields this reads are at the top of
# the object regardless.
verify_ref_reachable_from_main() {
    local ref="$1" resp status
    resp="$(curl --proto '=https' --tlsv1.2 -fsSL "$GITHUB_API/compare/$ref...main?per_page=1" 2>/dev/null)" || {
        echo "ERROR: could not verify --ref $ref is reachable from main (GitHub's compare API did not answer)." >&2
        echo "       Refusing to trust an unverified commit. Retry, or omit --ref to use main." >&2
        return 1
    }
    if command -v jq >/dev/null 2>&1; then
        status="$(printf '%s' "$resp" | jq -r '.status // empty' 2>/dev/null)"
    else
        # No jq: fall back to a plain-text scan, but not a naive one. The
        # object's own top-level "status" comes before its "commits" and
        # "files" arrays (confirmed against the live API) — and a *file's*
        # `status` ("added"/"modified"/…) is exactly as valid JSON there as
        # the one this needs, so a match that is not anchored to arrive
        # before those arrays would read a later, per-file status instead of
        # the real one. Truncating the text at the first "commits"/"files"
        # is what closes that off — but sed truncates per LINE, and GitHub
        # may or may not pretty-print this response, so `tr -d '\n'` runs
        # FIRST to squash it onto one line: without that, a per-file
        # "status" sitting on its own line, inside a multi-line "files"
        # array, would be on a line the truncation below never touches, and
        # could still be read as the answer — a JSON string cannot legally
        # contain a raw newline, so nothing this needs to read is lost by
        # removing them. With everything on one line, truncating at
        # "commits"/"files" makes the top-level field the only one an
        # unanchored, greedy match can ever reach, regardless of how the
        # response was formatted.
        status="$(printf '%s' "$resp" \
            | tr -d '\n' \
            | sed -e 's/"commits".*//' -e 's/"files".*//' \
            | sed -n 's/.*"status"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
            | head -n1)"
    fi
    case "$status" in
        identical | ahead) return 0 ;;
        *)
            echo "ERROR: --ref $ref is not reachable from main (GitHub reports '${status:-no status}' between them)." >&2
            echo "       --ref only trusts a commit main's own history already contains — an unmerged" >&2
            echo "       branch or a fork's pull request would let its own release-signing-key.pub and" >&2
            echo "       install.sh stand in for main's. Pin a commit that is actually on main." >&2
            return 1
            ;;
    esac
}

# extract_from_archive <archive> <dest> <pattern...>
# Extract only the archive members matching the given patterns. GNU tar
# (Linux) matches member-name patterns literally unless told otherwise, so
# --wildcards is tried first; BSD tar (macOS's stock `tar`) rejects that
# flag outright and globs by default, so the second attempt is what runs
# there. A GNU tar that fails for a real reason (nothing matches) fails
# again on the retry, now with its own message on screen instead of
# swallowed by the first attempt.
extract_from_archive() {
    local archive="$1" dest="$2"
    shift 2
    if tar --wildcards -xzf "$archive" -C "$dest" "$@" 2>/dev/null; then
        return 0
    fi
    tar -xzf "$archive" -C "$dest" "$@"
}

bootstrap_main() {
    # `id` is checked before it is trusted for the root refusal below — a
    # missing binary must never silently read as "not root". A failing
    # `id -u` (missing tool, or any other reason) is refused the same as an
    # actual root uid: "cannot confirm this is not root" is not a fact to
    # proceed on.
    command -v id >/dev/null 2>&1 || { echo "ERROR: id not found." >&2; exit 1; }
    local uid
    uid="$(id -u)" || {
        echo "ERROR: id -u failed; cannot confirm this is not root. Refusing." >&2
        exit 1
    }
    # No sudo anywhere in this chain, same as install.sh: the agent installs
    # user-owned. Refused here, before a byte is downloaded, rather than
    # left for install.sh to discover deep inside its own preflight.
    if [ "$uid" = "0" ]; then
        echo "ERROR: refusing to run as root. The agent installs user-owned, with no sudo" >&2
        echo "       anywhere in this chain; run this as the user the agent should run as." >&2
        exit 1
    fi

    local ref="main"
    local install_args=()
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --ref)
                if [ "$#" -lt 2 ]; then
                    echo "ERROR: --ref needs a value (a 40-character commit SHA)." >&2
                    exit 2
                fi
                ref="$2"
                shift 2
                ;;
            --ref=*)
                ref="${1#--ref=}"
                shift
                ;;
            -h | --help | help)
                usage
                exit 0
                ;;
            *)
                install_args+=("$1")
                shift
                ;;
        esac
    done
    validate_ref "$ref" || exit 2

    for tool in curl tar mktemp basename; do
        command -v "$tool" >/dev/null 2>&1 || { echo "ERROR: $tool not found." >&2; exit 1; }
    done

    # main is trivially reachable from itself; a pinned --ref is not trusted
    # on the strength of its shape alone (see verify_ref_reachable_from_main's
    # own comment for why this matters).
    if [ "$ref" != "main" ]; then
        verify_ref_reachable_from_main "$ref" || exit 1
    fi

    # Staged under ~/.cache, exactly where install.sh stages its own
    # download, and for the same reason: 0700 from birth (umask), and never
    # /tmp, which a hardened host commonly mounts noexec — install.sh below
    # is a script this process is about to execute directly.
    #
    # `stage` is deliberately NOT `local`: the EXIT trap below is evaluated
    # when the whole script exits, which is after this function has already
    # returned — a `local` would be out of scope by then, and `$stage` would
    # read as unset under `set -u` right when the trap needs it.
    local stage_root
    stage_root="${XDG_CACHE_HOME:-$HOME/.cache}"
    # Both under the one handler: `set -e` would otherwise kill the run on a
    # bare `mkdir:` message the moment $stage_root's parent is unwritable,
    # before this function ever gets to say what it was trying to do.
    mkdir -p "$stage_root" && stage="$(umask 077 && mktemp -d "$stage_root/solador-agent-bootstrap.XXXXXX")" || {
        echo "ERROR: could not create a staging directory under $stage_root." >&2
        exit 1
    }
    # Removed once install.sh (below) returns, success or failure alike —
    # deliberately NOT an `exec` into install.sh, which would replace this
    # process and skip this trap, leaving the staged checkout behind.
    trap 'rm -rf "$stage"' EXIT

    local archive="$stage/repo.tar.gz" archive_url="$CODELOAD_URL/$ref"
    echo "==> Downloading $archive_url"
    if ! curl --proto '=https' --tlsv1.2 -fsSL -o "$archive" "$archive_url"; then
        echo "ERROR: could not download $archive_url" >&2
        if [ "$ref" != "main" ]; then
            echo "       --ref '$ref' may not name a commit on $REPO." >&2
        fi
        echo "       Check that this host can reach codeload.github.com over HTTPS." >&2
        exit 1
    fi

    echo "==> Extracting agent/deploy and the signing key(s)"
    if ! extract_from_archive "$archive" "$stage" '*/agent/deploy/*' '*/agent/release-signing-key.pub'; then
        echo "ERROR: could not extract agent/deploy or agent/release-signing-key.pub from the archive." >&2
        echo "       $archive_url may not be a Solador checkout, or its layout changed." >&2
        exit 1
    fi
    # The standby key (once committed, docs/SECRETS.md) is optional: an
    # archive of a commit before it existed carries only the one active key,
    # and that is not a reason to refuse. Never fatal, and never lets a
    # genuine extraction failure above hide behind it.
    extract_from_archive "$archive" "$stage" '*/agent/release-signing-key-next.pub' >/dev/null 2>&1 || true

    # The single directory this extraction created, e.g. "solador-<ref>" —
    # codeload names it after the resolved commit for a full SHA, but
    # literally "main" (never resolved) for the default ref — confirmed
    # against the live endpoint, not assumed.
    local top=""
    for d in "$stage"/*/; do
        top="$(basename "${d%/}")"
        break
    done
    local resolved="${top#solador-}"
    local install_sh="$stage/$top/agent/deploy/install.sh"
    if [ -z "$top" ] || [ ! -f "$install_sh" ]; then
        echo "ERROR: $install_sh not found after extraction — the archive does not have the" >&2
        echo "       expected agent/deploy layout." >&2
        exit 1
    fi
    chmod +x "$install_sh"

    # For the default `main` ref, "$resolved" above is only the word "main"
    # — not the revision an operator would need to name what they trusted.
    # The exact commit is still in the archive: codeload writes it into the
    # tar's pax global header as `comment=<sha>`. Best-effort only, and
    # never piped through a truncating command (head/`grep -m`) straight
    # from gzip's stdout: a tar stream is full of NUL padding, which would
    # cut a `$(...)` capture short long before reaching it, and a
    # downstream reader closing early under `set -o pipefail` reports
    # gzip's own SIGPIPE as this function's exit status (proven: `yes |
    # head -c5` exits 141 under pipefail even though head succeeds) — set
    # -e would then abort an otherwise-successful install over a
    # diagnostic nicety. Read from a FILE instead, and `|| true`
    # throughout: this never gates anything verify_ref_reachable_from_main
    # did not already decide.
    if [ "$ref" = "main" ]; then
        local header_bytes="$stage/archive-head" from_header=""
        gzip -dc "$archive" 2>/dev/null | head -c 4096 > "$header_bytes" 2>/dev/null || true
        if [ -s "$header_bytes" ]; then
            from_header="$(LC_ALL=C grep -am1 -o 'comment=[0-9a-f]\{40\}' "$header_bytes" 2>/dev/null | sed 's/^comment=//')" || true
        fi
        [ -n "$from_header" ] && resolved="$from_header"
    fi
    echo "==> Running agent/deploy from commit $resolved ($REPO_URL/tree/$resolved)"

    # $0 inside the extracted install.sh names a path under $stage, which is
    # gone by the time anyone could act on a hint built from it (the trap
    # above removes it the moment install.sh returns). SOLADOR_AGENT_BOOTSTRAP
    # is how install.sh knows to print "bash bootstrap.sh ..." in a re-run
    # hint instead of $0 — see install.sh's RERUN_CMD.
    export SOLADOR_AGENT_BOOTSTRAP=1
    if [ "${#install_args[@]}" -gt 0 ]; then
        "$install_sh" "${install_args[@]}"
    else
        "$install_sh"
    fi
}

bootstrap_main "$@"
