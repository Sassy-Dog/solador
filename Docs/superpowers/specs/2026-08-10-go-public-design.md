# Go public — fresh repo at `cpmadrid/devcanopy`

Date: 2026-08-10
Status: proposed

## Problem

DevCanopy lives at `Sassy-Dog/devcanopy` with `INTERNAL` visibility. The
2026-07-27 cross-platform spec set the goal as *"shipped to other developers"*,
but nothing has shipped: `gh release list` is empty, and the repo is not
readable by anyone outside the org.

Making it visible is not a settings toggle. The repo carries three classes of
problem that only become problems once strangers can see it or run it:

1. **A security exposure.** Two of three CI jobs run on `[self-hosted, …,
   sassy-dog]`. On a public repo, any stranger's pull request executes arbitrary
   code on those machines. This is the most reliably exploited GitHub Actions
   misconfiguration there is, and the irony is direct: the app ships a **GitHub
   Runners** panel whose purpose is watching that exact pool.
2. **A broken first run.** The shipped store seeds six `Sassy-Dog/*` repos and a
   `sassydog-ghr-ubu-*` container rule. A stranger's first launch opens on six
   rows of 404s against repos they cannot read.
3. **Dangling identity.** Settings links "Report an Issue" at a repo that is
   about to become an archive, and the Azure Cost help text names the storage
   account `stsassydog` in the UI.

An exposure scan of the working tree found **no secrets**: no Sentry DSN
(`project.yml:74` ships `SENTRY_DSN: ""`), no Azure subscription IDs or storage
account names in code, no Tailscale addresses, no personal identifiers, and
**zero `secrets.*` referenced anywhere in CI**. The blockers above are
configuration and product defaults, not leaks.

## Goal

A public repository at `cpmadrid/devcanopy` that a stranger can read, build,
run without hitting Sassy-Dog-specific defaults, and file a well-routed issue
against — with the self-hosted runner pool no longer reachable from it.

### What "public" is for

Stated once, because several decisions below only make sense against it:

| | |
|---|---|
| Motivation | Real users — installs, bug reports, possibly contributions |
| Home | `cpmadrid` (personal), not the `Sassy-Dog` org |
| History | Fresh start. Current state as one commit |
| Delivered by this spec | *Publishable*, not *installable* — see *Out of scope* |

The last row is the honest limit. Without signed binaries, macOS Gatekeeper
hard-blocks the app for anyone who did not build it, so Phase 1's practical
audience is developers willing to run `cargo build`. Phase 2 is what makes
"real users" real.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Migration shape | Fresh repo at `cpmadrid/devcanopy`; **archive** the old one | Fresh start was the stated constraint. Archiving (not deleting) keeps 86 closed issues and full history searchable |
| Old repo disposition | Archive, read-only, reversible | The 86 closed issues hold reasoning `CLAUDE.md` references but does not contain |
| Visibility flip timing | **After** CI moves off self-hosted, verified green | Never a window where public + self-hosted coexist |
| License | Apache-2.0 | Permissive like MIT, plus a patent grant and a §6 trademark clause protecting a named product |
| CI runners | `ubuntu-latest`, `macos-latest`, `windows-latest` | Free on public repos, including macOS — removes the two-runner pool constraint that froze the Swift jobs |
| Portfolio seed | Empty | A seeded roster is correct for one user and wrong for every other |
| Bundle ID / keychain service | **Unchanged** (`com.sassydog.*`) | Invisible to users; changing it orphans every stored credential for zero user benefit |
| Product name | **Unchanged** (DevCanopy) | With no website there is no domain to buy, so the name only has to be a good repo name. `devcanopy` is unclaimed and unambiguous |
| `.claude/sassy-dog/*` | Ship it | Process, not secrets. Differentiating, and useful to contributors. Reversible |
| Signing / releases | Deferred to Phase 2 | Certificate procurement has lead time; see *Out of scope* |

### On the name

