# DevCanopy - AI Assistant Instructions

This file provides context for Claude Code when working with the DevCanopy codebase.

## Project Overview

DevCanopy is a native macOS cockpit that watches development infrastructure at a
glance, rendered as a grid of panels (see `DevCanopy/Views/Cockpit/Panels/`):
- **Hosts** — live CPU/memory/disk/network/GPU/battery from a per-host agent over Tailscale
- **Containers** — podman/docker/tart containers and VMs on those hosts
- **GitHub Workflows** — GitHub Actions status across a portfolio of repos
- **GitHub Runners** — self-hosted runner availability/activity
- **Git Worktrees** — local worktrees and their remote sync state
- **Claude Usage** — token/cost rollups from local Claude Code usage logs

It has three parts:
- **macOS app** (`DevCanopy/`) — SwiftUI + SwiftData cockpit.
- **Agent** (`agent/`) — Rust (axum) HTTP service exposing host metrics + container
  list as JSON behind a bearer token; reached over Tailscale. See `agent/README.md`.
- **HostMetricsKit** (`Packages/HostMetricsKit/`) — local Swift package for
  local-machine metric collection (CPU/GPU/battery via IOKit), shared by the app.

## Development Workflow

### Quick Commands
- `./dev` - Build and run (debug mode)
- `./dev run --release` - Run release build
- `./dev test` - Run all tests
- `./dev lint` - SwiftLint + SwiftFormat checks, mirrors CI (run before pushing)
- `./dev format` - Auto-fix formatting with SwiftFormat
- `./dev clean` - Clean build artifacts
- `./dev xcode` - Open in Xcode
- `./dev publish --bump patch` - Publish new version
- `./prd` - Production build (alias for `./dev build --release`)

> `./dev lint` is the local mirror of CI's Lint job. `./Scripts/install-hooks.sh`
> (one-time) wires it to a pre-push hook so lint/baseline failures never reach CI.
> When renaming `.swift` files, also re-point their entries in `lint-baseline.json`
> (the baseline is path-keyed; a rename un-baselines its violations).

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
├── DevCanopy/             # macOS app source
│   ├── App/              # App lifecycle, ContentView, CockpitView host
│   ├── Models/           # SwiftData models (MonitoredHost, AppSettings, WorkflowRunModels)
│   ├── Services/         # Host/agent, GitHub CI, containers, Claude usage, worktrees
│   ├── Views/            # SwiftUI views (Cockpit panels + Settings)
│   └── Resources/        # Info.plist, entitlements
├── DevCanopyTests/        # App unit tests
├── Packages/
│   └── HostMetricsKit/   # Local Swift package: local-machine metrics collection
└── agent/                 # Rust per-host metrics agent (axum)
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

### Git worktrees
- `Services/GitMonitor/` parses git/worktree state without modifying files
  (`GitWorktreeService.swift`, `GitStatusParser.swift`, `WorktreeParsing.swift`).
- Surfaced by `Views/Cockpit/Panels/GitWorktreesPanel.swift`.

### CI & Claude usage
- GitHub Actions data: `Services/GitHub/` (workflow health + self-hosted runners).
- Claude Code usage rollups: `Services/ClaudeUsage/`.

### Authentication
- **GitHub CI panels**: a fine-grained PAT with read-only access to Actions, entered
  in Settings → GitHub Token (`Views/Settings/SettingsView.swift`).
- **Remote hosts**: per-host bearer token entered in Settings → Hosts.
- All credentials stored in the macOS Keychain (`Services/KeychainHelper.swift`);
  never persisted in SwiftData.

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

1. Ensure clean working tree on main branch
2. Run `./dev publish --bump patch` (or minor/major)
3. Script will:
   - Run tests
   - Update version
   - Build release
   - Create git tag
   - Push to GitHub

## Common Tasks

### Adding a New Cockpit Panel
1. Add a service under `DevCanopy/Services/` for the data source.
2. Add a `CockpitPanelKind` case and wire it in `Views/Cockpit/CockpitView.swift`.
3. Create the panel view in `Views/Cockpit/Panels/`.
4. Run `./Scripts/generate-project.sh` so new files land in the Xcode project.

### Working on Git Worktree Monitoring
- Parsing/logic in `Services/GitMonitor/` (`GitWorktreeService.swift`,
  `GitStatusParser.swift`, `WorktreeParsing.swift`).
- Panel in `Views/Cockpit/Panels/GitWorktreesPanel.swift`.

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