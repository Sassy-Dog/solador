//! The **Sentry Crons** panel: every cron monitor environment that is not ok,
//! and **how long it has been broken**.
//!
//! No counterpart — this panel exists only on the cross-platform cockpit. The
//! data layer beneath it is [`usage::sentry`]; this module is the view side and
//! holds to the same rule as every other panel here: **every string and colour
//! the frontend paints is made in Rust.**
//!
//! ## Why the age rule is the whole panel
//!
//! `cron-relay-drift-check` sat red for a week with no signal after the first
//! hour, because the Sentry rule behind it fires on *first seen* and
//! *regression*: a weekly cron alerts once and then goes quiet, so day 6 of an
//! outage looks identical to day 1. This panel's job is to make the sixth day
//! *look* like the sixth day, which it can only do if the age is measured from
//! the incident's start (`crates/usage`'s [`CronAge`]) rather than from the last
//! check-in. On that monitor the two read **7d 22h** and **0d 22h**.
//!
//! The provenance therefore travels all the way to the pixel: an age derived
//! from a check-in is rendered with a `≈` and says so in its own row, because it
//! is a weaker claim and must not pass for the precise one.
//!
//! ## Three states that must not be conflated
//!
//! **No token** is a setup instruction, muted — nothing is wrong. **A failed
//! read** is red and names the failure. **A blind read** — no monitors at all, or
//! a monitor whose environments could not be read — is also red, because a panel
//! that renders those as empty-and-green is the exact failure mode the work
//! exists to remove. Only the fourth case, a measured org with nothing broken,
//! is entitled to say so.

use serde_json::{json, Value};
use usage::sentry::{CronAge, CronAlert, CronMonitorsSummary};
use viewmodel::cockpit::PanelKind;
use viewmodel::color;

use crate::panel::{status_footer, Configured};

/// The staleness window. Reuses the Neon/Sentry figure rather than restating it:
/// this read is on the same hourly cadence, so the window has to sit above it or
/// the panel would be permanently stale.
pub const STALE_AFTER_SECS: u64 = crate::usage::PROVIDER_STALE_AFTER_SECS;

/// The panel's zero-setup state. An instruction, not a failure — and the only
/// message [`Configured::Absent`] may paint.
pub const UNCONFIGURED_MESSAGE: &str = "Connect a Sentry token in Settings";

/// Nothing has read the credential store yet, or a read is in flight.
pub const LOADING_MESSAGE: &str = "reading monitors…";

/// Configured, not loading, no summary and no error — a state that should never
/// last, kept because a blank card would be worse.
pub const NO_DATA_MESSAGE: &str = "no monitor data";

/// Rendered where a duration is not a duration.
const UNKNOWN: &str = "—";

// MARK: - State

/// Everything the panel renders from.
#[derive(Debug, Default)]
pub struct CronsState {
    /// Whether a Sentry token is stored. Three states, not a `bool`: the frame
    /// before any pass has looked is neither "configured" nor "not", and a
    /// defaulted `false` would open the panel on a setup instruction at a machine
    /// whose token is fine.
    token: Configured,
    /// The last successful read. Retained through a failure — an hourly read
    /// carried forward with its age in the footer beats a blank card, and a cron
    /// that has been broken for a week is not less broken because one poll failed.
    summary: Option<CronMonitorsSummary>,
    /// The last **success**, which is what [`status_footer`] renders as
    /// `last ok {age}`. Never the last attempt.
    last_updated: Option<u64>,
    last_error: Option<String>,
    /// True from startup (or a newly-saved token) until the first read settles —
    /// what separates [`LOADING_MESSAGE`] from [`NO_DATA_MESSAGE`].
    loading: bool,
}

impl CronsState {
    #[must_use]
    pub fn new() -> Self {
        CronsState {
            loading: true,
            ..CronsState::default()
        }
    }

    /// A pass read the credential store and found no token. Everything is
    /// dropped, so a stale monitor list cannot sit behind the setup message and
    /// reappear the moment a *different* token is pasted in.
    pub fn unconfigure(&mut self) {
        *self = CronsState {
            token: Configured::Absent,
            ..CronsState::default()
        };
    }

