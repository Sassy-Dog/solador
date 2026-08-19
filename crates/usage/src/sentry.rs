//! Two reads for one Sentry organization: accepted error events over a rolling
//! window, and the cron monitors that are not ok. Rust port of
//! the original app, plus the cron-monitor read that has no
//! counterpart.
//!
//! Distinct from the app's own crash-reporting bootstrap: nothing here touches
//! a Sentry SDK. The token carries only the read-only `org:read` scope and
//! travels in an `Authorization: Bearer` header, never in a URL.
//!
//! **No wall clock**, as everywhere in this crate: the cron read takes `now` as
//! an argument, which is what lets a fixture pin an age of exactly seven days.

use chrono::{DateTime, Utc};
use std::num::NonZeroUsize;
use std::time::Duration;

use fault::Fault;
use percent_encoding::utf8_percent_encode;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

use crate::urlpath::PATH_SEGMENT;

/// The operator-facing name of what failed, and the only thing
/// [`Fault::message`] interpolates.
const VENDOR: &str = "Sentry";

pub const DEFAULT_BASE_URL: &str = "https://sentry.io";

// MARK: - Query vocabulary

/// The exact `stats_v2` query vocabulary this crate sends, and the one outcome
/// it reports. Sentry's outcome axis splits an org's ingest into `accepted`,
/// `filtered`, `rate_limited`, `invalid`, `dropped`… — only `accepted` counts
/// against quota, so that is the single figure the cockpit shows.
pub mod query {
    /// The aggregate field: event *quantity*, not times-seen.
    pub const FIELD: &str = "sum(quantity)";
    /// The data category. Errors only — transactions and attachments bill
    /// separately, and folding them together would produce a number that
    /// matches nothing in Sentry.
    pub const CATEGORY: &str = "error";
    /// The axis we split on, so `accepted` can be isolated.
    pub const GROUP_BY: &str = "outcome";
    /// The outcome whose total is the headline figure.
    pub const ACCEPTED_OUTCOME: &str = "accepted";
    /// Rolling window, in Sentry's own `statsPeriod` grammar.
    pub const STATS_PERIOD: &str = "30d";
    /// Days in [`STATS_PERIOD`], for the panel's label.
    pub const WINDOW_DAYS: u32 = 30;
}

// MARK: - Summary

/// Accepted error events for one Sentry organization over the rolling window.
///
/// # Why this is an enum
///
/// Sentry returns an outcome group only when that outcome had data in the
/// window, so a *missing* `accepted` group among other groups is a real zero —
/// the org ingested errors and none were accepted. But a response with no
/// groups at all measured nothing (wrong slug, brand-new org, no ingest), which
/// is genuinely unknown: the panel must render `—` there rather than a
/// fabricated 0.
///
/// The variants make that structural rather than merely documented:
/// [`SentryUsageSummary::Unmeasured`] holds no count to read, and
/// [`SentryUsageSummary::Measured`] cannot exist without at least one outcome
/// group (`NonZeroUsize`) having come back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SentryUsageSummary {
    /// The API answered, but carried no outcome groups. The count is unknown.
    Unmeasured,
    /// At least one outcome group came back, so the window was measured.
    Measured {
        /// Accepted error events over the window. Zero here is a real zero.
        accepted_error_events: u64,
        /// How many outcome groups the response carried.
        outcome_group_count: NonZeroUsize,
    },
}

impl SentryUsageSummary {
    /// Accepted error events, or `None` when nothing was measured.
    #[must_use]
    pub fn accepted_error_events(&self) -> Option<u64> {
        match self {
            SentryUsageSummary::Unmeasured => None,
            SentryUsageSummary::Measured {
                accepted_error_events,
                ..
            } => Some(*accepted_error_events),
        }
    }

    /// How many outcome groups the response carried. Zero exactly when nothing
    /// was measured.
    #[must_use]
    pub fn outcome_group_count(&self) -> usize {
        match self {
            SentryUsageSummary::Unmeasured => 0,
            SentryUsageSummary::Measured {
                outcome_group_count,
                ..
            } => outcome_group_count.get(),
        }
    }

    /// Whether the API answered successfully but measured nothing — the case
    /// the panel footer explains with [`NO_STATS_MESSAGE`].
    #[must_use]
    pub fn is_unmeasured(&self) -> bool {
        matches!(self, SentryUsageSummary::Unmeasured)
    }
}

/// Shown when the API answers successfully but reports no outcome groups at
/// all — the wrong org slug, or an org that has ingested nothing in the window.
pub const NO_STATS_MESSAGE: &str =
    "no Sentry event stats reported — check the org slug in Settings";

// MARK: - Wire format

/// `GET /api/0/organizations/{org}/stats_v2/` — the org event-count response.
/// Every nested container is optional because this is an external contract we
/// don't control; a response without groups must decode cleanly and fold to
/// "unknown".
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SentryStatsResponse {
    pub start: Option<String>,
    pub end: Option<String>,
    pub groups: Option<Vec<SentryStatsGroup>>,
}

/// One `groupBy` bucket. `by` carries the axis values (here
/// `{"outcome": "accepted"}`) and `totals` the aggregates keyed by the requested
/// field name.
///
/// Both maps are typed narrowly for the one query this crate sends: `outcome`
/// values are strings, and `sum(quantity)` is numeric. Decoding `totals` as
/// `f64` (rather than an integer) tolerates a JSON number written with a
/// fractional part; the fold rounds it back to a whole event count.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SentryStatsGroup {
    pub by: Option<HashMap<String, String>>,
    pub totals: Option<HashMap<String, f64>>,
}

// MARK: - Errors

/// Failures from reading Sentry org stats.
/// [`SentryUsageError::is_auth_failure`] is the signal the panel turns into a
/// "paste a new token" prompt — a revoked or under-scoped token answers with
/// 401 / 403.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SentryUsageError {
    #[error("Add your Sentry org slug in Settings")]
    MissingOrgSlug,
    #[error("Invalid Sentry API URL — check the org slug in Settings")]
    InvalidUrl,
    #[error("Invalid response from the Sentry API")]
    InvalidResponse,
    #[error("Sentry API request failed (HTTP {status})")]
    Http { status: u16 },
    /// The payload is the decoder's complaint, kept for the log and for
    /// `Debug`. It is **not** interpolated: `serde_json` quotes the input it
    /// choked on, so a body carrying a URL would put one in a panel footer by
    /// a route nobody would think to look at.
    #[error("couldn't read the Sentry response")]
    DecodingFailed(String),
    /// No counterpart: `URLSession` folds transport failures into a generic
    /// `Error` the service reports via `localizedDescription`.
    ///
    /// # Invariant: no request URL in the message
    ///
    /// `reqwest` attaches the request URL to its errors, and Sentry's carries
    /// the org slug and the whole percent-encoded stats query. Interpolating it
    /// is what filled this panel's footer with four lines of URL (#352). Every
    /// construction site strips it with [`reqwest::Error::without_url`] before
    /// the error becomes a string — [`unreachable()`] is the one-line helper, the
    /// twin of `crates/azurecost/src/blob.rs`'s — and the payload is not
    /// interpolated here either. Two guards, because one of them is a habit and
    /// the other is a type.
    #[error("couldn't reach Sentry")]
    Unreachable(String),
    /// The cron monitor list ran past [`MAX_MONITOR_PAGES`].
    ///
    /// A failure rather than a truncated answer on purpose: a partial list of
    /// monitors reads as "the ones that are missing are fine", which is the
    /// blind read this panel exists to refuse.
    #[error("Sentry listed more cron monitors than one read will page through")]
    MonitorListTruncated,
}

/// Strip the request URL — and with it the org slug and the query — before a
/// transport error becomes a string, then keep the stripped cause in the log.
///
/// The panel gets the sentence, the log keeps the detail. Same split, and the
/// same one-liner, as `crates/azurecost/src/blob.rs`.
fn unreachable(error: reqwest::Error) -> SentryUsageError {
    let detail = error.without_url().to_string();
    eprintln!("sentry: couldn't reach the API: {detail}");
    SentryUsageError::Unreachable(detail)
}

/// A decode failure, with the decoder's complaint logged rather than rendered.
fn decoding_failed(error: &serde_json::Error) -> SentryUsageError {
    eprintln!("sentry: couldn't read the response: {error}");
    SentryUsageError::DecodingFailed(error.to_string())
}

impl SentryUsageError {
    /// True when the failure looks like a bad, revoked, or under-scoped token.
    #[must_use]
    pub fn is_auth_failure(&self) -> bool {
        matches!(self, SentryUsageError::Http { status: 401 | 403 })
    }

    /// The message the panel footer shows. Cause-specific so the operator fixes
    /// the right thing: a 403 is a token to replace, a 404 is a slug to
    /// correct, and everything else names the failure it actually was.
    ///
    /// The two payload-carrying variants render the `fault` vocabulary's stock
    /// sentences. Until #354 they *retyped* them into their `#[error(…)]`
    /// attributes and reached them through `self.to_string()`, because the
    /// vocabulary lived in `crates/viewmodel` and no vendor crate may depend on
    /// the panel layer; the words are now a leaf crate this one can point at,
    /// so there is one copy again. Nothing a transport or a decoder produced is
    /// interpolated: it is in the log instead.
    ///
    /// The first two arms stay hand-written on purpose. Sentry's reads *are*
    /// account-scoped, so both are sharper than the stock sentence for the same
    /// state — they name the field to fix rather than the layer it lives in.
    ///
    /// One credential feeds two panels, so this wording lands in Usage **and**
    /// Sentry Crons.
    #[must_use]
    pub fn user_message(&self) -> String {
        if self.is_auth_failure() {
            return "token invalid — paste a new one in Settings".to_string();
        }
        match self {
            SentryUsageError::Http { status: 404 } => {
                "Sentry org not found — check the org slug in Settings".to_string()
            }
            SentryUsageError::Http { status } => fault::http_status_message(*status, VENDOR),
            SentryUsageError::Unreachable(_) => Fault::Unreachable.message(VENDOR),
            SentryUsageError::DecodingFailed(_) => Fault::Undecodable.message(VENDOR),
            SentryUsageError::MissingOrgSlug
            | SentryUsageError::InvalidUrl
            | SentryUsageError::InvalidResponse
            | SentryUsageError::MonitorListTruncated => self.to_string(),
        }
    }
}

