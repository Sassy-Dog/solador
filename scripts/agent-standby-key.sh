#!/usr/bin/env bash
set -euo pipefail

# Provision the agent's STANDBY release-signing key (#393 §A).
#
#   scripts/agent-standby-key.sh [--project solador] [--config custody] [--repo Sassy-Dog/solador]
#
# The updater compiled into `solador-agent` (#393) trusts TWO public keys:
# `agent/release-signing-key.pub` — the key every release is signed under
# today, whose private half is Doppler `solador/prd`'s
# SOLADOR_AGENT_SIGNING_PRIVATE_KEY, synced to the GitHub `prd` environment —
# and `agent/release-signing-key-next.pub`, a STANDBY whose private half is
# held in a Doppler config that syncs NOWHERE. Two accepted keys are what make
# a rotation a release rather than a recall: if the active key is lost or
# compromised, releases switch to the standby every deployed agent already
# trusts. This script mints the standby, exactly once, and proves custody.
#
# What it does, in order — each step refuses before the next changes anything:
#   1. Preflight: `doppler`, `gh` and the stock `minisign` on PATH; the pinned
#      `rsign2` (installed if missing, in a step that holds no key). The target
#      config must exist, must not be `prd`, and must show no active sync in
#      its audit log.
#   2. Looks for SOLADOR_AGENT_SIGNING_STANDBY_PRIVATE_KEY in the target
#      config BY NAME ONLY (`--only-names`; no value is ever read here). An
#      existing standby is never overwritten or rotated by a re-run.
#   3. If absent (and no local public file exists either): generates a fresh
#      minisign keypair with the pinned `rsign2` in a mode-0700 temp dir,
#      writes the PUBLIC half to agent/release-signing-key-next.pub, and
#      uploads the PRIVATE half to Doppler on stdin — never on argv, never
#      printed.
#   4. Proves custody with the value RETRIEVED FROM DOPPLER, not the file it
#      just generated: signs non-secret fixture bytes with it and verifies the
#      signature with the stock `minisign` under the committed public file;
#      shows the same signature FAILS under the current key and that an
#      unrelated key's signature FAILS under the standby's file.
#   5. Confirms, by secret-NAME metadata, that the standby is NOT in the
#      GitHub `prd` environment, the repository's Actions secrets, or the
#      organisation's. A listing that cannot be read is a refusal, never an
#      "absent".
#   6. Runs the updater's own trust-set test, so "two distinct real keys are
#      embedded" is asserted by the binary that will verify under them.
#   7. Removes every temporary copy of the private key on every exit path.
#
# Re-running is safe and idempotent: with the secret present and the public
# file committed, step 3 is skipped and 4–6 run again. The two half-states
# (secret without file, file without secret) are REFUSED with the recovery
# named, never repaired by guessing.
#
# Nothing here prints a private key, puts one on a command line, or writes
# one outside the temp dir. `set -x` must never be enabled in this file.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"
# `ensure_rsign` / `require_rsign`: the ONE pinned signer, shared with the
# release. Sourcing it defines functions only — its CLI dispatch is guarded.
source "$SCRIPT_DIR/agent-signing.sh"
ROOT_DIR="$( cd "$SCRIPT_DIR/.." && pwd )"

# Belt and braces on the one rule this file exists to keep: even a caller
# that ran us under `bash -x` traces nothing past this line.
set +x

STANDBY_NAME="SOLADOR_AGENT_SIGNING_STANDBY_PRIVATE_KEY"
ACTIVE_NAME="SOLADOR_AGENT_SIGNING_PRIVATE_KEY"
NEXT_PUB_REL="agent/release-signing-key-next.pub"
NEXT_PUB="$ROOT_DIR/$NEXT_PUB_REL"
CURRENT_PUB="$ROOT_DIR/$AGENT_SIGNING_PUBKEY"

PROJECT="solador"
CONFIG="custody"
REPO="Sassy-Dog/solador"

