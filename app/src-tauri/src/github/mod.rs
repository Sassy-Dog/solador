//! The **Repos** and **GitHub Runners** panels: one fixed row per tracked repo
//! with its glanceable counts, and the org's self-hosted runners with the
//! roster memory that makes an absent one visible.
//!
//! Port of `DevCanopy/Views/Cockpit/Panels/GHWorkflowsPanel.swift` and
//! `GHRunnersPanel.swift`. The data layer beneath them is `crates/github`; this
//! module is the view side, and it holds to the same rule as
//! [`crate::containers`] and `crates/viewmodel`: **every string, colour, width
//! and count the frontend paints is made here.** A threshold or a status word
//! typed into JavaScript is one that can drift from the Swift panel with no
//! test noticing.
//!
//! Two rules run through the whole module, both inherited from `crates/github`:
//!
//! **Unknown is not zero.** Every count cell is an `Option`. `None` renders the
//! muted em dash — a failed fetch, a PAT missing a scope, a repo that is not
//! checked out here. `Some(0)` renders a dimmed `0`, which is a positive claim
//! that there are none. Collapsing the two is the exact bug the em dash exists
//! to prevent (the `/issues` cursor-pagination undercount).
//!
//! **Clocks advance only on success.** The runner roster is folded forward only
//! by a successful fetch, and the panel keeps its last-good rows through a
//! failing one, with the footer carrying the failure. An outage must not age a
//! healthy runner into a red "missing 40m": the runner never went anywhere, our
//! view of it did.

pub mod git;
pub mod notify;

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use github::roster::{self, RunnerRosterEntry};
use github::runners::{GhRunner, RunnerState, RunnerSummary};
use github::workflows::RunRef;
use github::{GhRunnerAbsence, GhRunnerDisplayRow, PresenceState, RepoWorkflowHealth};
use serde_json::{json, Value};
use store::RunnerRosterRecord;
use viewmodel::cockpit::PanelKind;
use viewmodel::color;

use git::LocalRepoCounts;

/// Re-exported so `main.rs` — where the module name `github` shadows the crate
/// name — reaches the client through this module rather than through a
/// `::github::` escape hatch that reads like a typo.
pub use github::GitHubClient;

/// The org whose self-hosted runners the Runners panel reports on — Swift's
/// `PortfolioRepos.org`, which `crates/store` already spells once.
pub const ORG: &str = store::repos::ORG;

/// Absence grace before a de-registered runner escalates from amber
/// "recycling" to red "missing" — `crates/github`'s shipped 5 minutes, which
/// is a little longer than the mac slots' 1–4 minute recycle window.
pub const RUNNER_GRACE_SECS: i64 = github::presence::DEFAULT_GRACE_SECS;

/// The wall clock, as the GitHub layer's `DateTime<Utc>`.
///
/// Built from [`crate::panel::now_unix`] rather than `Utc::now()` so the whole
/// shell reads one clock — and so `chrono` here needs no `clock` feature, which
/// would drag a timezone database into a build that only ever wants UTC.
#[must_use]
pub fn now_utc() -> DateTime<Utc> {
    let secs = i64::try_from(crate::panel::now_unix()).unwrap_or(0);
    DateTime::from_timestamp(secs, 0)
        .unwrap_or_else(|| DateTime::from_timestamp(0, 0).expect("the epoch is representable"))
}

/// Both panels' zero-credential state. One string, because it names one action
/// and the user should not have to notice that two panels worded it differently.
pub const UNAUTHENTICATED_MESSAGE: &str = "connect a GitHub token in Settings";

/// Repos, authenticated, before the first fetch has landed.
pub const REPOS_LOADING_MESSAGE: &str = "loading…";

/// Runners, same moment. Swift words this one differently and the difference is
/// kept: the Repos panel says what it is doing, the Runners panel says what it
/// is fetching.
pub const RUNNERS_LOADING_MESSAGE: &str = "loading runners…";

/// What a failed org-runners fetch says. Names the *likely* cause rather than
/// the transport error, because a PAT missing the org self-hosted-runners scope
/// is overwhelmingly what this is, and "403" sends the operator nowhere useful.
pub const RUNNERS_ERROR_MESSAGE: &str =
    "couldn't read runners — token needs org self-hosted runners (read)";

/// `PanelStatusFooter(..., staleAfter: 150)` on the Runners panel — 2.5× the
/// default 60s refresh interval, so one missed poll is not yet a warning.
pub const RUNNERS_STALE_AFTER_SECS: u64 = 150;

// Fixed column widths, verbatim from `GHWorkflowsPanel`. The cockpit's
// monospace font is what makes them align; they sum to 312pt, which is the
// figure `PanelKind::GhWorkflows.min_width` is built on — widen one and that
// breakpoint has to move with it.
const ISSUES_W: f64 = 52.0;
const PRS_W: f64 = 34.0;
const LOCAL_W: f64 = 44.0;
const REMOTE_W: f64 = 52.0;
const WT_W: f64 = 34.0;
const JOBS_W: f64 = 40.0;
const LONGEST_W: f64 = 56.0;

/// The header row. `REPO` has no width — it takes whatever the fixed columns
/// leave, and is the only left-aligned one.
const COLUMNS: [(&str, Option<f64>); 8] = [
    ("REPO", None),
    ("ISSUES", Some(ISSUES_W)),
    ("PRS", Some(PRS_W)),
    ("REMOTE", Some(REMOTE_W)),
    ("LOCAL", Some(LOCAL_W)),
    ("WT", Some(WT_W)),
    ("JOBS", Some(JOBS_W)),
    ("LONGEST", Some(LONGEST_W)),
];

/// Everything both panels render from, and the memory that survives one bad
/// poll.
///
/// One struct for two panels because they share a credential and a cadence: the
/// token that authenticates one authenticates the other, and a single poll pass
/// fills both. Splitting them would mean two copies of "are we authenticated",
/// free to disagree.
#[derive(Debug, Default)]
pub struct GitHubState {
    /// Whether a non-empty token was loaded on the last pass. Not "whether
    /// GitHub accepted it" — a rejected token is a per-fetch failure, and the
    /// Repos panel reports that as an unreadable repo rather than as a missing
    /// credential.
    authenticated: bool,
    /// Per-repo health from the last completed pass, one entry per **enabled**
    /// tracked repo (unreachable ones included). `None` until the first pass
    /// finishes, which is what "loading…" means.
    health: Option<Vec<RepoWorkflowHealth>>,
    /// Local branch/worktree counts, keyed by [`git::normalize`]d repo name.
    local: BTreeMap<String, LocalRepoCounts>,
    /// Runners, from the last **successful** fetch. Retained through a failing
    /// one so the panel keeps showing real (if not current) rows.
    summary: Option<RunnerSummary>,
    runners: Vec<GhRunner>,
    absent: Vec<GhRunnerAbsence>,
    runners_error: Option<String>,
    /// When the runners last fetched successfully — the footer's clock. Only
    /// advanced by a success, so a failing GitHub ages the footer instead of
    /// freezing it at a reassuring "just now".
    runners_last_updated: Option<u64>,
}

impl GitHubState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// No token configured. Both panels drop back to the connect-a-token line.
    ///
    /// Everything fetched is cleared, matching `GHRunnersService.refresh()`:
    /// with no credential there is nothing to keep current, and a stale runner
    /// list left on screen would claim knowledge the app no longer has. The
    /// *roster* is untouched — it lives in the store, so expectations resume
    /// intact when a token comes back rather than re-learning from scratch.
    pub fn apply_unauthenticated(&mut self) {
        self.authenticated = false;
        self.health = None;
        self.summary = None;
        self.runners.clear();
        self.absent.clear();
        self.runners_error = None;
    }

    /// Records one completed Repos pass. Wholesale, never merged: the row set
    /// is the enabled-repo list, so a repo removed in Settings must lose its
    /// row on the next pass rather than linger as a stale one.
    pub fn apply_repos(&mut self, health: Vec<RepoWorkflowHealth>) {
        self.authenticated = true;
        self.health = Some(health);
    }

    /// Records one local git scan.
    pub fn apply_local(&mut self, local: BTreeMap<String, LocalRepoCounts>) {
        self.local = local;
    }

    /// Records one **successful** org-runners fetch.
    pub fn apply_runners(&mut self, update: &roster::RosterUpdate, now: u64) {
        self.authenticated = true;
        self.summary = Some(update.summary);
        self.runners.clone_from(&update.runners);
        self.absent.clone_from(&update.absent);
        self.runners_error = None;
        self.runners_last_updated = Some(now);
    }

    /// Records a failed org-runners fetch: the message, and nothing else.
    ///
    /// Deliberately touches neither the rows nor `runners_last_updated`. Those
    /// are the record of the last thing we actually heard; clearing them would
    /// blank a panel that still holds real data, and advancing the clock would
    /// let a permanently failing fetch look freshly updated forever.
    pub fn apply_runners_error(&mut self, message: impl Into<String>) {
        self.authenticated = true;
        self.runners_error = Some(message.into());
    }
}

