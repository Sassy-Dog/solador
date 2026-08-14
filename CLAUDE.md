# Solador — AI Assistant Instructions

This file provides context for Claude Code when working with the Solador codebase.

## Project Overview

Solador is a cross-platform desktop cockpit that watches development
infrastructure at a glance, rendered as a grid of panels. It is a Rust
workspace (`crates/*`) plus a Tauri v2 app (`app/`), macOS/Windows-portable.

It has three parts:
- **App** (`app/`) — Tauri v2 shell plus a bundler-free HTML/CSS/JS frontend.
- **Crates** (`crates/*`) — all the logic: polling, view models, storage,
  vendor clients. The shell is thin on purpose; panels are testable without a UI.
- **Agent** (`agent/`) — Rust (axum) HTTP service exposing host metrics +
  container list as JSON behind a bearer token, reached over Tailscale. It is a
  member of the root workspace and has its own CI job (`agent-tests`, on Linux).
  See `agent/README.md`.

`app/README.md` is the reference doc for the app itself and is far more
detailed than this file.

> **History:** Solador began as a the original macOS app. The Tauri app reached
> panel parity with it, and the original macOS app was deleted.
>
> Roughly 380 comments across the Rust and JS sources — plus ~23 test names
> like `defaults_match_the_original_app` — still cite the originals they
> were ported from. **Those files are not in this repo.** Treat the references
> as provenance: they record which decision was ported, and, more usefully,
> where this app deliberately diverged. The assertions themselves still guard
> real values; only the names point at something gone.

### The panels

- **Hosts** — this machine's card plus one per configured agent, in a
  width-aware grid (1s poll).
- **Containers/VMs** — local docker/podman/tart plus every host's agent, with
  grouping rules and presence memory (10s).
