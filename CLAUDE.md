# Solador - AI Assistant Instructions

This file provides context for Claude Code when working with the Solador codebase.

## Project Overview

Solador is a native macOS cockpit that watches development infrastructure at a
glance, rendered as a grid of panels (see `DevCanopy/Views/Cockpit/Panels/`):
- **Hosts** — live CPU/memory/disk/network/GPU/battery from a per-host agent over Tailscale
- **Containers** — podman/docker/tart containers and VMs on those hosts
- **GitHub Repos** — one fixed row per watched repo: running-workflow count + longest-running
  elapsed, plus local/remote branch and worktree counts (folds in the former Git Worktrees panel)
- **GitHub Runners** — self-hosted runner availability/activity
- **Usage** — token rollups from local Claude Code usage logs (subscription; no USD),
  plus per-provider usage sections (Neon compute/storage MTD, Sentry accepted error
  events over 30d). The Neon cost rows added by #221 — `NEON EST. CHARGES (MTD)`,
  priced from operator-entered rates rather than a shipped price table, and
  `NEON LAST INVOICE`, off an undocumented endpoint and treated as best-effort —
  are on the cross-platform cockpit only; this panel renders compute/storage alone.

It has three parts:
- **macOS app** (`DevCanopy/`) — SwiftUI + SwiftData cockpit. This is the shipped app.
- **Agent** (`agent/`) — Rust (axum) HTTP service exposing host metrics + container
  list as JSON behind a bearer token; reached over Tailscale. See `agent/README.md`.
- **HostMetricsKit** (`Packages/HostMetricsKit/`) — local Swift package for
  local-machine metric collection (CPU/GPU/battery via IOKit), shared by the app.

**The Tauri app is the one going forward.** A Rust workspace (`crates/*`) plus
a Tauri v2 app (`app/`), macOS/Windows-portable, rendering every panel the
SwiftUI cockpit renders. It began as a walking skeleton (one host card); epic
#150 took it to panel parity across fourteen slices, and `app/README.md` is its
reference doc.

**The SwiftUI app is frozen.** It still exists, still builds locally
(`./dev build`, `./dev xcode`), and its sources stay the reference for parity
questions — but new features land Tauri-only, and as of 2026-08-04 it is
**neither built, tested nor linted in CI**: `Swift app tests` and `Lint` were
the only two jobs serving it, both sat on the org's two-runner macOS pool, and
`./dev test`/`./dev lint` no longer touch Swift either. A Swift compile break
can therefore reach `main` unnoticed; that is the accepted cost of freezing it.

- **Hosts** — this machine's card plus one per configured agent, in a
  width-aware grid (1s poll).
- **Containers/VMs** — local docker/podman/tart plus every host's agent, with
  grouping rules and presence memory (10s).
