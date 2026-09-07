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
3. Tests (`scripts/test.sh`, skippable with `--skip-tests`).
4. **Mint**: probe `git ls-remote --tags origin` (remote-visible, never
   locally-cached tags; annotated tags peeled via `^{}`), then the ladder —
   tag exists at HEAD → **reuse** (idempotent re-run); exists elsewhere →
   **bump** patch until free (the bumped version IS the version); free →
   **create + push** annotated `vYYYY.M.P`. Probe failure → fail closed,
   never mint blind. Output contract: one `(version, tag, action)` triple.
5. Release build stamped from the minted version (tag lands before the build
   on purpose: a failed build re-runs into the same-commit reuse branch).

## Tags (§5)

Umbrella `v*` only, no tier tags, no tier-vs-tag change detection. The two
shipping tiers deliberately share **one** number and one tag, so there is
nothing to path-scope: a tag push builds the cockpit and the agent from the same
commit into the same release.

The **accepted cost** of that sharing, stated rather than discovered: the agent
gets a new version on every cockpit-only release, including ones where not a
byte of `agent/` changed. What stops that becoming a pointless download and
restart on every host is the update feed's **content hash**, not the version —
same bytes, stop (`docs/AGENT-DISTRIBUTION.md` §3).

Declared stance: **no channel tags yet.** This clause used to read "builds are
unsigned/un-notarized and local-only until #15 lands"; that is no longer the
reason, because [#15](https://github.com/Sassy-Dog/solador/issues/15) has
landed and external distribution has happened. The reason now is that nothing
consumes a per-submission channel tag: distribution is direct download plus the
`latest.json` feed, and no App Store submission exists to date. Add
`mac-direct/<version>-<build>-<UTCts>` at the first submission that needs one.

### Mapping onto the update feed

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

The mint is local-only today; `ci.yml` computes no versions, so it needs no
special checkout. **If the mint (or any version computation) ever moves into
a workflow**: that job MUST check out with `fetch-depth: 0` **and** fetch
tags (two distinct requirements — tags present, and the §4 probe actually
performed), keep UTC dates, and remain the single mint site (a CI release
action consumes the minted tag, never `tag_name:`-creates its own).

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

> **These scripts currently have NO test coverage.** Their only tests were
> `VersioningScriptTests`, which ran hermetic bare-origin
> git fixtures (real `ls-remote` probes) over: patch floor, month-roll reset,
> §2 idempotency, the §4 collision replay (prior-month-commit release →
> first-commit-of-month release → two distinct versions), same-commit mint
> reuse, pin-never-auto-bumps, probe fail-closed, build-number totality /
> `--at <ref>` / pin / fail-closed, and the §6 CalVer-exceeds-`v0.1.1`
> monotonicity vector.
>
> That file was deleted with the original macOS app. The scripts it covered survive
> because **#15** needs them, so the coverage has to be rebuilt — as a shell
> or Rust integration test — before the minting logic is trusted to stamp a
> real release. Restoring it is part of #15's scope.