while [[ $# -gt 0 ]]; do
    case $1 in
        --project) PROJECT="${2:-}"; [[ -n "$PROJECT" ]] || { log_error "--project needs a name"; exit 2; }; shift 2 ;;
        --config)  CONFIG="${2:-}";  [[ -n "$CONFIG" ]]  || { log_error "--config needs a name"; exit 2; };  shift 2 ;;
        --repo)    REPO="${2:-}";    [[ -n "$REPO" ]]    || { log_error "--repo needs owner/name"; exit 2; }; shift 2 ;;
        -h|--help)
            awk 'NR > 3 && /^#/ { sub(/^# ?/, ""); print; next } NR > 3 { exit }' "$0"
            exit 0
            ;;
        *)
            log_error "unknown argument: $1"
            exit 2
            ;;
    esac
done

# ---------------------------------------------------------------------------
# The private temp dir, and its removal on every exit path
# ---------------------------------------------------------------------------

# Armed BEFORE anything is written into it. Every private-key byte this
# script ever holds on disk lives under here and nowhere else.
# With a template: stock macOS `mktemp -d` without one ignores TMPDIR, and
# the test harness proves the cleanup by pointing TMPDIR at a directory it
# can look into afterwards.
KEYDIR="$(mktemp -d "${TMPDIR:-/tmp}/solador-standby.XXXXXX")"
chmod 700 "$KEYDIR"
cleanup() {
    rm -rf "$KEYDIR"
}
trap cleanup EXIT
umask 077

# ---------------------------------------------------------------------------
# 1. Preflight
# ---------------------------------------------------------------------------

for tool in doppler gh minisign openssl od sed grep head; do
    command_exists "$tool" || { log_error "$tool not found on PATH"; exit 1; }
done
command_exists cargo || { log_error "cargo not found on PATH — the pinned rsign2 is installed with it, and the updater's trust-set test runs with it"; exit 1; }
# It must BE minisign: rsign answers `-V` with its version and exit 0, so a
# `minisign -> rsign` symlink would be an accept-everything verifier and the
# custody proof below would prove nothing.
minisign_ident="$(minisign -v 2>/dev/null | head -n1 || true)"
case "$minisign_ident" in
    "minisign "*) ;;
    *) log_error "the 'minisign' on PATH does not identify itself as minisign (got: ${minisign_ident:-<nothing>})"; exit 1 ;;
esac
ensure_rsign
require_rsign

# The key id `minisign -V` prints, read out of the KEY BYTES — eight bytes
# after the two-byte algorithm tag, little-endian, uppercase hex — never out
# of the comment above them. The comment is untrusted by minisign's own
# naming, and its spelling differs between the two implementations (`rsign`
# writes `public key:`, the stock tool `public key`); the bytes do not.
# `crates/updatefeed::agent::key_id` and `solador-agent`'s
# `update::key_id` are the same arithmetic in Rust.
pubkey_id() {
    local b64 hex id="" i
    b64="$(sed -n '2p' "$1" | tr -d '[:space:]')"
    hex="$(printf '%s' "$b64" | openssl base64 -d -A 2>/dev/null | od -An -tx1 -v | tr -d ' \n')"
    [[ "${#hex}" -eq 84 ]] || return 1
    for i in 9 8 7 6 5 4 3 2; do
        id="$id${hex:$((i * 2)):2}"
    done
    printf '%s\n' "$id" | tr 'abcdef' 'ABCDEF'
}

[[ -f "$CURRENT_PUB" ]] || { log_error "missing $AGENT_SIGNING_PUBKEY — run from a checkout"; exit 1; }
current_id="$(pubkey_id "$CURRENT_PUB")" || { log_error "$AGENT_SIGNING_PUBKEY is not a minisign public key (42 bytes on line 2)"; exit 1; }

if [[ "$CONFIG" == "prd" ]]; then
    log_error "the standby must not live in '$PROJECT/prd' — that config holds the ACTIVE key and syncs to GitHub's prd environment"
    log_error "provision it into a config that syncs nowhere (default: custody)"
    exit 1