- **GitHub Repos + GitHub Runners** — per-repo CI health and counts, local
  branch/worktree counts, the org's self-hosted runners with the absence roster
  (the store's `refresh_interval_secs`).
- **Usage** — Claude token rollups (same interval); Neon, Sentry + Vercel (hourly).
  Vercel reads the FOCUS billing export: month-to-date spend and what falls
  beyond the plan. Neon
  renders compute/storage MTD, `NEON EST. CHARGES (MTD)` from operator-entered
  rates, and a best-effort `NEON LAST INVOICE` off an undocumented endpoint —
  that one failing degrades to `—` plus a footer, never the consumption rows.
- **Azure Cost** — the daily cost export, on its own 4h cadence.
- **Sentry Crons** — every cron monitor environment that is not `ok`, and **how
  long it has been broken** (the same fixed 1h cadence as the other Sentry read).
  The age comes from `activeIncident.startingTimestamp`, never from
  `lastCheckIn` — measuring from the last attempt is what made day 6 of an
  outage look identical to day 1, and on the monitor that motivated the panel
  the two read 7d 22h and 0d 22h. A check-in-derived age is a *different* enum
  variant and renders as the weaker claim it is (`≈`, amber, and a row that says
  why). Suppressed entries (`disabled`, or a strict `isMuted: true`, at either
  monitor or environment level) are counted and shown with their reason, never
  dropped; and a **blind read** — no monitors at all, or a monitor carrying no
  environments — is red, because an empty green panel is the failure this exists
  to remove.
- **Services** — third-party availability for the five vendors this stack
  depends on (GitHub, Anthropic, Vercel, Neon, Azure), read on the GitHub poll's
  cadence. Three transports behind one vocabulary (`crates/servicestatus`), and
  a change in either direction fires a desktop notification.
- **OpenClaw** — an agent farm over a live WebSocket. **Event-driven, on no
  cadence at all**, which is why it is the one panel with no staleness footer.

Those panels are arranged into rows `viewmodel::cockpit` reflows for the measured
width — one full-width Hosts row, GitHub Repos + GitHub Runners as halves,
Containers beside OpenClaw and Usage as quarters, then Azure Cost at a half
beside quarter-width Services and Sentry Crons —
and every card in a row is the same height. (Azure Cost gave up a quarter to
Sentry Crons and is a Half now, so it drops to one content column below a
~1648pt cockpit rather than ~1094 — a measured trade, not a regression.)
An in-app Settings surface over
`crates/store` (hosts CRUD, portfolio, credentials, container group rules,
cockpit layout, general prefs) applies changes without a restart. The frontend is
plain HTML/CSS/JS with no bundler (`app/ui/`) and has its own Playwright e2e suite
(`tests/frontend/`). The Tauri IPC boundary itself is **not** automatically
tested — `app/README.md` carries a five-minute manual smoke checklist that is the
only thing covering it (see #123).

See `.superpowers/sdd/2026-07-27-cross-platform-walking-skeleton/` for the
plan/reviews that produced the original skeleton.

## Development Workflow

### Quick Commands
- `./dev` - Build and run (debug mode)
- `./dev run --release` - Run release build
- `./dev test` - Run all tests: the root Rust workspace (`cargo test --locked
  --workspace` — `crates/*`, `app/src-tauri`) and the `tests/frontend`
  Playwright e2e suite. **Not the Swift app** — it is frozen (see above); open
  `./dev xcode` to run its tests by hand.
- `./dev lint` - `cargo fmt --check` + `cargo clippy` for the root Rust
  workspace, mirrors CI (run before pushing)
- `./dev format` - Auto-fix formatting: `cargo fmt` for the root Rust workspace
- `./dev clean` - Clean build artifacts
- `./dev xcode` - Open in Xcode
- `./dev publish` - Publish a new release (CalVer minted from git — see `Docs/VERSIONING.md`)
- `./prd` - Production build (alias for `./dev build --release`)

> `./dev lint` is the local mirror of CI's Rust lint gates, which live inside the
> `rust-workspace` and `agent-tests` jobs — there is no separate `Lint` job any
> more. `./Scripts/install-hooks.sh` (one-time) wires it to a pre-push hook so
> lint failures never reach CI. `lint-baseline.json` and `.swiftlint.yml` remain
> in the tree for the frozen Swift app but nothing runs them.
>
> The root Rust workspace (`crates/*`, `app/src-tauri`) is distinct from `agent/`,
> which has its own `Cargo.toml`/`Cargo.lock`/`rust-toolchain.toml` and its own CI
> job (`agent-tests`); it is not run by `./dev test`/`./dev lint` and has no local
> wiring of its own — see `agent/README.md`. The root workspace's toolchain is
> pinned by the root `rust-toolchain.toml`, same convention as `agent/`'s.

### Backlog & workflow skills
The backlog is **GitHub Project board #5** (`Sassy-Dog`), status-column driven:
**Backlog → Ready → In progress → In review → Done**. It is the source of truth for
backlog state (not labels). Five generated `.claude/skills/` automate the loop:
- **plate-it** (`plate it`) — synthesize a prioritized plate from the board + CI + tech debt.
- **fill-it** (`fill it`) — groom Backlog issues until dispatchable, promote to **Ready**.
- **take-it** (`take #N`) — ship specific issues in parallel worktrees.
- **drain-it** (`drain it` / `/loop 5m /drain-it`) — loop dispatcher; ships from **Ready** until empty.
- **send-it** (`send it`) — single-PR end-to-end flow (worktree audit → gates → PR → merge).

The contract: **Ready means dispatchable** — fill-it produces it, drain-it consumes it.

### Project Structure
```
DevCanopy/
├── dev                     # Development script (entry point)
├── prd                     # Production build script
├── Scripts/                # Build and development scripts
│   ├── lib.sh             # Common functions
│   ├── config.sh          # App configuration
│   └── *.sh               # Implementation scripts
├── project.yml            # XcodeGen configuration
├── DevCanopy/             # macOS app source (the shipped app)
│   ├── App/              # App lifecycle, ContentView, CockpitView host
│   ├── Models/           # SwiftData models (MonitoredHost, AppSettings, WorkflowRunModels)
│   ├── Services/         # Host/agent, GitHub CI, containers, Claude usage, worktrees
│   ├── Views/            # SwiftUI views (Cockpit panels + Settings)
│   └── Resources/        # Info.plist, entitlements
├── DevCanopyTests/        # App unit tests
├── Packages/
│   └── HostMetricsKit/   # Local Swift package: local-machine metrics collection
├── agent/                 # Rust per-host metrics agent (axum) -- own Cargo
│                          # workspace/toolchain/CI job, not part of the
│                          # root Cargo.toml below
│
│                          # Cross-platform cockpit — the app going
│                          # forward (see Project Overview):
├── Cargo.toml             # Root Rust workspace: crates/* + app/src-tauri
├── rust-toolchain.toml    # Pins the root workspace's toolchain (agent/'s convention)
├── crates/
│   ├── wire/              # Wire-format types shared with the agent's JSON contract
│   │                      # (package `devcanopy-wire`, imported as `wire`)
│   ├── viewmodel/         # host_card(): every string/colour the frontend paints
│   ├── agentclient/       # HTTP client polling the same agent the Swift app polls
│   ├── store/             # settings/hosts/repos/container-rules/runner-roster/
│   │                      # cockpit-layout JSON + OS credential-store wrappers
│   ├── github/            # GitHub REST client (workflows, runners) + the
│   │                      #   "is it us?" conjunction verdict
│   ├── servicestatus/     # Third-party availability: Atlassian Statuspage,
│   │                      #   status.io and Azure's RSS incident feed
│   ├── localhost/         # this machine's metrics (sysinfo); every field the
│   │                      # platform can decline is an Option, never a 0
│   ├── usage/             # Claude Code log rollups + Neon + Sentry usage
│   ├── azurecost/         # Azure Cost Management export reader (SAS blob + CSV)
│   └── openclaw/          # OpenClaw gateway client: WS protocol v3, the Ed25519
│                          # device identity, the frame→snapshot reducer
├── app/
│   ├── src-tauri/         # Tauri v2 shell: one poll task per host plus this
│   │                      # machine (src/local.rs); `cockpit`, `containers`
│   │                      # (src/containers/), `repos`/`runners` (src/github/),
│   │                      # `usage` (src/usage.rs), `azure_cost` (src/azure.rs),
│   │                      # `crons` (src/crons.rs — Sentry cron monitors),
│   │                      # `openclaw` (src/openclaw.rs — a live session, not a
│   │                      # poll) + the `settings_*` surface (src/settings.rs)
│   └── ui/                # Frontend: plain HTML/CSS/JS, no bundler
│                          # (app.js = cockpit + panel-row layout,
│                          #  settings.js = Settings view,
│                          #  containers.js = Containers/VMs panel,
│                          #  github.js = Repos + GitHub Runners panels,
│                          #  usage.js = Usage panel, azure.js = Azure Cost,
│                          #  cronmonitors.js = Sentry Crons,
│                          #  openclaw.js = OpenClaw panel)
└── tests/frontend/         # Playwright e2e suite for app/ui/ (own package.json)
```

> After adding new `.swift` files, run `./Scripts/generate-project.sh` to regenerate
> the Xcode project — `./dev build`/`test` won't pick them up otherwise.

## Technology Stack
- **App language**: Swift 5.9+ (SwiftUI, SwiftData)
- **Agent language**: Rust (axum, tokio, sysinfo-style sampling)
- **Credential Storage**: macOS Keychain
- **Transport**: HTTP/JSON over Tailscale, guarded by a bearer token
- **Project Generation**: XcodeGen

## Key Implementation Notes

### Hosts & metrics
- Remote hosts run the Rust agent (`agent/`), polled by the app over Tailscale.
- Agent endpoints: `GET /v1/snapshot` (CPU/mem/disk/net/gpu/battery),
  `GET /v1/containers`, `GET /v1/health`. All require `Authorization: Bearer <token>`.
- Remote polling lives in `Services/HostMetrics/` and `Services/RemoteHosts/`;
  local-machine metrics come from `Packages/HostMetricsKit`.
- Container discovery: `Services/Containers/`.
- **Unknown is representable.** Every metric a producer may not be able to measure
  (memory used/swap/pressure, thermal state, the GPU fields, disk/network rates) is
  an Optional in `HostSnapshot.swift`, mirroring the `Option` in `crates/wire`: an
  absent key decodes to `nil`, `nil` re-encodes as an *omitted* key, and `0` means
  measured zero. `HostMetricLabels` is the one place `nil` becomes "—"; unmeasured
  samples never enter a history buffer, and `HostSnapshot.unmeasuredFields` is
  logged on change so every em dash is diagnosable. Known limit: agents predating
  #183 send literal zeros, which decode as measurements — only the agent can fix
  that.
- **Local collection is unknown-first too**, not just the wire: `MemorySampler`
  and `ProcessDiskIOSampler` return `nil` for a reading the kernel declined, and
  log the failure on the *transition* rather than once per 1 Hz poll. Capacity
  figures (`memory.totalGB`) come from infallible sources and stay non-Optional.
- **Configuration is unknown-first as well** (Tauri only): `panel::Configured`
  is `Unknown` / `Absent` / `Present`, defaulting to `Unknown`, and `Present` is
  recorded **when the credential is read, before the request**. Every panel used
  to store this as a `bool` a completed fetch set, so the first frame — before
  any pass had looked — was indistinguishable from "there is no credential":
  Repos and Runners opened on `connect a GitHub token in Settings`, Azure on
  `Add an Azure storage account in Settings`, Containers on `no containers
  detected`, on a machine where all of it was fine. Only `Absent` may paint a
  setup instruction; `Unknown` renders the panel's loading line. A defaulted
  *state* is as much a fabrication as a defaulted number.
- **`status_footer`'s `last_updated` is the last success, not the last attempt.**
  It renders `last ok {age}`, which is a promise only the caller can keep.
  Containers and Usage passed "when we last looked", so a Docker daemon that had
  never once answered reported `⚠ couldn't read docker · last ok 0s ago` — on
  every poll, forever. A panel needing both clocks keeps them as separate fields
  (`local_last_updated` vs `local_last_success`).

### Git worktrees & branches
- `Services/GitMonitor/` parses git/worktree state without modifying files
  (`GitWorktreeService.swift`, `GitStatusParser.swift`, `WorktreeParsing.swift`) and
  counts a repo's local branches + worktrees.
- Surfaced as per-repo counts in the **Repos** panel
  (`Views/Cockpit/Panels/GHWorkflowsPanel.swift`) — there is no standalone worktree panel.

### CI & Claude usage
- GitHub Actions data: `Services/GitHub/` (workflow health, self-hosted runners, and
  remote branch / open-issue / open-PR counts via the GitHub API).
- Claude Code usage rollups: `Services/ClaudeUsage/` (tokens only — USD is computed and
  unit-tested but never displayed, since the account is subscription-based).
- Neon consumption: `Services/NeonUsage/` (org-wide `consumption_history` read, MTD
  compute + branch storage). Own fixed 1h poll cadence, not the shared refresh
  interval; renders `—` when the key/org is missing or the API fails.
- Sentry usage: `Services/SentryUsage/` (org `stats_v2` read, accepted error events
  over a rolling 30d window, optional quota bar). Same 1h cadence + `—` rules as Neon.
  Distinct from `Services/SentrySetup.swift`, which is the app's own crash-reporting
  bootstrap — nothing in `SentryUsage*` touches the Sentry SDK.
- Sentry cron monitors (Tauri only): `crates/usage/src/sentry.rs`'s
  `cron_monitors()` / `summarize_monitors()` plus `app/src-tauri/src/crons.rs`.
  Same `org:read` token as the usage read and the same 1h cadence. Three wire
  traps are documented there and each has a test: there is **no** flat
  `projectSlug` (it is `project.slug`), **no** `hasMoreEnvironments` and no
  environment-truncation signal at all, and `activeIncident` is a key on every
  environment that holds `null` on the healthy ones. Build fixtures from the raw
  REST payload — the Sentry MCP server normalises the response and synthesises
  the first two, so fixtures derived from it are self-consistent and wrong.

### Authentication
- **Repos / GitHub Runners panels**: a fine-grained PAT with read-only access to
  **Actions** (workflow runs), **Contents** (remote branch counts), **Issues**
  (open-issue counts), and **Pull requests** (open-PR counts), entered in
  Settings → GitHub Token (`Views/Settings/SettingsView.swift`).
- **Remote hosts**: per-host bearer token entered in Settings → Hosts.
- **Usage panel → Neon**: an *organization* API key (scoped to the org's projects,
  not tied to a user account), entered in Settings → Usage. The non-secret `org_id`
  is a normal `@AppStorage` preference, not a Keychain item.
- **Usage panel → Sentry**: a personal or internal-integration token carrying only
  the read-only `org:read` scope, entered in Settings → Usage (org auth tokens carry
  a fixed CI-oriented scope set that excludes it). The non-secret org slug and
  monthly event quota are `@AppStorage` preferences, not Keychain items.
- **Sentry Crons panel** (Tauri only): the *same* `org:read` token and org slug as
  the Usage panel's Sentry section — one credential, two panels, so saving or
  clearing it wakes both poll loops or one of them keeps describing the previous
  token for up to an hour.
- All credentials stored in the macOS Keychain (`Services/KeychainHelper.swift`);
  never persisted in SwiftData. The Tauri shell (`crates/store::secrets`) stores
  its own credentials in the same Keychain service under different account names,
  and — as of the single-keychain-item migration, macOS only — consolidates them
  into one item, `secrets_v1`, rather than Swift's one-item-per-secret scheme; see
  `app/README.md`'s "Consolidated credential item" section. The Azure Cost panel no longer
  stores a credential at all: it mints a short-lived, container-scoped SAS per
  poll by shelling out to the Azure CLI (`az`, signed in as the operator), so
  there is no LaunchAgent, no keychain item and no consolidation exemption. The
  storage account and container are ordinary `Settings` fields.

### Responsive layout (breakpoints)
- `crates/viewmodel/src/cockpit.rs` holds the responsive math for the Tauri app
  (the app going forward); `Views/Cockpit/CockpitBreakpoints.swift` is the frozen
  Swift original it was ported from. Pure values, no UI, unit-tested like
  `CoreGridLayout`/`VolumeGridLayout`.
- The model is CSS `repeat(auto-fit, minmax(<min>, 1fr))`, **not** global `sm/md/lg`
  tiers: every panel declares its own `PanelKind::min_width`, and `reflow()`
  splits a row only when *its* panels stop fitting. So OpenClaw + Usage stay
  side-by-side at a width where the host cards — and Repos + Runners — must stack.
- **Spans, not even splits** (Tauri only): each placement carries a `PanelSpan` —
  `Full` / `ThreeQuarters` / `Half` / `Quarter`, weights 4/3/2/1 — and
  `panel_widths()` gives each panel its share of **one four-quarter grid**: a
  quarter track is `(width − 3·gap) / 4`, and a span of `k` gets `k` tracks plus
  the `k−1` gutters it swallows. The frontend paints the identical construction,
  `repeat(4, minmax(0,1fr))` plus `grid-column: <start> / span k`, so the two
  cannot drift. A panel is held to its `min_width` against the width its span
  gives it, never against the row's sum of minimums (that let a hungry panel
  borrow width it then never got).
- **The grid is the same on every row, so a span means one thing.** The
  denominator is a fixed four, *not* the weight of the panels in this row —
  which looks equivalent, since `fill_row` widens every rendered row to four
  quarters anyway, and is not: dividing `width − (n−1)·gap` by the row's own
  weight makes the subtracted gutter total depend on how many panels happen to
  share the row. A Half came out half a gutter narrower beside two Quarters than
  beside one Half, and the vertical edge under Repos|Runners missed the edge
  under Containers|OpenClaw by 8pt. Same span, same width, same gridline,
  wherever it lands.
- **Every rendered row is exactly four quarters.** When reflow cuts a row short
  its remaining panels are *widened* to fill it (`fill_row`), so a track is
  always one of the four named widths — a row left at three quarters would
  stretch to thirds, a width no picker offers and no user can name. The fill is
  the distribution closest to the authored proportions by least squared error
  (Half + Quarter becomes ThreeQuarters + Quarter; Quarter + Quarter becomes
  Half + Half, not a lopsided pair), and every candidate is checked against
  `min_width` at its *final* width — filling shrinks the panels that don't grow.
  A row with no legible filling is refused, and that refusal is what reflow
  reads as "this panel does not fit here", so the fit test and the widths it
  tests are one piece of arithmetic rather than two that can disagree.
- **The arrangement is the user's, per width band** (Tauri only): Settings →
  **Layout** edits a list of *breakpoints*, each an ordered list of
  `{panel, span}` slots plus its own host-overflow mode, persisted as
  `store.json`'s `layout`. `cockpit` picks the widest band the measured width
  clears (`settings::breakpoint_for`) on every frame, so a third-of-a-4K column
  can tab its host cards while the same cockpit maximised lays them out side by
  side. Rows are *packed* from each band's list (`CockpitLayout::from_order`,
  four quarters to a row) rather than authored.
  `settings::normalized_order` drops unknown or duplicate slots and appends
  missing panels from `DEFAULT_ORDER`, so a stored layout always renders every
  panel exactly once — including one added by a later build. `layout: null`
  means "never configured" and is what **Reset to default** restores; `reflow`
  still splits a packed row on a narrow window.
