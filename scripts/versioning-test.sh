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
#   5. THE AGENT'S VERSION (--agent-version, #490): a source build is
#      `<base>+dev.<k>.g<sha>` against the legacy base (the bridge, staged
#      through the LAST_COMBINED_AGENT_RELEASE seam, and read from
#      scripts/config.sh when the seam is empty) and against the highest
#      REACHABLE `agent-v*` tag once one exists; an `agent-v*` tag at HEAD is
#      that version (the highest, numerically); unreachable, malformed and
#      `agent-latest` tags are not bases; the cockpit's pin, clock and seams do
#      not move it; a shallow clone, a clone without the base tag and a
#      directory outside a checkout yield NOTHING (exit 1, empty stdout).
#   6. THE AGENT MINT (--agent-tag, #490): the output contract; reuse at HEAD
#      (annotated and lightweight, with no `gh` call and no push); the first
#      release in a month, a release in the same month as the legacy base, the
#      next month, a numeric (not lexical) maximum; a LOCAL-only tag ignored;
#      malformed and `agent-latest` tags not counted; a release that already
#      exists refused (with the negative control that the same run without it
#      creates); a version not above everything shipped refused; `gh` failing,
#      the remote probe failing and no origin each refusing blind; `--push`
#      creating an annotated tag at HEAD with exactly one push, a re-run
#      pushing nothing, a remote that rejects the push leaving no local tag;
#      a bad base and a stray argument.
#   7. THE AGENT MODE OF THE RELEASE-TAG ASSERTION (assert-release-tag.sh
#      --agent): a minted tag passes, after the month rolls too; the wrong
#      commit, a local-only tag, a different tag reused, every malformed shape
#      (the cockpit's own shape included, and the converse), a future month, a
#      month that ended before the commit, an unreachable origin, no origin, a
#      shallow clone and no checkout are refused with every cause named.
#   8. `./dev publish --agent` (scripts/publish.sh, #490): the real script
#      against a scratch origin, never pushing anywhere else — it mints and
#      pushes exactly one agent tag, requires none of the cockpit's credentials,
#      and each pre-flight (dirty tree, not on main, main behind origin, CI not
#      green, CI unknowable, CI green only at another commit, HEAD without the
#      agent release workflow), a refused mint and each cockpit-only option
#      refuse with nothing tagged.
#
# Not here, on purpose: the shallow-clone REFUSAL of the build scripts lives
# in crates/buildversion (the `Shallow` arm of its `resolve`, in Rust), not
# in the cockpit's shell mint, which has no such check — it is tested there
# (#417), together with the pins (the agent's ignores MARKETING_VERSION) and
# the agent's tag-watching. The agent's `--agent-version` DOES refuse a shallow
# clone itself (section 5), since it is the script that counts commits since a
# tag. The comparison the §6 vector cites is
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

# A `gh` shim for the AGENT cases (#490), on their PATH only. The agent mint asks
# `gh release view <tag>` whether a release already holds the name it is about to
# mint, and `publish.sh` asks `gh run list` whether CI is green; neither may reach
# GitHub from a unit test, and both answers have to be the test's to give:
#   GH_SHIM_EXISTING="agent-v2026.8.4 ..."  releases that "exist"
#   GH_SHIM_FAIL="HTTP 502"                 `release view` fails with that text
#   GH_SHIM_CI_GREEN=0|1                    the count `run list --jq` would print —
#                                           for `--workflow CI` and, when
#                                           GH_SHIM_CI_COMMIT is set, only at
#                                           that commit (anything else counts 0)
#   GH_SHIM_CI_FAIL=1                       `run list` itself errors
# Every call's argv is appended to $GH_SHIM_CALLS. The real `gh` says exactly
# "release not found" on stderr, exit 1, for an absent release (checked by hand
# against the real CLI when this was written). A `bash` link beside it makes the
# scripts' `#!/usr/bin/env bash` children run under THIS harness's interpreter,
# so the macOS 3.2 leg tests publish.sh's children under 3.2.
mkdir -p "$work/ghshim"
cat > "$work/ghshim/gh" <<'SHIM'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${GH_SHIM_CALLS:-/dev/null}"
case "$1 $2" in
    "release view")
        if [ -n "${GH_SHIM_FAIL:-}" ]; then echo "$GH_SHIM_FAIL" >&2; exit 1; fi
        for t in ${GH_SHIM_EXISTING:-}; do
            if [ "$t" = "$3" ]; then echo "title: $3"; exit 0; fi
        done
        echo "release not found" >&2
        exit 1
        ;;
    "run list")
        if [ -n "${GH_SHIM_CI_FAIL:-}" ]; then echo "gh: run list failed" >&2; exit 1; fi
        # Green only for the question publish.sh MUST ask: the `CI` workflow, at
        # the commit under test. Any other question gets "0 runs", so a
        # publish.sh that asked about the wrong workflow or commit is refused.
        shift 2
        wf=""; commit=""
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --workflow) wf="${2:-}"; shift ;;
                --commit) commit="${2:-}"; shift ;;
            esac
            shift
        done
        if [ "$wf" = "CI" ] && { [ -z "${GH_SHIM_CI_COMMIT:-}" ] || [ "$commit" = "$GH_SHIM_CI_COMMIT" ]; }; then
            echo "${GH_SHIM_CI_GREEN:-1}"
        else
            echo 0
        fi
        ;;
    *)
        echo "gh shim: unexpected call: $*" >&2
        exit 2
        ;;
esac
SHIM
chmod +x "$work/ghshim/gh"
ln -s "$BASH" "$work/ghshim/bash"

# Which mode the next `expect` runs the assertion in: empty is the cockpit's,
# `--agent` is the agent's (#490).
assert_flag=""

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
    out="$( cd "$repo" && env "$@" "$BASH" "$ASSERT" ${assert_flag:+"$assert_flag"} "$tag" 2>&1 )" || rc=$?
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
# Asked in git's own terms rather than by path prefix: from a repository's
# top level `--git-dir` answers the relative `.git` for the repository's own
# dir, and an inherited GIT_DIR answers with that path instead. (A prefix
# compare against $work is not portable — git.exe reports `D:/a/...` where
# Git Bash's pwd says `/d/a/...`, and that read a healthy fixture as
# poisoned on the Windows leg.)
case "$( cd "$repo" && git rev-parse --git-dir )" in
    .git) echo "ok   fixture: the fixture's git dir is its own .git, not an inherited GIT_DIR"; pass=$((pass + 1)) ;;
    *) echo "FAIL fixture: the fixture's git dir is '$( cd "$repo" && git rev-parse --git-dir )' — an inherited GIT_DIR reached the real checkout"; fail=$((fail + 1)); exit 1 ;;
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

# =============================================================================
# The agent's own number (#490)
# =============================================================================
#
# Fresh scratch repositories per scenario, never the cockpit fixture above: the
# agent's answers depend on exactly which tags the REMOTE holds, and a shared
# origin would make each case's expectation a function of the cases before it.

