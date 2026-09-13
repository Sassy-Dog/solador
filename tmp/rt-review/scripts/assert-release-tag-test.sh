#!/usr/bin/env bash
set -euo pipefail

# Proves scripts/assert-release-tag.sh passes what the mint produced and
# refuses what it did not (#404), against a TEMPORARY bare origin.
#
# Dependency-free bash in the shape of agent/deploy/lib_test.sh: builds a
# throwaway repository with commits dated into two months, mints tags into a
# scratch bare origin with the REAL `get-version-info.sh --tag --push` (the
# push goes to that scratch origin and nowhere else), and runs THE SAME
# assertion script the release workflows run. Run by ci.yml's `agent-tests`
# job (Linux, bash 5), its `rust-workspace` job (macOS, stock /bin/bash 3.2 —
# the interpreter a macOS release runner and `./dev publish` execute under)
# and its `windows-tests` job (Git Bash — the Windows release leg's), by
# `./dev test`, and by hand:
#
#   scripts/assert-release-tag-test.sh
#
# The script under test and the mint run under "$BASH" — the interpreter THIS
# harness was started with — so the 3.2 leg tests the script under 3.2 and
# not under whichever bash is first on PATH.
#
# The three outcomes #404 names are the spine; the rest are the fail-closed
# edges around them. Two cases are negative controls on the FIXTURE, not on
# the script: they show the bare `--version` the old assertion compared
# against does disagree with the tag here, so the pass cases pass because of
# the pinned-month ladder and not because the fixture never reproduced the
# failure.
#
# This is also the first executable coverage of the mint's ladder (the
# month-roll reuse and the collision bump vectors docs/VERSIONING.md §3 lists
# as owed to #405); #405 absorbs this file into the fuller harness.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
ASSERT="$SCRIPT_DIR/assert-release-tag.sh"
MINT="$SCRIPT_DIR/get-version-info.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

pass=0
fail=0

# The fixture's calendar. Real dates in the past, so `date -u` in the script
# under test is always after them; only the future-month cases look at the
# real clock, and they derive their tags from it.
AUG=2026-08
SEP=2026-09

# commit REPO DATE MESSAGE — an empty commit with author AND committer date
# pinned (`rev-list --since` and the assertion's lower bound both read the
# committer date).
commit() {
    local repo="$1" date="$2" msg="$3"
    ( cd "$repo" && GIT_AUTHOR_DATE="${date}T12:00:00Z" GIT_COMMITTER_DATE="${date}T12:00:00Z" \
        git commit -q --allow-empty -m "$msg" )
}

# mint REPO TODAY — publish.sh's mint, as run on TODAY, pushing to the scratch
# origin. Prints the resolved tag. Its narration is kept in $work/mint.err
# and shown by `fixture` on failure, so a mint that breaks under a future
# change is a readable FAIL rather than a silent abort under `set -e`.
mint() {
    local repo="$1" today="$2"
    ( cd "$repo" && env -u MARKETING_VERSION -u VERSION_PATCH_OVERRIDE VERSION_DATE_OVERRIDE="$today" "$BASH" "$MINT" --tag --push 2>"$work/mint.err" ) \
        | sed -n 's/^tag=//p'
}

# bare_version REPO TODAY — the wall-clock derivation the old assertion used.
bare_version() {
    ( cd "$1" && env -u MARKETING_VERSION -u VERSION_PATCH_OVERRIDE VERSION_DATE_OVERRIDE="$2" "$BASH" "$MINT" --version )
}

# fixture LABEL — the previous fixture command (a mint, a push) failed; the
# scenario cannot be built, so nothing below it means anything. Ends the run.
fixture() {
    echo "FAIL fixture: $1"
    [[ -s "$work/mint.err" ]] && sed 's/^/     /' "$work/mint.err"
    exit 1
}