    /// A token is configured and a read is in flight.
    ///
    /// Called **before** the request, not after it: the panel is what tells the
    /// operator this watch exists, and learning that from a completed fetch is
    /// what made every panel's first frame indistinguishable from "there is no
    /// credential".
    pub fn begin(&mut self) {
        self.token = Configured::Present;
        if self.summary.is_none() {
            self.loading = true;
        }
    }

    pub fn succeeded(&mut self, summary: CronMonitorsSummary, at: u64) {
        self.token = Configured::Present;
        self.summary = Some(summary);
        self.last_updated = Some(at);
        self.last_error = None;
        self.loading = false;
    }

    pub fn failed(&mut self, error: String) {
        self.token = Configured::Present;
        self.last_error = Some(error);
        self.loading = false;
    }

    /// The credential store would not answer, so we do not know whether a token
    /// is configured.
    ///
    /// Deliberately not [`unconfigure`](Self::unconfigure): that paints the setup
    /// instruction, which is the one state this panel must never confuse with a
    /// failure. A panel nobody configured stays silent — a keychain hiccup must
    /// not conjure a card for a watch nobody set up.
    pub fn unreadable(&mut self, error: String) {
        if self.token.is_present() {
            self.last_error = Some(error);
            self.loading = false;
        }
    }

    /// Whether a read has ever landed. Drives the poll loop's first-read retry,
    /// the same way the Azure panel's does.
    #[must_use]
    pub fn has_succeeded(&self) -> bool {
        self.last_updated.is_some()
    }
}

// MARK: - Formatting

/// `7d 22h` / `0d 22h` / `14m`.
///
/// Days-and-hours down to the hour, matching the Slack digest this panel is the
/// ambient half of, verbatim — including the `0d` on a sub-day age, which is what
/// makes `0d 22h` and `7d 22h` line up as the same shape. `viewmodel::format`'s
/// `duration` collapses to a single unit (`7d`), which is right for a staleness
/// footer and wrong here: the hours are the difference between "this broke this
/// morning" and "this broke last week".
#[must_use]
pub fn broken_for(secs: u64) -> String {
    let (days, hours) = (secs / 86_400, (secs % 86_400) / 3600);
    if days == 0 && hours == 0 {
        return format!("{}m", (secs % 3600) / 60);
    }
    format!("{days}d {hours}h")
}

/// Said when there is no incident to measure from.
const NEVER_CHECKED_IN: &str = "never checked in";

/// How an age renders, and how confident that rendering is entitled to look.
///
/// The `≈` and the amber are the *visible* half of the fallback rule: a figure
/// derived from the last check-in is a weaker claim than one derived from an
/// incident start, and rendering the two identically would imply precision this
/// panel does not have.
fn age_label(age: CronAge) -> (String, u32) {
    match age {
        CronAge::Incident { secs } => (broken_for(secs), color::RED),
        CronAge::SinceLastCheckIn { secs } => (format!("≈ {}", broken_for(secs)), color::AMBER),
        CronAge::NeverCheckedIn => (NEVER_CHECKED_IN.to_owned(), color::AMBER),
        CronAge::Unreadable => (UNKNOWN.to_owned(), color::MUTED),
    }
}

/// The second line of a row: where the monitor lives, what state it is in, and
/// any qualification the first line's number needs.
fn detail(alert: &CronAlert) -> String {
    let mut parts = vec![alert.scope(), alert.status.clone()];
    if let Some(reason) = alert.suppression {
        parts.push(reason.reason().to_owned());
    }
    match alert.age {
        // Named, not merely marked with a `≈`: the operator has to be able to
        // tell why this row's age is softer than the one above it.
        CronAge::SinceLastCheckIn { .. } => {
            parts.push("no incident · since last check-in".to_owned())
        }
        CronAge::Unreadable => parts.push("incident start unreadable".to_owned()),
        CronAge::Incident { .. } | CronAge::NeverCheckedIn => {}
    }
    parts.join(" · ")
}

/// One row. The dot and the label go muted when the entry is suppressed: the
/// operator asked not to be told, and it is still listed so a mute nobody
/// remembers setting stays findable.
fn row(alert: &CronAlert) -> Value {
    let (age, age_color) = age_label(alert.age);
    let row_color = if alert.is_suppressed() {
        color::MUTED
    } else {
        color::RED
    };
    json!({
        "id": alert.id,
        "label": alert.monitor,
        "detail": detail(alert),
        "age": age,
        "ageColor": color::hex(if alert.is_suppressed() { color::MUTED } else { age_color }),
        "color": color::hex(row_color),
        "suppressed": alert.is_suppressed(),
        // The whole row as one sentence, for the hover — the same wording the
        // Slack digest uses, so the two surfaces read alike.
        "title": title(alert),
    })
}