// MARK: - Folding

/// Pick the `accepted` outcome's `sum(quantity)` total out of the grouped
/// response.
///
/// A response with no groups yields [`SentryUsageSummary::Unmeasured`] — the
/// `—` path, because nothing was measured. A response *with* groups but no
/// `accepted` bucket folds to a real 0: Sentry measured the window and accepted
/// nothing.
#[must_use]
pub fn summarize(response: &SentryStatsResponse) -> SentryUsageSummary {
    let groups = response.groups.as_deref().unwrap_or_default();
    let Some(outcome_group_count) = NonZeroUsize::new(groups.len()) else {
        return SentryUsageSummary::Unmeasured;
    };

    let accepted: f64 = groups
        .iter()
        .filter(|group| {
            group
                .by
                .as_ref()
                .and_then(|by| by.get(query::GROUP_BY))
                .is_some_and(|outcome| outcome == query::ACCEPTED_OUTCOME)
        })
        // A group that carries no `sum(quantity)` contributes nothing rather
        // than a value guessed out of the series.
        .filter_map(|group| group.totals.as_ref()?.get(query::FIELD).copied())
        .sum();

    SentryUsageSummary::Measured {
        // Negative or non-finite totals are not event counts; clamping to 0
        // keeps a nonsense payload from wrapping into an enormous figure.
        accepted_error_events: accepted.round().max(0.0) as u64,
        outcome_group_count,
    }
}

// MARK: - Cron monitors
//
// `GET /api/0/organizations/{org}/monitors/`, verified against the live API on
// 2026-08-04. A monitor object carries exactly `config`, `dateCreated`,
// `environments`, `id`, `isMuted`, `isUpserting`, `name`, `owner`, `project`,
// `slug`, `status`; an environment carries `activeIncident`, `dateCreated`,
// `isMuted`, `lastCheckIn`, `name`, `nextCheckIn`, `nextCheckInLatest`,
// `status`.
//
// Three traps, all of which bit the Slack half of this work first, and all three
// traceable to fixtures built from the Sentry **MCP server's** normalised output
// rather than from the raw REST payload:
//
// 1. **There is no `projectSlug`.** The slug is nested at `project.slug`. The
//    Slack digest rendered `undefined/prd` for a week.
// 2. **There is no `hasMoreEnvironments`**, and no environment cursor either —
//    the payload carries no environment-truncation signal at all, so there is
//    nothing to guard against and no guard here pretending otherwise.
// 3. **`activeIncident` is a key on every environment** and is `null` on the
//    healthy ones. Only a failing environment carries an object.

/// The status word that means "this environment is fine". Every other value —
/// and an absent one — is not ok.
pub const STATUS_OK: &str = "ok";

/// The status word the operator sets to stop caring about a monitor or one of
/// its environments.
pub const STATUS_DISABLED: &str = "disabled";

/// Rendered for an environment whose payload carried no `status` at all.
/// Unknown, and therefore not ok — never quietly promoted to healthy.
pub const STATUS_UNKNOWN: &str = "unknown";

/// The page size this crate asks for. The org this was built against has 33
/// monitors, so the whole list is normally one request.
const MONITORS_PER_PAGE: &str = "100";

/// How many monitor pages one read will walk.
///
/// A bound rather than an open loop: the cursor is opaque, so a server that kept
/// answering `results="true"` would spin forever. Ten pages is a thousand
/// monitors — thirty times the largest real list — and hitting it is reported as
/// [`SentryUsageError::MonitorListTruncated`] rather than answered with a
/// partial list.
const MAX_MONITOR_PAGES: usize = 10;

/// Shown when the API answers successfully and lists no monitors at all.
pub const NO_MONITORS_MESSAGE: &str =
    "no cron monitors reported — check the Sentry token's scope and the org slug in Settings";

/// One monitor, with only the fields this panel reads declared.
///
/// Every one of them is optional because this is an external contract we do not
/// control, and an absent key must decode cleanly rather than take the panel
/// down.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CronMonitor {
    pub slug: Option<String>,
    pub name: Option<String>,
    pub status: Option<String>,
    /// Deliberately a [`Value`] and not a `bool`: suppression is **strict**
    /// `true`, so a payload carrying `1` or `"true"` must stay red — and it must
    /// not fail to decode, which would take every other monitor with it.
    #[serde(default, rename = "isMuted")]
    pub is_muted: Option<Value>,
    /// The nested project. **There is no flat `projectSlug`** — see the module
    /// note above.
    pub project: Option<CronProject>,
    /// `None` and `Some([])` are the same fact: nothing could be read about this
    /// monitor. [`summarize_monitors`] reports it rather than counting the
    /// monitor as healthy.
    pub environments: Option<Vec<CronEnvironment>>,
}

impl CronMonitor {
    /// The monitor's own suppression, if it has one.
    #[must_use]
    pub fn suppression(&self) -> Option<CronSuppression> {
        if self.status.as_deref() == Some(STATUS_DISABLED) {
            return Some(CronSuppression::MonitorDisabled);
        }
        if strictly_true(self.is_muted.as_ref()) {
            return Some(CronSuppression::MonitorMuted);
        }
        None
    }

    /// The slug, or the name, or a placeholder — a row still has to be
    /// addressable when the payload names it neither way.
    #[must_use]
    pub fn key(&self) -> &str {
        non_empty(self.slug.as_deref())
            .or_else(|| non_empty(self.name.as_deref()))
            .unwrap_or("(unnamed monitor)")
    }
}

/// The project a monitor belongs to. One field, because one field is what the
/// panel reads — and because writing it flat would be the `projectSlug` bug.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CronProject {
    pub slug: Option<String>,
}

/// One environment of one monitor.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CronEnvironment {
    pub name: Option<String>,
    pub status: Option<String>,
    /// Strict `true`, for the same reason as [`CronMonitor::is_muted`]. Muting
    /// exists at **both** levels and either one suppresses.
    #[serde(default, rename = "isMuted")]
    pub is_muted: Option<Value>,
    /// RFC 3339, or `null` for an environment that has never checked in.
    #[serde(default, rename = "lastCheckIn")]
    pub last_check_in: Option<String>,
    /// `null` on a healthy environment. Only a failing one carries an object.
    #[serde(default, rename = "activeIncident")]
    pub active_incident: Option<CronIncident>,
}

impl CronEnvironment {
    /// Whether this environment is fine. Anything other than the literal `ok` —
    /// including an absent status — is not.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.status.as_deref() == Some(STATUS_OK)
    }

    /// The environment's own suppression, if it has one.
    #[must_use]
    pub fn suppression(&self) -> Option<CronSuppression> {
        if self.status.as_deref() == Some(STATUS_DISABLED) {
            return Some(CronSuppression::EnvironmentDisabled);
        }
        if strictly_true(self.is_muted.as_ref()) {
            return Some(CronSuppression::EnvironmentMuted);
        }
        None
    }
}

/// The open incident behind a failing environment.
///
/// `startingTimestamp` is second-precision `Z` (`2026-07-27T16:42:16Z`) — note
/// this differs from `dateCreated`, which is microsecond precision.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CronIncident {
    #[serde(default, rename = "startingTimestamp")]
    pub starting_timestamp: Option<String>,
}

/// Strict `true`, and nothing else. `None`, `false`, `1`, `"true"` are all "not
/// muted", which is what keeps a red monitor red.
fn strictly_true(raw: Option<&Value>) -> bool {
    raw == Some(&Value::Bool(true))
}

/// A trimmed non-empty string, or `None`. An empty slug is not a name.
fn non_empty(raw: Option<&str>) -> Option<&str> {
    let trimmed = raw?.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Why an environment that is not `ok` is nonetheless not red.
///
/// Suppressed entries are **counted and shown with their reason**, never
/// silently dropped: a monitor somebody muted six months ago and forgot is a
/// thing the operator needs to be able to see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CronSuppression {
    MonitorDisabled,
    MonitorMuted,
    EnvironmentDisabled,
    EnvironmentMuted,
}

impl CronSuppression {
    /// The reason, as the panel prints it.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            CronSuppression::MonitorDisabled => "monitor disabled",
            CronSuppression::MonitorMuted => "monitor muted",
            CronSuppression::EnvironmentDisabled => "environment disabled",
            CronSuppression::EnvironmentMuted => "environment muted",
        }
    }
}

/// How long an environment has been broken — and **what the figure is measured
/// from**, because the two available sources differ by days on exactly the
/// monitors that matter.
///
/// [`Incident`](Self::Incident) is the answer: the open incident's
/// `startingTimestamp` is when the monitor stopped being ok.
///
/// Measuring from `lastCheckIn` instead is not a rounding error, it reproduces
/// the original bug. A job that runs on schedule and keeps failing checks in
/// constantly, so it looks *freshly broken every day* — day 6 of an outage
/// rendering identically to day 1, which is the whole reason this panel exists.
/// Measured live on `cron-relay-drift-check`, the monitor that motivated the
/// work: `lastCheckIn` said **0d 22h** and the incident start said **7d 22h**.
///
/// So [`SinceLastCheckIn`](Self::SinceLastCheckIn) is available only when there
/// is no incident to read, and it is a *separate variant* rather than the same
/// number from a different place — the renderer has to mark it as the weaker
/// claim it is, and a plain `u64` would let it pass for the precise one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CronAge {
    /// `now - activeIncident.startingTimestamp`. The real thing.
    Incident { secs: u64 },
    /// `now - lastCheckIn`, and only because there was no active incident.
    SinceLastCheckIn { secs: u64 },
    /// No incident and no check-in at all. Never a computed duration.
    NeverCheckedIn,
    /// There was a timestamp and it could not be read. Broken, for how long we
    /// cannot say — and no substitute is invented from the other field.
    Unreadable,
}