# expect VERDICT LABEL REPO TAG [ENV...] — run the assertion in REPO for TAG
# with any extra ENV assignments, and compare. A refusal is exit 1 WITH a
# `::error` line: any other non-zero exit is the script crashing, which must
# not score as a refusal. On a refusal every cause must be named, because the
# operator reading a red run is meant to find theirs in the list.
last_out=""
expect() {
    local verdict="$1" label="$2" repo="$3" tag="$4"; shift 4
    local out rc=0 ok=false
    out="$( cd "$repo" && env "$@" "$BASH" "$ASSERT" "$tag" 2>&1 )" || rc=$?
    last_out="$out"
    case "$verdict" in
        pass)
            [[ $rc -eq 0 ]] && grep -q "is the mint's own answer" <<< "$out" && ok=true
            ;;
        refuse)
            if [[ $rc -eq 1 ]] && grep -q '^::error::' <<< "$out" \
                && grep -q -- '- a hand-made tag:' <<< "$out" \
                && grep -q -- '- a tag on the wrong commit:' <<< "$out" \
                && grep -q -- '- the pinned-month derivation could not run:' <<< "$out"; then
                ok=true
            fi
            ;;
    esac
    if $ok; then
        echo "ok   $label ($verdict)"
        pass=$((pass + 1))
    else
        echo "FAIL $label: expected $verdict, exit $rc"
        echo "$out" | sed 's/^/     /'
        fail=$((fail + 1))
    fi
}

# expect_mentions REGEX LABEL — the previous expect's output names the cause
# that applied, not just the list.
expect_mentions() {
    if grep -qE -- "$1" <<< "$last_out"; then
        echo "ok   $2"
        pass=$((pass + 1))
    else
        echo "FAIL $2: output does not match /$1/"
        echo "$last_out" | sed 's/^/     /'
        fail=$((fail + 1))
    fi
}

# expect_absent REGEX LABEL — the previous expect's output does NOT contain
# it: a refusal that must land before the mint runs must not show the mint
# running.
expect_absent() {
    if ! grep -qE -- "$1" <<< "$last_out"; then
        echo "ok   $2"
        pass=$((pass + 1))
    else
        echo "FAIL $2: output matches /$1/"
        echo "$last_out" | sed 's/^/     /'
        fail=$((fail + 1))
    fi
}

# control EXPECTED ACTUAL LABEL — a fixture assertion: the scenario is the one
# it claims to be.
control() {
    if [[ "$1" == "$2" ]]; then
        echo "ok   $3"
        pass=$((pass + 1))
    else
        echo "FAIL $3: expected '$1', got '$2'"
        fail=$((fail + 1))
    fi
}

# --- the fixture -------------------------------------------------------------
origin="$work/origin.git"
repo="$work/work"
git init -q --bare "$origin"
git init -q -b main "$repo"
( cd "$repo" && git config user.email test@example.com && git config user.name test \
    && git remote add origin "$origin" )

# Five commits in August. Derived in August: 2026.8.5.
for i in 1 2 3 4 5; do commit "$repo" "$AUG-0$i" "aug $i"; done
( cd "$repo" && git push -q origin main )
aug_head="$( cd "$repo" && git rev-parse HEAD )"

control "2026.8.5" "$(bare_version "$repo" "$AUG-20")" "fixture: August HEAD derives 2026.8.5 in August"

# --- (a) month roll -----------------------------------------------------------
# publish.sh mints v2026.8.5 on the 20th. The release is re-run (or the draft
# published) in September, when the bare derivation at the SAME commit says
# 2026.9.1 — the failure of run 32732819910, attempt 2.
tag_a="$(mint "$repo" "$AUG-20")" || fixture "the mint could not tag the August HEAD"
control "v2026.8.5" "$tag_a" "fixture: the mint produced v2026.8.5 at the August HEAD"
control "2026.9.1" "$(bare_version "$repo" "$SEP-06")" "control: the bare derivation the old assertion compared says 2026.9.1 after the roll"
expect pass "(a) a tag minted in August, asserted after the month rolled" "$repo" "$tag_a"

# The everyday case, for completeness: same month, no bump, tag == derivation.
expect pass "same-month tag with no bump (the everyday path)" "$repo" "$tag_a"

# --- (b) ladder bump ----------------------------------------------------------
# A post-roll release of the August HEAD on Sep 2 mints the September floor
# slot, v2026.9.1, at the AUGUST commit. The month's first real commit then
# derives 2026.9.1 again, and publish.sh's ladder bumps it to v2026.9.2. The
# old assertion at that commit compared the tag to the unbumped 2026.9.1.
tag_floor="$(mint "$repo" "$SEP-02")" || fixture "the mint could not take the September floor slot"
control "v2026.9.1" "$tag_floor" "fixture: a post-roll release of the August HEAD mints the floor slot v2026.9.1"
commit "$repo" "$SEP-03" "sep 1"
( cd "$repo" && git push -q origin main )
sep_head="$( cd "$repo" && git rev-parse HEAD )"
tag_b="$(mint "$repo" "$SEP-03")" || fixture "the mint could not tag the September commit"
control "v2026.9.2" "$tag_b" "fixture: the first September commit ladder-bumps to v2026.9.2"
control "2026.9.1" "$(bare_version "$repo" "$SEP-03")" "control: the bare derivation at that commit is the unbumped 2026.9.1"
expect pass "(b) a ladder-bumped tag on its first run" "$repo" "$tag_b"