/// `cron-relay-drift-check (platform/prd) — error for 7d 22h`, the Slack half's
/// sentence.
fn title(alert: &CronAlert) -> String {
    let scope = alert.scope();
    let tail = match alert.age {
        CronAge::Incident { secs } => format!("for {}", broken_for(secs)),
        CronAge::SinceLastCheckIn { secs } => format!(
            "with no open incident; last checked in {} ago",
            broken_for(secs)
        ),
        CronAge::NeverCheckedIn => "and has never checked in".to_owned(),
        CronAge::Unreadable => "for an unreadable length of time".to_owned(),
    };
    let suffix = alert
        .suppression
        .map(|reason| format!(" ({})", reason.reason()))
        .unwrap_or_default();
    format!(
        "{} ({scope}) — {} {tail}{suffix}",
        alert.monitor, alert.status
    )
}

// MARK: - View

/// The panel payload.
#[must_use]
pub fn view(state: &CronsState, now: u64) -> Value {
    let kind = PanelKind::SentryCrons;
    let mut payload = json!({
        "id": kind.id(),
        "title": kind.title(),
        "trailing": "",
        "message": Value::Null,
        "rows": Value::Array(vec![]),
        "footer": Value::Null,
        // Published for the frontend's refresh cadence, which is faster while a
        // panel is still filling in. Nothing is known until a pass has read the
        // credential store.
        "loading": state.token.is_unknown() || (state.loading && state.last_error.is_none()),
    });

    // The ladder, and its order is the point: the unconfigured branch comes
    // before the error branch so a machine with no token never reports a failure
    // it has no way to have had — but only `Absent` may take it, because
    // `Unknown` is the frame before any pass has looked.
    let Some(summary) = state.summary.as_ref().filter(|_| state.token.is_present()) else {
        payload["message"] = match (state.token, state.last_error.as_deref()) {
            (Configured::Unknown, _) => message(LOADING_MESSAGE, color::MUTED),
            (Configured::Absent, _) => message(UNCONFIGURED_MESSAGE, color::MUTED),
            // Red, and the failure named: this one *is* broken.
            (Configured::Present, Some(error)) => message(error, color::RED),
            (Configured::Present, None) => message(
                if state.loading {
                    LOADING_MESSAGE
                } else {
                    NO_DATA_MESSAGE
                },
                color::MUTED,
            ),
        };
        return payload;
    };

    payload["rows"] = Value::Array(summary.alerts().iter().map(row).collect());
    payload["trailing"] = json!(trailing(summary));

    // A read that cannot be trusted outranks everything else the panel could
    // say — including the rows it did manage to read, which stay on screen under
    // the warning rather than being suppressed by it.
    payload["message"] = match (summary.blind_read(), summary.alerts().is_empty()) {
        (Some(blind), _) => message(&blind, color::RED),
        // The only rendering entitled to say nothing is wrong: monitors came
        // back, environments came back, and none of them is a row.
        (None, true) => message(
            &format!(
                "all {} monitors ok across {} environments",
                summary.monitor_count(),
                summary.environment_count()
            ),
            color::GREEN_DIM,
        ),
        (None, false) => Value::Null,
    };

    payload["footer"] = status_footer(
        state.last_updated,
        state.last_error.as_deref(),
        now,
        STALE_AFTER_SECS,
    );
    payload
}

fn message(text: &str, color: u32) -> Value {
    json!({ "text": text, "color": color::hex(color) })
}

/// Said instead of `all ok` when the read cannot support that claim.
const UNREAD: &str = "couldn't read";

