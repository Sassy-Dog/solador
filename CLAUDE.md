# DevCanopy - AI Assistant Instructions

This file provides context for Claude Code when working with the DevCanopy codebase.

## Project Overview

DevCanopy is a native macOS cockpit that watches development infrastructure at a
glance, rendered as a grid of panels (see `DevCanopy/Views/Cockpit/Panels/`):
- **Hosts** — live CPU/memory/disk/network/GPU/battery from a per-host agent over Tailscale
- **Containers** — podman/docker/tart containers and VMs on those hosts
- **Repos** — one fixed row per watched repo: running-workflow count + longest-running
  elapsed, plus local/remote branch and worktree counts (folds in the former Git Worktrees panel)
- **GitHub Runners** — self-hosted runner availability/activity
- **Usage** — token rollups from local Claude Code usage logs (subscription; no USD),
  plus per-provider usage sections (Neon compute/storage MTD, Sentry accepted error
  events over 30d)

It has three parts:
- **macOS app** (`DevCanopy/`) — SwiftUI + SwiftData cockpit. This is the shipped app.
- **Agent** (`agent/`) — Rust (axum) HTTP service exposing host metrics + container
  list as JSON behind a bearer token; reached over Tailscale. See `agent/README.md`.
- **HostMetricsKit** (`Packages/HostMetricsKit/`) — local Swift package for
  local-machine metric collection (CPU/GPU/battery via IOKit), shared by the app.

There is also an experimental cross-platform walking skeleton, kept separate from
the shipped SwiftUI app above (which stays untouched): a Rust workspace
(`crates/wire`, `crates/viewmodel`, `crates/agentclient`) plus a Tauri v2 app
(`app/`) that polls one live agent and renders one host-monitoring card, proving out
a macOS/Windows-portable stack. Its frontend is plain HTML/CSS/JS with no bundler
(`app/ui/`) and its own Playwright e2e suite (`tests/frontend/`). See
`.superpowers/sdd/2026-07-27-cross-platform-walking-skeleton/` for the plan/reviews
that produced it.

## Development Workflow

### Quick Commands
- `./dev` - Build and run (debug mode)
- `./dev run --release` - Run release build
- `./dev test` - Run all tests: the Swift app, **plus** the root Rust workspace
  (`cargo test --locked --workspace` — `crates/*`, `app/src-tauri`) and the
  `tests/frontend` Playwright e2e suite
- `./dev lint` - SwiftLint + SwiftFormat, **plus** `cargo fmt --check` + `cargo clippy`
  for the root Rust workspace, mirrors CI (run before pushing)
- `./dev format` - Auto-fix formatting: SwiftFormat, **plus** `cargo fmt` for the
  root Rust workspace
- `./dev clean` - Clean build artifacts
- `./dev xcode` - Open in Xcode
- `./dev publish` - Publish a new release (CalVer minted from git — see `Docs/VERSIONING.md`)
- `./prd` - Production build (alias for `./dev build --release`)

> `./dev lint` is the local mirror of CI's Lint job. `./Scripts/install-hooks.sh`
> (one-time) wires it to a pre-push hook so lint/baseline failures never reach CI.
> When renaming `.swift` files, also re-point their entries in `lint-baseline.json`
> (the baseline is path-keyed; a rename un-baselines its violations).
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
│                          # Cross-platform walking skeleton (experimental,
│                          # see Project Overview):
├── Cargo.toml             # Root Rust workspace: crates/* + app/src-tauri
├── rust-toolchain.toml    # Pins the root workspace's toolchain (agent/'s convention)
├── crates/
│   ├── wire/              # Wire-format types shared with the agent's JSON contract
│   │                      # (package `devcanopy-wire`, imported as `wire`)
│   ├── viewmodel/         # host_card(): every string/colour the frontend paints
│   └── agentclient/       # HTTP client polling the same agent the Swift app polls
├── app/
│   ├── src-tauri/         # Tauri v2 shell: polls the agent, exposes `snapshot`
│   └── ui/                # Frontend: plain HTML/CSS/JS, no bundler
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
- All credentials stored in the macOS Keychain (`Services/KeychainHelper.swift`);
  never persisted in SwiftData.

### Responsive layout (breakpoints)
- `Views/Cockpit/CockpitBreakpoints.swift` holds the responsive math — pure values,
  no SwiftUI, unit-tested like `CoreGridLayout`/`VolumeGridLayout`.
- The model is CSS `repeat(auto-fit, minmax(<min>, 1fr))`, **not** global `sm/md/lg`
  tiers: every panel declares its own `CockpitPanelKind.minWidth`, and `reflow()`
  splits a row only when *its* panels stop fitting. So Claude Usage + Azure Cost stay
  side-by-side at a width where the host cards must stack.
- Panels never measure themselves. One `GeometryReader` at the `CockpitView` root is
  the only measurement; each panel's width is *derived* from the reflowed row and
  handed down as `\.cockpitPanelWidth` (`CockpitPanelWidth.swift`).
  **Don't reach for `.background(GeometryReader { … .preference(…) })` here** — those
  preferences do not reach `onPreferenceChange` in this SwiftUI version, and the
  reader silently stays at 0.
- Host cards need ≥ 900pt each (`CockpitBreakpoints.hostCardMinWidth`); below that
  they stack, or collapse to tabs if the user picks that in General settings
  (`hostOverflowMode`).
- Unknown width (0) means "not in a cockpit", and every fallback picks the layout that
  can't be unreadable — `hostColumns` stacks rather than assuming wide. Assuming wide
  is what let a dead measurement pass for a deliberate layout.

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