# The legacy base every scenario stages: a `v*` tag one commit below HEAD, as the
# bridge release is for a checkout after it.
LEGACY_TAG="v2026.8.3"
LEGACY_VERSION="2026.8.3"

# new_agent_repo NAME — a scratch origin ($a_origin) and working repo ($a_repo):
# four August commits, the legacy base tagged (annotated, pushed) on the third.
a_origin=""
a_repo=""
new_agent_repo() {
    local name="$1" i
    a_origin="$work/$name.origin.git"
    a_repo="$work/$name"
    git init -q --bare -b main "$a_origin"
    git init -q -b main "$a_repo"
    ( cd "$a_repo" && git config user.email test@example.com && git config user.name test \
        && git remote add origin "$a_origin" ) || fixture "agent fixture $name: could not configure the scratch repo"
    for i in 1 2 3 4; do commit "$a_repo" "$AUG-0$i" "agent $name $i"; done
    ( cd "$a_repo" && git tag -a "$LEGACY_TAG" -m "legacy base" HEAD~1 \
        && git push -q origin main "refs/tags/$LEGACY_TAG" ) || fixture "agent fixture $name: could not tag the legacy base"
}

# agent_tag REPO TAG [REV] — an annotated tag, created and pushed.
agent_tag() {
    local repo="$1" tag="$2" rev="${3:-HEAD}"
    ( cd "$repo" && git tag -a "$tag" -m "$tag" "$rev" && git push -q origin "refs/tags/$tag" ) \
        || fixture "could not create and push $tag"
}

# agent_tag_light REPO TAG [REV] — a LIGHTWEIGHT tag, created and pushed (the
# remote advertises no peeled line for it).
agent_tag_light() {
    local repo="$1" tag="$2" rev="${3:-HEAD}"
    ( cd "$repo" && git tag "$tag" "$rev" && git push -q origin "refs/tags/$tag" ) \
        || fixture "could not create and push $tag"
}

# run_agent REPO TODAY FLAG... [ENV=value ...] — get-version-info.sh's agent
# modes as run on TODAY, with stdout in $work/agent.out, stderr in
# $work/agent.err, the exit status in $agent_rc, every git call in
# $work/git.calls and every gh call in $work/gh.calls. The cockpit's pin and
# seam are SET to values that would wreck an answer that read them, so every
# case also shows the agent ignores them; the legacy base is staged through its
# seam unless a case passes its own.
agent_rc=0
run_agent() {
    local repo="$1" today="$2"; shift 2
    local flags=() envs=()
    while [[ $# -gt 0 ]]; do
        case "$1" in
            [A-Z_]*=*) envs+=("$1") ;;
            *) flags+=("$1") ;;
        esac
        shift
    done
    agent_rc=0
    : > "$work/git.calls"
    : > "$work/gh.calls"
    ( cd "$repo" && env MARKETING_VERSION=2030.1.9 VERSION_PATCH_OVERRIDE=77 \
        LAST_COMBINED_AGENT_RELEASE="$LEGACY_TAG" VERSION_DATE_OVERRIDE="$today" \
        GH_SHIM_CALLS="$work/gh.calls" PATH="$work/ghshim:$work/shim:$PATH" \
        ${envs[@]+"${envs[@]}"} "$BASH" "$MINT" "${flags[@]}" >"$work/agent.out" 2>"$work/agent.err" ) || agent_rc=$?
}

# agent_field NAME — the value of `NAME=` in the last run_agent's stdout.
agent_field() {
    sed -n "s/^$1=//p" "$work/agent.out"
}

# agent_contract LABEL — the last run_agent's stdout is EXACTLY the output
# contract: three lines, version= / tag= / action= in that order, the tag is
# `agent-v` + the version, and the action is create or reuse.
agent_contract() {
    local label="$1" lines version tag action ok=true why=""
    lines="$(wc -l < "$work/agent.out" | tr -d ' ')"
    version="$(agent_field version)"; tag="$(agent_field tag)"; action="$(agent_field action)"
    [[ "$lines" == "3" ]] || { ok=false; why="$lines lines on stdout, not 3"; }
    [[ "$(sed -n '1p' "$work/agent.out")" == "version=$version" ]] || { ok=false; why="${why:+$why; }line 1 is not version="; }
    [[ "$(sed -n '2p' "$work/agent.out")" == "tag=$tag" ]] || { ok=false; why="${why:+$why; }line 2 is not tag="; }
    [[ "$(sed -n '3p' "$work/agent.out")" == "action=$action" ]] || { ok=false; why="${why:+$why; }line 3 is not action="; }
    [[ "$tag" == "agent-v$version" ]] || { ok=false; why="${why:+$why; }tag '$tag' is not agent-v$version"; }
    case "$action" in
        create | reuse) ;;
        *) ok=false; why="${why:+$why; }action '$action' is not create|reuse" ;;
    esac
    if $ok; then
        echo "ok   $label (contract: version=$version tag=$tag action=$action)"
        pass=$((pass + 1))
    else
        echo "FAIL $label: $why"
        sed 's/^/     /' "$work/agent.out"
        fail=$((fail + 1))
    fi
}

# agent_refused LABEL — the last run_agent was a REFUSAL: exit 1, nothing on
# stdout (no contract is printed for a mint that did not happen), and no push.
agent_refused() {
    control "1" "$agent_rc" "$1: exits 1"
    control "" "$(cat "$work/agent.out")" "$1: ...printing no contract"
    control "0" "$(git_pushes)" "$1: ...pushing nothing"
}

# agent_stderr_has REGEX LABEL
agent_stderr_has() {
    file_mentions "$1" "$work/agent.err" "$2"
}

# remote_agent_tag_names ORIGIN — the origin's agent-v* tags, sorted, one line.
remote_agent_tag_names() {
    "$REAL_GIT" --git-dir="$1" for-each-ref --format='%(refname:short)' 'refs/tags/agent-v*' | sort | tr '\n' ' ' | sed 's/ $//'
}

# =============================================================================
# 5. THE AGENT'S VERSION (--agent-version)
# =============================================================================
echo "--- agent version (--agent-version) ---"

new_agent_repo agent-version
a_head_short="$( cd "$a_repo" && git rev-parse --short HEAD )"

# A source build: the legacy base, the commits since it, HEAD's short sha. The
# base tag is on the third of four commits, so one commit lies since it.
run_agent "$a_repo" "$AUG-20" --agent-version
control "0" "$agent_rc" "agent version: a source build derives (exit 0)"
control "$LEGACY_VERSION+dev.1.g$a_head_short" "$(cat "$work/agent.out")" "agent version: <legacy base>+dev.<commits since>.g<sha> (2026.8.3+dev.1.g$a_head_short)"
control "1" "$(wc -l < "$work/agent.out" | tr -d ' ')" "agent version: exactly one line on stdout"