- **GitHub Repos + GitHub Runners** — per-repo CI health and counts, local
  branch/worktree counts, self-hosted runners with the absence roster
  (the store's `refresh_interval_secs`).
- **Usage** — Claude token rollups (same interval); Neon, Sentry + Vercel
  (hourly). Vercel reads the FOCUS billing export: month-to-date spend and what
  falls beyond the plan. Neon renders compute/storage MTD, `NEON EST. CHARGES
  (MTD)` from operator-entered rates, and a best-effort `NEON LAST INVOICE` off
  an undocumented endpoint — that one failing degrades to `—` plus a footer,
  never the consumption rows.
- **Azure Cost** — the daily cost export, on its own 4h cadence.
- **Sentry Crons** — every cron monitor environment that is not `ok`, and **how
  long it has been broken** (fixed 1h cadence). The age comes from
  `activeIncident.startingTimestamp`, never from `lastCheckIn` — measuring from
  the last attempt is what made day 6 of an outage look identical to day 1, and
  on the monitor that motivated the panel the two read 7d 22h and 0d 22h. A
  check-in-derived age is a *different* enum variant and renders as the weaker
  claim it is (`≈`, amber, and a row that says why). Suppressed entries
  (`disabled`, or a strict `isMuted: true`, at either monitor or environment
  level) are counted and shown with their reason, never dropped; and a **blind
  read** — no monitors at all, or a monitor carrying no environments — is red,
  because an empty green panel is the failure this exists to remove.
- **Services** — third-party availability for the five vendors this stack
  depends on (GitHub, Anthropic, Vercel, Neon, Azure), read on the GitHub poll's
  cadence. Three transports behind one vocabulary (`crates/servicestatus`), and
  a change in either direction fires a desktop notification.
- **OpenClaw** — an agent farm over a live WebSocket. **Event-driven, on no
  cadence at all**, which is why it is the one panel with no staleness footer.

Rows are reflowed by `viewmodel::cockpit` for the measured width, and every card
in a row is the same height. An in-app Settings surface over `crates/store`
(hosts CRUD, portfolio, credentials, container group rules, cockpit layout,
general prefs) applies changes without a restart.

The Tauri IPC boundary itself is **not** automatically tested — `app/README.md`
carries a five-minute manual smoke checklist that is the only thing covering it
(see #123).

## Development Workflow

### Quick Commands
- `./dev` — Build and run (debug)
- `./dev run --release` — Run release build
- `./dev build` — Compile the cockpit binary (plain cargo, no bundle).
  `./dev build --bundle` and `./dev build --release` (= `./prd`) go through
  `cargo tauri build` and produce `Solador.app`; see **Releasing** below
- `./dev test` — Root Rust workspace (`cargo test --locked --workspace` —
  `crates/*`, `app/src-tauri`), `agent/deploy/lib_test.sh`, plus the
  `tests/frontend` Playwright e2e suite
- `./dev lint` — `cargo fmt --check` + `cargo clippy`, plus `bash -n` and
  `shellcheck -S warning` over `agent/deploy/*.sh`; mirrors CI
- `./dev format` — `cargo fmt`
- `./dev clean` — Clean build artifacts
- `./dev publish` — **Not implemented.** Refuses before minting a tag; see #15.

> `./dev lint` is the local mirror of CI's Rust lint gates, which live inside the
> `rust-workspace` and `agent-tests` jobs. `./scripts/install-hooks.sh`
> (one-time) wires it to a pre-push hook so lint failures never reach CI.
>
> `agent/` is a workspace member, so `./dev test` and `./dev lint` cover it.
> Its CI job (`agent-tests`) runs on Linux and is scoped `-p solador-agent`:
> a bare `cargo build` there would resolve the whole workspace, including
> `app/src-tauri`, which needs webkit2gtk libraries the runner does not have.

### Releasing

There is a **bundle** but still no release path.

`./dev build --release` produces `target/release/bundle/macos/Solador.app`
through `cargo tauri build` (#303). The bundler is not in `tauri-build` — that
build.rs helper reads `bundle.*` and assembles nothing — so it comes from
`tauri-cli`, pinned by `TAURI_CLI_VERSION` in `scripts/config.sh` and installed
on demand by `build.sh`. Both version numbers are **derived from git**:
`tauri.conf.json` authors no `version` at all, the CalVer arrives as a
`--config` overlay and lands as `CFBundleShortVersionString`, the build number
is stamped over `CFBundleVersion`, and the build asserts all of it back out of
the Info.plist rather than trusting that it worked. CI runs the same path at
the debug profile on every PR (`./dev build --bundle`, job **macOS bundle
(unsigned)**), so bundling cannot break unnoticed the way the agent deploy did
in #269.

The bundle is **unsigned and unnotarized**, so Gatekeeper refuses it anywhere
it was not built. Signing is **#306**, updates are **#308**, and the release
train is **#15**, which is still gating. `scripts/publish.sh` keeps the CalVer
minting and CI-verification pre-flight as the scaffold #15 will complete, and
refuses *before* minting a tag.

Under `cargo tauri build`, `.cargo/config.toml`'s `MACOSX_DEPLOYMENT_TARGET` is
**shadowed**: the CLI exports the floor from `bundle.macOS.minimumSystemVersion`
into cargo's environment, and `[env]` yields to an inherited value. Both
declare 14.0; keep them equal, and do not assume editing the cargo config moves
the bundle's floor.

### Project Structure
```
├── dev                     # Development entry point
├── prd                     # ./dev build --release
├── scripts/                # Build and development scripts
│   ├── lib.sh              # Common functions
│   ├── config.sh           # App configuration (deliberately small)
│   └── *.sh                # Implementation scripts
├── Cargo.toml              # Root Rust workspace: crates/* + app/src-tauri
├── rust-toolchain.toml     # Pins the root workspace's toolchain
├── crates/
│   ├── wire/               # Wire-format types shared with the agent's JSON
│   │                       #   contract (package `solador-wire`, imported as `wire`)
│   ├── viewmodel/          # every string/colour the frontend paints
│   ├── agentclient/        # HTTP client polling the agent
│   ├── store/              # settings/hosts/repos/container-rules/runner-roster/
│   │                       #   cockpit-layout JSON + OS credential-store wrappers
│   ├── github/             # GitHub REST client + the "is it us?" verdict
│   ├── servicestatus/      # Atlassian Statuspage, status.io, Azure RSS
│   ├── localhost/          # this machine's metrics (sysinfo); every field the
│   │                       #   platform can decline is an Option, never a 0
│   ├── usage/              # Claude Code log rollups + Neon + Sentry + Vercel
│   ├── azurecost/          # Azure Cost Management export reader (SAS blob + CSV)
│   └── openclaw/           # OpenClaw gateway client: WS protocol v3, Ed25519
│                           #   device identity, the frame→snapshot reducer
├── app/
│   ├── src-tauri/          # Tauri v2 shell: one poll task per host plus this
│   │                       #   machine (src/local.rs); `cockpit`, `containers`,
│   │                       #   `repos`/`runners`, `usage`, `azure_cost`,
│   │                       #   `crons`, `openclaw` + `settings_*` (src/settings.rs)
│   └── ui/                 # Frontend: plain HTML/CSS/JS, no bundler
├── agent/                  # Per-host metrics agent (workspace member, Linux CI)
├── tests/fixtures/           # Wire-contract fixtures shared by agent/ + crates/
├── tests/frontend/         # Playwright e2e suite for app/ui/
├── brand/                  # Brand assets
└── docs/                   # Versioning, secrets, PRD
```

## Technology Stack
- **Language**: Rust (Tauri v2, tokio, axum for the agent)
- **Frontend**: plain HTML/CSS/JS, no bundler
- **Credential storage**: OS credential store (macOS Keychain, Windows
  Credential Manager)
- **Transport**: HTTP/JSON over Tailscale, guarded by a bearer token

## Key Implementation Notes

### Hosts & metrics
- Remote hosts run the Rust agent (`agent/`), polled over Tailscale.
- Agent endpoints: `GET /v1/snapshot` (CPU/mem/disk/net/gpu/battery),
  `GET /v1/containers`, `GET /v1/health`. All require `Authorization: Bearer <token>`.
- **Unknown is representable.** Every metric a producer may not be able to
  measure (memory used/swap/pressure, thermal state, the GPU fields,
  disk/network rates) is an `Option` in `crates/wire`: an absent key decodes to
  `None`, `None` re-encodes as an *omitted* key, and `0` means measured zero.
  Unmeasured samples never enter a history buffer. Known limit: agents
  predating #183 send literal zeros, which decode as measurements — only the
  agent can fix that.
- **Local collection is unknown-first too**, not just the wire: `crates/localhost`
  returns `None` for a reading the kernel declined, and logs the failure on the
  *transition* rather than once per 1 Hz poll. Capacity figures come from
  infallible sources and stay non-Optional.
- **Configuration is unknown-first as well**: `panel::Configured` is
  `Unknown` / `Absent` / `Present`, defaulting to `Unknown`, and `Present` is
  recorded **when the credential is read, before the request**. Every panel used
  to store this as a `bool` a completed fetch set, so the first frame — before
  any pass had looked — was indistinguishable from "there is no credential":
  Repos and Runners opened on `connect a GitHub token in Settings`, Azure on
  `Add an Azure storage account in Settings`, Containers on `no containers
  detected`, on a machine where all of it was fine. Only `Absent` may paint a
  setup instruction; `Unknown` renders the panel's loading line. **A defaulted
  state is as much a fabrication as a defaulted number.**
- **`status_footer`'s `last_updated` is the last success, not the last attempt.**
  It renders `last ok {age}`, which is a promise only the caller can keep.
  Containers and Usage passed "when we last looked", so a Docker daemon that had
  never once answered reported `⚠ couldn't read docker · last ok 0s ago` — on
  every poll, forever. A panel needing both clocks keeps them as separate fields
  (`local_last_updated` vs `local_last_success`).

### Containers
- **No seeded grouping rules.** A store that has never configured any starts
  with none, deliberately: the rules that used to ship named one operator's
  machines, and a shipped example rule silently groups a stranger's containers
  by a rule they never wrote — harder to diagnose than no grouping at all,
  because the panel looks like it is working while hiding or folding rows.
- **Order is the contract**: matching is first-match-wins, so a hide rule above
  a collapse rule changes what the panel shows.

### CI & usage data
- GitHub Actions data: `crates/github` (workflow health, self-hosted runners,
  remote branch / open-issue / open-PR counts).
- Claude Code usage rollups: `crates/usage` (tokens only — USD is computed and
  unit-tested but never displayed, since the account is subscription-based).
- Neon, Sentry, Vercel consumption: `crates/usage`. Own fixed 1h cadence, not
  the shared refresh interval; render `—` when the key is missing or the API fails.
- Sentry cron monitors: `crates/usage/src/sentry.rs`'s `cron_monitors()` /
  `summarize_monitors()` plus `app/src-tauri/src/crons.rs`. Three wire traps are
  documented there and each has a test: there is **no** flat `projectSlug` (it is
  `project.slug`), **no** `hasMoreEnvironments` and no environment-truncation
  signal at all, and `activeIncident` is a key on every environment that holds
  `null` on the healthy ones. Build fixtures from the raw REST payload — the
  Sentry MCP server normalises the response and synthesises the first two, so
  fixtures derived from it are self-consistent and wrong.

### Authentication
- **Repos / GitHub Runners**: a fine-grained PAT with read-only access to
  **Actions**, **Contents**, **Issues**, and **Pull requests**.
- **Remote hosts**: per-host bearer token.
- **Usage → Claude**: no credential — and **no account either**. The rollups
  are a walk of `~/.claude/projects`, and those logs record what was consumed,
  never who paid for it: a full key survey of a real session file — 50+
  top-level fields including `cwd`, `gitBranch`, `sessionId`, `version`,
  `userType`, `requestId`, `messageId` — carries no `account`, `organization`,
  `email`, `plan`, `tier` or API-key field of any kind. So the panel's numbers
  are **one machine-local aggregate**, and two subscriptions used on one machine
  are *inherently* unseparable from this source — impossible, not
  unimplemented; per-subscription attribution needs a source that knows who
  paid. Anthropic **API keys** are `VendorAccount`s when that integration lands
  (#283) and their usage *is* attributable. **A subscription is not an account
  and must not be modelled as one**: an account id on these rollups would be an
  attribution invented to fill a gap — the unknown-as-zero error in another
  costume. If a number cannot be attributed, say so rather than attributing it.
- **Usage → Neon**: an *organization* API key. The non-secret `org_id` is a
  normal preference, not a credential-store item.
- **Usage → Sentry** and **Sentry Crons**: the *same* read-only `org:read`
  token and org slug — one credential, two panels, so saving or clearing it
  wakes both poll loops or one keeps describing the previous token for an hour.
- All credentials live in the OS credential store (`crates/store::secrets`),
  never in `store.json`. On macOS they are consolidated into one item,
  `secrets_v1`, under service `app.solador.desktop`; see `app/README.md`'s
  "Consolidated credential item".
- **`LEGACY_SERVICE` / `LEGACY_APP_DIR_NAME` must not be renamed.** They name
  what is already sitting in users' keychains and config dirs from before the
  rename, and are what the migration reads *from*. Changing them orphans every
  stored credential at once — and orphaned is worse than deleted, because they
  stay there being useless.
- The Azure Cost panel stores no credential: it mints a short-lived,
  container-scoped SAS per poll by shelling out to the Azure CLI (`az`, signed
  in as the operator).

### Responsive layout (breakpoints)
- `crates/viewmodel/src/cockpit.rs` holds the responsive math. Pure values, no UI.
- The model is CSS `repeat(auto-fit, minmax(<min>, 1fr))`, **not** global
  `sm/md/lg` tiers: every panel declares its own `PanelKind::min_width`, and
  `reflow()` splits a row only when *its* panels stop fitting.
- **Spans, not even splits**: each placement carries a `PanelSpan` —
  `Full` / `ThreeQuarters` / `Half` / `Quarter`, weights 4/3/2/1 — and
  `panel_widths()` gives each panel its share of **one four-quarter grid**. The
  frontend paints the identical construction, `repeat(4, minmax(0,1fr))` plus
  `grid-column: <start> / span k`, so the two cannot drift.
- **The grid is the same on every row, so a span means one thing.** The
  denominator is a fixed four, *not* the weight of the panels in this row —
  which looks equivalent and is not: dividing by the row's own weight makes the
  subtracted gutter total depend on how many panels happen to share the row. A
  Half came out half a gutter narrower beside two Quarters than beside one Half.
- **Every rendered row is exactly four quarters.** When reflow cuts a row short
  its remaining panels are *widened* to fill it (`fill_row`). The fill is the
  distribution closest to the authored proportions by least squared error, and
  every candidate is checked against `min_width` at its *final* width. A row
  with no legible filling is refused, and that refusal is what reflow reads as
  "this panel does not fit here".
- **The arrangement is the user's, per width band**: Settings → **Layout** edits
  a list of *breakpoints*, each an ordered list of `{panel, span}` slots plus its
  own host-overflow mode, persisted as `store.json`'s `layout`.
  `settings::normalized_order` drops unknown or duplicate slots and appends
  missing panels from `DEFAULT_ORDER`, so a stored layout always renders every
  panel exactly once — including one added by a later build. `layout: null`
  means "never configured".
- Unknown width (0) means "not in a cockpit", and every fallback picks the
  layout that can't be unreadable. Assuming wide is what let a dead measurement
  pass for a deliberate layout.
- The per-core CPU grid picks its columns from `core_column_ladder`, which obeys
  the same two invariants `core_columns` documents: divide the core count
  evenly, stay at or below `CORE_MAX_COLUMNS`, and leave **at least two rows**.
  Until the one-row fix it offered every divisor, including the count itself,
  rendering a 10-core M1 Max as one row of ten stretched cells while
  `core_columns` was answering 5 × 2 and passing all of its own tests.

### UI Design
- Dark mode optimized; glanceable grid of cockpit panels.
- Status indicated by color: green (good), orange (warning), red (error).
- Designed for persistent full-screen display on a second monitor.

## Testing

```bash
./dev test
```

Runs `cargo test --locked --workspace` (`crates/*`, `app/src-tauri`),
`agent/deploy/lib_test.sh`, and the `tests/frontend` Playwright suite. Agent
tests run via `cargo test` in `agent/`.

## Versioning (`docs/VERSIONING.md`)

Two decoupled numbers, both derived from git — never hand-maintained:
- **Marketing version**: CalVer `YYYY.M.<commits-this-month>` (UTC, non-padded
  month, floored at 1) — `scripts/get-version-info.sh --version`.
- **Build number**: total commit count, monotonic forever —
  `scripts/get-build-number.sh [--at <ref>]`.

Never compute a version anywhere else. `app/src-tauri/tauri.conf.json` no
longer authors a `version` key at all (#303) — the bundle's two plist numbers
come from those two scripts and are asserted back out of the artifact.
`app/src-tauri/Cargo.toml`'s `0.1.0` is unpublished package metadata, like the
nine sibling crates', and is still what the in-app About string reads through
`settings::VERSION`; wiring that display to the derived version is not done.

## Common Tasks

### Adding a New Cockpit Panel
1. Add the data source as a crate under `crates/` (or a module in an existing one).
2. Add a `PanelKind` case — **including its `min_width`** — in
   `crates/viewmodel/src/cockpit.rs`. The layout tests fail if you skip it.
3. Add the Tauri command in `app/src-tauri/src/`.
4. Add the frontend renderer in `app/ui/`.

### Working on the Agent
- Rust source in `agent/src/` (`server.rs`, `metrics.rs`, `containers.rs`).
- Deploy via `agent/deploy`; see `agent/README.md` for endpoints and rollout.
- **`agent/deploy/` is gated too, as of #269**: `agent/deploy/lib_test.sh`
  (dependency-free bash, stubs cargo/curl/sleep) plus `shellcheck`/`bash -n`,
  all three in the `agent-tests` job. Before that the deploy path was the one
  place where "all green" carried no information — #268 broke every deploy and
  nothing could have caught it. The load-bearing assertion is that
  `build_release_binary` *fails* when the workspace target dir is empty: a
  lenient fallback finds the stale pre-#264 binary in `agent/target/release/`
  and deploys it, reporting success.

## Debugging

- The frontend is plain JS — the Tauri webview's devtools work normally.
- `crates/*` are ordinary Rust libraries; prefer a unit test over launching the app.

## Important Conventions

1. Business logic in `crates/`, not in the Tauri shell or the frontend.
2. Keep the frontend thin — it paints what `viewmodel` hands it.
3. Handle errors gracefully with user-friendly messages; `user_message()` on
   error types is the repo-wide convention.
4. Never fabricate a value to fill a gap — render `—` and say why.

## Security Considerations

- Never log credentials or tokens (the agent must not echo its bearer token).
- Use the OS credential store for all sensitive data; never persist tokens in
  `store.json`.
- Request minimal token scopes (fine-grained, read-only where possible).
- No telemetry or analytics by default.
