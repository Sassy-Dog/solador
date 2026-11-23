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
  Dependabot`) and a **DEP** column after PRS in every Repos layout (the
  overview tile, Detail and the Full view).
  Repos are **grouped by owner**, the part of `owner/name` before the slash
  (compared case-insensitively, labelled with its first spelling in
  owner/name order, never defaulted), decided once in `viewmodel::repo_owners`.
  Every row carries its group's label (`github::attach_owners`), so `Acme/x`
  and `acme/z` sit under one owner in the tile, Detail and the Full view. A Repos tile's
  Configure → **Layout** (`repoLayout`, `cards` by default and for a store
  written before it) chooses **Cards**, one box per owner, or **Sections**, one
  table with a header row per owner; the tile and its Full view both follow it,
  and Detail lists each owner's repos under its name. Each owner's header
  carries its totals: issues, PRs, Dependabot and how many workflows are
  running, over every repo **in the tile's scope**, the ones past the row limit
  included. A total that includes an unknown count is unknown and reads `—`.
  The tile's row limit applies **per owner**, each owner keeping its own
  "N more" line. In Cards, owners that fit share a row; one showing more than
  `CARD_SPAN_ROWS` (8) rows spans the width and splits its rows into columns.
  When any card or split column the tile paints is narrower than a small
  tile, every card in that tile keeps ISSUES, PRS and DEP only, so the columns
  line up (its dot still carries the status). The Full view's cards show REPO, ISSUES, PRS,
  DEP, JOBS and LONGEST, as many cards across as `cockpit::repo_cards` allows
  at the window's width (a second card only when each gets 320pt, at most 4), dropping LONGEST
  below 380pt and JOBS below 300pt; its Sections keep every column and repeat
  an owner's header at the top of a column that continues it.
  The org's runners are walked the same way (`org_runners`: 100 per page,
  `Link: rel="next"`, de-duplicated by `id`, refused past `RUNNERS_PAGE_CAP`
  pages with `TooManyToCount`): the roster reads a fetch as the complete
  registered set, so a partial list would walk every runner beyond it to
  `missing`. A refused walk takes the failure path, rows and absence clocks
  frozen, and its footer says so (`runners_error_message`) instead of the
  PAT-scope hint.
  Runner groups are OS · architecture, split by the operator's **runner group
  labels** (`store.json`'s `runner_group_labels`, edited in a GitHub Runners tile's
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
- **AI Usage** — every AI vendor in one tile (`aiUsage`, command `ai_usage`,
  `usage::ai_usage_view`). The **Anthropic** and **OpenAI** sources in Settings
  both feed it, each with its own Admin key, so a part is set up in the source
  that owns it and a source adds the one tile, never two. The body reads, in this
  order: the **plan limits** (Claude's, then Codex's), the **API spend** (`apis`,
  Anthropic's then OpenAI's), then this machine's Claude Code rollups last.
  Plans are **opt-in and off in a fresh store** (15 min by default, see
  `docs/APP-INTERNALS.md`, **Authentication**).
  A plan is one row named by its provider and plan (`Claude · Ultra`,
  `Codex · Plus`; the name is the one the tool itself displays, never mapped from
  a tier code) carrying one meter per limit: `5h`, `Week`, a per-model weekly
  such as `Week · Fable`, or, for an Enterprise seat with no windows, a `Budget`
  (`$312 / $500 · 62%`; spend beside windows is `Extra usage`). The meters are the
  Machines tile's CPU/RAM meters (`app/ui/meter.js`, 70/90%). A meter exists only
  for a window the reply carries: a verified Ultra account has no 5-hour window,
  so it has no `5h` meter. Each window also gets a **pace**: the average burn
  since the window opened, judged at the reading's own time (never at render),
  shown as a tick on the track once a tenth of the window has passed and hidden
  once the reading is stale. The row's verdict is the tightest one: `limit
  reached` (red, the only plan state that raises attention, and the chip reads
  `AI Usage · N`), else the earliest run-out (`out in ~1d 1h`, amber), else the
  busiest window's reset, and `reset since read` when every window rolled over
  after its read; never blank. A spend cap has no pace: the reply states no reset
  for it. Under `THIS MACHINE`, Claude Code's token rollups (`LAST 5H`, `LAST
  7D`, top projects; their own `PanelInterval::ClaudeRollups`, #615), which the
  tile folds into one `Claude Code` row. The header's figure is labelled, because
  the tile covers several vendors: `trailing` is `Claude Code · 1.2M today` and
  `claudeToday` carries the bare `1.2M today` for that row. The plan endpoints
  are undocumented, and degrade like any best-effort read: the plan row reads `—`
  with no meters and the header names the reason. **API spend** is each vendor's
  `Anthropic API spend (MTD)` / `OpenAI API spend (MTD)` from its Admin key, on
  `PanelInterval::UsageProviders`, with a note saying how far the figure reaches
  (`counted 3h ago`), which is not when it was read: Anthropic's lag by hours.
  `apis` is always a list, empty when neither key is there. The header's warning
  and clock lines read in body order and each names its row's own id
  (`claude-plan`, `codex-plan`, `anthropic-api`, `openai-api`, and the rollups'
  `claude-today`, last), so a line about one vendor is never read as another's.
  `loading` is the OR of every enabled part's. **A part that is not set up says
  nothing**: no row, no warning, no setup line, no walk of the logs, and no
  `Claude Code` row on the tile. Only **no part at all** paints the one sentence
  (`No AI usage yet — add Anthropic or OpenAI in Settings → Sources → Add
  source`, built from `settings::AI_SOURCE_TITLES`), and only once **every** key
  read is `Absent`: any key still unread keeps the tile loading, and otherwise a
  key that could not be read says why, first in body order, instead of the setup
  sentence. Plans are their own block (`crates/subscription`,
  `app/src-tauri/src/plan_usage.rs`, `crates/viewmodel/src/plan_usage.rs`): a plan
  is not an account, and its figures never merge into the rollups.
  *Scopes.* Besides `all` and `attention`, a tile can be narrowed to `plans` (both
  plans), `claude` (the Claude Code row and its rollups), `anthropic` or `openai`
  (that vendor's plan, spend and, for Anthropic, the Claude Code row), or one row
  (`item:<id>`). The row ids are unchanged from the two tiles this one replaced,
  so a saved `item:` scope keeps working (`dashboard::vendor_of` maps each row to
  its vendor scope). A tile that does not keep `claude-today` also drops the
  header's Claude Code figure. **A tile's scope picks only which
  figures it shows.** Every `⚠` warning from a configured AI source (both plans,
  both keys, the Claude Code logs, and a key the credential store would not
  read) shows on every AI Usage tile, overview card and Full view alike, in body
  order and labelled with its row id (`claude-plan`, `codex-plan`,
  `anthropic-api`, `openai-api`, `claude-today`). A key whose read failed has no
  section, so it warns as `⚠ anthropic-api: <reason>`; a key merely unread warns
  of nothing, it is loading. Only the freshness clock, `loading` and the
  "isn’t set up" sentence follow the scope (an overview card's clock entry is the
  kept rows' clocks, the line its Full view paints). `all` and `attention` keep
  the whole-tile values. Any `⚠` line, a key the credential store would not read
  included, lights the amber "AI Usage · check readings" Needs Attention chip;
  only red plan rows raise a red one. Every other scope looks at its **relevant set**
  (`anthropic`: the Anthropic vendor, meaning its plan, key and logs; `claude`
  and `item:claude-today`: the logs; `item:claude-plan`, `item:codex-plan`: that
  plan; `item:anthropic-api`, `item:openai-api`: that key; `openai`: the OpenAI
  vendor; `plans`: both plans; an unknown `item:`: nothing, so no message and not
  loading). Its `freshness` carries only the relevant rows' clocks; it is
  loading when a relevant row is or a relevant key is unread; and its message is, in order: (1) the
  loading line if a relevant key is unread, else (2) the rollups' own line
  (`reading logs…`, the no-logs line) when the Claude Code logs are relevant and
  on but have no data yet (both are said even beside a kept plan with content),
  else, only when no relevant part is on, (3) `Anthropic isn’t set up — …` /
  `OpenAI isn’t set up — …` for each vendor with a relevant part off (every
  relevant key read and absent; `plans` with both plan parts off names both, in
  body order), else (4) none. A key's failure is said once, as its `⚠` line,
  never also as a message, so a key the credential store would not read gives
  no message and no "isn’t set up" (only an `Absent` key may paint a setup
  sentence). The whole-tile "No AI usage yet" sentence is never used on a
  narrowed tile, and the overview tile's empty text is that same message and
  loading (one function, `tile_scope::ai_header`). The Full view says its
  sentence once: it is the payload's `message`, and `tileEmpty` is not set when
  the narrowed `message` is what the panel paints.
  *Migration* (`StoreData::merge_ai_tiles`, after `split_usage_tiles`). The
  Anthropic and OpenAI tiles an earlier build saved become `aiUsage`, and none is
  left behind, since a leftover kind would make every later save fail
  `validate`. The first `anthropic` and first `openai` tile of the same scope
  (`all`, or `attention`) collapse into one: the earlier in layout order survives
  with its id, place and view settings, hidden only if both were. `anthropic` +
  `plans` becomes `item:claude-plan` (so it does not widen to Codex's plan);
  `claude` and every `item:*` scope are kept. **A lone `all` or `attention`
  vendor tile widens on purpose**: it now also shows the other vendor's
  configured parts, which is the "one tile" request, and it stops `with_source_tile`
  from counting a narrowed tile as the AI Usage tile and withholding the offer.
  Only the default titles (`Anthropic`, `OpenAI`) are renamed, and only on
  `all`/`attention` tiles. One-way: an older build does not know `aiUsage` and
  refuses every save, so downgrade is not supported.
- **Usage** — Neon, Sentry + Vercel (hourly by default, `PanelInterval::UsageProviders`).
  Vercel reads the FOCUS billing export: month-to-date spend and what falls
  beyond the plan. Neon renders compute/storage MTD, `NEON EST. CHARGES (MTD)`
  from operator-entered rates, and a best-effort `NEON LAST INVOICE` off an
  undocumented endpoint — that one failing degrades to `—` plus a footer, never
  the consumption rows. A provider with no key has no section. Before a pass has
  read every key the panel is loading, because only a key seen to be missing may
  paint a setup sentence; with all three missing it says what to add, and a
  credential store that would not answer says that instead.
- **Azure Cost** — the daily cost export, on its own cadence (4h by default).
- **Actions billing** — a GitHub organization's month-to-date hosted Actions
  minutes and **net** spend (the headline), a bar of net spend over the org's
  organization-scoped Actions budget, a per-SKU breakdown, the top repos by
  minutes and a plain `usage through <newest item time>` line. Its own loop and
  cadence (`PanelInterval::ActionsBilling`, 1h by default, floor 300s, the same
  figures and reason as `UsageProviders`: a vendor API against a rate-limit
  budget, with figures that move over hours), its own credential (a dedicated
  billing token, never the GitHub account's) and the organization's login, both
  re-read from the store on every pass. It is two reads, kept as two halves of
  state (`usage` and `budget`; a pass also reads the previous month's usage, as
  a result of its own, only when the Billing alerts' 7-day window crosses a month
  start), so a failed budget read never hides the usage
  and the reverse, and each half retains its last good figure, unless the
  organization changed: a pass for a different org drops both halves' figures
  first (before the credential is read, and without recording an absence, so an
  unreadable store says the keychain refused and never shows the old org's
  figures), while the same org's refused keychain read keeps its last good
  figures. **Staleness:** the footer and the freshness line are about the
  *poller* and keep the meaning every panel gives them: the last successful
  read (per half, attributed `usage:` / `budget:`), the footer's window 1.5
  cycles of the cadence, and the freshness line at one cycle. **`usage through`
  is the *report's* lag, which is separate and is fed into neither:** any
  non-empty footer or freshness is a Needs Attention warning
  (`dashboard::source_view`), and a report that always lags by hours would
  light the tile permanently. The bar is amber from 0.9 and red from 1.0 of the
  budget (Azure Cost's constants) and flags the tile row exactly as Azure's
  does; its label says whether the budget stops jobs (`prevent_further_usage`;
  `BUDGET · STOPS JOBS` / `BUDGET · JOBS CONTINUE`). A budget whose amount is 0
  is GitHub's "no paid usage" setting, and the crate names its states
  (`BudgetSpend`), never a ratio: measured net spend above 0 paints
  `<spend> / $0.00` with a full red bar, which flags the row and raises Needs
  Attention; measured spend of exactly 0 paints `$0.00 / $0.00` with no bar and
  no flag, and a measured credit (below 0) paints that spend over `$0.00` with
  no bar and no flag; an unmeasured spend paints `—` with no bar. The measured
  spend is printed whenever it was measured, except against a negative amount
  (which GitHub does not issue), where nothing is compared and it paints `—`.
  No budget shows `no Actions budget set` and no bar, never a 0% one; a month
  with no Actions usage says so in words, never `$0.00`.
  **Billing alerts (#770)** are two rules over the report's per-repo, per-day
  hosted minutes (`actionsbilling::hosted`; `Minutes` items of the `actions`
  product, every SKU, `Actions storage` never counts; a day with no item for a
  repo inside the read range is a measured 0). They judge the report's newest
  day (the `usage through` day, which may still be partial) and the day before
  it, so a flag clears by itself after about two days. **Spike:** a repo's day
  trips when its hosted minutes are at least the floor (default **1,000**) AND
  more than the multiple (default **3×**) of its average over the 7 days before
  that day; exactly 3× does not trip, and the 7-day window is fixed. **macOS on
  self-hosted:** a repo on the operator's list (empty by default, which is the
  rule off and says nothing; enter bare repo names, `headroom` and not
  `owner/headroom`, which is refused) with any hosted macOS minutes (a `Minutes` item
  whose SKU names macOS) on those two days; names match `repositoryName` as the
  report gives it, case-insensitively after trimming. Each tripped rule is a
  **red row** (`SPIKE · <repo>`, `MACOS ON SELF-HOSTED · <repo>`) that flags the
  tile and feeds Needs Attention through `dashboard::source_rows`, as the budget
  bar does. A spike's value carries the figures only so the overview row is not
  ellipsised (`<minutes> · <multiple>×`, or `<minutes> · from 0` when the 7 days
  before held none); a macOS row's is `<minutes> on <MM-DD>`. **Large figures
  compact** so the value fits the overview's 18-character slot: minutes print
  exactly up to 9,999, then `12.3k min`, `100k min`, `1.2M min`; a multiple
  prints to a tenth up to 999.9, then `1.0k×`, `7.1k×`, `123k×`. Figures below
  those thresholds print exactly as `minutes()` and `{:.1}` always did (the
  threshold is judged on that printed figure, so the value agrees with the
  detail); only the compaction step rounds half up, on the whole number in
  integer arithmetic. Every row's detail leads with the exact
  minute count (`1,500 min on 11-14 against a 7-day average of 300 min`;
  `<minutes> of hosted macOS on a repo listed as self-hosted only`), so the
  hover title and the Full view keep it. `OVERVIEW_VALUE_SLOT_CH` in
  `actions_billing.rs` tests every value against the slot, and a second test
  fails if `dashboard.css` narrows it. **No OS banner** is sent
  (`deliver_banners` is untouched). The rules
  are judged when the view is built, from the retained fold and the settings of
  that moment, so a save re-flags the tile without waiting for a poll. When the
  7-day window reaches into the previous month, that month is read too, as its
  own result (`PreviousMonth`, never fetched when the window stays inside the
  current month); if that read fails the affected days' spike verdict is
  **unknown**: a muted `SPIKE CHECK  —` row says the baseline could not be read,
  and nothing is evaluated over a shortened window or with the missing days as 0.
  The unknown row flags nothing (a failed read says so; it is not a spike). When
  the self-hosted list is non-empty and the day before the newest day falls in
  the unreadable month, a second muted row, `MACOS CHECK  —`, says the macOS
  minutes for that day are unknown; it flags nothing either. The
  multiple, the floor and the list are Configure → **Billing alerts**, shared by
  every Actions billing tile (`store::actions_billing_alerts`).
  **The daily trend (#771)** is one line of the month's **total hosted minutes
  per day**, drawn in the tile's **Full view only** with the host sparkline
  painter (`spark()`); the overview tile and Detail show no chart. It sums every
  `actions` SKU whose `unitType` is `Minutes` over every repo, from the same
  per-repo, per-day fold the rules use (`HostedDays::daily_total`; storage never
  counts, and there is no per-SKU split). The series runs from the **1st through
  the `usage through` day, inclusive**: a day in that range with no item is a
  measured **0**, and a day after it is not drawn at all (no point, no zero). With
  fewer than 2 days (the newest day is the 1st) there is no line and the Full
  view says `the daily trend starts on the 2nd`. A failed read, a month with no
  Actions usage and the setup state have no chart and no sentence: the tile's own
  message stands. Org-level items (no `repositoryName`) are counted in each
  day's total, as the `HOSTED MINUTES` stat counts them, though they feed no
  per-repo rule. A month whose minutes are all zero (storage only) keeps a flat
  line at the floor with the caption `peak 0 min`: a measured zero is a value,
  not a gap. The y range is `0` to the series maximum. The decision is
  Rust's: the `actions_billing` command keeps `trend` and `trendNote` only in a
  read that names a tile (which only the Full view sends) and nulls them in any
  other, `tile_scope` drops them for a tile that picked rows (the trend is not
  a row), no overview row carries them, and the JS only paints them.
  Setup (no token or organization) is a muted sentence naming Settings →
  Sources; a rejected token is the red `Fault` sentence and names Administration:
  Read and an org admin; a response missing a field the read decides on is
  unreadable, not softened.
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
  accounts already in the cockpit — plus any part of the Anthropic source, each
  of which is Anthropic's evidence (`anthropic_configured`) — and appends the operator's own Atlassian Statuspages
  (`store.json`'s `status_vendors`). `poll_service_status` reads exactly that
  list through `read_vendor`, and `readings`/`view` render exactly that list, so
  a vendor cannot be polled and then dropped from the panel. A cockpit with
  nothing configured watches nothing and **says so** rather than painting an
  empty body under "all clear". `ServiceStatuses::watching` forgets the readings
  of a vendor that leaves the set, which is what stops a re-added vendor
  rendering the status it had when it left — the same rule `StatusWatch` applies
  to its baseline, and what keeps #297's seed-don't-alert true. Finishing a
  source's steps wakes the Services loop, because any part of it can add or
  remove a watched vendor. The panel carries a
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
  loosen the 10s default five-fold, #633), Azure Cost (cadence + 1h), Actions billing (1.5 cycles, per half) and the plan limits (twice the
  cadence). Sentry Crons and the metered Usage providers (Neon, Sentry, Vercel)
  read it at render time too, each its own (`PanelInterval::Crons` and
  `UsageProviders`): the window is 1.5 cycles
  (`panel::stale_after_for_slow_cadence`; 90 minutes at the 1h default, 7.5
  minutes at the 300s floor), so a healthy loop is not stale between polls
  and a stuck one is flagged within two cycles (#635). The usage loop runs
  its two halves on their own cadences: each turn runs only the half that is
  due (the Claude rollups never ride the providers' turn, so a slow rollups
  cadence is honoured), then sleeps until the next half is, and each provider
  is stamped as its read completes. A saved provider credential forces the
  providers onto the next turn; a resume from sleep forces both.
- **OpenClaw** — an agent farm over a live WebSocket. **Event-driven, on no
  cadence at all**, which is why it is the one panel with no staleness signal of
  any kind (the polling panels carry a `status_footer`; Hosts warns per card,
  via `sampler_stale` and unreachable).

Each panel is sized by `viewmodel::cockpit` for the window it fills in its tile's
Full view (`panel_columns`; Machines also `host_columns`), and sibling host cards
are the same height. An in-app Settings surface over `crates/store`
(hosts CRUD, portfolio, credentials, general prefs; the
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