impl CronAge {
    /// Seconds, when a duration was measured at all.
    #[must_use]
    pub fn secs(self) -> Option<u64> {
        match self {
            CronAge::Incident { secs } | CronAge::SinceLastCheckIn { secs } => Some(secs),
            CronAge::NeverCheckedIn | CronAge::Unreadable => None,
        }
    }

    /// Whether the figure came from the incident start — the only source that
    /// answers "how long has this been broken".
    #[must_use]
    pub fn is_precise(self) -> bool {
        matches!(self, CronAge::Incident { .. })
    }

    /// Sort rank: an environment that has never checked in comes first, then one
    /// whose age could not be read, then the aged ones (oldest first).
    ///
    /// The two rankless cases lead rather than trail: they are the entries
    /// nobody can put a number on, and burying them under a sorted list is how
    /// they stay unnoticed.
    fn rank(self) -> u8 {
        match self {
            CronAge::NeverCheckedIn => 0,
            CronAge::Unreadable => 1,
            CronAge::Incident { .. } | CronAge::SinceLastCheckIn { .. } => 2,
        }
    }
}

/// One environment that is not ok, already resolved into the facts a row needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronAlert {
    /// `project/monitor/environment` — stable, so a frontend can key a row by it
    /// and a test can address one.
    pub id: String,
    /// The monitor's slug (or its name, when it has no slug).
    pub monitor: String,
    /// The project slug, from the **nested** `project.slug`.
    pub project: Option<String>,
    pub environment: String,
    /// The wire's own status word, or [`STATUS_UNKNOWN`].
    pub status: String,
    pub age: CronAge,
    pub suppression: Option<CronSuppression>,
}

impl CronAlert {
    /// `platform/prd`, or just the environment when the project is unreadable —
    /// never `undefined/prd`.
    #[must_use]
    pub fn scope(&self) -> String {
        match &self.project {
            Some(project) => format!("{project}/{}", self.environment),
            None => self.environment.clone(),
        }
    }

    /// Whether this entry is one the operator asked not to be told about.
    #[must_use]
    pub fn is_suppressed(&self) -> bool {
        self.suppression.is_some()
    }
}

/// Every cron monitor in one org, folded to what the panel renders.
///
/// An enum for the same reason [`SentryUsageSummary`] is one: a response with no
/// monitors at all measured nothing. An org with no crons, a mistyped slug and a
/// token that cannot see monitors are indistinguishable there, so
/// [`NoMonitors`](Self::NoMonitors) carries no counts to mistake for a clean
/// bill of health — and [`Measured`](Self::Measured) cannot exist without at
/// least one monitor having come back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CronMonitorsSummary {
    /// The API answered and listed no monitors. A blind read, not "all clear".
    NoMonitors,
    /// At least one monitor came back.
    Measured {
        monitor_count: NonZeroUsize,
        /// Environments across every monitor — what "all clear" is a claim about.
        environment_count: usize,
        /// Every environment that is not `ok`, oldest first, suppressed ones
        /// included and carrying their reason.
        alerts: Vec<CronAlert>,
        /// Monitors that carried no environments at all, by key. Nothing could
        /// be read about them, so they are neither green nor red.
        unreadable: Vec<String>,
    },
}

impl CronMonitorsSummary {
    /// Monitors the read saw. Zero exactly when nothing was measured.
    #[must_use]
    pub fn monitor_count(&self) -> usize {
        match self {
            CronMonitorsSummary::NoMonitors => 0,
            CronMonitorsSummary::Measured { monitor_count, .. } => monitor_count.get(),
        }
    }

    /// Environments the read saw.
    #[must_use]
    pub fn environment_count(&self) -> usize {
        match self {
            CronMonitorsSummary::NoMonitors => 0,
            CronMonitorsSummary::Measured {
                environment_count, ..
            } => *environment_count,
        }
    }

    /// The rows, oldest first.
    #[must_use]
    pub fn alerts(&self) -> &[CronAlert] {
        match self {
            CronMonitorsSummary::NoMonitors => &[],
            CronMonitorsSummary::Measured { alerts, .. } => alerts,
        }
    }

    /// How many rows are live problems rather than suppressed ones.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.alerts().iter().filter(|a| !a.is_suppressed()).count()
    }

    /// How many rows the operator has already silenced.
    #[must_use]
    pub fn suppressed_count(&self) -> usize {
        self.alerts().iter().filter(|a| a.is_suppressed()).count()
    }

    /// Why this read cannot be trusted to mean "nothing is broken", if it
    /// cannot.
    ///
    /// Two cases, both of which would otherwise render as a green, empty panel:
    /// **no monitors at all**, and **a monitor whose environments could not be
    /// read** — an entry we can say nothing about, which is not the same as one
    /// we can say is fine.
    ///
    /// There is deliberately no third case for "suspiciously few monitors": the
    /// wire carries no expected count, and a threshold shipped in this crate
    /// would be a number nobody set, warning a fresh org about a list that is
    /// simply short. Same rule as the absent environment-truncation signal —
    /// a guard against a fact you do not have is false assurance.
    #[must_use]
    pub fn blind_read(&self) -> Option<String> {
        match self {
            CronMonitorsSummary::NoMonitors => Some(NO_MONITORS_MESSAGE.to_owned()),
            CronMonitorsSummary::Measured { unreadable, .. } if !unreadable.is_empty() => {
                Some(format!(
                    "{} monitor{} reported no environments — nothing could be read about {}",
                    unreadable.len(),
                    if unreadable.len() == 1 { "" } else { "s" },
                    unreadable.join(", ")
                ))
            }
            CronMonitorsSummary::Measured { .. } => None,
        }
    }
}

/// How long ago `raw` was, or `None` when it cannot be read as a timestamp.
///
/// `saturating` rather than signed: a `startingTimestamp` in the future (a clock
/// skew between Sentry and this machine) reads as "just now", never as a
/// negative age.
fn age_secs(raw: Option<&str>, now: DateTime<Utc>) -> Option<u64> {
    let parsed = DateTime::parse_from_rfc3339(non_empty(raw)?).ok()?;
    Some(
        now.signed_duration_since(parsed.with_timezone(&Utc))
            .num_seconds()
            .max(0) as u64,
    )
}

/// The age of one failing environment, and where the figure came from.
///
/// The whole point of the module, in four lines: the incident start is used
/// **whenever there is an active incident**, and `lastCheckIn` is reached only
/// when there is not.
#[must_use]
pub fn environment_age(environment: &CronEnvironment, now: DateTime<Utc>) -> CronAge {
    if let Some(incident) = environment.active_incident.as_ref() {
        return match age_secs(incident.starting_timestamp.as_deref(), now) {
            Some(secs) => CronAge::Incident { secs },
            // An incident with no readable start does NOT fall through to
            // `lastCheckIn`: that is the wrong number wearing the right label.
            None => CronAge::Unreadable,
        };
    }
    match environment.last_check_in.as_deref() {
        None => CronAge::NeverCheckedIn,
        Some(raw) => match age_secs(Some(raw), now) {
            Some(secs) => CronAge::SinceLastCheckIn { secs },
            None => CronAge::Unreadable,
        },
    }
}

/// Fold a monitor list into the panel's summary.
///
/// An environment is a row iff its status is not `ok`. Suppression does not
/// remove the row — it annotates it, so a muted monitor is visible as muted
/// rather than invisible.
///
/// Rows come back oldest first, with the two unrankable cases (never checked in,
/// unreadable age) ahead of them, and ties broken by [`CronAlert::id`] so the
/// order is stable across polls.
#[must_use]
pub fn summarize_monitors(monitors: &[CronMonitor], now: DateTime<Utc>) -> CronMonitorsSummary {
    let Some(monitor_count) = NonZeroUsize::new(monitors.len()) else {
        return CronMonitorsSummary::NoMonitors;
    };

    let mut environment_count = 0usize;
    let mut alerts: Vec<CronAlert> = Vec::new();
    let mut unreadable: Vec<String> = Vec::new();

    for monitor in monitors {
        let environments = monitor.environments.as_deref().unwrap_or_default();
        if environments.is_empty() {
            unreadable.push(monitor.key().to_owned());
            continue;
        }
        environment_count += environments.len();
        let project = non_empty(monitor.project.as_ref().and_then(|p| p.slug.as_deref()))
            .map(ToOwned::to_owned);
        for environment in environments {
            if environment.is_ok() {
                continue;
            }
            let name = non_empty(environment.name.as_deref()).unwrap_or("(unnamed)");
            alerts.push(CronAlert {
                id: format!(
                    "{}/{}/{name}",
                    project.as_deref().unwrap_or("-"),
                    monitor.key()
                ),
                monitor: monitor.key().to_owned(),
                project: project.clone(),
                environment: name.to_owned(),
                status: non_empty(environment.status.as_deref())
                    .unwrap_or(STATUS_UNKNOWN)
                    .to_owned(),
                age: environment_age(environment, now),
                // The monitor's own suppression outranks the environment's:
                // when both are set, the broader reason is the one to print.
                suppression: monitor.suppression().or_else(|| environment.suppression()),
            });
        }
    }

    alerts.sort_by(|a, b| {
        a.age
            .rank()
            .cmp(&b.age.rank())
            // Oldest first: the larger age leads.
            .then(b.age.secs().unwrap_or(0).cmp(&a.age.secs().unwrap_or(0)))
            .then_with(|| a.id.cmp(&b.id))
    });

    CronMonitorsSummary::Measured {
        monitor_count,
        environment_count,
        alerts,
        unreadable,
    }
}

/// The `cursor` of a `Link` header's `rel="next"` relation, when it claims a
/// further page actually holds results.
///
/// Sentry's pagination is cursor-based and it emits a `next` relation on **every**
/// page, including the last, where it carries `results="false"`. So the presence
/// of `next` is not a claim that there is more — only `results="true"` is — and
/// paging on the relation alone would loop forever.
fn next_cursor(link: &str) -> Option<&str> {
    link.split(',')
        .find(|part| part.contains("rel=\"next\"") && part.contains("results=\"true\""))
        .and_then(|part| part.split("cursor=\"").nth(1))
        .and_then(|rest| rest.split('"').next())
}

