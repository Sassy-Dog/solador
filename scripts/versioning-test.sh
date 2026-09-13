#!/usr/bin/env bash
set -euo pipefail
#
# The versioning scripts' test suite (#405): scripts/get-version-info.sh (the
# CalVer derivation and the §4 mint), scripts/get-build-number.sh (the build
# number), and scripts/assert-release-tag.sh (the release workflows' tag
# assertion, #404) — every one of them against TEMPORARY bare-origin git
# repositories built here, with the REAL scripts and real `ls-remote` probes.
# Nothing pushes anywhere but the scratch origin under $work.
#
# Dependency-free bash in the shape of agent/deploy/lib_test.sh: git and
# coreutils only, no bats, no jq, Bash 3.2-clean because it runs under macOS
# stock /bin/bash in CI (the interpreter a macOS release runner and `./dev
# publish` execute under), under bash 5 on the Linux leg, and under Git Bash
# on the Windows leg. The scripts under test and the mint run under "$BASH" —
# the interpreter THIS harness was started with — so each leg tests the
# scripts under its own bash and not whichever is first on PATH. Run by
# ci.yml's `agent-tests`, `rust-workspace` and `windows-tests` jobs, by
# `./dev test`, and by hand:
#
#   scripts/versioning-test.sh
#
# What it holds, section by section (docs/VERSIONING.md §Tests is the list
# this file answers to):
#
#   1. DERIVATION (--version): the patch floor (a month with no commits
#      derives .1), the month-roll reset, §2 idempotency, the non-padded
#      month, the two test seams (VERSION_DATE_OVERRIDE, VERSION_PATCH_OVERRIDE)
#      and the MARKETING_VERSION pin taken verbatim, and the §6 migration
#      vector: a derived CalVer orders above the last semver tag v0.1.1 under
#      the documented numeric-per-component rule (crates/viewmodel's
#      `is_newer` has the Rust tests for the comparison itself; the vector
#      here only asserts the derived STRING sorts above 0.1.1 by that rule).
#   2. BUILD NUMBER (get-build-number.sh): totality (the count is `rev-list
#      --count`), monotonic across a commit, `--at <ref>` (an annotated tag is
#      peeled), the BUILD_NUMBER pin (verbatim, and it wins over --at), the
#      delegation from `get-version-info.sh --build`, and fail-closed: an
#      unresolvable ref and a directory outside any checkout exit 1 with an
#      EMPTY stdout — never a clock value, never a made-up floor.
#   3. MINT (--tag): the OUTPUT CONTRACT — exactly three lines on stdout,
#      `version=`, `tag=` (v + version), `action=` ∈ {create, reuse} and
#      nothing else, which scripts/publish.sh's epilogue branches on (#395);
#      a dry run creates nothing; `--push` creates an annotated tag whose
#      peeled commit is HEAD; the same commit re-run answers `reuse` and
#      PERFORMS NO PUSH — observed two ways, because a ref snapshot alone
#      cannot see an idempotent re-push of the same tag: the origin's tag
#      list is snapshotted before and after (fail-closed — an empty
#      snapshot is a failure, not "no tags"), AND a `git` shim on the
#      mint's PATH records every git invocation, so the reuse must record
#      no `push` at all where the create must record one (the second
#      contract property #395 depends on); the §4
#      collision replay (a prior-month commit released after the roll mints
#      vM.1 at the old commit; the month's first real commit derives M.1,
#      finds it elsewhere, and is bumped to M.2, `create`); a pin is NEVER
#      auto-bumped (exit 1, nothing tagged) but reuses at its own commit and
#      creates when free; a failed remote probe refuses to mint blind (exit
#      1, nothing tagged, empty stdout); with no origin the probe reads local
#      tags, as documented; a stray argument is a usage error (exit 2).
#   4. THE RELEASE-TAG ASSERTION (assert-release-tag.sh, #404), absorbed from
#      the fixture that first proved it: a tag minted before the UTC month
#      rolled and a ladder-bumped tag pass; a hand-made tag on the wrong
#      commit, a prior-month tag at a later commit, a local-only tag, every
#      malformed shape, a tag after the current UTC month, an unreachable
#      origin, no origin, a shallow clone and a directory outside a checkout
#      are refused before or after the mint as each case demands, with every
#      cause named. Two of these are negative controls on the FIXTURE: they
#      show the bare `--version` the old assertion compared against does
#      disagree with the tag here, so the pass cases pass because of the
#      pinned-month ladder and not because the fixture never reproduced the
#      failure.
#
# Not here, on purpose: the shallow-clone REFUSAL of the build scripts lives
# in crates/buildversion (Rust, `is_shallow`), not in the shell mint, which
# has no such check — it is #417's. The comparison the §6 vector cites is
# Rust (`viewmodel::update::is_newer`). And the assertion script's "output
# contract was violated" branch is unreachable from here for the reason its
# own header gives: the mint is the sibling script and always prints its
# three lines on exit 0.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
ASSERT="$SCRIPT_DIR/assert-release-tag.sh"
MINT="$SCRIPT_DIR/get-version-info.sh"
BUILD="$SCRIPT_DIR/get-build-number.sh"