- Cards sharing a row are the same height (`.panel-row { align-items:stretch }`);
  content stays top-aligned, so the extra height is trailing space inside the
  shorter card. Panels whose body splits at `--panel-cols` (Containers, Runners,
  Repos, Azure Cost, Usage) use that width rather than stretching one column.
- Panels never measure themselves. One `GeometryReader` at the `CockpitView` root is
  the only measurement; each panel's width is *derived* from the reflowed row and
  handed down as `\.cockpitPanelWidth` (`CockpitPanelWidth.swift`).
  **Don't reach for `.background(GeometryReader { … .preference(…) })` here** — those
  preferences do not reach `onPreferenceChange` in this SwiftUI version, and the
  reader silently stays at 0.
- Host cards need ≥ 900pt each (`CockpitBreakpoints.hostCardMinWidth`); below that
  they stack, or collapse to tabs when the applicable layout breakpoint says so
  (`Breakpoint::host_overflow`; Swift's global `hostOverflowMode` survives in the
  store only as the seed a pre-breakpoint layout is migrated from).
- Unknown width (0) means "not in a cockpit", and every fallback picks the layout that
  can't be unreadable — `hostColumns` stacks rather than assuming wide. Assuming wide
  is what let a dead measurement pass for a deliberate layout.
- The per-core CPU grid picks its columns from `core_column_ladder`, and that
  ladder obeys the same two invariants `core_columns` documents: divide the core
  count evenly, stay at or below `CORE_MAX_COLUMNS`, and leave **at least two
  rows**. Until the one-row fix it offered every divisor, including the count
  itself, which rendered a 10-core M1 Max as one row of ten stretched cells
  while `core_columns` was answering 5 × 2 and passing all of its own tests. The
  ladder is what the shell renders, so the two must agree.