fi

log_info "Custody home: Doppler $PROJECT/$CONFIG (must exist, must sync nowhere)"
if ! doppler configs get "$CONFIG" --project "$PROJECT" --json >/dev/null 2>&1; then
    log_error "Doppler config '$PROJECT/$CONFIG' does not exist (or you are not signed in: doppler login)"
    log_error "create it deliberately — an environment with no integration, never a clone of prd:"
    log_error "    doppler environments create \"Custody (never synced)\" $CONFIG --project $PROJECT"
    log_error "then re-run. This script creates nothing in Doppler except the one secret."
    exit 1
fi
# The audit log's newest integration event says whether a sync is live:
# "Added GitHub, Actions: … integration" with no later "Removed …" is a
# synced config, and the standby must never be uploaded into one. The log
# is newest-first and PAGED (20 entries by default), so it is walked page
# by page until an integration event or the end of the log — the event
# that matters is whichever came last, however deep. A page that cannot
# be read is a refusal: this gate decides whether the upload below is
# safe, and "could not check" is not "no sync". Names only — the log's
# `diff` field is never printed.
sync_state=""
page=1
while :; do
    if ! page_json="$(doppler configs logs --project "$PROJECT" --config "$CONFIG" --json --number 100 --page "$page" 2>/dev/null)"; then
        log_error "could not read $PROJECT/$CONFIG's audit log (page $page); refusing to assume it syncs nowhere"
        exit 1
    fi
    sync_state="$(grep -o '"text":"[^"]*integration[^"]*"' <<< "$page_json" | head -n1 || true)"
    [[ -n "$sync_state" ]] && break
    # An empty page — `[]`, or an object with no entries — is the end.
    if ! grep -q '"text":' <<< "$page_json"; then
        break
    fi
    page=$((page + 1))
    if [[ "$page" -gt 50 ]]; then
        log_error "$PROJECT/$CONFIG's audit log runs past 50 pages with no integration event; refusing to assume"
        exit 1
    fi
done
# A positive list: no integration event at all, or a newest one that says
# "Removed … integration", is a config that syncs nowhere. "Added" is a live
# sync, and any other verb — "Updated", "Enabled", one nobody has seen — is
# refused and printed rather than read as clean.
case "$sync_state" in
    "") ;;
    *'"text":"Removed '*) ;;
    *'"text":"Added '*)
        log_error "$PROJECT/$CONFIG has an active sync: ${sync_state#*:}"
        log_error "the standby must live in a config that syncs nowhere; remove the integration or pick another config"
        exit 1
        ;;
    *)
        log_error "$PROJECT/$CONFIG's newest integration event is one this script does not classify: ${sync_state#*:}"
        log_error "refusing to assume it syncs nowhere; inspect the config's integrations in the Doppler dashboard"
        exit 1
        ;;
esac
log_success "$PROJECT/$CONFIG exists and its audit log shows no active sync"

# ---------------------------------------------------------------------------
# 2. Does a standby already exist? By NAME only.
# ---------------------------------------------------------------------------

# `--only-names --json` is `{"NAME":{},…}` — names and nothing else, and a
# shape a fixed string can be looked up in without a JSON parser.
if ! names_json="$(doppler secrets --project "$PROJECT" --config "$CONFIG" --only-names --json 2>/dev/null)"; then
    log_error "could not list secret names in $PROJECT/$CONFIG"
    exit 1
fi
if grep -qF "\"$STANDBY_NAME\":" <<< "$names_json"; then
    have_secret=true
else
    have_secret=false
fi
if [[ -f "$NEXT_PUB" ]]; then
    have_pub=true
else
    have_pub=false
fi

# ---------------------------------------------------------------------------
# 3. Generate + upload, or resume, or refuse a half-state
# ---------------------------------------------------------------------------

