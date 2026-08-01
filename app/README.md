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
│   ├── capabilities/     # default.json — the webview's ACL
│   └── tauri.conf.json   # window, CSP, `frontendDist: ../ui`
└── ui/                   # frontend: plain HTML/CSS/JS, no bundler
    ├── app.js            # the cockpit
    └── settings.js       # the Settings view
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

- **General is stored, not yet consumed.** The shell polls every host once a
  second because one history sample is one fixed time slice, and it has none of
  the periodic services the refresh interval governs in the Swift app; the core
  row span and host-overflow mode are read by `viewmodel`'s card and cockpit
  functions from their own constants. The preferences persist (same file, same
  keys, same laundering rules as Swift); wiring them through is the next slice.
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
```

`--dump-settings` is a `settings_view` payload built from a fixed configuration
(one enabled host with a token and a hidden volume, one disabled host with
neither; two credentials stored, two not) with hard-coded uuids, so it is
byte-stable across regenerations and covers both sides of every badge. The rest
are full `cockpit` payloads — the same shape the command returns, so
the offline path cannot diverge from the real one — built from the committed
agent-contract fixture, so they reproduce on a clean checkout with no agent
involved. `npm test` in `tests/frontend` writes them under `app/ui/` (all
gitignored) — which matters for the smoke test below.

## Manual IPC smoke test

**Nothing automated exercises the Tauri IPC boundary**
([#123](https://github.com/Sassy-Dog/devcanopy/issues/123)). Both sides of the
seam are tested and the seam itself is not: the Rust tests call
`cockpit_view(&[HostState], width)` and `settings::view(…)` directly rather than
through their `#[tauri::command]` wrappers, and the Playwright suite stubs
`window.__TAURI__.core.invoke` — every command, cockpit and settings alike —
with Rust-dumped JSON. A break in the ACL
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
   is absent. If a fixture is sitting there from a Playwright run, a completely
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