### UI Design
- Dark mode optimized; glanceable grid of cockpit panels.
- Status indicated by color: green (good), orange (warning), red (error).
- Designed for persistent full-screen display on a second monitor.

### SwiftData Models
- `MonitoredHost`: a remote host running the agent (address, port, hidden volumes).
- `AppSettings`: user preferences.
- `WorkflowRunModels`: persisted workflow run state.

## Testing

Run tests with:
```bash
./dev test
```

Tests cover the app (`DevCanopyTests/`) and the HostMetricsKit package
(`Packages/HostMetricsKit/Tests/`). Agent tests run via `cargo test` in `agent/`.

## Building for Distribution

1. Ensure clean working tree on main branch, up to date with origin
2. Run `./dev publish`
3. Script will:
   - Verify CI is green for HEAD (fails closed without a verdict)
   - Run tests
   - Mint + push the CalVer tag `vYYYY.M.P` (§4 probe/reuse/bump ladder; no version bump commit)
   - Build release stamped with the minted version

### Versioning (org spec v1.0 — `Docs/VERSIONING.md`)

Two decoupled numbers, both derived from git — never hand-maintained:
- **Marketing version**: CalVer `YYYY.M.<commits-this-month>` (UTC, non-padded
  month, floored at 1) — `Scripts/get-version-info.sh --version`.