# Not the cockpit's number and not date-dependent: another day, another
# MARKETING_VERSION / VERSION_PATCH_OVERRIDE, the same answer. (run_agent sets
# both seams to wreckage already, so the first case above is also this proof.)
run_agent "$a_repo" "2031-03-31" --agent-version
control "$LEGACY_VERSION+dev.1.g$a_head_short" "$(cat "$work/agent.out")" "agent version: ignores the clock, MARKETING_VERSION and VERSION_PATCH_OVERRIDE"
run_agent "$a_repo" "$AUG-20" --agent-version AGENT_MARKETING_VERSION=2040.1.1
control "$LEGACY_VERSION+dev.1.g$a_head_short" "$(cat "$work/agent.out")" "agent version: the build pin AGENT_MARKETING_VERSION is the build plumbing's, not read by the script"

# The legacy base is read from scripts/config.sh when the seam is empty — which
# `./dev` and every script that sources config.sh arranges anyway. The fixture
# has no such tag, so the answer is a refusal that names the CONFIGURED base:
# proof the value came from the file and not from the seam.
cfg_release="$(sed -n 's/^export LAST_COMBINED_AGENT_RELEASE="\(.*\)"$/\1/p' "$SCRIPT_DIR/config.sh")"
if [[ "$cfg_release" =~ ^v[1-9][0-9]{3}\.([1-9]|1[0-2])\.[1-9][0-9]*$ ]]; then
    echo "ok   agent version: scripts/config.sh carries LAST_COMBINED_AGENT_RELEASE=$cfg_release in the shape the script reads"; pass=$((pass + 1))
else
    echo "FAIL agent version: scripts/config.sh's LAST_COMBINED_AGENT_RELEASE line is '$cfg_release', not an \`export LAST_COMBINED_AGENT_RELEASE=\"vYYYY.M.N\"\` line"; fail=$((fail + 1))
fi
run_agent "$a_repo" "$AUG-20" --agent-version LAST_COMBINED_AGENT_RELEASE=
control "1" "$agent_rc" "agent version: with the seam empty the base is config.sh's, absent here, so it refuses (exit 1)"
control "" "$(cat "$work/agent.out")" "agent version: ...printing nothing"
agent_stderr_has "base tag $cfg_release is not in this checkout" "agent version: ...naming the configured base"
agent_stderr_has "git fetch --tags" "agent version: ...and the remedy"
run_agent "$a_repo" "$AUG-20" --agent-version LAST_COMBINED_AGENT_RELEASE=2026.8.3
control "1" "$agent_rc" "agent version: a base without its v prefix is refused (exit 1)"
agent_stderr_has "expected vYYYY\.M\.N" "agent version: ...as malformed, not used"

# An agent-v* tag at HEAD IS the version.
agent_tag "$a_repo" agent-v2026.8.4
run_agent "$a_repo" "$AUG-20" --agent-version
control "2026.8.4" "$(cat "$work/agent.out")" "agent version: an agent-v* tag at HEAD is that tag's version (2026.8.4)"

# ...and the highest of several, numerically (lexically 2026.8.9 > 2026.8.10).
( cd "$a_repo" && git tag agent-v2026.8.9 && git tag agent-v2026.8.10 )
run_agent "$a_repo" "$AUG-20" --agent-version
control "2026.8.10" "$(cat "$work/agent.out")" "agent version: the highest tag at HEAD wins, numerically (2026.8.10 over 2026.8.9)"
( cd "$a_repo" && git tag -d agent-v2026.8.9 agent-v2026.8.10 >/dev/null )

# Past it, the base is the highest REACHABLE agent-v* tag, not the legacy one.
commit "$a_repo" "$AUG-05" "agent version 5"
a_head_short="$( cd "$a_repo" && git rev-parse --short HEAD )"
run_agent "$a_repo" "$AUG-20" --agent-version
control "2026.8.4+dev.1.g$a_head_short" "$(cat "$work/agent.out")" "agent version: past an agent tag the base is that tag (2026.8.4+dev.1.g$a_head_short)"

# Not reachable from HEAD: a tag on another line of history is nobody's base.
( cd "$a_repo" && git checkout -q -b elsewhere HEAD~3 ) || fixture "could not branch for the unreachable-tag case"
commit "$a_repo" "$AUG-06" "elsewhere"
( cd "$a_repo" && git tag agent-v2026.9.9 && git checkout -q main )
run_agent "$a_repo" "$AUG-20" --agent-version
control "2026.8.4+dev.1.g$a_head_short" "$(cat "$work/agent.out")" "agent version: an agent tag NOT reachable from HEAD is not a base (agent-v2026.9.9 ignored)"

# Names that look like agent tags and are not versions.
( cd "$a_repo" && git tag agent-latest && git tag agent-v2026.08.5 && git tag agent-vfoo && git tag agent-v2026.8.99-rc1 )
run_agent "$a_repo" "$AUG-20" --agent-version
control "2026.8.4+dev.1.g$a_head_short" "$(cat "$work/agent.out")" "agent version: agent-latest and malformed agent-v* names at HEAD are not versions"

# Nothing at all: a shallow clone cannot count the commits since the base.
git clone -q --no-local --depth 1 "$a_origin" "$work/agent-shallow"
control "true" "$( cd "$work/agent-shallow" && git rev-parse --is-shallow-repository )" "fixture: the agent shallow clone is shallow"
run_agent "$work/agent-shallow" "$AUG-20" --agent-version
control "1" "$agent_rc" "agent version: a shallow clone yields no version (exit 1)"
control "" "$(cat "$work/agent.out")" "agent version: ...and prints NOTHING on stdout"
agent_stderr_has "this checkout is shallow" "agent version: ...naming the checkout"

# A clone that never fetched the legacy base's tag: the commits since it cannot
# be counted, so there is no version rather than a guess.
git clone -q --no-tags "$a_origin" "$work/agent-notags"
control "" "$( cd "$work/agent-notags" && git tag -l )" "fixture: the --no-tags clone has no tags"
run_agent "$work/agent-notags" "$AUG-20" --agent-version
control "1" "$agent_rc" "agent version: a clone without the base tag yields no version (exit 1)"
control "" "$(cat "$work/agent.out")" "agent version: ...and prints NOTHING on stdout"
agent_stderr_has "base tag v2026\.8\.3 is not in this checkout" "agent version: ...naming the missing base"

# Asking never touches the network: an origin that cannot be reached changes
# nothing about the answer.
git clone -q "$a_origin" "$work/agent-offline"
( cd "$work/agent-offline" && git remote set-url origin "$work/does-not-exist.git" )
run_agent "$work/agent-offline" "$AUG-20" --agent-version
control "0" "$agent_rc" "agent version: an unreachable origin does not matter (nothing is fetched)"
control "2026.8.4" "$(cat "$work/agent.out")" "agent version: ...the derivation comes from the clone's own tags (the pushed HEAD carries agent-v2026.8.4)"

