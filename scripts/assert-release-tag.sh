#!/usr/bin/env bash
set -euo pipefail

# Assert that a release tag is the one scripts/publish.sh would have minted at
# HEAD — by asking the mint, not by re-deriving a number (#404).
#
#   scripts/assert-release-tag.sh vYYYY.M.P
#   scripts/assert-release-tag.sh --agent agent-vYYYY.M.P
#
# Exit 0 when the tag is the mint's own answer for this commit; exit 1 with a
# `::error` line and every cause named otherwise; exit 2 on bad usage. Run by
# every leg of .github/workflows/release.yml and by publish-feed.yml's feed job,
# checked out at the tag, before anything is built or uploaded — and, with
# `--agent`, by the build job of .github/workflows/release-agent.yml (#472, #491)
# before it builds.
#
# AGENT MODE (#490). `--agent` takes an `agent-vYYYY.M.P` tag and asks the
# AGENT mint (`get-version-info.sh --agent-tag`, read-only) instead of the
# cockpit's, requiring the same `action=reuse` for exactly that tag. The
# agent's mint is not a ladder over a commit count: its first rule is "an
# `agent-v*` tag already at HEAD on the remote is reused", so what this binds
# is that the tag NAME is a well-formed agent version, its month is one the
# mint's clock could have produced for this commit (the same two bounds as
# below), the checkout is full, and the tag exists ON THE REMOTE at THIS commit
# — a local-only tag is `action=create`, refused. UNLIKE the cockpit's ladder,
# which only ever resolves to the next number, the agent mint's reuse rule
# accepts ANY well-formed `agent-v*` tag on the remote at HEAD — including a
# number below one already shipped (a hand-pushed `agent-v2026.10.9` at HEAD
# passes while `agent-v2026.10.15` exists). That is deliberate: re-running an
# older release's workflow legitimately asserts an older tag, so refusing it
# here would break the re-run. What keeps a stale number from reaching hosts is
# elsewhere: the publish side's newest-release guard (#491), `solador-agent
# update`'s forward-only rule, and `install.sh` re-run over an installed release
# (a fresh install, and an installed `+dev` source build, have nothing to
# compare against and are not covered by that last one). The provenance of the
# commit is `prd`'s required reviewer's, not this script's.
# The agent mint's `create` path asks `gh release view`; a refused tag can reach
# it, so a refusal here may also say that `gh` could not answer — which is still
# a refusal, and the cause named is the mint's own.
#
# WHY THE MINT AND NOT `--version`. The tag names the artifact and the build
# derives its version from history; nothing forces those to agree, so the
# workflows assert it — without that, hand-tagging v2026.8.5 at a commit whose
# derived CalVer is 2026.8.109 publishes a release called v2026.8.5 containing
# Solador-2026.8.109.dmg, and a mislabelled artifact is worse than a failed
# release. Until #404 the assertion compared the tag to a bare
# `get-version-info.sh --version`, which refused two tags the mint had
# legitimately produced:
#
#   * a re-run after the UTC month rolled: `--version` counts commits since the
#     1st of TODAY'S month, so v2026.8.139 re-run on 2026-09-06 derived
#     v2026.9.1 at the same, unchanged commit (run 32732819910, attempt 2);
#   * a ladder-bumped tag on its FIRST run: the §4 mint bumps the patch when
#     the derived tag already exists at another commit (the month-roll floor
#     collision it exists to resolve), so the tag says .2 and `--version` says
#     .1.
#
# The CalVer algorithm has exactly one home (docs/VERSIONING.md: "never
# compute a version anywhere else"), and the mint already encodes both ways a
# tag's number can legitimately differ from a bare derivation. So this script
# re-runs the mint's own ladder READ-ONLY (`--tag` without `--push`), pinned to
# the tag's own month through the `VERSION_DATE_OVERRIDE` seam, and requires
# it to answer `action=reuse` for EXACTLY this tag:
#
#   * month roll: pinned to 2026-08, the derivation at that commit is .139
#     again; the tag exists at HEAD → reuse. Passes.
#   * ladder bump: .1 exists at another commit → the ladder bumps to .2, which
#     exists at HEAD → reuse. Passes.
#   * wrong commit: v2026.8.5 at a commit deriving .109 → the ladder probes
#     .109 upward and never walks DOWN to .5; it reports create, or reuse of a
#     different tag. Refused.
#
# A tolerance band (tag.patch within k of derived) would be a second, weaker
# copy of the ladder; this is the ladder itself, so the two cannot drift.
#
# WHAT THIS BINDS, AND WHAT IT DOES NOT. It binds the tag's NAME to the
# COMMIT it points at: this number is the one the ladder resolves to at this
# commit in this month, and no other. It does not establish where the commit
# came from — nothing here requires it to be on `main` (publish.sh does,
# locally, at mint time), and a committer date is author-chosen. Provenance
# is `prd`'s required reviewer's, who approves the run before any leg holds a
# credential; this check is what makes the number that reviewer sees mean
# something.
#
# WHAT IS REFUSED BEFORE THE MINT RUNS — the shapes the ladder itself cannot
# see. A name that is not `vYYYY.M.P` is a hand-made tag whatever the mint
# would say. The tag's month is bounded on BOTH sides: a month AFTER the
# current UTC month cannot have come from the mint (its clock is `date -u`,
# and pinned to a month that has not started the ladder WOULD answer reuse
# for its floor slot at any commit — zero commits floor to 1); a month that
# ENDED before HEAD's committer date cannot either (the mint tags HEAD with
# the month it runs in, never earlier — and pinned to a past month, `--since`
# counts every commit from that 1st to this later HEAD, so a hand-pushed
# vYYYY.M.K with K equal to that count would answer reuse). A shallow clone
# is refused because `git rev-list --count --since` answers 1 there instead of
# failing — the reason every one of these checkouts pins `fetch-depth: 0`. A
# missing origin is refused because the mint's probe then reads LOCAL tags,
# and a local tag is what a hand-made tag is. The replay seams
# `MARKETING_VERSION` and `VERSION_PATCH_OVERRIDE` are scrubbed from the
# mint's environment: a pin inherited from the job would make the mint echo
# it back and the check compare the tag to itself.
#
# The mint's probe is a live `git ls-remote origin`, so unlike the bare
# comparison this replaced, a network failure fails this step — closed, with
# nothing built. The remedy for that one is a re-run and nothing else.
#
# ON SUCCESS the caller pins `MARKETING_VERSION=<tag's version>` for the build,
# exactly as publish.sh pins the minted version locally (docs/VERSIONING.md
# §4 step 6): the build re-derives with the wall clock otherwise, and in the
# two cases above that re-derivation is the mislabelled artifact this check
# exists to prevent. This script only proves; the pin is the workflow's line,
# and the artifact validation after the build reads the version back out of
# the file and compares it to the tag, so a lost pin is red, not mislabelled.
#
# scripts/versioning-test.sh (§4 of it) drives all of it against a temporary
# bare origin — nothing here ever pushes a tag, and no case there does either.
# One branch it cannot reach: the "output contract was violated" refusal
# below, because the mint is the sibling script and always prints its three
# lines on exit 0. It is diagnostic, for the day that script's stdout changes.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
MINT="$SCRIPT_DIR/get-version-info.sh"