# An inherited GIT_DIR (a `git rebase -x` from a linked worktree exports one)
# would point every `git init`/`git config` below at the REAL checkout. Drop
# the whole family before the first git call; a control below proves the
# fixture's git dir is under $work.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_OBJECT_DIRECTORY

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
# Resolved, because git reports resolved paths (macOS's /var is a symlink to
# /private/var) and the git-dir control below compares against this prefix.
work="$( cd "$work" && pwd -P )"

# A `git` shim, first on the mint's PATH only: records every invocation's
# argv to $work/git.calls and execs the real git. This is how "reuse pushes
# nothing" is observed as an absence of the push COMMAND, not inferred from
# an unchanged remote — a re-push of a tag the remote already has at the
# same commit changes nothing there either.
REAL_GIT="$(command -v git)"
mkdir -p "$work/shim"
cat > "$work/shim/git" <<SHIM
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$work/git.calls"
exec "$REAL_GIT" "\$@"
SHIM
chmod +x "$work/shim/git"

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

# run_mint REPO TODAY [--push] [ENV=value ...] — publish.sh's mint, as run on
# TODAY, with its stdout in $work/mint.out, its stderr in $work/mint.err, its
# exit status in $mint_rc and every git call it made in $work/git.calls, so a
# case can read all four. The two seams and the pin are scrubbed unless the
# case passes them. A mint that BUILDS the fixture (the tags §4 asserts on)
# is followed by `|| fixture "…"`, so a mint broken by a future change ends
# the run as one readable FAIL with its stderr shown, not a `set -e` abort
# half-way through with no summary line.
mint_rc=0
run_mint() {
    local repo="$1" today="$2"; shift 2
    local push=""
    if [[ "${1:-}" == "--push" ]]; then push="--push"; shift; fi
    mint_rc=0
    : > "$work/git.calls"
    # shellcheck disable=SC2086
    ( cd "$repo" && env -u MARKETING_VERSION -u VERSION_PATCH_OVERRIDE VERSION_DATE_OVERRIDE="$today" PATH="$work/shim:$PATH" "$@" \
        "$BASH" "$MINT" --tag $push >"$work/mint.out" 2>"$work/mint.err" ) || mint_rc=$?
    [[ $mint_rc -eq 0 ]]
}

# git_pushes — how many `push` invocations the last run_mint made.
git_pushes() {
    grep -c '^push ' "$work/git.calls" 2>/dev/null || true
}

# mint_field NAME — the value of `NAME=` in the last run_mint's stdout.
mint_field() {
    sed -n "s/^$1=//p" "$work/mint.out"
}

# bare_version REPO TODAY [ENV=value ...] — the wall-clock derivation the old
# assertion used, with any extra environment the case wants.
bare_version() {
    local repo="$1" today="$2"; shift 2
    ( cd "$repo" && env -u MARKETING_VERSION -u VERSION_PATCH_OVERRIDE VERSION_DATE_OVERRIDE="$today" "$@" "$BASH" "$MINT" --version )
}

# origin_tags — the scratch origin's tags, sorted: one half of the
# reuse-performs-no-push observation. Fail-closed: a `show-ref` that cannot
# run prints a sentinel the equality below can never match, and the cases
# that read it first assert it names the tag they just created — an empty
# string compared to an empty string was how this assertion once passed
# against a git dir that did not exist.
origin_tags() {
    "$REAL_GIT" --git-dir="$origin" show-ref --tags 2>&1 | sort || echo "SHOW-REF FAILED"
}