// MARK: - Repos

/// Status-dot precedence, most urgent first. The dot collapses what used to be
/// separate RUNNING / NEEDS APPROVAL / STUCK / NEEDS ATTENTION sections into
/// one fixed-size signal, which is what keeps the card from resizing as CI
/// churns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepoStatus {
    /// Runs couldn't be fetched (auth/network) — muted, because we know
    /// nothing, which is not the same as knowing it is broken.
    Unreachable,
    /// Main or last-PR failed, or a queued run has gone stale.
    Failed,
    /// A run is parked at a deployment-protection gate: a human must act.
    NeedsApproval,
    /// Actively executing, nothing wrong.
    Running,
    Healthy,
}

fn status_of(health: &RepoWorkflowHealth) -> RepoStatus {
    if !health.reachable {
        return RepoStatus::Unreachable;
    }
    if health.main.as_ref().is_some_and(RunRef::is_failed)
        || health.last_pr.as_ref().is_some_and(RunRef::is_failed)
        || !health.stuck.is_empty()
    {
        return RepoStatus::Failed;
    }
    if !health.needs_approval.is_empty() {
        return RepoStatus::NeedsApproval;
    }
    if !health.running.is_empty() {
        return RepoStatus::Running;
    }
    RepoStatus::Healthy
}

fn status_color(status: RepoStatus) -> u32 {
    match status {
        RepoStatus::Unreachable => color::MUTED,
        RepoStatus::Failed => color::RED,
        RepoStatus::NeedsApproval | RepoStatus::Running => color::AMBER,
        RepoStatus::Healthy => color::GREEN,
    }
}

/// Only the approval gate blinks. Needs-approval and running are the same
/// amber, and the pulse is what separates "a human must act" from "a machine is
/// working" without spending a second colour on it.
fn status_blinks(status: RepoStatus) -> bool {
    status == RepoStatus::NeedsApproval
}

/// A repo row's tap target — `GHWorkflowsPanel.openActions(_:)`'s
/// `https://github.com/\(slug)/actions`, character for character.
///
/// Built **here**, from the slug the poll pass fetched, and never assembled in
/// the webview. That is not style: this string is the only thing the granted
/// `opener:allow-open-url` scope will accept, and a frontend free to compose it
/// would be a frontend free to compose everything else that scope's glob also
/// matches. See `actions_url_is_the_only_shape_the_granted_scope_admits`.
#[must_use]
pub fn actions_url(slug: &str) -> String {
    format!("https://github.com/{slug}/actions")
}

/// What a screen reader announces for the row, and the only *label* the click
/// target carries.
///
/// The Swift panel has none — an `onTapGesture` on a `VStack` is invisible to
/// VoiceOver — so this is not parity, it is the web platform's own floor: a
/// `role="link"` whose accessible name would otherwise be the row's seven
/// numbers read aloud in a row.
fn open_label(slug: &str) -> String {
    format!("Open {slug} on GitHub Actions")
}

/// The whole Repos payload.
///
/// `now` is render time, and it is only used for the LONGEST column: a running
/// job's elapsed time has to advance between fetches, or the panel would claim
/// the longest run froze at whatever it was when the poll landed.
#[must_use]
pub fn repos_view(state: &GitHubState, now: DateTime<Utc>) -> Value {
    let message = if state.authenticated {
        if state.health.is_none() {
            Some(REPOS_LOADING_MESSAGE)
        } else {
            None
        }
    } else {
        Some(UNAUTHENTICATED_MESSAGE)
    };

    // Nothing is rendered beside a message: the Swift panel branches before
    // building the table, and a half-populated grid under "loading…" would be
    // a state it never has.
    let health: &[RepoWorkflowHealth] = if message.is_none() {
        state.health.as_deref().unwrap_or_default()
    } else {
        &[]
    };

    let mut sorted: Vec<&RepoWorkflowHealth> = health.iter().collect();
    sorted.sort_by_cached_key(|h| h.short_name().to_lowercase());

    json!({
        "id": PanelKind::GhWorkflows.id(),
        "title": PanelKind::GhWorkflows.title(),
        "trailing": message.map_or_else(|| json!(trailing_label(health)), |_| Value::Null),
        "message": message.map_or(Value::Null, |text| json!({ "text": text })),
        "columns": columns(),
        "rows": sorted
            .iter()
            .map(|h| repo_row(h, &state.local, now))
            .collect::<Vec<_>>(),
        "health": if message.is_none() { health_line(health) } else { Value::Null },
    })
}

fn columns() -> Vec<Value> {
    COLUMNS
        .iter()
        .map(|(label, width)| json!({ "label": label, "width": width }))
        .collect()
}

/// `"2 needs approval · 1 stuck · 4 running"`, or `"all green"`.
///
/// Ordered by urgency, not by count: what needs a human comes first, and the
/// merely-informational running total sits between the problems that block and
/// the ones that already happened.
fn trailing_label(health: &[RepoWorkflowHealth]) -> String {
    let approval: usize = health.iter().map(|h| h.needs_approval.len()).sum();
    let stuck: usize = health.iter().map(|h| h.stuck.len()).sum();
    let running: usize = health.iter().map(|h| h.running.len()).sum();
    let attention = attention_count(health);
    let unreadable = unreadable_count(health);

    let mut parts = Vec::new();
    if approval > 0 {
        parts.push(format!("{approval} needs approval"));
    }
    if stuck > 0 {
        parts.push(format!("{stuck} stuck"));
    }
    if running > 0 {
        parts.push(format!("{running} running"));
    }
    if attention > 0 {
        parts.push(format!("{attention} failed"));
    }
    if unreadable > 0 {
        parts.push(format!("{unreadable} unreadable"));
    }
    if parts.is_empty() {
        "all green".to_owned()
    } else {
        parts.join(" · ")
    }
}

/// Failed *slots*, not failed repos: a repo whose main and last-PR runs both
/// failed contributes 2, because that is two things to go and fix.
fn attention_count(health: &[RepoWorkflowHealth]) -> usize {
    health
        .iter()
        .map(|h| {
            usize::from(h.main.as_ref().is_some_and(RunRef::is_failed))
                + usize::from(h.last_pr.as_ref().is_some_and(RunRef::is_failed))
        })
        .sum()
}

fn unreadable_count(health: &[RepoWorkflowHealth]) -> usize {
    health.iter().filter(|h| !h.reachable).count()
}

/// The reassurance line under the table.
///
/// "Healthy" excludes only failed and unreachable repos — a repo that is merely
/// *running* still counts, so the fraction never implies a problem just because
/// a build is in flight. When nothing is wrong it says "all N" rather than
/// "N/N": the fraction is the shape of a problem, and there isn't one.
fn health_line(health: &[RepoWorkflowHealth]) -> Value {
    let total = health.len();
    let healthy = health.iter().filter(|h| h.is_healthy()).count();
    let text = if attention_count(health) == 0 && unreadable_count(health) == 0 {
        format!("✓ all {total} healthy")
    } else {
        format!("✓ {healthy}/{total} healthy")
    };
    json!({ "text": text, "color": color::hex(color::GREEN) })
}