- **Build number**: total commit count, monotonic forever —
  `Scripts/get-build-number.sh [--at <ref>]`.

`Scripts/build.sh` injects both as xcodebuild command-line build settings;
`project.yml` carries only static inert baselines (never computed into it,
never hand-bumped). Replay pins: `MARKETING_VERSION` / `BUILD_NUMBER` env.
Never compute a version anywhere else.

## Common Tasks

### Adding a New Cockpit Panel
1. Add a service under `DevCanopy/Services/` for the data source.
2. Add a `CockpitPanelKind` case — including its `minWidth` breakpoint — and wire it
   in `Views/Cockpit/CockpitView.swift`. `CockpitLayoutTests` fails if you skip the
   `minWidth`.
3. Create the panel view in `Views/Cockpit/Panels/`.
4. Run `./Scripts/generate-project.sh` so new files land in the Xcode project.

### Working on Git Worktree Monitoring
- Parsing/logic in `Services/GitMonitor/` (`GitWorktreeService.swift`,
  `GitStatusParser.swift`, `WorktreeParsing.swift`).
- Surfaced as per-repo branch/worktree counts in the **Repos** panel
  (`Views/Cockpit/Panels/GHWorkflowsPanel.swift`).

### Working on the Agent
- Rust source in `agent/src/` (`server.rs`, `metrics.rs`, `containers.rs`).
- Deploy via `agent/deploy`; see `agent/README.md` for endpoints and rollout.

### Modifying Build Scripts
- Edit scripts in `Scripts/` directory
- Common functions in `lib.sh`
- Configuration in `config.sh`

## Debugging

- App console logging: `./dev run --log console --log-level debug`
  (`DEBUG=1 ./dev run` only toggles the shell script's own logging).
- Check Console.app for app logs
- SwiftUI previews available for most views

## Important Conventions

1. Use SwiftUI's environment for dependency injection
2. Keep views small and focused
3. Business logic in Services, not Views
4. Use SwiftData's @Query for reactive updates
5. Handle errors gracefully with user-friendly messages

## Security Considerations

- Never log credentials or tokens (the agent must not echo its bearer token)
- Use Keychain for all sensitive data; never persist tokens in SwiftData
- Request minimal token scopes (fine-grained, read-only where possible)
- No telemetry or analytics by default