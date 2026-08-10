# Solador Cross-Platform Cockpit (`app/`)

An experimental cross-platform shell proving out a macOS/Windows-portable stack: a
[Tauri v2](https://v2.tauri.app) app that renders the **whole cockpit** the SwiftUI
app renders — the local card plus one card per configured Solador
[agent](../agent/README.md), then the **Containers/VMs**, **GitHub Repos**,
**GitHub Runners**, **Usage** (Claude + Neon + Sentry), **Azure Cost** and **OpenClaw**
panels — reflowed into rows for the measured width, and configured from an in-app
**Settings** surface backed by the OS credential store.

It began as a walking skeleton (one host card, one command). It is not one any
more: [#150](https://github.com/cpmadrid/solador/issues/150) took it to panel
parity across fourteen slices. What it still is not is *shipped* — the product you
install is the SwiftUI app in [`DevCanopy/`](../Solador), which stays untouched.
Packaging, signing and updates are [#15](https://github.com/cpmadrid/solador/issues/15)'s,
not this tree's.

```
app/
├── src-tauri/            # Rust shell
│   ├── src/main.rs       # per-host poll tasks + the `#[tauri::command]` surface
│   ├── src/settings.rs   # the Settings view-model and its pure rules
│   ├── src/panel.rs      # the refresh-health warning + progress bar every panel
│   │                     #   shares (`footer` in the payload, painted in the header)
│   ├── src/local.rs      # this machine's card: the sampler's poll state and
│   │                     #   the honest-unknown rendering
│   ├── src/containers/   # the Containers/VMs panel: local runtimes, grouping,
│   │                     #   presence, and every string it paints
│   ├── src/github/       # the Repos + GitHub Runners panels, and the local
│   │                     #   git scan behind their LOCAL/WT columns
│   ├── src/usage.rs      # the Usage panel: Claude tokens + Neon + Sentry
│   ├── src/azure.rs      # the Azure Cost panel
│   ├── src/openclaw.rs   # the OpenClaw panel: the device-key store, the live
│   │                     #   session, and every string the panel paints
│   ├── capabilities/     # default.json — the webview's ACL (one grant; see
│   │                     #   "The one granted capability")
│   └── tauri.conf.json   # window, CSP, `frontendDist: ../ui`
└── ui/                   # frontend: plain HTML/CSS/JS, no bundler
    ├── app.js            # the cockpit (host grid + the panel-row layout)
    ├── settings.js       # the Settings view
    ├── containers.js     # the Containers/VMs panel
    ├── github.js         # the Repos + GitHub Runners panels
    ├── usage.js          # the Usage panel
    ├── azure.js          # the Azure Cost panel
    └── openclaw.js       # the OpenClaw panel
```

Every string and colour the frontend paints comes from Rust
([`crates/viewmodel`](../crates/viewmodel)); the frontend does layout and nothing
else. That split is why the panels are testable without a webview at all: a panel
is a function returning JSON, and the offline fixtures below are that same JSON
dumped to disk.

The shell sits at the top of the root Cargo workspace — distinct from `agent/`,
which pins its own toolchain and has its own CI job:

| crate | what it owns |
|---|---|
| [`wire`](../crates/wire) | the agent's JSON contract (package `devcanopy-wire`, imported as `wire`) |
| [`agentclient`](../crates/agentclient) | the HTTP client for `/v1/snapshot`, `/v1/containers`, `/v1/health` |
| [`viewmodel`](../crates/viewmodel) | every string, colour and layout number the frontend paints |
| [`store`](../crates/store) | settings / hosts / repos / rules / roster JSON + the OS credential store |
| [`localhost`](../crates/localhost) | this machine's metrics; every field the platform can decline is an `Option` |
| [`github`](../crates/github) | the GitHub REST client: workflows, runners, roster/presence |
| [`usage`](../crates/usage) | Claude Code log rollups, Neon consumption, Sentry stats |
| [`azurecost`](../crates/azurecost) | the Cost Management export reader (SAS blob list + RFC4180 CSV) |
| [`openclaw`](../crates/openclaw) | the OpenClaw gateway client: WS protocol v3, Ed25519 identity, reducer |

## The `cockpit` command

One command, called on every poll tick and on resize:

```js
await window.__TAURI__.core.invoke("cockpit", { width: gridWidth });
```

```jsonc
{
  "hosts": [ /* the local card, then one card per enabled host, each with its `id` */ ],
  "hostColumns": 2,        // viewmodel::cockpit::host_columns for `width`
  "hostCardMinWidth": 900, // …and the numbers it used, so the frontend can't
  "spacing": 16,           //    re-derive them and disagree
  "hostTabs": null,        // or {"minHeight": 780, "tabs": [{"id", "label"}, …]}
  "panels": [ /* the panel table: id, title, minWidth */ ],
  "panelRows": [ /* the reflowed layout: who shares a row, in order, and the
                    span/weight/width/columns each one gets */ ],
  "empty": null,           // or {"message": …} when no REMOTE host is configured
  "settingsLabel": "Settings"  // the button that opens the Settings view
}
```

The column count is decided in Rust, not by a CSS `repeat(auto-fit, minmax(900px,
1fr))`: that CSS would be a second implementation of
`CockpitBreakpoints.hostColumns`, free to disagree with the tested one. The
frontend passes the grid's measured width and applies the answer.

`hostTabs` is the **overflow mode**, and it is the same argument one layer down.
Below the side-by-side breakpoint the cards stack — every host stays visible,
which is the point of an always-on cockpit — unless the operator picks *Show as
tabs* under Settings → General, and then one card shows at a time behind a bar.
Which of those happens is `viewmodel::cockpit::host_tabs`, and its three
conditions are `HostsPanel.content`'s: `columns <= 1`, more than one host, and
the preference set. A `columns <= 1 && hosts > 1` written in JS would be that
rule restated where no Rust test can see it, so the payload carries `null` or a
finished tab bar — one tab per card, in payload order (this machine leads the
bar exactly as it leads the grid), labelled with the card's own host name and
carrying the container's `minHeight`. That floor is `HostsPanel`'s
`.frame(minHeight: 780)`: with one card on screen the grid has nothing else
sizing it and would collapse to the height of the bar.

The frontend **hides** the other cards rather than removing them. A card torn
down on every tab switch would lose its chart nodes and therefore its sparkline
history, which is the one thing the card is worth looking at for; hidden cards
keep recording and are current the moment they are shown.

`panelRows` is the same argument for the panels *below* the grid:
`CockpitLayout::hosts_forward()` reflowed for that width by
`viewmodel::cockpit::reflow`. It is not a global `sm`/`md`/`lg` tier and it is
not a CSS `auto-fit` — **every panel declares its own `min_width`**, and a row
splits only when *its* panels stop fitting. The case that model exists for is
visible at the 880pt window floor: Containers + OpenClaw still share a row at
412pt each where Repos + Runners (896pt) must break apart. Rows naming a panel
this frontend has no section for — `hosts`, which is the grid above — still
travel; app.js skips them rather than Rust silently omitting a row it did
produce.

Each entry also carries a **span**: `full`, `threeQuarters`, `half` or
`quarter`, plus the `weight` (4, 3, 2, 1) of quarter tracks the frontend gives
that panel. So the shipped third row is Containers over two tracks beside
OpenClaw and Usage over one each, and Azure Cost owns all four, which is what
affords its two-column body (the top-resource breakdowns sit beside the costs
rather than under them). A panel is held to its `min_width` against **the width
its span gives it**, not against the row's sum of minimums: the sum let a hungry
panel borrow width from a lean neighbour that it then never got, since the track
it renders in is its span's share. `panel_widths` is the one place that
arithmetic lives, and `width` in the payload is its answer — the same number
`columns` was derived from.

**Every row is the same four-quarter grid.** A quarter track is
`(width − 3 × gap) / 4` and a span of `k` gets `k` tracks plus the `k − 1`
gutters it swallows — the frontend paints exactly that, `repeat(4, minmax(0,1fr))`
plus `grid-column: <start> / span k`, so it and `panel_widths` are one
construction rather than two that can disagree. The denominator is a fixed four
and not the row's own weight, which is a distinction that cost 8pt: dividing
`width − (n−1) × gap` by the weights in *this* row makes the gutter total move
with the panel count, so a `half` beside two `quarter`s came out half a gutter
narrower than a `half` beside one `half`, and the card edge under Repos|Runners
missed the one under Containers|OpenClaw directly below it.

**A rendered row always adds up to four quarters**, so `span` is always one of
those four words. Reflow can cut a row short — evict Usage from the quarter row
and Containers + OpenClaw are left holding three quarters between them — and
`fr` tracks would then stretch that pair to two thirds and one third: a width
nobody authored, no picker offers, and a reader can only take for a bug. So
`fill_row` widens the survivors first, by the distribution closest to their
authored proportions (Half + Quarter → **ThreeQuarters + Quarter**; Quarter +
Quarter → Half + Half, not a lopsided pair), and every candidate is checked
against `min_width` at its *final* width, because filling shrinks whoever does
not grow. A row with no legible filling is refused — which is exactly how reflow
learns the panel does not fit there. The `span` a panel travels with is
therefore the one it is **rendered** at, which can be wider than the one the
user authored; the Settings editor reads the authored width from the store, not
from this payload.

Every card in a rendered row is **the same height** (`align-items:stretch`).
Content stays top-aligned, so a short panel beside a long one carries trailing
space inside its card rather than leaving a ragged edge in the row.

**Where a warning goes.** A panel's `footer` — `panel::status_footer`'s
`{text, color}`, the amber `⚠ couldn't read runners … · last ok 2m ago` line —
is painted **in the panel header, beside the title** (`.panel-stale`), not under
the body where the Swift original puts it. It used to be a `<p>` after
`.panel-body`, and that made the card a line taller the moment the panel
degraded; combined with the equal-height rule above, every other card in the row
grew with it. So a token quietly losing a scope moved half the cockpit. The
header is always rendered, so the warning now costs no height at all. It
ellipsises rather than wrapping — a second header line would be the growth this
exists to prevent — with the panel title yielding its width first
(`flex-shrink:100`, the host card's `.stale` rule) and the full text on the
element's `title`. Usage's *per-provider* footers stay in the body: they belong
to a section, and a section has no header to move to.

`empty` keys on the count of *monitored* hosts, not on the number of cards: the
local card is always there, so counting cards would answer "is anything
configured" wrong forever.

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
                                     //    / {"message": "looking for containers…"}
  "loading": false,                  // true until the first `docker ps` returns
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
"workflow jobs" on `ubu-01`, `ghcr.io/*` hidden) and seed **only** when the
store has never carried rules: a deliberately emptied list stays empty. They are
edited under [Settings → Hosts](#settings), beside the host list whose names
scope them.

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
  "id": "ghWorkflows", "title": "GitHub Repos",
  "trailing": "1 needs approval · 1 running · 1 failed · 1 unreadable",
  "message": null,                       // or {"text": "connect a GitHub token in Settings"} / {"text": "loading…"}
  "loading": false,                      // true while the panel is still filling in
  "availability": {"label": "Operational", "color": "#1c6b41", "detail": "GitHub Actions is operational and …"},
  "columns": [{"label": "REPO", "width": null}, {"label": "ISSUES", "width": 52.0}, …],
  "rows": [{
    "repo": "Sassy-Dog/velovate", "name": "velovate",
    "dotColor": "#e05a4f", "blinking": false,
    "url": "https://github.com/Sassy-Dog/velovate/actions",   // the row's click target
    "linkLabel": "Open Sassy-Dog/velovate on GitHub Actions", // its accessible name
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
`PanelKind::GhWorkflows.min_width` (440pt) is built on, and a width re-typed in
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

**The availability verdict answers "is it us?", which a status chip cannot.**
Both panels carry an `availability` chip beside their title, and it is a
*conjunction*: GitHub's published `Actions` status folded with our fleet's
per-OS state (`crates/github/src/status.rs`).

| Actions | fleet | chip | colour |
|---|---|---|---|
| `operational` | dark | **Fleet Down** | red — ours to investigate |
| `major_outage` | any | **Major Outage** | red — runs are failing |
| `degraded_performance` / `partial_outage` | any | **Services Degraded** | amber — GitHub is slow |
| `operational` | online | **Operational** | dim green |
| unreadable | any | **Status Unknown** | muted |

The healthy label is **Operational** — the same word, in the same dim green, as
the Services panel's own GitHub row (described below). Both render one
`ComponentStatus`, and the header saying *GitHub OK* while the row beneath said
*Operational* invited the reading that they measured different things. The
subject is already in the panel title the chip sits beside, so naming GitHub
twice bought nothing. The two literals live either side of a crate boundary, so
a test in `services.rs` pins them together.

When GitHub admits to a problem it takes the headline, at *its* severity; the
fleet half becomes the sentence behind it ("Linux being offline is expected
while this lasts"). The fleet only takes the headline in the case GitHub is not
explaining — an operational Actions with a platform dark anyway, which is the
one row that is a page rather than reassurance. Red is shared between
**Major Outage** and **Fleet Down** deliberately: both mean *something is badly
wrong right now*, and the label is what says whose problem it is. The
motivating incident (2026-08-06) had every Linux runner cycling through
`Registration <uuid> was not found` while both macs stayed up, which is
indistinguishable at a glance from a real fleet deregistration; GitHub's own
page read `Actions: major_outage` throughout.

Three rules it inherits from the crate it lives in. **Unreachable is not
operational** — a statuspage fetch that fails yields an explicit muted
*unknown*, never green, and never suppresses the fleet reading it annotates.
**A failed refresh keeps the last good reading**, because GitHub's status does
not change on the timescale of one dropped request and flipping "it's GitHub"
back to a red "it's us" on a single timeout is the exact misdirection this
exists to prevent. And **an absent platform is not a dark one**: the verdict
gates on `*_total > 0`, or an org with no Windows runners would show a
permanent red forever.

**And a fleet reading can only convict itself while it is fresh.** The verdict
does not take a `RunnerSummary`; it takes a `Fleet`, which is either `Known(&…)`
or `Unknown`, and `Unknown` makes **Fleet Down** unreachable by construction.
The app hands over `Known` only when `runners_last_updated` — which advances on
a successful fetch, never on an attempt — is within **`RUNNERS_STALE_AFTER_SECS`
(150s)**, the same window the Runners panel's own footer uses to call its list
stale. That coupling is the point: the moment the panel admits its roster is out
of date, the chip stops blaming the fleet for it, and two readings of one fact
cannot disagree on screen. A stale fleet still reports GitHub's *own* severity —
staleness silences the fleet half, not the vendor half — and the `AllGood` detail
drops its "every registered platform has runners online" claim, because that is a
statement an unverifiable roster cannot support.

Without that gate, an overnight suspend was enough: the laptop opened on
2026-08-07 to a red **Fleet Down** computed from the previous evening's roster
while all twelve runners were in fact online, which is the alarm this chip exists
to make trustworthy. The other half of that fix is the [resume
watchdog](#waking-up-with-the-machine).

Two details worth keeping: the Actions component is matched by **id**
(`br0l2tvcx85d`), because `components[]` carries a non-component entry called
*"Visit www.githubstatus.com for more information"* and display names are
Atlassian's to re-word; and the read is issued **before** `poll_github`'s token
gate, since "GitHub is on fire" is most useful precisely when the panel is
otherwise blank.

**The runner roster is the memory that makes an absence visible.** The org's
ephemeral runners de-register between jobs, so GitHub's registered list can
never say "mac-s2 *should* exist". Every name it has shown us is remembered in
`store.json`'s new `runner_roster` field (the Rust counterpart of the Swift
app's `ghRunnerRoster` UserDefaults blob), with a 24h age-out for rotated
names. An absent name renders amber `recycling 40s` inside the 300s grace and
red `missing 12m` past it — and those clocks are folded forward **only by a
successful fetch**, so an hour of GitHub being unreachable ages nothing. The
panel keeps its last-good rows through a failure and puts the reason in the
`footer` field (`staleAfter: 150s`), which the frontend paints **beside the
panel title, not under its body** — see "Where a warning goes" below.

**Applied without a restart.** The token and the portfolio are re-read on every
pass, and `github_wake` cuts the sleep short after a Save, a Clear, a portfolio
edit or a new refresh interval — shortening a 5-minute cadence to 30 seconds
and then waiting five minutes for it to take is indistinguishable from the
setting doing nothing. This is the periodic-service counterpart to
`reload_hosts`, which reconciles tasks instead.

**Tapping a row opens its Actions page**, the way the Swift panel's
`onTapGesture` + `NSWorkspace.open` does
([#187](https://github.com/cpmadrid/solador/issues/187)). The URL is
`github::actions_url`'s and is never composed in the webview — it is the only
string the granted ACL scope accepts, and a second author of it would be a
second author of the app's whole browser-opening surface. See
[The one granted capability](#the-one-granted-capability).

The row is a `div`, not an `<a>`, so github.js spells out what a real link would
carry for free: `role="link"`, Rust's `linkLabel` as the accessible name (the
row's own text is seven numbers), a tab stop and an Enter handler. That is
*more* than the Swift panel, whose `onTapGesture` on a `VStack` is invisible to
VoiceOver and unreachable from a keyboard — parity with a gap is not a reason to
reproduce the gap.

**A run entering an approval gate raises an OS notification**, one per run, on
the transition only. The rule lives in
[`src/github/notify.rs`](src-tauri/src/github/notify.rs) — a port of
`GHWorkflowsService.notifyApprovalTransitions(in:)` — and it is pure, so the
part that decides *whether* to alert is unit-tested without a notification
centre anywhere near it:

- **Transition, not state.** A parked run stays parked for as many passes as it
  takes a human to notice. The banner fires on the pass where the run *enters*
  `waiting` and never again; re-alerting every 60 seconds is how a signal
  becomes noise.
- **The first pass only seeds.** Launching must not alert for gates that were
  already open. The first `observe` records the baseline and returns nothing.
- **The baseline advances even when the preference is off.** Swift updates
  `knownApprovalRunIDs` in a `defer` outside its preference guard; skipping that
  would make re-enabling the alert fire a backlog for everything that parked
  while it was off.
- **The preference is `notify_on_approval_needed`, default true, and has no
  UI** — exactly as in Swift, where `WorkflowDisplayOptions.notifyOnApprovalNeeded`
  likewise persists with no control anywhere in Settings. It is re-read on every
  pass, so editing the store file applies without a relaunch.

**Five vendors, three transports, one vocabulary.** `crates/servicestatus`
reads GitHub, Anthropic and Vercel from Atlassian Statuspage, Neon from
status.io, and Azure from an RSS incident feed, and lowers all three onto one
`ComponentStatus`. The Services panel renders a row each; the GitHub reading
additionally feeds the Repos/Runners conjunction chip.

Two vendor-specific traps are recorded in the code rather than here, but both
are worth knowing: `status.anthropic.com` **302s** to `status.claude.com`, and
**Azure cannot say "operational"** — its feed lists active incidents and nothing
else, so a quiet feed renders as *No Incidents*, a weaker claim than every other
row's *Operational*, and never as a green tick Azure did not give.

**The second notifier watches services rather than runs.** `services::StatusWatch`
(`app/src-tauri/src/services.rs`) fires when a watched third-party service
changes availability — GitHub Actions entering a major outage, and coming back
out of one. It inherits all three rules above and adds a fourth the approval
watch does not need:

- **Unknown is not a transition.** A statuspage we could not reach is not a
  status, so an unreadable pass leaves the baseline exactly where it was.
  Reading `None → Operational` as a recovery would announce "GitHub is back!"
  every time a CDN blip resolved, having never said it was down. `ApprovalWatch`
  lives with the mirror-image wart deliberately (an unreachable repo re-alerts on
  its return) because its source is one call per repo; this one reads a single
  endpoint, so the honest answer is available and worth taking.
- **Seeding is per service, not global** — there is no `seeded` flag. Vendors
  enter the map at whatever pass each first answers, and a global flag would let
  the second vendor's very first reading fire a banner.
- Preference: `notify_on_service_change`, default true, no UI, same rules as its
  neighbour.

Both notifiers deliver through one `deliver_banners` (`main.rs`), which is why
the ACL still grants `tauri-plugin-notification` nothing: the webview is never
in the path.

Two honest gaps against Swift, both platform, neither hidden:

- **The banner is not tappable.** Swift attaches the run's `htmlURL` to the
  notification's `userInfo` and opens it from the delegate;
  `tauri-plugin-notification`'s desktop path is fire-and-forget through
  `notify-rust`, with no action callback to hang that on. So `ApprovalNotice`
  carries two fields, not three — a URL nothing can act on is the same
  "field that reads as a feature" this panel refused before there was a click to
  spend it on.
- **The sound is macOS-only.** `content.sound = .default` becomes
  `NSUserNotificationDefaultSoundName`, which is a platform *resource name*
  rather than a portable "make a noise" flag. There is no Windows spelling of
  "the default one", so the banner is silent there rather than named with
  something the platform would ignore.

One thing is still deliberately **not** here: **no "Forget" on an absent
runner**. That is a right-click context menu over a `roster::forget` that
`crates/github` already implements, and a remembered name ages out on its own
after 24h regardless.

Both panels now sit in whichever row `panelRows` puts them — side by side at
≥896pt, stacked below it. See [the `cockpit` command](#the-cockpit-command).

Two counts are deliberately not the same number: the rollup counts *every*
container the runtimes reported, including the ones rules hid or collapsed (so
cruft building up stays visible), while `· N missing` counts exactly what the
rollup can no longer see.

## The `usage` and `azure_cost` commands

```js
await window.__TAURI__.core.invoke("usage");
await window.__TAURI__.core.invoke("azure_cost");
```

```jsonc
// usage
{
  "id": "claudeUsage", "title": "Usage",
  "trailing": "1.2M today",              // "" when no summary exists
  "message": null,                       // or {"text": "reading logs…"} / "no usage data"
                                         //    / "no Claude usage in the last 7 days"
  "windows": [{"label": "5H", "value": "820k", "valueColor": "#33d17a"}, …],
  "projects": {"label": "TOP PROJECTS (7D)", "rows": [{"name": …, "value": …, "dotColor": …}]},
  "providers": [
    {"id": "neon", "rows": [        // "—" (muted) for any figure that wasn't measured
      {"label": "NEON COMPUTE (MTD)",     "value": "12.4 CU-h", "valueColor": …},
      {"label": "NEON STORAGE",           "value": "3.2 GiB",   "valueColor": …},
      {"label": "NEON EST. CHARGES (MTD)", "value": "≈ $2.45",  "valueColor": …},
      //   ^ the row is *absent*, not "—", when no rate is set or usage is unmeasured
      {"label": "NEON LAST INVOICE",      "value": "$15.91",    "valueColor": …}
      //   ^ "—" when the org has no invoices yet, or none could be read
    ], "footer": …},
    {"id": "sentry", "rows": […], "bar": {"fraction": 0.94, "color": "#e09a26"}, "footer": …}
  ],
  "footer": null                         // Claude's own, staleAfter 150s
}

// azure_cost
{
  "id": "azureCost", "title": "Azure Cost",
  "trailing": "$1,284.55 MTD",           // or "$1,284.55 · Jun" in the fallback
  "message": null,                       // or {"text": …, "color": …} — muted setup vs red failure
  "headline": {"value": "$1,284.55", "caption": "month-to-date", "captionColor": …},
  "stats": [{"label": "PRIOR MONTH", "value": "$2,011.40"}, …],
  "budget": {"label": "PROJECTED VS BUDGET", "value": "$1,942.18 / $2,000.00", "bar": …},
  "breakdowns": [{"title": "TOP RESOURCE GROUPS", "rows": […]}, …],
  "footer": null                         // staleAfter 5h
}
```

**Four sources, and Vercel is the odd one.** Claude Code's token rollups are
a walk of `~/.claude/projects` on the store's `refresh_interval_secs`
(`staleAfter` 150s). Neon consumption and Sentry event stats are hourly API reads
inside the same loop (`staleAfter` 90m — above their own cadence, so a warning
means a stuck poller and not the gap between polls). Azure cost is its own loop
on `azurecost::POLL_INTERVAL` (4h), because the export is published about once a
day and the crate's fingerprint cache makes an unchanged cycle cost one blob
listing and zero partition bodies.

**Unknown is not zero, again — and this time it also suppresses a bar.**
`crates/usage` models Neon's and Sentry's summaries as enums whose *unmeasured*
variant carries no figures at all, so `—` can never be typed into a `0` by
mistake. The Sentry quota bar therefore needs **both** a configured quota and a
known count: a bar drawn at a defaulted zero would read "comfortably under quota"
when the truth is "nobody measured". A measured `0` does get its bar, empty.

**The two Neon cost rows are priced by the operator, never by us.** `NEON EST.
CHARGES (MTD)` is consumption × the rates entered under **Settings → Usage** —
`$ per CU-hour` and `$ per GiB-month storage`, both plain non-secret preferences
— multiplied by `usage::neon::estimate_usd`, which reproduces the Neon console's
own "Charges to date" arithmetic. The app ships **no price table on purpose**: a
hard-coded rate goes quietly wrong the day Neon reprices, and a wrong number
with correct digits is worse than no number. Leave both rates at `0` and the row
is *absent* — not `$0.00`, and not `—` — because an unset rate is setup, not a
measurement; set one and it prices its half with the other counted as zero.

`NEON LAST INVOICE` needs no rate: it is what Neon actually billed, read from
`GET /api/v2/organizations/{org_id}/billing/invoices` — an endpoint **absent
from Neon's public OpenAPI spec** but served to org API keys, so it is treated
as **best-effort** everywhere. A failure leaves the last figure standing (or `—`
if there never was one) and degrades the section to estimate-only; its reason
reaches the footer only when consumption is healthy, since consumption is the
section's primary content and there is one footer per section. An org with no
invoices yet renders that same `—` rather than `$0.00`, which would assert a
bill that does not exist. The amount wears a `$` only when the invoice's
currency is USD — a euro total in dollar clothing is a wrong number with right
digits again.

**An unconfigured provider has no section at all** — no heading, no em dash, no
layout shift; the panel is pixel-identical to its Claude-only self. The em dash
is reserved for "configured, and we could not find out". Azure draws the same
line in a different colour: **no SAS URL is a muted setup instruction**, a failed
read is **red** and names the failure, and rendering the first as the second
would send an operator hunting a break that does not exist.

**"Nobody has looked yet" is a state of its own**, and it is the one every panel
used to get wrong. Each stored "is this configured" as a `bool` that only a
*completed fetch* set, so the value at launch — before any pass had read the
credential store — was byte-identical to "we looked and there is nothing there".
Repos and Runners opened on `connect a GitHub token in Settings`, Azure on
`Add an Azure Cost SAS URL in Settings`, and Containers asserted
`no containers detected` before it had run a single `docker ps`. All of it at a
machine where everything was configured and working.

`panel::Configured` is the fix — `Unknown` / `Absent` / `Present`, defaulting to
`Unknown` — and the half that matters is *when* `Present` is recorded: the moment
the credential is read, **before the request**, not when the response lands. A
pass holding a token spends seconds fetching, and for all of it the panel now
says "loading…" rather than denying it has one. Only `Absent` may paint a setup
instruction, because only `Absent` observed the absence. Panels also publish
`"loading"` so the frontend can poll faster while they fill in and settle
afterwards — the panel scripts refuse to read Rust's strings, so inferring it
from the message text was never an option.

**A credential the store refuses to read is a fourth state**, and it is neither
of those. `Credential::Unreadable` (`main.rs`) deliberately does *not*
unconfigure anything: a locked keychain would otherwise delete a live Neon
section for an hour, or paint "Add an Azure Cost SAS URL in Settings" over a
perfectly good configuration *and* discard the fingerprint cache, so the next
successful read re-downloads every partition. Instead the panel keeps its figures
and the footer says `couldn't read the credential store`. A provider that was
never configured stays silent — a hiccup must not conjure a section for something
nobody set up. This is a deliberate divergence from Swift, whose `KeychainHelper`
collapses a read failure into `nil`.

**Every credential read obeys that rule** — the GitHub token and the per-host
agent tokens joined it in #224, having predated the helper. An unreadable store
leaves the Repos and GitHub Runners panels holding everything the last pass
fetched, with `couldn't read the credential store` in place of `connect a GitHub
token in Settings`; `apply_unauthenticated()` — which clears both panels — now
runs only when the store *answers* that nothing is stored. Runners keeps its
rows under that line; Repos follows its own long-standing render contract, where
a message replaces the table (as `loading…` does), and keeps the table in state
so the next answered read repaints it rather than re-deriving it. A host card
whose token could not be read says `Couldn't read the credential store for this
host's token.` instead of asserting nobody configured one; either way no request
is made, since an empty bearer token buys one 401 per tick and blames the agent
for a failure on this side. That read still happens once per `spawn_host`, so a
store that recovers is picked up on the next hosts reload, not the next tick.

**One fabricated zero survives, and it is named rather than hidden.** Azure's
`PRIOR MONTH` renders `$0.00` when the prior-month export is missing, because
`azurecost::CostSummary::spend_prior_month` is a bare `f64` the crate documents
as best-effort ("a missing prior-month export leaves this 0 rather than failing
the whole read") and Swift's `AzureCostService` does the same. It is the one
figure on either panel that cannot tell "we spent nothing" from "we could not
look" — the Usage panel solved exactly this shape with `Unmeasured` enums, and
lifting `spend_prior_month` to an `Option` is the same fix, in `crates/azurecost`
rather than here.

Two things the Claude half deliberately does not have. There is **no progress bar
on the 5H / WEEK rows** — Swift's `fiveHourTokenLimit` and `weeklyTokenLimit` are
both `nil`, a subscription publishes no ceiling, and a bar against an invented
one is a percentage of a number nobody set. And **no USD**: `crates/usage` prices
every record and the figure is unit-tested, but the account is subscription-based
so it is never displayed.

**Applied without a restart**, like the GitHub panels: saving or clearing a Neon
key, a Sentry token, an org id or a slug wakes the usage loop *and* forces its
hourly half to run on that pass, so a newly-saved key fills its section in
seconds rather than within the hour. A SAS URL wakes the Azure loop the same way.
The Sentry quota, the Azure budget and the two Neon rates wake nothing on
purpose — all four are read at render time, so changing one repaints the bar or
row it feeds on the next 10s frontend tick with no fetch involved.

## The `crons` command

The **Sentry Crons** panel: every cron monitor environment that is not `ok`, and
**how long it has been broken**.

```js
await window.__TAURI__.core.invoke("crons");
```

```jsonc
{
  "id": "sentryCrons", "title": "Sentry Crons",
  "trailing": "3 not ok · 1 suppressed",  // or "all ok", or "couldn't read"
  "message": null,                        // or {"text": …, "color": …} — see the ladder below
  "rows": [
    {
      "id": "platform/cron-relay-drift-check/prd",
      "label": "cron-relay-drift-check",
      "detail": "platform/prd · error",   // + the suppression reason, + why an age is soft
      "age": "7d 22h",                    // "≈ 0d 22h" / "never checked in" / "—"
      "ageColor": "#e05a4f",              // red = from the incident, amber = weaker claim
      "color": "#e05a4f",                 // muted when suppressed
      "suppressed": false,
      "title": "cron-relay-drift-check (platform/prd) — error for 7d 22h"
    }
  ],
  "footer": null                          // staleAfter 90m, the other Sentry read's window
}
```

**The age is the whole panel.** `cron-relay-drift-check` sat red for a week with
no signal after the first hour, because the Sentry rule behind it fires on *first
seen* and *regression*: a weekly cron alerts once and then goes quiet, so day 6 of
an outage looks identical to day 1. Making the sixth day *look* like the sixth day
means measuring from `activeIncident.startingTimestamp`, **never** from
`lastCheckIn` — a job that runs on schedule and keeps failing checks in
constantly, so it looks freshly broken every day. Measured live on that monitor,
the two read **7d 22h** and **0d 22h**.

So the provenance travels all the way to the pixel. `crates/usage`'s `CronAge` is
an enum, not a `u64`: `Incident` is the real figure, `SinceLastCheckIn` is reached
only when there is no incident to read and renders with a `≈` in amber plus a
detail line naming the fallback, `NeverCheckedIn` renders words rather than a
duration, and an unreadable timestamp is the em dash — it does **not** borrow the
other field, which would be the wrong number wearing the right label.

**Three traps in the wire**, all of which bit the Slack half of this work first,
all three traceable to fixtures built from the Sentry **MCP server's** normalised
output rather than the raw REST payload:

| the mistake | the truth |
|---|---|
| `projectSlug` | nested at `project.slug` — the flat field does not exist, and reading it rendered `undefined/prd` for a week |
| `hasMoreEnvironments` | does not exist, and there is **no** environment-truncation signal of any kind, so no guard here pretends otherwise |
| `activeIncident` present ⇒ failing | it is a key on **every** environment and is `null` on healthy ones |

**Suppression is counted, not dropped.** `status == "disabled"` or
`isMuted == true` — *strict* `true`; missing, `false`, `1` and `"true"` all stay
red — at **either** the monitor or the environment level mutes a row to grey and
prints its reason, and it still occupies a row. A monitor somebody muted six
months ago and forgot is exactly what this panel should surface.

**A blind read is red, never empty-and-green.** Four states the ladder in
`view()` keeps apart, in this order: `Configured::Unknown` (the frame before any
pass has read the keychain) paints the loading line; `Absent` is the only state
entitled to say *"Connect a Sentry token in Settings"*; a failed read is red and
names the failure; and a **measured** org with nothing broken is the only one
allowed to say so. On top of that, two readings that would otherwise render as a
calm empty card are red: **no monitors at all** (an org with no crons, a mistyped
slug and an under-scoped token are indistinguishable) and **a monitor carrying no
environments** (nothing could be read about it, which is not the same as "it is
fine"). Rows that *were* read stay on screen under that warning, and the trailing
label says `couldn't read` rather than `all ok`.

There is deliberately **no** "suspiciously few monitors" guard: the wire carries
no expected count, and a threshold shipped in the binary would be a number nobody
set, warning a fresh org about a list that is simply short.

**Hourly, and a persistence watch rather than an alarm.** `crons_loop` shares
`usage::PROVIDER_POLL_INTERVAL_SECS` with the Sentry read inside `usage_loop` —
same API, same rhythm, one constant — so a newly-red monitor can be invisible for
up to an hour. That is accepted: the outage that motivated the work ran seven
days, and the daily Slack digest remains the prompt signal. Saving or clearing the
Sentry token, or editing the org slug, wakes both loops; a first read that failed
retries after a minute rather than waiting out the hour. The monitor list is
walked across `Link`-header pages (only a `results="true"` next relation means
there is more — Sentry emits `next` on the last page too), and a list running past
ten pages is a **failure**, because a partial list of monitors reads as "the ones
that are missing are fine".

## The `openclaw` command

The **OpenClaw** panel: a glanceable rollup of an OpenClaw agent farm — per-agent
status, cron health, channel connectivity and token usage.

**An agent is two lines, not one:** the name (with its status dot, emoji and
`running` badge) and the model ref beneath it, indented to the dot column. On
one line the two competed for the width of a quarter-width card and *both* lost
characters; stacked, each gets the whole card. That is what took this panel's
`min_width` from 340 to **240** — measured by shrinking the dumped fixture until
something that must not truncate does, which is now the cron summary rather than
an agent row — and 240 is what lets OpenClaw sit in a quarter beside a
three-quarter Containers on a ~1256pt cockpit. The extra line is free where this
panel lives: it shares a row with Containers and is the shorter card, so
`align-items:stretch` was padding that space out anyway. If OpenClaw ever
becomes the tallest panel in its row, revisit it.

```js
await window.__TAURI__.core.invoke("openclaw");
```

```jsonc
{
  "id": "openclawAgents",
  "title": "OpenClaw",
  "trailing": "3 agents",       // "pairing required" > "N agent(s)" > "connecting…" > "disconnected"
  "message": null,              // or {"text": "no agent runtime configured", …}
  "runtimes": [{
    "id": "openclaw",
    "heading": null,            // "OPENCLAW" once a SECOND runtime exists
    // At most one of the next three, mirroring the Swift panel's if/else chain:
    "pairing": null,            // {title, command, device, blinking, …}
    "connection": null,         // {"text": "connecting…", "dotColor": …}
    "hint": null,               // {"text": "add a gateway URL in Settings → OpenClaw"}
    "agents": {"header": "AGENTS (3)", "rows": [
      {"dot": {"color": "#e09a26", "opacity": 1.0}, "emoji": "🦀",
       "name": "Sebastian", "detail": "anthropic/claude-opus-4-8",
       "trailing": "running"}
    ]},
    "cron": {"header": "CRON (4)", "summary": "2 ok · 1 running · 1 error",
             "dot": {}, "error": {"text": "backup: disk full", "color": "#e05a4f"}},
    "channels": {"header": "CHANNELS (3)", "rows": [{"name": "slack", "dot": {}}]},
    "usage": {"text": "1.2M tokens · ctx 5.0k", "color": "#5a6b60"}
  }]
}
```

**This is the one panel that is not a poll.** Everything beneath it is
[`crates/openclaw`](../crates/openclaw): WS protocol v3, an Ed25519 device
identity, and a frame→snapshot reducer. `src/openclaw.rs` runs one `Session` over
a real socket and rewrites the panel state as frames land — `hello-ok` marks it
connected, a data frame folds through the reducer, and a liveness broadcast
(`health`/`heartbeat`/`tick`) bumps freshness *without* rebuilding a section, or
the snapshot would churn several times a minute for no visible reason. The
command only reads what the socket has already published; the 2s frontend
interval decides how soon the window notices, and drives nothing.

**Which is why there is no staleness warning.** Every other panel here carries a
`status_footer` because it polls and can therefore be stale. This one's
connection line already answers "is this current", exactly; a staleness clock
beside it would be a second, weaker answer to the same question.

**A dot needs both its colour and its opacity.** `unknown` and `disabled` are the
same muted colour — the opacity (0.4 for disabled) is the only thing separating a
channel someone switched off from one nobody has heard from. Both travel, and
`openclaw.js` applies both.

**A token counter the gateway did not report is an em dash, not a zero.** The
gateway sends each counter optionally, so `SessionUsageRollup` carries
`Option<u64>` all the way to `usage_row` and the line reads
`— tokens · ctx —` when nothing was reported (`0 tokens · ctx 0` was #184: a
figure nobody measured, in the one panel with no em dash anywhere). A session
that genuinely burned nothing still says `0`. The line itself stays either way —
`"usage": null`, which drops it, means *there is no session*, and folding the two
together would hide a live session behind the rendering for an absent one.

### Pairing

The gateway authenticates this install by an Ed25519 key it mints on first
connect and stores as **32 raw bytes** in the OS credential store, under account
`openclaw_device_key` — byte-for-byte what the Swift app writes, under the same
account, on purpose: the operator approves one device id rather than one per app.
That is also why the seed is not base64-encoded here. Each app would then read
the other's entry as corrupt and replace it, and the pairing would never settle.

Until the operator approves it, every connect is rejected with `PAIRING_REQUIRED`
and the panel shows the banner — a pulsing amber dot, which kind of approval is
pending, and the **literal** line to paste:

```
openclaw devices approve req-7f31
```

It is rendered selectable in both the panel and Settings → OpenClaw, and it is
built in Rust. A frontend that assembled it from a request id would be a second
implementation of the one string whose entire value is being exactly right.

Reconnect pacing is `openclaw::Backoff`, and the two cases it keeps apart are the
point: an ordinary drop escalates 0.5s → 30s, while a pending approval waits on a
**fixed 15s** and deliberately does not touch the exponential state — a human
being slow says nothing about whether the network is healthy. **Retry now** in
Settings skips that wait, because the operator knows something the app cannot:
that they have just run the command.

### Reconnects keep the rows; a new gateway does not

The reducer outlives a session. A dropped socket is not new information about the
farm, so reconnecting keeps the agent list on screen instead of blanking it and
repainting a second later. Changing the *gateway URL* resets both the reducer and
the published sections — those rows describe a different farm, and carrying them
across would attribute one farm's agents to another.

**Applied without a restart**, and more literally than anywhere else here: the
session is raced against its wake, so saving a URL or a bearer token tears down a
live socket rather than waiting it out. A healthy socket never ends on its own,
so without that race a new gateway would apply only whenever the old one happened
to drop. The bearer token in particular cannot be swapped mid-session — it is
folded into the *signed connect payload*, so it needs a fresh handshake.

## Hosts

Hosts come from [`crates/store`](../crates/store) (`Store::open()` — one JSON
file under the platform config dir), and their bearer tokens from the OS
credential store, never from that file. Each **enabled** host gets its own poll
task, its own `AgentClient` and its own history buffers, so an unreachable host
shows its own error card while every other card stays live; cards are ordered by
name, mirroring the Swift coordinator's `SortDescriptor(\.name)`.

### This machine leads

The first card is the local machine, matching `HostsPanel.hosts` in Swift
(`[local] + remoteHosts`). It is sampled in-process by
[`crates/localhost`](../crates/localhost) on the same 1s cadence, and its
connection dot is **always green**: this process *is* the host, so there is no
link to lose and no staleness to report (`ConnectionState.local`). Its name is
the platform host name minus macOS's cosmetic `.local`, exactly as
`LocalHostMetricsService` derives it.

It is otherwise the *same* card a remote host gets — same charts, same core grid,
same volume bars — because it is built by the same `viewmodel::card::host_card`.
What [`src/local.rs`](src-tauri/src/local.rs) adds is the honest-unknown pass.
`LocalSnapshot::to_wire()` is lossy **by construction**: `wire::Memory::pressure`
and the two rate pairs are bare `f64`s where the sampler has `Option`s, so an
unmeasured pressure lands as `0.0` and would paint a permanently green
"Pressure: 0%". So each field whose *source* was `None` is replaced with the
muted em dash afterwards — driven by matching on the `Option` itself, never by
testing the lowered number for zero. An idle disk really does read `0.0 MB/s`,
and hiding that would be the mirror-image bug.

On macOS today that means memory pressure (no portable source: the Swift
collector reaches into mach for wired and compressed page counts) and the GPU (no
dependency-free read on either platform) render `—` permanently, and the disk and
network rates render `—` for exactly one tick at startup, before there are two
samples to diff. A partially-measured sample is **shown but not plotted**:
pushing the wire lowering's `0.0` into a history buffer would draw a spike from a
floor nobody measured.

"No hosts configured. Add one in Settings." is therefore about *monitored* hosts
and still appears beside the local card on a fresh install.

A failed poll is debounced two ticks before the card stops claiming to be
current, matching `RemoteHostMetricsService.failureThreshold`: one missed poll on
a flappy tailnet is a blip, not an outage.

**Past that, the card goes blank.** A host we can no longer contact renders its
name, a red border and one sentence dating the outage — not its last snapshot
behind a badge, which is what it did until ubu-01 went down during the
2026-08-06 GitHub outage and sat there showing four-minute-old numbers as if
they were now. Every figure on a host card is a present-tense claim; at a glance
a card *is* its figures, and the badge is the part nobody reads. This is the
em-dash rule at card scale rather than per field, and `viewmodel::card` carries
no `Connection::Stale` variant any more so it cannot come back by accident.

The loss is only on screen: `latest` and `histories` stay in state, so the
sparklines return intact the moment the host answers. What survives on the card
is *when* it went quiet, the one fact still true.

In tabs mode a hidden host has nothing on screen but its button, so the **tab
carries the alarm** — red and pulsing (`alert` in the `hostTabs` payload, a
verdict Rust makes rather than a state string the frontend re-reads). And either
way a reachability change fires a **banner**: `services::HostWatch`, the same
transition discipline as the statuspage watch, keyed on the same `error` field
the card renders from so a banner and a red card can never disagree — debounce
included.

### A succeeding poll is not proof the data is current

The card's four states — connecting, live, stale, failed — are facts about
*this* side's polling, and there is one way for all four to be wrong at once.
The agent answers `/v1/snapshot` from a sampler running on its own clock, so a
sampler that has stopped (or has not yet produced its first sample, where
`empty_snapshot()` supplies zeros) is served as a perfectly successful 200. Every
poll succeeds, the dot stays green, and the numbers are frozen — the failure mode
[#182](https://github.com/cpmadrid/solador/issues/182) is named after.

The agent already publishes the answer and nothing consumed it: `/v1/health`
carries `samplerStale` and `sampleAgeSeconds`. So each host with a token is
**also polled for health, every 10s**, alongside its 1s snapshot poll
(`health_loop` in `src/main.rs`), and `samplerStale: true` renders the stale
badge — red, real data, unmissable. This is the **one** case that keeps its
numbers behind a badge, and it earns that: the host is answering, so the figures
are what it is genuinely serving. A host we cannot *reach* blanks instead (see
below). What differs is the message and the clock:

- The message names the **agent**, not the link
  (`viewmodel::card::SAMPLER_STALLED_MESSAGE`). A stalled sampler wants the agent
  restarted; an unreachable host wants the tailnet checked. Sharing one "not
  current" phrasing would send an operator to the wrong layer.
- The age is the **agent's** `sampleAgeSeconds`, not this side's elapsed. Our
  last successful request is about a second old however long the sampler has
  been dead, so a coordinator-side age is precisely the number that makes frozen
  data read as current.

Three rules make this a diagnostic rather than a second failure mode:

- **A failed health poll changes nothing.** It publishes no error, does not
  touch the failure streak, and cannot redden a card whose snapshots are
  arriving. A probe nobody asked for must not gain the power to fail the host it
  was added to describe — the `Err` arm of `record_health` is deliberately not
  written.
- **Withheld is not reset.** A sampler known to be stalled keeps its badge
  through a health poll that fails: a request we could not make is not evidence
  of recovery, and putting the green dot back over frozen numbers is the whole
  defect. Recovery arrives as a health poll that lands saying `samplerStale:
  false`.
- **Unknown is not healthy.** `None` — no health poll yet, or an agent older
  than [#35](https://github.com/cpmadrid/solador/issues/35) that never sends
  the field — leaves the card exactly as the snapshot poll found it, and a
  stalled agent that reports no age gets `last update unknown` rather than a
  fabricated `0s`.

When the link *is* down, the transport failure wins the badge: it is both the
more proximate cause and the fresher fact (`samplerStale` is by then up to a
health cadence old), and naming the sampler would send someone to restart a
daemon they cannot reach.

**The Swift app has the same gap and is deliberately untouched here** —
`RemoteHostMetricsService` decodes `samplerStale`/`sampleAgeSeconds` into
`HealthInfo` and uses them only for the Settings → Test result line, so its cards
still show a stalled agent as live.

## Settings

The **Settings** button opens an in-app view over the cockpit: General, Layout,
GitHub, Portfolio, Hosts, Azure Cost, Usage, OpenClaw and About — the Swift
window's tabs plus **Layout**, which has no Swift counterpart. Every label, help
string and result line it paints comes from `src/settings.rs`, exactly as the
cards' do from `crates/viewmodel`.

**In-app view, not a second window.** A second window means the frontend calls
`WebviewWindow`, which means granting the webview
`core:webview:allow-create-webview-window` (or `core:default`) —
widening the one seam in this app with no automated coverage
([#123](https://github.com/cpmadrid/solador/issues/123)), for a surface that
needs no platform capability at all. Every command below is *app-defined*, which
Tauri's ACL permits without a grant, so none of them appears in
`capabilities/default.json` — see [The one granted
capability](#the-one-granted-capability) for the single entry that does.

| Command | What it does |
|---|---|
| `settings_view` | the whole surface, including a `stored: bool` per credential |
| `settings_save_general` | refresh interval, core-row span |
| `settings_move_panel` / `settings_set_panel_span` / `settings_set_breakpoint_overflow` | the [cockpit layout](#the-layout-tab), inside one breakpoint named by `minWidth`: one panel a place along the order, one panel's width, or that band's host-overflow mode |
| `settings_add_breakpoint` / `settings_remove_breakpoint` / `settings_reset_layout` | add a width band (seeded from the one it splits), drop one (never the last), or forget the arrangement entirely |
| `settings_save_providers` | Neon org id + rates, Sentry slug + quota, Azure budget (every non-secret provider preference in one go) |
| `settings_add_host` / `settings_remove_host` / `settings_set_host_enabled` | hosts CRUD; add files the token, remove deletes it |
| `settings_unhide_volume` | one mount, on a host or on the local list |
| `settings_add_container_rule` / `settings_set_container_rule` / `settings_remove_container_rule` | the [container group rules](#the-containers-command), by index — one **field** per call |
| `settings_test_host` | one `/v1/health` probe → the Swift result line |
| `settings_add_repo` / `settings_remove_repo` / `settings_set_repo_enabled` / `settings_set_repo_workflows` | the tracked-repo portfolio |
| `settings_save_openclaw` | the OpenClaw gateway URL |
| `settings_openclaw_retry` | reconnect now, instead of waiting out the pairing backoff |
| `settings_save_secret` / `settings_clear_secret` | one credential, by key (`github`/`neon`/`sentry`/`azure`/`openclaw`) |

**Every mutation wakes exactly the loop its data feeds, and no other.** A wake
spends a whole poll pass, so nudging the GitHub loop after a Neon save would burn
a portfolio fetch on a credential it has no use for.

| Edit | Wakes |
|---|---|
| the portfolio, the refresh interval, the `github` credential | the GitHub loop ([Repos + Runners](#the-repos-and-runners-commands)) |
| the refresh interval | …and the usage loop's Claude half, which shares that cadence |
| the `neon` / `sentry` credential, the Neon org id, the Sentry slug | the usage loop, *forcing* its hourly provider half onto that pass |
| the `azure` credential | the Azure loop |
| the gateway URL, the `openclaw` credential, **Retry now** | the OpenClaw session — cutting short the *session*, not a sleep |
| the Sentry quota, the Azure budget, the two Neon rates | nothing — all four are read at render time |
| a container group rule | nothing — the rules are read at render time too, by `containers` |
| the [layout](#the-layout-tab) — a move, a width, an overflow mode, a breakpoint added or removed, a reset | nothing: `cockpit` re-reads the bands and picks one by the measured width on every frame, so the change is on screen a second later (or the moment Settings closes, which repaints immediately) |
| a host added / removed / disabled | nothing; `reload_hosts` reconciles poll **tasks** instead |

#### Waking up with the machine

**A resume is the one caller allowed to wake all four at once**, and it is not a
mutation. Closing a laptop suspends every poll task, so the four slow loops —
GitHub, usage (1h), Azure (4h), OpenClaw — otherwise resume on their own
schedule and the cockpit paints last night's data for up to a full interval
after the lid opens. The rule above holds for *edits* because an edit changes
one source; a resume is every source at once becoming untrustworthy, so
`resume_loop` ([`src/resume.rs`](src-tauri/src/resume.rs)) fires the lot.

It notices by watching two clocks rather than by asking the OS: it samples
`Instant` and `SystemTime` every **2s**, and calls it a resume when the wall
clock has run ≥ 20s further than the monotonic one. That is chosen over
`NSWorkspace.didWakeNotification` for being correct whichever way macOS's
monotonic clock behaves — if it excludes sleep the gap appears and the loops are
nudged; if it includes sleep their own deadlines have already passed and tokio
fires them unaided, the clocks agree, and this stays quiet. It also needs no
platform code, so Windows gets it for free. A wall clock corrected forward by
NTP reads as a resume too; the cost of that is one extra poll of each source,
which is the harmless direction to be wrong in.

The 1s and 10s loops need nothing — they self-heal within a tick, and
`poll_loop` is spawned without an `Arc<App>` to be woken through anyway. But the
host **watch** is re-seeded first (`HostWatch::reset`): the tailnet takes a few
seconds to come back, so without it every lid-open would banner an
"unreachable" and then a "back online" for every host. `StatusWatch` is
deliberately *not* reset — a vendor that changed state overnight is a transition
worth hearing about, and one that broke and recovered while we slept compares
equal and stays quiet on its own.

Every mutation answers in one shape — `{status, settings}` — and the frontend
re-renders from the `settings` it gets back rather than patching its own copy,
so it can never show an edit that failed to save.

### The Layout tab

Where each panel sits, how wide it is, and **at which window widths**. The
arrangement persists as `store.json`'s `layout`: a list of **breakpoints**, each
one an ordered list of `{panel, span}` slots plus its own `hostOverflow`.

```jsonc
"layout": [
  { "min_width": 0,    "host_overflow": "tabs",  "slots": [ /* … */ ] },
  { "min_width": 1816, "host_overflow": "stack", "slots": [ /* … */ ] }
]
```

`cockpit` picks the widest band the measured width clears
(`settings::breakpoint_for`) on **every frame**, so a cockpit parked in a third
of a 4K display can tab its host cards while the same window maximised lays them
side by side. Below every authored band the narrowest one still applies — there
is always something to render. The mode is per band and not global for exactly
that reason; `Settings.host_overflow_mode` survives in the store as the *seed* a
pre-breakpoint layout is migrated from, and the General tab no longer offers it.

Inside a band the arrangement is an **ordered list of slots**, not rows — rows
are packed from it (`CockpitLayout::from_order`, four quarters to a row), so
moving a panel is one operation wherever it sits and no editor has to ask where a
row breaks. `reflow` still runs on top at render time, so a window too narrow for
a band's rows splits them exactly as it always did: a breakpoint is the widest
arrangement for its band, not a promise about every size inside it.

Three rules make a stored layout always renderable, and all three live in
`settings::normalized_order` — applied on the way in *and* on the way out, like
`normalized_general`:

- a slot naming a panel or span this build does not know is **dropped** (a file
  from a newer build must not move someone's cockpit around by proxy);
- a panel named twice keeps its **first** slot;
- a panel named nowhere is **appended** with its default span.

So every band always holds every panel exactly once. That is what makes a *new*
panel appear for an existing user — in its default place — instead of vanishing
because their saved layout predates it. `settings::breakpoints` does the same for
the bands themselves: widths below zero clamp to 0, two bands claiming one width
keep the first, and an empty list becomes the shipped default, so
`breakpoint_for` can never come up empty.

**Migration is a shape, not a version bump.** Before breakpoints, `layout` was a
bare slot array. `store::layout::lenient_layout` still accepts that and reads it
as one band at width 0 with an *empty* `host_overflow` — which
`settings::breakpoints` then fills from the General preference that used to own
the decision. Upgrading changes nothing on screen, and the first Layout edit
writes the migrated shape back. The discriminator between the two shapes is
`slots` being required on a profile and `panel` on a slot: without those, every
field is optional and each shape would parse as a degenerate version of the
other.

`layout: null` (never configured) and a stored layout that happens to match the
default are deliberately distinct: only the absent one follows a future change to
`CockpitLayout::DEFAULT_ORDER`, which is what **Reset to default** stores — it
*clears* the key rather than writing today's default into it. **Remove
breakpoint** refuses to take the last band, because a cockpit with no arrangement
is not a state the editor may produce; **Add breakpoint** seeds the new band from
whatever already applied at that width, so adding one changes nothing until it is
edited.

Every edit names its band by `minWidth` rather than by index — adding a band
re-sorts the list, and an index would then address the wrong one. Which band the
editor is *showing* is frontend state (`S.band`), never persisted: the whole band
list travels in the payload so switching is not a round trip.

The tab's preview is Rust's packing, delivered as rows of cells carrying the `fr`
weight each panel gets — the same numbers `panelRows` carries. A frontend
deriving "what will this look like" from the spans would be a second
implementation of the packer, free to promise an arrangement the cockpit then
does not render.

**The rules editor writes one field per call, and that is the whole of its
concurrency story.** Swift builds a `Binding` per `WritableKeyPath` that
re-reads the persisted list on *every* access, precisely so editing a rule's
label cannot clobber the pattern someone changed a moment earlier. The port is
`settings_set_container_rule(index, field, value)`: the frontend sends the field
that changed and nothing else, and Rust does the read-modify-write of the whole
list under the store's lock. A whole-row command would have been a client-side
snapshot of four fields, which is the bug that shape exists to avoid. Rows are
addressed by **index** because the persisted model has no id and order *is* the
engine's contract (first match wins); an index that no longer names a rule is a
rejected edit — `Skipped — unknown rule.` — never a misdirected one.

Two rules of the editor are Rust's, not the field's: the group label and the
expected count exist **only for a Collapse rule** (`collapseOnly` in the
payload — Hide renders no row and Expect's row is the entity's own name), and an
expected count that is empty, zero, negative or not a number **clears** the
expectation rather than becoming `0`. An expectation of zero is no expectation,
and `×0/0` would be a figure nobody typed.

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

- **One of the two General preferences is consumed; the core row span is not.**
  `refresh_interval_secs` is the GitHub panels' *and* the Claude usage
  rollup's cadence, and changing it applies immediately (see below). Neither the
  host poll loop nor the provider reads are on it: the host loop stays at 1s
  because one history sample is one fixed time slice
  (`PX_PER_SAMPLE`), so that cadence is part of the charts' time axis rather
  than a preference, and Neon, Sentry and the Azure export publish on the order
  of hours or days, so each keeps its own fixed cadence. The **core row span**
  is still read by `viewmodel`'s card functions from their own constant; it
  persists (same file, same key, same laundering rules as Swift) and nothing
  reads the stored value yet. `host_overflow_mode` left this tab for the
  [Layout tab](#the-layout-tab), where it is one value per breakpoint; the
  stored field remains only as that migration's seed.
- **About's version is hard-coded** to the crate version, not the CalVer the
  Swift app derives from git ([#15](https://github.com/cpmadrid/solador/issues/15)),
  and the About links render as selectable URLs rather than anchors — following
  one would navigate the cockpit's own webview away from the app, and the opener
  scope granted below deliberately does **not** reach them. They are repo roots
  and issue pages; the grant admits `/{owner}/{repo}/actions` and nothing else,
  so making those links openable would be a second widening, argued separately.

## The one granted capability

`src-tauri/capabilities/default.json` grants the cockpit window **one** plugin
command. Everything else it calls is app-defined, which Tauri's ACL permits
without a grant. Here is the whole `permissions` list, line by line:

```jsonc
"permissions": [
  {
    "identifier": "opener:allow-open-url",              // 1
    "allow": [{ "url": "https://github.com/*/*/actions" }]   // 2
  }
]
```

1. **`opener:allow-open-url`** — the `plugin:opener|open_url` command, and only
   it. `tauri-plugin-opener` also ships `allow-open-path` (opens a file or
   directory with the system handler) and `allow-reveal-item-in-dir`; neither is
   granted, so the webview can reach no path on this machine. The plugin's own
   `opener:default` bundles all three, which is why it is not used here.
2. **`allow: [{url}]`** — a *scope* on that one command. The plugin compiles the
   string into a `glob::Pattern` and rejects any `open_url` whose URL does not
   match ([`src/commands.rs`](https://docs.rs/tauri-plugin-opener) →
   `scope.is_url_allowed`), so the grant is "this shape of URL", not "URLs". The
   glob is the tightest static expression of the Repos row's target: the scheme
   and host are literal, and the two `*`s are the owner and repo, which are
   user-editable at runtime and so cannot be enumerated in a file compiled at
   build time. Omitting the entry's optional `app` key leaves it at
   `Application::Default`, which additionally means the caller cannot name
   *which* program opens the URL.

**What is deliberately absent:**

- **`core:default`.** Never granted, and this is not an oversight: it hands the
  webview every core-plugin default — window, webview, app, event, menu, tray,
  image, path, resources — none of which this frontend calls.
- **Anything for `tauri-plugin-notification`.** It is a dependency and is
  registered with `.plugin(...)`, but the ACL grants it *nothing*. Needs-approval
  banners are built and shown in Rust (`deliver_approval_notices` →
  `NotificationExt`), which does not pass through the ACL at all, so the webview
  is never in the path and needs no permission to be in it. The plugin's
  `notify` / `request_permission` / `is_permission_granted` commands remain
  unreachable from JavaScript.

**How far it is verified.** `actions_url_is_the_only_shape_the_granted_scope_admits`
(in `src/github/mod.rs`) reads the real capability file, rebuilds the glob with
the same `glob::Pattern` the plugin enforces it with, and asserts it admits every
URL `github::actions_url` can produce and refuses a list it must not — including
the About tab's own links, `http://` instead of `https://`, and
`https://github.com.evil.example/…`. Widening the scope fails that test.

What the test does **not** do is exercise the IPC boundary that enforces the
scope: it reads the file, it does not invoke through it. Nothing automated does
— that is still [#123](https://github.com/cpmadrid/solador/issues/123), and
it is why the checklist below grew a tap-to-open line.

## Build & run

There is **no Tauri CLI in this repo**: no `cargo-tauri` dependency, no
`package.json` under `app/`, and `tauri.conf.json` points `frontendDist` at the
static `../ui` directory with no `beforeDevCommand`. There is nothing for
`tauri dev` to do that plain cargo does not, so the front door is the repo's own
entry point:

```bash
./dev run --tauri                   # from the repo root; --release composes
```

That builds the same package plain cargo does and then, on macOS, re-signs the
binary with the stable `Apple Development` identity (team `52YMXC3348`) before
launching it. That step is the whole reason to prefer it: cargo stamps a *fresh
ad-hoc* signature on every relink, and each new identity invalidates the Keychain
ACLs on the app's stored credentials — so a bare-cargo launch re-prompts for every
stored item on every rebuild. Where no identity is installed (CI, a non-macOS
machine) the step is skipped silently and you get the bare-cargo behaviour.

The bare command still works and is what everything non-interactive uses:

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

| Env var                     | Default                        | Meaning                                                                                     |
|------------------------------|--------------------------------|---------------------------------------------------------------------------------------------|
| `DEVCANOPY_SEED_HOST`       | —                              | `"name\|address\|port\|token"`. Provisions that host **if no host with that address exists**; port defaults to 7878 and the token (when non-empty) goes to the OS credential store under the new host's id. Same parse and same no-op rule as Swift's `RemoteHostsCoordinator.seedFromEnvironmentIfNeeded()`, so it is safe to leave exported — relaunching never accumulates duplicates. |
| `DEVCANOPY_STORE_DIR`       | platform config dir            | Where `store.json` lives. A scratch directory here keeps `store.json` — and only `store.json` — out of the real one. **Not** the keychain: the credential *service* is always the real one, so credential migration is skipped whenever this is set (see "Consolidated credential item" below); a scratch run touches no keychain item at all beyond whatever per-item reads/writes the panels themselves make. |
| `DEVCANOPY_LEGACY_SECRETS`  | unset                          | Set to `1` to skip credential migration and force per-item keychain routing, even on macOS — the rollback switch for consolidation. See "Consolidated credential item" below. |

Tokens live in the OS credential store (service `com.sassydog.devcanopy`), never
in `store.json`. Account `host-<uuid>` is the storage key either way, but what
that means depends on platform and migration state: pre-migration, on any
non-macOS target, or under `DEVCANOPY_LEGACY_SECRETS=1`, it names its own
keychain item; on macOS once migrated, it is one key inside the consolidated
`secrets_v1` item (see "Consolidated credential item" below). An empty token
never leaves the process, so it gets its own message — *"No agent token
configured for this host. Add one in Settings."* — rather than reusing the
agent's 401 text and sending you to check the wrong layer.

### Consolidated credential item

On macOS, every text credential above — host tokens, the GitHub PAT, the Neon
and Sentry usage keys, the OpenClaw bearer token — lives
in one keychain item: service `com.sassydog.devcanopy`, account `secrets_v1`,
value a JSON map keyed by the same account strings each credential used to have
its own item under. One item means one keychain ACL prompt covers every
credential this app stores, rather than a fresh "Always Allow" per secret. One
exception keeps its own item regardless of platform: the OpenClaw *device*
identity key — raw key material, not text, and an account the Swift app also
writes to directly.

There used to be a second. The Azure Cost SAS URL was written from outside this
process by a LaunchAgent that re-minted it every four days, so a blob copy
shadowed every refresh with a frozen one (it did — the panel read a
migration-time SAS until it expired). That whole arrangement is gone: the app
mints its own SAS per poll from the operator's Azure CLI session and stores
nothing at all. `migrate_legacy` still scrubs a stale `azure_cost_sas_url`
entry out of an upgraded install's blob, because nothing else would — see
`RETIRED_AZURE_SAS_ACCOUNT` in `crates/store/src/secrets.rs`.

The first launch after this landed, and every launch since that finds no
`secrets_v1` item, copies every legacy per-item secret into that blob once
(`migrate_legacy`, called at startup before anything can write a secret) and
never touches the legacy items again — they are left in place, intentionally
stale, as the blob's rebuild source. New and edited secrets go only to the blob
from then on, so a legacy item's value silently drifts from whatever the blob
holds.

**If the blob is ever damaged** (unparseable JSON — the affected panel keeps its
last-good figures and shows a "couldn't read the credential store" footer
rather than losing anything): delete the `secrets_v1` item in Keychain Access
and relaunch. Migration rebuilds it from the legacy items, which means the
rebuild restores *migration-time* values, not current ones — a credential
rotated after migration (a re-issued PAT, a rotated SAS URL) reverts to its old
value on rebuild, and needs re-saving in Settings afterward.

**macOS only.** `keyring`'s Windows Credential Manager backend rejects a
credential blob over 2560 bytes (`TooLong`); per-item storage never approached
that limit, but a shared JSON blob crosses it at roughly five to six hosts,
after which every save and delete would fail (the read-modify-write rewrites
the whole map). The ACL-prompt problem consolidation exists to fix is
macOS-specific too, so every other platform keeps the pre-consolidation
one-item-per-secret scheme unchanged.

**Escape hatch:** set `DEVCANOPY_LEGACY_SECRETS=1` to skip migration and force
per-item routing even on macOS — abandoning consolidation is one env var, not
deleting a keychain item before every launch.

### Offline fixtures

```bash
cargo run -p devcanopy-app -- --dump sample.json                 # one live host
cargo run -p devcanopy-app -- --dump-unreachable sample-unreachable.json
#   …the link is down on a host we used to reach: a BLANK card, which is what
#   `view_for` produces for it. No figures survive.
cargo run -p devcanopy-app -- --dump-sampler-stale sample-sampler-stale.json
#   …the poll SUCCEEDED and the card is stale anyway: the agent's own
#   `/v1/health` says its sampler stopped, dated by the agent's clock.
cargo run -p devcanopy-app -- --dump-cockpit sample-cockpit.json # three hosts: live / stale / failed
#   …plus `--width <pt>` (which column count to compute), `--hosts <n>` (how
#   many of the three to include; 0 is the unconfigured cockpit) and `--tabs`
#   (the "Show as tabs" overflow mode, which only changes the payload at a
#   width where the cards were going to stack anyway).
cargo run -p devcanopy-app -- --dump-settings sample-settings.json # the Settings surface
cargo run -p devcanopy-app -- --dump-containers sample-containers.json # the Containers panel
#   …plus `--empty`, which dumps the no-runtimes state with a failed-tool footer.
cargo run -p devcanopy-app -- --dump-repos sample-repos.json         # the Repos panel
cargo run -p devcanopy-app -- --dump-runners sample-runners.json     # the Runners panel
#   …both take `--empty`, which dumps the no-credential state.
cargo run -p devcanopy-app -- --dump-usage sample-usage.json         # the Usage panel
#   …plus `--unmeasured` (both providers answering, neither measuring: the em
#   dash path, with the quota set and the bar therefore suppressed) and
#   `--empty` (no summary, no provider configured).
cargo run -p devcanopy-app -- --dump-azure sample-azure.json         # the Azure Cost panel
#   …plus `--fallback` (the rollover gap: amber caption, month stamped),
#   `--error` (red, an expired SAS) and `--empty` (no SAS URL at all).
cargo run -p devcanopy-app -- --dump-openclaw sample-openclaw.json   # the OpenClaw panel
#   …plus `--pairing` (the banner with the approve command), `--error` (a
#   rejected handshake, red), `--idle` (no gateway URL: the muted Settings
#   hint), `--empty` (no runtime at all) and `--unmeasured` (the same live farm
#   whose session reported no token counters: `— tokens · ctx —`).
```

`--dump-settings` is a `settings_view` payload built from a fixed configuration
(one enabled host with a token and a hidden volume, one disabled host with
neither; two credentials stored, two not) with hard-coded uuids, so it is
byte-stable across regenerations and covers both sides of every badge. Its
**container group rules** are the seeded three plus the two renderings seeding
alone never reaches — an Expect rule (whose Collapse-only fields must therefore
be absent) and a scope naming a host that is not configured (the case the host
picker grows an extra option for, and the one where it renders blank if it
doesn't) — all asserted by
`the_settings_fixture_covers_every_rule_rendering_the_editor_has`.
`--dump-containers` is the same idea one panel over: a hand-made state at a
**fixed** timestamp (a relative age like "recycling 40s" would otherwise drift
on every dump and no test could assert one), covering a present container, a
stopped one, a VM recycling, one missing past grace, and a collapsed group on a
remote section. `--dump-repos` / `--dump-runners` are the same idea again, and
their state is asserted by a Rust test (`the_fixture_covers_every_rendering_
the_panels_have`) precisely so the Playwright suite cannot pass against a
payload that quietly lost the case it claims to exercise — it carries an
unknown count beside a genuine zero, an approval gate, a failing repo, an
unreachable one, and remembered runners in both absence states.
`--dump-usage` / `--dump-azure` / `--dump-openclaw` carry the same guard
(`the_fixtures_cover_every_rendering_the_panel_has`, one per module): between
them they pin the quota bar's amber step, its suppression when the count is
unknown, the em dash beside a measured figure, the amber rollover caption, the
muted-setup-vs-red-failure split, and — for OpenClaw — a disabled dot beside an
unknown one at the same colour, the approve command, the four trailing labels,
and a session whose token counters went unreported beside one whose did not
(the em dash the panel used to render as a fabricated `0`, #184). `--dump-settings` is also dumped **mid-pairing** on purpose: the Device
Pairing block is the only part of Settings built from live session state, so a
fixture without one would leave it uncovered. The rest are full `cockpit` payloads — the same shape the command returns,
so the offline path cannot diverge from the real one — built from the committed
agent-contract fixture, so they reproduce on a clean checkout with no agent
involved. Their **local card is hand-made** at a fixed shape for the same
byte-stability reason, and it carries the em dashes the shipped card really does
(pressure, GPU) so the Playwright suite asserts that rule against Rust's own
output. `npm test` in `tests/frontend` writes them all under `app/ui/` (all
gitignored) — which matters for the smoke test below.

## Manual IPC smoke test

**Nothing automated exercises the Tauri IPC boundary**
([#123](https://github.com/cpmadrid/solador/issues/123)). Both sides of the
seam are tested and the seam itself is not: the Rust tests call
`cockpit_view(…)`, `settings::view(…)`, `containers::view(…)`, `usage::view(…)`,
`azure::view(…)` and `openclaw::view(…)` directly rather than through their
`#[tauri::command]`
wrappers, and the Playwright suite stubs `window.__TAURI__.core.invoke` — every
command alike — with Rust-dumped JSON. A break in the ACL
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
harness for what was then a walking skeleton, covering only the Windows build
path. Trade-off accepted: ACL and command-registration regressions are
**documented, not prevented**.

**That rationale is now weaker than it was.** It was priced against one command
and one card; the surface it leaves unguarded is now nine commands and eight
panels, and the checklist below has grown with it. The trade-off still holds —
`tauri-driver` still has no macOS support, so the cost side has not moved — but
"revisit if the skeleton graduates" has arguably come due. Re-priced during the
#150 close-out audit (#178) and left standing; the deferred register there names
it.

**And weaker again since [#187](https://github.com/cpmadrid/solador/issues/187).**
Until then the ACL's `permissions` list was empty, so "an ACL break" could only
mean *losing* access to app-defined commands — a failure that blanks a panel and
is therefore loud. There is now [one granted
capability](#the-one-granted-capability), and a break in *its* direction is the
quiet kind: a scope that admits more than it should changes nothing on screen.
The unit test named there is what covers the file; step 11 below is what covers
the boundary, and it is a human clicking a row.

So this is a manual step. Run it after changing anything under
`src-tauri/capabilities/`, the `invoke_handler` registration, or any frontend
`invoke` call — including the settings ones, which is what step 5 is for.

### The five-minute checklist

Every command's boundary check, in one pass, with no credentials and no live
agent. This is the whole test for a routine change; the annotated procedure
below explains *why* each line is the signal, and the credentialed and live-agent
paths are the ⏱ extras at the end.

```bash
rm -f app/ui/sample*.json                      # 1. fixtures MUST be gone
DEVCANOPY_STORE_DIR=$(mktemp -d) \
DEVCANOPY_SEED_HOST="smoke-$(date +%H%M%S)|100.100.100.100|7878" \
  cargo run -p devcanopy-app                   # 2. scratch store, distinctive name
```

Then tick these off. Eight terminal lines and four on-screen reads — the terminal
half works on a machine whose screen you cannot see.

- [ ] **Terminal** — `cockpit: first frontend request (1 host(s), <N>pt)`
- [ ] **Terminal** — `containers: first frontend request (N section(s))`
- [ ] **Terminal** — `repos: first frontend request (N repo row(s))` — **0 is a pass**
- [ ] **Terminal** — `runners: first frontend request (N runner row(s))` — **0 is a pass**
- [ ] **Terminal** — `usage: first frontend request (N provider section(s))` — **0 is a pass**
- [ ] **Terminal** — `azure_cost: first frontend request (headline: false)` — **false is a pass**
- [ ] **Terminal** — `crons: first frontend request (nothing read yet)` — that wording
      **is** the pass with no Sentry token; `all ok` or `N not ok` with one
- [ ] **Terminal** — `openclaw: first frontend request (trailing: "")` — **empty is a pass**
- [ ] **Screen** — the **local card** leads the host grid with this machine's name, a
      green dot, CPU/memory changing between ticks, and on macOS `Pressure: —` and
      `VRAM: —`. Those em dashes are the check, not a defect.
- [ ] **Screen** — the seeded host card shows the name you passed, then either live
      figures or one of the two named failure sentences (see [Pass](#pass)).
- [ ] **Screen** — **Containers / VMs** shows a `N total · N up · N stopped` line, or
      the sentence `no container runtimes`. Allow 10s — that is the panel's cadence.
- [ ] **Click Settings** → terminal prints
      `settings: first frontend request (N host(s), N repo(s))`, the Hosts tab lists
      your seeded host **and the three seeded container group rules below it**, and
      **Done** returns to the cockpit.

All twelve ticked ⇒ every registered command round-tripped through the ACL and the
IPC transport. **A zero, a `false` or an empty string is a pass**: those are Rust's
own unconfigured sentences, and none of them has a path to the DOM except a
successful `invoke`. What fails this test is a *missing line* or a blank panel.

⏱ **Extras** — only when you touched that surface. Each applies without a relaunch,
and that immediacy is itself the check on the corresponding wake:

| Touched | Do | Expect |
|---|---|---|
| `github_wake` / Repos / Runners | save a fine-grained PAT under Settings → GitHub | both panels fill within seconds; **Clear** drops them back just as fast. `—` (not `0`) under LOCAL/WT for a repo absent from `~/Repos` |
| **the ACL** (`capabilities/`), `github::actions_url`, github.js | with the Repos panel populated, **click any repo row** — then **Tab** to one and press **Enter** | your default browser opens `https://github.com/{owner}/{repo}/actions`. Nothing happens ⇒ the grant or the scope is wrong; the webview console names the rejected URL. **This is the only check on the granted scope at the boundary** — step 11 |
| the needs-approval notifier | with a PAT saved and the panel already populated, add a repo that has a run **parked at a deployment-protection gate** under Settings → Portfolio | one banner, `{repo} · needs approval`, within seconds. It must **not** repeat on later passes, and adding a repo with no gate must produce nothing — step 11 |
| `settings_test_host` | press **Test** on the seeded host | `✓ <host> · agent v<version>`, or `✗ unreachable …`, or `✗ auth failed (401) …` with no token |
| the rules editor | under Settings → Hosts, press **Add Rule**, set its action to **Hide**, then **Delete** it | the row appears with an empty pattern; switching to Hide drops the group-label and expected-count fields; the status line reads `Added rule.` / `Saved.` / `Removed rule.` |
| the tabs mode, per breakpoint | with two hosts configured, set Settings → **Layout** → *Any width* → **Show as tabs**, **Done**, then narrow the window below ~1816pt | a tab bar appears above one card and the others go off screen; widening past the breakpoint puts them all back with no bar left behind. Add a breakpoint at **1816** and set it to *Stack* to prove the band, not the window, is what decides |
| `settings_move_panel` / `settings_set_panel_span` / `settings_reset_layout` | under Settings → **Layout**, set **Usage** to *Full width*, press **Move up** once, then **Done** | the preview re-draws under each edit (`Saved.` on the status line), and the cockpit shows the new arrangement the moment Settings closes. **Reset to default** — enabled only once you have edited something — puts it back. A change that survives the preview but not the close means `cockpit` is not re-reading the store |
| `settings_add_breakpoint` / `settings_remove_breakpoint` | in Settings → **Layout**, type `1816` under *Applies from (pt)* and press **Add**, edit the new band, then **Remove breakpoint** | the switcher gains `1816pt and up`, selected, holding a copy of what applied there; editing it leaves *Any width* untouched (switch back and check). With one band left **Remove breakpoint** is disabled |
| usage providers | save a Neon org key and/or Sentry `org:read` token | sections appear in seconds. A key with **no org id** renders `—` on both figures, never `0.0 CU-h` |
| `openclaw_wake` | put a gateway URL under Settings → OpenClaw, **Save** | `connecting…` (amber) within a second or two; then the pairing banner or green AGENTS/CRON/CHANNELS rows |
| a live agent | re-run step 2 with `\|$TOKEN` appended to `DEVCANOPY_SEED_HOST` | the host card fills with live figures and a green dot |

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
   independent discriminator, since the fixture hard-codes `ubu-01` and so does
   every seeded example. `DEVCANOPY_STORE_DIR` is what makes the run repeatable:
   seeding is a no-op when the address is already configured, so a smoke run
   against the *real* store would silently reuse the host from last time — and its
   name — instead of the one you just passed.

   ```bash
   DEVCANOPY_STORE_DIR=$(mktemp -d) \
   DEVCANOPY_SEED_HOST="smoke-$(date +%H%M%S)|100.100.100.100|7878|$TOKEN" \
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
   DOM except a successful `invoke`. Expect `loading…` for the first moment
   either way: until a pass has read the keychain the panels cannot say whether
   a token exists, and the setup instruction appears only once one has looked. What the counts add is the *second* thing:
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

8. **Read the terminal a last time** for the Usage and Azure Cost panels' own
   one-line signals (both print at load, alongside the others):

   ```
   usage: first frontend request (0 provider section(s))
   azure_cost: first frontend request (headline: false)
   ```

   **Both zero/false is still a pass for the boundary.** With no Neon key and no
   Sentry token in the scratch keychain the Usage panel is its Claude-only self —
   the provider sections are *absent*, deliberately, not blank — and with no SAS
   URL the Azure panel is the single sentence `Add an Azure Cost SAS URL in
   Settings`. That sentence is `azure::UNCONFIGURED_MESSAGE`'s and has no path to
   the DOM except a successful `invoke("azure_cost")`. It is preceded by
   `reading export…` until the first pass reads the keychain — same rule as the
   GitHub panels above.

   The Usage panel is the one surface here that needs **no credential at all** to
   populate: it walks `~/.claude/projects` on this machine. On a machine that has
   run Claude Code, expect `5H` and `WEEK` token counts, a `TOP PROJECTS (7D)`
   list, and an `N today` trailing label within one refresh interval. On one that
   has not, expect `no ~/.claude/projects` in the footer with `no usage data`
   above it — also a pass, and the two are different Rust sentences.

   To exercise the provider half, save a Neon org API key (plus its org id under
   Settings → Usage) and/or a Sentry `org:read` token and slug. Both apply
   without a relaunch — `settings_save_secret` wakes the usage loop *and* forces
   its hourly half — so the sections should appear within seconds. That
   immediacy is itself the check on the wake. A key with no org id is the
   interesting case: the section appears, both figures render `—`, and the footer
   says `Add your Neon org ID in Settings` — never a fabricated `0.0 CU-h`.

9. **Read the terminal for the OpenClaw panel's own one-line signal** (it prints
   at load, alongside the others):

   ```
   openclaw: first frontend request (trailing: "")
   ```

   An **empty trailing label is a pass for the boundary.** With no gateway URL
   in the scratch store the session never starts, the runtime stays *idle*, and
   the panel is the single muted sentence `add a gateway URL in Settings →
   OpenClaw` — which is `openclaw::IDLE_HINT`'s and has no path to the DOM
   except a successful `invoke("openclaw")`. Idle is deliberately not
   "disconnected": nothing was attempted.

   To exercise the live path, put a gateway URL under **Settings → OpenClaw**
   (`ws://host:7878` or `wss://host`) and press **Save**. It applies without a
   relaunch — the save cuts the *session* short, not a sleep — so the panel
   should move to `connecting…` (amber) within a second or two, and that
   immediacy is itself the check on `openclaw_wake`.

   The first connect against a gateway that has never seen this device is the
   interesting one: expect the amber **pairing banner**, `device pairing
   required`, and a selectable `openclaw devices approve <requestId>`. Run that
   command on the gateway host, then press **Retry now** in Settings rather than
   waiting out the 15s pairing backoff — the panel should go green and fill with
   AGENTS / CRON / CHANNELS rows. A gateway that rejects the upgrade instead
   (`controlUi.allowedOrigins` not permitting this host) shows a **red**
   connection line naming the rejection, which is the right answer and also a
   pass for the boundary.

   Settings → OpenClaw should show a 64-character **Device ID** once a key
   exists, or `Device key is generated on first connect.` before one does — and
   never a blank row, which would claim an identity that has not been minted.

10. **Read the local card**, at the head of the host grid. It should carry this
   machine's name (hostname minus `.local`), a green dot, live CPU and memory
   that change between ticks, and — on macOS — `Pressure: —` and `VRAM: —`. Those
   two em dashes are the point: neither figure has a portable source, and the
   card says so rather than painting a permanently green 0%. Disk and network
   read `—` for the first tick only, then real rates.

11. **Exercise the two seams that leave the app** — the only two, and the only
    ones that need a credential. Both are ⏱ extras, not part of the
    credential-free pass above, but **step 11a is not optional after any change
    under `src-tauri/capabilities/`**: it is the sole check that the granted
    scope is enforced at the boundary rather than merely written in a file.

    **11a — tap to open (the ACL).** With a PAT saved (step 7) and the Repos
    table populated, click any repo row. Your default browser should open
    `https://github.com/{owner}/{repo}/actions` for that repo. Then press
    **Tab** until a row takes the focus ring and press **Enter** — same result,
    because a click-only target is one a keyboard cannot reach.

    Nothing happening is the failure, and it has exactly two shapes worth
    telling apart: **Inspect Element** → Console shows
    `opener.open_url not allowed` (the permission is missing from
    `capabilities/default.json`) or a `Forbidden URL` error naming the URL (the
    permission is there and its *scope* rejected it). A row that is not
    clickable at all is neither — that is `row.url` missing from the payload,
    i.e. a Rust-side regression the unit tests should have caught.

    The negative half cannot be clicked, only reasoned about: the granted scope
    admits `/{owner}/{repo}/actions` and nothing else, which
    `actions_url_is_the_only_shape_the_granted_scope_admits` asserts against the
    real file. The About tab's links are the visible proof — they render as
    selectable text and stay unopenable.

    **11b — a needs-approval banner.** The transition rule is unit-tested
    (`src/github/notify.rs`); what is not is delivery. Forcing a real transition
    without a real gate is the awkward part, and the trick is that the watch is
    **per process and already seeded** by the pass in step 7: any repo added
    *after* that is diffed against a baseline that never contained it.

    So — with the panel already populated — add a repo whose CI has a run parked
    at a deployment-protection gate under Settings → Portfolio. Saving wakes the
    loop, and the pass that first sees the gate is not the seeding pass, so it
    delivers: one banner reading `{repo} · needs approval`, body
    `{workflow} · {branch} is parked at an approval gate.` Watch two more passes
    go by (a minute at the default interval) and confirm **no second banner** —
    the alert is on the transition, not on the state.

    Under `cargo run` on macOS the banner is attributed to **Terminal**, not to
    Solador: `notify-rust` sets the bundle id to `com.apple.Terminal` when
    `tauri::is_dev()`, because an unbundled binary has no identity of its own to
    notify under. That is expected, not a defect — and it means a *bundled*
    build's notification permission prompt is still unexercised
    ([#15](https://github.com/cpmadrid/solador/issues/15) owns packaging). If
    macOS Focus is on, or notifications are denied for Terminal, delivery is a
    silent no-op with nothing on the terminal either; check
    System Settings → Notifications before concluding the code is wrong.

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

Step 8 passes when `usage:` and `azure_cost: first frontend request …` both
print and both panels carry a heading plus *either* their content or their
zero-credential state — the same argument a sixth and seventh time.

Step 9 passes when `openclaw: first frontend request …` prints and the panel
carries a heading plus *either* its runtime sections, its connection line, its
pairing banner, or the muted `add a gateway URL in Settings → OpenClaw` hint —
the same argument an eighth time. Every one of those strings is
`openclaw::view`'s, reachable only through `invoke("openclaw")`. A machine with
no gateway configured passes; a missing panel does not.

Step 10 passes when the **first** card in the grid is this machine, named after
this machine, with a green dot. It is the one card no configuration can produce
and no configuration can remove, so it is also the cheapest read on the whole
procedure: a grid that leads with `ubu-01` means either the local sampler never
started or step 1 was skipped.

Step 11 is the only part of this procedure whose pass condition is **outside the
app**: a browser window at the right URL, and a banner in Notification Center.
Everything above proves a payload reached the DOM; 11a proves the webview can
reach *out* through the one grant it has, and only to where that grant points.
11b proves the notifier can, without the webview being in the path at all.
Neither has an in-app symptom, which is exactly why they are steps rather than
tick-boxes: nothing on screen changes if either is broken.

Seeding a second host — run once more with a different address, against the same
`DEVCANOPY_STORE_DIR` — is the multi-card version of the same check: two cards,
side by side above ~1816pt of window (2 × 900 + 16) and stacked below it.

### Fail

| Symptom                                                                                       | Reading                                                                                                                                                              |
|-----------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| The window is a red `failed to load cockpit: …` line instead of a card.                        | The first `invoke` rejected — `app.js`'s initial `catch` replaces the whole body with the error. This is the expected shape of an ACL or registration break.           |
| The card's structure is there but every field is blank.                                       | `app.js` never ran at all — a CSP violation or a script error, so nothing ever reached the IPC boundary. Check the console before suspecting the ACL.                    |
| A card renders with plausible numbers that never change, and the host name is `ubu-01` rather than the one you passed. | You skipped step 1. That is the fixture, not the boundary. Delete `app/ui/sample*.json` and re-run.                                                                     |
| `No hosts configured. Add one in Settings.` and no card at all.                                | A malformed `DEVCANOPY_SEED_HOST` (empty name or address) — the boundary is fine, the configuration is not. Note this still *proves* the round-trip: that sentence is `cockpit_payload`'s and has no other path to the DOM. Fix the variable and re-run to get a named card. |
| The window opens and stays up, but the `cockpit: first frontend request …` line never prints.  | The boundary is broken and the window is hiding it. This is the definitive terminal-side failure: `invoke` never reached Rust. Verified as a real discriminator on 2026-07-31 by renaming the command in `app.js` — the app still launched and still held a window, and the line stayed absent. |
| The cards paint, but clicking **Settings** does nothing and `settings: first frontend request …` never prints. | The cockpit half of the boundary is fine and the settings half is not: an unregistered `settings_view`, or a script error in `settings.js` that stopped it before it wired the button. Check the webview console. |
| Settings opens with no tabs and no controls, or the button carries no label.                   | `settings_view` answered with something that isn't a settings payload — or `app.js` painted a cockpit payload with no `settingsLabel`. Regenerate the fixtures and check the payload shape, not the ACL. |
| The cards paint but there is no **Containers / VMs** panel at all, and `containers: first frontend request …` never prints. | The containers half of the boundary is broken: an unregistered `containers` command, or a script error in `containers.js`. The panel stays hidden until a payload arrives — deliberately, so a broken boundary cannot masquerade as an idle machine. Check the webview console. |
| The panel renders but every section says `no containers` on a machine that is definitely running some. | The boundary is fine; the *discovery* is not. The tools are resolved by absolute path (`/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin`) — a docker installed anywhere else is invisible. A failing tool would instead name itself in the footer (`⚠ couldn't read docker`). |
| The **Repos** or **GitHub Runners** panel is missing entirely, and its `first frontend request …` line never prints. | That half of the boundary is broken: an unregistered `repos`/`runners` command, or a script error in `github.js`. Both panels stay hidden until a payload arrives, so this cannot be mistaken for "no token configured" — that state renders a visible panel with one sentence in it. Check the webview console. |
| Both panels render, but every LOCAL and WT cell is `—` on a machine that definitely has the repos checked out. | The boundary is fine; the *scan* is not. It looks only under `~/Repos`, three levels deep, and joins by name with punctuation and case stripped — a checkout somewhere else is invisible, and a directory renamed away from its slug will not match. `—` is the honest answer to both, which is why it is not a zero. |
| The Runners panel shows `⚠ couldn't read runners — token needs org self-hosted runners (read)`. | Not a boundary failure — the round-trip worked and that string is `github::RUNNERS_ERROR_MESSAGE`. The PAT is missing the org self-hosted-runners read permission, which is a separate grant from the repo-scoped ones. The Repos panel beside it should still be populated. |
| The **Usage** or **Azure Cost** panel is missing entirely, and its `first frontend request …` line never prints. | That half of the boundary is broken: an unregistered `usage`/`azure_cost` command, or a script error in `usage.js`/`azure.js`. Both stay hidden until a payload arrives, so this cannot be mistaken for "nothing configured" — that state renders a visible panel with a sentence in it. Check the webview console. |
| The Usage panel shows Claude tokens but no Neon or Sentry section on a machine where those credentials *are* saved. | Not a boundary failure. A blank key reads as unconfigured by design, and the section is *absent* rather than empty. Check Settings → Usage shows **Stored** for the credential; if it does, the hourly read has not run yet — saving wakes it, so re-save to force a pass. |
| Neon or Sentry shows `—` for every figure with a message under it. | Also not a failure — the round-trip worked and the API answered. `Add your Neon org ID in Settings` means the id is missing; `no Neon consumption reported …` means the org measured nothing (empty org, wrong id, or a plan without consumption history). The em dash is the honest answer to all of them, which is why it is not a zero. |
| The **OpenClaw** panel is missing entirely, and `openclaw: first frontend request …` never prints. | That half of the boundary is broken: an unregistered `openclaw` command, or a script error in `openclaw.js`. The panel stays hidden until a payload arrives, so this cannot be mistaken for "no gateway configured" — that state renders a visible panel with one muted sentence in it. Check the webview console. |
| The OpenClaw panel sits on `connecting…` forever, or cycles connecting → disconnected. | Not a boundary failure — the round-trip worked and those words are `openclaw::view`'s. The session is retrying with exponential backoff, and the disconnect reason names the cause: `handshake timed out` (no gateway there), `gateway rejected: …` (its own words, often `controlUi.allowedOrigins`), or `invalid gateway URL` (not a `ws://`/`wss://` address). |
| The pairing banner keeps returning after the approve command was run. | Also not a failure. The command has to run **on the gateway host**, and the request id is single-use — a stale one from a previous banner will not clear it. Press **Retry now** and re-read the id from the fresh banner. If the device id in Settings changes between attempts, the credential store is refusing to persist the seed; the terminal says so (`openclaw: could not persist the device key: …`). |
| The local card is missing, or the grid leads with a remote host. | The local sampler never started, or its first sample has not landed (it renders `waiting for first sample…` for one tick). A card that never appears at all points at the poll task, not the ACL: the card is built in `cockpit`, which the terminal line in step 3 already proved runs. |
| The local card renders but every figure is `—`. | Sampling is failing, not the boundary. Expected on the very first tick; persisting past a few seconds means `sysinfo` is returning nothing on this platform. Note that `Pressure: —` and `VRAM: —` are permanent and correct on macOS — see [This machine leads](#this-machine-leads). |
| Clicking a repo row does nothing, and the console says `opener.open_url not allowed`. | The **permission** is missing: `opener:allow-open-url` is not in `capabilities/default.json`, or the plugin is not registered on the builder. Not a scope problem — the command was rejected before any URL was looked at. |
| Clicking a repo row does nothing, and the console names a `Forbidden URL`. | The permission is there and its **scope** rejected the URL. Either the glob was narrowed, or something other than `github::actions_url` composed the string — the two live one line apart in `capabilities/default.json` and `src/github/mod.rs`, and only they may disagree. |
| The rows render but none of them is clickable, and no console error appears. | Neither: `row.url` is absent from the payload, so github.js never wires a handler. A Rust-side regression, and `a_row_carries_the_swift_tap_target` should have caught it — check the fixtures are not stale first (step 1). |
| No needs-approval banner for a run that is definitely parked at a gate. | Four ordinary causes before suspecting the code: it was the **seeding** pass (the first pass after launch never alerts — see step 11b for how to force a real transition); the run was already parked on the previous pass, so this one is not a transition; `notify_on_approval_needed` is `false` in the store file; or macOS Focus / denied notifications for **Terminal** (the id an unbundled dev build notifies under) is swallowing it silently. |
| A needs-approval banner repeats every poll pass. | A real failure, and the one this feature exists to avoid: the baseline is not being retained across passes. `ApprovalWatch` lives on `App`, so a per-pass instance would produce exactly this. |

For the underlying error, open the webview console: right-click in the window →
**Inspect Element** (devtools are enabled in debug builds). An ACL rejection names
the command; the mock-harness form of it is `cockpit not allowed. Plugin not
found`.

### Recording a run

The last acceptance item on
[#123](https://github.com/cpmadrid/solador/issues/123) is a human one: launch
once per this procedure and record the result. That record is currently the only
evidence the boundary works.

| Date       | Change under test | Step 3 (terminal) | Step 4 (visual) |
|------------|-------------------|-------------------|-----------------|
| 2026-08-01 | **Live-gateway + credentialed session** ([#186](https://github.com/cpmadrid/solador/issues/186)) — human at the unlocked Mac, real credentials end to end: seeded agent token (ubu-01), fine-grained PAT, OpenClaw gateway `ws://127.0.0.1:18789` + bearer. | **Pass — and it found three real defects, each fixed + pinned by a test in the same session:** (1) the hand-built upgrade request sent none of the mandatory WebSocket headers (`ws.rs` — tungstenite passes prebuilt requests through verbatim; rejected with `sec-websocket-key` before this fix); (2) the gateway's connect gate requires protocol **v4** for UI-mode clients (`PROTOCOL_VERSION` was 3, ported faithfully from Swift code that has never run live — the Swift app shares this bug); (3) no `User-Agent` — GitHub 403s every request regardless of token permissions (reqwest sends none by default; URLSession always does, which is why Swift never hit it). | **Performed.** Host card live (volumes, top processes), Containers live (23 incl. the tart runner VMs), OpenClaw **connected end to end** — pairing status, persisted device identity, live agent rendered — Repos live with real counts (honest `—` on velovate's local columns, running/failed dots per Swift), Runners 12/12 with busy/idle. Keychain prompt storm fixed by re-signing debug builds with the stable team identity (now part of #190's scope). **Still unobserved:** step 11 (tap-to-open click + notification banner) and the Neon/Sentry/Azure sections (credentials not configured this session). |
| 2026-08-01 | Tap-to-open + needs-approval notifications, and **the first non-empty ACL** ([#187](https://github.com/cpmadrid/solador/issues/187)) | **Partial pass — every terminal line, neither new seam.** Fixtures absent, scratch `DEVCANOPY_STORE_DIR`, no seeded host, no credentials. All **seven** `first frontend request …` lines printed (`cockpit … (0 host(s), 968pt)`, `containers … (1 section(s))`, `repos … (0 repo row(s))`, `runners`, `usage`, `azure_cost … (headline: false)`, `openclaw … (trailing: "")`). That is the regression this change most risked: `permissions` went from `[]` to a real entry, and the app-defined commands still all carry. **What was NOT performed is step 11 — both halves.** With no PAT the Repos table is empty, so no row was clickable, no `open_url` has ever crossed the boundary, and no banner has been observed in Notification Center. Both features are therefore *implemented, unit-tested, and unverified end to end*, and **step 11a remains the only check that the granted scope is enforced rather than merely written**. Verified either side of the boundary instead: `actions_url_is_the_only_shape_the_granted_scope_admits` reads the real capability file and asserts the glob admits every URL `github::actions_url` produces and refuses eight it must not (About links, `http://`, `github.com.evil.example`, `file://`, `javascript:`) — with a negative control, narrowing the glob to `https://github.com/*` and confirming the test fails; four Playwright specs assert the click, the Enter key, the `role`/`aria-label`/`tabIndex`, and that the URL handed to `plugin:opener|open_url` is Rust's own string byte for byte, IPC stubbed as always; eight unit tests cover the notification transition, the seeding pass, the disabled-but-still-advancing baseline and re-entry. **Still untouched by any of it:** whether Tauri enforces the scope at runtime, whether `notify-rust` shows anything on this machine, and the macOS notification prompt (an unbundled dev build notifies as Terminal — #15 owns packaging). Needs a human at a Mac with a PAT. | **Not performed** — headless run, no screen read. |
| 2026-08-01 | OpenClaw panel + Settings tab ([#177](https://github.com/cpmadrid/solador/issues/177)) | **Not performed.** The three new commands (`openclaw`, `settings_save_openclaw`, `settings_openclaw_retry`) and their **step 9** are therefore *documented, not verified* — no `openclaw: first frontend request …` line has ever been observed, and no live gateway was reached. What was verified instead is everything below the boundary: all five payloads were dumped from the real binary and rendered in a browser under the app's own CSP (`tests/frontend/csp_server.py`), exercising `openclaw.js`, the Settings tab, the pairing banner and the dot-opacity path while stubbing the IPC transport exactly as the rest of the suite does. **Also unexercised against a real gateway:** the WebSocket handshake, the signed connect payload, the pairing round-trip and the keyring seed persistence — those are covered by `crates/openclaw`'s own tests over a scripted transport (#173) and by this crate's `MemoryCredentialStore` round-trip, not by a socket. The ACL is untouched (`permissions` still `[]`, all three commands app-defined), which is the only reason to expect this to be uneventful — not evidence that it is. | **Not performed** (see left). |
| 2026-08-01 | Usage + Azure Cost panels and the local host card ([#175](https://github.com/cpmadrid/solador/issues/175)) | **Not performed.** The two new commands (`usage`, `azure_cost`) and their **step 8**, plus the local card's **step 9**, are therefore *documented, not verified* — no `usage: first frontend request …` line has ever been observed, and neither has the local card on a screen. What was verified instead is everything below the boundary: every payload was dumped from the real binary and rendered in a browser under the app's own CSP (`tests/frontend/csp_server.py`), which exercises `usage.js`, `azure.js`, the panel-row layout and the CSSOM colour path but stubs the IPC transport exactly as the rest of the suite does. The ACL is untouched (`permissions` still `[]`, both commands app-defined), which is the only reason to expect this to be uneventful — not evidence that it is. | **Not performed** (see left). |
| 2026-08-01 | Repos + GitHub Runners panels ([#172](https://github.com/cpmadrid/solador/issues/172)) | **Not performed.** The two new commands (`repos`, `runners`) and their **step 7** are therefore *documented, not verified* — no `repos: first frontend request …` line has ever been observed. What was verified instead is everything below the boundary: the payloads were dumped from the real binary and rendered in a browser under the app's own CSP (`tests/frontend/csp_server.py`), which exercises `github.js`, the CSSOM colour path and the column-width math, but stubs the IPC transport exactly as the Playwright suite does. The ACL is untouched (`permissions` still `[]`, both commands app-defined), which is the only reason to expect this to be uneventful — not evidence that it is. | **Not performed** (see left). |
| 2026-08-01 | Settings surface + `App` state restructure ([#163](https://github.com/cpmadrid/solador/issues/163)) | **Pass.** Fixtures removed, scratch store, `DEVCANOPY_SEED_HOST="smoke-…\|100.100.100.100\|7878\|"` (no token). Terminal: `cockpit: first frontend request (1 host(s), 968pt)` — so the ACL, the handler registration and the transport still carry the call after `manage()` changed from `Cockpit` to `App` and the handler list grew from one command to fifteen. | **Not performed**, and neither was **step 5** — both need a click on a Mac someone else is working on. The settings half of the boundary is therefore *documented, not verified*: `settings: first frontend request …` has never been observed. Worth ten seconds from anyone who launches this next. |
| 2026-07-31 | `snapshot` → `cockpit`, N-card grid ([#157](https://github.com/cpmadrid/solador/issues/157)) | **Pass.** Fixtures removed, scratch store, `DEVCANOPY_SEED_HOST="smoke-233344\|100.100.100.100\|7878\|"` (no token). Terminal: `cockpit: first frontend request (1 host(s), 968pt)` — so the ACL, the handler registration and the transport all carried the call, and `width` arrived. App still up when the run ended. Negative control run immediately before (command renamed in `app.js`, rebuilt) printed nothing, so the signal discriminates. | **Not performed** — the Mac's screen was locked (`CGSSessionScreenIsLocked`), which makes `screencapture` return black frames, and no Accessibility grant was available to read the window's text. Worth a human glance next time someone has the screen in front of them. |

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