/// One repo's row: the dot, the short name, and the seven fixed cells.
///
/// The local counts are joined by [`git::normalize`]d name, the same key the
/// Swift panel joins on — a repo not checked out here simply has no entry, and
/// [`LocalRepoCounts::default`] is two `None`s, which is exactly "—".
fn repo_row(
    health: &RepoWorkflowHealth,
    local: &BTreeMap<String, LocalRepoCounts>,
    now: DateTime<Utc>,
) -> Value {
    let status = status_of(health);
    let on_disk = local
        .get(&git::normalize(health.short_name()))
        .copied()
        .unwrap_or_default();
    // The *oldest* start among running runs is the longest-running one.
    let longest = health
        .running
        .iter()
        .filter_map(|run| run.started_at)
        .min()
        .map(|started| elapsed((now - started).num_seconds().max(0)));

    json!({
        "repo": health.repo,
        "name": health.short_name(),
        "dotColor": color::hex(status_color(status)),
        "blinking": status_blinks(status),
        // The row's click target. Present on every row, including an
        // unreachable one: not being able to read a repo's runs is precisely
        // when you want to go and look at them.
        "url": actions_url(&health.repo),
        "linkLabel": open_label(&health.repo),
        "cells": [
            count_cell(health.open_issues, ISSUES_W, color::INK),
            count_cell(health.open_prs, PRS_W, color::INK),
            count_cell(health.remote_branches, REMOTE_W, color::INK),
            count_cell(on_disk.local_branches, LOCAL_W, color::INK),
            count_cell(on_disk.worktrees, WT_W, color::INK),
            // JOBS is amber when non-zero, pairing it with the amber LONGEST
            // cell beside it — the two most ephemeral columns read as a unit.
            count_cell(
                u32::try_from(health.running.len()).ok(),
                JOBS_W,
                color::AMBER,
            ),
            longest_cell(longest.as_deref()),
        ],
    })
}

/// A right-aligned count.
///
/// Three renderings, and the difference between the first two is the whole
/// point: `None` is "we could not find out" (muted em dash), `Some(0)` is "there
/// are none" (dimmed, so a non-zero pops), `Some(n)` is the number in
/// `non_zero_color`.
fn count_cell(value: Option<u32>, width: f64, non_zero_color: u32) -> Value {
    let (text, tint) = match value {
        None => ("—".to_owned(), color::MUTED),
        Some(0) => ("0".to_owned(), color::MUTED),
        Some(n) => (n.to_string(), non_zero_color),
    };
    json!({ "text": text, "color": color::hex(tint), "width": width })
}

/// The LONGEST cell: an amber elapsed time, or a muted `·` when nothing is
/// running. A middle dot rather than an em dash, because "nothing is running"
/// is a known answer — the em dash is reserved for counts we could not read.
fn longest_cell(elapsed: Option<&str>) -> Value {
    let (text, tint) = match elapsed {
        Some(text) => (text, color::AMBER),
        None => ("·", color::MUTED),
    };
    json!({ "text": text, "color": color::hex(tint), "width": LONGEST_W })
}

/// `"45s"` / `"12m"` / `"3h07m"` — `GHWorkflowsPanel.elapsed`.
///
/// Deliberately **not** [`viewmodel::format::duration`], which drops to a
/// single unit above an hour (`"3h"`). A CI job that has been running for three
/// hours and one that has been running for three hours fifty is a very
/// different situation, and this is the column you watch to tell them apart.
fn elapsed(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{:02}m", secs / 3_600, (secs % 3_600) / 60)
    }
}

// MARK: - Runners

/// The whole GitHub Runners payload.
///
/// `now` is wall-clock unix seconds, used only by the footer's staleness. Every
/// *absence* clock in `state.absent` was computed at the last successful fetch
/// and is never recomputed here — that is what freezes them while GitHub is
/// unreachable.
#[must_use]
pub fn runners_view(state: &GitHubState, now: u64) -> Value {
    if !state.authenticated {
        return json!({
            "id": PanelKind::GhRunners.id(),
            "title": PanelKind::GhRunners.title(),
            "trailing": Value::Null,
            "message": { "text": UNAUTHENTICATED_MESSAGE },
            "stats": [],
            "chips": [],
            "rows": [],
            // No footer without credentials: there is nothing to be stale.
            "footer": Value::Null,
        });
    }

    // "loading runners…" only while nothing has been heard AND nothing has
    // failed. Once there is an error the footer carries it, and a "loading"
    // line beside a failure would be a second, contradictory story.
    let message = if state.summary.is_none() && state.runners_error.is_none() {
        Some(RUNNERS_LOADING_MESSAGE)
    } else {
        None
    };

    json!({
        "id": PanelKind::GhRunners.id(),
        "title": PanelKind::GhRunners.title(),
        "trailing": runners_trailing(state).map_or(Value::Null, Value::String),
        "message": message.map_or(Value::Null, |text| json!({ "text": text })),
        "stats": state.summary.map(summary_stats).unwrap_or_default(),
        "chips": state.summary.map(os_chips).unwrap_or_default(),
        // Registered and remembered-absent rows, merged into one display order
        // so an absent runner holds the exact slot it occupied while
        // registered instead of jumping to the bottom of the list.
        "rows": roster::display_rows(&state.runners, &state.absent)
            .iter()
            .map(runner_row)
            .collect::<Vec<_>>(),
        "footer": crate::panel::status_footer(
            state.runners_last_updated,
            state.runners_error.as_deref(),
            now,
            RUNNERS_STALE_AFTER_SECS,
        ),
    })
}

/// `"3/4"`, or `"3/4 · 1 missing"` once something remembered is absent beyond
/// grace. Recycling absences deliberately do not appear: ephemeral runners
/// de-register between jobs constantly, and a count that ticks up and down with
/// normal churn is a count nobody reads.
fn runners_trailing(state: &GitHubState) -> Option<String> {
    let summary = state.summary?;
    let missing = state
        .absent
        .iter()
        .filter(|absence| matches!(absence.state, PresenceState::Missing { .. }))
        .count();
    Some(if missing > 0 {
        format!("{}/{} · {missing} missing", summary.online, summary.total)
    } else {
        format!("{}/{}", summary.online, summary.total)
    })
}

/// ONLINE / BUSY / IDLE. BUSY is the only one that changes colour: zero busy
/// runners is a resting org, not a warning, so it dims rather than glowing
/// amber at all times.
fn summary_stats(summary: RunnerSummary) -> Vec<Value> {
    let stat = |label: &str, value: String, tint: u32| json!({ "label": label, "value": value, "color": color::hex(tint) });
    vec![
        stat(
            "ONLINE",
            format!("{}/{}", summary.online, summary.total),
            color::GREEN,
        ),
        stat(
            "BUSY",
            summary.busy.to_string(),
            if summary.busy > 0 {
                color::AMBER
            } else {
                color::MUTED
            },
        ),
        stat("IDLE", summary.idle.to_string(), color::GREEN),
    ]
}

/// `"macOS 2/2"` / `"Linux 1/2"` — and only for a platform the org actually
/// has. A `Linux 0/0` chip is a row of furniture describing nothing.
fn os_chips(summary: RunnerSummary) -> Vec<Value> {
    let mut chips = Vec::new();
    if summary.macos_total > 0 {
        chips.push(json!(format!(
            "macOS {}/{}",
            summary.macos_online, summary.macos_total
        )));
    }
    if summary.linux_total > 0 {
        chips.push(json!(format!(
            "Linux {}/{}",
            summary.linux_online, summary.linux_total
        )));
    }
    chips
}

/// One runner row — registered, or remembered and currently absent.
fn runner_row(row: &GhRunnerDisplayRow) -> Value {
    let (kind, status, tint) = match row {
        GhRunnerDisplayRow::Registered(runner) => (
            "registered",
            runner.state.label().to_owned(),
            runner_color(runner.state),
        ),
        GhRunnerDisplayRow::Absent(absence) => (
            "absent",
            // `Present` has no label and cannot occur here (an absence is
            // absent by construction), but the em dash is what renders if it
            // ever does — never a fabricated state word.
            github::presence::label(absence.state).unwrap_or_else(|| "—".to_owned()),
            presence_color(absence.state),
        ),
    };
    json!({
        "kind": kind,
        "name": row.name(),
        "os": row.os().label().to_uppercase(),
        "dotColor": color::hex(tint),
        "status": status,
        "statusColor": color::hex(tint),
    })
}

fn runner_color(state: RunnerState) -> u32 {
    match state {
        RunnerState::Idle => color::GREEN,
        RunnerState::Busy => color::AMBER,
        RunnerState::Offline => color::MUTED,
    }
}

/// Amber while recycling (normal ephemeral churn), red once past grace.
fn presence_color(state: PresenceState) -> u32 {
    match state {
        PresenceState::Present => color::GREEN,
        PresenceState::Recycling { .. } => color::AMBER,
        PresenceState::Missing { .. } => color::RED,
    }
}