if [[ "$have_secret" == true && "$have_pub" == true ]]; then
    log_info "$STANDBY_NAME already exists in $PROJECT/$CONFIG and $NEXT_PUB_REL is present — reusing, not rotating"
elif [[ "$have_secret" == true && "$have_pub" == false ]]; then
    log_error "$STANDBY_NAME exists in $PROJECT/$CONFIG but $NEXT_PUB_REL is not in this checkout"
    log_error "refusing to guess: restore the committed public file (git log -- $NEXT_PUB_REL, or the"
    log_error "branch that provisioned it), or — only if that key was never trusted by a release —"
    log_error "delete the Doppler secret deliberately and re-run to mint a fresh pair. Nothing was changed."
    exit 1
elif [[ "$have_secret" == false && "$have_pub" == true ]]; then
    log_error "$NEXT_PUB_REL exists but $PROJECT/$CONFIG holds no $STANDBY_NAME"
    log_error "a public key with no private half in custody is worthless and must not be trusted:"
    log_error "remove the local file deliberately and re-run to mint a fresh pair. Nothing was changed."
    exit 1
else
    log_info "No standby yet — generating a fresh keypair with the pinned rsign2 $RSIGN_VERSION"
    # -W: unencrypted, because the secret store is the protection (same as
    # the active key). </dev/null so a signer that wanted a password would
    # fail rather than hang. Its stdout is discarded: it prints nothing
    # secret, and nothing is the safest thing to print.
    rsign generate -W -f -p "$KEYDIR/next.pub" -s "$KEYDIR/next.key" \
        -c "Solador agent release signing STANDBY key (#393)" </dev/null >/dev/null
    [[ -s "$KEYDIR/next.key" && -s "$KEYDIR/next.pub" ]] || { log_error "rsign generate produced no keypair"; exit 1; }
    next_id="$(pubkey_id "$KEYDIR/next.pub")" || { log_error "the generated public key is not a minisign public key"; exit 1; }
    if [[ "$next_id" == "$current_id" ]]; then
        log_error "the generated key has the SAME id as the current key ($current_id) — refusing (this cannot happen with a fresh keypair)"
        exit 1
    fi

    # The public half, committed. Same comment shape as the current key's so
    # `crates/updatefeed`'s comment-agrees test and a human reading the file
    # both find the id on line 1; the key line is rsign's, untouched.
    {
        printf 'untrusted comment: minisign public key: %s — Solador agent release signing STANDBY key (rotation window, #393; separate from the desktop app updater key)\n' "$next_id"
        sed -n '2p' "$KEYDIR/next.pub"
    } > "$NEXT_PUB.tmp"
    chmod 644 "$NEXT_PUB.tmp"
    mv -f "$NEXT_PUB.tmp" "$NEXT_PUB"
    log_success "Wrote $NEXT_PUB_REL (key id $next_id)"

    # The private half, to Doppler, on STDIN. Not argv (ps, shell history,
    # audit logs), not an env var, not a file Doppler is pointed at. The
    # CLI's own output is discarded: `secrets set` echoes what it set.
    log_info "Uploading $STANDBY_NAME to $PROJECT/$CONFIG (value on stdin, never printed)"
    if ! doppler secrets set "$STANDBY_NAME" --project "$PROJECT" --config "$CONFIG" --silent \
        < "$KEYDIR/next.key" >/dev/null; then
        log_error "doppler secrets set failed; the public file was written but NO standby is in custody"
        log_error "remove $NEXT_PUB_REL and re-run"
        exit 1
    fi
    log_success "Uploaded $STANDBY_NAME"
    have_secret=true
    have_pub=true
fi

# ---------------------------------------------------------------------------
# 4. Custody proof, with the value retrieved from Doppler
# ---------------------------------------------------------------------------