// MARK: - Client

/// The one Sentry REST read the Usage panel needs.
pub struct SentryClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl SentryClient {
    /// Builds a client, or `None` when the token is blank.
    ///
    /// "No token" is modelled as the absence of a client rather than as an
    /// error from one: the panel hides the Sentry section entirely when the
    /// user has not configured a token, which is not a failure to report.
    #[must_use]
    pub fn new(token: &str) -> Option<Self> {
        Self::with_base_url(DEFAULT_BASE_URL, token)
    }

    /// # Invariant: no credentials in `base_url`
    ///
    /// `base_url` must be scheme/host/port only. The token is a separate
    /// argument and travels only via `bearer_auth()`, which puts it in a
    /// header. URLs leak where headers do not: `reqwest` attaches the request
    /// URL to its errors and does not redact userinfo.
    ///
    /// Exists so tests (and only tests) can point the client at a mock server.
    #[must_use]
    pub fn with_base_url(base_url: &str, token: &str) -> Option<Self> {
        if token.trim().is_empty() {
            return None;
        }
        Some(SentryClient {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
        })
    }

    /// Error-category event counts for `org_slug` over `stats_period`, grouped
    /// by outcome.
    ///
    /// A blank slug short-circuits to [`SentryUsageError::MissingOrgSlug`]
    /// without a request: the slug is a path segment, so without it there is
    /// nothing to ask for.
    pub async fn error_outcomes(
        &self,
        org_slug: &str,
        stats_period: &str,
    ) -> Result<SentryStatsResponse, SentryUsageError> {
        let org_slug = org_slug.trim();
        if org_slug.is_empty() {
            return Err(SentryUsageError::MissingOrgSlug);
        }
        let slug = utf8_percent_encode(org_slug, PATH_SEGMENT);

        let resp = self
            .http
            .get(format!(
                "{}/api/0/organizations/{slug}/stats_v2/",
                self.base_url
            ))
            .query(&[
                ("field", query::FIELD),
                ("category", query::CATEGORY),
                ("groupBy", query::GROUP_BY),
                ("statsPeriod", stats_period),
            ])
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(unreachable)?;

        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(SentryUsageError::Http { status });
        }