# Outside any checkout.
run_agent "$notrepo" "$AUG-20" --agent-version GIT_CEILING_DIRECTORIES="$work"
control "1" "$agent_rc" "agent version: outside a checkout yields no version (exit 1)"
control "" "$(cat "$work/agent.out")" "agent version: ...and prints NOTHING on stdout"
agent_stderr_has "not inside a git checkout" "agent version: ...naming the missing checkout"

# =============================================================================
# 6. THE AGENT MINT (--agent-tag)
# =============================================================================
echo "--- agent mint (--agent-tag) ---"

# --- reuse -------------------------------------------------------------------
# An agent-v* tag already at HEAD on the remote answers for itself. Annotated and
# lightweight, because the remote advertises a peeled line only for the first.
new_agent_repo agent-reuse
agent_tag "$a_repo" agent-v2026.8.4
tags_before="$(remote_agent_tag_names "$a_origin")"
run_agent "$a_repo" "$AUG-20" --agent-tag
control "0" "$agent_rc" "agent mint: a tag at HEAD exits 0"
agent_contract "agent mint: the reuse's stdout is the output contract"
control "reuse" "$(agent_field action)" "agent mint: an annotated agent-v* tag at HEAD is reused"
control "agent-v2026.8.4" "$(agent_field tag)" "agent mint: ...as that very tag"
control "2026.8.4" "$(agent_field version)" "agent mint: ...and that very version"
control "0" "$(git_pushes)" "agent mint: reuse performs no push"
control "0" "$(wc -l < "$work/gh.calls" | tr -d ' ')" "agent mint: reuse asks gh nothing (an existing release is the expected state of a reused tag)"
control "$tags_before" "$(remote_agent_tag_names "$a_origin")" "agent mint: ...and the origin's agent tags are untouched"
agent_stderr_has "reusing \(idempotent re-run\)" "agent mint: the reuse says so on stderr"

new_agent_repo agent-reuse-light
agent_tag_light "$a_repo" agent-v2026.8.6
run_agent "$a_repo" "$AUG-20" --agent-tag
control "reuse" "$(agent_field action)" "agent mint: a LIGHTWEIGHT agent-v* tag at HEAD is reused too"
control "agent-v2026.8.6" "$(agent_field tag)" "agent mint: ...as that very tag"
agent_contract "agent mint: the lightweight reuse's stdout is the output contract"

# Two at HEAD: the highest, numerically.
agent_tag_light "$a_repo" agent-v2026.8.10
run_agent "$a_repo" "$AUG-20" --agent-tag
control "agent-v2026.8.10" "$(agent_field tag)" "agent mint: with two tags at HEAD the higher is reused, numerically (2026.8.10 over 2026.8.6)"

# --- create: the first release in a month, and in the legacy base's month ------
new_agent_repo agent-create
run_agent "$a_repo" "$AUG-20" --agent-tag
control "0" "$agent_rc" "agent mint: a dry run exits 0"
agent_contract "agent mint: the create's stdout is the output contract"
control "create" "$(agent_field action)" "agent mint: with nothing shipped but the legacy base, create"
control "agent-v2026.8.4" "$(agent_field tag)" "agent mint: a release in the legacy base's own month is 1 + its patch (agent-v2026.8.4)"
control "0" "$(git_pushes)" "agent mint: the dry run pushes nothing"
file_lacks "^tag " "$work/git.calls" "agent mint: ...and runs no git tag"
control "" "$(remote_agent_tag_names "$a_origin")" "agent mint: ...and the origin has no agent tag after it"
control "" "$( cd "$a_repo" && git tag -l 'agent-v*' )" "agent mint: ...and neither does the checkout"
file_mentions "Dry run: would create tag agent-v2026\.8\.4" "$work/agent.err" "agent mint: the dry run says so on stderr"
file_mentions "^release view agent-v2026\.8\.4$" "$work/gh.calls" "agent mint: it asked gh whether a release holds that name"

run_agent "$a_repo" "$SEP-10" --agent-tag
control "agent-v2026.9.1" "$(agent_field tag)" "agent mint: the first release in a later month starts at 1 (agent-v2026.9.1), not at the legacy patch"
control "create" "$(agent_field action)" "agent mint: ...as a create"
run_agent "$a_repo" "2027-01-05" --agent-tag
control "agent-v2027.1.1" "$(agent_field tag)" "agent mint: the first release of a new year is 2027.1.1"

# --- create: counted from the REMOTE's tags --------------------------------------
# 2026.8.4, 2026.8.9 and 2026.8.10 shipped (on older commits): the next is
# 2026.8.11 — the numeric maximum, where a lexical one says 2026.8.9 + 1.
new_agent_repo agent-numeric
agent_tag "$a_repo" agent-v2026.8.4 HEAD~2
agent_tag "$a_repo" agent-v2026.8.9 HEAD~2
agent_tag "$a_repo" agent-v2026.8.10 HEAD~2
run_agent "$a_repo" "$AUG-20" --agent-tag
control "agent-v2026.8.11" "$(agent_field tag)" "agent mint: 1 + the NUMERIC maximum of the month's shipped patches (2026.8.11, not 2026.8.10)"
control "create" "$(agent_field action)" "agent mint: ...as a create (no tag of the three is at HEAD)"
run_agent "$a_repo" "$SEP-10" --agent-tag
control "agent-v2026.9.1" "$(agent_field tag)" "agent mint: the next month after those releases starts at 1 again"

# A tag that exists only HERE is not a release: ignored, as a local tag must be.
( cd "$a_repo" && git tag agent-v2026.8.40 HEAD~2 )
run_agent "$a_repo" "$AUG-20" --agent-tag
control "agent-v2026.8.11" "$(agent_field tag)" "agent mint: a LOCAL-only tag is ignored (agent-v2026.8.40 is not shipped)"
( cd "$a_repo" && git tag -d agent-v2026.8.40 >/dev/null )

# Names that look like agent tags and are not versions are not shipped ones.
# agent-latest AT HEAD is not a reuse either.
( cd "$a_repo" && git tag agent-latest && git tag agent-v2026.08.5 && git tag agent-vfoo && git tag agent-v2026.8.99-rc1 \
    && git push -q origin refs/tags/agent-latest refs/tags/agent-v2026.08.5 refs/tags/agent-vfoo refs/tags/agent-v2026.8.99-rc1 ) \
    || fixture "could not push the non-version agent-* tags"
run_agent "$a_repo" "$AUG-20" --agent-tag
control "agent-v2026.8.11" "$(agent_field tag)" "agent mint: agent-latest and malformed agent-v* names on the remote are not counted"
control "create" "$(agent_field action)" "agent mint: ...and agent-latest at HEAD is not a reuse"