next_id="$(pubkey_id "$NEXT_PUB")" || { log_error "$NEXT_PUB_REL is not a minisign public key (42 bytes on line 2)"; exit 1; }
if [[ "$next_id" == "$current_id" ]] \
    || [[ "$(sed -n '2p' "$NEXT_PUB")" == "$(sed -n '2p' "$CURRENT_PUB")" ]]; then
    log_error "$NEXT_PUB_REL is the CURRENT key ($current_id) — one key listed twice is not a rotation window"
    exit 1
fi
# The comment on line 1 must agree with the bytes on line 2, the way
# `crates/updatefeed` holds the current key's file to its comment.
if ! sed -n '1p' "$NEXT_PUB" | grep -qF "$next_id"; then
    log_error "$NEXT_PUB_REL's comment does not name the id its bytes carry ($next_id)"
    exit 1
fi

log_info "Retrieving $STANDBY_NAME from $PROJECT/$CONFIG into the private temp dir (never printed)"
if ! doppler secrets get "$STANDBY_NAME" --project "$PROJECT" --config "$CONFIG" --plain --raw \
    > "$KEYDIR/retrieved.key" 2>/dev/null; then
    log_error "could not retrieve $STANDBY_NAME from $PROJECT/$CONFIG"
    exit 1
fi
lines="$(grep -c '' "$KEYDIR/retrieved.key" || true)"
first="$(head -n1 "$KEYDIR/retrieved.key")"
if [[ "$lines" -lt 2 || "$first" != untrusted\ comment:* ]]; then
    log_error "the retrieved value is not a minisign secret key (expected two lines beginning 'untrusted comment:'; got $lines line(s))"
    exit 1
fi

# Non-secret fixture bytes, made here; only the signatures over them are
# meaningful, and nothing below is retained.
fixture="$KEYDIR/custody-fixture"
printf 'solador-agent standby custody proof: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$fixture"
rsign sign -W -s "$KEYDIR/retrieved.key" -x "$fixture.minisig" \
    -t "custody-fixture" -c "solador-agent standby custody proof" "$fixture" </dev/null >/dev/null

if minisign -Vq -m "$fixture" -x "$fixture.minisig" -p "$NEXT_PUB"; then
    log_success "custody proven: a signature made with the RETRIEVED standby verifies under $NEXT_PUB_REL ($next_id)"
else
    log_error "CUSTODY PROOF FAILED: the standby retrieved from $PROJECT/$CONFIG does not match $NEXT_PUB_REL"
    log_error "the private half in Doppler and the committed public half are not a pair; do not commit"
    exit 1
fi
if minisign -Vq -m "$fixture" -x "$fixture.minisig" -p "$CURRENT_PUB" 2>/dev/null; then
    log_error "the standby's signature ALSO verifies under the current key $current_id — not a distinct key"
    exit 1
fi
log_success "distinct identities: the same signature does NOT verify under the current key ($current_id)"

# An unrelated key's signature must fail under the standby's file — the
# verifier is verifying, not saying yes.
rsign generate -W -f -p "$KEYDIR/unrelated.pub" -s "$KEYDIR/unrelated.key" -c "unrelated" </dev/null >/dev/null
rsign sign -W -s "$KEYDIR/unrelated.key" -x "$fixture.unrelated.minisig" \
    -t "custody-fixture" -c "unrelated" "$fixture" </dev/null >/dev/null
if minisign -Vq -m "$fixture" -x "$fixture.unrelated.minisig" -p "$NEXT_PUB" 2>/dev/null; then
    log_error "an UNRELATED key's signature verified under $NEXT_PUB_REL — the verifier is not verifying"
    exit 1
fi
log_success "an unrelated key's signature is rejected under $NEXT_PUB_REL"

# ---------------------------------------------------------------------------
# 5. GitHub: the standby must be in NO routine release scope (names only)
# ---------------------------------------------------------------------------

