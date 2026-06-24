# DevCanopy

A native macOS cockpit for watching your development infrastructure at a glance —
designed to live full-screen on a second monitor.

## Features

DevCanopy renders a grid of glanceable panels:

- **Hosts** — live CPU / memory / disk / network / GPU / battery for each machine,
  pulled from a small per-host agent over [Tailscale](https://tailscale.com).
- **Containers** — podman, docker, and tart containers/VMs running on those hosts.
- **Repos** — one fixed row per watched repo: running-workflow count and longest-running
  elapsed, alongside local/remote branch and worktree counts, across your portfolio.
- **GitHub Runners** — self-hosted runner availability and activity.
- **Claude Usage** — token rollups from your local Claude Code usage logs.

## Architecture

- **macOS app** (`DevCanopy/`) — SwiftUI + SwiftData cockpit. Polls remote hosts,
  reads local git/Claude state, and renders the panels.
- **Agent** (`agent/`) — a small Rust ([axum](https://crates.io/crates/axum)) HTTP
  service that exposes host metrics and a container list as JSON, guarded by a bearer
  token. Runs on Linux and macOS; the app reaches it over Tailscale. See
  [`agent/README.md`](agent/README.md).
- **HostMetricsKit** (`Packages/HostMetricsKit/`) — a local Swift package that
  collects local-machine metrics (CPU/GPU/battery via IOKit and `sysinfo`-style
  sampling), shared by the app and reusable by the agent's macOS build.

## Quick Start

1. Clone the repository:
```bash
git clone https://github.com/Sassy-Dog/devcanopy.git
cd devcanopy
```

2. Build and run:
```bash
./dev run
```

## Development

### Common Commands

- `./dev` - Build and run in debug mode
- `./dev run --release` - Run release build
- `./dev run --log console --log-level debug` - Run with console logging
- `./dev test` - Run all tests
- `./dev lint` - Run SwiftLint + SwiftFormat checks (mirrors CI)
- `./dev format` - Auto-fix formatting with SwiftFormat
- `./dev clean` - Clean build artifacts
- `./dev xcode` - Open in Xcode
- `./dev publish --bump patch` - Create a new release
- `./prd` - Build production version

### Linting

`./dev lint` runs the exact checks CI runs (`swiftlint --strict` against
`lint-baseline.json`, plus `swiftformat --lint`), so lint failures surface locally
instead of after a CI round-trip. Run `./Scripts/install-hooks.sh` once to enable a
**pre-push hook** that runs it automatically before every push (bypass a single push
with `git push --no-verify`). With Claude Code, a `PostToolUse` hook
(`.claude/settings.json`) also auto-formats each `.swift` file as it's edited.

### Requirements

- macOS 14.0 (Sonoma) or later
- Xcode 15.0 or later
- XcodeGen (`brew install xcodegen`)
- SwiftLint + SwiftFormat (`brew install swiftlint swiftformat`) — for `./dev lint`
- [Rust toolchain](https://rustup.rs) — only if building/deploying the agent (`agent/`)

### Project Structure

```
DevCanopy/
├── dev                     # Development script (entry point)
├── prd                     # Production build script
├── Scripts/                # Build and utility scripts
├── project.yml             # XcodeGen configuration
├── DevCanopy/              # macOS app source
│   ├── App/               # App lifecycle, ContentView, CockpitView host
│   ├── Models/            # SwiftData models (MonitoredHost, AppSettings, WorkflowRunModels)
│   ├── Services/          # Host/agent, GitHub CI, containers, Claude usage, worktrees
│   ├── Views/             # SwiftUI views (Cockpit panels + Settings)
│   └── Resources/         # Info.plist, entitlements, assets
├── DevCanopyTests/         # App unit tests
├── Packages/
│   └── HostMetricsKit/    # Local Swift package: local-machine metrics collection
└── agent/                  # Rust per-host metrics agent
```

## Configuration

### Connecting hosts

Remote hosts run the agent (`agent/`) and are reached over Tailscale. Add a host in
**Settings → Hosts** with its Tailscale address and the agent's bearer token; the
token is stored in the macOS Keychain, never in SwiftData.

### GitHub authentication

The Repos and GitHub Runners panels read GitHub data using a **fine-grained personal
access token** with read-only access to **Actions** (workflow runs) and **Contents**
(remote branch counts). Add it in **Settings → GitHub Token**. See
[`Docs/github-setup.md`](Docs/github-setup.md).

All credentials are stored in the macOS Keychain.

### Terminal support

DevCanopy can open repositories in your preferred terminal:
- Terminal.app
- iTerm2
- Warp
- Ghostty

## Building for Release

1. Ensure you're on the main branch with a clean working tree
2. Run `./dev publish --bump patch` (or `minor`/`major`)
3. The script will:
   - Run tests
   - Update version number
   - Build release version
   - Create and push git tag

## Contributing

This is an internal Sassy Dog repository. The backlog lives on GitHub Project board
#5 (status-column driven); see [`CLAUDE.md`](CLAUDE.md) for the workflow.

1. Create a feature branch (`git checkout -b feat/your-change`)
2. Run `./Scripts/install-hooks.sh` once to enable the pre-push lint gate
3. Commit using conventional commits (`feat:`, `fix:`, `chore:`, `docs:`)
4. Push and open a Pull Request
5. Wait for CI to pass, then merge

## Acknowledgments

- Built with Swift and SwiftUI
- Agent built in Rust with axum and tokio
- Integrates with the GitHub API and Tailscale
