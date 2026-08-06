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

use crate::panel::Configured;
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

// Fixed column widths. Each is the wider of the two things it must hold — its
// 9pt header label and its 11pt value — plus a little margin, and nothing
// more: the Swift panel's originals were half again this size, which spread
// the numbers so far apart that a row read as scattered digits rather than one
// record, and cost Repos the second column it now fits in.
//
// They sum to 214pt, the figure `PanelKind::GhWorkflows.min_width` is built
// on — widen one and that breakpoint has to move with it.
const ISSUES_W: f64 = 36.0; // "ISSUES" 32.4
const PRS_W: f64 = 24.0; // 3-digit value 19.8
const LOCAL_W: f64 = 30.0; // "LOCAL" 27.0
const REMOTE_W: f64 = 36.0; // "REMOTE" 32.4
const WT_W: f64 = 20.0; // 3-digit value 19.8
const JOBS_W: f64 = 26.0; // "JOBS" 21.6
const LONGEST_W: f64 = 42.0; // "LONGEST" 37.8

/// The cockpit's monospace advance at the repo rows' 11pt, in points — the
/// same 0.6em rule [`MONO_9_CHAR_W`] rests on, one size up.
#[cfg(test)]
const MONO_11_CHAR_W: f64 = 6.6;

/// The longest repo short-name the column holds without ellipsis, in
/// characters. `tailoredtip` is 11 today; 14 leaves headroom without leaving a
/// visible void between the name and the numbers, and anything longer
/// ellipsizes rather than pushing a column (`.gh-repo-name` sets
/// `text-overflow`).
#[cfg(test)]
const REPO_NAME_CHARS: usize = 14;

/// **Fixed, not a minimum**, for the reason every other column here is
/// (#206): a name column that grows to its own text drags all seven numeric
/// columns right on exactly the row whose name is longest.
///
/// It is also what stops the numeric block being flung to the panel's far
/// edge. Before this, `REPO` took every point the fixed columns left over, so
/// on a wide panel a row read as two clusters with a void between them; now
/// the row is one unit and the slack goes to the panel's trailing edge —
/// which is where a second column lands when one fits.
const REPO_NAME_W: f64 = 96.0;

/// The header row. `REPO` is the only left-aligned column.
const COLUMNS: [(&str, Option<f64>); 8] = [
    ("REPO", Some(REPO_NAME_W)),
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
    ///
    /// Three states rather than a `bool`: this used to default to `false` and
    /// only become `true` once a fetch *completed*, so for the whole of the
    /// first pass — the credential read plus every request after it — both
    /// panels claimed there was no token. See [`Configured`].
    token: Configured,
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
    /// Set when the credential store would not answer, cleared by every read
    /// that does. A third state on purpose: `!authenticated` asserts there is
    /// no token and `runners_error` blames GitHub, and this is neither — we do
    /// not know whether a token is configured, so both panels must say exactly
    /// that instead of picking one of the two claims they cannot support.
    credential_error: Option<String>,
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
        self.token = Configured::Absent;
        self.health = None;
        self.summary = None;
        self.runners.clear();
        self.absent.clear();
        self.runners_error = None;
        self.credential_error = None;
    }

    /// The credential store refused to answer, so we do not know whether a
    /// token is configured.
    ///
    /// Deliberately **not** [`apply_unauthenticated`](Self::apply_unauthenticated):
    /// that one paints "connect a GitHub token in Settings" — a statement about
    /// how the app is configured — and clears every fetched row on the way.
    /// Making that statement because a keychain was locked wipes two panels and
    /// sends the operator to paste a token they already pasted. Nothing fetched
    /// is touched here; only the reason is recorded, and both views lead with
    /// it.
    pub fn apply_credential_unreadable(&mut self, message: impl Into<String>) {
        self.credential_error = Some(message.into());
    }

    /// Records one completed Repos pass. Wholesale, never merged: the row set
    /// is the enabled-repo list, so a repo removed in Settings must lose its
    /// row on the next pass rather than linger as a stale one.
    pub fn apply_repos(&mut self, health: Vec<RepoWorkflowHealth>) {
        self.token = Configured::Present;
        self.credential_error = None;
        self.health = Some(health);
    }

    /// Records one local git scan.
    pub fn apply_local(&mut self, local: BTreeMap<String, LocalRepoCounts>) {
        self.local = local;
    }

    /// Records one **successful** org-runners fetch.
    pub fn apply_runners(&mut self, update: &roster::RosterUpdate, now: u64) {
        self.token = Configured::Present;
        self.credential_error = None;
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
        self.token = Configured::Present;
        self.credential_error = None;
        self.runners_error = Some(message.into());
    }

    /// A pass read a non-empty token from the credential store.
    ///
    /// Called the moment the credential is in hand, **before** the first
    /// request — which is the whole point. Every other setter here runs after a
    /// fetch completes, so without this one a panel spends the entire first pass
    /// unable to say whether a token exists, and says the thing that sends the
    /// operator to Settings. Nothing fetched is touched: this is a statement
    /// about the credential store, not about GitHub.
    pub fn apply_token_present(&mut self) {
        self.token = Configured::Present;
        self.credential_error = None;
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
    // The credential-store failure outranks both of the others: it is the only
    // one of the three that is true when it is set, since "connect a token" and
    // "loading…" are each a claim about a credential this pass never saw.
    let message = if let Some(reason) = state.credential_error.as_deref() {
        Some(reason)
    } else if state.token.is_absent() {
        // Only a pass that looked and found nothing may say this. `Unknown`
        // falls through to "loading…" below, which is what the first frame is.
        Some(UNAUTHENTICATED_MESSAGE)
    } else if state.health.is_none() {
        Some(REPOS_LOADING_MESSAGE)
    } else {
        None
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
        // Published, not re-derived: the frontend polls faster while a panel is
        // still filling in, and inferring that from the message text would make
        // it parse a string it is otherwise careful never to interpret.
        "loading": repos_loading(state),
        "columns": columns(),
        "rows": sorted
            .iter()
            .map(|h| repo_row(h, &state.local, now))
            .collect::<Vec<_>>(),
        "health": if message.is_none() { health_line(health) } else { Value::Null },
    })
}