/// `2 not ok` / `all ok · 1 suppressed` / `all ok` / `couldn't read`.
///
/// Counted from the summary's own rows, so the trailing label can never disagree
/// with what is under it. Suppressed entries never inflate the headline count —
/// that is what suppression is for — but they are named, because a muted monitor
/// nobody remembers muting is exactly the thing this panel should surface.
///
/// **A blind read never says `all ok`.** The message under it is already red, and
/// a trailing label claiming the opposite in the same card is the fabrication this
/// panel exists to remove wearing four characters instead of a row.
fn trailing(summary: &CronMonitorsSummary) -> String {
    let (active, suppressed) = (summary.active_count(), summary.suppressed_count());
    let head = if active > 0 {
        format!("{active} not ok")
    } else if summary.blind_read().is_some() {
        UNREAD.to_owned()
    } else {
        "all ok".to_owned()
    };
    if suppressed > 0 {
        return format!("{head} · {suppressed} suppressed");
    }
    head
}

// MARK: - Fixtures

/// Which rendering `--dump-crons` should produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fixture {
    /// Every row this panel has: an incident-derived age, the check-in fallback,
    /// a never-checked-in environment, and a suppressed one.
    Alerting,
    /// A measured org with nothing broken — the only state allowed to say so.
    Healthy,
    /// The blind read: the API answered and listed no monitors at all.
    Blind,
    /// A failed read — red, with the failure named.
    Failed,
    /// No Sentry token at all — the setup instruction.
    Unconfigured,
}

/// A hand-made state for the offline fixtures.
///
/// The **wire** objects are hand-made and then run through the real
/// [`usage::summarize_monitors`], rather than the summary being assembled
/// directly: the ages, the sort order and the suppression reasons in the dumped
/// fixture are then the classifier's own output, so a Playwright assertion about
/// `7d 22h` is an assertion about the code and not about a string typed here.
///
/// Every timestamp is relative to `now`, so the ages are the same on every
/// machine and at every hour: `7d 22h`, `0d 22h`.
#[must_use]
pub fn fixture_state(kind: Fixture, at: u64, now: chrono::DateTime<chrono::Utc>) -> CronsState {
    let mut state = CronsState::new();
    if kind == Fixture::Unconfigured {
        state.unconfigure();
        return state;
    }
    state.begin();
    if kind == Fixture::Failed {
        state.failed("token invalid — paste a new one in Settings".to_owned());
        return state;
    }
    let monitors = match kind {
        Fixture::Blind => Vec::new(),
        Fixture::Healthy => fixture_monitors(now, false),
        _ => fixture_monitors(now, true),
    };
    state.succeeded(usage::summarize_monitors(&monitors, now), at);
    state
}