# --- refusals -----------------------------------------------------------------
# A release already holds the name — a deleted tag's number must not be handed
# out again. The negative control follows: without that release, the same run
# creates, so the refusal is the release check's.
new_agent_repo agent-release-exists
run_agent "$a_repo" "$AUG-20" --agent-tag GH_SHIM_EXISTING="agent-v2026.8.4"
agent_refused "agent mint: a name whose release exists"
agent_stderr_has "a release named agent-v2026\.8\.4 already exists" "agent mint: ...naming the release"
agent_stderr_has "never delete an agent-v\* tag" "agent mint: ...and the rule that keeps it from recurring"
run_agent "$a_repo" "$AUG-20" --agent-tag GH_SHIM_EXISTING="some-other-tag"
control "agent-v2026.8.4" "$(agent_field tag)" "agent mint: (negative control) the same run with no such release creates agent-v2026.8.4"

# gh cannot say: fail closed rather than mint blind.
run_agent "$a_repo" "$AUG-20" --agent-tag GH_SHIM_FAIL="HTTP 502: Bad Gateway"
agent_refused "agent mint: gh failing for any reason but 'release not found'"
agent_stderr_has "could not ask whether a release named agent-v2026\.8\.4 exists" "agent mint: ...saying what could not be asked"
agent_stderr_has "HTTP 502" "agent mint: ...with gh's own words"
agent_stderr_has "refusing to mint blind" "agent mint: ...and that it never mints blind"

# Not strictly above everything shipped: a tag from a LATER month is on the
# remote (a future-dated mint, or a skewed clock here).
new_agent_repo agent-not-above
agent_tag "$a_repo" agent-v2026.11.2 HEAD~2
run_agent "$a_repo" "$AUG-20" --agent-tag
agent_refused "agent mint: a result below an already shipped version"
agent_stderr_has "agent-v2026\.8\.4 does not sort above 2026\.11\.2" "agent mint: ...naming both"
control "" "$(cat "$work/gh.calls")" "agent mint: ...before it asked gh anything"
run_agent "$a_repo" "2026-12-03" --agent-tag
control "agent-v2026.12.1" "$(agent_field tag)" "agent mint: (negative control) a month past the shipped one mints normally (agent-v2026.12.1)"

# The legacy base itself is shipped history: a clock behind it is refused too.
new_agent_repo agent-behind-legacy
run_agent "$a_repo" "2026-07-20" --agent-tag
agent_refused "agent mint: a month before the legacy base's"
agent_stderr_has "agent-v2026\.7\.1 does not sort above 2026\.8\.3" "agent mint: ...naming the legacy base"

# The base from config.sh (seam empty): a clock behind the CONFIGURED base is
# refused, naming it — the mint reads it from the file when nothing overrides.
run_agent "$a_repo" "2026-09-10" --agent-tag LAST_COMBINED_AGENT_RELEASE=
agent_refused "agent mint: with the seam empty the base is config.sh's"
agent_stderr_has "does not sort above ${cfg_release#v}" "agent mint: ...and it is named ($cfg_release's version)"
run_agent "$a_repo" "$AUG-20" --agent-tag LAST_COMBINED_AGENT_RELEASE=2026.8.3
agent_refused "agent mint: a base without its v prefix"
agent_stderr_has "expected vYYYY\.M\.N" "agent mint: ...as malformed"

# The remote cannot be read: refuse blind, whatever the local tags say.
broken_agent="$work/agent-broken"
git clone -q "$a_origin" "$broken_agent"
( cd "$broken_agent" && git tag agent-v2026.8.1 && git remote set-url origin "$work/does-not-exist.git" )
run_agent "$broken_agent" "$AUG-20" --agent-tag
agent_refused "agent mint: an unreachable origin"
agent_stderr_has "remote tag probe failed" "agent mint: ...saying the probe failed"
agent_stderr_has "refusing to mint blind" "agent mint: ...and that it never mints blind"
( cd "$broken_agent" && git remote remove origin )
run_agent "$broken_agent" "$AUG-20" --agent-tag
agent_refused "agent mint: no origin at all (a local agent tag is right there, and is not read)"
agent_stderr_has "no origin remote" "agent mint: ...naming the missing remote"

# A stray argument is a usage error.
run_agent "$a_repo" "$AUG-20" --agent-tag --extra
control "2" "$agent_rc" "agent mint: --agent-tag with a stray argument is a usage error (exit 2)"
control "" "$(cat "$work/agent.out")" "agent mint: ...printing no contract"

# --- --push ----------------------------------------------------------------------
new_agent_repo agent-push
a_push_head="$( cd "$a_repo" && git rev-parse HEAD )"
run_agent "$a_repo" "$AUG-20" --agent-tag --push
control "0" "$agent_rc" "agent mint: --agent-tag --push exits 0"
agent_contract "agent mint: the pushed create's stdout is the output contract"
control "create" "$(agent_field action)" "agent mint: ...a create"
control "agent-v2026.8.4" "$(agent_field tag)" "agent mint: ...of agent-v2026.8.4"
control "tag" "$( cd "$a_repo" && git cat-file -t agent-v2026.8.4 )" "agent mint: the tag is annotated (a tag object, not a lightweight ref)"
control "$a_push_head" "$( cd "$a_repo" && git ls-remote --tags origin 'refs/tags/agent-v2026.8.4^{}' | cut -f1 )" "agent mint: the origin's peeled agent-v2026.8.4 is HEAD"
control "1" "$(git_pushes)" "agent mint: the create made exactly one git push (positive control: the shim sees pushes)"
file_mentions "^push origin refs/tags/agent-v2026\.8\.4$" "$work/git.calls" "agent mint: ...of that tag's ref, to origin"
file_mentions "Created and pushed tag: agent-v2026\.8\.4" "$work/agent.err" "agent mint: it says what it did"
tags_after_push="$(remote_agent_tag_names "$a_origin")"
control "agent-v2026.8.4" "$tags_after_push" "agent mint: the origin's snapshot names exactly the new tag (the snapshot is a real observation)"

# The same commit again: reuse, no push, no gh, origin untouched.
run_agent "$a_repo" "$AUG-20" --agent-tag --push
control "reuse" "$(agent_field action)" "agent mint: a re-run at the same commit answers reuse"
control "agent-v2026.8.4" "$(agent_field tag)" "agent mint: ...for the same tag"
control "0" "$(git_pushes)" "agent mint: ...performing no push at all"
control "0" "$(wc -l < "$work/gh.calls" | tr -d ' ')" "agent mint: ...and asking gh nothing"
file_lacks "Created and pushed" "$work/agent.err" "agent mint: ...and never claims a push"
control "$tags_after_push" "$(remote_agent_tag_names "$a_origin")" "agent mint: ...and the origin's agent tags are byte-identical"

# A later commit: the next number.
commit "$a_repo" "$AUG-05" "agent push 5"
( cd "$a_repo" && git push -q origin main )
run_agent "$a_repo" "$AUG-21" --agent-tag --push
control "agent-v2026.8.5" "$(agent_field tag)" "agent mint: a later commit mints the next number (agent-v2026.8.5)"
control "create" "$(agent_field action)" "agent mint: ...as a create"