/// Whether the Repos panel is still waiting on the answer to its first pass.
///
/// The same predicate the message ladder uses, so the flag cannot disagree with
/// the line the panel is painting. An unreadable credential store is *not*
/// loading — that pass finished, with an answer we did not like.
fn repos_loading(state: &GitHubState) -> bool {
    state.credential_error.is_none() && !state.token.is_absent() && state.health.is_none()
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

/// The cockpit's monospace advance at the runner rows' 9pt, in points. Every
/// glyph in `ui-monospace`/SF Mono is 0.6em wide, which is the only reason a
/// *character* count below can become a *point* width at all.
///
/// Only the test that re-derives [`RUNNER_STATUS_W`] reads it: the shipped
/// payload carries the points, not the arithmetic behind them.
#[cfg(test)]
const MONO_9_CHAR_W: f64 = 5.4;

/// The longest status a runner row can hold, in characters: `"recycling 59s"`
/// and `"missing 1234d"` both land here, and
/// `the_reserved_status_column_fits_every_word_the_panel_can_say` walks the
/// whole vocabulary to keep that true.
#[cfg(test)]
const RUNNER_STATUS_CHARS: usize = 13;

/// The status column's reserved footprint, in points — `RUNNER_STATUS_CHARS`
/// characters of the panel's 9pt monospace, rounded up.
///
/// **Fixed, not a minimum.** A row's status is the widest thing in it that
/// changes: a presence label (`"recycling 40s"`) is nearly three times the
/// width of a state word (`"idle"`), and a column that sizes itself to its
/// content drags the OS chip left on exactly the rows something is wrong with —
/// so the panel's alignment breaks at the moment it is being read hardest
/// (#206). `GHRunnersPanel` reserves 48pt, which fits the state words and not
/// the presence labels; this is the same reservation sized for both.
const RUNNER_STATUS_W: f64 = 74.0;

/// The whole GitHub Runners payload.
///
/// `now` is wall-clock unix seconds, used only by the footer's staleness. Every
/// *absence* clock in `state.absent` was computed at the last successful fetch
/// and is never recomputed here — that is what freezes them while GitHub is
/// unreachable.
#[must_use]
pub fn runners_view(state: &GitHubState, now: u64) -> Value {
    // `credential_error` holds this branch back: the zero-credential payload
    // asserts there is no token *and* blanks the rows, and neither survives
    // "we could not ask". Everything the last good fetch left — stats, chips,
    // rows, footer — is rendered below with the reason on top of it.
    //
    // `is_absent`, not `!is_present`: before the first pass reads the credential
    // store this panel knows nothing about the token, and the zero-credential
    // payload is an assertion it has no basis for. That state falls through to
    // "loading runners…" below instead.
    if state.token.is_absent() && state.credential_error.is_none() {
        return json!({
            "id": PanelKind::GhRunners.id(),
            "title": PanelKind::GhRunners.title(),
            "trailing": Value::Null,
            "message": { "text": UNAUTHENTICATED_MESSAGE },
            "loading": false,
            "stats": [],
            "chips": [],
            "rows": [],
            // No footer without credentials: there is nothing to be stale.
            "footer": Value::Null,
        });
    }

    // "loading runners…" only while nothing has been heard AND nothing has
    // failed. Once there is an error the footer carries it, and a "loading"
    // line beside a failure would be a second, contradictory story. An
    // unreadable credential store is the same argument one layer up: a pass
    // that never got a token is not fetching, so it cannot be loading.
    let message = if let Some(reason) = state.credential_error.as_deref() {
        Some(reason)
    } else if state.summary.is_none() && state.runners_error.is_none() {
        Some(RUNNERS_LOADING_MESSAGE)
    } else {
        None
    };

    json!({
        "id": PanelKind::GhRunners.id(),
        "title": PanelKind::GhRunners.title(),
        "trailing": runners_trailing(state).map_or(Value::Null, Value::String),
        "message": message.map_or(Value::Null, |text| json!({ "text": text })),
        // Same predicate as the ladder above, published for the frontend's
        // refresh cadence rather than inferred from the message text.
        "loading": message.is_some_and(|text| text == RUNNERS_LOADING_MESSAGE),
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

/// BUSY / IDLE / OFFLINE. No ONLINE stat: the panel's trailing already carries
/// `online/total` in the top right, where every other card puts its rollup, so
/// a fourth copy of it here spent a whole stat slot restating the header.
/// OFFLINE is what that slot now holds — the one count the trailing does *not*
/// give you, since `total - online` is arithmetic nobody should do at a glance.
///
/// BUSY and OFFLINE both dim at zero and glow amber above it: no busy runners
/// is a resting org and no offline runners is a healthy one, and a stat that
/// glows permanently is a stat the eye stops reading.
fn summary_stats(summary: RunnerSummary) -> Vec<Value> {
    let stat = |label: &str, value: String, tint: u32| json!({ "label": label, "value": value, "color": color::hex(tint) });
    let alert = |count: usize| {
        if count > 0 {
            color::AMBER
        } else {
            color::MUTED
        }
    };
    // Derived rather than counted: `online` is already "not offline", so the
    // remainder is exactly the offline set and cannot disagree with the
    // trailing built from the same two numbers.
    let offline = summary.total.saturating_sub(summary.online);
    vec![
        stat("BUSY", summary.busy.to_string(), alert(summary.busy)),
        stat("IDLE", summary.idle.to_string(), color::GREEN),
        stat("OFFLINE", offline.to_string(), alert(offline)),
    ]
}

/// `"macOS 2/2"` / `"Linux 1/2"` / `"Windows 0/0"` — the three tracked
/// platforms, always, even at zero.
///
/// This used to hide a platform the org had none of, on the argument that
/// `Linux 0/0` describes nothing. That is wrong for a platform you are in the
/// middle of standing up: the chip is the tracked *slot*, and an empty one says
/// "still nothing here", which is precisely the thing being watched. A chip
/// that only exists once it is non-zero cannot report the zero you are waiting
/// to see change.
///
/// `Other` keeps the old conditional. It is the untracked remainder rather than
/// a slot anyone is filling, so an `Other 0/0` really would be furniture — but
/// a non-zero one has to appear, or a runner on something we do not name would
/// count toward the total with no chip accounting for it.
fn os_chips(summary: RunnerSummary) -> Vec<Value> {
    let chip =
        |label: &str, online: usize, total: usize| json!(format!("{label} {online}/{total}"));
    let mut chips = vec![
        chip("macOS", summary.macos_online, summary.macos_total),
        chip("Linux", summary.linux_online, summary.linux_total),
        chip("Windows", summary.windows_online, summary.windows_total),
    ];
    if summary.other_total > 0 {
        chips.push(chip("Other", summary.other_online, summary.other_total));
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
        // Every row, registered or absent, reserves the same slot — that is
        // what keeps the OS chips in one column while one runner recycles.
        "statusWidth": RUNNER_STATUS_W,
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

    /// The first frame, before any pass has read the credential store.
    ///
    /// This used to assert [`UNAUTHENTICATED_MESSAGE`] — the panel told an
    /// operator with a perfectly good token to go and connect one, on every
    /// launch, because `authenticated: bool` defaulted to `false` and only a
    /// *completed fetch* set it. A fresh state knows nothing about the token,
    /// and "loading…" is the only line it can support.
    #[test]
    fn the_repos_panel_says_loading_before_it_has_looked_for_a_token() {
        let view = repos_view(&GitHubState::new(), now());
        assert_eq!(view["message"]["text"], REPOS_LOADING_MESSAGE);
        assert_eq!(view["loading"], true);
        assert!(
            view["trailing"].is_null(),
            "no counts before the first pass"
        );
        assert!(view["health"].is_null());
        assert!(rows(&view).is_empty());
        // The title is `PanelKind::GhWorkflows.title()`; the id stays the one
        // every payload and DOM section has always used.
        assert_eq!(view["title"], "GitHub Repos");
        assert_eq!(view["id"], "ghWorkflows");
    }

    /// …and the setup instruction still appears for the state that earned it:
    /// a pass that read the store and found nothing.
    #[test]
    fn the_repos_panel_asks_for_a_token_once_a_pass_finds_none() {
        let mut state = GitHubState::new();
        state.apply_unauthenticated();
        let view = repos_view(&state, now());
        assert_eq!(view["message"]["text"], UNAUTHENTICATED_MESSAGE);
        assert_eq!(view["loading"], false, "we looked; this is not loading");
        assert!(view["trailing"].is_null(), "no counts without credentials");
        assert!(rows(&view).is_empty());
    }

    #[test]
    fn the_repos_panel_says_loading_between_the_token_and_the_first_fetch() {
        let mut state = GitHubState::new();
        state.apply_runners_error("boom"); // authenticates without repo health
        let view = repos_view(&state, now());
        assert_eq!(view["message"]["text"], REPOS_LOADING_MESSAGE);
        assert_eq!(view["loading"], true);
        assert!(rows(&view).is_empty());
    }

    /// The window the `Credential::Present` arm closes: a token is in hand and
    /// the fetch is still running. Reading the credential is what proves there
    /// is one — waiting for the fetch to finish is what used to make this frame
    /// indistinguishable from having no token at all.
    #[test]
    fn a_token_in_hand_reads_as_loading_for_the_whole_of_the_fetch() {
        let mut state = GitHubState::new();
        state.apply_token_present();
        for view in [repos_view(&state, now()), runners_view(&state, now_unix())] {
            assert_ne!(view["message"]["text"], UNAUTHENTICATED_MESSAGE);
            assert_eq!(view["loading"], true);
        }
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

    /// …and the case that is *not* that one: a credential store that would not
    /// answer is not a cleared token, so the panel must not print the line that
    /// tells the operator to go and configure what they already configured.
    #[test]
    fn an_unreadable_credential_store_names_the_store_instead_of_asking_for_a_token() {
        let mut state = ready(vec![health_of("o/r", &[], RepoCounts::default())]);
        state.apply_credential_unreadable(crate::CREDENTIAL_UNREADABLE_MESSAGE);
        let view = repos_view(&state, now());
        assert_eq!(
            view["message"]["text"],
            crate::CREDENTIAL_UNREADABLE_MESSAGE
        );
        assert_ne!(view["message"]["text"], UNAUTHENTICATED_MESSAGE);
    }

    /// The retention half: nothing fetched is dropped, so the moment a pass
    /// reads the credential again the table is the one that was already there.
    /// After `apply_unauthenticated` the same sequence would render "loading…"
    /// over an empty panel instead.
    #[test]
    fn an_unreadable_credential_store_keeps_the_repo_table() {
        let mut state = ready(vec![health_of("o/r", &[], RepoCounts::default())]);
        state.apply_credential_unreadable(crate::CREDENTIAL_UNREADABLE_MESSAGE);
        // The next pass reads the token fine; only its runners fetch fails.
        state.apply_runners_error(RUNNERS_ERROR_MESSAGE);
        let view = repos_view(&state, now());
        assert!(
            view["message"].is_null(),
            "a readable store must not leave the reason behind"
        );
        assert_eq!(row_names(&view), vec!["r".to_owned()]);
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
        // REPO is a reservation like every other column: a name column that
        // grew to its own text would drag all seven numeric columns right on
        // the one row whose name is longest (#206).
        assert_eq!(columns[0]["width"], REPO_NAME_W);

        // The seven numeric widths sum to the figure
        // `PanelKind::GhWorkflows.min_width` is built on — widen a column
        // without moving that breakpoint and the panel silently outgrows the
        // width it claims to need.
        let numeric: f64 = columns
            .iter()
            .skip(1)
            .filter_map(|c| c["width"].as_f64())
            .sum();
        assert!(
            (numeric - 214.0).abs() < f64::EPSILON,
            "numeric columns sum to {numeric}"
        );
        // …and the whole fixed block, name included, still fits inside it.
        assert!(PanelKind::GhWorkflows.min_width() >= numeric + REPO_NAME_W);
    }

    /// Every numeric column fits its own header — the widest fixed text it
    /// holds — and no more than it needs. The upper bound is the half of this
    /// that keeps the row compact: a column padded well past its label is what
    /// spread the numbers apart and cost Repos its second column.
    #[test]
    fn every_numeric_column_fits_its_header_without_padding_it() {
        for (label, width) in COLUMNS.iter().skip(1) {
            let width = width.expect("numeric columns carry a width");
            let header = label.len() as f64 * MONO_9_CHAR_W;
            assert!(
                width >= header,
                "{label} is {header}pt of header in a {width}pt column"
            );
            assert!(
                width <= header + 12.0,
                "{label} reserves {width}pt for {header}pt of header"
            );
        }
    }

    /// The values have to fit too, and they are set at a larger size than the
    /// headers — a three-digit count and the longest elapsed label.
    #[test]
    fn every_numeric_column_fits_the_widest_value_it_can_show() {
        let widest = |chars: usize| chars as f64 * MONO_11_CHAR_W;
        for (label, width) in COLUMNS.iter().skip(1) {
            let width = width.expect("numeric columns carry a width");
            // "999" everywhere, except LONGEST which holds an elapsed label.
            let value = if *label == "LONGEST" {
                widest(4) // "125m"
            } else {
                widest(3)
            };
            assert!(
                width >= value,
                "{label} holds {value}pt of value in a {width}pt column"
            );
        }
    }

    /// The reservation is sized from the text it has to hold, like the runner
    /// status column — not picked to make a layout look right today.
    #[test]
    fn the_repo_name_column_fits_the_names_it_reserves_for() {
        let needed = REPO_NAME_CHARS as f64 * MONO_11_CHAR_W;
        assert!(
            REPO_NAME_W >= needed,
            "{REPO_NAME_W}pt reserved for {REPO_NAME_CHARS} chars needing {needed}pt"
        );
        // The names actually on the board today are far inside it, so nothing
        // ellipsizes in practice.
        for name in ["devcanopy", "tailoredtip", "sassydog-web", "what2wear"] {
            let width = name.len() as f64 * MONO_11_CHAR_W;
            assert!(width <= REPO_NAME_W, "{name} needs {width}pt");
        }
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

    /// See `the_repos_panel_says_loading_before_it_has_looked_for_a_token` —
    /// the same first frame, and the same line this used to get wrong.
    #[test]
    fn the_runners_panel_says_loading_before_it_has_looked_for_a_token() {
        let view = runners_view(&GitHubState::new(), now_unix());
        assert_eq!(view["message"]["text"], RUNNERS_LOADING_MESSAGE);
        assert_eq!(view["loading"], true);
        assert!(view["trailing"].is_null());
        assert!(view["footer"].is_null(), "nothing to be stale yet");
        assert!(rows(&view).is_empty());
        assert_eq!(view["title"], "GitHub Runners");
        assert_eq!(view["id"], "ghRunners");
    }

    #[test]
    fn the_runners_panel_asks_for_a_token_once_a_pass_finds_none() {
        let mut state = GitHubState::new();
        state.apply_unauthenticated();
        let view = runners_view(&state, now_unix());
        assert_eq!(view["message"]["text"], UNAUTHENTICATED_MESSAGE);
        assert_eq!(view["loading"], false, "we looked; this is not loading");
        assert!(view["trailing"].is_null());
        assert!(
            view["footer"].is_null(),
            "nothing to be stale without a token"
        );
        assert!(rows(&view).is_empty());
    }

    #[test]
    fn the_runners_panel_says_loading_between_the_token_and_the_first_fetch() {
        let mut state = GitHubState::new();
        state.apply_repos(Vec::new()); // authenticates without a runners fetch
        let view = runners_view(&state, now_unix());
        assert_eq!(view["message"]["text"], RUNNERS_LOADING_MESSAGE);
        assert_eq!(view["loading"], true);
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
        // No ONLINE stat — the trailing above already says 3/4. OFFLINE is the
        // count it does not give you: 4 registered, 3 online.
        assert_eq!(
            view["stats"],
            json!([
                { "label": "BUSY", "value": "1", "color": color::hex(color::AMBER) },
                { "label": "IDLE", "value": "2", "color": color::hex(color::GREEN) },
                { "label": "OFFLINE", "value": "1", "color": color::hex(color::AMBER) },
            ])
        );
        assert_eq!(
            view["chips"],
            json!(["macOS 2/2", "Linux 1/2", "Windows 0/0"])
        );
    }

    /// An org running on something we do not name still has to be accounted
    /// for: it counts toward the total, so it gets a chip rather than
    /// disappearing between the tracked three and the rollup.
    #[test]
    fn an_untracked_platform_appears_only_once_it_has_a_runner() {
        let state = with_runners(
            &[
                runner("mac-s1", RunnerOs::MacOs, RunnerState::Idle),
                runner("bsd-1", RunnerOs::Other, RunnerState::Offline),
            ],
            &[],
        );
        assert_eq!(
            runners_view(&state, now_unix())["chips"],
            json!(["macOS 1/1", "Linux 0/0", "Windows 0/0", "Other 0/1"])
        );
    }

    /// Zero busy and zero offline runners is a resting, healthy org — neither
    /// stat is a warning at rest.
    #[test]
    fn a_resting_org_dims_the_busy_and_offline_stats() {
        let state = with_runners(&[runner("mac-s1", RunnerOs::MacOs, RunnerState::Idle)], &[]);
        let view = runners_view(&state, now_unix());
        assert_eq!(view["stats"][0]["label"], "BUSY");
        assert_eq!(view["stats"][0]["color"], color::hex(color::MUTED));
        assert_eq!(view["stats"][2]["label"], "OFFLINE");
        assert_eq!(view["stats"][2]["value"], "0");
        assert_eq!(view["stats"][2]["color"], color::hex(color::MUTED));
        // Every tracked platform holds its slot, including the two this org has
        // nothing on — an empty slot is what someone standing a runner up is
        // watching.
        assert_eq!(
            view["chips"],
            json!(["macOS 1/1", "Linux 0/0", "Windows 0/0"])
        );
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

    /// #206: the status column is a *reservation*, not a measurement. A
    /// registered row and an absent one hand the frontend the same footprint,
    /// so the OS chip beside them cannot move when one runner starts recycling.
    #[test]
    fn every_runner_row_reserves_the_same_status_width() {
        let state = with_runners(
            &[
                runner("mac-s1", RunnerOs::MacOs, RunnerState::Busy),
                runner("ubu-1", RunnerOs::Linux, RunnerState::Idle),
                runner("ubu-2", RunnerOs::Linux, RunnerState::Offline),
            ],
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
        assert_eq!(rows.len(), 5, "idle, busy, offline, recycling and missing");
        for row in rows {
            assert_eq!(
                row["statusWidth"], RUNNER_STATUS_W,
                "{} sized its status column to its own text",
                row["name"]
            );
        }
    }

    /// And the reservation is big enough for every word that can land in it.
    /// The state words are a closed set; the presence labels are a ladder, so
    /// this walks the whole grace window a runner can recycle inside and years
    /// of the one it can go missing for.
    #[test]
    fn the_reserved_status_column_fits_every_word_the_panel_can_say() {
        let base = now();
        let mut widest = [RunnerState::Idle, RunnerState::Busy, RunnerState::Offline]
            .into_iter()
            .map(|state| state.label().chars().count())
            .max()
            .expect("three states");

        let absences = (0..=RUNNER_GRACE_SECS)
            .chain((1..=3_650).map(|days| days * 86_400))
            .chain([3_599, 3_600, 86_399, 86_400, 999 * 86_400]);
        for absence_secs in absences {
            let state = github::presence::state(
                false,
                base,
                base + TimeDelta::seconds(absence_secs),
                RUNNER_GRACE_SECS,
            );
            let label = github::presence::label(state).expect("an absence has a label");
            widest = widest.max(label.chars().count());
        }

        assert_eq!(
            widest, RUNNER_STATUS_CHARS,
            "the widest status the panel can say moved; re-derive the reservation"
        );
        assert!(
            RUNNER_STATUS_W >= RUNNER_STATUS_CHARS as f64 * MONO_9_CHAR_W,
            "{RUNNER_STATUS_W}pt cannot hold {RUNNER_STATUS_CHARS} characters of 9pt monospace"
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

    /// An unreadable credential store keeps every row the last good fetch left
    /// — this panel's failure policy for a bad *fetch*, applied one layer up to
    /// a read that never happened — and says why on top of them.
    #[test]
    fn an_unreadable_credential_store_keeps_the_runner_rows_and_names_the_cause() {
        let mut state = with_runners(&[runner("mac-s1", RunnerOs::MacOs, RunnerState::Idle)], &[]);
        let before = runners_view(&state, now_unix());

        state.apply_credential_unreadable(crate::CREDENTIAL_UNREADABLE_MESSAGE);
        let after = runners_view(&state, now_unix());

        assert_eq!(
            after["message"]["text"],
            crate::CREDENTIAL_UNREADABLE_MESSAGE
        );
        assert_eq!(
            after["rows"], before["rows"],
            "a locked keychain ages nothing"
        );
        assert_eq!(after["stats"], before["stats"]);
        assert_eq!(after["trailing"], before["trailing"]);
    }

    /// The worst moment for the old collapse: a launch into a locked keychain,
    /// where there is nothing retained to soften it. Neither panel has heard
    /// anything, so neither may assert there is no token — the only honest line
    /// is the one that names the store.
    #[test]
    fn an_unreadable_credential_store_at_launch_never_asks_for_a_token() {
        let mut state = GitHubState::new();
        state.apply_credential_unreadable(crate::CREDENTIAL_UNREADABLE_MESSAGE);

        let repos = repos_view(&state, now());
        let runners = runners_view(&state, now_unix());
        for view in [&repos, &runners] {
            assert_eq!(
                view["message"]["text"],
                crate::CREDENTIAL_UNREADABLE_MESSAGE
            );
            assert_ne!(view["message"]["text"], UNAUTHENTICATED_MESSAGE);
            assert!(rows(view).is_empty(), "nothing was ever fetched");
        }
        assert!(
            runners["footer"].is_null(),
            "no successful fetch to be stale"
        );
    }

    /// Nothing sticks: the reason is the last read's, never a previous one's.
    #[test]
    fn a_credential_the_store_hands_over_clears_the_reason() {
        let mut state = GitHubState::new();
        state.apply_credential_unreadable(crate::CREDENTIAL_UNREADABLE_MESSAGE);
        let update = roster::apply_fetch(
            &[],
            &[runner("mac-s1", RunnerOs::MacOs, RunnerState::Idle)],
            now(),
            github::presence::DEFAULT_GRACE_SECS,
        );
        state.apply_runners(&update, now_unix());
        assert!(runners_view(&state, now_unix())["message"].is_null());

        // …and the same for the branch that clears it by dropping back to the
        // zero-credential state.
        state.apply_credential_unreadable(crate::CREDENTIAL_UNREADABLE_MESSAGE);
        state.apply_unauthenticated();
        assert_eq!(
            runners_view(&state, now_unix())["message"]["text"],
            UNAUTHENTICATED_MESSAGE
        );
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
        assert_eq!(
            runners["chips"],
            json!(["macOS 2/2", "Linux 1/2", "Windows 0/0"]),
            "the Windows slot is empty on purpose — the fixture has to prove a \
             tracked platform still gets its chip at zero"
        );
        // `ubu-spare` is the offline runner, so the dumped fixture exercises a
        // non-zero OFFLINE stat. A fixture that only ever showed 0 could not
        // tell the amber rule from the muted one.
        assert_eq!(
            runners["stats"][2],
            json!({ "label": "OFFLINE", "value": "1", "color": color::hex(color::AMBER) })
        );
    }
}
