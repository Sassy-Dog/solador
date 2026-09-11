#!/usr/bin/env bash
set -euo pipefail

# The ONE implementation that minisigns an agent artifact (#390, #391).
#
# Two callers, one signer:
#   scripts/build-agent.sh   sources this and signs the four published binaries
#   publish-feed.yml         runs it to sign `agent-latest.json` at publish time
#
#   scripts/agent-signing.sh ensure              install the pinned rsign2 if
#                                                 it is not already the one on
#                                                 PATH — needs no key, so a
#                                                 workflow runs it BEFORE the
#                                                 key exists on disk
#   scripts/agent-signing.sh sign FILE [FILE…]   write FILE.minisig beside each,
#                                                 then re-verify it under the
#                                                 COMMITTED public key
#
# Same key, same pinned `rsign2`, same flags, same re-verification — extracted
# here rather than copied into the feed workflow, because a second signer in
# YAML is how the binaries and the feed would come to be signed differently, and
# a second version policy is how one of them would drift off the pin.
#
# Inputs, through the environment:
#   SOLADOR_AGENT_SIGNING_KEY   path to the minisign SECRET key file (0600, in a
#                               private temp dir; the caller removes it on exit)
# and `AGENT_SIGNING_PUBKEY` / `RSIGN_VERSION` from scripts/config.sh.
#
# Nothing here prints the key, and nothing here uploads anything.

AGENT_SIGNING_SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
# When sourced by a script that already loaded these, sourcing them again is
# harmless: every value is a plain `export`.
source "$AGENT_SIGNING_SCRIPT_DIR/lib.sh"
source "$AGENT_SIGNING_SCRIPT_DIR/config.sh"
AGENT_SIGNING_ROOT_DIR="$( cd "$AGENT_SIGNING_SCRIPT_DIR/.." && pwd )"

# The pinned minisign signer. Same shape as build.sh's `ensure_tauri_cli`: the
# check is on the INSTALLED version, not on presence, so a machine carrying some
# other project's rsign does not sign a release with it.
ensure_rsign() {
    local installed=""
    if command_exists rsign; then
        # `|| true`: under `set -euo pipefail` a failing command substitution in
        # an assignment takes the whole script down, silently when its stderr is
        # discarded. An rsign too old to answer `--version` is a case to REPORT
        # and reinstall over, not to die on.
        installed="$(rsign --version 2>/dev/null | awk 'NR == 1 { print $2 }')" || true
    fi
    if [[ "$installed" == "$RSIGN_VERSION" ]]; then
        log_debug "rsign $installed already installed"
        return 0
    fi
    if [[ -n "$installed" ]]; then
        log_warning "rsign $installed found, this repo pins $RSIGN_VERSION — replacing it"
    else
        log_info "rsign not found — installing the pinned $RSIGN_VERSION"
    fi
    cargo install --locked "rsign2@$RSIGN_VERSION"
    installed="$(rsign --version 2>/dev/null | awk 'NR == 1 { print $2 }')" || true
    if [[ "$installed" != "$RSIGN_VERSION" ]]; then
        log_error "rsign is '${installed:-<missing>}' after installing $RSIGN_VERSION — is ~/.cargo/bin on PATH?"
        exit 1
    fi
}

# The pinned signer must ALREADY be on PATH. The check-only counterpart of
# ensure_rsign, for the step that holds the key: it must never be the step
# that runs `cargo install`, and "run `ensure` first" is a sentence, where a
# fallback install would be a policy that holds only while nobody reorders
# the workflow.
require_rsign() {
    local installed=""
    if command_exists rsign; then
        installed="$(rsign --version 2>/dev/null | awk 'NR == 1 { print $2 }')" || true
    fi
    if [[ "$installed" != "$RSIGN_VERSION" ]]; then
        log_error "rsign is '${installed:-<missing>}', this repo pins $RSIGN_VERSION — run 'scripts/agent-signing.sh ensure' first, in a step that holds no key"
        exit 1
    fi
}

