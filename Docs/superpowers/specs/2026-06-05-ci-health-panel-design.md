# CI Health panel — surface running workflows and CI failures to triage

Date: 2026-06-05
Status: approved (design), pending implementation

## Problem

The cockpit's **Portfolio CI** panel shows one health dot per repo for four
hardcoded repos. It collapses each repo to a single "latest run", so it can't
answer the questions that actually drive action:

- Which workflows are **running right now**?
- Did the **last PR's** CI or **main's** CI **fail** (so I know what to go look at)?

It also omits the two repos the user pushes to most (`devcanopy`, `platform`).

## Goal

At a glance, see (1) what CI is currently running and (2) what has failed and
needs attention, across the repos the user actively works in — so the panel
answers "what do I need to go look at" without opening GitHub.

## Decisions

- **Repos:** curated set of six — the four portfolio repos plus `devcanopy` and
  `platform`.
- **Layout:** two grouped sections — **RUNNING** and **NEEDS ATTENTION** — with a
  "rest are green" reassurance line.
- **Rename** the panel from "Portfolio CI" to **"CI Health"** (panel kind
  `portfolioCI` → `ciHealth`).
- **Assume `main`** as the default branch for all six repos.
- **Reuse the existing fetch** — no new GitHub endpoints; categorize the runs we
  already pull. Refresh cadence 120s → **60s** so "running" feels live.

## Design

### Data flow (reuse)

`PortfolioCIService` already calls `GET /repos/{repo}/actions/runs?per_page=30`
per repo (returns completed + in-progress + queued, push + pull_request events).
Keep that. Change only what we derive from the response and how often.

- `PortfolioRepos.slugs` gains `cpmadrid/solador` and `Sassy-Dog/platform`.
- Poll interval 120s → 60s (6 repos × 1 call/min ≈ 360/hr, far under the 5000/hr
  authenticated limit).

### Categorization (pure mapping)

Replace the "latest run only" collapse in `PortfolioCIMapping` with a richer,
pure, unit-tested categorizer producing a per-repo result from the 30 runs:

```
struct RepoCIHealth {
    let repo: String            // "owner/name"
    let main: RunRef?           // latest run: event == push, headBranch == "main"
    let lastPR: RunRef?         // latest run: event == pull_request
    let running: [RunRef]       // every run with an in-progress status
}
struct RunRef {                 // the view-facing slice of a run
    let title: String           // workflow name
    let context: String         // "main" or the PR head branch
    let conclusion: RunConclusion?
    let status: RunStatus
    let startedAt: Date?        // runStartedAt ?? createdAt (for elapsed)
    let htmlURL: String
}
```

- **main** = newest run with `event == "push"` and `headBranch == "main"`.
- **lastPR** = newest run with `event == "pull_request"`.
- **running** = all runs whose status ∈ {queued, in_progress, requested, waiting,
  pending}, newest first.
- **failure** = conclusion ∈ {failure, timedOut, cancelled, startupFailure}
  (reuse the existing `RunConclusion` helper).

"Newest" is by `createdAt` descending (matches the existing mapping).

### Panel (`CIHealthPanel`)

Renders three parts inside the standard `CockpitPanelContainer` (title "CI
HEALTH", trailing summary e.g. `2 running · 1 failed`):

1. **RUNNING (n)** — one row per running run across all repos, newest first:
   `repo · workflow · context · elapsed` (elapsed = now − startedAt). Row is
   clickable → opens `htmlURL`.
2. **NEEDS ATTENTION (n)** — one row per failure: for each repo, if `main` failed
   emit a `main` row, if `lastPR` failed emit a `PR` row. Shows
   `repo · which · "failed · <relative time>"`. Clickable → `htmlURL`.
3. **Green line** — `✓ N repos green` (repos with main+PR not-failed and nothing
   running). If everything is clean and nothing running: `✓ All 6 repos green`.

States:
- No token → muted "connect a GitHub token in Settings".
- Fetch error / never fetched → muted "unreachable" / "waiting…".
- Sections with zero items are omitted (e.g. no RUNNING header when nothing runs).

### Edge cases

- **Default branch:** assumed `main` for all six (true today). A repo whose
  default branch isn't `main` would simply show no `main` row — acceptable and
  documented.
- **PR number:** the runs API doesn't reliably carry it without extra decoding;
  show the PR **head branch** as context. (`#number` is a later enhancement if the
  run's `pull_requests` array is populated.)
- **Stale PR failure:** `lastPR` reflects the newest pull_request run; a failure
  shows until a newer PR run supersedes it. Acceptable.
- **Multiple running runs per repo:** all listed (newest first).

## Files

- `DevCanopy/Services/GitHub/PortfolioRepos.swift` — add the two repos.
- `DevCanopy/Services/GitHub/PortfolioCIMapping.swift` — `RepoCIHealth`/`RunRef`
  + pure categorizer (replaces the latest-run collapse).
- `DevCanopy/Services/GitHub/PortfolioCIService.swift` — return `[RepoCIHealth]`;
  60s interval.
- `DevCanopy/Views/Cockpit/CockpitPanel.swift` — rename kind `portfolioCI` →
  `ciHealth`, title "CI Health".
- `DevCanopy/Views/Cockpit/CockpitView.swift` — switch case `ciHealth`.
- `DevCanopy/Views/Cockpit/Panels/PortfolioCIPanel.swift` → rename to
  `CIHealthPanel.swift` — the two-section view.
- `DevCanopyTests/PortfolioCIMappingTests.swift` — extend for categorization.

## Testing

Pure mapping unit tests (no I/O): from a representative mixed run list assert
- main picks the newest push@main run (not an older success, not a PR run),
- lastPR picks the newest pull_request run,
- running collects exactly the in-progress runs,
- failure detection across the failing conclusions,
- a repo with green main + green PR + nothing running counts as "green".

`CockpitLayoutTests` continues to pass (panel renamed, still placed once).

## Out of scope

- Org-wide repo discovery (kept to the curated six).
- Per-job / log drill-in; PR numbers; non-`main` default branches.
- Changes to the CI Runners panel or host panels.
