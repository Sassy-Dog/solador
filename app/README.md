# DevCanopy Walking Skeleton (`app/`)

An experimental cross-platform shell proving out a macOS/Windows-portable stack: a
[Tauri v2](https://v2.tauri.app) app that polls one live DevCanopy
[agent](../agent/README.md) and renders one host-monitoring card. The shipped
product is still the SwiftUI app in [`DevCanopy/`](../DevCanopy) — this is a
skeleton, not a replacement.

```
app/
├── src-tauri/            # Rust shell
│   ├── src/main.rs       # poll loop + `#[tauri::command] snapshot`
│   ├── capabilities/     # default.json — the webview's ACL
│   └── tauri.conf.json   # window, CSP, `frontendDist: ../ui`
└── ui/                   # frontend: plain HTML/CSS/JS, no bundler
```

Every string and colour the frontend paints comes from Rust
([`crates/viewmodel`](../crates/viewmodel)); the frontend does layout and nothing
else. It lives in the root Cargo workspace alongside `crates/metrics`,
`crates/viewmodel`, and `crates/agentclient` — distinct from `agent/`, which pins
its own toolchain and has its own CI job.

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

Env-driven for the skeleton; a real Settings surface arrives with the store crate.

| Env var                 | Default                       | Meaning                                                     |
|-------------------------|-------------------------------|-------------------------------------------------------------|
| `DEVCANOPY_HOST_URL`    | `http://100.87.202.125:7878`  | Agent base URL (Tailscale IP).                              |
| `DEVCANOPY_HOST_NAME`   | `ubu-3xdv`                    | Name shown on the card.                                     |
| `DEVCANOPY_HOST_ID`     | `default`                     | Credential-store account suffix (`host-<id>`).              |
| `DEVCANOPY_AGENT_TOKEN` | —                             | Bearer token. Falls back to the OS credential store (service `com.sassydog.devcanopy`, account `host-<id>`). |

An empty token never leaves the process, so it gets its own message —
*"No agent token configured for this host. Add one in Settings."* — rather than
reusing the agent's 401 text and sending you to check the wrong layer.

### Offline fixtures

```bash
cargo run -p devcanopy-app -- --dump sample.json               # a live-connection card
cargo run -p devcanopy-app -- --dump-stale sample-stale.json   # …stale, same numbers
```

Both are built from the committed agent-contract fixture, so they reproduce on a
clean checkout with no agent involved. `npm test` in `tests/frontend` writes them
to `app/ui/sample.json` and `app/ui/sample-stale.json` (both gitignored) — which
matters for the smoke test below.

## Manual IPC smoke test

**Nothing automated exercises the Tauri IPC boundary**
([#123](https://github.com/Sassy-Dog/devcanopy/issues/123)). Both sides of the
seam are tested and the seam itself is not: the Rust tests call
`view_for(&HostState)` directly rather than through `#[tauri::command] snapshot()`,
and the Playwright suite stubs `window.__TAURI__.core.invoke` with hand-built JSON.
A break in the ACL
([`src-tauri/capabilities/default.json`](src-tauri/capabilities/default.json)), in
the `invoke_handler` registration, or in the IPC transport itself would leave every
one of those tests green.

`tauri::test::mock_builder` is not a usable oracle here. Against the real
`generate_context!()` it returns `"snapshot not allowed. Plugin not found"`
*identically* under `permissions: []`, under `["core:default"]`, and with the
capability file deleted outright. Three configurations, one result — it is not
measuring the ACL at all. WebDriver automation was considered and rejected on
2026-07-31: `tauri-driver` has no macOS support (WKWebView ships no WebDriver), so
the only automatable host would be the GitHub-hosted Windows job — an oversized
harness for a walking skeleton, covering only the Windows build path. Trade-off
accepted: ACL and command-registration regressions are **documented, not
prevented**. Revisit if the skeleton graduates.

So this is a manual step. Run it after changing anything under
`src-tauri/capabilities/`, the `invoke_handler` registration, or the frontend's
`invoke` call.

### Procedure

1. **Remove the offline fixtures.** This is not optional:

   ```bash
   rm -f app/ui/sample.json app/ui/sample-stale.json
   ```

   `app/ui/app.js` falls back to `fetch("sample.json")` whenever `window.__TAURI__`
   is absent. If a fixture is sitting there from a Playwright run, a completely
   broken IPC boundary still paints a full, plausible, green-dotted card — the one
   failure mode that looks exactly like success. With the fixtures gone, nothing
   can paint the card except a successful `invoke` round-trip.

2. **Launch with a distinctive host name** — a second, independent discriminator,
   since the fixture hard-codes `ubu-3xdv` and so does the default:

   ```bash
   DEVCANOPY_HOST_NAME=smoke-$(date +%H%M%S) cargo run -p devcanopy-app
   ```

3. **Read the card**, after ~3s (the poll loop ticks every 2s).

### Pass

The window shows the host name you passed in step 2, plus **any one** of:

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
none of them has any path to the DOM except a successful `invoke("snapshot")`
round-trip. **You do not need a reachable agent to pass this test.** You need a
working boundary.

### Fail

| Symptom                                                                                       | Reading                                                                                                                                                              |
|-----------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| The window is a red `failed to load snapshot: …` line instead of a card.                       | The first `invoke` rejected — `app.js`'s initial `catch` replaces the whole body with the error. This is the expected shape of an ACL or registration break.           |
| The card's structure is there but every field is blank.                                       | `app.js` never ran at all — a CSP violation or a script error, so nothing ever reached the IPC boundary. Check the console before suspecting the ACL.                    |
| A card renders with plausible numbers that never change, and the host name is `ubu-3xdv` rather than the one you passed. | You skipped step 1. That is the fixture, not the boundary. Delete `app/ui/sample*.json` and re-run.                                                                     |

For the underlying error, open the webview console: right-click in the window →
**Inspect Element** (devtools are enabled in debug builds). An ACL rejection names
the command; the mock-harness form of it is `snapshot not allowed. Plugin not
found`.

### Recording a run

The last acceptance item on
[#123](https://github.com/Sassy-Dog/devcanopy/issues/123) is a human one: launch
once per this procedure and record the result. That record is currently the only
evidence the boundary works.

## Tests

All of it runs from the repo root via `./dev test` / `./dev lint`, and **none of it
covers the IPC boundary above**:

```bash
cargo test --locked --workspace     # crates/* + app/src-tauri unit tests
cd tests/frontend && npm test       # Playwright e2e over app/ui (stubs `invoke`)
```
