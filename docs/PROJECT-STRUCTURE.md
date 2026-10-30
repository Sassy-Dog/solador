# Solador project structure

> Moved verbatim from `AGENTS.md` (#638), which is held under the org's 32 KiB guidance budget. Edit it here; `AGENTS.md` points at this file.

### Project Structure
```
├── .claude/sassy-dog/      # config the OPTIONAL sassy-dog Claude Code plugin
│                           #   reads, one .md per skill. The only tracked path
│                           #   under .claude/ (see .gitignore and Security
│                           #   Considerations in `AGENTS.md`)
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
│   ├── buildversion/       # the git-derived versions — the cockpit's CalVer and
│   │                       #   the agent's own (#490) — published to a crate at
│   │                       #   build time, two entry points. A BUILD dependency
│   │                       #   of both app/src-tauri and agent/, so the plumbing
│   │                       #   around scripts/get-version-info.sh has one home.
│   │                       #   NO dependencies, for the same reason fault has none
│   ├── certpin/            # the certificate fingerprint FORMAT (package
│   │                       #   `solador-certpin`, imported as `certpin`):
│   │                       #   uppercase colon-hex SHA-256, plus the parser an
│   │                       #   operator's paste goes through. NO dependencies —
│   │                       #   each consumer hashes with what it already has;
│   │                       #   the agent's `tls-fingerprint` and the cockpit's
│   │                       #   pin both write through it, so they cannot drift.
│   │                       #   Shared test vector: `tests/fixtures/tls/`
│   ├── fault/              # the stock message vocabulary: one sentence per
│   │                       #   anticipated failure. NO dependencies, deliberately —
│   │                       #   every vendor crate points at it, so anything added
│   │                       #   here is added to all of them. `viewmodel::fault`
│   │                       #   re-exports it
│   ├── viewmodel/          # every string/colour the frontend paints
│   ├── agentclient/        # HTTP client polling the agent; owns the rule that
│   │                       #   the token never crosses plain HTTP off loopback
│   │                       #   and Tailscale (`plain`, #449)
│   ├── store/              # settings/hosts/repos/container-rules/runner-roster/
│   │                       #   dashboard-layout JSON + OS credential-store wrappers
│   ├── github/             # GitHub client (REST + one GraphQL walk) + the "is it us?" verdict
│   ├── servicestatus/      # Atlassian Statuspage, status.io, Azure RSS
│   ├── accelerator/        # the IOKit `IOAccelerator` GPU reader, shared by
│   │                       #   localhost (this Mac's card) and agent/ (a Mac
│   │                       #   host's). Floor 11.0, the agent's: no newer symbol
│   ├── mempressure/        # the kernel's memory-pressure level on macOS (#544):
│   │                       #   one `sysctlbyname`, shared by localhost and agent/.
│   │                       #   Other platforms return None
│   ├── thermal/            # OS thermal pressure, shared by localhost and agent/
│   ├── localhost/          # this machine's metrics (sysinfo); every field the
│   │                       #   platform can decline is an Option, never a 0
│   ├── usage/              # Claude Code log rollups + Neon + Sentry + Vercel
│   ├── subscription/       # the Claude and Codex plans' limit windows: reads
│   │                       #   each tool's OWN saved sign-in (read-only, never
│   │                       #   copied or refreshed) and one undocumented usage
│   │                       #   endpoint each. APP-ONLY — agent/ must never
│   │                       #   resolve it
│   ├── azurecost/          # Azure Cost Management export reader (SAS blob + CSV)
│   ├── openclaw/           # OpenClaw gateway client: WS protocol v3, Ed25519
│   │                       #   device identity, the frame→snapshot reducer
│   ├── updatefeed/         # the `latest.json` manifest + the minisign check
│   │                       #   that gates publishing it, and the agent's
│   │                       #   `agent-latest.json` (module `agent`, #391).
│   │                       #   RELEASE TOOLING — neither the app nor the agent
│   │                       #   depends on it; publish-feed.yml runs its
│   │                       #   `solador-update-feed` bin, release.yml's Windows
│   │                       #   leg its `solador-verify-signature` bin (#705),
│   │                       #   publish-agent-feed.yml its `solador-agent-feed` bin
│   ├── agentrelease/       # the cockpit's reader of the AGENT feed (#489): fetches
│   │                       #   `agent-latest.json` + `.minisig` from `agent-latest`
│   │                       #   and verifies the exact bytes under the agent's keys
│   │                       #   (`build.rs` compiles `agent/release-signing-key*.pub`
│   │                       #   in, as `agent/build.rs` does) before decoding.
│   │                       #   APP-ONLY — agent/ must never resolve it, and it does
│   │                       #   not use `crates/updatefeed`
│   ├── hostsetup/          # `solador-app host …`: the command mode (dispatched
│   │                       #   first in main(), before the store opens) and the
│   │                       #   hand-off channel the running app serves it on (a
│   │                       #   Unix socket, a Windows named pipe), so
│   │                       #   the app stays the only writer of the store and
│   │                       #   keychain; `remote/host.sh` is the script
│   │                       #   `host add --ssh` embeds and runs on the host.
│   │                       #   APP-ONLY, like agentrelease
│   └── crashreport/        # opt-in crash reporting: the consent gate, the
│                           #   payload allow-list, the Sentry SDK. The ONLY
│                           #   crate carrying the SDK, and only app/src-tauri
│                           #   depends on it — agent/ must never resolve it
├── app/
│   ├── src-tauri/          # Tauri v2 shell: one poll task per host plus this
│   │                       #   machine (src/local.rs); `cockpit`, `containers`,
│   │                       #   `repos`/`runners`, `usage`, `azure_cost`,
│   │                       #   `crons`, `openclaw` + `settings_*` (src/settings.rs)
│   └── ui/                 # Frontend: plain HTML/CSS/JS, no bundler
├── agent/                  # Per-host metrics agent (workspace member, Linux CI)
│   ├── src/main.rs         #   the daemon. `src/lib.rs` re-exports the two
│   │                       #   modules `tests/` needs to reach (a binary
│   │                       #   crate's own modules cannot be):
│   ├── src/tls.rs          #   SOLADOR_AGENT_TLS=1 (#447) — the self-signed
│   │                       #   keypair's whole lifecycle, kept for the host's
│   │                       #   lifetime; PEM on disk, DER in memory
│   ├── src/update.rs       #   `solador-agent update`/`rollback` (#393) — the
│   │                       #   compiled-in trust set, the feed consumer, the
│   │                       #   stage/swap/restart/verify/restore transaction
│   ├── tests/              #   update_flow.rs: that transaction end to end over
│   │                       #   a loopback release + fakes; opt-in launchd smoke
│   └── release-signing-key.pub  # the trust set's first key; a committed
│                           #   release-signing-key-next.pub is its second
├── tests/fixtures/           # Wire-contract fixtures shared by agent/ + crates/,
│                           #   plus the feeds' signed fixtures (updater/, and
│                           #   agent-v/, the agent feed's: written by the
│                           #   producer, read by it and by the updater)
├── tests/frontend/         # Playwright e2e suite for app/ui/
├── brand/                  # Brand assets
├── site/                   # solador.app: plain HTML/CSS + two small JS modules,
│                           #   no build step; deployed by .github/workflows/pages.yml
│                           #   (see `docs/WEBSITE.md`). assets/screenshots/ is
│                           #   generated (`npm run screenshots`), never authored
└── docs/                   # Versioning, secrets, PRD
```