        let body = resp.text().await.map_err(unreachable)?;
        serde_json::from_str(&body).map_err(|e| decoding_failed(&e))
    }

    /// The read the panel makes: accepted error events over the default
    /// rolling window, folded to a summary.
    pub async fn accepted_errors(
        &self,
        org_slug: &str,
    ) -> Result<SentryUsageSummary, SentryUsageError> {
        let response = self.error_outcomes(org_slug, query::STATS_PERIOD).await?;
        Ok(summarize(&response))
    }

    /// Every cron monitor in `org_slug`, across as many pages as it takes.
    ///
    /// `GET /api/0/organizations/{org}/monitors/`, **no project filter** — the
    /// scope is the whole org, because a cron that stopped running matters
    /// wherever it lives.
    ///
    /// A blank slug short-circuits to [`SentryUsageError::MissingOrgSlug`]
    /// without a request, exactly as [`Self::error_outcomes`] does: the slug is a
    /// path segment, so without it there is nothing to ask for.
    ///
    /// # Errors
    /// [`SentryUsageError::MonitorListTruncated`] when the list runs past
    /// [`MAX_MONITOR_PAGES`] — a loud failure in place of a partial list that
    /// would read as "the rest are fine".
    pub async fn cron_monitors(
        &self,
        org_slug: &str,
    ) -> Result<Vec<CronMonitor>, SentryUsageError> {
        let org_slug = org_slug.trim();
        if org_slug.is_empty() {
            return Err(SentryUsageError::MissingOrgSlug);
        }
        let slug = utf8_percent_encode(org_slug, PATH_SEGMENT);
        let url = format!("{}/api/0/organizations/{slug}/monitors/", self.base_url);

        let mut monitors: Vec<CronMonitor> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_MONITOR_PAGES {
            let mut request = self
                .http
                .get(&url)
                .query(&[("per_page", MONITORS_PER_PAGE)]);
            if let Some(cursor) = cursor.as_deref() {
                request = request.query(&[("cursor", cursor)]);
            }
            let resp = request
                .bearer_auth(&self.token)
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(unreachable)?;

            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                return Err(SentryUsageError::Http { status });
            }
            // Read before the body: `text()` consumes the response.
            let next = resp
                .headers()
                .get(reqwest::header::LINK)
                .and_then(|value| value.to_str().ok())
                .and_then(next_cursor)
                .map(ToOwned::to_owned);
            let body = resp.text().await.map_err(unreachable)?;
            let page: Vec<CronMonitor> =
                serde_json::from_str(&body).map_err(|e| decoding_failed(&e))?;
            monitors.extend(page);

            match next {
                None => return Ok(monitors),
                Some(next) => cursor = Some(next),
            }
        }
        Err(SentryUsageError::MonitorListTruncated)
    }

    /// The read the Sentry Crons panel makes: every org monitor, folded to the
    /// environments that are not ok.
    ///
    /// `now` is an argument for the same reason it is everywhere in this crate —
    /// the age this panel exists to render must be reproducible from a fixture.
    pub async fn cron_monitor_status(
        &self,
        org_slug: &str,
        now: DateTime<Utc>,
    ) -> Result<CronMonitorsSummary, SentryUsageError> {
        let monitors = self.cron_monitors(org_slug).await?;
        Ok(summarize_monitors(&monitors, now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // MARK: - Fixtures
    // Twins of `SentryUsageMappingTests`.

    const OUTCOMES: &str = r#"
    {
      "start": "2026-07-01T00:00:00Z",
      "end": "2026-07-31T00:00:00Z",
      "intervals": ["2026-07-01T00:00:00Z"],
      "groups": [
        {
          "by": { "outcome": "accepted" },
          "totals": { "sum(quantity)": 12345 },
          "series": { "sum(quantity)": [12345] }
        },
        {
          "by": { "outcome": "rate_limited" },
          "totals": { "sum(quantity)": 500 },
          "series": { "sum(quantity)": [500] }
        },
        {
          "by": { "outcome": "invalid" },
          "totals": { "sum(quantity)": 165665 },
          "series": { "sum(quantity)": [165665] }
        }
      ]
    }
    "#;

    fn fixture(json: &str) -> SentryStatsResponse {
        serde_json::from_str(json).expect("fixture decodes")
    }

    // MARK: - summarize

    /// Twin of `testSummarizePicksOnlyTheAcceptedOutcome`: rate_limited (500)
    /// and invalid (165665) must not land in the headline — they don't count
    /// against quota, and folding them in would inflate the figure.
    #[test]
    fn picks_only_the_accepted_outcome() {
        let summary = summarize(&fixture(OUTCOMES));
        assert_eq!(summary.accepted_error_events(), Some(12_345));
        assert_eq!(summary.outcome_group_count(), 3);
    }

    /// Twin of `testSummarizeTreatsAMissingAcceptedGroupAsARealZero`: Sentry
    /// returns an outcome group only when that outcome had data, so a response
    /// that measured *something* but accepted nothing is a real zero.
    #[test]
    fn a_missing_accepted_group_among_others_is_a_real_zero() {
        let json = r#"
        {
          "groups": [
            { "by": { "outcome": "rate_limited" }, "totals": { "sum(quantity)": 42 } }
          ]
        }
        "#;
        let summary = summarize(&fixture(json));
        assert_eq!(
            summary.accepted_error_events(),
            Some(0),
            "the window was measured and accepted nothing"
        );
        assert_eq!(summary.outcome_group_count(), 1);
    }

    /// Twin of `testSummarizeReportsUnknownWhenNoGroupsComeBack`: nothing was
    /// measured — the wrong slug or an org with no ingest. Genuinely unknown,
    /// so the figure must be `None`, never 0.
    #[test]
    fn no_groups_at_all_is_unmeasured_not_zero() {
        let summary = summarize(&fixture(r#"{ "groups": [] }"#));
        assert_eq!(summary, SentryUsageSummary::Unmeasured);
        assert_eq!(summary.accepted_error_events(), None);
        assert_eq!(summary.outcome_group_count(), 0);
    }

    /// Twin of `testDecodeToleratesAMissingGroupsKey`.
    #[test]
    fn a_missing_groups_key_decodes_and_folds_to_unmeasured() {
        let summary = summarize(&fixture("{}"));
        assert_eq!(summary.accepted_error_events(), None);
        assert_eq!(summary.outcome_group_count(), 0);
    }

    /// Twin of `testSummarizeToleratesAnAcceptedGroupWithoutTheRequestedField`:
    /// the group exists but carries no `sum(quantity)`, so it contributes
    /// nothing rather than a value guessed from the series.
    #[test]
    fn an_accepted_group_without_the_requested_field_contributes_nothing() {
        let json = r#"{ "groups": [ { "by": { "outcome": "accepted" }, "totals": {} } ] }"#;
        assert_eq!(
            summarize(&fixture(json)).accepted_error_events(),
            Some(0),
            "the window was still measured — one group came back"
        );
    }

    /// The distinction the whole enum exists for, asserted side by side: same
    /// headline intent, two different answers, and they must not be equal.
    #[test]
    fn unmeasured_and_a_real_zero_are_different_answers() {
        let unmeasured = summarize(&fixture(r#"{ "groups": [] }"#));
        let real_zero = summarize(&fixture(
            r#"{ "groups": [ { "by": { "outcome": "invalid" }, "totals": { "sum(quantity)": 9 } } ] }"#,
        ));

        assert_eq!(unmeasured.accepted_error_events(), None);
        assert_eq!(real_zero.accepted_error_events(), Some(0));
        assert_ne!(unmeasured, real_zero);
        assert!(unmeasured.is_unmeasured());
        assert!(!real_zero.is_unmeasured());
    }

    /// A fractional total is rounded back to a whole event count.
    #[test]
    fn a_fractional_total_rounds_to_a_whole_event_count() {
        let json = r#"{ "groups": [ { "by": { "outcome": "accepted" }, "totals": { "sum(quantity)": 12.6 } } ] }"#;
        assert_eq!(summarize(&fixture(json)).accepted_error_events(), Some(13));
    }

    /// Several `accepted` buckets (a shape the axis shouldn't produce, but the
    /// contract permits) sum rather than overwrite.
    #[test]
    fn repeated_accepted_groups_sum() {
        let json = r#"
        {
          "groups": [
            { "by": { "outcome": "accepted" }, "totals": { "sum(quantity)": 10 } },
            { "by": { "outcome": "accepted" }, "totals": { "sum(quantity)": 5 } }
          ]
        }
        "#;
        let summary = summarize(&fixture(json));
        assert_eq!(summary.accepted_error_events(), Some(15));
        assert_eq!(summary.outcome_group_count(), 2);
    }

    // MARK: - Query vocabulary

    /// Twin of `testQueryVocabularyMatchesTheDocumentedStatsV2Contract`.
    #[test]
    fn the_query_vocabulary_matches_the_documented_contract() {
        assert_eq!(query::FIELD, "sum(quantity)");
        assert_eq!(query::CATEGORY, "error");
        assert_eq!(query::GROUP_BY, "outcome");
        assert_eq!(query::ACCEPTED_OUTCOME, "accepted");
        assert_eq!(query::STATS_PERIOD, "30d");
        assert_eq!(query::WINDOW_DAYS, 30);
    }

    /// The footer text for a successful-but-empty read, carried over verbatim
    /// so the panel keeps saying why it shows `—` rather than a 0.
    #[test]
    fn the_no_stats_message_matches_the_original_verbatim() {
        assert_eq!(
            NO_STATS_MESSAGE,
            "no Sentry event stats reported — check the org slug in Settings"
        );
    }

    // MARK: - Errors

    /// Twin of `testAuthFailuresAreDetectedFor401And403`.
    #[test]
    fn auth_failures_are_401_and_403_only() {
        assert!(SentryUsageError::Http { status: 401 }.is_auth_failure());
        assert!(SentryUsageError::Http { status: 403 }.is_auth_failure());
        assert!(!SentryUsageError::Http { status: 500 }.is_auth_failure());
        assert!(!SentryUsageError::InvalidResponse.is_auth_failure());
        assert!(!SentryUsageError::MissingOrgSlug.is_auth_failure());
    }

    /// Twin of `testFriendlyMessageAsksForANewTokenOnAuthFailure`.
    #[test]
    fn an_auth_failure_asks_for_a_new_token() {
        assert_eq!(
            SentryUsageError::Http { status: 403 }.user_message(),
            "token invalid — paste a new one in Settings"
        );
        assert_eq!(
            SentryUsageError::Http { status: 401 }.user_message(),
            "token invalid — paste a new one in Settings"
        );
    }

    /// Twin of `testFriendlyMessagePointsAtTheOrgSlugOn404`.
    #[test]
    fn a_404_points_at_the_org_slug() {
        assert_eq!(
            SentryUsageError::Http { status: 404 }.user_message(),
            "Sentry org not found — check the org slug in Settings"
        );
    }

    /// Twin of `testATransientFailureKeepsTheLastGoodFigure`'s footer text: a
    /// transient failure names its own status so it reads differently from an
    /// auth problem. (Keeping the last-good summary is the shell's job — this
    /// crate holds no state.)
    #[test]
    fn other_failures_name_themselves() {
        assert_eq!(
            SentryUsageError::Http { status: 503 }.user_message(),
            "Sentry is failing on its side (HTTP 503)"
        );
        assert_eq!(
            SentryUsageError::MissingOrgSlug.user_message(),
            "Add your Sentry org slug in Settings"
        );
        // The decoder's complaint is deliberately absent — see the twin in
        // `neon.rs`: `serde_json` quotes the input it choked on.
        assert_eq!(
            SentryUsageError::DecodingFailed("bad token".into()).user_message(),
            Fault::Undecodable.message(VENDOR)
        );
        assert_eq!(
            SentryUsageError::InvalidUrl.user_message(),
            "Invalid Sentry API URL — check the org slug in Settings"
        );
    }

    // MARK: - Client

    fn json(body: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(body, "application/json")
    }

    fn client(base_url: &str) -> SentryClient {
        SentryClient::with_base_url(base_url, "sentry_token_value").expect("a non-blank token")
    }

    /// Twin of `testServiceRequestsTheRollingWindowForTheConfiguredOrg` — the
    /// matchers ARE the assertion: without them the mock never matches and the
    /// call comes back `Http { status: 404 }`.
    #[tokio::test]
    async fn requests_the_rolling_window_for_the_configured_org() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/0/organizations/acme/stats_v2/"))
            .and(header("authorization", "Bearer sentry_token_value"))
            .and(query_param("field", query::FIELD))
            .and(query_param("category", "error"))
            .and(query_param("groupBy", "outcome"))
            .and(query_param("statsPeriod", "30d"))
            .respond_with(json(OUTCOMES))
            .mount(&server)
            .await;

        let summary = client(&server.uri())
            .accepted_errors("acme")
            .await
            .expect("should decode");
        assert_eq!(summary.accepted_error_events(), Some(12_345));
    }

    /// Twin of `testAMissingOrgSlugIsReportedWithoutCallingTheAPI`.
    #[tokio::test]
    async fn a_blank_org_slug_is_reported_without_calling_the_api() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json(OUTCOMES))
            .mount(&server)
            .await;

        let err = client(&server.uri())
            .accepted_errors("   ")
            .await
            .unwrap_err();
        assert_eq!(err, SentryUsageError::MissingOrgSlug);
        assert_eq!(err.user_message(), "Add your Sentry org slug in Settings");
        assert!(
            server
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty(),
            "no slug means nothing to ask for"
        );
    }

    /// Twin of `testNoTokenLeavesTheServiceUnconfiguredSoThePanelHidesTheSection`:
    /// with no token there is no client to fail, so the panel has nothing to
    /// show rather than an error to render.
    #[test]
    fn a_blank_token_yields_no_client_at_all() {
        assert!(SentryClient::new("").is_none());
        assert!(SentryClient::new("   ").is_none());
        assert!(SentryClient::new("sentry_token_value").is_some());
    }

    /// The slug lands in a path segment, so a separator inside it must be
    /// encoded or the request retargets. `a/../..%2Fusers` would otherwise walk
    /// out of `/api/0/organizations/`.
    #[tokio::test]
    async fn a_slug_containing_a_separator_cannot_retarget_the_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/0/organizations/evil%2F..%2F..%2Fusers/stats_v2/",
            ))
            .respond_with(json(OUTCOMES))
            .mount(&server)
            .await;

        let summary = client(&server.uri())
            .accepted_errors("evil/../../users")
            .await
            .expect("the slug stays one segment");
        assert_eq!(summary.accepted_error_events(), Some(12_345));

        let requests = server.received_requests().await.expect("recorded requests");
        assert_eq!(
            requests.first().map(|r| r.url.path()),
            Some("/api/0/organizations/evil%2F..%2F..%2Fusers/stats_v2/"),
            "the separator must survive as an escape, not as a path boundary"
        );
    }

    /// Twin of `testASuccessfulButEmptyResponseKeepsTheValueUnknown`.
    #[tokio::test]
    async fn a_successful_but_empty_response_stays_unmeasured() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json(r#"{ "groups": [] }"#))
            .mount(&server)
            .await;

        let summary = client(&server.uri())
            .accepted_errors("acme")
            .await
            .expect("an org with no ingest is a successful answer");
        assert_eq!(summary, SentryUsageSummary::Unmeasured);
    }

    /// Twin of `testTransportFailureLeavesTheValueUnknownAndSurfacesTheError`:
    /// the failure is an `Err`, so there is no summary to mistake for a zero.
    #[tokio::test]
    async fn a_401_is_an_auth_failure_carrying_the_token_prompt() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = client(&server.uri())
            .accepted_errors("acme")
            .await
            .unwrap_err();
        assert!(err.is_auth_failure());
        assert_eq!(
            err.user_message(),
            "token invalid — paste a new one in Settings"
        );
    }

    #[tokio::test]
    async fn a_503_is_reported_with_its_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let err = client(&server.uri())
            .accepted_errors("acme")
            .await
            .unwrap_err();
        assert_eq!(err, SentryUsageError::Http { status: 503 });
        assert_eq!(
            err.user_message(),
            "Sentry is failing on its side (HTTP 503)"
        );
    }

    #[tokio::test]
    async fn malformed_json_is_a_decode_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json("{\"groups\": 7}"))
            .mount(&server)
            .await;

        let err = client(&server.uri())
            .accepted_errors("acme")
            .await
            .unwrap_err();
        assert!(
            matches!(err, SentryUsageError::DecodingFailed(_)),
            "got {err:?}"
        );
    }

    /// The #352 bug, proved against a *real* `reqwest` error — see the twin in
    /// `neon.rs`. Sentry's request URL carries the org slug and the whole
    /// percent-encoded stats query, and all four lines of it reached the
    /// footer.
    #[tokio::test]
    async fn an_unroutable_host_is_unreachable_without_saying_where() {
        let err = client("http://127.0.0.1:1")
            .accepted_errors("acme")
            .await
            .unwrap_err();
        assert!(
            matches!(err, SentryUsageError::Unreachable(_)),
            "got {err:?}"
        );
        assert_eq!(err.user_message(), "couldn't reach Sentry");
        assert_eq!(err.user_message(), Fault::Unreachable.message(VENDOR));
        for rendered in [err.user_message(), err.to_string()] {
            for forbidden in ["://", "127.0.0.1", "http", "?", "%", "acme"] {
                assert!(
                    !rendered.contains(forbidden),
                    "{rendered:?} carries {forbidden:?}"
                );
            }
        }
    }

    /// The token must never reach the URL, or it would ride along in
    /// `reqwest`'s error strings and any log line built from them.
    #[tokio::test]
    async fn the_token_travels_in_a_header_and_never_in_the_url() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json(OUTCOMES))
            .mount(&server)
            .await;

        client(&server.uri())
            .accepted_errors("acme")
            .await
            .expect("decodes");

        let requests = server.received_requests().await.expect("recorded requests");
        let request = requests.first().expect("one request");
        assert!(
            !request.url.as_str().contains("sentry_token_value"),
            "the token must not appear in the URL: {}",
            request.url
        );
        assert_eq!(
            request
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok()),
            Some("Bearer sentry_token_value")
        );
    }

    // MARK: - Cron monitors
    //
    // Every fixture below is shaped after the **raw REST** payload, captured from
    // the live API on 2026-08-04 — not after the Sentry MCP server's output. The
    // MCP layer normalises the response and synthesises both `projectSlug` and
    // `hasMoreEnvironments`; fixtures derived from it are internally consistent,
    // wrong about the wire, and pass every review. That is the root cause of two
    // of the three traps this module documents, so the shape here is load-bearing
    // and not incidental.

    /// `now` for every cron fixture: 2026-08-04T15:00:00Z.
    fn cron_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-04T15:00:00Z")
            .expect("a fixed now")
            .with_timezone(&Utc)
    }

    /// The monitor that motivated this work, as the REST API returns it.
    ///
    /// **`lastCheckIn` and `activeIncident.startingTimestamp` differ by a week
    /// here, on purpose.** The Slack half's original regression test used a
    /// fixture where the two happened to be equal, so it passed under the *wrong*
    /// formula and proved nothing. A fixture that cannot distinguish two
    /// implementations is not a regression test.
    ///
    /// - incident start `2026-07-27T16:42:16Z` -> **7d 22h** from `cron_now`
    /// - last check-in `2026-08-03T16:42:16Z` -> **0d 22h** from `cron_now`
    const DRIFT_CHECK: &str = r#"
    [
      {
        "id": "2001",
        "slug": "cron-relay-drift-check",
        "name": "Relay drift check",
        "status": "active",
        "isMuted": false,
        "isUpserting": false,
        "owner": null,
        "dateCreated": "2026-05-01T09:12:44.123456Z",
        "config": { "schedule": "0 4 * * *", "schedule_type": "crontab" },
        "project": { "id": "11", "slug": "platform", "name": "Platform" },
        "environments": [
          {
            "name": "prd",
            "status": "error",
            "isMuted": false,
            "dateCreated": "2026-05-01T09:12:44.123456Z",
            "lastCheckIn": "2026-08-03T16:42:16Z",
            "nextCheckIn": "2026-08-04T16:42:16Z",
            "nextCheckInLatest": "2026-08-04T16:47:16Z",
            "activeIncident": {
              "startingTimestamp": "2026-07-27T16:42:16Z",
              "resolvingTimestamp": null
            }
          },
          {
            "name": "dev",
            "status": "ok",
            "isMuted": false,
            "dateCreated": "2026-05-01T09:12:44.123456Z",
            "lastCheckIn": "2026-08-04T14:42:16Z",
            "nextCheckIn": "2026-08-05T14:42:16Z",
            "nextCheckInLatest": "2026-08-05T14:47:16Z",
            "activeIncident": null
          }
        ]
      }
    ]
    "#;

    fn monitors(json: &str) -> Vec<CronMonitor> {
        serde_json::from_str(json).expect("fixture decodes")
    }

    fn summarized(json: &str) -> CronMonitorsSummary {
        summarize_monitors(&monitors(json), cron_now())
    }

    /// `2026-08-03T16:42:16Z` -> `2026-08-04T15:00:00Z`, the check-in age.
    const TWENTY_TWO_HOURS: u64 = 22 * 3600 + 17 * 60 + 44;
    /// `2026-07-27T16:42:16Z` -> `2026-08-04T15:00:00Z`, the incident age. A
    /// whole week more than the figure above, which is the entire point.
    const SEVEN_DAYS_22H: u64 = 7 * 86_400 + TWENTY_TWO_HOURS;

    /// **The regression this panel exists for.** Age is measured from the open
    /// incident's start, never from the last check-in — and the fixture's two
    /// timestamps are a week apart, so an implementation using the wrong one
    /// cannot pass this.
    #[test]
    fn age_comes_from_the_incident_start_and_not_from_the_last_check_in() {
        let summary = summarized(DRIFT_CHECK);
        let alert = summary
            .alerts()
            .iter()
            .find(|a| a.environment == "prd")
            .expect("the failing environment");

        assert_eq!(
            alert.age,
            CronAge::Incident {
                secs: SEVEN_DAYS_22H
            },
            "7d 22h, from activeIncident.startingTimestamp"
        );
        assert!(alert.age.is_precise());

        // …and the number the *wrong* formula would have produced, asserted
        // explicitly so the two can never be confused for each other again.
        let from_check_in = age_secs(Some("2026-08-03T16:42:16Z"), cron_now()).expect("parses");
        assert_eq!(from_check_in, TWENTY_TWO_HOURS, "0d 22h");
        assert_ne!(
            alert.age.secs(),
            Some(from_check_in),
            "the two sources must not agree, or this test proves nothing"
        );
        // Days apart, on the very monitor this work came from — measured off the
        // alert rather than off the two constants, which the compiler could fold.
        let gap = alert.age.secs().expect("an age") - from_check_in;
        assert!(gap > 6 * 86_400, "{gap}s apart, expected over six days");
    }

    /// The nested read. **There is no `projectSlug`** on the wire, and inventing
    /// one is what rendered `undefined/prd` in the Slack half for a week.
    #[test]
    fn the_project_slug_is_read_from_the_nested_object() {
        let summary = summarized(DRIFT_CHECK);
        let alert = &summary.alerts()[0];
        assert_eq!(alert.project.as_deref(), Some("platform"));
        assert_eq!(alert.scope(), "platform/prd");
        assert_eq!(alert.id, "platform/cron-relay-drift-check/prd");
        // A flat `projectSlug` is not part of the contract, so a payload that
        // carries one anyway must change nothing.
        let with_flat = DRIFT_CHECK.replace(
            "\"slug\": \"cron-relay-drift-check\",",
            "\"slug\": \"cron-relay-drift-check\", \"projectSlug\": \"WRONG\",",
        );
        assert_eq!(summarized(&with_flat).alerts()[0].scope(), "platform/prd");
    }

    /// A healthy environment is not a row, and `activeIncident` being present as
    /// a `null` key on it must decode cleanly.
    #[test]
    fn only_the_environments_that_are_not_ok_become_rows() {
        let summary = summarized(DRIFT_CHECK);
        assert_eq!(summary.monitor_count(), 1);
        assert_eq!(summary.environment_count(), 2);
        assert_eq!(summary.alerts().len(), 1);
        assert_eq!(summary.active_count(), 1);
        assert_eq!(summary.suppressed_count(), 0);
        assert_eq!(summary.blind_read(), None);
    }

    /// No incident to read, so the age falls back to the last check-in — as a
    /// **different variant**, because the renderer has to mark it as the weaker
    /// claim it is.
    #[test]
    fn the_check_in_fallback_is_used_only_when_there_is_no_incident() {
        let json = r#"
        [
          {
            "slug": "nightly-rollup",
            "status": "active",
            "isMuted": false,
            "project": { "slug": "gadget-jobs" },
            "environments": [
              {
                "name": "prd",
                "status": "missed_checkin",
                "isMuted": false,
                "lastCheckIn": "2026-08-03T16:42:16Z",
                "activeIncident": null
              }
            ]
          }
        ]
        "#;
        let summary = summarized(json);
        let alert = &summary.alerts()[0];
        assert_eq!(
            alert.age,
            CronAge::SinceLastCheckIn {
                secs: TWENTY_TWO_HOURS
            }
        );
        assert!(
            !alert.age.is_precise(),
            "a check-in age must never pass for an incident age"
        );
        assert_eq!(alert.status, "missed_checkin");
    }

    /// `lastCheckIn: null` with no incident is **never checked in** — never a
    /// computed duration, and never a zero.
    #[test]
    fn a_null_last_check_in_with_no_incident_is_never_checked_in() {
        let json = r#"
        [
          {
            "slug": "brand-new-cron",
            "status": "active",
            "project": { "slug": "platform" },
            "environments": [
              { "name": "prd", "status": "active", "lastCheckIn": null, "activeIncident": null }
            ]
          }
        ]
        "#;
        let summary = summarized(json);
        assert_eq!(summary.alerts()[0].age, CronAge::NeverCheckedIn);
        assert_eq!(
            summary.alerts()[0].age.secs(),
            None,
            "no duration to render"
        );
    }

    /// An incident whose start cannot be read does **not** fall through to
    /// `lastCheckIn`: that would be the wrong number wearing the right label.
    #[test]
    fn an_unreadable_incident_start_does_not_borrow_the_check_in() {
        let json = r#"
        [
          {
            "slug": "odd-timestamps",
            "project": { "slug": "platform" },
            "environments": [
              {
                "name": "prd",
                "status": "error",
                "lastCheckIn": "2026-08-03T16:42:16Z",
                "activeIncident": { "startingTimestamp": "not a timestamp" }
              }
            ]
          }
        ]
        "#;
        assert_eq!(summarized(json).alerts()[0].age, CronAge::Unreadable);
    }

    /// A future timestamp (clock skew) reads as "just now", never as a negative
    /// age that would wrap into an enormous one.
    #[test]
    fn a_future_incident_start_reads_as_just_now() {
        assert_eq!(age_secs(Some("2099-01-01T00:00:00Z"), cron_now()), Some(0));
    }

    /// One failing environment, with whatever monitor-level and environment-level
    /// extras a suppression case needs. The environment's status is a parameter
    /// rather than an extra so no case can produce a duplicate JSON key.
    fn suppression_reason(
        monitor_extra: &str,
        env_status: &str,
        env_extra: &str,
    ) -> Option<&'static str> {
        let json = format!(
            r#"[ {{ "slug": "s", "project": {{ "slug": "p" }}{monitor_extra},
                    "environments": [ {{ "name": "prd", "status": "{env_status}",
                      "lastCheckIn": null,
                      "activeIncident": {{ "startingTimestamp": "2026-08-01T00:00:00Z" }}
                      {env_extra} }} ] }} ]"#
        );
        let summary = summarized(&json);
        let alert = summary.alerts().first().expect("one row");
        alert.suppression.map(CronSuppression::reason)
    }

    /// Suppression at either level, and the strictness that keeps a red monitor
    /// red. `isMuted` exists on **both** the monitor and the environment.
    #[test]
    fn suppression_is_disabled_or_a_strictly_true_mute_at_either_level() {
        assert_eq!(
            suppression_reason("", "error", ""),
            None,
            "a plain error stays red"
        );
        assert_eq!(
            suppression_reason(r#", "status": "disabled""#, "error", ""),
            Some("monitor disabled")
        );
        assert_eq!(
            suppression_reason(r#", "isMuted": true"#, "error", ""),
            Some("monitor muted")
        );
        assert_eq!(
            suppression_reason("", "disabled", ""),
            Some("environment disabled")
        );
        assert_eq!(
            suppression_reason("", "error", r#", "isMuted": true"#),
            Some("environment muted")
        );

        // Strict `true` only: missing, false, and truthy-non-true all stay red —
        // and none of them may break decoding, which would take the whole panel
        // down over one odd field.
        for muted in [
            r#", "isMuted": false"#,
            r#", "isMuted": 1"#,
            r#", "isMuted": "true""#,
            r#", "isMuted": null"#,
        ] {
            assert_eq!(
                suppression_reason(muted, "error", ""),
                None,
                "monitor {muted}"
            );
            assert_eq!(
                suppression_reason("", "error", muted),
                None,
                "environment {muted}"
            );
        }
    }

    /// A suppressed entry is counted and shown with its reason, never silently
    /// dropped — a monitor somebody muted and forgot has to stay visible.
    #[test]
    fn suppressed_entries_are_still_rows() {
        let json = r#"
        [
          {
            "slug": "muted-cron",
            "isMuted": true,
            "project": { "slug": "platform" },
            "environments": [
              { "name": "prd", "status": "error", "lastCheckIn": null,
                "activeIncident": { "startingTimestamp": "2026-08-01T00:00:00Z" } }
            ]
          }
        ]
        "#;
        let summary = summarized(json);
        assert_eq!(summary.alerts().len(), 1);
        assert!(summary.alerts()[0].is_suppressed());
        assert_eq!(summary.active_count(), 0);
        assert_eq!(summary.suppressed_count(), 1);
        assert_eq!(
            summary.blind_read(),
            None,
            "a muted monitor is a choice, not a blind read"
        );
    }

    /// The monitor's own suppression outranks the environment's: with both set,
    /// the broader reason is the one printed.
    #[test]
    fn the_monitors_reason_outranks_the_environments() {
        let json = r#"
        [
          {
            "slug": "s", "isMuted": true, "project": { "slug": "p" },
            "environments": [
              { "name": "prd", "status": "disabled", "isMuted": true, "lastCheckIn": null,
                "activeIncident": null }
            ]
          }
        ]
        "#;
        assert_eq!(
            summarized(json).alerts()[0].suppression,
            Some(CronSuppression::MonitorMuted)
        );
    }

    /// An environment with no `status` at all is unknown, and unknown is not ok.
    #[test]
    fn an_environment_with_no_status_is_not_quietly_healthy() {
        let json = r#"
        [
          { "slug": "s", "project": { "slug": "p" },
            "environments": [ { "name": "prd", "lastCheckIn": null } ] }
        ]
        "#;
        let summary = summarized(json);
        assert_eq!(summary.alerts().len(), 1);
        assert_eq!(summary.alerts()[0].status, STATUS_UNKNOWN);
    }

    /// Oldest first, with the two unrankable cases leading — never-checked-in
    /// first of all, then an unreadable age, then the durations in descending
    /// order.
    #[test]
    fn rows_sort_oldest_first_with_never_checked_in_leading() {
        let json = r#"
        [
          { "slug": "recent", "project": { "slug": "p" }, "environments": [
              { "name": "prd", "status": "error", "lastCheckIn": null,
                "activeIncident": { "startingTimestamp": "2026-08-04T14:00:00Z" } } ] },
          { "slug": "ancient", "project": { "slug": "p" }, "environments": [
              { "name": "prd", "status": "error", "lastCheckIn": null,
                "activeIncident": { "startingTimestamp": "2026-06-01T00:00:00Z" } } ] },
          { "slug": "never", "project": { "slug": "p" }, "environments": [
              { "name": "prd", "status": "active", "lastCheckIn": null, "activeIncident": null } ] },
          { "slug": "unreadable", "project": { "slug": "p" }, "environments": [
              { "name": "prd", "status": "error", "lastCheckIn": null,
                "activeIncident": { "startingTimestamp": "" } } ] }
        ]
        "#;
        let summary = summarized(json);
        assert_eq!(
            summary
                .alerts()
                .iter()
                .map(|a| a.monitor.as_str())
                .collect::<Vec<_>>(),
            vec!["never", "unreadable", "ancient", "recent"]
        );
    }

    /// Ties break on the row id, so two equally-old monitors do not swap places
    /// between polls.
    #[test]
    fn equal_ages_keep_a_stable_order() {
        let json = r#"
        [
          { "slug": "zulu", "project": { "slug": "p" }, "environments": [
              { "name": "prd", "status": "error", "lastCheckIn": null,
                "activeIncident": { "startingTimestamp": "2026-08-01T00:00:00Z" } } ] },
          { "slug": "alpha", "project": { "slug": "p" }, "environments": [
              { "name": "prd", "status": "error", "lastCheckIn": null,
                "activeIncident": { "startingTimestamp": "2026-08-01T00:00:00Z" } } ] }
        ]
        "#;
        assert_eq!(
            summarized(json)
                .alerts()
                .iter()
                .map(|a| a.monitor.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "zulu"]
        );
    }

    /// The headline blind read: an empty list is **not** "all clear". An org with
    /// no crons, a mistyped slug and an under-scoped token are indistinguishable
    /// here, and the variant carries no counts to mistake for health.
    #[test]
    fn an_empty_monitor_list_is_a_blind_read_not_all_clear() {
        let summary = summarized("[]");
        assert_eq!(summary, CronMonitorsSummary::NoMonitors);
        assert_eq!(summary.monitor_count(), 0);
        assert!(summary.alerts().is_empty());
        assert_eq!(summary.blind_read().as_deref(), Some(NO_MONITORS_MESSAGE));
    }

    /// A monitor with no environments is a monitor we can say nothing about —
    /// neither green nor red — and it must surface rather than pass as healthy.
    #[test]
    fn a_monitor_with_no_environments_is_a_blind_read() {
        for json in [
            r#"[ { "slug": "hollow", "project": { "slug": "p" }, "environments": [] } ]"#,
            r#"[ { "slug": "hollow", "project": { "slug": "p" } } ]"#,
        ] {
            let summary = summarized(json);
            assert_eq!(summary.monitor_count(), 1, "for {json}");
            assert_eq!(summary.environment_count(), 0);
            assert!(summary.alerts().is_empty());
            let blind = summary.blind_read().expect("a blind read");
            assert!(blind.contains("hollow"), "{blind}");
            assert!(
                blind.contains("1 monitor reported no environments"),
                "{blind}"
            );
        }
    }

    /// The all-ok case, which is the only one entitled to say so: monitors came
    /// back, environments came back, and none of them is a row.
    #[test]
    fn a_healthy_org_is_measured_and_says_nothing_is_wrong() {
        let json = r#"
        [
          { "slug": "a", "project": { "slug": "p" }, "environments": [
              { "name": "prd", "status": "ok", "lastCheckIn": "2026-08-04T14:00:00Z",
                "activeIncident": null } ] },
          { "slug": "b", "project": { "slug": "p" }, "environments": [
              { "name": "prd", "status": "ok", "lastCheckIn": "2026-08-04T14:00:00Z",
                "activeIncident": null } ] }
        ]
        "#;
        let summary = summarized(json);
        assert_eq!(summary.monitor_count(), 2);
        assert_eq!(summary.environment_count(), 2);
        assert!(summary.alerts().is_empty());
        assert_eq!(summary.blind_read(), None);
    }

    /// A monitor with neither slug nor name is still addressable rather than
    /// dropped.
    #[test]
    fn a_monitor_with_no_slug_still_gets_a_row() {
        let json = r#"[ { "environments": [ { "status": "error", "lastCheckIn": null } ] } ]"#;
        let summary = summarized(json);
        assert_eq!(summary.alerts()[0].monitor, "(unnamed monitor)");
        assert_eq!(
            summary.alerts()[0].scope(),
            "(unnamed)",
            "an unreadable project must never render as `undefined/x`"
        );
    }

    // MARK: - Cron monitor pagination

    /// Only `results="true"` is a claim that another page exists. Sentry emits a
    /// `next` relation on the last page too, so paging on the relation alone
    /// would loop forever.
    #[test]
    fn only_a_results_true_next_relation_advertises_another_page() {
        let more = "<https://sentry.io/api/0/organizations/x/monitors/?&cursor=0%3A0%3A1>; \
                    rel=\"previous\"; results=\"false\"; cursor=\"0:0:1\", \
                    <https://sentry.io/api/0/organizations/x/monitors/?&cursor=0%3A100%3A0>; \
                    rel=\"next\"; results=\"true\"; cursor=\"0:100:0\"";
        assert_eq!(next_cursor(more), Some("0:100:0"));

        let last = "<https://sentry.io/api/0/organizations/x/monitors/?&cursor=0%3A0%3A1>; \
                    rel=\"previous\"; results=\"true\"; cursor=\"0:0:1\", \
                    <https://sentry.io/api/0/organizations/x/monitors/?&cursor=0%3A200%3A0>; \
                    rel=\"next\"; results=\"false\"; cursor=\"0:200:0\"";
        assert_eq!(
            next_cursor(last),
            None,
            "the last page still carries `next`"
        );
        assert_eq!(next_cursor(""), None);
    }

    // MARK: - Cron monitor client

    /// The matchers ARE the assertion: the path, the bearer token, and **no
    /// project filter** — the scope is every monitor in the org.
    #[tokio::test]
    async fn cron_monitors_reads_every_monitor_in_the_org() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/0/organizations/acme/monitors/"))
            .and(header("authorization", "Bearer sentry_token_value"))
            .and(query_param("per_page", "100"))
            .respond_with(json(DRIFT_CHECK))
            .mount(&server)
            .await;

        let summary = client(&server.uri())
            .cron_monitor_status("acme", cron_now())
            .await
            .expect("should decode");
        assert_eq!(summary.monitor_count(), 1);
        assert_eq!(
            summary.alerts()[0].age,
            CronAge::Incident {
                secs: SEVEN_DAYS_22H
            }
        );

        let requests = server.received_requests().await.expect("recorded requests");
        let query = requests.first().expect("one request").url.query();
        assert!(
            !query.unwrap_or_default().contains("project"),
            "no project filter: {query:?}"
        );
    }

    /// A live-shaped payload with **everything ok**, end to end through the
    /// client: no rows, no blind read, and the environment count that makes "all
    /// clear" a claim about something.
    ///
    /// Worth a round trip of its own rather than only a fold: the healthy payload
    /// is the one whose `activeIncident: null` keys and full field set must decode
    /// cleanly, and a decode failure here would surface as a red panel on a
    /// perfectly healthy org.
    #[tokio::test]
    async fn a_live_shaped_healthy_payload_reports_nothing_broken() {
        let healthy = DRIFT_CHECK
            .replace("\"status\": \"error\"", "\"status\": \"ok\"")
            .replace(
                r#""activeIncident": {
              "startingTimestamp": "2026-07-27T16:42:16Z",
              "resolvingTimestamp": null
            }"#,
                r#""activeIncident": null"#,
            );
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/0/organizations/acme/monitors/"))
            .respond_with(json(&healthy))
            .mount(&server)
            .await;

        let summary = client(&server.uri())
            .cron_monitor_status("acme", cron_now())
            .await
            .expect("a healthy payload decodes");
        assert_eq!(summary.monitor_count(), 1);
        assert_eq!(summary.environment_count(), 2);
        assert!(summary.alerts().is_empty(), "{:?}", summary.alerts());
        assert_eq!(summary.blind_read(), None);
    }

    /// A blank slug is reported without a request — the slug is a path segment,
    /// so without it there is nothing to ask for.
    #[tokio::test]
    async fn a_blank_slug_asks_for_no_monitors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json(DRIFT_CHECK))
            .mount(&server)
            .await;

        let err = client(&server.uri())
            .cron_monitor_status("  ", cron_now())
            .await
            .unwrap_err();
        assert_eq!(err, SentryUsageError::MissingOrgSlug);
        assert!(server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty());
    }

    /// An under-scoped token answers 403, which is a failure carrying the "paste
    /// a new one" prompt — never an empty, green panel.
    #[tokio::test]
    async fn an_auth_failure_on_the_monitor_read_is_reported_as_one() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let err = client(&server.uri())
            .cron_monitor_status("acme", cron_now())
            .await
            .unwrap_err();
        assert!(err.is_auth_failure());
        assert_eq!(
            err.user_message(),
            "token invalid — paste a new one in Settings"
        );
    }

    #[tokio::test]
    async fn a_malformed_monitor_payload_is_a_decode_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json(r#"{ "monitors": [] }"#))
            .mount(&server)
            .await;

        let err = client(&server.uri())
            .cron_monitor_status("acme", cron_now())
            .await
            .unwrap_err();
        assert!(
            matches!(err, SentryUsageError::DecodingFailed(_)),
            "got {err:?}"
        );
    }

    /// A second page is followed, so a long list is not silently undercounted.
    #[tokio::test]
    async fn a_paged_monitor_list_is_walked_to_the_end() {
        let server = MockServer::start().await;
        let more = format!(
            "<{0}/api/0/organizations/acme/monitors/?&cursor=0%3A100%3A0>; \
             rel=\"next\"; results=\"true\"; cursor=\"0:100:0\"",
            server.uri()
        );
        Mock::given(method("GET"))
            .and(query_param("cursor", "0:100:0"))
            .respond_with(json(
                r#"[ { "slug": "second-page", "project": { "slug": "p" },
                       "environments": [ { "name": "prd", "status": "ok" } ] } ]"#,
            ))
            .mount(&server)
            .await;
        // Mutually exclusive with the mock above rather than relying on match
        // precedence between two mocks that both accept the request.
        Mock::given(method("GET"))
            .and(query_param_is_missing("cursor"))
            .respond_with(json(DRIFT_CHECK).insert_header("link", more.as_str()))
            .mount(&server)
            .await;

        let summary = client(&server.uri())
            .cron_monitor_status("acme", cron_now())
            .await
            .expect("both pages decode");
        assert_eq!(summary.monitor_count(), 2, "both pages counted");
        assert_eq!(summary.alerts().len(), 1);
    }

    /// A list that never ends is a **failure**, not a truncated answer: a partial
    /// monitor list reads as "the ones that are missing are fine".
    #[tokio::test]
    async fn an_endless_monitor_list_fails_rather_than_answering_partially() {
        let server = MockServer::start().await;
        let more = format!(
            "<{0}/api/0/organizations/acme/monitors/>; \
             rel=\"next\"; results=\"true\"; cursor=\"0:100:0\"",
            server.uri()
        );
        Mock::given(method("GET"))
            .respond_with(json(DRIFT_CHECK).insert_header("link", more.as_str()))
            .mount(&server)
            .await;

        let err = client(&server.uri())
            .cron_monitor_status("acme", cron_now())
            .await
            .unwrap_err();
        assert_eq!(err, SentryUsageError::MonitorListTruncated);
        assert_eq!(
            err.user_message(),
            "Sentry listed more cron monitors than one read will page through"
        );
    }

    /// The token must never reach the URL on this read either.
    #[tokio::test]
    async fn the_monitor_read_keeps_the_token_in_a_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json(DRIFT_CHECK))
            .mount(&server)
            .await;

        client(&server.uri())
            .cron_monitors("acme")
            .await
            .expect("decodes");

        let requests = server.received_requests().await.expect("recorded requests");
        let request = requests.first().expect("one request");
        assert!(
            !request.url.as_str().contains("sentry_token_value"),
            "the token must not appear in the URL: {}",
            request.url
        );
    }

    /// The wire vocabulary, pinned: the status words the classifier compares
    /// against are the API's, and a typo here would silently turn every monitor
    /// red (or green).
    #[test]
    fn the_monitor_status_vocabulary_matches_the_wire() {
        assert_eq!(STATUS_OK, "ok");
        assert_eq!(STATUS_DISABLED, "disabled");
        assert_eq!(STATUS_UNKNOWN, "unknown");
    }

    /// The wire shape itself, asserted against the live-API key list. A field
    /// this build reads that the API does not send is exactly the `projectSlug`
    /// bug; a fixture built from the MCP server's output is how it got there.
    #[test]
    fn the_monitor_fixture_carries_the_live_wire_shape() {
        let raw: Value = serde_json::from_str(DRIFT_CHECK).expect("fixture decodes");
        let monitor = &raw[0];
        let mut keys: Vec<&str> = monitor
            .as_object()
            .expect("a monitor object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "config",
                "dateCreated",
                "environments",
                "id",
                "isMuted",
                "isUpserting",
                "name",
                "owner",
                "project",
                "slug",
                "status",
            ],
            "the monitor object's keys are the live API's"
        );

        let mut env_keys: Vec<&str> = monitor["environments"][0]
            .as_object()
            .expect("an environment object")
            .keys()
            .map(String::as_str)
            .collect();
        env_keys.sort_unstable();
        assert_eq!(
            env_keys,
            vec![
                "activeIncident",
                "dateCreated",
                "isMuted",
                "lastCheckIn",
                "name",
                "nextCheckIn",
                "nextCheckInLatest",
                "status",
            ]
        );

        assert!(
            monitor.get("projectSlug").is_none(),
            "there is no flat projectSlug on the wire"
        );
        assert!(
            monitor.get("hasMoreEnvironments").is_none(),
            "there is no environment-truncation signal on the wire"
        );
        assert!(
            monitor["environments"][1]["activeIncident"].is_null(),
            "activeIncident is a key on healthy environments too, holding null"
        );
    }

    /// The two fields that do not exist, asserted from the *outside*: a payload
    /// carrying either one must change nothing this module decides.
    ///
    /// A guard written against `hasMoreEnvironments` would compile, pass its own
    /// tests, and protect nothing — the original issue specified one. So the only
    /// honest assertion is that the code is indifferent to it: the two-environment
    /// monitor still reports two environments whatever the field claims.
    #[test]
    fn a_synthesised_field_changes_nothing_because_nothing_reads_it() {
        let with_synthetic = DRIFT_CHECK.replace(
            "\"isUpserting\": false,",
            "\"isUpserting\": false, \"hasMoreEnvironments\": true, \"projectSlug\": \"WRONG\",",
        );
        let honest = summarized(DRIFT_CHECK);
        let mcp_shaped = summarized(&with_synthetic);
        assert_eq!(
            honest, mcp_shaped,
            "the MCP server's synthesised fields must not be load-bearing"
        );
        assert_eq!(mcp_shaped.environment_count(), 2);
        assert_eq!(mcp_shaped.alerts()[0].scope(), "platform/prd");
    }
}