# The floor-slot tag itself, at the August commit it names, still passes when
# that release is re-run: pinned to September, the August HEAD derives .1,
# which exists at HEAD. This is also the lower bound's legitimate edge — a
# tag whose month is LATER than its commit's.
( cd "$repo" && git checkout -q "$tag_floor" )
expect pass "the floor-slot tag v2026.9.1 at its own (prior-month) commit" "$repo" "$tag_floor"

# --- (c) a hand-made tag on the wrong commit --------------------------------
# v2026.8.2 pushed by hand at the August HEAD, whose August derivation is .5.
# The ladder probes .5 and finds it at HEAD — reuse of a DIFFERENT tag — and
# never walks down to .2.
( cd "$repo" && git checkout -q "$aug_head" && git tag v2026.8.2 && git push -q origin refs/tags/v2026.8.2 )
expect refuse "(c) a hand-made tag at the wrong commit (v2026.8.2 at the .5 commit)" "$repo" "v2026.8.2"
expect_mentions 'resolves v2026\.8\.5 \(reuse\) at this commit, not v2026\.8\.2' "     ...and says what the mint resolved instead"
expect_mentions 'tag=v2026\.8\.5' "     ...and prints the mint's own report"

# The same hand-made name, with a pin inherited from the job environment. A
# check that honoured MARKETING_VERSION here would compare the tag to itself.
expect refuse "an inherited MARKETING_VERSION pin does not make the wrong tag pass" "$repo" "v2026.8.2" MARKETING_VERSION=2026.8.2
expect refuse "an inherited VERSION_PATCH_OVERRIDE does not either" "$repo" "v2026.8.2" VERSION_PATCH_OVERRIDE=2
# ...and a pin does not break the right tag either — the seams are scrubbed,
# not consulted.
expect pass "the minted tag still passes under an inherited pin" "$repo" "$tag_a" MARKETING_VERSION=2026.8.2

# A past month at a LATER commit — the blind spot the ladder cannot see. At
# the September commit, "commits since Aug 1" is six, so pinned to August the
# ladder resolves v2026.8.6; pushed by hand at that commit it would answer
# reuse. The mint never tags a commit with a month that ended before it, so
# the lower bound refuses it before the ladder is asked.
( cd "$repo" && git checkout -q "$sep_head" && git tag v2026.8.6 && git push -q origin refs/tags/v2026.8.6 )
control "2026.8.6" "$(bare_version "$repo" "$AUG-31")" "control: pinned to August, the September commit derives 2026.8.6"
expect refuse "a prior-month tag hand-pushed at a later month's commit (v2026.8.6 at the September commit)" "$repo" "v2026.8.6"
expect_mentions "ended \(at 2026-09-01T00:00:00Z\) before this commit was made" "     ...naming the boundary the commit is past"
expect_absent "Re-running the mint" "     ...before the mint ran"

# A tag the mint would resolve to, but that is not on the remote: `create`,
# not `reuse`. The probe is remote-visible, so a local-only tag is refused.
( cd "$repo" && git checkout -q "$tag_b" )
commit "$repo" "$SEP-04" "sep 2"
( cd "$repo" && git tag v2026.9.3 )
expect refuse "a local-only tag the remote has never seen (action=create)" "$repo" "v2026.9.3"
expect_mentions 'resolves v2026\.9\.3 \(create\)' "     ...naming create, not reuse"
( cd "$repo" && git tag -d v2026.9.3 >/dev/null && git checkout -q "$aug_head" )

# --- shape ---------------------------------------------------------------------
# Each is refused by the shape check itself, BEFORE the mint runs — asserted,
# because a looser regex would still see most of these refused downstream by
# the ladder after a live probe, and the fixture would stay green.
shape() {
    expect refuse "$1" "$repo" "$2"
    expect_mentions 'not a vYYYY\.M\.P CalVer tag' "     ...by the shape check"
    expect_absent "Re-running the mint" "     ...before the mint ran"
}
shape "a semver tag" "v1.2.3"
shape "a padded month" "v2026.08.5"
shape "a padded patch" "v2026.8.05"
shape "a .0 patch (the floor never emits one)" "v2026.8.0"
shape "a thirteenth month" "v2026.13.1"
shape "a year with a leading zero" "v0999.1.1"
shape "a suffix" "v2026.8.5-rc1"
shape "no v prefix" "2026.8.5"
shape "not a tag at all" "release"

