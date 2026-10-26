# Solador panels

> Moved verbatim from `AGENTS.md` (#638), which is held under the org's 32 KiB guidance budget. Edit it here; `AGENTS.md` points at this file.

### The panels

- **Hosts** — this machine's card plus one per configured agent, in a
  width-aware grid (1s poll).
- **Containers/VMs** — local docker/podman/tart plus every host's agent, with
  grouping rules and presence memory (10s by default).
- **GitHub Repos + GitHub Runners** — per-repo CI health and counts (open
  issues, the board's **Ready** backlog, open PRs — the three also ride on the
  compact dashboard's repo rows in every presentation), local branch/worktree
  counts, self-hosted runners with the absence roster (one poll, one cadence:
  `PanelInterval::GitHub`, edited from either tile's Configure, #615). READY is a GraphQL read (`crates/github::ready`),
  issue-side so no board identity lives in config; it needs the PAT's org
  Projects permission and renders `—`, never `0`, without it — and, alone
  among the four counts, says why in the panel footer (`ready_error`). Open PRs
  are a walk of `/pulls` (100 per page, refused past `PULLS_PAGE_CAP` rather
  than truncated), not the `per_page=1` trick, because the walk also yields how
  many Dependabot opened: the PRs count's tooltip (`7 open PRs · 5 from
  Dependabot`) and a **DEPENDABOT** column after PRS in Repos Detail.
  The org's runners are walked the same way (`org_runners`: 100 per page,
  `Link: rel="next"`, de-duplicated by `id`, refused past `RUNNERS_PAGE_CAP`
  pages with `TooManyToCount`): the roster reads a fetch as the complete
  registered set, so a partial list would walk every runner beyond it to
  `missing`. A refused walk takes the failure path, rows and absence clocks
  frozen, and its footer says so (`runners_error_message`) instead of the
  PAT-scope hint.
  Runner groups are OS · architecture, split by the operator's **runner group
  labels** (`store.json`'s `runner_group_labels`, edited in a Runners tile's
  Configure and marked shared — see **Where a setting lives** in
  `app/README.md`). **An OS · architecture splits only where a listed label
  appears**: into one group per listed label a runner carries, plus `other`;
  one where no runner carries any stays a plain `group:OS:ARCH` with no
  `· other`. That split is decided once across every org's runners and
  remembered absences (`split_platforms`), never per tile scope. A runner
  joins the group of the first listed label it carries (ASCII
  case-insensitive, as GitHub matches `runs-on`), one carrying none is
  `other`, and so is one whose labels were never recorded — a roster entry
  from before labels were remembered — which keeps its alarm and reads
  `— not recorded` in the Labels column Detail and Full show. Nothing is
  seeded, for the container rules' reason. `viewmodel::dashboard::runner_group`
  decides a row's group once, in `runner_row`, and every view reads its
  `groupId`/`groupLabel`/`groupRank` rather than re-deriving it — which is
  what keeps a group opening onto the runners it counted. A group's colour is
  its load, busy ÷ online (`group_load_color`, value `2/3 busy`): gray idle,
  green, amber from half busy, red once no runner is free — all busy, or none
  online. Saturation is colour only; a missing runner is still the one thing
  that raises attention, and a group holding one leads with it (value
  `1 missing`, red) rather than a load figure, which moves to the line under
  it. Individual rows keep their state colours.
- **Usage** — Claude token rollups (their own `PanelInterval::ClaudeRollups`, #615); the Claude and Codex
  **plans' own limit windows** (`CLAUDE PLAN (5H)`, `(WEEK)`, a per-model
  weekly such as `(WEEK · FABLE)`, each with its reset countdown and a bar
  coloured like the CPU/RAM meters, 70/90%), **opt-in per provider and off in
  a fresh store** (15 min by default, see `docs/APP-INTERNALS.md`, **Authentication**); Neon, Sentry +
  Vercel (hourly by default). Vercel reads the FOCUS billing export: month-to-date spend and what
  falls beyond the plan. Neon renders compute/storage MTD, `NEON EST. CHARGES
  (MTD)` from operator-entered rates, and a best-effort `NEON LAST INVOICE` off
  an undocumented endpoint — that one failing degrades to `—` plus a footer,
  never the consumption rows. Both plan endpoints are undocumented too, and
  degrade the same way: the window rows read `—` and the header names the
  reason. A plan that is off is one muted setup line for every plan that is off
  (`Claude and Codex plan limits are off — turn on in Settings`), never rows of
  `—`, and adds nothing to the dashboard. Plan rows are their own block
  (`crates/subscription`, `app/src-tauri/src/plan_usage.rs`): a plan is not an
  account, and its figures never merge into the rollups.
- **Azure Cost** — the daily cost export, on its own cadence (4h by default).
- **Sentry Crons** — every cron monitor environment that is not `ok`, and **how
  long it has been broken** (its own cadence, 1h by default). The age comes from
  `activeIncident.startingTimestamp`, never from `lastCheckIn` — measuring from
  the last attempt is what made day 6 of an outage look identical to day 1, and
  on the monitor that motivated the panel the two read 7d 22h and 0d 22h. A
  check-in-derived age is a *different* enum variant and renders as the weaker
  claim it is (`≈`, amber, and a row that says why). Suppressed entries
  (`disabled`, or a strict `isMuted: true`, at either monitor or environment
  level) are counted and shown with their reason, never dropped; and a **blind
  read** — no monitors at all, or a monitor carrying no environments — is red,
  because an empty green panel is the failure this exists to remove.
- **Services** — third-party availability, on a loop and a cadence of its own
  (`services_loop`, `PanelInterval::Services`, #615; it used to ride the top of
  the GitHub pass), which runs with no GitHub token.
  Three transports behind one vocabulary (`crates/servicestatus`), and a change
  in either direction fires a desktop notification. **The watched list is
  derived from configuration, never shipped** (#284, wired up in #375):
  `services::active_vendors` answers it every pass from the credentials and
  accounts already in the cockpit — plus Claude rollups, which are Anthropic's
  only evidence — and appends the operator's own Atlassian Statuspages
  (`store.json`'s `status_vendors`). `poll_service_status` reads exactly that
  list through `read_vendor`, and `readings`/`view` render exactly that list, so
  a vendor cannot be polled and then dropped from the panel. A cockpit with
  nothing configured watches nothing and **says so** rather than painting an
  empty body under "all clear". `ServiceStatuses::watching` forgets the readings
  of a vendor that leaves the set, which is what stops a re-added vendor
  rendering the status it had when it left — the same rule `StatusWatch` applies
  to its baseline, and what keeps #297's seed-don't-alert true. Claude rollups
  appearing or disappearing is an input no edit makes, so a usage pass that
  sees `claude_present()` change wakes the Services loop
  (`claude_presence_wakes_services`, #627) — and only then. The panel carries a
  **staleness footer** on `ServiceStatuses`' last-success clock against
  `stale_after_for_cadence` of the Services cadence. A pass succeeds when it
  watched nothing or read at least one vendor (one vendor down is its own row);
  a pass that watched vendors and read none keeps the clock and says so with
  `last ok`. Before any pass there is the loading line, never an age. The
  windows cached in panel state (GitHub, Claude rollups, Services) are set from
  the store by `sync_cadences` both at the top of each pass and in
  `panel_interval_save`/`_clear` before they re-render, so a shortened cadence
  moves the window with the save and not one pass later. Every other
  cadence-following footer reads its cadence at render time, so a save already
  moves it: Containers (2.5 cycles, never under 30s; the shared 150s floor would
  loosen the 10s default five-fold, #633), Azure Cost (cadence + 1h) and the plan limits (twice the
  cadence). Sentry Crons and the metered Usage providers (Neon, Sentry, Vercel)
  read it at render time too, each its own (`PanelInterval::Crons` and
  `UsageProviders`): the window is 1.5 cycles
  (`panel::stale_after_for_slow_cadence`; 90 minutes at the 1h default, 7.5
  minutes at the 300s floor), so a healthy loop is not stale between polls
  (the usage loop sleeps `min(rollups cadence, time until the providers are
  due)` and stamps each provider as its read completes) and a stuck one is
  flagged within two cycles (#635).
- **OpenClaw** — an agent farm over a live WebSocket. **Event-driven, on no
  cadence at all**, which is why it is the one panel with no staleness signal of
  any kind (the polling panels carry a `status_footer`; Hosts warns per card,
  via `sampler_stale` and unreachable).

Rows are reflowed by `viewmodel::cockpit` for the measured width, and every card
in a row is the same height. An in-app Settings surface over `crates/store`
(hosts CRUD, portfolio, credentials, cockpit layout, general prefs; the
container group rules are edited from a Containers tile's Configure, #613) applies changes without a restart. The main window also remembers
its size, position and maximized state across normal launches through the
Rust-only window-state module (logical points on macOS, the window-state plugin
on Windows); see app/README.md for storage and the isolated
native smoke check.

The Tauri IPC boundary itself is **not** automatically tested — `app/README.md`
carries a five-minute manual smoke checklist, plus a dated observation log, and
that is the only thing covering it. This is a **decision, not a gap left open**:
#123 closed on it deliberately, because `tauri-driver` has no macOS support
(WKWebView ships no WebDriver), so the only automatable host would be the
Windows CI job — an oversized harness covering one build path. A test claiming
coverage it does not have would be worse than the checklist.