# Everything signing needs, checked BEFORE anything expensive: the key file
# must exist, the committed public half must exist, and the signer must be the
# pinned one. A four-target release build is tens of minutes; discovering
# afterwards that the key was never passed is the class of waste the release
# workflow's secret preflight exists to prevent.
#
# `--install` lets build-agent.sh install the signer as part of a local
# `./dev agent --sign` (the operator's own key is already on that machine);
# the `sign` subcommand never passes it.
agent_signing_preflight() {
    if [[ -z "${SOLADOR_AGENT_SIGNING_KEY:-}" || ! -f "$SOLADOR_AGENT_SIGNING_KEY" ]]; then
        log_error "signing needs SOLADOR_AGENT_SIGNING_KEY to point at the agent's minisign secret key file"
        log_error "the release reads it from the prd environment secret; see docs/AGENT-DISTRIBUTION.md §5"
        exit 1
    fi
    if [[ ! -f "$AGENT_SIGNING_ROOT_DIR/$AGENT_SIGNING_PUBKEY" ]]; then
        log_error "missing $AGENT_SIGNING_PUBKEY — the committed public half of the agent keypair"
        exit 1
    fi
    if [[ "${1:-}" == "--install" ]]; then
        ensure_rsign
    else
        require_rsign
    fi
}

# Sign one file to FILE.minisig, then verify that signature under the COMMITTED
# public key. Call agent_signing_preflight first.
agent_sign_file() {
    local artifact="$1" name key pubkey
    name="$(basename "$artifact")"
    key="$SOLADOR_AGENT_SIGNING_KEY"
    pubkey="$AGENT_SIGNING_ROOT_DIR/$AGENT_SIGNING_PUBKEY"

    if [[ ! -f "$artifact" ]]; then
        log_error "nothing to sign at $artifact"
        exit 1
    fi

    rm -f "$artifact.minisig"
    # The trusted comment is covered by the signature. It does not prevent a
    # signature being lifted onto another file — the signature over the CONTENT
    # is what does that — but it makes such a pair self-describing: a verifier
    # prints "solador-agent-<v>-<triple>" (or "agent-latest.json") and a human
    # sees immediately that it does not name the file in front of them.
    #
    # `-W` and `</dev/null`: the CI key is unencrypted (the secret store is the
    # protection), and a signer that falls back to a password prompt on a
    # runner hangs until the job times out instead of failing.
    rsign sign -W -s "$key" -x "$artifact.minisig" \
        -t "$name" -c "solador-agent release signature" "$artifact" </dev/null >/dev/null

    # Verified against the COMMITTED public key, not against the key that just
    # signed. That is the whole check: it proves the private key in CI is the
    # one this repo publishes, so a mis-provisioned secret fails the release
    # instead of shipping signatures nobody can check.
    if ! rsign verify -q -p "$pubkey" -x "$artifact.minisig" "$artifact"; then
        log_error "$name does not verify under $AGENT_SIGNING_PUBKEY"
        log_error "the signing key is not the published keypair's private half"
        exit 1
    fi
    log_success "Signed $name (verified under $(head -n1 "$pubkey" | sed 's/^untrusted comment: //'))"
}

# Executed directly (not sourced): `agent-signing.sh ensure` / `sign FILE…`.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    case "${1:-}" in
        ensure)
            # `cargo install` runs crates.io build scripts. Doing that in a
            # step that holds no secret — and before the key has been written
            # anywhere — is what keeps third-party build code away from it.
            ensure_rsign
            ;;
        sign)
            shift
            if [[ $# -eq 0 ]]; then
                log_error "sign needs at least one file"
                exit 2
            fi
            agent_signing_preflight
            for f in "$@"; do
                agent_sign_file "$f"
            done
            ;;
        -h|--help)
            awk 'NR > 3 && /^#/ { sub(/^# ?/, ""); print; next } NR > 3 { exit }' "$0"
            exit 0
            ;;
        *)
            log_error "usage: $0 ensure | sign FILE [FILE…]"
            exit 2
            ;;
    esac
fi
