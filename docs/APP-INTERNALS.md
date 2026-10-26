# Solador app internals

> Moved verbatim from `AGENTS.md` (#638), which is held under the org's 32 KiB guidance budget. Edit it here; `AGENTS.md` points at this file.

## Technology Stack
- **Language**: Rust (Tauri v2, tokio, axum for the agent)
- **Frontend**: plain HTML/CSS/JS, no bundler
- **Credential storage**: OS credential store (macOS Keychain, Windows
  Credential Manager)
- **Transport**: HTTPS pinned to the agent's certificate for a paired host,
  guarded by a bearer token; Tailscale is optional. Plain HTTP/JSON still exists — the agent serves
  it unless `SOLADOR_AGENT_TLS=1` (#447) is set, which serves HTTPS instead, with a self-signed
  certificate the agent keeps for the host's lifetime, and the cockpit dials a
  host over HTTPS only once the operator has **paired** it (#448): the
  certificate is fetched, its fingerprint shown, and pinned only when the
  operator presses Trust. An unpaired host is still dialled over plain HTTP,
  but only on loopback or Tailscale (below).
  Two different defaults, deliberately: the AGENT ITSELF is plain HTTP unless
  that env var is set; `install.sh` is what opts a *fresh* install into TLS
  (capability permitting — see `docs/AGENT-DEPLOYMENT.md`), which is not the
  same claim. **Plain HTTP is protected by the network, and only two networks
  qualify (#449).** Without TLS the agent binds only the Tailscale IP and
  refuses to start without one; with TLS on and no Tailscale it binds all
  interfaces instead, an explicit `SOLADOR_AGENT_BIND` always winning. That
  exposes the authenticated agent on every network the host is on (a cloud VM's
  public interface included), so `install.sh` names the interface it bound and
  says "all interfaces" when that is the result, and the docs tell the operator
  to firewall the port or pin `SOLADOR_AGENT_BIND`. **The cockpit enforces the
  other half: it never sends the bearer token over plain HTTP to an address that
  is not loopback or Tailscale** (`crates/agentclient`'s `plain`: IPv4
  `100.64.0.0/10`, IPv6 `fd7a:115c:a1e0::/48` minus Tailscale's 4via6 prefix
  `fd7a:115c:a1e0:b1a::/64`, whose last hop can leave the tailnet in cleartext.
  It trusts the address range, not tailnet membership: `100.64.0.0/10` is RFC
  6598 CGNAT space that some ISP, hotel and VPC networks also use, so pairing is
  the stronger protection. A host name counts only if
  *every* address it resolves to does, and the connection is made to the
  addresses that were vetted, so check and connect cannot disagree; a name that
  does not resolve sends nothing). So a **paired** host uses pinned TLS, and an
  **unpaired** host is only ever dialled over plain HTTP on loopback or
  Tailscale — that, not "the token plus the pinned certificate", is the whole
  defence, and it holds for an agent listening on any interface. An unpaired
  host anywhere else is not polled and reads "unpaired, off-tailnet — pair it in
  Settings" (`Fault::PlainHttpRefused`), never *Unreachable*; Settings fixes it
  with the existing Check certificate / Trust flow.

## Key Implementation Notes

### Hosts & metrics
- Remote hosts run the Rust agent (`agent/`), polled over any address the
  cockpit can reach — Tailscale, a LAN, a VPN. A **paired** host (#448) is
  dialled over HTTPS pinned to its agent's certificate; an **unpaired** host is
  dialled over plain HTTP, and only when its address is loopback or Tailscale —
  anywhere else the cockpit sends nothing until the host is paired (#449).
- **The agent measures a GPU on a Mac and on an NVIDIA host** — IOKit through
  `crates/accelerator` (the cockpit's own reader, so a Mac host's remote card
  matches its local one) and `nvidia-smi` respectively, on a probe task off the
  1s sample path. It only knew `nvidia-smi` before 0.5.1, so every Mac host's
  Graphics card read `—`. `crates/accelerator` must not reference a symbol newer
  than the agent's macOS 11.0 floor: `kIOMainPortDefault` is 12.0, so the walk
  passes its value, `0`, instead — a check `vtool` cannot make.
- **A Mac's RAM alert follows the kernel's memory-pressure level, not used ÷
  total (#544).** macOS keeps RAM full on purpose, so `used ÷ total` left every
  healthy Mac amber. `crates/mempressure` (modelled on `crates/thermal`: one OS
  query shared by `crates/localhost` and `agent/`, no sampler dependency) reads
  `sysctlbyname("kern.memorystatus_vm_pressure_level")` — `1`/`2`/`4` map to
  normal/warning/critical, anything else or a failed read is `None` — and the
  wire carries it as `memory.pressureLevel`, an `Option<i64>` (`0`/`1`/`2`, the
  thermal encoding) omitted when unknown. **The absent key is the whole version
  check** (#378's precedent; the agent's wire-contract marker moved to 0.6.0 for
  it). `memory.pressure` (the percentage) is deliberately **not** filled from the
  level: they are different claims. Where the level is known the RAM colour and
  the host's attention follow it and the RAM thresholds do not apply; where it is
  unknown (Linux, Windows, an older agent) `used ÷ total` against the thresholds
  is unchanged. The meter's fill stays used ÷ total everywhere. The rule lives in
  `viewmodel::card` and is re-applied with the operator's limits by
  `machine_alerts::apply`, which reads the card's `memPressureLevel`.
- **Thermal pressure is measured on local and remote Macs** through the shared
  `crates/thermal` reader (`NSProcessInfo.thermalState`, available since macOS
  10.10.3, below the agent's 11.0 floor). The agent reads it each sample;
  unsupported platforms and unknown future OS states omit `cpu.thermalState`.
  Linux temperatures are not converted into a guessed pressure level. Machines
  Detail renders an unavailable state as `—`; the full host card omits its
  badge. A remote Mac needs the updated agent to begin reporting this field.
- **A host is pinned to its agent's certificate, or it is plain HTTP — never
  both, and never a fallback (#448).** `Host.tls_fingerprint` (`store.json`,
  non-secret, canonical form from `crates/certpin`) makes `Host::base_url()`
  `https://`; `None` is `http://`, so a store from before pairing polls
  exactly as it did — **on loopback or Tailscale only (#449):** the unpaired
  client (`AgentClient::new`) is built with the guard's resolver, refuses an IP
  literal that fails the rule before opening a socket, follows no redirects and
  uses no proxy (a proxy would receive the token and resolve the name where the
  guard cannot see it), and reports the refusal as `AgentError::PlainHttpRefused`,
  whose `error.kind` (`plain-http-refused`) — like the two pairing kinds — the
  dashboard paints as its own state and does not count toward "All remote hosts
  unreachable": nothing was tried, so nothing was learned about the network.
  The refusal carries its evidence (#461) — `AgentError::PlainHttpRefused(Refused)`
  holds the looked-up name and only the addresses the guard refused — and the
  shell logs one `eprintln!` line naming the host and those addresses on the
  poll that *enters* the refused state, never on later refused polls, never with
  the token (`refusal_transition` in `app/src-tauri/src/main.rs`); the card's
  words are unchanged. `AgentClient::pinned` accepts **exactly one certificate**
  (SHA-256 of the end-entity DER; a chain is refused) with no system roots and
  no hostname check — the agent's SANs are fixed at first start, so the
  certificate is the identity, not the name — and still verifies the handshake
  signature with that certificate's key, so a copy without the key fails. It
  all happens inside the handshake, so an impostor never sees the
  `Authorization` header. `https_only` plus no redirects means a pinned host is
  never dialled over `http://`, including after a failure (a `302` to `http://`
  is a downgrade the client would otherwise take itself). The rustls provider
  is `ring`, passed to the config builder explicitly — the one backend the
  workspace already resolves — never `aws-lc-rs`, whose C/cmake build the
  macOS and Windows cockpit builds should not gain. **Pairing is
  confirm-on-add**: `agentclient::probe_certificate` fetches the certificate
  without trusting it (one unauthenticated request; it also falls back to one
  plain-HTTP request to tell "no TLS" from "nothing there", which is allowed
  only because no pin exists yet), Settings shows the fingerprint, and only the
  **Trust** click puts one in the arguments of Add / Save. A probe alone
  persists nothing; an untrusted found certificate holds Add back; editing the
  address drops the answer. The one other way a pin is set is
  `solador-app host pair` (next bullet), and it keeps the same rule: the app
  pins only a fingerprint it found itself *and* the request named. Two `Fault` states — `CertificateChanged` and
  `NoTls` — are their own sentences and never `Unreachable`; the host card's
  `error.kind` tags them so the dashboard's Machines row reads `Cert changed` /
  `No TLS`, and **neither counts as "the network is down"** for the
  all-remote-hosts-unreachable hint (the machine answered, and the hint asks
  about the VPN). **Re-pair** (Settings → the host) appears only when the
  certificate changed and replaces the pin through the same probe → Trust flow.
  There is no unpin: to go back to plain HTTP, delete the host and add it again.
- **`solador-app host …` asks the running app; it never writes the store or the
  keychain itself.**
  The app's executable in a command mode (`crates/hostsetup`), dispatched on
  `main()`'s first line — before `open_store`, whose migration writes the file —
  when argv[1] is `host`/`help`/`--help`/`-h` or argv[0] is named `solador`
  (the documented symlink; on Windows `solador.exe`, a copy or hard link of
  `solador-app.exe`, since `file_stem` ignores the extension). Every request
  goes over the hand-off channel to the
  open instance (`src/handoff.rs`) — `<store dir>/run/cli.sock` on Unix, a
  named pipe on Windows (below) — which writes through `add_host_connection` /
  `save_host_connection` — the Settings form's own functions — and reloads, so
  the app stays the **only writer** of `store.json` and `secrets_v1` (no
  keychain prompts from a second binary, no clobbered store). Not running is
  exit `4` and a sentence; there is no fallback writer, by design. On Unix the
  socket's directory is `0700`, both sides refuse it otherwise, and the server
  checks the peer's uid. **On Windows the channel is a named pipe**
  (`crates/hostsetup/src/channel/windows.rs`; `channel/unix.rs` is the socket,
  and `channel.rs` the shared interface, error type and one-line exchange —
  `protocol`, `pairing` and `server` do not know which they are on): the name is
  `\\.\pipe\solador-handoff-<FNV-1a hash of the store dir>` (not a secret; the
  directory is first made absolute and normalised lexically — separators to `\`,
  trailing ones dropped but a drive root kept, case folded — so spellings of one
  directory share a pipe; junctions and 8.3 names are not resolved),
  created with an **owner-only DACL** (`D:P(A;;GA;;;<this user's SID>)`), as the
  *first* instance (`FILE_FLAG_FIRST_PIPE_INSTANCE`, so a process that took the
  name first, or a second Solador, makes the app's create fail — one log line,
  the window carries on, as for a socket bind failure) and with remote clients
  rejected. The server resolves each client's user SID
  (`GetNamedPipeClientProcessId` → process token → `TokenUser`) and drops one that
  is not this process's; **the command mode does the same to the server before it
  sends anything**, because a request carries a token. The decision is the pure
  `channel::admit_sid` (tested on every platform, with a negative control), the
  Win32 calls a thin layer around it. **No reply carries a token** on either. `host pair` probes the address
  itself, pins only if that equals `--fingerprint`, proves the token over the
  pinned connection (`/v1/health`) before saving, and reads the token from
  stdin only. `hostsetup::pairing::decide` refuses a second row for one machine
  (same address, paired or not, or the same certificate elsewhere) unless
  `--replace`, and `Pairing::confirm` asks it again under the store's lock
  just before the write, so a Settings edit during the network checks is
  refused rather than duplicated or overwritten. Both channels are exercised by
  tests in `./dev test` on macOS and by the `Windows workspace tests` job on
  Windows; **`host add --ssh` against a real Windows `ssh.exe` has not been
  observed** (CI cannot), so what the code assumes — `ssh -G` output, `host.sh`
  on stdin, `collect`'s token line, `BatchMode`/`StrictHostKeyChecking` — is
  unverified there. `host.sh` is `eol=lf` in `.gitattributes`: a CRLF checkout
  would embed it with a `\r` on every line.
- **`host add --ssh` is `host pair` with the fingerprint and token read over
  ssh, and it changes the host only behind a flag.** `hostsetup::remote` sends
  `crates/hostsetup/remote/host.sh` (embedded, `include_str!`) on ssh's stdin as
  `ssh -o BatchMode=yes -o StrictHostKeyChecking=yes -T -- <target> /bin/sh -s
  -- <mode>`: `discover` (never the token), `rebind`, `install`, and `collect`,
  the one mode that prints the token, captured and never shown. The pure
  `remote::plan` decides from `discover` what must change for the address the
  app will dial (default `ssh -G`'s `HostName`; `--address` behind a
  ProxyJump) and refuses, with nothing changed, anything a flag does not
  allow: `--install` (bootstrap.sh with stdin closed, so the host generates its
  token), `--bind-all` (the "LAN too" rewrite and restart; the only way the
  command itself asks for `0.0.0.0` — a plain `--install` on a host with no
  tailnet gets the installer's own provisional wildcard, which `plan` notes
  first), `--enable-tls` (an installer re-run). It **never
  removes `SOLADOR_AGENT_REQUIRE_TAILNET`**, never rotates a token, never
  shadows another user's agent, installs no packages and never runs `sudo`: a
  system daemon's restart is printed with exit `3`. The installer runs with the
  bind/port/TLS/tailnet/release/launchd-label variables unset, so a login rc file cannot
  decide them. The app is asked first (exit `4` before any ssh), the agent's
  fingerprint is sent as `expectedFingerprint`, and the app's own probe still
  has to match it. **After a step, only a TLS answer at the address ends the
  wait** (15 s, `remote::SETTLE`): an agent that never gives one is its own
  failure, the change named and no add sent. Every failure after a step says
  what changed on the host, and ssh dropping (255) during a step that changes
  it says the host *may* have changed, never "could not reach". Text from the
  host is made printable before it reaches the terminal, and ssh gives up on
  a silent host (`ConnectTimeout`, `ServerAlive*`). `tests/ssh_flow.rs` runs
  the script under a stand-in ssh with `env -i` and every host command stubbed
  first on PATH, against a real hand-off server; nothing there reaches a real service manager or agent.
  `host.sh` restates where `install.sh` puts an agent and how `lib.sh` reads
  the env file, so `agent/deploy/lib_test.sh` holds it to both: the path and
  label lines word for word, `env_value` against `lib.sh`'s over one fixture,
  and the tailnet lookup run beside `install.sh`'s `detect_tailscale_ip` under
  `install.sh`'s own `pipefail`, which is what decides that a CLI that fails
  is no answer. CI selects that suite for a change on either side.
- **"Reach it on the LAN and the tailnet" is configuration, not code.**
  `SOLADOR_AGENT_BIND` is one address (`resolve_bind_host`; a list fails to
  bind), and `install.sh` writes a detected tailnet IP into the env file, so a
  cockpit off the tailnet reads `✗ not paired` at the host's LAN address and
  **Check certificate** finds nothing. The answer is TLS on,
  `SOLADOR_AGENT_BIND=0.0.0.0`, no `SOLADOR_AGENT_REQUIRE_TAILNET` (it refuses a
  wildcard: FATAL at start, a restart loop under the service manager; it is a
  deliberate tailnet-only policy, so its presence is a question for the
  operator, never a key to drop silently) and no
  `SOLADOR_AGENT_BIND_AUTO` (the next installer run would resolve back to the
  tailnet IP), a restart, then pairing the LAN address. The certificate is not
  regenerated and the pin checks no hostname, so existing pairings survive and
  the LAN IP need not be a SAN. `agent/README.md`'s **Reaching a Tailscale host
  from the LAN too** is the operator procedure. A real agent bound to `0.0.0.0`
  may be bound there on purpose: report it, never "fix" it.
- Agent endpoints: `GET /v1/snapshot` (CPU/mem/disk/net/gpu/battery),
  `GET /v1/containers`, `GET /v1/health`. All require `Authorization: Bearer <token>`.
- **`/v1/health`'s `version` is the agent's own version, and it is optional
  (#390, #490).** The agent is a published artifact with a release train of its
  own, so a release's binary carries that release's `YYYY.M.N` (`agent-v…`), and
  a source build carries `<base>+dev.<k>.g<sha>` — the Settings row prints
  either verbatim as `v{version}`. `agent/Cargo.toml`'s semver stopped naming a
  release and survives only as the wire-contract marker its comment block
  describes. A build made outside a full git checkout cannot count the commits
  since its base, so it **omits** the key — never `null`, never the crate semver
  as a stand-in — and the Settings row reads `agent version —`.
- **Each host's Settings row says whether its agent is behind the newest
  *verified* release (#489), without pressing Test.** `HostState.version` keeps
  the `version` of each successful `/v1/health` read on `sampler_stale`'s
  lifecycle (a failed read changes nothing; a successful one overwrites it,
  `None` included). `crates/agentrelease` fetches `agent-latest.json` and its
  `.minisig` from the `agent-latest` release **once at startup, then hourly**
  (`AGENT_RELEASE_CHECK_INTERVAL`, a fixed constant, not an operator setting)
  and verifies the exact bytes under the compiled-in **agent** keys before
  reading `version`; `viewmodel::agent_release` owns the eight states, their
  words and their colour. They are never alike: not checked, host version
  unknown (`—`), source build (`+dev`), version that is not a CalVer, check
  failed (with the classified sentence), no agent release published yet (a 404
  on `agent-latest`, true until the first `agent-v*` release — neutral, never a
  failure), up to date, behind. **Only *behind* is amber and nothing is ever
  red**: a host that is behind is healthy, and on an org host run at a pinned
  version it is the expected state between re-pins, so the sentence says a
  newer release *exists* (`2026.11.2 available`) and never that the host should
  have moved. CalVer compares numerically `(year, month, n)`, never through
  `update::is_newer`'s semver. The line reaches the row through
  `settings_payload` → `StoreSections` → `hosts_tab`, as `agentRelease`
  (`state`, `text`, `color`), and `app/ui/settings.js` paints it and nothing more.
- **Unknown is representable.** Every metric a producer may not be able to
  measure (memory used/swap/pressure/pressureLevel, thermal state, the GPU fields,
  disk/network rates, `processes[].cpuCores`) is an `Option` in `crates/wire`:
  an absent key decodes to `None`, `None` re-encodes as an *omitted* key, and
  `0` means measured zero.
  Unmeasured samples never enter a history buffer. Known limit: agents
  predating #183 send literal zeros, which decode as measurements — only the
  agent can fix that.
- **An added `Option` is also how the wire carries a version (#378).**
  `processes[].cpuCores` is a core count and the only per-process CPU figure the
  Hosts card renders; `cpuPercent` stays beside it, unredefined. An agent
  predating the fix omits the new key and its `cpuPercent` is inflated 60× on
  Linux (a 60s numerator over a 1s denominator, from sharing one sysinfo
  `System` between the two sampler cadences), so `None` renders `—` rather than
  `cpuPercent / 100`. **The absent key is the whole version check** — no version
  parsing, no extra `/v1/health` request, fails closed. The agent's fix is
  structural (`SamplerSystems` gives the process refresh its own `System`),
  never a correction constant, because the arithmetic being corrected is a
  sysinfo internal. `crates/localhost` has the same two-cadence shape and is not
  wrong only because macOS and Windows take the denominator from the process
  refresh; the comment on `PROCESS_SAMPLE_INTERVAL` records that.
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
- **The rules are edited in a Containers tile's Configure (#613)**, under
  **Container Group Rules**, not in Settings. They are shared by every
  Containers tile and the Full view, so the section says so and saves on the
  change (`container_rule_add` / `_set` / `_remove`), never with the tile's
  Apply.

### CI & usage data
- GitHub Actions data: `crates/github` (workflow health, self-hosted runners,
  remote branch / open-issue / open-PR counts).
- Claude Code usage rollups: `crates/usage` (tokens only — USD is computed and
  unit-tested but never displayed, since the account is subscription-based).
- Neon, Sentry, Vercel consumption: `crates/usage`. Own 1h cadence, not the
  Claude rollups'; render `—` when the key is missing or the API fails.
- Claude and Codex plan limits: `crates/subscription`, one task
  (`plan_usage_loop` in `app/src-tauri/src/main.rs`) on its own cadence
  (`panel_intervals.plan_usage`, 15 min by default, floor 5 min). Each provider
  keeps its own schedule (a 429's `Retry-After`, capped at a day, pushes only
  that provider back) and a generation counter, so a reading that lands after
  its toggle flipped is dropped rather than painted under the new setting. A
  provider that is off is never read and never woken.
- **Eight cadences are the operator's, not constants**: containers, GitHub
  (Repos and Runners), Services, the Claude rollups, the metered providers,
  Azure cost, Sentry crons and the plan limits are `store.json`'s `panel_intervals`,
  each clamped to a floor stated beside the source it protects
  (`PanelInterval::spec`). Today's constants are their *defaults*, so an
  unconfigured store polls exactly as it always did. Every loop re-reads its
  cadence **after each pass** through `panel_interval` in
  `app/src-tauri/src/main.rs` — capturing it at spawn is what would make a
  change take up to one full interval (four hours, on Azure) to apply. Each is
  edited in the **Configure of the tile(s) it feeds** (#614; it was Settings →
  Preferences → **Panel Poll Cadence**, #337) — Containers, GitHub Repos *and*
  Runners (one `github` cadence, one key, on both tiles: `cadence::source_ids`
  maps a panel to a list of sources), Service Health, Usage (three: the Claude
  rollups, the metered providers and the plan limits), Azure Cost and Scheduled
  Jobs (Sentry crons) — under **Poll cadence**, one control per cadence (a
  source carries a `cadences` list, so a tile fed by three polls shows all
  three), which says it is shared by the tiles it names and the Full view and saves on the click (`panel_interval_save` / `panel_interval_clear`,
  answering with the re-rendered dashboard), not with the tile's Apply. It shows
  its floor *and why that floor exists* (`app/src-tauri/src/cadence.rs`), and a
  below-floor value is **refused with that reason** rather than clamped to a
  number the operator did not choose. `set_panel_interval_secs` still clamps and
  `panel_interval_secs` still clamps on read, because a hand-edited `store.json`
  reaches neither gate. Settings carries none of them.
  **The GitHub, Services and Claude-rollup cadences were one setting until
  #615** — the shared Refresh Interval (30s / 1m / 5m), which drove
  `github_loop` (Repos, Runners, and the Services pass inside it) and
  `usage_loop`'s outer sleep. Each defaults to that interval's 60s with a 30s
  floor, so an unconfigured store polls as it did. A store that had chosen 30s
  or 5m is carried over once: on load, `Settings::seed_cadences_from_legacy_refresh_interval`
  copies a non-default `refresh_interval_secs` into each of the three keys not
  already set (a stored 60
  is "never chosen" and seeds nothing), then empties the field, which is never
  serialised — so the first save drops the key and a cadence cleared afterwards
  is not seeded again. The GitHub, Services and Claude-rollup footers' staleness windows follow
  their cadence (`panel::stale_after_for_cadence`, 2.5 cycles, never under 150s)
  rather than a flat 150s; Containers, Azure Cost and the plan limits have their
  own rules (see the footer list under Services in `docs/PANELS.md`).
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
  **Actions**, **Contents**, **Issues**, and **Pull requests**, plus the
  *organization* permission **Projects** (read) for the READY column — without
  it that one column reads `—` and everything else is unaffected.
- **Remote hosts**: per-host bearer token, plus — once paired (#448) — the
  pinned certificate fingerprint, which is public and lives in `store.json`.
- **Usage → Claude** (the rollups): no credential — and **no account either**. The rollups
  are a walk of `~/.claude/projects`, and those logs record what was consumed,
  never who paid for it: a full key survey of a real session file — 50+
  top-level fields including `cwd`, `gitBranch`, `sessionId`, `version`,
  `userType`, `requestId`, `messageId` — carries no `account`, `organization`,
  `email`, `plan`, `tier` or API-key field of any kind. So the panel's numbers
  are **one machine-local aggregate**, and two subscriptions used on one machine
  are *inherently* unseparable from this source — impossible, not
  unimplemented; per-subscription attribution needs a source that knows who
  paid. Anthropic **API keys** would be `VendorAccount`s and their usage *would
  be* attributable — but that integration **has not been built**: the
  vendor-accounts epic (#283) closed with `VendorKind::GitHub` as its only
  variant, and nothing currently tracks adding Anthropic. **A subscription is
  not an account
  and must not be modelled as one**: an account id on these rollups would be an
  attribution invented to fill a gap — the unknown-as-zero error in another
  costume. If a number cannot be attributed, say so rather than attributing it.
- **Usage → Claude and Codex plan limits**: **another program's credential,
  read only while the operator has opted in** (`store.json`'s
  `claude_plan_usage_enabled` / `codex_plan_usage_enabled`, both `false` in a
  fresh store and on any store written before them; Settings → Connections →
  **Claude plan limits** / **Codex plan limits**, saved on the click like the
  crash-reporting toggle). Claude: the macOS keychain item `Claude
  Code-credentials` (account `$USER`), read by `/usr/bin/security`, else
  `~/.claude/.credentials.json`; then `GET
  https://api.anthropic.com/api/oauth/usage`. Codex: `$CODEX_HOME/auth.json`,
  else `~/.codex/auth.json`; then `GET
  https://chatgpt.com/backend-api/wham/usage`. Both endpoints are
  undocumented. **Solador only reads**: it never writes, refreshes or rotates
  either sign-in, never starts `claude` or `codex` (Codex's RPC path can
  rewrite `auth.json`), and never copies a token into `crates/store::secrets`;
  the token lives in a redacting `AccessToken` (no `Display`, no `Clone`, a
  `Debug` that prints `AccessToken(<redacted>)`) for one request and is dropped. An expired
  sign-in is `Fault::SignInExpired` (`run it to refresh`), never
  *unreachable*. Reading the keychain item from another binary may make macOS
  ask (unverified: `subscription::keychain` explains why it cannot be); a refusal **parks** that provider until its toggle is turned off and on or
  the app restarts, so a refused prompt is never asked again on a timer.
  Windows and Linux have no keychain step: the file is the only source, and a
  Codex that keeps its sign-in in the OS keyring reads as not signed in. Like
  the rollups, the reading is machine-local: it is whichever account the tool
  is signed in as, and is not modelled as an account.
- **Usage → Neon**: an *organization* API key. The non-secret `org_id` is a
  normal preference, not a credential-store item.
- **Usage → Sentry** and **Sentry Crons**: the *same* read-only `org:read`
  token and org slug — one credential, two panels, so saving or clearing it
  wakes both poll loops or one keeps describing the previous token for an hour.
- All credentials live in the OS credential store (`crates/store::secrets`),
  never in `store.json`. On macOS they are consolidated into one item,
  `secrets_v1`, under service `app.solador.desktop`; see `app/README.md`'s
  "Consolidated credential item".
- **`LEGACY_APP_DIR_NAME` must not be renamed.** It names the config dir from
  before the rename, and is what the store migration reads *from*. The
  keychain side of that rename (adopting credentials from the pre-rename
  service) has been removed: nothing reads `com.sassydog.devcanopy` any more,
  so `SERVICE` itself is now the one name that must never move — a move
  orphans every stored credential at once, and orphaned is worse than
  deleted, because they stay there being useless.
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
- Overview Machines has numeric CPU/RAM meters, GPU and GPU-memory meters below
  them (never threshold-coloured; GPU memory always reads `VRAM`, and only when
  the producer sets `gpu.unifiedMemory` do RAM and VRAM carry a "Unified memory"
  tooltip; both totals are whole GB), and a full-width Disk strip for the fullest visible
  volume (`disk_volume` in `app/src-tauri/src/dashboard.rs`). **Hidden volumes**
  (`Host.hidden_volume_mounts` / `Settings.local_hidden_volume_mounts`) are
  filtered once per frame by the `cockpit` command's `apply_machine_preferences`
  (via `app/src-tauri/src/volumes.rs`, on `cockpit_view`'s payload), before the
  Hosts panel or the dashboard reads a card, so a hidden volume never
  colours the strip, names itself in the status or raises attention. The list's
  one home is the Machines tile's Configure → **Volumes shown** (rule 4: shared
  by every Machines tile, saved on the click); Hide on the tile's Disk strip and
  Hide / Unhide in Detail are shortcuts to the same list, all through
  `machine_set_volume_hidden`. A Machines tile's
  `showDisk: false` (Configure → **Disk strip**) drops the strip from that tile's
  rows only, in `tile_view`; status, attention and Detail are unchanged. Each
  machine renders as an inset card. GitHub tiles persist row limits
  (including All); Repos also persists selected repository identities and column
  sorting, and Runners supports OS + architecture groups, split by the runner
  group labels above. `viewmodel::dashboard` owns sorting/aggregation.
  Architecture is carried from GitHub's standard labels through the runner
  roster so absences retain their type, and every label is carried the same way
  so absences retain their group; unknown stays unknown.
  See `app/README.md` for controls, count meanings and responsive behavior.
- **Where a setting lives**: see `app/README.md`'s section of that name — what
  changes a tile goes in its Configure, what reaches a source in Settings →
  Connections, what is app-wide in Settings. A shared setting edited from a tile
  says it is shared. Its inventory names the follow-up (#616) for the
  settings that do not fit yet.
- The Machines tile's Configure → **Machine alerts** has the shared CPU/RAM alert
  thresholds and a per-machine override for each machine, including the local
  one, marked shared and saved on the click (`machine_save_alerts`, answering
  with the re-rendered dashboard; Settings carries neither). Defaults remain 70/90%.
  The CPU pair applies to **sustained** load (#545): the minimum of the last
  `CPU_SUSTAIN_SAMPLES` (60, about a minute) samples in `HostHistories.cpu`,
  carried on the card as `cpuSustainedFraction`, so a build or test run does
  not flap the row. With fewer samples than the window (just launched or just
  added; it counts successful samples, so an outage does not reset it) it is
  absent and the CPU raises no alert; the meter's fill and `%` stay the live
  sample. The RAM pair applies only to machines that do not report the
  kernel's memory-pressure level (a Mac does; see Hosts & metrics).
  `store::machine_alerts` owns persistence/validation; the shell applies the
  resolved limits to cached numeric fractions on every render, so meter colors
  and dashboard attention agree without restarting host pollers.
- Machine Details (Table/List) can acknowledge CPU/RAM warnings independently
  per stable host ID. `settings.machine_alerts.acknowledged_warnings` persists
  until explicitly cleared, even across recovery/critical episodes. Only warning
  severity is excluded from attention: colours remain truthful, critical always
  breaks through, and disk/thermal/connection problems are unaffected. The
  overview meter and Detail both mark an acknowledged warning with a muted ✓
  beside the value (tooltip), never a standing text line.
  `machine_acknowledge_warning` saves before rendering, rolls back on failure,
  and does not enter dashboard layout undo history. Host deletion removes its
  acknowledgements. Polls started before the save cannot overwrite its response.
- Attention chips toggle their Detail inspector. Detail defaults to compact
  tables, with a Table/List choice persisted per source in `detailViews`.
  Summary stays compact and Full keeps source-specific controls.
  Grouped runners open inline Detail, like Machines, filtered by the Rust-authored
  group identity each runner row carries (`groupId`, label split included);
  membership and counts refresh together.
  A tile's footer ends in a **View** button (`views` in the tile JSON,
  authored in `dashboard.rs`'s `TILE_VIEWS`) whose menu opens Detail or Full
  view; the frontend paints only the view ids its `VIEW_ACTIONS` can open, and
  an open menu is rebuilt with the footer on each repaint.
  Detail opened from a tile (View, `N more`, a row or a runner group) offers
  **Full view**, the tile's Full view: its source's panel holding only the tile's
  rows (the panel command takes the tile id and `tile_scope` filters, never
  inserting a key the payload lacks; Machines cards before layout, while `No
  hosts configured` still counts the configured hosts, not the kept cards),
  titled with the tile's name, with Rust's `fullNote` under the header row when
  the scope filters (`Scope: …`, plus the totals clause on every source but
  Machines, which has no totals). It keeps the selected group, and Full's
  All runners clears that filter. Escape or the toolbar toggle returns to the
  overview, focused on that tile's View button; a tile hidden or removed meanwhile sends the
  overview back with `tileGone` and closes the Detail opened from it. The
  overview's 1s read keeps running in a tile's Full view only to notice that,
  and repaints nothing there but the tile's heading: `updateView` runs on a
  forced render, so the open panel is not rebuilt every second. Detail from an
  attention chip has no tile and opens the whole source.
  Full Runners shares the same Table/List preference and table styling. Detail
  columns have fixed widths so live readings cannot shift their neighbors.
  Machine Detail volumes sort by mount and align used/total/percentage, each
  with Hide, and hidden mounts listed beneath with Unhide; Full
  host cards retain their fullest-first order. Overview and Full view share one
  compact wrapping toolbar and Solador heading without a background frame or
  divider. Full view is a pressed-state toggle; Fit window to tiles, Edit
  dashboard and Settings use the same icons in both views, with Add tile and
  Undo icons during editing. Fit window to tiles sends `document.body`'s
  measured height and the viewport's to the `window_fit_height` command
  (`src/window_fit.rs`), which moves the window's height by the difference (a
  relative resize, because macOS Tauri counts the title bar in `inner_size`)
  and keeps its width; no `core:window:*`
  permission is granted, so the frontend never resizes the window itself.
  Tooltips appear on hover or keyboard focus. Dashboard action successes have
  no visible footer; failures stay beside their controls, and reading failures
  beside Needs Attention. About puts update controls above naturally flowing
  status/help/notes without nested scrolling. Notes format headings, lists,
  emphasis and code through DOM nodes; HTML stays text, and URLs stay selectable.
  Settings calls the full cockpit arrangement **Full view layout** and keeps a 12px vertical/
  16px horizontal header inset.

### Error vocabulary (`crates/fault`)

3. Handle errors gracefully with user-friendly messages; `user_message()` on
   error types is the repo-wide convention. **Error enums classify; `crates/fault`
   owns the words** (#352, #354): every state the `Fault` vocabulary names
   renders `Fault::message(vendor)`, with the one thing only that crate knows
   appended — a stock sentence is the floor a message may not fall below, never
   a cap on how specific one may be. A `self.to_string()` fallback is how a
   transport error, a request URL and a storage path each reached a panel
   footer, so `user_message()` writes out every arm. Reading 401/403/404 as
   *account* states is true of an org-scoped read and false of a host agent's
   endpoint or a public status page, so that classifier is **private** to
   `crates/fault` (#364) — a caller with no sharper reading takes
   `fault::http_status_message`, which classifies only the two status classes
   that mean the same thing everywhere. The vendor slot is an untyped `&str`
   carrying two grammatical categories, and `Fault::ToolUnavailable` and
   `Fault::SignInExpired` are the two sentences that need a program's name
   rather than `"that host"` (one says to install it, the other to run it).
