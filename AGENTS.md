# Solador — AI Assistant Instructions

This file provides context for coding agents (Codex, and Claude Code through `CLAUDE.md`'s import) when working with the Solador codebase.

## Project Overview

Solador is a cross-platform desktop cockpit that watches development
infrastructure at a glance, rendered as an overview of tiles, each opening to its panel. It is a Rust
workspace (`crates/*`) plus a Tauri v2 app (`app/`), macOS/Windows-portable.

It has three parts:
- **App** (`app/`) — Tauri v2 shell plus a bundler-free HTML/CSS/JS frontend.
- **Crates** (`crates/*`) — all the logic: polling, view models, storage,
  vendor clients. The shell is thin on purpose; panels are testable without a UI.
- **Agent** (`agent/`) — Rust (axum) HTTP service exposing host metrics +
  container list as JSON behind a bearer token, reached over any network path
  (Tailscale is optional). It is a
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

## Where the depth lives

This file is held under 32 KiB (the org's agent-guidance budget; Codex truncates
past it). `scripts/agent-guide-size-guard.sh` fails `./dev lint` and CI's
`Secrets guard` job above 32,768 bytes. **Read the file below before changing
its area** — each holds what used to be a section here, moved verbatim (#638).
Comments in code cite these files by name.

| Before changing… | Read |
|---|---|
| a panel's behaviour, cadence, footer or staleness rule | `docs/PANELS.md` |
| hosts, containers, auth, layout math, Settings/Configure, or any UI behaviour; the transport rules | `docs/APP-INTERNALS.md` (and `app/README.md`) |
| `./dev` commands, the test suites, CI-only shell gates | `docs/TESTING.md`, `docs/CI.md` |
| the release path (signing, notarization, `.dmg`, the Windows installer, the updater feed, the agent release train) | `docs/RELEASING.md`, `docs/AGENT-DISTRIBUTION.md`, `docs/SECRETS.md` |
| anything under `agent/` or `agent/deploy/` (install, update, rollback, TLS, bind rules, the timer) | `docs/AGENT-DEPLOYMENT.md`, `agent/README.md` |
| how a version is derived or minted | `docs/VERSIONING.md` |
| `site/` | `docs/WEBSITE.md` |
| the crate / directory layout | `docs/PROJECT-STRUCTURE.md` |

## Development Workflow

CI selects work by changed paths; see [docs/CI.md](docs/CI.md). Local commands
remain full validation.

- `./dev` — build and run (debug); `./dev run --release`; `./dev build`
  (plain cargo); `./dev build --bundle` / `--release` (= `./prd`) go through
  `cargo tauri build`; `./dev agent` cross-compiles the agent's four targets.
- `./dev test` — the whole suite: root Rust workspace (`agent/` included),
  the shell suites and the `tests/frontend` Playwright e2e. **The gate pair is
  `./dev test && ./dev lint`**; a clean `cargo test` still fails CI if fmt,
  clippy, shellcheck or a guard is off. Inventory: `docs/TESTING.md`.
- `./dev lint` — `cargo fmt --check`, clippy, `bash -n` / `shellcheck` over
  every shell source, and the repo's guards (secrets, `.claude` tracking,
  pin guards, agent-deps, and the agent-guide size budget).
  `./scripts/install-hooks.sh` (one-time) wires it to a pre-push hook.
- `./dev format`, `./dev clean` (`--over-budget` for tidy-repo).
- `./dev publish` **pushes a real tag** and builds a signed, notarized
  `.dmg`; `--skip-mint` skips the tag, `--agent` mints the agent's own
  `agent-v*` tag. Never run a signed or notarized build casually.

`agent/` is a workspace member (covered by `./dev test` / `./dev lint`); its
CI job runs `-p solador-agent` on Linux, because a bare `cargo build` there
would resolve `app/src-tauri` and need webkit2gtk.

### Releasing (summary; detail in `docs/RELEASING.md`)

Two release trains, neither triggers the other: a `v*` tag runs `release.yml`
(signed, notarized, universal macOS bundle plus a signed NSIS Windows
installer, attached to a **draft**; publishing runs `publish-feed.yml`), and an
`agent-v*` tag runs `release-agent.yml` (four agent binaries, the Linux musl
ones static, with minisign signatures; publishing runs `publish-agent-feed.yml`). Every job that
holds a credential declares `environment: prd`; `ci.yml` references zero
secrets and `scripts/secrets-guard.sh` asserts it. Neither train can be
exercised by a PR. Signing runs last, after the version stamp; the build
asserts what it produced rather than trusting its exit code.

### Project Structure

`crates/*` holds the logic (viewmodel, store, vendor clients, `wire`, `fault`,
`certpin`, `buildversion`, `localhost`, `usage`, `subscription`, `openclaw`,
`hostsetup`, `crashreport`, `updatefeed`, `agentrelease`, …), `app/src-tauri`
the thin Tauri shell, `app/ui` the plain HTML/CSS/JS frontend, `agent/` the
per-host daemon, `scripts/` the build and guard shell scripts behind `./dev`,
`tests/frontend` the Playwright suite, `site/` solador.app, `docs/` the
long-form docs. **Dependency edges that must not exist:** `agent/` never
resolves `crates/updatefeed`, `crates/agentrelease`, `crates/hostsetup`,
`crates/subscription`, `crates/crashreport` (asserted by
`scripts/agent-deps-guard.sh`). The per-crate descriptions are in
`docs/PROJECT-STRUCTURE.md`. `.claude/` is untracked except
`.claude/sassy-dog/<name>.md` (see Security).

## Technology Stack

Rust (Tauri v2, tokio, axum for the agent); plain HTML/CSS/JS frontend with no
bundler; credentials in the OS credential store (macOS Keychain, Windows
Credential Manager). **Transport rule:** a *paired* host is dialled over HTTPS
pinned to its agent's certificate; an *unpaired* host over plain HTTP, and only
on loopback or Tailscale. **The cockpit never sends the bearer token over plain
HTTP to any other address** (`crates/agentclient`'s `plain`, #449): Tailscale is
IPv4 `100.64.0.0/10` and IPv6 `fd7a:115c:a1e0::/48` minus the 4via6 prefix
`fd7a:115c:a1e0:b1a::/64`; a host name counts only if every address it resolves
to does, the connection goes to those vetted addresses, and there are no
redirects and no proxy. Detail:
`docs/APP-INTERNALS.md`.

## Key Implementation Notes (rules that apply everywhere)

Detail for each is in `docs/APP-INTERNALS.md` and `docs/PANELS.md`.

- **Unknown is representable.** Every metric a producer may not measure is an
  `Option` in `crates/wire`; an absent key is `None`, `0` means measured zero,
  and unmeasured samples never enter a history buffer. Configuration is
  unknown-first too: `panel::Configured` is `Unknown` / `Absent` / `Present`
  and only `Absent` may paint a setup instruction. A defaulted state is as much
  a fabrication as a defaulted number.
- **An added `Option` is also how the wire carries a version**: the absent key
  is the whole version check (#378).
- **`status_footer`'s `last_updated` is the last success, not the last
  attempt.**
- **A host is pinned to its agent's certificate, or it is plain HTTP — never
  both, and never a fallback (#448).** `solador-app host …` asks the running
  app over the hand-off channel; it never writes the store or keychain itself.
- **No seeded container grouping rules**, and matching is first-match-wins.
- **Another program's credential is read, never kept** (Security below); Usage
  → Claude rollups have **no account** and a subscription is not an account.
- **Eight panel cadences are the operator's**, edited in the owning tile's
  Configure and read after every pass; a below-floor value is refused with its
  reason, not clamped (`set_panel_interval_secs` / `panel_interval_secs` still
  clamp, because a hand-edited `store.json` never reaches the refusal).
- **Layout is decided in Rust, never re-derived in JS** (`crates/viewmodel/src/cockpit.rs`):
  host columns, host tabs, per-panel `min_width` and `panel_columns`. A tile owns
  its own view settings (`coreRowSpan`, `hostOverflow`, `rowGapPx`, `detailView`,
  `runnerFullView`), edited in its Configure; there is no global layout, and no
  Full view of Solador itself (#647).
- **Where a setting lives**: tile-specific in its Configure, source credentials
  in Settings → Connections, app-wide in Settings (`app/README.md`).
- **The Tauri IPC boundary is deliberately not automated** (#123 closed on it:
  `tauri-driver` has no macOS support). `app/README.md` carries the five-minute
  manual smoke checklist, and that is the only coverage; a test claiming
  coverage it does not have would be worse.
- **`LEGACY_APP_DIR_NAME` and the credential-store `SERVICE` must not be
  renamed**: the first is what the store migration reads from, the second
  orphans every stored credential at once.

## Testing

`./dev test` (see Development Workflow; inventory and the long notes on the
e2e server bind, the multi-frame cockpit stub and the `dmg` / notarization
suites in `docs/TESTING.md`). Agent tests run via `cargo test` in `agent/`;
`agent/deploy/lib_test.sh` gates the deploy scripts. Do not weaken or
reorder a guard without reading its test: every guard here except `scripts/agent-deps-guard.sh` ships a negative
control proving it fails when it should.

## Versioning (`docs/VERSIONING.md`)

Two decoupled numbers, both derived from git — **never compute a version
anywhere else, never hand-maintain one**: the marketing version is CalVer
`YYYY.M.<commits-this-month>` (`scripts/get-version-info.sh --version`), the
build number is the total commit count (`scripts/get-build-number.sh`).
`tauri.conf.json` authors no `version`, and `app/src-tauri/Cargo.toml`'s
`0.1.0` is unpublished metadata nothing reads. The agent has its own
`agent-vYYYY.M.N` (`--agent-version`, `--agent-tag`); `MARKETING_VERSION` never
reaches it, and **never delete an `agent-v*` tag**. A shallow clone yields no
version rather than a wrong one. Derivation, mint and pin detail:
`docs/VERSIONING.md`.

## Website (`site/`, solador.app)

Static, in this repo, and it **makes product claims**: a change that alters
something the site says (a panel, a permission, an install step, a platform
floor) updates `site/` in the same PR. No analytics, ever. Download buttons
never fabricate a URL. The coding-agent prompt lives in README.md and
`/setup/#coding-agent` and must match word for word (`site.spec.js` checks).
Deploy, CSP and screenshot detail: `docs/WEBSITE.md`.

## Common Tasks

### Adding a New Cockpit Panel
1. Add the data source as a crate under `crates/` (or a module in an existing one).
2. Add a `PanelKind` case — **including its `min_width`** — in
   `crates/viewmodel/src/cockpit.rs`. Skipping it does not compile (the `match`
   is exhaustive), and `every_panel_kind_declares_a_positive_min_width` fails on
   a zero. Add the kind to `PanelKind::ALL` too; nothing enforces that, and the
   tests that iterate `ALL` never see a kind left out.
3. Add the Tauri command in `app/src-tauri/src/`.
4. Add the frontend renderer in `app/ui/`.

### Working on the Agent
Source in `agent/src/` (`server.rs`, `metrics.rs`, `containers.rs`); deploy via
`agent/deploy`. **Read `docs/AGENT-DEPLOYMENT.md` before changing install,
update, rollback, TLS, bind or timer behaviour** — its order of operations is
the security design.

## Debugging

- The frontend is plain JS — the Tauri webview's devtools work normally.
- `crates/*` are ordinary Rust libraries; prefer a unit test over launching the app.

## Important Conventions

1. Business logic in `crates/`, not in the Tauri shell or the frontend.
2. Keep the frontend thin — it paints what `viewmodel` hands it.
3. Handle errors gracefully with user-friendly messages; `user_message()` on
   error types is the repo-wide convention. **Error enums classify;
   `crates/fault` owns the words** (#352, #354): every state the `Fault`
   vocabulary names renders `Fault::message(vendor)`; a stock sentence is the
   floor a message may not fall below, never a cap. Never fall back to
   `self.to_string()`. The full rule is in `docs/APP-INTERNALS.md`, **Error
   vocabulary**.
4. Never fabricate a value to fill a gap — render `—` and say why. **A state is
   a value**: an unanticipated failure keeps `Fault::Unexpected` rather than
   being rounded to the nearest anticipated one, and a crate whose failure the
   vocabulary has no word for keeps its own sentence rather than borrowing a
   neighbour's.

## Security Considerations

- Never log credentials or tokens (the agent must not echo its bearer token).
- **Another program's credential is read, never kept** (`crates/subscription`).
  Nothing reads Claude Code's or Codex's sign-in until that provider's opt-in is
  on; the reader runs on each poll, the token is used for one request and
  dropped, and no error message, `Debug` output or test assertion may carry it
  (`secret.rs`'s redaction is tested). The crate is app-only, and
  `scripts/agent-deps-guard.sh` asserts `agent/` never resolves it. A change
  that would write, refresh or rotate either sign-in needs the operator's
  explicit approval first: refreshing a shared OAuth token rotates it for the
  tool that owns it.
- **`.claude/` is untracked except `.claude/sassy-dog/`**, the config of the
  optional sassy-dog plugin (the public Sassy-Dog/skills). Nothing reads it
  unless that plugin is installed, but it is not inert for those who have it:
  `preflight_commands` runs as a shell command, `tidy-repo.md`'s lists decide
  what an untracked-file sweep deletes, and the skills read the CHECKED-OUT
  branch's copy. Review changes there like changes to `scripts/`. Never commit
  `.claude/settings.json` or `.claude/hooks/`: hooks in a committed settings
  file run with no trust prompt in parent-trusted, `claude -p`/SDK and cloud
  sessions, and a cloud session never loads a repository's `enabledPlugins`
  anyway. Never commit a per-checkout key there (`execution_site` above all:
  it names the one machine a checkout answers to). `scripts/claude-dir-guard.sh`
  fails CI on any other tracked `.claude` path, at any depth and in any case.
- **Check out contributor PRs into a worktree, never into the main checkout.**
  A branch that force-adds an ignored path (`git add -f .claude/settings.json`)
  silently overwrites the local ignored copy when it is checked out, and
  deletes it on switching back, before any CI has run. The guard above keeps
  such a branch from merging, not from being checked out. A fresh worktree has
  no ignored files to overwrite, but it does not stop the PR's OWN hooks: a
  worktree under this checkout's `.claude/worktrees/` is inside a trusted
  folder, so a `.claude/settings.json` the PR tracks loads there with no
  prompt. Before starting Claude Code in a contributor's worktree, check
  `gh pr diff <n> --name-only` for any `.claude` path, or run `main`'s guard
  against it (`REPO_DIR=<worktree> scripts/claude-dir-guard.sh` from the main
  checkout), never the PR's own copy, which the PR can edit.
- Use the OS credential store for all sensitive data; never persist tokens in
  `store.json`.
- Request minimal token scopes (fine-grained, read-only where possible).
- No telemetry or analytics by default. Crash reporting (`crates/crashreport`,
  #309) is the one thing that can leave the machine unasked, and it **cannot**:
  it is off in a fresh store, the Settings toggle is the only opt-in, and a
  compiled-in DSN is not consent and never becomes consent. With it off no
  client is created and no panic hook is installed. What a report may carry is
  an **allow-list** — the event is rebuilt from named fields rather than edited,
  so a field a later SDK adds is dropped for never having been listed. Never
  turn that into a blocklist, and never add a field to it without asking what
  an operator's infrastructure would look like inside it.