# `--paginate` on every listing: GitHub answers 30 names a page, the
# organisation already holds more than that, and a name that sorts past the
# first page is exactly the one this scan exists to find. gh applies `--jq`
# per page and concatenates. The exit status is checked separately from the
# output: an empty listing is a legitimate "no secrets", and only the status
# says whether it was read at all.
if ! prd_names="$(gh api --paginate "repos/$REPO/environments/prd/secrets" --jq '.secrets[].name' 2>/dev/null)"; then
    log_error "could not list the GitHub prd environment's secret names for $REPO (gh auth status?); refusing to assume the standby is absent"
    exit 1
fi
if [[ -z "$prd_names" ]]; then
    log_error "the GitHub prd environment on $REPO lists NO secrets — the active key's sync is broken, or this is the wrong repository"
    exit 1
fi
if grep -qx "$STANDBY_NAME" <<< "$prd_names"; then
    log_error "$STANDBY_NAME IS in $REPO's prd environment — the custody config is synced, or someone copied it"
    log_error "remove it there and remove the sync; the standby must not be reachable by a routine release job"
    exit 1
fi
if grep -qx "$ACTIVE_NAME" <<< "$prd_names"; then
    log_success "GitHub prd environment: $ACTIVE_NAME present, $STANDBY_NAME absent"
else
    log_warning "GitHub prd environment: $STANDBY_NAME absent, but $ACTIVE_NAME is NOT there either — the active key's sync looks broken; investigate before releasing"
fi
if ! repo_names="$(gh api --paginate "repos/$REPO/actions/secrets" --jq '.secrets[].name' 2>/dev/null)"; then
    log_error "could not list $REPO's repository Actions secret names; refusing to assume the standby is absent"
    exit 1
fi
if grep -qx "$STANDBY_NAME" <<< "$repo_names"; then
    log_error "$STANDBY_NAME IS a repository Actions secret on $REPO — remove it and whatever put it there"
    exit 1
fi
log_success "GitHub repository secrets: $STANDBY_NAME absent"
ORG="${REPO%%/*}"
if ! org_names="$(gh api --paginate "orgs/$ORG/actions/secrets" --jq '.secrets[].name' 2>/dev/null)"; then
    log_error "could not list the $ORG organisation's Actions secret names (needs admin:org); refusing to assume the standby is absent"
    exit 1
fi
if grep -qx "$STANDBY_NAME" <<< "$org_names"; then
    log_error "$STANDBY_NAME IS an organisation Actions secret on $ORG — remove it and whatever put it there"
    exit 1
fi
log_success "GitHub organisation secrets: $STANDBY_NAME absent"

# ---------------------------------------------------------------------------
# 6. The updater's own assertion over the compiled-in set
# ---------------------------------------------------------------------------

# Every private-key byte is gone BEFORE cargo runs anything: a test build
# runs third-party build scripts, and the repo's own rule (agent-signing.sh)
# is that such a step holds no secret. The proof above is complete; nothing
# after it needs a key.
rm -f "$KEYDIR"/*.key
log_info "cargo test: the compiled-in trust set is the two committed files, both decode, and they are distinct…"
if ( cd "$ROOT_DIR" && cargo test --locked -q -p solador-agent --lib \
    update::tests::the_compiled_in_trust_set_is_the_committed_key_files_and_they_are_distinct >/dev/null 2>&1 ); then
    log_success "solador-agent embeds $current_id and $next_id as two distinct trusted keys"
else
    log_error "the updater's trust-set test FAILED with $NEXT_PUB_REL in place — do not commit; run it by hand to see why"
    exit 1
fi

echo
log_success "Standby provisioned and custody proven."
echo "  Doppler:  $PROJECT/$CONFIG → $STANDBY_NAME   (syncs nowhere; NOT the active $PROJECT/prd → $ACTIVE_NAME)"
echo "  Commit:   git add $NEXT_PUB_REL   (public half only; the private half exists in Doppler and nowhere else)"
echo "  Key ids:  current $current_id, standby $next_id"
echo "  Not done: no release signs with the standby yet — see docs/AGENT-DISTRIBUTION.md §5, 'Rotation'"