`DevCockpit` / `cockpit.dev` was considered and rejected on three grounds:
[Cockpit](https://cockpit-project.org) is Red Hat's server admin console in the
same product category; `cockpit` is already the load-bearing internal noun
across 101 files (`CockpitView`, `crates/viewmodel/src/cockpit.rs`,
`cockpit_layout`); and "cockpit" is operator vocabulary, while the stated
audience is developers. DevCanopy's metaphor does not land, but the name is
unclaimed — a rarer property, and the one a rename would spend.

## Sequencing

The ordering is the security control, not a convenience:

1. Create `cpmadrid/devcanopy` **private**. Push current state as one commit —
   a fresh `git init` in a clean export of the working tree, not an orphan
   branch pushed from the existing clone, so no object from the old history can
   ride along. The local working clone re-points at the new remote here, at
   step 1 — not at the end.
2. Land all of *Code changes* below.
3. Verify all three CI jobs green on hosted runners, still private.
4. Add the public surface (license, README, templates).
5. Re-run the exposure scan.
6. **Flip to public.**
7. Migrate the board, re-point workflow config, archive the old repo.

Step 3 gates step 6. At no point does a public repo reference a self-hosted
runner.

## Code changes

### CI off the self-hosted fleet

`.github/workflows/ci.yml` — two lines:

| Line | From | To |
|---|---|---|
| 29 | `runs-on: [self-hosted, linux, sassy-dog]` | `runs-on: ubuntu-latest` |
| 64 | `runs-on: [self-hosted, macOS, sassy-dog]` | `runs-on: macos-latest` |

Neither job depends on the fleet. `agent-tests` is `cargo fmt`/`clippy`/`build`/
`test` inside `agent/`, and its own comment (L26-28) records that the toolchain
installs into `$HOME` with no root. `rust-workspace` needs macOS only for the
Tauri target and a WebView-capable desktop; `macos-latest` provides both, and
Playwright installs its own chromium. No podman, tart, or Tailscale dependency
exists in either job.

Fork pull requests are already safe: `permissions: contents: read` (L14-15) and
no `secrets.*` anywhere in the file.

Stale comments at L26-28 and L60-64 describe fleet portability and must be
rewritten. The caching rationale at L135-138 ("GitHub-hosted runners are
ephemeral… so caching matters more here") now applies to all three jobs.

### First-run defaults

The shipped app is Tauri. `DevCanopy/Services/GitHub/PortfolioRepos.swift:12-21`
is the frozen Swift mirror; the live path is:

- `crates/store/src/repos.rs:16-23` — seed of six `Sassy-Dog/*` slugs → empty.
- `crates/store/src/repos.rs:8` — `ORG` const, removed with the seed.
- `crates/store/src/repos.rs:77` — `seed_matches_the_swift_portfolio()` asserts
  `len() == 6`. Rewrite and rename: it treats the frozen Swift app as the
  contract, which is inverted now.
- `crates/store/src/containers.rs:224` — container group rule seeded with
  `sassydog-ghr-ubu-*`. Same defect, different panel: the seed becomes **empty**,
  not a generic example pattern. A shipped example rule silently groups a
  stranger's containers by a rule they did not write, which is harder to
  diagnose than no grouping at all.
- `PortfolioRepos.swift:12-21` — mirror the change so the two do not diverge in
  the reference sources, even though Swift does not ship.

**To verify, not assume:** with an empty portfolio, the Repos panel must render
a setup instruction rather than a blank or error card. `panel::Configured`
covers absent *credentials*; an empty *list* may be a different path. If it is
blank, fix it — an empty green panel is the failure mode this codebase already
rejects elsewhere (see the Sentry Crons blind-read rule).

### User-facing strings and links

`app/src-tauri/src/settings.rs`:

- **L1171-1172** — "GitHub Repository" and "Report an Issue" point at
  `Sassy-Dog/devcanopy`. Re-point to `cpmadrid/devcanopy`, or users file bugs
  into an archive.
- **L1032** — Azure Cost help text reads *"the cost-exports container on
  stsassydog"*. Generalize; it ships a private storage account name in the UI.
- **L39** — doc link to `Sassy-Dog/devcanopy/issues/15`, which will 404.

### Governance

`.github/required-checks.yml` states it is *"Rendered into this repo's branch
ruleset by Sassy-Dog/platform"* against a contract doc in a private repo. That
automation does not exist at `cpmadrid`. Correct the comment to say the ruleset
is hand-maintained, or remove the file.

`Scripts/publish.sh:113` resolves `SENTRY_DSN` from Doppler. `--skip-sentry`
already exists, so contributors can build; the failure message should say the
Doppler path is maintainer-only.

## Public surface

| File | Content |
|---|---|
| `LICENSE` | Apache-2.0, full text |
| `NOTICE` | Copyright attribution, per Apache-2.0 convention |
| `README.md` | Rewritten — see below |
| `SECURITY.md` | Disclosure path and expected response window |
| `CONTRIBUTING.md` | `./dev test`, `./dev lint`, `./Scripts/install-hooks.sh`, **and that Swift is frozen** |
| `.github/ISSUE_TEMPLATE/` | Bug report (platform + panel required), feature request |

### README

The current README opens *"A native macOS cockpit"* — accurate for the frozen
app, wrong for the shipped one. It needs:

- **Cross-platform framing** in the first line.
- **Screenshots.** `Docs/assets/` holds the icon and logo variants; the README
  embeds no images. This product is a visual grid of panels — screenshots are
  the pitch, prose is not.
- **Who this is for**, stated plainly. It assumes Tailscale, GitHub Actions, and
  a specific vendor stack. Saying so prevents a stream of "doesn't work for me"
  issues from bad-fit users.
- **The two-app explanation.** A contributor cloning 370 files finds a complete
  SwiftUI app that is frozen and absent from CI. Unexplained, that is their
  first confusing hour.
- **Build from source**, since there are no releases.
- **No telemetry**, which is verifiable rather than aspirational: the Tauri app
  carries no crash-reporting SDK. Every `sentry` reference in `main.rs` is the
  Usage panel reading Sentry's REST API with the user's own token;
  `SentrySetup.swift` is Swift-only and frozen.

### Repo settings

Issues enabled; topics set for discoverability; `delete_branch_on_merge=true`
set **explicitly** — per the org's own GitHub conventions it silently defaults
to `false` on a new repo.

## Process migration

1. **Board** — create a user-level project under `cpmadrid` with the same five
   columns (Backlog → Ready → In progress → In review → Done). Carry the 4 open
   issues. The 86 closed ones stay with the archive.
2. **Workflow config** — re-point the five `.claude/sassy-dog/*.md` files,
   `.claude/settings.json`, and `.claude/hooks/sassydog-post-edit.sh` at the new
   owner, repo, and project number.
3. **Branch protection** — hand-build the ruleset and merge queue that
   `Sassy-Dog/platform` used to render. Required contexts: `Rust agent`,
   `Rust workspace + frontend e2e`, `Windows workspace tests`.
4. **Archive** `Sassy-Dog/devcanopy` — last, after the new repo is verified.

The local clone's remote is *not* in this list: it moves at sequencing step 1,
because pushing the fresh commit requires it.

## Out of scope

**Phase 2 — distribution.** Developer ID signing, notarization, Windows
signing, tag-triggered release automation, Homebrew cask. Deferred because
certificate procurement has lead time and it is a separable body of work. The
2026-07-27 cross-platform spec already treats signing as non-negotiable for the
shipping goal; this spec is its prerequisite, not its replacement.

**Also out of scope:** renaming the product, changing the
`com.sassydog.*` bundle identifier or keychain service, a marketing website,
and un-freezing the Swift app (though free macOS CI minutes make that newly
affordable).

## Risks

| Risk | Mitigation |
|---|---|
| Public repo briefly runs self-hosted CI | Sequencing: private → fix → verify → flip. Step 3 gates step 6 |
| `macos-latest` behaves differently from the self-hosted Mac | Verified green while private, before any visibility change |
| Testing first-run destroys the author's working setup | Test against a **temporary data directory**. Never wipe the real `store.json` or keychain — it holds every credential in daily use |
| Losing the 86 closed issues | Archive rather than delete; they stay searchable |
| Bad-fit users filing noise | README states assumptions and audience up front |
| Agent's bearer-token model attracts scrutiny | `SECURITY.md` gives researchers a path that is not a public drop |

## Testing

- All three CI jobs green on hosted runners, **while the repo is still
  private**.
- Exposure scan re-run immediately before the visibility flip.
- First-run smoke against a temporary data directory: empty portfolio renders a
  setup instruction, no 404 rows, no `sassydog-ghr-ubu-*` rule present.
- Settings → both links resolve to `cpmadrid/devcanopy`.
- Azure Cost help text contains no storage account name.
- A fork PR from a throwaway account triggers CI and touches no self-hosted
  runner.
