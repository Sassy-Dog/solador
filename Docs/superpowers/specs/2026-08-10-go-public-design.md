# Go public — fresh repo at `cpmadrid/solador`

Date: 2026-08-10
Status: proposed

## Problem

The project sits in its pre-publication home — a company-org repository with
`INTERNAL` visibility, under its former name. The 2026-07-27 cross-platform
spec set the goal as *"shipped to other developers"*, but nothing has shipped:
`gh release list` is empty, and the repository is not readable by anyone
outside the org.

(Written before the rename, so the sections below describe the old repository
and the old name. The destination is `cpmadrid/solador`; see *On the name*.)

Making it visible is not a settings toggle. The repo carries three classes of
problem that only become problems once strangers can see it or run it:

1. **A security exposure.** Two of three CI jobs run on `[self-hosted, …,
   sassy-dog]`. On a public repo, any stranger's pull request executes arbitrary
   code on those machines. This is the most reliably exploited GitHub Actions
   misconfiguration there is, and the irony is direct: the app ships a **GitHub
   Runners** panel whose purpose is watching that exact pool.
2. **A broken first run.** The shipped store seeds six `acme/*` repos and a
   `sassydog-ghr-ubu-*` container rule. A stranger's first launch opens on six
   rows of 404s against repos they cannot read.
3. **Dangling identity.** Settings links "Report an Issue" at a repo that is
   about to become an archive, and the Azure Cost help text names the storage
   account name in the UI.

An exposure scan of the working tree found **no credentials**: no Sentry DSN
(`project.yml:74` ships `SENTRY_DSN: ""`), no Azure subscription IDs, no
personal identifiers, and **zero `secrets.*` referenced anywhere in CI**.

It did find **private network topology**, which a first pass missed because the
grep output was truncated:

The literal values are deliberately not repeated here — this document is
published too — but they were a real machine name and a real tailnet address,
appearing in roughly 180 places:

- `crates/store/src/lib.rs:15` — the crate's module-level **doc example**
  constructed a `Host` from both. This is the instance that mattered most: it
  is documentation rather than test data, so it renders on docs.rs and reads
  as an invitation.
- `crates/store/src/containers.rs` — `seeded_rules()` shipped three default
  rules, **two of them pinned to that host by name**.
- Test fixtures throughout `crates/store`, `crates/viewmodel`, `crates/wire`,
  `app/src-tauri`, the Playwright suite, and `agent/`.

None of it is a credential, and a `100.64.0.0/10` CGNAT address is routable
only from inside the tailnet. It is still a map of private infrastructure
published under the author's name, and it is cheap to remove.

## Goal

A public repository at `cpmadrid/solador` that a stranger can read, build,
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
| Migration shape | Fresh repo at `cpmadrid/solador`; **archive** the old one | Fresh start was the stated constraint. Archiving (not deleting) keeps 86 closed issues and full history searchable |
| Old repo disposition | Archive, read-only, reversible | The 86 closed issues hold reasoning `CLAUDE.md` references but does not contain |
| Visibility flip timing | **After** CI moves off self-hosted, verified green | Never a window where public + self-hosted coexist |
| License | Apache-2.0 | Permissive like MIT, plus a patent grant and a §6 trademark clause protecting a named product |
| CI runners | `ubuntu-latest`, `macos-latest`, `windows-latest` | Free on public repos, including macOS — removes the two-runner pool constraint that froze the Swift jobs |
| Portfolio seed | Empty | A seeded roster is correct for one user and wrong for every other |
| Bundle ID / keychain service | **Unchanged** (`com.sassydog.*`) | Invisible to users; changing it orphans every stored credential for zero user benefit |
| Product name | **Solador** | See below |
| Rename scope | Package manifests, product strings, docs — **not** the identity layer | The `devcanopy-` prefix exists only in `Cargo.toml`; crates are imported as `wire`/`store`/`github`, so zero `.rs` files change. The identity layer is stateful and stays |
| `.claude/sassy-dog/*` | Ship it | Process, not secrets. Differentiating, and useful to contributors. Reversible |
| Signing / releases | Deferred to Phase 2 | Certificate procurement has lead time; see *Out of scope* |

### On the name

**Solador** — Spanish, from *solar* (to floor, to pave, to tile) plus the agent
suffix *-dor*: a tiler, the tradesperson who lays tiles. The cockpit is a grid
of tiles; a solador is who arranges them.

It is the only candidate examined that is unclaimed on **every** registry:
no GitHub repository of that name, and `solador`, `solador-wire`,
`solador-store` all free on crates.io, plus Homebrew and npm.

Rejected alternatives, and why they are recorded rather than forgotten — each
died of a collision that a narrow search initially hid:

| Candidate | Killed by |
|---|---|
| `DevCockpit` / `cockpit.dev` | [Cockpit](https://cockpit-project.org) is Red Hat's server admin console in the same category; `cockpit` is already the internal noun across 101 files; and it is *operator* vocabulary while the audience is developers |
| `Tessera` | Nine exact-name repos, including a 1,177★ graphite dashboard, a 288★ AI-coding-session workspace, and a 257★ Rust UI library. Bare search is swamped by Tesseract (75k★) |
| `Vane`, `Crow`, `Vigil` | 36k★ / 7.6k★ / 1.9k★ exact-name repos — and Vigil is itself an infrastructure status page |
| `Keel`, `Pennant`, `Ballast` | 2,721★ / 590★ / 469★ |
| `Telltale`, `Plumb` | Clean enough (22★ / 145★) but passed over |

The old name is retired for the reasons originally stated: the canopy metaphor
communicates nothing, and the `Dev-` prefix was filler. The prefix's job —
signalling the audience — turns out to be one no name has to do: Sentry,
Grafana, Prometheus, Tailscale, Linear, Vercel, Neon, Bun and Deno all decline
it. The README, the screenshots, and where it is posted carry that instead.

Two known costs, accepted: *Solador* is one vowel from *Salvador* and will be
misheard, and it names the **rendering** rather than the **watching**, so it
carries no meaning to a newcomer until the README supplies one.

## Sequencing

The ordering is the security control, not a convenience:

1. Create `cpmadrid/solador` **private**. Push current state as one commit —
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

- `crates/store/src/repos.rs:16-23` — seed of six `acme/*` slugs → empty.
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

### Network topology in code and docs

- `crates/store/src/lib.rs:15` — module doc example. Replace the hostname and
  address with obvious placeholders (`"workstation"`, `"100.100.100.100"`).
  Highest priority of the three: it renders as documentation.
- `crates/store/src/containers.rs:217-230` — `seeded_rules()` returns three
  rules, two `.on_host("ubu-01")`. Seed becomes empty (see above).
- `crates/store/src/lib.rs:715,945,1002`, `crates/store/src/hosts.rs:81,96,104`
  — test fixtures. Lowest priority, but change them in the same pass so the
  string is gone from the tree entirely and a future grep stays clean.

### User-facing strings and links

`app/src-tauri/src/settings.rs`:

- **L1171-1172** — "GitHub Repository" and "Report an Issue" point at
  `cpmadrid/solador`. Re-point to `cpmadrid/solador`, or users file bugs
  into an archive.
- **L1032** — Azure Cost help text reads *"the cost-exports container on
  <the account>"*. Generalize; it ships a private storage account name in the UI.
- **L39** — doc link to `cpmadrid/solador/issues/15`, which will 404.

### Rename to Solador

Cheaper than it looks. The `devcanopy-` prefix lives **only in package
manifests** — every crate declares a short `[lib] name` (`wire`, `store`,
`github`), so there are **zero `devcanopy_` paths in any `.rs` file** and no
`use` statement changes.

Renamed:

- Nine `name = "devcanopy-*"` lines in `crates/*/Cargo.toml` and
  `app/src-tauri/Cargo.toml`, plus the ~10 dependency declarations that
  reference them, plus a regenerated `Cargo.lock`.
- `app/src-tauri/tauri.conf.json:3` `productName`, and the window `title` at
  `:10`.
- `README.md`, `CLAUDE.md`, `app/README.md`, `Docs/*`, `Scripts/*` strings.

**Not** renamed, and this is the point of the split: `SERVICE`
(`crates/store/src/secrets.rs:30`), `APP_DIR_NAME`
(`crates/store/src/lib.rs:61`), the bundle identifier
(`app/src-tauri/tauri.conf.json:5`). A fourth — the `azurecost-sas` LaunchAgent
label — was on this list until Task 10 deleted the LaunchAgent outright.
Those four are **stateful** — they address a live keychain item, an on-disk
store, an installed app's macOS identity, and a running LaunchAgent. Renaming
them orphans every credential in daily use to change strings no user ever sees.

### Governance

`.github/required-checks.yml` states it is *"Rendered into this repo's branch
ruleset by acme/toolkit"* against a contract doc in a private repo. That
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
   `acme/toolkit` used to render. Required contexts: `Rust agent`,
   `Rust workspace + frontend e2e`, `Windows workspace tests`.
4. **Archive** the pre-publication repository — the private one this was
   exported from, never the repo created in step 1 — last, after the new repo
   is verified.

The local clone's remote is *not* in this list: it moves at sequencing step 1,
because pushing the fresh commit requires it.

## Out of scope

**Phase 2 — distribution.** Developer ID signing, notarization, Windows
signing, tag-triggered release automation, Homebrew cask. Deferred because
certificate procurement has lead time and it is a separable body of work. The
2026-07-27 cross-platform spec already treats signing as non-negotiable for the
shipping goal; this spec is its prerequisite, not its replacement.

**Also out of scope:** changing the `com.sassydog.*` bundle identifier,
keychain service, `APP_DIR_NAME`, or LaunchAgent label — the rename stops at
the identity layer, deliberately. A marketing website. Un-freezing the Swift
app (though free macOS CI minutes make that newly affordable).

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
- Settings → both links resolve to `cpmadrid/solador`.
- Azure Cost help text contains no storage account name.
- A fork PR from a throwaway account triggers CI and touches no self-hosted
  runner.
