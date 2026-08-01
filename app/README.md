# DevCanopy Walking Skeleton (`app/`)

An experimental cross-platform shell proving out a macOS/Windows-portable stack: a
[Tauri v2](https://v2.tauri.app) app that polls every configured DevCanopy
[agent](../agent/README.md) and renders one host-monitoring card per host in a
width-aware grid. The shipped product is still the SwiftUI app in
[`DevCanopy/`](../DevCanopy) — this is a skeleton, not a replacement.

```
app/
├── src-tauri/            # Rust shell
│   ├── src/main.rs       # per-host poll tasks + the `#[tauri::command]` surface
│   ├── src/settings.rs   # the Settings view-model and its pure rules
│   ├── src/panel.rs      # the refresh-health footer every panel shares
│   ├── src/containers/   # the Containers/VMs panel: local runtimes, grouping,
│   │                     #   presence, and every string it paints
│   ├── src/github/       # the Repos + GitHub Runners panels, and the local
│   │                     #   git scan behind their LOCAL/WT columns
│   ├── capabilities/     # default.json — the webview's ACL
│   └── tauri.conf.json   # window, CSP, `frontendDist: ../ui`
└── ui/                   # frontend: plain HTML/CSS/JS, no bundler
    ├── app.js            # the cockpit
    ├── settings.js       # the Settings view
    ├── containers.js     # the Containers/VMs panel
    └── github.js         # the Repos + GitHub Runners panels
```

Every string and colour the frontend paints comes from Rust
([`crates/viewmodel`](../crates/viewmodel)); the frontend does layout and nothing
else. It lives in the root Cargo workspace alongside `crates/wire`,
`crates/viewmodel`, `crates/agentclient`, and `crates/store` — distinct from
`agent/`, which pins its own toolchain and has its own CI job.

## The `cockpit` command

One command, called on every poll tick and on resize:

```js
await window.__TAURI__.core.invoke("cockpit", { width: gridWidth });
```

```jsonc
{
  "hosts": [ /* one host_card / pending_card per enabled host, each with its `id` */ ],
  "hostColumns": 2,        // viewmodel::cockpit::host_columns for `width`
  "hostCardMinWidth": 900, // …and the numbers it used, so the frontend can't
  "spacing": 16,           //    re-derive them and disagree
  "panels": [ /* the panel table: id, title, minWidth */ ],
  "empty": null,           // or {"message": …} when no host is configured
  "settingsLabel": "Settings"  // the button that opens the Settings view
}
```

The column count is decided in Rust, not by a CSS `repeat(auto-fit, minmax(900px,
1fr))`: that CSS would be a second implementation of
`CockpitBreakpoints.hostColumns`, free to disagree with the tested one. The
frontend passes the grid's measured width and applies the answer.

## The `containers` command

The Containers/VMs panel, on its own 10s cadence — the cockpit's 1s tick is a
metrics cadence, and `docker ps` is a process spawn:

```js
await window.__TAURI__.core.invoke("containers");
```

```jsonc
{
  "id": "containers",
  "title": "Containers / VMs",
  "trailing": "6 total · 4 up · 2 stopped · 1 missing",
  "empty": null,                     // or {"message": "no containers detected"}
  "sections": [{
    "host": "this machine",          // local first, then remotes by name
    "label": "THIS MACHINE",
    "empty": null,                   // "no container runtimes" vs "no containers"
    "rows": [{
      "kind": "present",             // present | absent | aggregate
      "name": "devcanopy-db", "runtime": "docker",
      "dotColor": "#33d17a",
      "status": "Up 3 hours", "statusColor": "#33d17a"
    }]
  }],
  "footer": null                     // or {"text": "⚠ stale · updated 2m ago", "color": …}
}
```

Three sources feed it, all in [`src-tauri/src/containers/`](src-tauri/src/containers):

- **This machine** — `docker`/`podman`/`tart`, spawned by absolute path
  (`/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin`) because a macOS GUI app
  inherits a `launchd` environment whose `PATH` has none of them; on Windows it
  is a `PATH` lookup and tart is skipped. A runtime whose invocation fails
  contributes its **last-known list**, so one transient `tart list` failure
  cannot blank every VM row.
- **Each host with a token** — `agentclient::containers()`, one in-flight
  request each. A failed fetch leaves that section's previous rows alone.
- **Grouping rules + presence**, from [`crates/store`](../crates/store). Rules
  collapse ephemeral entities into one aggregate row, hide never-interesting
  ones, or *expect* them — an expected name keeps a standing row while absent
  (amber `recycling 40s`, red `missing 12m` past 300s) instead of vanishing
  with the VM. Absence is measured against the section's last **successful**
  poll, never render time, so a failing source freezes its clocks rather than
  ageing everything toward a false alarm.

Seeded rules match Swift's (`sassydog-ghr-ubu-*` → "ghr runners" and `api-*` →
"workflow jobs" on `ubu-3xdv`, `ghcr.io/*` hidden) and seed **only** when the
store has never carried rules: a deliberately emptied list stays empty.
**Editing the rules is not in this slice** — Swift keeps that editor in
Settings → Hosts, and the engine ships before its UI.

## The `repos` and `runners` commands

The CI-visibility half of the cockpit, both fed by one poll pass over
[`crates/github`](../crates/github) and both on the store's
**`refresh_interval_secs`** — the first consumer of that preference in this
shell.

```js
await window.__TAURI__.core.invoke("repos");
await window.__TAURI__.core.invoke("runners");
```

```jsonc
// repos
{
  "id": "ghWorkflows", "title": "Repos",
  "trailing": "1 needs approval · 1 running · 1 failed · 1 unreadable",
  "message": null,                       // or {"text": "connect a GitHub token in Settings"} / {"text": "loading…"}
  "columns": [{"label": "REPO", "width": null}, {"label": "ISSUES", "width": 52.0}, …],
  "rows": [{
    "repo": "Sassy-Dog/velovate", "name": "velovate",
    "dotColor": "#e05a4f", "blinking": false,
    "cells": [{"text": "18", "color": "#cfe9d8", "width": 52.0}, …]
  }],
  "health": {"text": "✓ 4/6 healthy", "color": "#33d17a"}
}

// runners
{
  "id": "ghRunners", "title": "GitHub Runners",
  "trailing": "3/4 · 1 missing",
  "message": null,                       // or the connect / "loading runners…" line
  "stats": [{"label": "ONLINE", "value": "3/4", "color": "#33d17a"}, …],
  "chips": ["macOS 2/2", "Linux 1/2"],   // only for a platform the org actually has
  "rows": [{"kind": "absent", "name": "ubu-1", "os": "LINUX",
            "dotColor": "#e05a4f", "status": "missing 12m", "statusColor": "#e05a4f"}],
  "footer": null                         // or {"text": "⚠ … · last ok 4m ago", "color": …}
}
```

**Unknown is not zero, on every count cell.** `"—"` muted is "we could not find
out" — a failed fetch, a PAT missing the Issues or Pull requests scope, a repo
not checked out on this machine. `"0"` dimmed is "there are none". They are two
different Rust decisions arriving as two different `{text, color}` pairs, and
the frontend never derives either from a number: putting that distinction in JS
would put it where no Rust test can see it. It is the same rule the `/issues`
cursor-pagination guard in `crates/github` exists to protect.

The **column widths are Rust's**, in points, for the same reason the host
grid's column count is: seven fixed numeric columns summing to 312pt is what
`PanelKind::GhWorkflows.min_width` (560pt) is built on, and a width re-typed in
CSS is a second implementation free to disagree with the breakpoint.

**LOCAL and WT come from this machine**, not from GitHub:
[`src/github/git.rs`](src-tauri/src/github/git.rs) walks `~/Repos` three levels
deep, treats a directory holding a `.git` entry as a repo root and does not
descend into it (otherwise this repo's own `.claude/worktrees/…` checkouts
would each register as a second repo of the same name), then spawns
`git for-each-ref` and `git worktree list --porcelain` per repo. The join to a
tracked slug is `PortfolioRepos.normalize` — lowercase, letters and digits only
— which is what lets the slug `tailored-tip` find the folder `tailoredtip`. A
git invocation that fails yields `None`, i.e. `"—"`. Swift's `localBranchCount`
returns `0` there; that is a fabricated number and this deliberately does not
copy it. The scan root is a parameter, so the tests drive real temporary
repositories rather than a mock.

**The runner roster is the memory that makes an absence visible.** The org's
ephemeral runners de-register between jobs, so GitHub's registered list can
never say "mac-s2 *should* exist". Every name it has shown us is remembered in
`store.json`'s new `runner_roster` field (the Rust counterpart of the Swift
app's `ghRunnerRoster` UserDefaults blob), with a 24h age-out for rotated
names. An absent name renders amber `recycling 40s` inside the 300s grace and
red `missing 12m` past it — and those clocks are folded forward **only by a
successful fetch**, so an hour of GitHub being unreachable ages nothing. The
panel keeps its last-good rows through a failure and puts the reason in the
footer (`staleAfter: 150s`).

**Applied without a restart.** The token and the portfolio are re-read on every
pass, and `github_wake` cuts the sleep short after a Save, a Clear, a portfolio
edit or a new refresh interval — shortening a 5-minute cadence to 30 seconds
and then waiting five minutes for it to take is indistinguishable from the
setting doing nothing. This is the periodic-service counterpart to
`reload_hosts`, which reconciles tasks instead.

Two things are deliberately **not** in this slice, both noted rather than
hidden:

- **Tapping a row does not open GitHub.** The Swift panel's `onTapGesture` calls
  `NSWorkspace.open`; the equivalent here needs the opener plugin granted to
  `capabilities/default.json`, which widens the one seam with no automated
  coverage ([#123](https://github.com/Sassy-Dog/devcanopy/issues/123)). No URL
  is carried in the payload either — a field nothing can act on is a field that
  reads as a missing feature. Same reasoning as the About links.
- **No needs-approval notification, and no "Forget" on an absent runner.** The
  first is platform notification wiring (its own concern); the second is a
  right-click context menu over a `roster::forget` that `crates/github` already
  implements. A remembered name still ages out on its own after 24h.

Both panels stack full-width below the host grid, as Containers does. The
shipped Swift layout puts Repos and Runners side by side in one row, and
`viewmodel::cockpit::reflow` already computes that — wiring the multi-panel row
is one change that moves all four panels at once, not a piece of this slice.

Two counts are deliberately not the same number: the rollup counts *every*
container the runtimes reported, including the ones rules hid or collapsed (so
cruft building up stays visible), while `· N missing` counts exactly what the
rollup can no longer see.

## Hosts

Hosts come from [`crates/store`](../crates/store) (`Store::open()` — one JSON
file under the platform config dir), and their bearer tokens from the OS
credential store, never from that file. Each **enabled** host gets its own poll
task, its own `AgentClient` and its own history buffers, so an unreachable host
shows its own error card while every other card stays live; cards are ordered by
name, mirroring the Swift coordinator's `SortDescriptor(\.name)`. (The SwiftUI
cockpit puts the *local* machine first; this shell has no local collector —
`HostMetricsKit` is Swift-only — so the remote ordering is the whole ordering.)

A failed poll is debounced two ticks before the card stops claiming to be
current, matching `RemoteHostMetricsService.failureThreshold`: one missed poll on
a flappy tailnet is a blip, not an outage.

## Settings

The **Settings** button opens an in-app view over the cockpit: General, GitHub,
Portfolio, Hosts, Azure Cost, Usage and About — the Swift window's tabs, minus
OpenClaw (deferred to the OpenClaw slice; the store already carries its gateway
URL and bearer-token key). Every label, help string and result line it paints
comes from `src/settings.rs`, exactly as the cards' do from `crates/viewmodel`.

**In-app view, not a second window.** A second window means the frontend calls
`WebviewWindow`, which means granting the webview
`core:webview:allow-create-webview-window` (or `core:default`) —
widening the one seam in this app with no automated coverage
([#123](https://github.com/Sassy-Dog/devcanopy/issues/123)), for a surface that
needs no platform capability at all. Every command below is *app-defined*, which
Tauri's ACL permits without a grant, so `capabilities/default.json` keeps its
empty `permissions` list.

| Command | What it does |
|---|---|
| `settings_view` | the whole surface, including a `stored: bool` per credential |
| `settings_save_general` | refresh interval, core-row span, host-overflow mode |
| `settings_save_providers` | Neon org id, Sentry slug + quota, Azure budget (all four at once) |
| `settings_add_host` / `settings_remove_host` / `settings_set_host_enabled` | hosts CRUD; add files the token, remove deletes it |
| `settings_unhide_volume` | one mount, on a host or on the local list |
| `settings_test_host` | one `/v1/health` probe → the Swift result line |
| `settings_add_repo` / `settings_remove_repo` / `settings_set_repo_enabled` / `settings_set_repo_workflows` | the tracked-repo portfolio |
| `settings_save_secret` / `settings_clear_secret` | one credential, by key (`github`/`neon`/`sentry`/`azure`) |

The portfolio, the refresh interval and the `github` credential all wake the
GitHub poll loop as well as saving — see [the `repos` and `runners`
commands](#the-repos-and-runners-commands).

Every mutation answers in one shape — `{status, settings}` — and the frontend
re-renders from the `settings` it gets back rather than patching its own copy,
so it can never show an edit that failed to save.

**Secrets never travel back.** A credential goes frontend → `store::SecretKey` →
OS credential store and stops there; what `settings_view` carries is one boolean
per credential, which is all the "stored" badge needs. Nothing in the payload,
the store file, or a log line can carry a value.

**Applied without a restart.** Adding, removing or disabling a host rebuilds the
poll set in place: tasks are *reconciled*, not torn down, so every host that did
not change keeps its own task — and therefore its sparkline history, its failure
streak and its last-success time. Unhiding a volume deliberately skips that
reload entirely, mirroring the Swift view's `applyHiddenMounts()` vs `reload()`.

Two gaps, deliberate and worth knowing:

- **The refresh interval is consumed; the other two General preferences are
  not.** `refresh_interval_secs` is the GitHub panels' cadence, and changing it
  applies immediately (see below). The host poll loop is deliberately *not* on
  it — it stays at 1s because one history sample is one fixed time slice
  (`PX_PER_SAMPLE`), so that cadence is part of the charts' time axis rather
  than a preference. The core row span and host-overflow mode are still read by
  `viewmodel`'s card and cockpit functions from their own constants; they
  persist (same file, same keys, same laundering rules as Swift) and nothing
  reads the stored value yet.
- **About's version is hard-coded** to the crate version, not the CalVer the
  Swift app derives from git ([#15](https://github.com/Sassy-Dog/devcanopy/issues/15)),
  and the About links render as selectable URLs rather than anchors — following
  one would navigate the cockpit's own webview away from the app, and opening it
  externally needs the opener plugin granted to the ACL.

## Build & run

There is **no Tauri CLI in this repo**: no `cargo-tauri` dependency, no
`package.json` under `app/`, and `tauri.conf.json` points `frontendDist` at the
static `../ui` directory with no `beforeDevCommand`. There is nothing for
`tauri dev` to do that plain cargo does not, so the launch command is:

```bash
cargo run -p devcanopy-app          # from the repo root
```

CI builds the same binary the same way — `cargo test --locked --workspace` in both
the `rust-workspace` (self-hosted macOS) and `windows-tests` jobs — and the
Playwright suite's `pretest` shells out to it for its fixtures.

### Configuration

The store is the configuration, and the Settings view above is how you edit it.
Two env vars remain for the headless/first-run cases a UI can't cover — a smoke
run, a fresh checkout, a machine you are driving over SSH.

| Env var                | Default                        | Meaning                                                                                     |
|------------------------|--------------------------------|---------------------------------------------------------------------------------------------|
| `DEVCANOPY_SEED_HOST`  | —                              | `"name\|address\|port\|token"`. Provisions that host **if no host with that address exists**; port defaults to 7878 and the token (when non-empty) goes to the OS credential store under the new host's id. Same parse and same no-op rule as Swift's `RemoteHostsCoordinator.seedFromEnvironmentIfNeeded()`, so it is safe to leave exported — relaunching never accumulates duplicates. |
| `DEVCANOPY_STORE_DIR`  | platform config dir            | Where `store.json` lives. A scratch directory here keeps an experiment (or the smoke test below) out of the real store. |

Tokens live in the OS credential store (service `com.sassydog.devcanopy`, account
`host-<uuid>`), never in `store.json`. An empty token never leaves the process, so
it gets its own message — *"No agent token configured for this host. Add one in
Settings."* — rather than reusing the agent's 401 text and sending you to check
the wrong layer.

### Offline fixtures

```bash
cargo run -p devcanopy-app -- --dump sample.json                 # one live host
cargo run -p devcanopy-app -- --dump-stale sample-stale.json     # …stale, same numbers
cargo run -p devcanopy-app -- --dump-cockpit sample-cockpit.json # three hosts: live / stale / failed
#   …plus `--width <pt>` (which column count to compute) and `--hosts <n>`
#   (how many of the three to include; 0 is the unconfigured cockpit).
cargo run -p devcanopy-app -- --dump-settings sample-settings.json # the Settings surface
cargo run -p devcanopy-app -- --dump-containers sample-containers.json # the Containers panel
#   …plus `--empty`, which dumps the no-runtimes state with a failed-tool footer.
cargo run -p devcanopy-app -- --dump-repos sample-repos.json         # the Repos panel
cargo run -p devcanopy-app -- --dump-runners sample-runners.json     # the Runners panel
#   …both take `--empty`, which dumps the no-credential state.
```

`--dump-settings` is a `settings_view` payload built from a fixed configuration
(one enabled host with a token and a hidden volume, one disabled host with
neither; two credentials stored, two not) with hard-coded uuids, so it is
byte-stable across regenerations and covers both sides of every badge.
`--dump-containers` is the same idea one panel over: a hand-made state at a
**fixed** timestamp (a relative age like "recycling 40s" would otherwise drift
on every dump and no test could assert one), covering a present container, a
stopped one, a VM recycling, one missing past grace, and a collapsed group on a
remote section. `--dump-repos` / `--dump-runners` are the same idea again, and
their state is asserted by a Rust test (`the_fixture_covers_every_rendering_
the_panels_have`) precisely so the Playwright suite cannot pass against a
payload that quietly lost the case it claims to exercise — it carries an
unknown count beside a genuine zero, an approval gate, a failing repo, an
unreachable one, and remembered runners in both absence states. The rest
are full `cockpit` payloads — the same shape the command returns, so
the offline path cannot diverge from the real one — built from the committed
agent-contract fixture, so they reproduce on a clean checkout with no agent
involved. `npm test` in `tests/frontend` writes them under `app/ui/` (all
gitignored) — which matters for the smoke test below.

## Manual IPC smoke test

**Nothing automated exercises the Tauri IPC boundary**
([#123](https://github.com/Sassy-Dog/devcanopy/issues/123)). Both sides of the
seam are tested and the seam itself is not: the Rust tests call
`cockpit_view(&[HostState], width)`, `settings::view(…)` and
`containers::view(…)` directly rather than through their `#[tauri::command]`
wrappers, and the Playwright suite stubs `window.__TAURI__.core.invoke` — every
command, cockpit, settings and containers alike — with Rust-dumped JSON. A break in the ACL
([`src-tauri/capabilities/default.json`](src-tauri/capabilities/default.json)), in
the `invoke_handler` registration, or in the IPC transport itself would leave every
one of those tests green.

`tauri::test::mock_builder` is not a usable oracle here. Against the real
`generate_context!()` it returns `"<command> not allowed. Plugin not found"`
*identically* under `permissions: []`, under `["core:default"]`, and with the
capability file deleted outright. Three configurations, one result — it is not
measuring the ACL at all. WebDriver automation was considered and rejected on
2026-07-31: `tauri-driver` has no macOS support (WKWebView ships no WebDriver), so
the only automatable host would be the GitHub-hosted Windows job — an oversized
harness for a walking skeleton, covering only the Windows build path. Trade-off
accepted: ACL and command-registration regressions are **documented, not
prevented**. Revisit if the skeleton graduates.

So this is a manual step. Run it after changing anything under
`src-tauri/capabilities/`, the `invoke_handler` registration, or any frontend
`invoke` call — including the settings ones, which is what step 5 is for.

### Procedure

1. **Remove the offline fixtures.** This is not optional:

   ```bash
   rm -f app/ui/sample*.json
   ```

   `app/ui/app.js` falls back to `fetch("sample.json")` whenever `window.__TAURI__`
   is absent — and `containers.js` falls back to `sample-containers.json` the same
   way, which the glob above covers. If a fixture is sitting there from a
   Playwright run, a completely
   broken IPC boundary still paints a full, plausible, green-dotted card — the one
   failure mode that looks exactly like success. With the fixtures gone, nothing
   can paint the card except a successful `invoke` round-trip.

2. **Launch against a scratch store, with a distinctive host name** — a second,
   independent discriminator, since the fixture hard-codes `ubu-3xdv` and so does
   every seeded example. `DEVCANOPY_STORE_DIR` is what makes the run repeatable:
   seeding is a no-op when the address is already configured, so a smoke run
   against the *real* store would silently reuse the host from last time — and its
   name — instead of the one you just passed.

   ```bash
   DEVCANOPY_STORE_DIR=$(mktemp -d) \
   DEVCANOPY_SEED_HOST="smoke-$(date +%H%M%S)|100.87.202.125|7878|$TOKEN" \
     cargo run -p devcanopy-app
   ```

   Leave `$TOKEN` unset to exercise the zero-setup case below; set it to the
   agent's real bearer token to exercise the live one.

   Use `cargo run`, not a binary you built earlier: `generate_context!()` embeds
   `app/ui` into the executable at compile time, so an edited `app.js` that was
   never recompiled is simply not the frontend under test. (Confirmed the hard
   way — a deliberately broken `invoke` name still passed until the rebuild.)

3. **Read the terminal.** A successful round-trip prints exactly once:

   ```
   cockpit: first frontend request (1 host(s), 968pt)
   ```

   Every failure this test hunts has the same shape from inside Rust — a
   rejected ACL, an unregistered command, a CSP break that stops `app.js` before
   it ever calls `invoke` — so that one line separates a working boundary from
   all of them, and it does so on a machine whose screen you cannot see (a
   headless CI box, a locked Mac). It does **not** say what the frontend then
   painted, which is what step 4 is for.

4. **Read the card**, after ~3s (each host is polled once a second, and a failed
   poll is debounced two ticks).

5. **Click Settings**, then read the terminal again. One click is the whole
   check for the settings command surface, and it prints the same kind of
   one-line, screen-free signal step 3 does:

   ```
   settings: first frontend request (1 host(s), 6 repo(s))
   ```

   The counts are the store's own — the host you seeded in step 2, and the
   seeded portfolio — so the line proves the round-trip *and* that it read the
   real store rather than a default. The Hosts tab should list that host by the
   name you passed, with **No token** or **Token stored** matching what you set
   `$TOKEN` to. Pressing **Test** on it exercises `settings_test_host`, the one
   command that reaches the network from this surface: expect
   `✓ <hostname> · agent v<version>` against a live agent, `✗ unreachable —
   host down or agent stopped` against a dead one, and `✗ auth failed (401) —
   check token` with no token set. **Done** returns to the cockpit.

6. **Back on the cockpit, read the terminal once more** for the containers
   command's own one-line signal (it prints on the panel's first request, which
   happens at load — so it is usually already there):

   ```
   containers: first frontend request (1 section(s))
   ```

   The section count is this machine's own: 1 with no reachable agent, 2 once a
   host has answered `/v1/containers`. Below the host grid, the **Containers /
   VMs** heading should carry a `N total · N up · N stopped` line and a
   **THIS MACHINE** section listing whatever docker/podman/tart report here — or
   the sentence `no container runtimes` if none are installed, which is a pass:
   that string is `containers::view`'s and has no path to the DOM except a
   successful `invoke("containers")`. The first pass can take up to 10s (the
   panel's cadence), so give it a tick before reading it as broken.

7. **Read the terminal once more** for the two GitHub panels' own one-line
   signals (they print at load, alongside the containers one):

   ```
   repos: first frontend request (6 repo row(s))
   runners: first frontend request (4 runner row(s))
   ```

   **Zero rows on both is still a pass for the boundary** — with no GitHub
   token in the scratch keychain both panels render `connect a GitHub token in
   Settings`, and that sentence is `github::repos_view`'s with no path to the
   DOM except a successful `invoke`. What the counts add is the *second* thing:
   a non-zero repo count proves the loop read the seeded portfolio out of the
   real store rather than a default, exactly as step 5's counts do.

   To exercise the populated path, save a fine-grained PAT under Settings →
   GitHub (Actions, Contents, Issues and Pull requests, all read-only, plus org
   self-hosted runners read for the Runners panel). It applies without a
   relaunch — `settings_save_secret` wakes the loop — so the panels should fill
   within a few seconds rather than on the next refresh interval, and that
   immediacy is itself the check on `github_wake`. Then **Clear** it and watch
   both panels drop back to the connect line just as fast.

   The Repos table is where the "—"-vs-`0` rule is visible: a repo not checked
   out under `~/Repos` shows `—` in LOCAL and WT while one that is shows real
   counts, and neither ever shows a zero it did not read. A PAT missing the
   Issues scope shows `—` under ISSUES with the repo still green — a missing
   scope is not an outage.

### Pass

The terminal prints the `cockpit: first frontend request …` line above, and the
window shows a **Hosts** heading and one card carrying the host name you passed
in step 2, plus **any one** of:

- **the host card rendering live agent data** — CPU / memory / GPU / disk /
  network values that keep changing between ticks, next to a green connection dot;
  or
- **the text** `Couldn't reach the agent. Check the host is up and the agent is
  running.` where the CPU model would be, with `—` for the CPU value and a red dot
  (the agent is down or unreachable); or
- **the text** `No agent token configured for this host. Add one in Settings.` in
  that same slot (no token configured — the zero-setup case).

All three are equally good evidence, and that is the point: **every one of those
strings is produced in Rust** — `AgentError::user_message` in
[`crates/agentclient`](../crates/agentclient), `pending_card` in
[`crates/viewmodel`](../crates/viewmodel), or `host_card`'s formatted numbers — and
none of them has any path to the DOM except a successful `invoke("cockpit")`
round-trip. **You do not need a reachable agent to pass this test.** You need a
working boundary.

Step 5 passes when the `settings: first frontend request …` line prints with the
store's real counts and the Hosts tab names the host from step 2 — the same
argument, one command over: every string in that view is `settings::view`'s, and
its only path to the DOM is a successful `invoke("settings_view")`.

Step 6 passes when `containers: first frontend request …` prints and the panel
carries a heading, a rollup line and at least one section — the same argument a
third time: `"Containers / VMs"`, `"no container runtimes"` and every count are
`containers::view`'s, reachable only through `invoke("containers")`. An empty
machine passes; a missing panel does not.

Step 7 passes when both `repos:` and `runners: first frontend request …` print
and both panels carry a heading plus *either* their table or the connect-a-token
sentence — the same argument a fourth and fifth time. A missing panel does not
pass: both stay hidden until a payload arrives, deliberately, so a broken
boundary cannot masquerade as an unconfigured one.

Seeding a second host — run once more with a different address, against the same
`DEVCANOPY_STORE_DIR` — is the multi-card version of the same check: two cards,
side by side above ~1816pt of window (2 × 900 + 16) and stacked below it.

### Fail

| Symptom                                                                                       | Reading                                                                                                                                                              |
|-----------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| The window is a red `failed to load cockpit: …` line instead of a card.                        | The first `invoke` rejected — `app.js`'s initial `catch` replaces the whole body with the error. This is the expected shape of an ACL or registration break.           |
| The card's structure is there but every field is blank.                                       | `app.js` never ran at all — a CSP violation or a script error, so nothing ever reached the IPC boundary. Check the console before suspecting the ACL.                    |
| A card renders with plausible numbers that never change, and the host name is `ubu-3xdv` rather than the one you passed. | You skipped step 1. That is the fixture, not the boundary. Delete `app/ui/sample*.json` and re-run.                                                                     |
| `No hosts configured. Add one in Settings.` and no card at all.                                | A malformed `DEVCANOPY_SEED_HOST` (empty name or address) — the boundary is fine, the configuration is not. Note this still *proves* the round-trip: that sentence is `cockpit_payload`'s and has no other path to the DOM. Fix the variable and re-run to get a named card. |
| The window opens and stays up, but the `cockpit: first frontend request …` line never prints.  | The boundary is broken and the window is hiding it. This is the definitive terminal-side failure: `invoke` never reached Rust. Verified as a real discriminator on 2026-07-31 by renaming the command in `app.js` — the app still launched and still held a window, and the line stayed absent. |
| The cards paint, but clicking **Settings** does nothing and `settings: first frontend request …` never prints. | The cockpit half of the boundary is fine and the settings half is not: an unregistered `settings_view`, or a script error in `settings.js` that stopped it before it wired the button. Check the webview console. |
| Settings opens with no tabs and no controls, or the button carries no label.                   | `settings_view` answered with something that isn't a settings payload — or `app.js` painted a cockpit payload with no `settingsLabel`. Regenerate the fixtures and check the payload shape, not the ACL. |
| The cards paint but there is no **Containers / VMs** panel at all, and `containers: first frontend request …` never prints. | The containers half of the boundary is broken: an unregistered `containers` command, or a script error in `containers.js`. The panel stays hidden until a payload arrives — deliberately, so a broken boundary cannot masquerade as an idle machine. Check the webview console. |
| The panel renders but every section says `no containers` on a machine that is definitely running some. | The boundary is fine; the *discovery* is not. The tools are resolved by absolute path (`/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin`) — a docker installed anywhere else is invisible. A failing tool would instead name itself in the footer (`⚠ couldn't read docker`). |
| The **Repos** or **GitHub Runners** panel is missing entirely, and its `first frontend request …` line never prints. | That half of the boundary is broken: an unregistered `repos`/`runners` command, or a script error in `github.js`. Both panels stay hidden until a payload arrives, so this cannot be mistaken for "no token configured" — that state renders a visible panel with one sentence in it. Check the webview console. |
| Both panels render, but every LOCAL and WT cell is `—` on a machine that definitely has the repos checked out. | The boundary is fine; the *scan* is not. It looks only under `~/Repos`, three levels deep, and joins by name with punctuation and case stripped — a checkout somewhere else is invisible, and a directory renamed away from its slug will not match. `—` is the honest answer to both, which is why it is not a zero. |
| The Runners panel shows `⚠ couldn't read runners — token needs org self-hosted runners (read)`. | Not a boundary failure — the round-trip worked and that string is `github::RUNNERS_ERROR_MESSAGE`. The PAT is missing the org self-hosted-runners read permission, which is a separate grant from the repo-scoped ones. The Repos panel beside it should still be populated. |

For the underlying error, open the webview console: right-click in the window →
**Inspect Element** (devtools are enabled in debug builds). An ACL rejection names
the command; the mock-harness form of it is `cockpit not allowed. Plugin not
found`.

### Recording a run

The last acceptance item on
[#123](https://github.com/Sassy-Dog/devcanopy/issues/123) is a human one: launch
once per this procedure and record the result. That record is currently the only
evidence the boundary works.

| Date       | Change under test | Step 3 (terminal) | Step 4 (visual) |
|------------|-------------------|-------------------|-----------------|
| 2026-08-01 | Repos + GitHub Runners panels ([#172](https://github.com/Sassy-Dog/devcanopy/issues/172)) | **Not performed.** The two new commands (`repos`, `runners`) and their **step 7** are therefore *documented, not verified* — no `repos: first frontend request …` line has ever been observed. What was verified instead is everything below the boundary: the payloads were dumped from the real binary and rendered in a browser under the app's own CSP (`tests/frontend/csp_server.py`), which exercises `github.js`, the CSSOM colour path and the column-width math, but stubs the IPC transport exactly as the Playwright suite does. The ACL is untouched (`permissions` still `[]`, both commands app-defined), which is the only reason to expect this to be uneventful — not evidence that it is. | **Not performed** (see left). |
| 2026-08-01 | Settings surface + `App` state restructure ([#163](https://github.com/Sassy-Dog/devcanopy/issues/163)) | **Pass.** Fixtures removed, scratch store, `DEVCANOPY_SEED_HOST="smoke-…\|100.87.202.125\|7878\|"` (no token). Terminal: `cockpit: first frontend request (1 host(s), 968pt)` — so the ACL, the handler registration and the transport still carry the call after `manage()` changed from `Cockpit` to `App` and the handler list grew from one command to fifteen. | **Not performed**, and neither was **step 5** — both need a click on a Mac someone else is working on. The settings half of the boundary is therefore *documented, not verified*: `settings: first frontend request …` has never been observed. Worth ten seconds from anyone who launches this next. |
| 2026-07-31 | `snapshot` → `cockpit`, N-card grid ([#157](https://github.com/Sassy-Dog/devcanopy/issues/157)) | **Pass.** Fixtures removed, scratch store, `DEVCANOPY_SEED_HOST="smoke-233344\|100.87.202.125\|7878\|"` (no token). Terminal: `cockpit: first frontend request (1 host(s), 968pt)` — so the ACL, the handler registration and the transport all carried the call, and `width` arrived. App still up when the run ended. Negative control run immediately before (command renamed in `app.js`, rebuilt) printed nothing, so the signal discriminates. | **Not performed** — the Mac's screen was locked (`CGSSessionScreenIsLocked`), which makes `screencapture` return black frames, and no Accessibility grant was available to read the window's text. Worth a human glance next time someone has the screen in front of them. |

## Tests

All of it runs from the repo root via `./dev test` / `./dev lint`, and **none of it
covers the IPC boundary above**:

```bash
cargo test --locked --workspace     # crates/* + app/src-tauri unit tests
cd tests/frontend && npm test       # Playwright e2e over app/ui (stubs `invoke`)
```

`npm test`'s `pretest` regenerates every fixture above by running the real
binary, so a payload-shape change breaks the suite rather than drifting past it.
`npm run fixtures` does the same without running the tests — handy when opening
`app/ui/index.html` in a plain browser.