// MARK: - Roster persistence bridge

/// Stored records to the roster `crates/github` works in.
///
/// An entry whose `last_seen` cannot be represented as a timestamp is dropped
/// rather than clamped: a date we cannot read is a clock we cannot age, and a
/// forgotten runner is re-learned on the very next fetch while a mis-dated one
/// would sit in the panel claiming a nonsense absence.
#[must_use]
pub fn roster_from_records(records: &[RunnerRosterRecord]) -> Vec<RunnerRosterEntry> {
    records
        .iter()
        .filter_map(|record| {
            let last_seen = i64::try_from(record.last_seen)
                .ok()
                .and_then(|secs| DateTime::from_timestamp(secs, 0))?;
            Some(RunnerRosterEntry {
                name: record.name.clone(),
                os: github::RunnerOs::from_raw(&record.os),
                last_seen,
            })
        })
        .collect()
}

/// The roster back to stored records.
#[must_use]
pub fn roster_to_records(entries: &[RunnerRosterEntry]) -> Vec<RunnerRosterRecord> {
    entries
        .iter()
        .map(|entry| RunnerRosterRecord {
            name: entry.name.clone(),
            os: entry.os.as_raw().to_owned(),
            // Pre-epoch is not a time a runner was seen; 0 is the honest floor.
            last_seen: u64::try_from(entry.last_seen.timestamp()).unwrap_or(0),
        })
        .collect()
}

// MARK: - Fixtures