/// The wire fixture, shaped after the raw REST payload — never after the Sentry
/// MCP server's normalised output, which synthesises fields the API does not
/// send.
fn fixture_monitors(
    now: chrono::DateTime<chrono::Utc>,
    alerting: bool,
) -> Vec<usage::sentry::CronMonitor> {
    use usage::sentry::{CronEnvironment, CronIncident, CronMonitor, CronProject};

    let stamp = |secs: i64| (now - chrono::Duration::seconds(secs)).to_rfc3339();
    let environment =
        |name: &str, status: &str, check_in: Option<i64>, incident: Option<i64>| CronEnvironment {
            name: Some(name.to_owned()),
            status: Some(status.to_owned()),
            is_muted: Some(Value::Bool(false)),
            last_check_in: check_in.map(stamp),
            active_incident: incident.map(|secs| CronIncident {
                starting_timestamp: Some(stamp(secs)),
            }),
        };
    let monitor = |slug: &str, project: &str, environments: Vec<CronEnvironment>| CronMonitor {
        slug: Some(slug.to_owned()),
        name: Some(slug.to_owned()),
        status: Some("active".to_owned()),
        is_muted: Some(Value::Bool(false)),
        project: Some(CronProject {
            slug: Some(project.to_owned()),
        }),
        environments: Some(environments),
    };

    const DAY: i64 = 86_400;
    const HOUR: i64 = 3600;

    if !alerting {
        return vec![
            monitor(
                "cron-relay-drift-check",
                "platform",
                vec![environment("prd", "ok", Some(HOUR), None)],
            ),
            monitor(
                "nightly-rollup",
                "gadget-jobs",
                vec![environment("prd", "ok", Some(2 * HOUR), None)],
            ),
        ];
    }

    vec![
        // The monitor this work came from: an open incident a week old, and a
        // check-in from this morning. The two are what make the age rule visible.
        monitor(
            "cron-relay-drift-check",
            "platform",
            vec![
                environment("prd", "error", Some(22 * HOUR), Some(7 * DAY + 22 * HOUR)),
                environment("dev", "ok", Some(HOUR), None),
            ],
        ),
        // No incident, so the age falls back to the check-in — and says so.
        monitor(
            "nightly-rollup",
            "gadget-jobs",
            vec![environment("prd", "missed_checkin", Some(22 * HOUR), None)],
        ),
        // Never checked in at all: no duration to render.
        monitor(
            "brand-new-cron",
            "platform",
            vec![environment("prd", "active", None, None)],
        ),
        // Suppressed, and still listed with its reason.
        CronMonitor {
            is_muted: Some(Value::Bool(true)),
            ..monitor(
                "legacy-sweeper",
                "platform",
                vec![environment("prd", "error", Some(30 * DAY), Some(30 * DAY))],
            )
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use usage::sentry::CronSuppression;

    const NOW: u64 = 1_700_000_000;

    fn fixture_now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-08-04T15:00:00Z")
            .expect("a fixed now")
            .with_timezone(&chrono::Utc)
    }

    fn payload(kind: Fixture) -> Value {
        view(&fixture_state(kind, NOW, fixture_now()), NOW)
    }

    fn rows(payload: &Value) -> &Vec<Value> {
        payload["rows"].as_array().expect("rows")
    }

    fn row_by(payload: &Value, monitor: &str) -> Value {
        rows(payload)
            .iter()
            .find(|row| row["label"] == monitor)
            .cloned()
            .unwrap_or_else(|| panic!("no row for {monitor}"))
    }

    // MARK: the age rule

    /// **The regression this panel exists for, at the pixel.** The dumped fixture
    /// holds a monitor whose incident is a week old and whose last check-in is
    /// this morning, and the row renders the week.
    #[test]
    fn a_failing_monitor_renders_the_age_of_its_incident_not_of_its_check_in() {
        let row = row_by(&payload(Fixture::Alerting), "cron-relay-drift-check");
        assert_eq!(row["age"], "7d 22h");
        assert_eq!(
            row["age"], "7d 22h",
            "0d 22h here would be the check-in, which is the bug"
        );
        assert_eq!(row["ageColor"], color::hex(color::RED));
        assert_eq!(row["detail"], "platform/prd · error");
        assert_eq!(
            row["title"], "cron-relay-drift-check (platform/prd) — error for 7d 22h",
            "the Slack digest's sentence, verbatim"
        );
    }

    /// The fallback is visible in three places at once: the `≈`, the amber, and
    /// the detail line naming it. A `0d 22h` in plain red would be
    /// indistinguishable from an incident that started this morning.
    #[test]
    fn a_check_in_derived_age_is_marked_as_the_weaker_claim_it_is() {
        let row = row_by(&payload(Fixture::Alerting), "nightly-rollup");
        assert_eq!(row["age"], "≈ 0d 22h");
        assert_eq!(row["ageColor"], color::hex(color::AMBER));
        assert_eq!(
            row["detail"],
            "gadget-jobs/prd · missed_checkin · no incident · since last check-in"
        );
        assert!(row["title"]
            .as_str()
            .expect("title")
            .contains("with no open incident"));
    }

    /// `lastCheckIn: null` with no incident is words, never a duration — and
    /// never a `0d 0h`, which would read as "broke just now".
    #[test]
    fn a_monitor_that_never_checked_in_says_so_instead_of_reporting_a_duration() {
        let row = row_by(&payload(Fixture::Alerting), "brand-new-cron");
        assert_eq!(row["age"], "never checked in");
        assert_eq!(row["ageColor"], color::hex(color::AMBER));
    }

    /// An unreadable age is the em dash, not a substituted figure.
    #[test]
    fn an_unreadable_age_is_an_em_dash() {
        assert_eq!(age_label(CronAge::Unreadable).0, "—");
        assert_eq!(age_label(CronAge::Unreadable).1, color::MUTED);
    }

    /// Days and hours, down to the hour, with the `0d` kept — that is what makes
    /// `0d 22h` and `7d 22h` line up as the same shape, and it is the Slack
    /// half's format.
    #[test]
    fn the_age_format_keeps_its_hours_and_its_leading_zero_day() {
        assert_eq!(broken_for(7 * 86_400 + 22 * 3600 + 1017), "7d 22h");
        assert_eq!(broken_for(22 * 3600 + 1064), "0d 22h");
        assert_eq!(broken_for(86_400), "1d 0h");
        assert_eq!(broken_for(3600), "0d 1h");
        // Under an hour there is no day/hour pair worth printing.
        assert_eq!(broken_for(14 * 60), "14m");
        assert_eq!(broken_for(0), "0m");
    }

    // MARK: rows and ordering

    /// Rows come out oldest-first with the unrankable cases leading, exactly as
    /// the classifier sorted them — the view never re-sorts, so the two cannot
    /// disagree.
    #[test]
    fn the_rows_are_in_the_order_the_classifier_chose() {
        let payload = payload(Fixture::Alerting);
        let labels: Vec<&str> = rows(&payload)
            .iter()
            .map(|row| row["label"].as_str().expect("label"))
            .collect();
        assert_eq!(
            labels,
            vec![
                "brand-new-cron",         // never checked in
                "legacy-sweeper",         // 30d, suppressed
                "cron-relay-drift-check", // 7d 22h
                "nightly-rollup",         // 0d 22h
            ]
        );
    }

    /// A suppressed entry is listed with its reason and goes muted — counted, not
    /// dropped. A mute nobody remembers setting has to stay findable.
    #[test]
    fn a_suppressed_row_is_muted_and_carries_its_reason() {
        let row = row_by(&payload(Fixture::Alerting), "legacy-sweeper");
        assert_eq!(row["suppressed"], true);
        assert_eq!(row["color"], color::hex(color::MUTED));
        assert_eq!(row["ageColor"], color::hex(color::MUTED));
        assert!(row["detail"]
            .as_str()
            .expect("detail")
            .contains(CronSuppression::MonitorMuted.reason()));
    }

    /// The trailing count agrees with the rows under it, and suppression never
    /// inflates the headline.
    #[test]
    fn the_trailing_label_counts_live_problems_and_names_the_suppressed_ones() {
        assert_eq!(
            payload(Fixture::Alerting)["trailing"],
            "3 not ok · 1 suppressed"
        );
        assert_eq!(payload(Fixture::Healthy)["trailing"], "all ok");
    }

    /// Every scope is `project/environment`, read from the nested `project.slug`.
    /// `undefined/prd` is what a flat `projectSlug` produced in the Slack half.
    #[test]
    fn every_row_names_its_project_and_environment() {
        for row in rows(&payload(Fixture::Alerting)) {
            let detail = row["detail"].as_str().expect("detail");
            assert!(detail.contains('/'), "{detail} should carry project/env");
            assert!(!detail.contains("undefined"), "{detail}");
        }
    }

    // MARK: the message ladder

    /// The only rendering entitled to say nothing is wrong: monitors came back,
    /// environments came back, and none of them is a row.
    #[test]
    fn a_measured_healthy_org_says_so_in_green() {
        let payload = payload(Fixture::Healthy);
        assert!(rows(&payload).is_empty());
        assert_eq!(
            payload["message"]["text"],
            "all 2 monitors ok across 2 environments"
        );
        assert_eq!(payload["message"]["color"], color::hex(color::GREEN_DIM));
    }

    /// **The blind read.** An empty monitor list is red and explains itself — an
    /// org with no crons, a mistyped slug and an under-scoped token are
    /// indistinguishable, so none of them may render as a calm empty panel.
    #[test]
    fn an_empty_monitor_list_is_red_and_never_empty_and_green() {
        let payload = payload(Fixture::Blind);
        assert!(rows(&payload).is_empty());
        assert_eq!(payload["message"]["color"], color::hex(color::RED));
        assert_eq!(
            payload["message"]["text"],
            usage::sentry::NO_MONITORS_MESSAGE
        );
        assert_ne!(
            payload["message"]["color"],
            color::hex(color::GREEN_DIM),
            "an unread panel must never wear the healthy colour"
        );
        // …and the trailing label must not claim the opposite of the message
        // beside it. "all ok" here would be the fabrication wearing four
        // characters instead of a row.
        assert_eq!(payload["trailing"], UNREAD);
        assert_ne!(payload["trailing"], "all ok");
    }

    /// A monitor whose environments could not be read is a blind read too — and
    /// the rows that *were* read stay on screen under the warning rather than
    /// being hidden by it.
    #[test]
    fn a_monitor_with_no_environments_warns_without_hiding_the_rows_it_did_read() {
        let json = r#"
        [
          { "slug": "hollow", "project": { "slug": "platform" }, "environments": [] },
          { "slug": "loud", "project": { "slug": "platform" }, "environments": [
              { "name": "prd", "status": "error", "lastCheckIn": null,
                "activeIncident": { "startingTimestamp": "2026-08-01T00:00:00Z" } } ] }
        ]
        "#;
        let monitors: Vec<usage::sentry::CronMonitor> =
            serde_json::from_str(json).expect("fixture decodes");
        let mut state = CronsState::new();
        state.begin();
        state.succeeded(usage::summarize_monitors(&monitors, fixture_now()), NOW);
        let payload = view(&state, NOW);

        assert_eq!(payload["message"]["color"], color::hex(color::RED));
        let text = payload["message"]["text"].as_str().expect("text");
        assert!(text.contains("hollow"), "{text}");
        assert_eq!(rows(&payload).len(), 1, "the readable row still renders");
    }

    /// No token is a muted instruction, not a failure — and it is the only
    /// message [`Configured::Absent`] may paint.
    #[test]
    fn no_token_is_a_setup_instruction_and_no_rows() {
        let payload = payload(Fixture::Unconfigured);
        assert_eq!(payload["message"]["text"], UNCONFIGURED_MESSAGE);
        assert_eq!(payload["message"]["color"], color::hex(color::MUTED));
        assert!(rows(&payload).is_empty());
        assert_eq!(payload["loading"], false);
        assert!(payload["footer"].is_null());
    }

    /// **The first frame.** Nothing has read the credential store yet, so the
    /// panel says nothing about how it is configured — it must not tell an
    /// operator whose token is fine to go and paste one.
    #[test]
    fn the_frame_before_any_pass_is_loading_and_not_a_setup_instruction() {
        let payload = view(&CronsState::new(), NOW);
        assert_eq!(payload["message"]["text"], LOADING_MESSAGE);
        assert_ne!(
            payload["message"]["text"], UNCONFIGURED_MESSAGE,
            "Unknown must never paint the Absent instruction"
        );
        assert_eq!(payload["loading"], true);
    }

    /// A failed read is red and names the failure, which is a different claim
    /// from the muted setup instruction above.
    #[test]
    fn a_failed_read_is_red_and_names_itself() {
        let payload = payload(Fixture::Failed);
        assert_eq!(
            payload["message"]["text"],
            "token invalid — paste a new one in Settings"
        );
        assert_eq!(payload["message"]["color"], color::hex(color::RED));
        assert_eq!(payload["loading"], false, "a failure is not loading");
    }

    /// Configured, answered once, then failing: the rows and the count stay, and
    /// the footer carries the reason plus the age of the last **success**.
    #[test]
    fn a_failure_after_a_success_keeps_the_rows_and_dates_the_last_good_read() {
        let mut state = fixture_state(Fixture::Alerting, NOW - 600, fixture_now());
        state.failed("Sentry API request failed (HTTP 503)".to_owned());
        let payload = view(&state, NOW);

        assert_eq!(rows(&payload).len(), 4, "the last good rows stay on screen");
        assert_eq!(
            payload["footer"]["text"],
            "⚠ Sentry API request failed (HTTP 503) · last ok 10m ago"
        );
    }

    /// `last_updated` is the last **success**, never the last attempt: the footer
    /// renders it as `last ok {age}`, which is a promise only this module can
    /// keep. A panel that had never once succeeded used to report
    /// `last ok 0s ago`.
    #[test]
    fn a_read_that_never_succeeded_has_no_last_ok_to_name() {
        let payload = payload(Fixture::Failed);
        // No footer at all, because the failure is the panel's whole message
        // here — and no invented "last ok" either way.
        assert!(payload["footer"].is_null());
        let mut state = CronsState::new();
        state.begin();
        state.failed("boom".to_owned());
        assert_eq!(view(&state, NOW)["footer"], Value::Null);
    }

    /// A fresh, healthy panel renders no footer — that is what makes a footer
    /// mean something when it appears.
    #[test]
    fn a_fresh_read_renders_no_footer_and_a_stale_one_says_so() {
        assert!(payload(Fixture::Alerting)["footer"].is_null());
        let state = fixture_state(Fixture::Alerting, NOW - STALE_AFTER_SECS - 1, fixture_now());
        assert_eq!(
            view(&state, NOW)["footer"]["text"],
            "⚠ stale · updated 1h ago"
        );
    }

    /// The window sits above the hourly cadence behind it, or the panel would be
    /// permanently stale. One definition, shared with the other Sentry read —
    /// so the pair cannot drift apart.
    ///
    /// "The hourly cadence" is the *default* since #301: an operator who sets
    /// this panel's interval past 90 minutes gets a footer between every pair
    /// of polls. That reading is truthful — it really is two hours old — but it
    /// is noise rather than a warning, and closing it means deriving the window
    /// from the configured cadence, which is the shape #302 is working out on
    /// the Azure panel.
    #[test]
    fn the_staleness_window_sits_above_the_hourly_cadence() {
        assert_eq!(STALE_AFTER_SECS, 90 * 60);
        assert_eq!(STALE_AFTER_SECS, crate::usage::PROVIDER_STALE_AFTER_SECS);
        assert_eq!(crate::usage::PROVIDER_POLL_INTERVAL_SECS, 60 * 60);
        // …and the constant is live rather than merely declared: exactly at the
        // window is still fresh, one second past it is a footer.
        let fresh = fixture_state(Fixture::Alerting, NOW - STALE_AFTER_SECS, fixture_now());
        assert_eq!(view(&fresh, NOW)["footer"], Value::Null);
    }

    /// An unreadable credential store keeps a live panel's rows and adds the
    /// reason — and stays silent about a panel nobody configured, so a keychain
    /// hiccup cannot conjure a card for a watch nobody set up.
    #[test]
    fn an_unreadable_credential_store_degrades_rather_than_unconfiguring() {
        let mut live = fixture_state(Fixture::Alerting, NOW, fixture_now());
        live.unreadable("the credential store would not answer".to_owned());
        let payload = view(&live, NOW);
        assert_eq!(rows(&payload).len(), 4);
        assert_eq!(
            payload["footer"]["text"],
            "⚠ the credential store would not answer · last ok 0s ago"
        );

        let mut never = CronsState::new();
        never.unreadable("the credential store would not answer".to_owned());
        assert_eq!(view(&never, NOW)["message"]["text"], LOADING_MESSAGE);
    }

    /// A vanished token drops the retained rows: a stale monitor list must not
    /// sit behind the setup message and reappear the moment a *different* token
    /// is saved.
    #[test]
    fn clearing_the_token_drops_the_rows_it_was_showing() {
        let mut state = fixture_state(Fixture::Alerting, NOW, fixture_now());
        state.unconfigure();
        let payload = view(&state, NOW);
        assert!(rows(&payload).is_empty());
        assert_eq!(payload["message"]["text"], UNCONFIGURED_MESSAGE);
        assert!(payload["footer"].is_null());
    }

    /// The panel's identity travels from the panel table, never restated here.
    #[test]
    fn the_payload_carries_the_panel_tables_id_and_title() {
        let payload = payload(Fixture::Alerting);
        assert_eq!(payload["id"], PanelKind::SentryCrons.id());
        assert_eq!(payload["id"], "sentryCrons");
        assert_eq!(payload["title"], "Sentry Crons");
    }

    /// Every fixture is a complete payload, so a Playwright run can render any of
    /// them without the frontend inventing a missing key.
    #[test]
    fn every_fixture_carries_the_whole_payload_shape() {
        for kind in [
            Fixture::Alerting,
            Fixture::Healthy,
            Fixture::Blind,
            Fixture::Failed,
            Fixture::Unconfigured,
        ] {
            let payload = payload(kind);
            for key in [
                "id", "title", "trailing", "message", "rows", "footer", "loading",
            ] {
                assert!(payload.get(key).is_some(), "{kind:?} is missing {key}");
            }
            assert!(payload["rows"].is_array(), "{kind:?}");
            assert!(payload["loading"].is_boolean(), "{kind:?}");
        }
    }
}
