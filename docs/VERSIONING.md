# Versioning — Solador instance

This repo's instance of a **Versioning spec v1.0** frozen 2026-07-11. The spec
itself is kept privately; everything it requires of this repo is restated here,
so this document stands alone. When this doc and the scripts disagree, that is
drift — fix one of them in the same PR.

## Classification (§7)

**Desktop app row.** One shipping tier: the Solador binary. One declared
non-tier:

- **Rust agent** (`agent/`): **N/A** — an internal artifact hand-deployed to
  our own hosts by operator-run scripts (`agent/deploy/redeploy.sh`), never
  published to a registry or distributed externally (the spec's literal
  "hand-copied agent" N/A case). Its `agent/Cargo.toml` semver is
  crate-internal (a declared §7 internal-tools semver exception); nothing
  consumes it as a release version. Revisit at the first artifact that leaves
  our machines.

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
  rather than a fresh re-derive.
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

Single-tier repo: umbrella `v*` only, no tier tags, no tier-vs-tag change
detection (nothing to path-scope). Declared stance: **no channel tags yet** —
builds are unsigned/un-notarized and local-only until
[#15](https://github.com/Sassy-Dog/solador/issues/15) (signing and
notarization, [#306](https://github.com/Sassy-Dog/solador/issues/306); the
update feed, [#308](https://github.com/Sassy-Dog/solador/issues/308)) lands;
at first external distribution, add `mac-direct/<version>-<build>-<UTCts>` per
submission.

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

Pre-release: Solador has not yet shipped an artifact intended to leave a
developer's machine (publish builds are unsigned; "do NOT distribute
externally until #15"). Per the §9 adoption-timing rule the scheme is wired
and active now, so the first distributed build simply uses whatever CalVer
resolves at that moment. Adoption is one-way — no semver "1.0 moment" is
coming back.

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