/// A populated state for the offline fixtures the Playwright suite renders
/// against (`--dump-repos` / `--dump-runners`) and for the tests below.
///
/// Hand-built rather than fetched, and at a **fixed** `now`, so it is
/// byte-stable across regenerations and covers every state a live org will not
/// reliably produce on demand: a repo parked at an approval gate, one whose
/// runs could not be read, a repo missing from disk, a genuine zero beside an
/// unknown, a busy runner, an offline one, and remembered runners in both
/// absence states.
#[must_use]
pub fn fixture_state(now: DateTime<Utc>) -> GitHubState {
    use github::runners::RunnerOs;
    use github::workflows::WorkflowRun;

    let run = |id: i64, name: &str, status: &str, conclusion: Option<&str>, minutes_ago: i64| {
        WorkflowRun {
            id,
            name: name.to_owned(),
            event: "push".to_owned(),
            status: status.to_owned(),
            html_url: format!("https://github.com/Sassy-Dog/x/actions/runs/{id}"),
            created_at: (now - chrono::TimeDelta::minutes(minutes_ago)).to_rfc3339(),
            head_branch: Some("main".to_owned()),
            conclusion: conclusion.map(ToOwned::to_owned),
            run_started_at: None,
            display_title: Some("a commit".to_owned()),
        }
    };
    let health = |slug: &str, runs: &[WorkflowRun], counts: github::workflows::RepoCounts| {
        github::workflows::health(slug, runs, None, counts, now)
    };
    let counts = |branches, issues_incl_prs, prs| github::workflows::RepoCounts {
        remote_branches: branches,
        open_issues_including_prs: issues_incl_prs,
        open_pull_requests: prs,
    };

    let mut state = GitHubState::new();
    state.apply_repos(vec![
        // Green, and a genuine zero on every count.
        health(
            "Sassy-Dog/devcanopy",
            &[run(1, "CI", "completed", Some("success"), 30)],
            counts(Some(12), Some(4), Some(0)),
        ),
        // A build in flight: amber dot, amber JOBS, an elapsed LONGEST.
        health(
            "Sassy-Dog/qr-ninja",
            &[run(2, "CI", "in_progress", None, 95)],
            counts(Some(3), Some(9), Some(2)),
        ),
        // Parked at an approval gate: the blinking dot.
        health(
            "Sassy-Dog/tailoredtip",
            &[run(3, "Release", "waiting", None, 6)],
            counts(Some(2), Some(1), Some(1)),
        ),
        // Red, and its side counts came back while its runs failed.
        health(
            "Sassy-Dog/velovate",
            &[run(4, "CI", "completed", Some("failure"), 12)],
            counts(Some(41), Some(23), Some(5)),
        ),
        // The PAT could read the runs but not the Issues/PRs scopes: every
        // side count is an em dash while the repo stays green.
        health(
            "Sassy-Dog/what2wear",
            &[run(5, "CI", "completed", Some("success"), 240)],
            counts(None, None, None),
        ),
        // The runs themselves could not be fetched: muted dot, all em dashes.
        RepoWorkflowHealth::unreachable("Sassy-Dog/platform"),
    ]);
    // Four of the six repos are checked out here; `platform` and `what2wear`
    // are not, so their LOCAL/WT cells are em dashes rather than zeroes.
    state.apply_local(BTreeMap::from([
        (
            "devcanopy".to_owned(),
            LocalRepoCounts {
                local_branches: Some(7),
                worktrees: Some(3),
            },
        ),
        (
            "qrninja".to_owned(),
            LocalRepoCounts {
                local_branches: Some(2),
                worktrees: Some(1),
            },
        ),
        (
            "tailoredtip".to_owned(),
            LocalRepoCounts {
                local_branches: Some(1),
                worktrees: Some(1),
            },
        ),
        (
            "velovate".to_owned(),
            LocalRepoCounts {
                local_branches: Some(0),
                worktrees: Some(1),
            },
        ),
    ]));

    let runner = |id: i64, name: &str, os: RunnerOs, state: RunnerState| GhRunner {
        id,
        name: name.to_owned(),
        os,
        state,
    };
    let registered = [
        runner(1, "mac-s1", RunnerOs::MacOs, RunnerState::Busy),
        runner(2, "mac-s2", RunnerOs::MacOs, RunnerState::Idle),
        runner(3, "ubu-3xdv", RunnerOs::Linux, RunnerState::Idle),
        runner(4, "ubu-spare", RunnerOs::Linux, RunnerState::Offline),
    ];
    // Two remembered names that are not registered right now: one inside the
    // 300s grace (amber "recycling"), one past it (red "missing").
    let roster = [
        RunnerRosterEntry {
            name: "mac-s3".to_owned(),
            os: RunnerOs::MacOs,
            last_seen: now - chrono::TimeDelta::seconds(40),
        },
        RunnerRosterEntry {
            name: "ubu-1".to_owned(),
            os: RunnerOs::Linux,
            last_seen: now - chrono::TimeDelta::seconds(720),
        },
    ];
    let update = roster::apply_fetch(
        &roster,
        &registered,
        now,
        github::presence::DEFAULT_GRACE_SECS,
    );
    state.apply_runners(&update, u64::try_from(now.timestamp()).unwrap_or(0));
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;
    use github::runners::RunnerOs;
    use github::workflows::{RepoCounts, WorkflowRun};

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).expect("valid timestamp")
    }

    fn now_unix() -> u64 {
        u64::try_from(now().timestamp()).expect("post-epoch")
    }

    /// Every synthetic run decodes from JSON, so the DTO is on the path of
    /// every assertion below rather than being bypassed by a struct literal.
    fn run(status: &str, conclusion: Option<&str>, minutes_ago: i64) -> WorkflowRun {
        serde_json::from_value(json!({
            "id": 1,
            "name": "CI",
            "event": "push",
            "status": status,
            "conclusion": conclusion,
            "head_branch": "main",
            "html_url": "https://github.com/o/r/actions/runs/1",
            "created_at": (now() - TimeDelta::minutes(minutes_ago)).to_rfc3339(),
        }))
        .expect("fixture-shaped run")
    }

    fn health_of(slug: &str, runs: &[WorkflowRun], counts: RepoCounts) -> RepoWorkflowHealth {
        github::workflows::health(slug, runs, None, counts, now())
    }

    fn ready(health: Vec<RepoWorkflowHealth>) -> GitHubState {
        let mut state = GitHubState::new();
        state.apply_repos(health);
        state
    }

    fn rows(view: &Value) -> &Vec<Value> {
        view["rows"].as_array().expect("rows array")
    }

    fn cell(row: &Value, index: usize) -> &Value {
        &row["cells"][index]
    }

    fn cell_text(row: &Value, index: usize) -> &str {
        cell(row, index)["text"].as_str().expect("cell text")
    }

    /// The single row a one-repo state renders, owned — so a test can hold it
    /// without keeping the whole payload alive alongside it.
    fn only_row(state: &GitHubState, now: DateTime<Utc>) -> Value {
        let mut rows = repos_view(state, now)["rows"]
            .as_array()
            .expect("rows array")
            .clone();
        assert_eq!(rows.len(), 1, "this helper is for one-repo states");
        rows.remove(0)
    }

    /// Every row's `name`, in render order.
    fn row_names(view: &Value) -> Vec<String> {
        rows(view)
            .iter()
            .map(|row| row["name"].as_str().expect("name").to_owned())
            .collect()
    }

    const ISSUES: usize = 0;
    const PRS: usize = 1;
    const REMOTE: usize = 2;
    const LOCAL: usize = 3;
    const WT: usize = 4;
    const JOBS: usize = 5;
    const LONGEST: usize = 6;

    // MARK: - Repos: states

    #[test]
    fn the_repos_panel_asks_for_a_token_before_it_has_one() {
        let view = repos_view(&GitHubState::new(), now());
        assert_eq!(view["message"]["text"], UNAUTHENTICATED_MESSAGE);
        assert!(view["trailing"].is_null(), "no counts without credentials");
        assert!(view["health"].is_null());
        assert!(rows(&view).is_empty());
        assert_eq!(view["title"], "Repos");
        assert_eq!(view["id"], "ghWorkflows");
    }

    #[test]
    fn the_repos_panel_says_loading_between_the_token_and_the_first_fetch() {
        let mut state = GitHubState::new();
        state.apply_runners_error("boom"); // authenticates without repo health
        let view = repos_view(&state, now());
        assert_eq!(view["message"]["text"], REPOS_LOADING_MESSAGE);
        assert!(rows(&view).is_empty());
    }

    /// Clearing the token must not leave the last-known table on screen
    /// claiming knowledge the app no longer has.
    #[test]
    fn clearing_the_token_drops_back_to_the_connect_message() {
        let mut state = ready(vec![health_of("o/r", &[], RepoCounts::default())]);
        assert!(!rows(&repos_view(&state, now())).is_empty());
        state.apply_unauthenticated();
        assert_eq!(
            repos_view(&state, now())["message"]["text"],
            UNAUTHENTICATED_MESSAGE
        );
    }

    // MARK: - Repos: the row's tap target

    /// Character-for-character parity with `GHWorkflowsPanel.openActions(_:)`.
    #[test]
    fn a_row_carries_the_swift_tap_target() {
        let state = ready(vec![health_of(
            "Sassy-Dog/devcanopy",
            &[],
            RepoCounts::default(),
        )]);
        let row = only_row(&state, now());
        assert_eq!(row["url"], "https://github.com/Sassy-Dog/devcanopy/actions");
        assert_eq!(
            row["linkLabel"],
            "Open Sassy-Dog/devcanopy on GitHub Actions"
        );
    }

    /// Not being able to read a repo's runs is exactly when you want to go and
    /// look at them, so the unreachable row is clickable too.
    #[test]
    fn an_unreachable_row_is_still_clickable() {
        let state = ready(vec![RepoWorkflowHealth::unreachable("Sassy-Dog/platform")]);
        assert_eq!(
            only_row(&state, now())["url"],
            "https://github.com/Sassy-Dog/platform/actions"
        );
    }

    /// The security half of tap-to-open, and the closest thing to an automated
    /// check the ACL has (#123 still owns the IPC boundary itself).
    ///
    /// Reads the **real** `capabilities/default.json`, rebuilds the granted
    /// glob with the same `glob::Pattern` the plugin uses, and asserts it both
    /// admits every URL [`actions_url`] can produce and refuses everything
    /// else — including the App links this app deliberately still cannot open.
    /// Widening the scope in that file breaks this test, which is the point.
    #[test]
    fn actions_url_is_the_only_shape_the_granted_scope_admits() {
        const CAPABILITY: &str = include_str!("../../capabilities/default.json");
        let capability: Value = serde_json::from_str(CAPABILITY).expect("valid capability JSON");
        let permissions = capability["permissions"]
            .as_array()
            .expect("permissions array");

        // One grant, and it is the opener's. A second entry here is a widening
        // that has to be argued for in app/README.md first.
        assert_eq!(
            permissions.len(),
            1,
            "the ACL grants exactly one permission"
        );
        assert_eq!(permissions[0]["identifier"], "opener:allow-open-url");

        let allow = permissions[0]["allow"].as_array().expect("allow array");
        assert_eq!(allow.len(), 1, "one URL shape, not a list of them");
        // No `app` key: the entry keeps `Application::Default`, so the webview
        // cannot name *which* program opens the URL either.
        assert!(
            allow[0].get("app").is_none(),
            "the scope must not let the caller pick an application"
        );
        let pattern =
            glob::Pattern::new(allow[0]["url"].as_str().expect("scope url")).expect("valid glob");

        for slug in [
            "Sassy-Dog/devcanopy",
            "Sassy-Dog/qr-ninja",
            "o/r",
            "some-org/some.repo",
        ] {
            let url = actions_url(slug);
            assert!(pattern.matches(&url), "the scope must admit {url}");
        }

        for refused in [
            // The About tab's links — still unopenable, and that is deliberate.
            "https://github.com/Sassy-Dog/devcanopy",
            "https://github.com/Sassy-Dog/devcanopy/issues",
            "https://github.com/settings/tokens",
            // Anywhere else at all.
            "https://evil.example/actions",
            "http://github.com/o/r/actions",
            "https://github.com.evil.example/o/r/actions",
            "file:///etc/passwd",
            "javascript:alert(1)",
        ] {
            assert!(!pattern.matches(refused), "the scope must refuse {refused}");
        }
    }

    // MARK: - Repos: the "—" vs dimmed-0 rule

    /// The load-bearing distinction on every count cell. An unknown is a muted
    /// em dash; a real zero is a dimmed zero; a real number is ink.
    #[test]
    fn unknown_renders_an_em_dash_and_a_real_zero_renders_a_dimmed_zero() {
        let state = ready(vec![health_of(
            "Sassy-Dog/velovate",
            &[],
            RepoCounts {
                remote_branches: Some(0),
                open_issues_including_prs: None,
                open_pull_requests: None,
            },
        )]);
        let view = repos_view(&state, now());
        let row = &rows(&view)[0];

        assert_eq!(cell_text(row, ISSUES), "—", "a failed fetch is not zero");
        assert_eq!(cell(row, ISSUES)["color"], color::hex(color::MUTED));
        assert_eq!(cell_text(row, PRS), "—");

        assert_eq!(cell_text(row, REMOTE), "0", "a genuine zero survives");
        assert_eq!(
            cell(row, REMOTE)["color"],
            color::hex(color::MUTED),
            "a real zero dims so a non-zero pops"
        );

        // Not checked out here: local counts are unknown, never zero.
        assert_eq!(cell_text(row, LOCAL), "—");
        assert_eq!(cell_text(row, WT), "—");
    }

    #[test]
    fn a_non_zero_count_renders_in_ink() {
        let state = ready(vec![health_of(
            "o/velovate",
            &[],
            RepoCounts {
                remote_branches: Some(41),
                open_issues_including_prs: Some(9),
                open_pull_requests: Some(2),
            },
        )]);
        let row = &only_row(&state, now());
        assert_eq!(cell_text(row, ISSUES), "7", "9 inclusive − 2 PRs");
        assert_eq!(cell(row, ISSUES)["color"], color::hex(color::INK));
        assert_eq!(cell_text(row, PRS), "2");
        assert_eq!(cell_text(row, REMOTE), "41");
    }

    /// An unreachable repo knows nothing at all, and says so on every column
    /// rather than reporting zeroes it never read.
    #[test]
    fn an_unreachable_repo_renders_every_github_count_as_an_em_dash() {
        let state = ready(vec![RepoWorkflowHealth::unreachable("o/platform")]);
        let view = repos_view(&state, now());
        let row = &rows(&view)[0];
        for index in [ISSUES, PRS, REMOTE] {
            assert_eq!(cell_text(row, index), "—", "column {index}");
        }
        assert_eq!(cell_text(row, JOBS), "0", "no runs is a real zero");
        assert_eq!(row["dotColor"], color::hex(color::MUTED));
        assert_eq!(view["trailing"], "1 unreadable");
    }

    // MARK: - Repos: the local join

    #[test]
    fn local_counts_join_by_normalized_name() {
        let mut state = ready(vec![health_of(
            "Sassy-Dog/tailored-tip",
            &[],
            RepoCounts::default(),
        )]);
        // The directory on disk is spelled differently from the slug — which
        // is exactly what `normalize` exists to bridge.
        state.apply_local(BTreeMap::from([(
            "tailoredtip".to_owned(),
            LocalRepoCounts {
                local_branches: Some(5),
                worktrees: Some(2),
            },
        )]));
        let row = &only_row(&state, now());
        assert_eq!(cell_text(row, LOCAL), "5");
        assert_eq!(cell_text(row, WT), "2");
    }

    /// A repo whose scan half-failed reports the half it knows and an em dash
    /// for the other — never a zero, and never both blanked.
    #[test]
    fn a_half_readable_repo_reports_the_half_it_knows() {
        let mut state = ready(vec![health_of("o/velovate", &[], RepoCounts::default())]);
        state.apply_local(BTreeMap::from([(
            "velovate".to_owned(),
            LocalRepoCounts {
                local_branches: Some(3),
                worktrees: None,
            },
        )]));
        let row = &only_row(&state, now());
        assert_eq!(cell_text(row, LOCAL), "3");
        assert_eq!(cell_text(row, WT), "—");
    }

    // MARK: - Repos: dot precedence

    #[test]
    fn the_status_dot_follows_the_urgency_ladder() {
        let cases = [
            (
                RepoStatus::Failed,
                health_of(
                    "o/r",
                    &[run("completed", Some("failure"), 5)],
                    RepoCounts::default(),
                ),
            ),
            (
                RepoStatus::NeedsApproval,
                health_of("o/r", &[run("waiting", None, 5)], RepoCounts::default()),
            ),
            (
                RepoStatus::Running,
                health_of("o/r", &[run("in_progress", None, 5)], RepoCounts::default()),
            ),
            (
                RepoStatus::Healthy,
                health_of(
                    "o/r",
                    &[run("completed", Some("success"), 5)],
                    RepoCounts::default(),
                ),
            ),
            (
                RepoStatus::Unreachable,
                RepoWorkflowHealth::unreachable("o/r"),
            ),
        ];
        for (want, health) in cases {
            assert_eq!(status_of(&health), want, "{health:?}");
        }
    }

    /// A failure outranks a run in flight: a repo that is both broken and busy
    /// is broken. Getting this backwards paints a red repo amber.
    ///
    /// The in-flight run is deliberately a *PR* run. Two push runs would not
    /// test this at all: the newer one simply takes the `main` slot and
    /// supersedes the older failure, which is `crates/github`'s rule and the
    /// right one — the repo really is no longer known to be broken.
    #[test]
    fn a_failure_outranks_a_run_in_flight() {
        let pr_run: WorkflowRun = serde_json::from_value(json!({
            "id": 2, "name": "CI", "event": "pull_request", "status": "in_progress",
            "conclusion": null, "head_branch": "feat/x",
            "html_url": "https://x", "created_at": now().to_rfc3339(),
        }))
        .expect("run");
        let health = health_of(
            "o/r",
            &[run("completed", Some("failure"), 30), pr_run],
            RepoCounts::default(),
        );
        assert!(
            !health.running.is_empty(),
            "precondition: something is running"
        );
        assert!(
            health.main.as_ref().is_some_and(RunRef::is_failed),
            "precondition: main is red"
        );
        assert_eq!(status_of(&health), RepoStatus::Failed);
    }

    /// A queued run that has gone stale is a failure, not activity — the 17h51m
    /// incident, where a blocked run sat looking like a healthy long build.
    #[test]
    fn a_stuck_run_reddens_the_repo() {
        let health = health_of("o/r", &[run("queued", None, 120)], RepoCounts::default());
        assert!(!health.stuck.is_empty());
        assert_eq!(status_of(&health), RepoStatus::Failed);
    }

    #[test]
    fn only_the_approval_gate_blinks() {
        let approval = ready(vec![health_of(
            "o/r",
            &[run("waiting", None, 5)],
            RepoCounts::default(),
        )]);
        let row = &only_row(&approval, now());
        assert_eq!(row["blinking"], true);
        assert_eq!(row["dotColor"], color::hex(color::AMBER));

        let running = ready(vec![health_of(
            "o/r",
            &[run("in_progress", None, 5)],
            RepoCounts::default(),
        )]);
        let row = &only_row(&running, now());
        assert_eq!(
            row["blinking"], false,
            "running is activity, and must not pulse for attention"
        );
        assert_eq!(row["dotColor"], color::hex(color::AMBER));
    }

    // MARK: - Repos: JOBS + LONGEST

    #[test]
    fn jobs_is_amber_when_non_zero_and_dimmed_at_zero() {
        let busy = ready(vec![health_of(
            "o/r",
            &[run("in_progress", None, 5)],
            RepoCounts::default(),
        )]);
        let row = &only_row(&busy, now());
        assert_eq!(cell_text(row, JOBS), "1");
        assert_eq!(cell(row, JOBS)["color"], color::hex(color::AMBER));

        let idle = ready(vec![health_of("o/r", &[], RepoCounts::default())]);
        let row = &only_row(&idle, now());
        assert_eq!(cell_text(row, JOBS), "0");
        assert_eq!(cell(row, JOBS)["color"], color::hex(color::MUTED));
    }

    #[test]
    fn longest_is_a_middle_dot_when_nothing_is_running() {
        let state = ready(vec![health_of("o/r", &[], RepoCounts::default())]);
        let row = &only_row(&state, now());
        assert_eq!(cell_text(row, LONGEST), "·");
        assert_eq!(cell(row, LONGEST)["color"], color::hex(color::MUTED));
    }

    /// LONGEST reports the OLDEST running run — the longest-running one, which
    /// is the one worth looking at.
    #[test]
    fn longest_reports_the_oldest_running_run_in_amber() {
        let state = ready(vec![health_of(
            "o/r",
            &[run("in_progress", None, 4), run("in_progress", None, 95)],
            RepoCounts::default(),
        )]);
        let row = &only_row(&state, now());
        assert_eq!(cell_text(row, LONGEST), "1h35m");
        assert_eq!(cell(row, LONGEST)["color"], color::hex(color::AMBER));
    }

    /// The elapsed ladder keeps minutes past the hour, unlike the panel
    /// footers' single-unit one: 3h and 3h50m are different situations.
    #[test]
    fn the_elapsed_ladder_keeps_minutes_past_the_hour() {
        assert_eq!(elapsed(0), "0s");
        assert_eq!(elapsed(45), "45s");
        assert_eq!(elapsed(59), "59s");
        assert_eq!(elapsed(60), "1m");
        assert_eq!(elapsed(3_599), "59m");
        assert_eq!(elapsed(3_600), "1h00m");
        assert_eq!(elapsed(13_020), "3h37m");
        assert_eq!(
            elapsed(90_000),
            "25h00m",
            "no day unit — hours keep climbing"
        );
        // A clock skew must not format as a negative age.
        assert_eq!(elapsed(-30), "0s");
    }

    /// The column advances between fetches: the same state rendered a minute
    /// later says a minute more.
    #[test]
    fn longest_advances_with_render_time_rather_than_freezing_at_the_fetch() {
        let state = ready(vec![health_of(
            "o/r",
            &[run("in_progress", None, 1)],
            RepoCounts::default(),
        )]);
        assert_eq!(
            cell_text(&rows(&repos_view(&state, now()))[0], LONGEST),
            "1m"
        );
        let later = now() + TimeDelta::minutes(5);
        assert_eq!(
            cell_text(&rows(&repos_view(&state, later))[0], LONGEST),
            "6m"
        );
    }

    // MARK: - Repos: trailing + health line

    #[test]
    fn the_trailing_label_orders_problems_by_urgency() {
        let state = ready(vec![
            health_of("o/a", &[run("waiting", None, 5)], RepoCounts::default()),
            health_of("o/b", &[run("queued", None, 120)], RepoCounts::default()),
            health_of("o/c", &[run("in_progress", None, 5)], RepoCounts::default()),
            health_of(
                "o/d",
                &[run("completed", Some("failure"), 5)],
                RepoCounts::default(),
            ),
            RepoWorkflowHealth::unreachable("o/e"),
        ]);
        assert_eq!(
            repos_view(&state, now())["trailing"],
            "1 needs approval · 1 stuck · 1 running · 1 failed · 1 unreadable"
        );
    }

    #[test]
    fn a_quiet_portfolio_says_all_green() {
        let state = ready(vec![health_of(
            "o/r",
            &[run("completed", Some("success"), 5)],
            RepoCounts::default(),
        )]);
        let view = repos_view(&state, now());
        assert_eq!(view["trailing"], "all green");
        assert_eq!(view["health"]["text"], "✓ all 1 healthy");
        assert_eq!(view["health"]["color"], color::hex(color::GREEN));
    }

    /// A repo with a build in flight is still healthy — the fraction must not
    /// imply a problem just because something is running.
    #[test]
    fn a_running_repo_still_counts_healthy() {
        let state = ready(vec![
            health_of("o/a", &[run("in_progress", None, 5)], RepoCounts::default()),
            health_of(
                "o/b",
                &[run("completed", Some("success"), 5)],
                RepoCounts::default(),
            ),
        ]);
        let view = repos_view(&state, now());
        assert_eq!(view["health"]["text"], "✓ all 2 healthy");
        assert_eq!(view["trailing"], "1 running");
    }

    #[test]
    fn the_health_line_becomes_a_fraction_once_something_is_wrong() {
        let state = ready(vec![
            health_of(
                "o/a",
                &[run("completed", Some("failure"), 5)],
                RepoCounts::default(),
            ),
            health_of(
                "o/b",
                &[run("completed", Some("success"), 5)],
                RepoCounts::default(),
            ),
            RepoWorkflowHealth::unreachable("o/c"),
        ]);
        assert_eq!(repos_view(&state, now())["health"]["text"], "✓ 1/3 healthy");
    }

    /// Failed *slots*, not failed repos: two broken things to fix is "2 failed"
    /// even when they are on one repo.
    #[test]
    fn a_repo_failing_on_both_main_and_its_last_pr_counts_twice() {
        let pr_run: WorkflowRun = serde_json::from_value(json!({
            "id": 2, "name": "CI", "event": "pull_request", "status": "completed",
            "conclusion": "failure", "head_branch": "feat/x",
            "html_url": "https://x", "created_at": now().to_rfc3339(),
        }))
        .expect("run");
        let state = ready(vec![health_of(
            "o/r",
            &[run("completed", Some("failure"), 5), pr_run],
            RepoCounts::default(),
        )]);
        assert_eq!(repos_view(&state, now())["trailing"], "2 failed");
    }

    // MARK: - Repos: shape

    #[test]
    fn the_columns_are_the_swift_widths_in_the_swift_order() {
        let view = repos_view(&ready(Vec::new()), now());
        let columns = view["columns"].as_array().expect("columns");
        let labels: Vec<&str> = columns
            .iter()
            .map(|c| c["label"].as_str().expect("label"))
            .collect();
        assert_eq!(
            labels,
            vec!["REPO", "ISSUES", "PRS", "REMOTE", "LOCAL", "WT", "JOBS", "LONGEST"]
        );
        assert!(columns[0]["width"].is_null(), "REPO takes what's left");

        // The fixed widths sum to the figure `PanelKind::GhWorkflows.min_width`
        // is built on — widen a column without moving that breakpoint and the
        // panel silently outgrows the width it claims to need.
        let fixed: f64 = columns.iter().filter_map(|c| c["width"].as_f64()).sum();
        assert!(
            (fixed - 312.0).abs() < f64::EPSILON,
            "fixed columns sum to {fixed}"
        );
        assert!(PanelKind::GhWorkflows.min_width() >= fixed);
    }

    /// Every row carries one cell per fixed column, in the header's order — the
    /// frontend zips the two and cannot notice a mismatch.
    #[test]
    fn every_row_has_one_cell_per_fixed_column() {
        let state = ready(vec![health_of("o/r", &[], RepoCounts::default())]);
        let view = repos_view(&state, now());
        let fixed = view["columns"].as_array().expect("columns").len() - 1;
        for row in rows(&view) {
            assert_eq!(row["cells"].as_array().expect("cells").len(), fixed);
        }
    }

    /// Sorted by short name, case-insensitively — a stable order, so a row does
    /// not jump around as CI activity changes.
    #[test]
    fn rows_are_sorted_by_short_name_case_insensitively() {
        let state = ready(vec![
            health_of("Sassy-Dog/Velovate", &[], RepoCounts::default()),
            health_of("Sassy-Dog/devcanopy", &[], RepoCounts::default()),
            health_of("Other-Org/apple", &[], RepoCounts::default()),
        ]);
        assert_eq!(
            row_names(&repos_view(&state, now())),
            vec!["apple", "devcanopy", "Velovate"]
        );
    }

    // MARK: - Runners

    fn runner(name: &str, os: RunnerOs, state: RunnerState) -> GhRunner {
        GhRunner {
            id: 1,
            name: name.to_owned(),
            os,
            state,
        }
    }

    fn with_runners(registered: &[GhRunner], roster: &[RunnerRosterEntry]) -> GitHubState {
        let mut state = GitHubState::new();
        let update = roster::apply_fetch(
            roster,
            registered,
            now(),
            github::presence::DEFAULT_GRACE_SECS,
        );
        state.apply_runners(&update, now_unix());
        state
    }

    #[test]
    fn the_runners_panel_asks_for_a_token_before_it_has_one() {
        let view = runners_view(&GitHubState::new(), now_unix());
        assert_eq!(view["message"]["text"], UNAUTHENTICATED_MESSAGE);
        assert!(view["trailing"].is_null());
        assert!(
            view["footer"].is_null(),
            "nothing to be stale without a token"
        );
        assert!(rows(&view).is_empty());
        assert_eq!(view["title"], "GitHub Runners");
        assert_eq!(view["id"], "ghRunners");
    }

    #[test]
    fn the_runners_panel_says_loading_between_the_token_and_the_first_fetch() {
        let mut state = GitHubState::new();
        state.apply_repos(Vec::new()); // authenticates without a runners fetch
        let view = runners_view(&state, now_unix());
        assert_eq!(view["message"]["text"], RUNNERS_LOADING_MESSAGE);
        assert!(view["footer"].is_null());
    }

    #[test]
    fn the_summary_row_and_os_chips_come_from_the_registered_runners() {
        let state = with_runners(
            &[
                runner("mac-s1", RunnerOs::MacOs, RunnerState::Busy),
                runner("mac-s2", RunnerOs::MacOs, RunnerState::Idle),
                runner("ubu-1", RunnerOs::Linux, RunnerState::Idle),
                runner("ubu-2", RunnerOs::Linux, RunnerState::Offline),
            ],
            &[],
        );
        let view = runners_view(&state, now_unix());
        assert_eq!(view["trailing"], "3/4");
        assert_eq!(
            view["stats"],
            json!([
                { "label": "ONLINE", "value": "3/4", "color": color::hex(color::GREEN) },
                { "label": "BUSY", "value": "1", "color": color::hex(color::AMBER) },
                { "label": "IDLE", "value": "2", "color": color::hex(color::GREEN) },
            ])
        );
        assert_eq!(view["chips"], json!(["macOS 2/2", "Linux 1/2"]));
    }

    /// Zero busy runners is a resting org, not a warning.
    #[test]
    fn a_resting_org_dims_the_busy_stat() {
        let state = with_runners(&[runner("mac-s1", RunnerOs::MacOs, RunnerState::Idle)], &[]);
        let view = runners_view(&state, now_unix());
        assert_eq!(view["stats"][1]["color"], color::hex(color::MUTED));
        // Only the platform the org actually has gets a chip.
        assert_eq!(view["chips"], json!(["macOS 1/1"]));
    }

    #[test]
    fn runner_rows_carry_their_state_word_and_colour() {
        let state = with_runners(
            &[
                runner("mac-s1", RunnerOs::MacOs, RunnerState::Busy),
                runner("ubu-1", RunnerOs::Linux, RunnerState::Idle),
                runner("ubu-2", RunnerOs::Linux, RunnerState::Offline),
            ],
            &[],
        );
        let view = runners_view(&state, now_unix());
        assert_eq!(
            rows(&view)
                .iter()
                .map(|r| (
                    r["name"].as_str().expect("name"),
                    r["os"].as_str().expect("os"),
                    r["status"].as_str().expect("status"),
                    r["dotColor"].as_str().expect("dot"),
                ))
                .collect::<Vec<_>>(),
            vec![
                ("mac-s1", "MACOS", "busy", color::hex(color::AMBER).as_str()),
                ("ubu-1", "LINUX", "idle", color::hex(color::GREEN).as_str()),
                (
                    "ubu-2",
                    "LINUX",
                    "offline",
                    color::hex(color::MUTED).as_str()
                ),
            ]
        );
    }

    /// A remembered runner that de-registered holds its slot, amber inside the
    /// grace window and red past it — and only the red ones reach the trailing
    /// count, because ephemeral churn is not news.
    #[test]
    fn absent_runners_hold_their_slot_and_escalate_past_grace() {
        let state = with_runners(
            &[runner("mac-s1", RunnerOs::MacOs, RunnerState::Idle)],
            &[
                RunnerRosterEntry {
                    name: "mac-s2".to_owned(),
                    os: RunnerOs::MacOs,
                    last_seen: now() - TimeDelta::seconds(40),
                },
                RunnerRosterEntry {
                    name: "mac-s3".to_owned(),
                    os: RunnerOs::MacOs,
                    last_seen: now() - TimeDelta::seconds(720),
                },
            ],
        );
        let view = runners_view(&state, now_unix());
        let rows = rows(&view);
        assert_eq!(
            rows.iter()
                .map(|r| (
                    r["kind"].as_str().expect("kind"),
                    r["name"].as_str().expect("name"),
                    r["status"].as_str().expect("status"),
                ))
                .collect::<Vec<_>>(),
            vec![
                ("registered", "mac-s1", "idle"),
                ("absent", "mac-s2", "recycling 40s"),
                ("absent", "mac-s3", "missing 12m"),
            ]
        );
        assert_eq!(rows[1]["dotColor"], color::hex(color::AMBER));
        assert_eq!(rows[2]["dotColor"], color::hex(color::RED));
        assert_eq!(
            view["trailing"], "1/1 · 1 missing",
            "recycling churn must not inflate the missing count"
        );
    }

    /// The clock-freeze contract, from the panel's side: a failed fetch keeps
    /// every row and every absence label exactly as the last successful one
    /// left them, and only the footer changes.
    #[test]
    fn a_failed_fetch_keeps_the_last_good_rows_and_adds_a_footer() {
        let mut state = with_runners(
            &[runner("mac-s1", RunnerOs::MacOs, RunnerState::Busy)],
            &[RunnerRosterEntry {
                name: "mac-s2".to_owned(),
                os: RunnerOs::MacOs,
                last_seen: now() - TimeDelta::seconds(40),
            }],
        );
        let before = runners_view(&state, now_unix());

        state.apply_runners_error(RUNNERS_ERROR_MESSAGE);
        // An hour later, still failing.
        let after = runners_view(&state, now_unix() + 3_600);

        assert_eq!(after["rows"], before["rows"], "an outage ages nothing");
        assert_eq!(after["stats"], before["stats"]);
        assert_eq!(after["trailing"], before["trailing"]);
        assert!(before["footer"].is_null(), "it was fresh a moment ago");
        assert_eq!(
            after["footer"]["text"],
            format!("⚠ {RUNNERS_ERROR_MESSAGE} · last ok 1h ago")
        );
        assert!(
            after["message"].is_null(),
            "a failure is not a loading state"
        );
    }

    #[test]
    fn a_stale_but_unbroken_panel_warns_only_past_its_own_window() {
        let state = with_runners(&[runner("mac-s1", RunnerOs::MacOs, RunnerState::Idle)], &[]);
        assert!(runners_view(&state, now_unix() + RUNNERS_STALE_AFTER_SECS)["footer"].is_null());
        assert_eq!(
            runners_view(&state, now_unix() + RUNNERS_STALE_AFTER_SECS + 1)["footer"]["text"],
            "⚠ stale · updated 2m ago"
        );
    }

    /// Clearing the token clears the rows but must NOT clear the roster, which
    /// lives in the store — expectations resume intact when auth returns.
    #[test]
    fn clearing_the_token_clears_the_runner_rows() {
        let mut state = with_runners(&[runner("mac-s1", RunnerOs::MacOs, RunnerState::Idle)], &[]);
        state.apply_unauthenticated();
        let view = runners_view(&state, now_unix());
        assert_eq!(view["message"]["text"], UNAUTHENTICATED_MESSAGE);
        assert!(rows(&view).is_empty());
        assert!(view["stats"].as_array().expect("stats").is_empty());
    }

    // MARK: - Roster persistence bridge

    #[test]
    fn the_roster_round_trips_through_the_stored_records() {
        let entries = vec![
            RunnerRosterEntry {
                name: "mac-s1".to_owned(),
                os: RunnerOs::MacOs,
                last_seen: now(),
            },
            RunnerRosterEntry {
                name: "ubu-1".to_owned(),
                os: RunnerOs::Linux,
                last_seen: now() - TimeDelta::seconds(60),
            },
            RunnerRosterEntry {
                name: "win-1".to_owned(),
                os: RunnerOs::Other,
                last_seen: now(),
            },
        ];
        let records = roster_to_records(&entries);
        assert_eq!(
            records.iter().map(|r| r.os.as_str()).collect::<Vec<_>>(),
            vec!["macOS", "linux", "other"],
            "the stored spelling is the Swift raw value, not the display label"
        );
        assert_eq!(roster_from_records(&records), entries);
    }

    /// One unreadable entry must cost us that entry, not the whole roster.
    #[test]
    fn an_undatable_record_is_dropped_without_taking_the_roster_with_it() {
        let records = vec![
            RunnerRosterRecord {
                name: "mac-s1".to_owned(),
                os: "macOS".to_owned(),
                last_seen: u64::MAX,
            },
            RunnerRosterRecord {
                name: "ubu-1".to_owned(),
                os: "linux".to_owned(),
                last_seen: now_unix(),
            },
        ];
        let roster = roster_from_records(&records);
        assert_eq!(
            roster.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["ubu-1"]
        );
    }

    #[test]
    fn an_unrecognised_stored_os_reads_as_other() {
        let roster = roster_from_records(&[RunnerRosterRecord {
            name: "bsd-1".to_owned(),
            os: "freebsd".to_owned(),
            last_seen: now_unix(),
        }]);
        assert_eq!(roster[0].os, RunnerOs::Other);
    }

    // MARK: - Fixture

    /// The dumped fixtures are what the Playwright suite renders, so the state
    /// behind them has to actually contain the cases those tests claim to
    /// cover — otherwise the suite passes against a payload with nothing in it.
    #[test]
    fn the_fixture_covers_every_rendering_the_panels_have() {
        let state = fixture_state(now());
        let repos = repos_view(&state, now());
        let rows = rows(&repos);
        assert_eq!(rows.len(), 6);

        let texts: Vec<Vec<&str>> = rows
            .iter()
            .map(|row| {
                row["cells"]
                    .as_array()
                    .expect("cells")
                    .iter()
                    .map(|c| c["text"].as_str().expect("text"))
                    .collect()
            })
            .collect();
        let flat: Vec<&str> = texts.iter().flatten().copied().collect();
        assert!(flat.contains(&"—"), "an unknown count");
        assert!(flat.contains(&"0"), "a genuine zero beside it");
        assert!(flat.contains(&"·"), "a repo with nothing running");
        assert!(
            rows.iter().any(|r| r["blinking"] == true),
            "a repo parked at an approval gate"
        );
        assert!(
            rows.iter().any(|r| r["dotColor"] == color::hex(color::RED)),
            "a failing repo"
        );
        assert!(
            rows.iter()
                .any(|r| r["dotColor"] == color::hex(color::MUTED)),
            "an unreachable repo"
        );
        assert_eq!(repos["health"]["text"], "✓ 4/6 healthy");

        let runners = runners_view(
            &state,
            u64::try_from(now().timestamp()).expect("post-epoch"),
        );
        let kinds: Vec<&str> = super::tests::rows(&runners)
            .iter()
            .map(|r| r["kind"].as_str().expect("kind"))
            .collect();
        assert!(kinds.contains(&"registered"));
        assert!(kinds.contains(&"absent"));
        assert_eq!(runners["trailing"], "3/4 · 1 missing");
        assert_eq!(runners["chips"], json!(["macOS 2/2", "Linux 1/2"]));
    }
}