# run_build REPO [ARGS...] [ENV=value ...] — get-build-number.sh with its
# stdout in $work/build.out, stderr in $work/build.err, status in $build_rc.
# Arguments starting with `--` go to the script; NAME=value pairs go to env.
build_rc=0
run_build() {
    local repo="$1"; shift
    local args=() envs=()
    while [[ $# -gt 0 ]]; do
        case "$1" in
            [A-Z_]*=*) envs+=("$1") ;;   # NAME=value; `=` is legal in a refname, so shape, not presence
            *) args+=("$1") ;;
        esac
        shift
    done
    build_rc=0
    ( cd "$repo" && env -u BUILD_NUMBER ${envs[@]+"${envs[@]}"} "$BASH" "$BUILD" ${args[@]+"${args[@]}"} >"$work/build.out" 2>"$work/build.err" ) || build_rc=$?
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

# control EXPECTED ACTUAL LABEL — an equality assertion; on the fixture, that
# the scenario is the one it claims to be, and on the scripts, the vector.
control() {
    if [[ "$1" == "$2" ]]; then
        echo "ok   $3"
        pass=$((pass + 1))
    else
        echo "FAIL $3: expected '$1', got '$2'"
        fail=$((fail + 1))
    fi
}

# file_mentions REGEX FILE LABEL / file_lacks REGEX FILE LABEL — on the
# captured stdout/stderr of the last run_mint / run_build.
file_mentions() {
    if grep -qE -- "$1" "$2"; then
        echo "ok   $3"
        pass=$((pass + 1))
    else
        echo "FAIL $3: $(basename "$2") does not match /$1/"
        sed 's/^/     /' "$2"
        fail=$((fail + 1))
    fi
}
file_lacks() {
    if ! grep -qE -- "$1" "$2"; then
        echo "ok   $3"
        pass=$((pass + 1))
    else
        echo "FAIL $3: $(basename "$2") matches /$1/"
        sed 's/^/     /' "$2"
        fail=$((fail + 1))
    fi
}

# mint_contract LABEL — the last run_mint's stdout is EXACTLY the output
# contract: three lines, version= / tag= / action= in that order, the tag is
# `v` + the version, and the action is one of the two words publish.sh's
# epilogue knows. Anything else — a fourth line, a renamed token, a bare
# "created" — is what would silently land every future publish on the
# epilogue's fail-closed `*` arm (#395).
mint_contract() {
    local label="$1" lines version tag action ok=true why=""
    lines="$(wc -l < "$work/mint.out" | tr -d ' ')"
    version="$(mint_field version)"; tag="$(mint_field tag)"; action="$(mint_field action)"
    [[ "$lines" == "3" ]] || { ok=false; why="$lines lines on stdout, not 3"; }
    [[ "$(sed -n '1p' "$work/mint.out")" == "version=$version" ]] || { ok=false; why="${why:+$why; }line 1 is not version="; }
    [[ "$(sed -n '2p' "$work/mint.out")" == "tag=$tag" ]] || { ok=false; why="${why:+$why; }line 2 is not tag="; }
    [[ "$(sed -n '3p' "$work/mint.out")" == "action=$action" ]] || { ok=false; why="${why:+$why; }line 3 is not action="; }
    [[ "$tag" == "v$version" ]] || { ok=false; why="${why:+$why; }tag '$tag' is not v$version"; }
    case "$action" in
        create | reuse) ;;
        *) ok=false; why="${why:+$why; }action '$action' is not create|reuse" ;;
    esac
    if $ok; then
        echo "ok   $label (contract: version=$version tag=$tag action=$action)"
        pass=$((pass + 1))
    else
        echo "FAIL $label: $why"
        sed 's/^/     /' "$work/mint.out"
        fail=$((fail + 1))
    fi
}

