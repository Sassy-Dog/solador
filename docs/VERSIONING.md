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

- **No build consumer yet.** The Xcode build that injected these as
  `MARKETING_VERSION` / `CURRENT_PROJECT_VERSION` went with the SwiftUI app.
  `app/src-tauri/tauri.conf.json` still carries a hand-written `version: 0.1.0`,
  which contradicts this document and is inert only while `bundle.active` is
  `false`. Wiring the minted version into the Tauri bundle is part of **#15**;
  until then nothing consumes the mint except `scripts/publish.sh`, which
  refuses to run.
- `scripts/publish.sh` consumes the mint's output contract (below) and pins
  the build via `MARKETING_VERSION=<minted>` so the artifact is stamped with
  exactly the tagged version.
- In-app displays (Settings footer, Sentry release, OpenClaw client version)
  read `CFBundleShortVersionString` from the stamped bundle — downstream of
  the scripts, compliant.

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
[devcanopy#15](https://github.com/cpmadrid/solador/issues/15) (signing /
notarization / Sparkle) lands; at first external distribution, add
`mac-direct/<version>-<build>-<UTCts>` per submission, and map Sparkle keys
per the §7 macOS row (`sparkle:version` = **build number**,
`sparkle:shortVersionString` = marketing version, appcast generated — never
hand-edited).

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
> `DevCanopyTests/VersioningScriptTests.swift`, which ran hermetic bare-origin
> git fixtures (real `ls-remote` probes) over: patch floor, month-roll reset,
> §2 idempotency, the §4 collision replay (prior-month-commit release →
> first-commit-of-month release → two distinct versions), same-commit mint
> reuse, pin-never-auto-bumps, probe fail-closed, build-number totality /
> `--at <ref>` / pin / fail-closed, and the §6 CalVer-exceeds-`v0.1.1`
> monotonicity vector.
>
> That file was deleted with the SwiftUI app. The scripts it covered survive
> because **#15** needs them, so the coverage has to be rebuilt — as a shell
> or Rust integration test — before the minting logic is trusted to stamp a
> real release. Restoring it is part of #15's scope.