mode=desktop
if [[ "${1:-}" == "--agent" ]]; then
    mode=agent
    shift
fi
if [[ $# -ne 1 || -z "${1:-}" ]]; then
    echo "usage: $0 vYYYY.M.P | $0 --agent agent-vYYYY.M.P" >&2
    exit 2
fi
tag="$1"

# What differs between the two modes, and nothing else: the tag's name, the
# mint's flag, and the words a refusal uses. Every check below is shared.
if [[ "$mode" == "agent" ]]; then
    tag_prefix="agent-v"
    tag_shape="an agent-vYYYY.M.P"
    mint_flag="--agent-tag"
else
    tag_prefix="v"
    tag_shape="a vYYYY.M.P"
    mint_flag="--tag"
fi

# refuse REASON [MINT_STDOUT] — the `::error` line, then every cause a refusal
# here can have, so the operator reads which one this run hit rather than
# assuming the first.
refuse() {
    local reason="$1" reported="${2:-}"
    echo "::error::tag $tag refused: $reason"
    if [[ -n "$reported" ]]; then
        echo "The mint reported, pinned to the tag's month at $head:" >&2
        printf '%s\n' "$reported" | sed 's/^/    /' >&2
    fi
    if [[ "$mode" == "agent" ]]; then
        cat >&2 <<EOF
Agent tags are minted by ./dev publish --agent (scripts/publish.sh, through
get-version-info.sh --agent-tag --push), and this check re-runs that mint
read-only, pinned to the tag's own month, and requires it to REUSE exactly this
tag: an agent-v* tag already on the remote at this commit. Landing here means
one of:
  - a hand-made tag: not $tag_shape, or a month the mint's clock has
    not reached or that ended before this commit;
  - a tag on the wrong commit: the remote has no such tag at this commit (a
    tag that exists only locally is not one), so the mint at this commit would
    create a different number, or reuse a different tag that is here;
  - the pinned-month derivation could not run: a shallow clone, no origin
    remote, or the mint's remote tag probe failed (network/auth) — it never
    mints blind, and this check never passes blind. Its own lines above say
    which; a probe failure is re-run and nothing else.
EOF
    else
        cat >&2 <<EOF
Tags are minted by scripts/publish.sh (get-version-info.sh --tag --push), and
this check re-runs that ladder read-only, pinned to the tag's own month, so a
minted tag passes after the month rolls and after a ladder bump. Landing here
means one of:
  - a hand-made tag: not $tag_shape, a month the mint's clock has not reached
    or that ended before this commit, or a number the ladder never resolves
    to at this commit;
  - a tag on the wrong commit: the mint at this commit resolves a different
    tag (it probes the derived number upward and never walks down);
  - the pinned-month derivation could not run: a shallow clone, no origin
    remote, or the mint's remote tag probe failed (network/auth) — it never
    mints blind, and this check never passes blind. Its own lines above say
    which; a probe failure is re-run and nothing else.
EOF
    fi
    exit 1
}

head="$(git rev-parse 'HEAD^{commit}' 2>/dev/null || true)"
if [[ -z "$head" ]]; then
    refuse "not inside a git checkout, so the pinned-month derivation could not run"
fi

# 1. Shape. Non-padded month and patch, both at least 1, exactly what the mint
#    emits (`YYYY.M.P`, floored at 1 — never X.Y.0, never 08, never 0999).
if [[ "$tag" != "$tag_prefix"* || ! "${tag#"$tag_prefix"}" =~ ^([1-9][0-9]{3})\.([1-9]|1[0-2])\.([1-9][0-9]*)$ ]]; then
    refuse "not $tag_shape CalVer tag (a hand-made tag)"
fi
year="${BASH_REMATCH[1]}"
month="${BASH_REMATCH[2]}"
tag_month=$((year * 100 + month))

# 2. The tag's month must have started, by the mint's own clock (UTC).
now_month=$(( $(date -u +%Y) * 100 + 10#$(date -u +%m) ))
if (( tag_month > now_month )); then
    refuse "its month $year.$month is after the current UTC month, which the mint's clock never reaches (a hand-made tag)"
fi

# 3. ...and must not have ENDED before this commit was made. The mint tags
#    HEAD with the month it runs in, and it cannot run before its own HEAD
#    exists, so a tag's month is never earlier than its commit's (UTC
#    committer date — the same clock `rev-list --since` counts by). Without
#    this bound a past month is a blind spot the ladder cannot see: pinned to
#    it, `--since` counts EVERY commit from that 1st up to this later HEAD, so
#    a hand-pushed vYYYY.M.K with K equal to that count answers `reuse` at a
#    commit made months later. The two legitimate shapes both survive: an
#    August tag at an August commit, and September's floor slot at an August
#    commit (the post-roll release the ladder exists for).
#
#    Asked the way the mint counts — `rev-list --since` on an ISO UTC instant
#    — rather than by formatting the committer date: `--since` skips a HEAD
#    older than the instant outright, so the count is 1 when HEAD was made
#    at or after the 1st of the following month and 0 when it was not, with
#    no timezone, locale, `log.showSignature` or `date(1)` dialect in the way.
if (( month == 12 )); then
    month_end="$((year + 1))-01-01T00:00:00Z"
else
    month_end="$(printf '%04d-%02d-01T00:00:00Z' "$year" "$((month + 1))")"
fi
case "$(git rev-list --count --max-count=1 --since="$month_end" "$head" 2>/dev/null || echo unknown)" in
    0) ;;
    1) refuse "its month $year.$month ended (at $month_end) before this commit was made, which the mint never does (a hand-made tag, or a tag on the wrong commit)" ;;
    *) refuse "git could not place HEAD's committer date against $month_end, so the pinned-month derivation could not run" ;;
esac

# 4. A shallow clone answers "commits this month" with 1 rather than failing.
if [[ "$(git rev-parse --is-shallow-repository 2>/dev/null || echo unknown)" != "false" ]]; then
    refuse "this checkout is shallow (or git could not say), so the pinned-month derivation could not run — check out with fetch-depth: 0"
fi

# 5. The mint's probe is remote-visible only when an origin exists; without
#    one it falls back to LOCAL tags, and a local tag is what a hand-made tag
#    is. Required rather than assumed, so "remote-visible" holds by
#    construction. actions/checkout always configures one.
if ! git remote get-url origin >/dev/null 2>&1; then
    refuse "no origin remote to probe tags on, so the pinned-month derivation could not run remote-visibly"
fi

# 6. The mint, read-only, pinned to the 1st of the tag's month. Its narration
#    (the ladder's probe/bump/reuse lines) goes to stderr as it runs; stdout is
#    the contract publish.sh parses, captured here for the same two fields.
pin="$(printf '%04d-%02d-01' "$year" "$month")"
echo "Re-running the mint read-only at $head, pinned to $pin (VERSION_DATE_OVERRIDE), for $tag"
if ! reported="$(env -u MARKETING_VERSION -u VERSION_PATCH_OVERRIDE \
        VERSION_DATE_OVERRIDE="$pin" "${BASH:-bash}" "$MINT" "$mint_flag")"; then
    refuse "the pinned-month derivation could not run (the mint exited non-zero; its reason is above)"
fi
reported_tag="$(printf '%s\n' "$reported" | sed -n 's/^tag=//p')"
reported_action="$(printf '%s\n' "$reported" | sed -n 's/^action=//p')"
if [[ -z "$reported_tag" || -z "$reported_action" ]]; then
    refuse "the mint's output contract was violated (no tag=/action= lines)" "$reported"
fi

# 7. Exactly this tag, already at HEAD. `reuse` of a different tag is the
#    wrong-commit case as much as `create` is.
if [[ "$reported_action" != "reuse" || "$reported_tag" != "$tag" ]]; then
    refuse "the mint resolves $reported_tag ($reported_action) at this commit, not $tag" "$reported"
fi

echo "Tag $tag is the mint's own answer at $head pinned to $pin (action=reuse)."