# calver_gt A B — A orders above B under docs/VERSIONING.md §6's rule: each
# dot-separated component compared as an integer, left to right. This is the
# documented rule restated for the string vector, not a second comparator —
# the app's comparison is `viewmodel::update::is_newer`, tested in Rust.
calver_gt() {
    local a="$1" b="$2" i x y
    for i in 1 2 3; do
        x="$(printf '%s' "$a" | cut -d. -f"$i")"; y="$(printf '%s' "$b" | cut -d. -f"$i")"
        x=$((10#${x:-0})); y=$((10#${y:-0}))
        if (( x > y )); then return 0; fi
        if (( x < y )); then return 1; fi
    done
    return 1
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
case "$( cd "$repo" && git rev-parse --absolute-git-dir )" in
    "$work"/*) echo "ok   fixture: the fixture's git dir is under \$work, not an inherited GIT_DIR"; pass=$((pass + 1)) ;;
    *) echo "FAIL fixture: the fixture's git dir is outside \$work — an inherited GIT_DIR reached the real checkout"; fail=$((fail + 1)); exit 1 ;;
esac

# =============================================================================
# 1. DERIVATION
# =============================================================================
echo "--- derivation (--version) ---"

# §2 idempotency: the same question twice, the same answer.
control "$(bare_version "$repo" "$AUG-20")" "$(bare_version "$repo" "$AUG-20")" "derivation: --version is idempotent (two calls, one answer)"

# The patch floor: a month with no commits derives .1, never .0.
control "2026.9.1" "$(bare_version "$repo" "$SEP-06")" "derivation: a month with no commits floors the patch at 1 (2026.9.1)"
control "2027.3.1" "$(bare_version "$repo" "2027-03-31")" "derivation: the floor holds any distance past the last commit (2027.3.1)"

# The month is not padded: 2026.1.x, never 2026.01.x. (Pinned to January, every
# August commit is after the 1st, so the patch is 5.)
control "2026.1.5" "$(bare_version "$repo" "2026-01-15")" "derivation: the month is not zero-padded (2026.1.5, not 2026.01.5)"

# The two seams. VERSION_PATCH_OVERRIDE pins the count; VERSION_DATE_OVERRIDE
# has been pinning the month all along.
control "2026.8.7" "$(bare_version "$repo" "$AUG-20" VERSION_PATCH_OVERRIDE=7)" "derivation: VERSION_PATCH_OVERRIDE pins the patch (2026.8.7)"
control "2026.8.1" "$(bare_version "$repo" "$AUG-20" VERSION_PATCH_OVERRIDE=0)" "derivation: a pinned patch of 0 is still floored to 1"

# The pin is emitted verbatim, never recomputed — not even normalised.
control "2030.1.9" "$(bare_version "$repo" "$AUG-20" MARKETING_VERSION=2030.1.9)" "derivation: MARKETING_VERSION is emitted verbatim"

# §6: every derived CalVer orders above the last semver tag, v0.1.1, under the
# per-component numeric rule, so the semver → CalVer switch needed no cutover
# gate. And the rule itself is monotonic across a month roll, which the
# derivation's reset (a smaller patch after the roll) would otherwise look
# like a step backwards.
if calver_gt "$(bare_version "$repo" "$AUG-20")" "0.1.1"; then
    echo "ok   derivation: 2026.8.5 orders above the last semver tag 0.1.1 (§6)"; pass=$((pass + 1))
else
    echo "FAIL derivation: 2026.8.5 does not order above 0.1.1"; fail=$((fail + 1))
fi
if calver_gt "$(bare_version "$repo" "$SEP-06")" "$(bare_version "$repo" "$AUG-20")"; then
    echo "ok   derivation: the post-roll 2026.9.1 orders above the pre-roll 2026.8.5 (a smaller patch is not a step back)"; pass=$((pass + 1))
else
    echo "FAIL derivation: 2026.9.1 does not order above 2026.8.5"; fail=$((fail + 1))
fi
if ! calver_gt "0.1.1" "2026.8.5" && ! calver_gt "2026.8.5" "2026.8.5"; then
    echo "ok   derivation: the ordering helper is strict and not reflexive (control on the vector itself)"; pass=$((pass + 1))
else
    echo "FAIL derivation: the ordering helper is wrong, so the two vectors above prove nothing"; fail=$((fail + 1))
fi
# ...and NUMERIC, which lexical ordering gets wrong the first October and the
# tenth commit of any month: "10" sorts below "9" as a string.
if calver_gt "2026.10.1" "2026.9.5" && ! calver_gt "2026.9.5" "2026.10.1" \
    && calver_gt "2026.8.10" "2026.8.9" && ! calver_gt "2026.8.9" "2026.8.10"; then
    echo "ok   derivation: the rule is per-component NUMERIC — 2026.10.1 > 2026.9.5 and 2026.8.10 > 2026.8.9 (lexical order says the opposite)"; pass=$((pass + 1))
else
    echo "FAIL derivation: the ordering helper is lexical, not numeric — October would order below September"; fail=$((fail + 1))
fi

# =============================================================================
# 2. BUILD NUMBER
# =============================================================================
echo "--- build number (get-build-number.sh) ---"

run_build "$repo"
control "0" "$build_rc" "build: exits 0 in a checkout"
control "5" "$(cat "$work/build.out")" "build: the build number is the total commit count (5 after five commits)"
control "$( cd "$repo" && git rev-list --count HEAD )" "$(cat "$work/build.out")" "build: ...and equals rev-list --count HEAD exactly"
control "$(cat "$work/build.out")" "$( cd "$repo" && env -u BUILD_NUMBER "$BASH" "$MINT" --build )" "build: get-version-info.sh --build delegates to it (same answer)"

run_build "$repo" BUILD_NUMBER=42
control "42" "$(cat "$work/build.out")" "build: BUILD_NUMBER is consumed verbatim"
run_build "$repo" --at "$aug_head" BUILD_NUMBER=42
control "42" "$(cat "$work/build.out")" "build: the pin wins even over an explicit --at"

run_build "$repo" --at
control "2" "$build_rc" "build: --at with no ref is a usage error (exit 2)"
run_build "$repo" --bogus
control "2" "$build_rc" "build: an unknown flag is a usage error (exit 2)"

run_build "$repo" --at no-such-ref-anywhere
control "1" "$build_rc" "build: an unresolvable ref exits 1 (fail closed)"
control "" "$(cat "$work/build.out")" "build: ...and prints NOTHING on stdout — no clock value, no floor"
file_mentions "could not derive a build number" "$work/build.err" "build: ...naming the failure on stderr"

notrepo="$work/notrepo"
mkdir -p "$notrepo"
run_build "$notrepo" GIT_CEILING_DIRECTORIES="$work"
control "1" "$build_rc" "build: outside any checkout exits 1 (fail closed)"
control "" "$(cat "$work/build.out")" "build: ...with an empty stdout"

# =============================================================================
# 3. MINT
# =============================================================================
echo "--- mint (--tag) ---"

# A dry run resolves the same triple and creates nothing, anywhere.
run_mint "$repo" "$AUG-20" || true
control "0" "$mint_rc" "mint: a dry run (--tag, no --push) exits 0"
mint_contract "mint: the dry run's stdout is the output contract"
control "create" "$(mint_field action)" "mint: the dry run's action is create (the tag is free)"
control "0" "$(git_pushes)" "mint: the dry run invoked no git push"
file_lacks "^tag " "$work/git.calls" "mint: ...and no git tag"
control "" "$( "$REAL_GIT" --git-dir="$origin" show-ref --tags 2>/dev/null || true )" "mint: the origin has no tag after the dry run"
control "" "$( cd "$repo" && git tag -l )" "mint: the dry run created no local tag either"
file_mentions "Dry run: would create tag v2026.8.5" "$work/mint.err" "mint: the dry run says so on stderr"

# --push: an annotated tag at HEAD, on the origin, and the contract says create.
run_mint "$repo" "$AUG-20" --push || fixture "the mint could not tag the August HEAD"
control "0" "$mint_rc" "mint: --tag --push exits 0"
mint_contract "mint: the create's stdout is the output contract"
control "create" "$(mint_field action)" "mint: a free tag is created"
control "v2026.8.5" "$(mint_field tag)" "mint: ...as v2026.8.5 at the August HEAD"
control "tag" "$( cd "$repo" && git cat-file -t v2026.8.5 )" "mint: the tag is annotated (a tag object, not a lightweight ref)"
control "$aug_head" "$( cd "$repo" && git ls-remote --tags origin 'refs/tags/v2026.8.5^{}' | cut -f1 )" "mint: the origin's peeled v2026.8.5 is the August HEAD"
control "1" "$(git_pushes)" "mint: the create made exactly one git push (positive control: the shim sees pushes)"
file_mentions "^push origin refs/tags/v2026\.8\.5$" "$work/git.calls" "mint: ...of that tag's ref, to origin"
tag_a="$(mint_field tag)"
tags_after_create="$(origin_tags)"
if grep -q 'refs/tags/v2026\.8\.5$' <<< "$tags_after_create"; then
    echo "ok   mint: the origin snapshot names v2026.8.5 (the snapshot is a real observation, not an empty string)"; pass=$((pass + 1))
else
    echo "FAIL mint: the origin snapshot does not name v2026.8.5: '$tags_after_create'"; fail=$((fail + 1))
fi

# The same commit again: reuse, and the origin is untouched — the property
# publish.sh's epilogue states ("this run pushed nothing") and #395 relies
# on. Observed as the ABSENCE OF THE PUSH COMMAND, because the origin's ref
# list cannot see a re-push of a tag it already has at the same commit.
run_mint "$repo" "$AUG-20" --push || true
control "0" "$mint_rc" "mint: a re-run at the same commit exits 0"
mint_contract "mint: the reuse's stdout is the output contract"
control "reuse" "$(mint_field action)" "mint: the same commit answers reuse"
control "v2026.8.5" "$(mint_field tag)" "mint: ...for the same tag"
control "0" "$(git_pushes)" "mint: reuse performs no push — no git push was invoked at all"
file_lacks "^tag " "$work/git.calls" "mint: ...and no git tag either"
control "$tags_after_create" "$(origin_tags)" "mint: ...and the origin's tag list is byte-identical before and after"
file_mentions "reusing \(idempotent re-run\)" "$work/mint.err" "mint: the reuse says so on stderr"
file_lacks "Created and pushed" "$work/mint.err" "mint: ...and never claims a push"

# The stray-argument usage error.
( cd "$repo" && "$BASH" "$MINT" --tag --extra >/dev/null 2>&1 ) && rc=0 || rc=$?
control "2" "$rc" "mint: --tag with a stray argument is a usage error (exit 2)"
control "$tags_after_create" "$(origin_tags)" "mint: ...and tags nothing"

# --- the §4 collision replay ----------------------------------------------------
# publish.sh mints v2026.8.5 on the 20th. A post-roll release of the August
# HEAD on Sep 2 derives 2026.9.1 (the floor slot) at the AUGUST commit and
# mints it there. The month's first real commit then derives 2026.9.1 again,
# finds it at the August commit, and the ladder bumps to v2026.9.2 — the
# bumped version IS the version, and the action is create.
run_mint "$repo" "$SEP-02" --push || fixture "the mint could not take the September floor slot"
control "0" "$mint_rc" "mint: a post-roll release of the August HEAD exits 0"
mint_contract "mint: the floor-slot mint's stdout is the output contract"
control "v2026.9.1" "$(mint_field tag)" "mint: ...taking the September floor slot v2026.9.1 at the August commit"
control "create" "$(mint_field action)" "mint: ...as a create"
tag_floor="$(mint_field tag)"

commit "$repo" "$SEP-03" "sep 1"
( cd "$repo" && git push -q origin main )
sep_head="$( cd "$repo" && git rev-parse HEAD )"
control "2026.9.1" "$(bare_version "$repo" "$SEP-03")" "mint: the first September commit derives the unbumped 2026.9.1 (month-roll reset: the count restarts)"
control "2026.8.6" "$(bare_version "$repo" "$AUG-31")" "mint: ...while pinned to August the same commit derives 2026.8.6 (the August count kept growing)"
run_mint "$repo" "$SEP-03" --push || fixture "the mint could not tag the September commit"
control "0" "$mint_rc" "mint: the first September commit's mint exits 0"
mint_contract "mint: the bumped mint's stdout is the output contract"
control "v2026.9.2" "$(mint_field tag)" "mint: the collision is resolved by bumping to v2026.9.2"
control "create" "$(mint_field action)" "mint: ...as a create (the bumped version IS the version)"
file_mentions "Tag v2026\.9\.1 exists at $aug_head \(not $sep_head\) — bumping patch" "$work/mint.err" "mint: the bump names the colliding tag and both commits"
control "$sep_head" "$( cd "$repo" && git ls-remote --tags origin 'refs/tags/v2026.9.2^{}' | cut -f1 )" "mint: the origin's peeled v2026.9.2 is the September commit"
control "1" "$(git_pushes)" "mint: the bumped create pushed exactly once"
tag_b="$(mint_field tag)"
tags_after_bump="$(origin_tags)"
if grep -q 'refs/tags/v2026\.9\.2$' <<< "$tags_after_bump"; then
    echo "ok   mint: the origin snapshot names v2026.9.2"; pass=$((pass + 1))
else
    echo "FAIL mint: the origin snapshot does not name v2026.9.2: '$tags_after_bump'"; fail=$((fail + 1))
fi

# Build number, now that the tree has grown: monotonic, and --at peels a tag.
run_build "$repo"
control "6" "$(cat "$work/build.out")" "build: the build number grew to 6 with the sixth commit (monotonic, never reset by the month roll)"
run_build "$repo" --at "$tag_a"
control "5" "$(cat "$work/build.out")" "build: --at v2026.8.5 counts at the tag's commit (5) — an annotated tag is peeled"
run_build "$repo" --at "$aug_head"
control "5" "$(cat "$work/build.out")" "build: --at <sha> counts at that commit"
run_build "$repo" --at "$tag_floor"
control "5" "$(cat "$work/build.out")" "build: --at the floor-slot tag v2026.9.1 also reads 5 — it names the August commit"

# --- a pin is never auto-bumped ---------------------------------------------------
# MARKETING_VERSION=2026.9.1 at the September commit: that tag exists at the
# August commit. Bumping a pin would ship under a number nobody chose; the
# mint refuses instead, tags nothing, and prints no contract at all.
run_mint "$repo" "$SEP-03" --push MARKETING_VERSION=2026.9.1 || true
control "1" "$mint_rc" "mint: a pin whose tag exists at another commit exits 1"
control "0" "$(git_pushes)" "mint: ...and pushes nothing"
control "" "$(cat "$work/mint.out")" "mint: ...printing no contract on stdout"
file_mentions "pinned MARKETING_VERSION=2026\.9\.1 but tag v2026\.9\.1 exists at $aug_head" "$work/mint.err" "mint: ...naming the pin, the tag and where it is"
file_mentions "never auto-bumped" "$work/mint.err" "mint: ...and the rule"
control "$tags_after_bump" "$(origin_tags)" "mint: ...and tags nothing"
# The same pin at its OWN commit is a reuse, and a free pin is created verbatim.
( cd "$repo" && git checkout -q "$aug_head" )
run_mint "$repo" "$SEP-03" --push MARKETING_VERSION=2026.9.1 || true
control "0" "$mint_rc" "mint: a pin at its own commit exits 0"
mint_contract "mint: the pinned reuse's stdout is the output contract"
control "reuse" "$(mint_field action)" "mint: ...and answers reuse"
control "0" "$(git_pushes)" "mint: ...pushing nothing"
control "$tags_after_bump" "$(origin_tags)" "mint: ...and leaving the origin as it was"
( cd "$repo" && git checkout -q main )
run_mint "$repo" "$SEP-03" MARKETING_VERSION=2026.9.7 || true
control "0" "$mint_rc" "mint: a free pin exits 0"
mint_contract "mint: the pinned dry run's stdout is the output contract"
control "v2026.9.7" "$(mint_field tag)" "mint: ...resolving the pin verbatim (no recomputation; a dry run, so nothing is tagged)"
control "create" "$(mint_field action)" "mint: ...as a create"
control "0" "$(git_pushes)" "mint: ...the dry run pushing nothing"

# --- the probe fails closed ---------------------------------------------------------
broken="$work/broken"
git clone -q "$origin" "$broken"
( cd "$broken" && git config user.email test@example.com && git config user.name test \
    && git checkout -q "$tag_a" && git remote set-url origin "$work/does-not-exist.git" ) || fixture "the minted tag $tag_a is not on the origin"
run_mint "$broken" "$AUG-20" --push || true
control "1" "$mint_rc" "mint: an unreachable origin exits 1 — the probe fails closed"
control "0" "$(git_pushes)" "mint: ...pushing nothing"
control "" "$(cat "$work/mint.out")" "mint: ...printing no contract"
file_mentions "refusing to mint blind" "$work/mint.err" "mint: ...and says why"
control "v2026.8.5 v2026.9.1 v2026.9.2" "$( cd "$broken" && git tag -l | tr '\n' ' ' | sed 's/ $//' )" "mint: ...creating no local tag (the clone's tags are the three it fetched)"

# --- no origin: the probe reads local tags, as documented ---------------------------
( cd "$broken" && git remote remove origin )
run_mint "$broken" "$AUG-20" || true
control "0" "$mint_rc" "mint: with no origin remote the probe reads local tags"
mint_contract "mint: the no-origin reuse's stdout is the output contract"
control "reuse" "$(mint_field action)" "mint: ...and the local v2026.8.5 at HEAD answers reuse"
( cd "$broken" && git tag -d v2026.8.5 >/dev/null )
run_mint "$broken" "$AUG-20" || true
mint_contract "mint: the no-origin create's stdout is the output contract"
control "create" "$(mint_field action)" "mint: ...and with that local tag gone, create"

# =============================================================================
# 4. THE RELEASE-TAG ASSERTION (#404)
# =============================================================================
echo "--- release-tag assertion (assert-release-tag.sh) ---"
( cd "$repo" && git checkout -q main )

# --- (a) month roll -----------------------------------------------------------
# v2026.8.5 was minted on the 20th. The release is re-run (or the draft
# published) in September, when the bare derivation at the SAME commit says
# 2026.9.1 — the failure of run 32732819910, attempt 2.
( cd "$repo" && git checkout -q "$aug_head" )
control "2026.9.1" "$(bare_version "$repo" "$SEP-06")" "control: the bare derivation the old assertion compared says 2026.9.1 after the roll"
expect pass "(a) a tag minted in August, asserted after the month rolled" "$repo" "$tag_a"

# The everyday case, for completeness: same month, no bump, tag == derivation.
expect pass "same-month tag with no bump (the everyday path)" "$repo" "$tag_a"

# --- (b) ladder bump ----------------------------------------------------------
# The mint section above produced the shape: the floor slot v2026.9.1 at the
# August commit, then v2026.9.2 ladder-bumped at the first September commit.
# The old assertion at that commit compared the tag to the unbumped 2026.9.1.
( cd "$repo" && git checkout -q "$sep_head" )
control "2026.9.1" "$(bare_version "$repo" "$SEP-03")" "control: the bare derivation at the September commit is the unbumped 2026.9.1"
expect pass "(b) a ladder-bumped tag on its first run" "$repo" "$tag_b"

# The floor-slot tag itself, at the August commit it names, still passes when
# that release is re-run: pinned to September, the August HEAD derives .1,
# which exists at HEAD. This is also the lower bound's legitimate edge — a
# tag whose month is LATER than its commit's.
( cd "$repo" && git checkout -q "$tag_floor" ) || fixture "the floor-slot tag $tag_floor is not in the working clone"
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
( cd "$repo" && git checkout -q "$tag_b" ) || fixture "the bumped tag $tag_b is not in the working clone"
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
# next case through. (A fresh clone: the one from section 3 has lost its
# origin, and the assertion refuses that shape separately below.)
broken2="$work/broken2"
git clone -q "$origin" "$broken2"
( cd "$broken2" && git checkout -q "$tag_a" && git remote set-url origin "$work/does-not-exist.git" ) || fixture "the minted tag $tag_a is not on the origin"
expect refuse "the mint's remote probe fails (origin unreachable)" "$broken2" "$tag_a"
expect_mentions "could not run \(the mint exited non-zero" "     ...naming the derivation, not the tag"

# No origin at all: the mint would fall back to LOCAL tags, and the local tag
# is right there. Refused before the mint is asked.
( cd "$broken2" && git remote remove origin )
expect refuse "no origin remote (the probe would read local tags)" "$broken2" "$tag_a"
expect_mentions "no origin remote" "     ...naming the missing remote"
expect_absent "Re-running the mint" "     ...before the mint ran"

# A shallow clone answers "commits this month" with 1. At the August HEAD that
# would derive 2026.8.1 — a number that IS a plausible tag — so the check
# refuses the shape of the checkout before it asks.
shallow="$work/shallow"
git clone -q --no-local --depth 1 "$origin" "$shallow"
( cd "$shallow" && git fetch -q --depth 1 origin "refs/tags/$tag_a:refs/tags/$tag_a" && git checkout -q "$tag_a" ) || fixture "the minted tag $tag_a could not be fetched into the shallow clone"
control "true" "$( cd "$shallow" && git rev-parse --is-shallow-repository )" "fixture: the shallow clone is shallow"
expect refuse "a shallow clone (fetch-depth: 1)" "$shallow" "$tag_a"
expect_mentions "this checkout is shallow" "     ...naming the checkout"
expect_absent "Re-running the mint" "     ...before the mint ran"

# Not a repository at all. GIT_CEILING_DIRECTORIES stops git discovering a
# checkout that happens to enclose the temp dir (a TMPDIR inside a repo), which
# would otherwise run the real mint against a real origin from a unit test.
expect refuse "outside any git checkout" "$notrepo" "$tag_a" GIT_CEILING_DIRECTORIES="$work"
expect_mentions "not inside a git checkout" "     ...naming the missing checkout"
expect_absent "Re-running the mint" "     ...before the mint ran"

# --- usage ------------------------------------------------------------------------
rc=0
( cd "$repo" && "$BASH" "$ASSERT" ) >/dev/null 2>&1 || rc=$?
control "2" "$rc" "no argument is a usage error (exit 2), not a refusal"
rc=0
( cd "$repo" && "$BASH" "$ASSERT" "$tag_a" extra ) >/dev/null 2>&1 || rc=$?
control "2" "$rc" "two arguments is a usage error (exit 2), not a refusal"

echo
echo "versioning: $pass passed, $fail failed"
[[ $fail -eq 0 ]]