# The remote refuses the push (another mint won the race, or a hook): nothing is
# minted, and the checkout is not left holding a tag the remote never took.
new_agent_repo agent-rejected
printf '#!/bin/sh\necho "remote: rejected by the test hook" >&2\nexit 1\n' > "$a_origin/hooks/pre-receive"
chmod +x "$a_origin/hooks/pre-receive"
run_agent "$a_repo" "$AUG-20" --agent-tag --push

control "1" "$agent_rc" "agent mint: a remote that rejects the push exits 1"
control "" "$(cat "$work/agent.out")" "agent mint: ...printing no contract"
agent_stderr_has "the remote refused agent-v2026\.8\.4" "agent mint: ...saying the remote refused"
control "" "$( cd "$a_repo" && git tag -l 'agent-v*' )" "agent mint: ...and leaving no local tag behind"
control "" "$(remote_agent_tag_names "$a_origin")" "agent mint: ...and none on the origin"

# =============================================================================
# 7. THE AGENT MODE OF THE RELEASE-TAG ASSERTION (assert-release-tag.sh --agent)
# =============================================================================
echo "--- release-tag assertion, agent mode (assert-release-tag.sh --agent) ---"

assert_flag="--agent"
# What the agent mint's `create` path needs when an assertion refuses a tag: the
# gh shim, and the staged legacy base (the assertion pins only the clock).
AENV=(PATH="$work/ghshim:$PATH" GH_SHIM_CALLS="$work/gh.calls" LAST_COMBINED_AGENT_RELEASE="$LEGACY_TAG")

new_agent_repo agent-assert
run_agent "$a_repo" "$AUG-20" --agent-tag --push || true
control "agent-v2026.8.4" "$(agent_field tag)" "fixture: the agent mint tagged agent-v2026.8.4 at HEAD"

# (a) A minted tag passes — and still passes long after its month, which is the
# real clock's September-and-after to this fixture's August commit.
expect pass "(agent) a minted tag, asserted after its month rolled" "$a_repo" agent-v2026.8.4 "${AENV[@]}"
# Under the cockpit's pin inherited from a job: scrubbed, the check does not
# compare the tag to itself.
expect pass "(agent) the minted tag still passes under an inherited MARKETING_VERSION" "$a_repo" agent-v2026.8.4 "${AENV[@]}" MARKETING_VERSION=2030.1.9

# (b) The wrong commit: the same tag asserted at the commit BELOW it. The remote
# has no such tag there, so the mint would create the next number.
( cd "$a_repo" && git checkout -q HEAD~1 )
expect refuse "(agent) a tag asserted at a commit it is not on" "$a_repo" agent-v2026.8.4 "${AENV[@]}"
expect_mentions 'resolves agent-v2026\.8\.5 \(create\) at this commit, not agent-v2026\.8\.4' "     ...naming what the mint resolved instead"
expect_mentions 'version=2026\.8\.5' "     ...and printing the mint's own report"
( cd "$a_repo" && git checkout -q main )

# (c) A local-only tag is not a release: create, not reuse. (At the commit below
# the minted one, where the remote carries nothing.)
( cd "$a_repo" && git checkout -q HEAD~1 && git tag agent-v2026.8.7 )
expect refuse "(agent) a local-only tag the remote has never seen (action=create)" "$a_repo" agent-v2026.8.7 "${AENV[@]}"
expect_mentions 'resolves agent-v2026\.8\.5 \(create\)' "     ...naming create, not reuse"
( cd "$a_repo" && git tag -d agent-v2026.8.7 >/dev/null && git checkout -q main )

# (d) A different tag is what the mint reuses at this commit: the hand-made one
# under test is not it.
expect refuse "(agent) a hand-made tag at a commit that carries another (agent-v2026.8.2)" "$a_repo" agent-v2026.8.2 "${AENV[@]}"
expect_mentions 'resolves agent-v2026\.8\.4 \(reuse\) at this commit, not agent-v2026\.8\.2' "     ...naming the tag the mint reuses"

# (e) The shapes, each refused BEFORE the mint runs. The cockpit's own shape is
# refused in agent mode, and the converse below.
agent_shape() {
    expect refuse "$1" "$a_repo" "$2" "${AENV[@]}"
    expect_mentions 'not an agent-vYYYY\.M\.P CalVer tag' "     ...by the shape check"
    expect_absent "Re-running the mint" "     ...before the mint ran"
}
agent_shape "(agent) the cockpit's own tag shape" "v2026.8.4"
agent_shape "(agent) no prefix" "2026.8.4"
agent_shape "(agent) the rolling feed release's name" "agent-latest"
agent_shape "(agent) a semver tag" "agent-v1.2.3"
agent_shape "(agent) a padded month" "agent-v2026.08.4"
agent_shape "(agent) a padded patch" "agent-v2026.8.04"
agent_shape "(agent) a .0 patch" "agent-v2026.8.0"
agent_shape "(agent) a thirteenth month" "agent-v2026.13.1"
agent_shape "(agent) a suffix" "agent-v2026.8.4-rc1"
agent_shape "(agent) a +dev version" "agent-v2026.8.4+dev.1.gabcdef0"
agent_shape "(agent) a double prefix" "agent-vagent-v2026.8.4"
assert_flag=""
# The converse: an agent tag is not a cockpit tag.
expect refuse "an agent tag asserted in the cockpit's mode" "$a_repo" agent-v2026.8.4
expect_mentions 'not a vYYYY\.M\.P CalVer tag' "     ...by the cockpit's shape check"
expect_absent "Re-running the mint" "     ...before the mint ran"
assert_flag="--agent"