# --- the clock -----------------------------------------------------------------
# Next month's floor slot, by the real clock — the realistic hand-made future
# tag — and next year's. Pinned to those months the ladder WOULD reuse either
# at any commit (zero commits floor to 1), so the refusal has to come from
# the clock, before the mint runs. Both are pushed, so the ladder is not what
# refuses them.
now_year=$(( 10#$(date -u +%Y) ))
now_month=$(( 10#$(date -u +%m) ))
if (( now_month == 12 )); then
    next_month_tag="v$((now_year + 1)).1.1"
else
    next_month_tag="v$now_year.$((now_month + 1)).1"
fi
next_year_tag="v$((now_year + 1)).$now_month.1"
( cd "$repo" && git tag "$next_month_tag" && git push -q origin "refs/tags/$next_month_tag" )
expect refuse "next month's floor slot ($next_month_tag, pushed)" "$repo" "$next_month_tag"
expect_mentions "after the current UTC month" "     ...naming the clock"
expect_absent "Re-running the mint" "     ...before the mint ran"
( cd "$repo" && git tag "$next_year_tag" && git push -q origin "refs/tags/$next_year_tag" )
expect refuse "next year's same month ($next_year_tag, pushed)" "$repo" "$next_year_tag"
expect_mentions "after the current UTC month" "     ...naming the clock"
expect_absent "Re-running the mint" "     ...before the mint ran"

# --- the derivation cannot run ---------------------------------------------------
# The mint's remote probe fails: it refuses to mint blind, and the check must
# refuse rather than pass — or fall back to local tags, which would let the
# next case through.
broken="$work/broken"
git clone -q "$origin" "$broken"
( cd "$broken" && git checkout -q "$tag_a" && git remote set-url origin "$work/does-not-exist.git" )
expect refuse "the mint's remote probe fails (origin unreachable)" "$broken" "$tag_a"
expect_mentions "could not run \(the mint exited non-zero" "     ...naming the derivation, not the tag"

# No origin at all: the mint would fall back to LOCAL tags, and the local tag
# is right there. Refused before the mint is asked.
( cd "$broken" && git remote remove origin )
expect refuse "no origin remote (the probe would read local tags)" "$broken" "$tag_a"
expect_mentions "no origin remote" "     ...naming the missing remote"
expect_absent "Re-running the mint" "     ...before the mint ran"

# A shallow clone answers "commits this month" with 1. At the August HEAD that
# would derive 2026.8.1 — a number that IS a plausible tag — so the check
# refuses the shape of the checkout before it asks.
shallow="$work/shallow"
git clone -q --no-local --depth 1 "$origin" "$shallow"
( cd "$shallow" && git fetch -q --depth 1 origin "refs/tags/$tag_a:refs/tags/$tag_a" && git checkout -q "$tag_a" )
control "true" "$( cd "$shallow" && git rev-parse --is-shallow-repository )" "fixture: the shallow clone is shallow"
expect refuse "a shallow clone (fetch-depth: 1)" "$shallow" "$tag_a"
expect_mentions "this checkout is shallow" "     ...naming the checkout"
expect_absent "Re-running the mint" "     ...before the mint ran"

# Not a repository at all. GIT_CEILING_DIRECTORIES stops git discovering a
# checkout that happens to enclose the temp dir (a TMPDIR inside a repo), which
# would otherwise run the real mint against a real origin from a unit test.
notrepo="$work/notrepo"
mkdir -p "$notrepo"
expect refuse "outside any git checkout" "$notrepo" "$tag_a" GIT_CEILING_DIRECTORIES="$work"
expect_mentions "not inside a git checkout" "     ...naming the missing checkout"

# --- usage ------------------------------------------------------------------------
rc=0
( cd "$repo" && "$BASH" "$ASSERT" ) >/dev/null 2>&1 || rc=$?
control "2" "$rc" "no argument is a usage error (exit 2), not a refusal"
rc=0
( cd "$repo" && "$BASH" "$ASSERT" "$tag_a" extra ) >/dev/null 2>&1 || rc=$?
control "2" "$rc" "two arguments is a usage error (exit 2), not a refusal"

echo
echo "assert-release-tag: $pass passed, $fail failed"
[[ $fail -eq 0 ]]
