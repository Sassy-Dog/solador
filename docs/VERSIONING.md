# Versioning — Solador instance

This repo's instance of a **Versioning spec v1.0** frozen 2026-07-11. The spec
itself is kept privately; everything it requires of this repo is restated here,
so this document stands alone. When this doc and the scripts disagree, that is
drift — fix one of them in the same PR.

## Classification (§7)

**Desktop app row.** Two shipping tiers, on **one** number:

- **Solador** (`app/`): the cockpit — a `.dmg` plus its `.app.tar.gz` updater
  payload on macOS, an NSIS installer on Windows.
- **Rust agent** (`agent/`): four cross-compiled, minisigned target binaries
  (`x86_64`/`aarch64-unknown-linux-musl`, `aarch64`/`x86_64-apple-darwin`), on
  the **same tag and the same GitHub Release** as the cockpit. One tag, one
  release, both products — deliberately not a second release train.

**The agent was N/A until [#390](https://github.com/Sassy-Dog/solador/issues/390),
and this is the revisit that clause asked for.** It read: *"an internal artifact
hand-deployed to our own hosts by operator-run scripts (`agent/deploy/redeploy.sh`),
never published to a registry or distributed externally … Revisit at the first
artifact that leaves our machines."* Those binaries are that artifact. Landing
#390 without this edit would have left this document asserting the opposite of
what ships, which its own opening rule forbids.

What changed, concretely:

- The agent's version **is** the marketing CalVer, from the one owner. It
  reaches the binary the same way the cockpit's does — `agent/build.rs` calls
  `crates/buildversion`, which shells out to `scripts/get-version-info.sh` and
  honours a `MARKETING_VERSION` pin ahead of deriving. `crates/buildversion`
  exists so that plumbing has ONE implementation rather than one per build
  script.
- `solador-agent --version` prints it and nothing else, and `/v1/health`'s
  `version` serves the same string. One binary, one answer.
- `agent/Cargo.toml`'s `0.5.0` **no longer names a release** and nothing reads
  it at runtime. It survives as the *wire-contract marker* its comment block has
  always been — a minor records a key the agent has never produced before, which
  is what `crates/wire`'s tolerance notes cite. It is now unpublished package
  metadata in the same sense as `app/src-tauri/Cargo.toml`'s `0.1.0`, and the
  §7 internal-tools semver exception that used to justify it no longer applies
  to a release version, because it is not one.
- Both deploy scripts read the version **out of the built binary**
  (`agent/deploy/lib.sh`'s `binary_version`, which runs `--version`) instead of
  parsing `agent/Cargo.toml`. `crate_version()` is gone: it would now assert the
  wire-contract number against `/v1/health` and blame the agent for the
  mismatch.

**A build that cannot name itself carries no version at all**, and that is the
same `Option` the cockpit has. `agent/src/main.rs`'s `VERSION` is
`option_env!("SOLADOR_MARKETING_VERSION")`; where it is `None`, `--version`
exits non-zero with the reason, `/v1/health` **omits** the key (never `null`,
never a stand-in), and the cockpit's Settings row reads `agent version —`.
`binary_version` fails closed on it, so a deploy cannot fall through to "just
come back online" and report success without ever proving which binary is
serving.

## The two numbers (§1–§3)

| Number | Owner (single source) | Value |
|---|---|---|
| Marketing version | `scripts/get-version-info.sh --version` | CalVer `YYYY.M.<commits-this-month>` (UTC, non-padded month, floored at 1) |
| Build number | `scripts/get-build-number.sh [--at <ref>]` | `git rev-list --count` — total commits, monotonic forever, never date-gated |

Consumers — version is **never** computed anywhere else:

- **`scripts/build.sh`'s bundle path** (`./dev build --release`, and
  `--bundle` for the CI shape) — the build-time consumer, as of **#303**. The
  marketing version reaches `cargo tauri build` as a `--config
  '{"version":…}'` overlay and lands as `CFBundleShortVersionString`; the build
  number is stamped over `CFBundleVersion` afterwards, because Tauri's config
  has exactly ONE version field and would otherwise write the marketing version
  into both keys. `app/src-tauri/tauri.conf.json` now authors **no** `version`
  at all — the number is derived, not written down — and the build asserts both
  plist keys back out of the artifact, so a silent fall back to a package
  version is a red build rather than a quiet lie. The Xcode build that injected
  these as `MARKETING_VERSION` / `CURRENT_PROJECT_VERSION` went with the
  original macOS app.
- **The Windows bundle path** (same commands, on Windows — **#341**) rides the
  same `--config` overlay, with one twist: the overlay version carries the
  build number as semver build metadata (`2026.8.14+152`), because Tauri maps
  that metadata onto the fourth word of the NSIS installer's
  `VIProductVersion`. Windows has no Info.plist; the analogue is the PE
  VERSIONINFO resource compiled into the installer, and `build.sh` asserts
  both numbers back out of it (`VS_FIXEDFILEINFO` = `YYYY.M.P.<build>`, string
  `ProductVersion` = `YYYY.M.P+<build>`) via `FileVersionInfo` — same
  derive-then-assert standard as the plist keys. MSI is not built at all:
  its `ProductVersion` caps the major field at 255, which CalVer's year
  cannot fit.
- **`scripts/build-agent.sh`** (`./dev agent`) — the agent's build-time
  consumer, as of **#390**. It calls `--version` once, names every artifact
  `solador-agent-<version>-<triple>`, and then reads the version back **out of
  each binary** on every runner that can execute it — the same derive-then-assert
  standard the plist keys are held to. `.github/workflows/release.yml` completes
  that: `--version` is executed for all four targets on runners matching them
  (`ubuntu-latest`, `ubuntu-24.04-arm`, `macos-latest`, `macos-15-intel`) and
  compared against the tag, before anything is signed or uploaded. An artifact
  that does not start is worse than no artifact, and a cross-compiled binary
  links perfectly well on a machine that cannot run one instruction of it.
- `scripts/publish.sh` consumes the mint's output contract (below) and pins
  the build via `MARKETING_VERSION=<minted>` so the artifact is stamped with
  exactly the tagged version.
- In-app displays (Settings footer, OpenClaw client version) read
  `CFBundleShortVersionString` from the stamped bundle — downstream of the
  scripts, compliant.
- The **About string** and the **Sentry release name** on an opt-in crash
  report (#309) both read `settings::VERSION`, which is now the derived CalVer:
  `app/src-tauri/build.rs` runs `get-version-info.sh --version` and publishes it
  as `SOLADOR_MARKETING_VERSION`. It computes nothing of its own, and an
  explicit `MARKETING_VERSION` in the environment wins over deriving — the same
  pin `publish.sh` sets so the artifact carries the version the *tag* carries
  rather than a fresh re-derive. Since #390 that build script is two lines: the
  work is `crates/buildversion::emit_marketing_version`, shared verbatim with
  `agent/build.rs`, because the *plumbing* around the one script (a worktree's
  `.git` file, the ref file a commit rewrites, the shallow-clone refusal) is
  fiddly enough that two copies would diverge at the first fix.
- `settings::VERSION` is an `Option<&str>`, and the `None` arm is load-bearing.
  A **shallow clone cannot be asked** how many commits landed this month: it
  answers `1` rather than failing, which is why the bundle job pins
  `fetch-depth: 0`. On a shallow checkout the build script emits nothing, About
  renders `Version —`, and the crash report carries **no release** rather than a
  stand-in — Sentry groups and regresses by release, so one shared placeholder
  release would make a fixed crash read as regressed on the next build.

**Replay pins / test seams** (org-canonical, §3): `MARKETING_VERSION` and
`BUILD_NUMBER` pin verbatim (a pin is never auto-bumped — a mint collision
under a pin fails loudly); `VERSION_DATE_OVERRIDE` / `VERSION_PATCH_OVERRIDE`
are test seams.

## Mint (§4, mode 2 — local)

Exactly one mint site: `scripts/publish.sh` (→ `./dev publish`) invoking
`scripts/get-version-info.sh --tag --push`:

1. Pre-flight: clean tree, on `main`, local `main` == `origin/main`.
2. **CI-green check** (mode-2 requirement): a completed, successful `CI`
   workflow run must exist for HEAD (`gh run list --commit`); fails closed
   without `gh` or without a verdict.
3. **Build credentials** (#402): `SENTRY_DSN` (or an explicit `--skip-sentry`)
   and the `TAURI_SIGNING_PRIVATE_KEY` / `_PASSWORD` pair (password set;
   empty only for an unencrypted key) — all from Doppler `solador/prd`, see
   `docs/SECRETS.md` — fail closed here, before the mint, so a missing one
   costs a re-run and never a pushed tag.
4. Tests (`scripts/test.sh`, skippable with `--skip-tests`).
5. **Mint**: probe `git ls-remote --tags origin` (remote-visible, never
   locally-cached tags; annotated tags peeled via `^{}`), then the ladder —
   tag exists at HEAD → **reuse** (idempotent re-run — within the same UTC
   month, see step 6); exists elsewhere →
   **bump** patch until free (the bumped version IS the version); free →
   **create + push** annotated `vYYYY.M.P`. Probe failure → fail closed,
   never mint blind. Output contract: one `(version, tag, action)` triple.
6. Release build stamped from the minted version (tag lands before the build
   on purpose: a failed build re-runs into the same-commit reuse branch).
   **Reuse holds only within the same UTC month.** The derivation is
   wall-clock (`date -u`), so after a roll the ladder derives a new train at
   the same HEAD and *creates* the new month's floor slot `vYYYY.M.1` rather
   than reusing — a second release of the same commit. So a failed CI leg
   after a roll is redone by **re-running that workflow run**, not by
   re-running `./dev publish`; and a fresh tag is best cut once `main` carries
   a commit in the new month, because a floor slot taken at a prior-month HEAD
   is what makes the month's first real commit ladder-bump.
7. **The workflows assert the tag by asking the mint, not by re-deriving**
   ([#404](https://github.com/Sassy-Dog/solador/issues/404)). Every leg of
   `release.yml` and `publish-feed.yml`'s desktop `feed` job (on both
   `release: published` and its `workflow_dispatch` door) run
   `scripts/assert-release-tag.sh <tag>` at the tag: it re-runs `--tag`
   **without `--push`** (read-only), pinned to the 1st of the tag's own month
   through `VERSION_DATE_OVERRIDE`, and requires the ladder to answer
   `action=reuse` for exactly that tag. A re-run after the month rolls
   derives the original number again and passes (`v2026.8.139`, attempt 2,
   had failed the bare `--version` comparison this replaced); a ladder-bumped
   tag walks up to its bumped number and passes, so "the bumped version IS
   the version" in step 5 now holds for the release as well as the mint; a
   hand-made tag on the wrong commit derives a number the ladder never walks
   *down* from and is refused with every cause named. Refused before the mint
   runs, because these are shapes the ladder cannot see: a name that is not
   `vYYYY.M.P`; a month after the current UTC month (the mint's clock never
   reaches it, and pinned there the ladder *would* reuse a floor slot at any
   commit); a month that **ended before HEAD's committer date** (the mint
   tags HEAD with the month it runs in, never earlier — and pinned to a past
   month the ladder counts every commit up to a later HEAD, so a hand-pushed
   `vYYYY.M.K` with the right `K` would reuse; September's floor slot at an
   August commit survives, its month is *later*); a shallow clone (it counts
   `1`); and no `origin` remote (the probe would read local tags). The replay
   seams are scrubbed from the mint's environment so an inherited pin cannot
   make the check compare the tag to itself. The probe is a live `ls-remote`,
   so a network failure fails the step closed; the remedy for that is a
   re-run and nothing else. Each `release.yml` build leg then **pins
   `MARKETING_VERSION=<tag's version>`** for its build — the same pin
   `publish.sh` sets locally — because `build.sh`, `build-agent.sh` and
   `crates/buildversion` all derive with the wall clock otherwise, and in the
   two passing cases above that re-derivation would be the mislabelled
   artifact the assertion exists to prevent; and each leg's validate step
   then reads the version **back out of the artifact** (`Info.plist` in the
   updater tarball, the NSIS `VERSIONINFO`, the agent's `--version`) and
   compares it to the **tag**, never to a derivation, so a pin that failed
   to reach the build is a red run rather than a mislabelled release.
   `publish-feed.yml` builds nothing versioned (the manifest's `version` is
   the tag's) and sets no pin; its `agent-feed` job asserts the approved
   *commit* rather than the tag (#391), as before. What is still true of a
   **draft**: its assets are not public until it is published, and publishing
   is what triggers the feed — but the month no longer matters to either
   workflow. **Only tags cut after #404 merged get this assertion**: both
   workflows run the copy of themselves at the tagged commit
   (`publish-feed.yml`'s header records the measurement), so a re-run or a
   publish of a tag cut *before* still runs that tag's bare comparison, and
   the recipe in #404's comments is how such a tag is retired. The one
   exception is `publish-feed.yml`'s desktop replay
   door: a `main`-ref dispatch reads the assertion script from the ref it was
   started from (a second checkout into `.tooling`, with the mint run against
   the tag checkout), so it still regenerates `latest.json` for a tag older
   than #404, whose tree has no such script. `scripts/versioning-test.sh`
   proves the three outcomes against a temporary bare origin;
   `scripts/publish.sh`'s epilogue states the remedy.

## Tags (§5)

Umbrella `v*` only, no tier tags, no tier-vs-tag change detection. The two
shipping tiers deliberately share **one** number and one tag, so there is
nothing to path-scope: a tag push builds the cockpit and the agent from the same
commit into the same release.

The **accepted cost** of that sharing, stated rather than discovered: the agent
gets a new version on every cockpit-only release, including ones where not a
byte of `agent/` changed. What the agent feed offers against that is a
**content hash** per binary (`agent-latest.json`, #391) — a consumer compares
bytes, not versions, and same bytes mean stop. How much that buys is bounded
by this very section: the agent compiles its CalVer in, so a cockpit-only
release *does* change the agent's bytes today, and the hash rule fires only on
a rebuild with no change at all (`docs/AGENT-DISTRIBUTION.md` §3 states it
plainly). Making the agent's bytes version-independent is a separate decision.

Declared stance: **no channel tags yet.** This clause used to read "builds are
unsigned/un-notarized and local-only until #15 lands"; that is no longer the
reason, because [#15](https://github.com/Sassy-Dog/solador/issues/15) has
landed and external distribution has happened. The reason now is that nothing
consumes a per-submission channel tag: distribution is direct download plus the
`latest.json` and `agent-latest.json` feeds, and no App Store submission exists
to date. Add
`mac-direct/<version>-<build>-<UTCts>` at the first submission that needs one.

### Mapping onto the update feed (the desktop `latest.json`)

The update mechanism is **`tauri-plugin-updater`**, settled in
[#304](https://github.com/Sassy-Dog/solador/issues/304). Its manifest carries
**exactly one `version` field**, and the default comparison is
`update.version > current` against the running app's configured version
([v2 docs](https://v2.tauri.app/plugin/updater)). So the two numbers land
asymmetrically:

- **Marketing version — the sole comparison key.** It reaches the bundle as
  `tauri.conf.json`'s `version` via the `--config` overlay (see Consumers,
  above) and therefore as `CFBundleShortVersionString`, and the same string is
  the manifest's one `version`. The manifest is generated from the mint's
  output — never hand-edited.
- **Build number — artifact only.** It is stamped over `CFBundleVersion`
  (#303) and it is what the `mac-direct/<version>-<build>-<UTCts>` channel tag
  above consumes. It has **no update-feed consumer**: one manifest field means
  one number carries the comparison, and it is not this one. Say it that
  narrowly — "no consumer" would be false, and the artifact half is asserted
  by `build.sh` on every bundle.

**Why one key is safe: CalVer is monotonic under semver ordering.** The
plugin compares as semver, and `YYYY.M.<commits-this-month>` compares
field-by-field the same way: `2026.8.40` < `2026.9.1` < `2027.1.1`. Every
reset of a lower field is paired with an increase in the field above it (the
patch resets only as the month advances, the month only as the year does), and
within a month the commit count only grows — so of any two versions the mint
derives, the later one sorts higher. The **non-padded** month is part of this
and not cosmetic: semver forbids leading zeroes in numeric identifiers, so
`2026.08.1` would not parse as a version at all. None of this mattered under
the superseded two-key mapping below, whose comparison key was a plain
monotonic integer — which is why the property was written down nowhere until
the comparison came to rest on this number.

**Superseded:** this section previously mapped Sparkle's two keys per the §7
macOS row — `sparkle:version` = build number (the comparison key),
`sparkle:shortVersionString` = marketing version (display only), appcast
generated. Sparkle is macOS-only and was inherited from the deleted SwiftUI
app rather than chosen here; #304 replaced it. The substantive change is not
the key names but the build number's demotion from comparison key to
artifact-only.

## Migration record (§6)

- Adopted 2026-07 from semver (last shipped tag: `v0.1.1`). **No cutover
  gate**: `2026.M.P` strictly exceeds `0.1.1`, so the switch was
  monotonic-safe mid-month; the first CalVer tag goes through the §4 mint
  like every other.
- **Build number unchanged**: `rev-list --count` was already the scheme
  (previously inline in `scripts/lib.sh`, now owned by
  `scripts/get-build-number.sh`), so no §6 offset is needed — the count only
  grows.
- Retired at adoption (§10 verified-in-sync-duplicate ban + adoption audit):
  `scripts/config.sh` `VERSION`, the hand-bumped `project.yml`
  `MARKETING_VERSION`, publish.sh's equality gate + `--bump` semver flow, and
  `lib.sh`'s `parse_version` / `increment_version` helpers.

## CI (§8) — before the mint ever moves to CI

The mint's create/push half is local-only; `ci.yml` computes no versions, so
it needs no special checkout. **If the mint (or any version computation) ever
moves into a workflow**: that job MUST check out with `fetch-depth: 0` **and**
fetch tags (two distinct requirements — tags present, and the §4 probe
actually performed), keep UTC dates, and remain the single mint site (a CI
release action consumes the minted tag, never `tag_name:`-creates its own).

One thing that *does* run in CI, by design and not in breach of the above:
`release.yml` and `publish-feed.yml` re-run the ladder **read-only** (`--tag`
without `--push`, §4 step 7 / #404) under `fetch-depth: 0` with the month
deliberately **pinned** to the tag's through `VERSION_DATE_OVERRIDE`. That is
not the mint moving to CI and it is not a UTC-date violation to "fix": the
pin is what lets a tag be asserted in the month it was minted rather than the
month the run happens to land in, and it creates nothing.

## Adoption status (§9)

**Shipping.** This section read "pre-release: Solador has not yet shipped an
artifact intended to leave a developer's machine (publish builds are unsigned;
do NOT distribute externally until #15)" — untrue since
[#15](https://github.com/Sassy-Dog/solador/issues/15) closed: `release.yml`
publishes signed, notarized, stapled macOS artifacts
([#306](https://github.com/Sassy-Dog/solador/issues/306),
[#307](https://github.com/Sassy-Dog/solador/issues/307)), an Authenticode-signed
Windows installer ([#341](https://github.com/Sassy-Dog/solador/issues/341),
[#342](https://github.com/Sassy-Dog/solador/issues/342)), and — since
[#390](https://github.com/Sassy-Dog/solador/issues/390) — the agent's four
minisigned binaries. `v2026.8.110` was the first release ever cut. Per the §9
adoption-timing rule the scheme was wired and active before then, so each
distributed build simply uses whatever CalVer resolves at that moment. Adoption
is one-way — no semver "1.0 moment" is coming back.

## Tests (§3, mandatory)

**These scripts are covered by `scripts/versioning-test.sh`** (#405) —
roughly 160 cases, dependency-free bash in the shape of
`agent/deploy/lib_test.sh`, against temporary bare-origin git repositories
with real `ls-remote` probes; nothing pushes anywhere but the scratch
origin. It runs from `./dev test` and in three CI jobs, each under the
interpreter that leg's release path can be run under: `agent-tests` (Linux,
bash 5), `rust-workspace` (macOS stock `/bin/bash` 3.2 — the oldest bash a
macOS release runner or `./dev publish` can resolve, since every script is
`#!/usr/bin/env bash` and a Homebrew bash ahead on `PATH` is newer) and
`windows-tests` (Git Bash, the Windows release leg's). The scripts under
test and the mint run under `"$BASH"`, so each leg tests them under its own
bash and not whichever is first on `PATH` — with one exception the mint
itself carries: `get-version-info.sh --build` delegates to
`get-build-number.sh` through a bare `bash`, so that one call resolves
`PATH`.

What it holds, against the vector list the deleted `VersioningScriptTests`
once covered (that Swift file went with the original macOS app, and the
mint shipped `v2026.8.110` through `v2026.9.12` on #404's partial fixture or
none at all):

- **Derivation** (`--version`): the patch floor (a month with no commits
  derives `.1`, at any distance past the last commit), the month-roll reset
  (September restarts at 1 while the August count keeps growing at the same
  commit), §2 idempotency, the non-padded month, both seams
  (`VERSION_DATE_OVERRIDE`, `VERSION_PATCH_OVERRIDE`, a pinned `0` still
  floored), `MARKETING_VERSION` emitted verbatim, and the §6 migration
  vector — a derived CalVer orders above `0.1.1`, and the post-roll `.1`
  above the pre-roll `.5`, under the per-component numeric rule. The
  comparison itself is `viewmodel::update::is_newer`, tested in Rust; the
  shell vector asserts only the derived string.
- **Build number** (`get-build-number.sh`): totality (`rev-list --count`),
  monotonic across the month roll, `--at <ref>` with an annotated tag
  peeled, the `BUILD_NUMBER` pin (verbatim, and it wins over `--at`), the
  delegation from `get-version-info.sh --build`, both usage errors, and
  fail-closed: an unresolvable ref and a directory outside any checkout exit
  1 with an **empty stdout**.
- **Mint** (`--tag`): the **output contract** — exactly three lines,
  `version=` / `tag=` (`v` + version) / `action=` ∈ {`create`, `reuse`} —
  asserted on every mint that exits 0, because `scripts/publish.sh`'s epilogue (#395)
  branches on it with a fail-closed `*` arm; a dry run creates nothing; a
  `--push` creates an annotated tag whose peeled commit is `HEAD`; the same
  commit re-run answers `reuse` and **performs no push** (the origin's tag
  advertisement is snapshotted before and after); the §4 collision replay
  (the floor slot `v2026.9.1` minted at the August commit, then the first
  September commit derives `2026.9.1`, finds it there, and is bumped to
  `v2026.9.2`, `create`); a pin is never auto-bumped (exit 1, no contract on
  stdout, nothing tagged) but reuses at its own commit and resolves
  verbatim when free (a dry run — nothing tagged); a failed remote probe
  refuses to mint blind; with no origin the probe reads local tags, as
  documented; a stray argument is exit 2. The no-push property is observed
  two ways, because a snapshot of the origin's refs cannot see an
  idempotent re-push of a tag it already has: the snapshot (fail-closed,
  and asserted to name the tag just created) *and* a `git` shim on the
  mint's `PATH` that records every invocation — the create must record
  exactly one `push`, the reuse none.
- **The release-tag assertion** (`assert-release-tag.sh`, #404), absorbed
  from the fixture that first proved it: the month-roll and ladder-bump
  passes, and every refusal — a hand-made tag on the wrong commit, a
  prior-month tag at a later commit, a local-only tag, nine malformed
  shapes, a tag after the current UTC month, an unreachable origin, no
  origin, a shallow clone, a directory outside a checkout — with every cause
  named and the pre-mint refusals asserted to land before the mint runs.

**Proven to bite** — six mutations, each run by hand and recorded on the PR
that shipped this file, the scripts restored byte-for-byte after each:
`git push` added to the mint's `reuse` arm (an idempotent re-push, which a
ref snapshot cannot see) → 2 red, both the shim's no-push assertions; the
snapshot helper pointed at a nonexistent git dir → 2 red (it names the
tag it expects, so a dead observation cannot read as "no tags"); the
ordering helper replaced by lexical `[[ > ]]` → 1 red (`2026.10.1` below
`2026.9.5`); the `create`-only push guard widened to any `--push` → 8 red
(`git tag -a` on the existing tag exits 128, and the reuse contract,
the no-`git tag` assertion and the pin-at-own-commit reuse go with it);
the `action` token renamed to `created` → red on the first mint's contract
(and the run ends as a `FAIL fixture:` line, not a `set -e` abort); the
build-number script's fail-closed `exit 1` plus its message replaced by
`echo 1` / `exit 0` → 5 red. A suite that has only ever seen the scripts
pass is indistinguishable from one that passes everything.

**Not here, on purpose.** The shallow-clone refusal of the *build scripts*
lives in `crates/buildversion` (the `Shallow` arm of its `resolve`, in Rust),
not in the shell mint, which has no such check — it is tested there (#417).
The assertion script's "output
contract was violated" branch is unreachable from a suite that runs the real
mint, for the reason its own header gives.

The consumers of the derived number are covered separately, as before:
`agent/deploy/lib_test.sh` asserts `binary_version` reads a version back out
of a real binary and fails closed without one, `scripts/build-agent.sh`
asserts each artifact's compiled-in version against the number it was named
with, and `release.yml` asserts on every leg that the tag is the mint's own
answer at that commit (#404, above).