# (f) The clock, both sides.
now_year=$(( 10#$(date -u +%Y) ))
now_month=$(( 10#$(date -u +%m) ))
if (( now_month == 12 )); then
    next_agent_tag="agent-v$((now_year + 1)).1.1"
else
    next_agent_tag="agent-v$now_year.$((now_month + 1)).1"
fi
( cd "$a_repo" && git tag "$next_agent_tag" && git push -q origin "refs/tags/$next_agent_tag" )
expect refuse "(agent) next month's tag ($next_agent_tag, pushed at HEAD)" "$a_repo" "$next_agent_tag" "${AENV[@]}"
expect_mentions "after the current UTC month" "     ...naming the clock"
expect_absent "Re-running the mint" "     ...before the mint ran"
# A month that ENDED before this commit: July's tag hand-pushed at the August
# commit — the mint tags HEAD with the month it runs in, never an earlier one.
( cd "$a_repo" && git tag agent-v2026.7.1 && git push -q origin refs/tags/agent-v2026.7.1 )
expect refuse "(agent) a prior-month tag hand-pushed at a later month's commit" "$a_repo" agent-v2026.7.1 "${AENV[@]}"
expect_mentions "ended \(at 2026-08-01T00:00:00Z\) before this commit was made" "     ...naming the boundary the commit is past"
expect_absent "Re-running the mint" "     ...before the mint ran"
# (The one that ENDS after the commit is the passing case above: an August tag
# at an August commit, asserted in whatever month the real clock is in.)

# (g) The derivation cannot run.
git clone -q "$a_origin" "$work/agent-assert-broken"
( cd "$work/agent-assert-broken" && git checkout -q agent-v2026.8.4 && git remote set-url origin "$work/does-not-exist.git" )
expect refuse "(agent) the mint's remote probe fails (origin unreachable)" "$work/agent-assert-broken" agent-v2026.8.4 "${AENV[@]}"
expect_mentions "could not run \(the mint exited non-zero" "     ...naming the derivation, not the tag"
( cd "$work/agent-assert-broken" && git remote remove origin )
expect refuse "(agent) no origin remote (the mint would not read local tags)" "$work/agent-assert-broken" agent-v2026.8.4 "${AENV[@]}"
expect_mentions "no origin remote" "     ...naming the missing remote"
expect_absent "Re-running the mint" "     ...before the mint ran"
git clone -q --no-local --depth 1 "$a_origin" "$work/agent-assert-shallow"
( cd "$work/agent-assert-shallow" && git fetch -q --depth 1 origin "refs/tags/agent-v2026.8.4:refs/tags/agent-v2026.8.4" && git checkout -q agent-v2026.8.4 ) \
    || fixture "could not fetch agent-v2026.8.4 into the shallow clone"
control "true" "$( cd "$work/agent-assert-shallow" && git rev-parse --is-shallow-repository )" "fixture: the agent assertion's shallow clone is shallow"
expect refuse "(agent) a shallow clone (fetch-depth: 1)" "$work/agent-assert-shallow" agent-v2026.8.4 "${AENV[@]}"
expect_mentions "this checkout is shallow" "     ...naming the checkout"
expect_absent "Re-running the mint" "     ...before the mint ran"
expect refuse "(agent) outside any git checkout" "$notrepo" agent-v2026.8.4 GIT_CEILING_DIRECTORIES="$work"
expect_mentions "not inside a git checkout" "     ...naming the missing checkout"

# (h) Usage. `--agent` alone, an empty tag, and two tags are not refusals.
for argv in "--agent" "--agent ''" "--agent agent-v2026.8.4 extra"; do
    rc=0
    ( cd "$a_repo" && eval "\"\$BASH\" \"\$ASSERT\" $argv" ) >/dev/null 2>&1 || rc=$?
    control "2" "$rc" "(agent) usage [$argv] is exit 2, not a refusal"
done
assert_flag=""

# =============================================================================
# 8. ./dev publish --agent (scripts/publish.sh)
# =============================================================================
echo "--- publish --agent (scripts/publish.sh) ---"

PUBLISH="$SCRIPT_DIR/publish.sh"

# publish.sh sources scripts/config.sh, whose unconditional `export` of
# LAST_COMBINED_AGENT_RELEASE replaces any seam passed in — so this section runs
# against the REAL configured base, and its expectations are computed from it
# rather than copied, so a later bump of that line moves them too.
cfg_version="${cfg_release#v}"
cfg_prefix="${cfg_version%.*}"
cfg_patch="${cfg_version##*.}"
pub_expect_tag="agent-v$cfg_prefix.$((cfg_patch + 1))"
pub_today="$(printf '%04d-%02d-20' "${cfg_prefix%%.*}" "${cfg_prefix##*.}")"

# new_publish_repo NAME — a scratch repo on `main`, current with its origin, clean.
new_publish_repo() {
    local name="$1"
    a_origin="$work/$name.origin.git"
    a_repo="$work/$name"
    git init -q --bare -b main "$a_origin"
    git init -q -b main "$a_repo"
    # `core.autocrlf false`: publish.sh's `git diff --quiet` clean-tree check must
    # see the file as written, on a Windows runner whose default is `true` too.
    # The stub workflow file is what `publish --agent` requires HEAD to carry.
    ( cd "$a_repo" && git config user.email test@example.com && git config user.name test \
        && git config core.autocrlf false \
        && git remote add origin "$a_origin" && echo one > f.txt \
        && mkdir -p .github/workflows && echo "name: stub" > .github/workflows/release-agent.yml \
        && git add f.txt .github/workflows/release-agent.yml ) || fixture "publish fixture $name: could not set up"
    ( cd "$a_repo" && GIT_AUTHOR_DATE="${AUG}-01T12:00:00Z" GIT_COMMITTER_DATE="${AUG}-01T12:00:00Z" git commit -q -m one \
        && git push -q origin main ) || fixture "publish fixture $name: could not commit and push"
}

# run_publish REPO FLAG... [ENV=value ...] — scripts/publish.sh as run in REPO.
# The cockpit's three credentials are scrubbed: the point of the agent mode is
# that it needs none of them.
pub_rc=0
run_publish() {
    local repo="$1"; shift
    local flags=() envs=()
    while [[ $# -gt 0 ]]; do
        case "$1" in
            [A-Z_]*=*) envs+=("$1") ;;
            *) flags+=("$1") ;;
        esac
        shift
    done
    pub_rc=0
    : > "$work/git.calls"
    : > "$work/gh.calls"
    local ci_head
    ci_head="$( cd "$repo" && git rev-parse HEAD )"
    ( cd "$repo" && env -u SENTRY_DSN -u TAURI_SIGNING_PRIVATE_KEY -u TAURI_SIGNING_PRIVATE_KEY_PASSWORD \
        -u MARKETING_VERSION -u LAST_COMBINED_AGENT_RELEASE \
        VERSION_DATE_OVERRIDE="$pub_today" GH_SHIM_CALLS="$work/gh.calls" GH_SHIM_CI_COMMIT="$ci_head" \
        PATH="$work/ghshim:$work/shim:$PATH" \
        ${envs[@]+"${envs[@]}"} "$BASH" "$PUBLISH" ${flags[@]+"${flags[@]}"} >"$work/pub.out" 2>"$work/pub.err" ) || pub_rc=$?
}
pub_output_has() {
    if grep -qE -- "$1" "$work/pub.out" "$work/pub.err"; then
        echo "ok   $2"; pass=$((pass + 1))
    else
        echo "FAIL $2: neither stream matches /$1/"
        sed 's/^/     /' "$work/pub.out" "$work/pub.err"
        fail=$((fail + 1))
    fi
}
pub_refused() {
    control "1" "$pub_rc" "$1: exits 1"
    control "" "$(remote_agent_tag_names "$a_origin")" "$1: ...and no agent tag reached the origin"
    control "0" "$(git_pushes)" "$1: ...nor was anything pushed"
}

new_publish_repo publish-agent
pub_head="$( cd "$a_repo" && git rev-parse HEAD )"
run_publish "$a_repo" --agent
control "0" "$pub_rc" "publish --agent: mints with no SENTRY_DSN or TAURI_SIGNING_* in the environment (exit 0)"
control "tag" "$( cd "$a_repo" && git cat-file -t "$pub_expect_tag" )" "publish --agent: it created $pub_expect_tag, an annotated tag (1 + the configured base's patch)"
control "$pub_head" "$( cd "$a_repo" && git ls-remote --tags origin "refs/tags/$pub_expect_tag^{}" | cut -f1 )" "publish --agent: ...and pushed it at HEAD"
control "$pub_expect_tag" "$(remote_agent_tag_names "$a_origin")" "publish --agent: ...and it is the only agent tag the origin holds"
control "1" "$(git_pushes)" "publish --agent: exactly one git push"
file_mentions "^push origin refs/tags/$pub_expect_tag$" "$work/git.calls" "publish --agent: ...of that tag"
pub_output_has "Minted $pub_expect_tag \(create\)" "publish --agent: it reports what it minted"
pub_output_has "release-agent\.yml" "publish --agent: ...and what the push triggers"
if grep -qiE "SENTRY|TAURI_SIGNING|\.dmg|Building release version" "$work/pub.out" "$work/pub.err"; then
    echo "FAIL publish --agent: its output mentions a cockpit credential or build"
    sed 's/^/     /' "$work/pub.out" "$work/pub.err"
    fail=$((fail + 1))
else
    echo "ok   publish --agent: it names no cockpit credential and starts no build"; pass=$((pass + 1))
fi
file_mentions "^run list " "$work/gh.calls" "publish --agent: it asked CI about HEAD"

# Re-run at the same HEAD: reuse, nothing pushed.
tags_published="$(remote_agent_tag_names "$a_origin")"
run_publish "$a_repo" --agent
control "0" "$pub_rc" "publish --agent: a re-run at the same HEAD exits 0"
pub_output_has "Minted $pub_expect_tag \(reuse\)" "publish --agent: ...reusing the tag"
control "0" "$(git_pushes)" "publish --agent: ...pushing nothing"
control "$tags_published" "$(remote_agent_tag_names "$a_origin")" "publish --agent: ...and the origin is untouched"
pub_output_has "pushed nothing" "publish --agent: ...and it says so"

# Each pre-flight refuses before anything is tagged.
new_publish_repo publish-agent-ci
run_publish "$a_repo" --agent GH_SHIM_CI_GREEN=0
pub_refused "publish --agent: CI not green on HEAD"
pub_output_has "No green 'CI' workflow run found" "publish --agent: ...naming the cause"
run_publish "$a_repo" --agent GH_SHIM_CI_FAIL=1
pub_refused "publish --agent: CI unknowable (gh errors)"
pub_output_has "Refusing to mint blind" "publish --agent: ...refusing blind"

( cd "$a_repo" && echo two >> f.txt )
run_publish "$a_repo" --agent
pub_refused "publish --agent: a dirty working tree"
pub_output_has "Working tree is not clean" "publish --agent: ...naming it"
( cd "$a_repo" && git checkout -q -- f.txt )

( cd "$a_repo" && git checkout -q -b not-main )
run_publish "$a_repo" --agent
pub_refused "publish --agent: not on main"
pub_output_has "Not on main branch" "publish --agent: ...naming it"
( cd "$a_repo" && git checkout -q main )

new_publish_repo publish-agent-behind
git clone -q "$a_origin" "$work/publish-agent-behind-other"
( cd "$work/publish-agent-behind-other" && git config user.email test@example.com && git config user.name test \
    && echo two >> f.txt && git add f.txt && git commit -q -m two && git push -q origin main ) || fixture "could not advance the origin's main"
run_publish "$a_repo" --agent
pub_refused "publish --agent: local main behind origin/main"
pub_output_has "not up to date with origin/main" "publish --agent: ...naming it"

# The CI gate asks the RIGHT question: the `CI` workflow, at THIS commit. The
# shim counts a green run only for that, so a CI answer about another commit is
# a refusal (the positive case above is its control: the same repo is accepted
# when the question matches).
new_publish_repo publish-agent-ci-question
run_publish "$a_repo" --agent GH_SHIM_CI_COMMIT=0000000000000000000000000000000000000000
pub_refused "publish --agent: CI green at some OTHER commit"
pub_output_has "No green 'CI' workflow run found" "publish --agent: ...naming the cause"
file_mentions "^run list .*--workflow CI .*--commit" "$work/gh.calls" "publish --agent: ...having asked about the CI workflow at a commit"

# A refusal by the mint propagates: exit 1, no "Minted" claim, nothing pushed.
new_publish_repo publish-agent-mint-refuses
run_publish "$a_repo" --agent GH_SHIM_EXISTING="$pub_expect_tag"
pub_refused "publish --agent: a mint that refuses (a release already holds $pub_expect_tag)"
pub_output_has "a release named $pub_expect_tag already exists" "publish --agent: ...saying why"
if grep -q "Minted" "$work/pub.out" "$work/pub.err"; then
    echo "FAIL publish --agent: a refused mint still claims it minted"; fail=$((fail + 1))
else
    echo "ok   publish --agent: ...and claims no mint"; pass=$((pass + 1))
fi

# HEAD must carry the agent release workflow: a tag push runs the workflow file
# as it is at the tagged commit, so a tag minted without it builds nothing and
# burns a number that can never be reused. Refused before anything is minted.
new_publish_repo publish-agent-no-workflow
( cd "$a_repo" && git rm -q .github/workflows/release-agent.yml && git commit -q -m "no workflow" && git push -q origin main ) \
    || fixture "could not remove the stub workflow from publish-agent-no-workflow"
run_publish "$a_repo" --agent
pub_refused "publish --agent: HEAD lacks .github/workflows/release-agent.yml"
pub_output_has "release-agent\.yml" "publish --agent: ...naming the missing workflow"
pub_output_has "Nothing was tagged" "publish --agent: ...and that nothing was tagged"
file_lacks "^release view" "$work/gh.calls" "publish --agent: ...before the mint asked gh about any release"

# The cockpit's options are about a build this mode does not make.
for opt in --skip-tests --skip-sentry --skip-mint; do
    run_publish "$a_repo" --agent "$opt"
    control "1" "$pub_rc" "publish --agent $opt: refused (exit 1)"
    pub_output_has "do not apply" "publish --agent $opt: ...saying the cockpit's options do not apply"
    control "" "$(remote_agent_tag_names "$a_origin")" "publish --agent $opt: ...and nothing was tagged"
done

# And ./dev routes there: its help names the option.
dev_help="$( cd "$SCRIPT_DIR/.." && "$BASH" ./dev help 2>&1 )" || true
if grep -q -- "publish --agent" <<< "$dev_help"; then
    echo "ok   ./dev help documents publish --agent"; pass=$((pass + 1))
else
    echo "FAIL ./dev help does not mention publish --agent"; fail=$((fail + 1))
fi

echo
echo "versioning: $pass passed, $fail failed"
[[ $fail -eq 0 ]]
