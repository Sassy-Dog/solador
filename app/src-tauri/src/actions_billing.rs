//! The **Actions billing** panel: this month's hosted Actions minutes and net
//! spend for one GitHub organization, a bar for its Actions budget, and the
//! breakdowns by SKU and by repo.
//!
//! The data layer beneath it is [`actionsbilling`] (#768): two reads of the
//! org's billing endpoints, folded without a clock. This module is the view
//! side, and holds to the rule every panel here holds to: **every string and
//! colour the frontend paints is made in Rust.** The credential is a dedicated
//! fine-grained token, never the GitHub account's, and it does not pass through
//! this file: the poll loop in `main.rs` hands the client a token it re-reads
//! from the credential store on every pass.
//!
//! ## Two clocks, kept apart
//!
//! **The footer and the freshness line are about the poller.** They keep the
//! meaning every panel gives them: the last *successful read*
//! ([`status_footer`](crate::panel::status_footer)'s `last_updated` is the last
//! success, not the last attempt), and how old the figure on screen is against
//! the operator's cadence. Healthy and on time, both are absent.
//!
//! **`dataThrough` is about the report.** GitHub's usage report lags by hours,
//! so the newest item it holds is older than the read that fetched it. That is
//! said once, in plain words (`usage through Nov 19 14:00 UTC`), and it is
//! deliberately **not** fed into the footer or the freshness payload: any
//! non-empty footer or freshness becomes a Needs Attention warning
//! (`dashboard::source_view`), and a report that always lags would otherwise
//! light the tile permanently.
//!
//! ## Two reads, kept apart
//!
//! Usage and budgets are separate results ([`actionsbilling::MonthRead`]), so a
//! failed budget read never hides the usage and the reverse. Each half is its
//! own [`ProviderState`], retains its last good reading through a failure, and
//! raises its own attributed footer line. The budget keeps the spend its
//! spend was measured against (it lives inside [`BudgetSpend`]), so a retained
//! bar and a fresher headline can never be printed as one ratio.
//!
//! ## Billing alerts (#770)
//!
//! Two rules over the usage report's per-repo, per-day hosted minutes, judged in
//! [`actionsbilling::hosted`] and **evaluated here, when the view is built**,
//! from the retained fold and the settings of that moment, so a save re-flags
//! the tile without waiting for a poll. A tripped rule is a red row in
//! `alerts`; a rule that could not be judged (the previous month's read failed,
//! so the 7-day baseline is unknown) is a muted row that says so and flags
//! nothing. The retained previous-month result lives and dies with the usage it
//! was read beside.
//!
//! ## States that must not be conflated
//!
//! **No token or no org** is a muted setup sentence: nothing is wrong. **A failed
//! read** is red and is the [`Fault`](fault::Fault) sentence #768 classified.
//! **No Actions usage this month** is a measurement and says so in words, not
//! `0` or `$0.00`. **No Actions budget** is [`BudgetStatus::Absent`] and says
//! so, with no bar: it is never a 0% bar and never a bar against a defaulted
//! zero. **A budget whose amount is 0** is a real setting (no paid usage):
//! spend above it paints the spend over `$0.00` with a full red bar, so the tile
//! raises Needs Attention; measured zero spend paints `$0.00 / $0.00` with no
//! bar; an unmeasured spend paints the em dash. No ratio is ever formed.

use actionsbilling::{
    BudgetSpend, BudgetStatus, DailyTrend, HostedDays, MonthRead, PreviousMonth, SpikeThresholds,
    Totals, UsageSummary,
};
use chrono::{DateTime, Datelike, NaiveDate, Timelike, Utc};
use serde_json::{json, Value};
use store::actions_billing_alerts::ActionsBillingAlerts;
use viewmodel::cockpit::PanelKind;
use viewmodel::color;
use viewmodel::freshness::Freshness;

use crate::azure::{
    group_thousands, usd, BUDGET_AMBER_AT, BUDGET_RED_AT, MONTHS_LONG, MONTHS_SHORT,
};
use crate::panel::{
    attributed_freshness_payload, attributed_status_footer, merged_footer, merged_freshness,
    progress_bar, stale_after_for_slow_cadence,
};
use crate::usage::ProviderState;

/// The panel's zero-setup state. An instruction, not a failure; it names where
/// the fix lives.
pub const UNCONFIGURED_MESSAGE: &str =
    "Add a GitHub org and billing token in Settings \u{2192} Sources";

/// Configured, with the first read in flight.
pub const LOADING_MESSAGE: &str = "reading billing\u{2026}";

/// Configured, nothing in flight, nothing to show and no reason: the state that
/// should never last, kept because a blank card would be worse.
pub const NO_DATA_MESSAGE: &str = "no billing data";

/// A month whose report read fine and holds no Actions item. A measurement.
pub const NO_USAGE_MESSAGE: &str = "no Actions usage this month";

/// An org with no organization-scoped Actions budget. A fact, not a zero.
pub const NO_BUDGET_MESSAGE: &str = "no Actions budget set";

/// The two halves' names in the header lines, and the sections they date.
const USAGE_ID: &str = "usage";
const BUDGET_ID: &str = "budget";

/// How many rows each breakdown shows: `actionsbilling::TOP_N`, restated as the
/// render cap and asserted equal in the tests below.
const TOP_ROWS: usize = actionsbilling::TOP_N;

/// Rendered wherever a figure is not a number.
const UNKNOWN: &str = "\u{2014}";

/// The row an item with no repository lands on. GitHub leaves the name empty
/// for org-level items; they are real spend and are not dropped.
const NO_REPOSITORY: &str = "(no repository)";

// MARK: - Formatting

/// `1234.4` -> `1,234 min`. A non-finite figure is not a number.
#[must_use]
pub fn minutes(value: f64) -> String {
    if !value.is_finite() {
        return UNKNOWN.to_owned();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    format!(
        "{sign}{} min",
        group_thousands(&format!("{:.0}", value.abs()))
    )
}

/// `Nov 19 14:00 UTC`: the report's own clock, in UTC like every month figure
/// here, so the line reads the same on every machine.
#[must_use]
pub fn through(at: DateTime<Utc>) -> String {
    format!(
        "{} {} {:02}:{:02} UTC",
        MONTHS_SHORT[(at.month0() as usize).min(11)],
        at.day(),
        at.hour(),
        at.minute()
    )
}

/// `November 2026`.
fn month_year((year, month): (i32, u32)) -> String {
    let name = MONTHS_LONG[(month.saturating_sub(1) as usize).min(11)];
    format!("{name} {year}")
}

/// What the trend is called wherever it is named.
const TREND_LABEL: &str = "HOSTED MINUTES PER DAY";

/// The sentence for a report whose newest day is the 1st: one day is not a line.
pub const TREND_STARTS_ON_SECOND: &str = "the daily trend starts on the 2nd";

/// `Nov 14`, in UTC like the report's own dates.
fn month_day(day: NaiveDate) -> String {
    format!(
        "{} {}",
        MONTHS_SHORT[(day.month0() as usize).min(11)],
        day.day()
    )
}

/// The Full view's daily hosted-minutes trend (#771), as the `(trend,
/// trendNote)` pair: a line with its y range and caption, or the sentence that
/// says why there is none. The series, the gaps and the range are decided in
/// [`actionsbilling::hosted`]; this only words them. `lo` is 0 and `hi` the
/// series maximum, so the painter needs nothing else.
fn trend_view(hosted: &HostedDays, data_through: DateTime<Utc>) -> (Value, Value) {
    match hosted.daily_total(data_through.date_naive()) {
        DailyTrend::StartsOnSecond => (
            Value::Null,
            json!({
                "text": TREND_STARTS_ON_SECOND,
                "color": color::hex(color::MUTED),
            }),
        ),
        DailyTrend::Series {
            first,
            last,
            minutes: values,
        } => {
            let peak = values.iter().copied().fold(0.0_f64, f64::max);
            (
                json!({
                    "label": TREND_LABEL,
                    "values": values,
                    "lo": 0.0,
                    "hi": peak,
                    "first": month_day(first),
                    "last": month_day(last),
                    "caption": format!(
                        "{} \u{2013} {} \u{b7} peak {}",
                        month_day(first),
                        month_day(last),
                        minutes(peak)
                    ),
                    "color": color::hex(color::GREEN),
                    "captionColor": color::hex(color::MUTED),
                }),
                Value::Null,
            )
        }
    }
}

/// The payload as the `actions_billing` command answers it: the daily trend
/// (#771) stays only for a read that names a tile (the Full view's). A read that
/// names none, such as the boot read, carries neither the line nor its sentence.
/// The dashboard does not come through here; no row it builds carries the trend.
#[must_use]
pub fn for_read(mut payload: Value, full_view: bool) -> Value {
    if !full_view {
        payload["trend"] = Value::Null;
        payload["trendNote"] = Value::Null;
    }
    payload
}

// MARK: - State

/// A failure a retry can fix: the request never completed, or GitHub itself
/// failed.
fn is_transient<T>(result: &Result<T, actionsbilling::ActionsBillingError>) -> bool {
    matches!(
        result,
        Err(actionsbilling::ActionsBillingError::Unreachable
            | actionsbilling::ActionsBillingError::VendorFailure { .. })
    )
}

/// What the budget half holds: the status, whose [`BudgetSpend`] carries the
/// net spend it was measured against. Kept together so a retained bar is never
/// painted beside a newer headline as if they were one ratio.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetReading {
    pub status: BudgetStatus,
}

/// Everything the panel renders from.
#[derive(Debug, Default)]
pub struct ActionsBillingState {
    usage: ProviderState<UsageSummary>,
    budget: ProviderState<BudgetReading>,
    /// The `(year, month)` the retained usage was read for. A read that fails
    /// after a month rollover keeps last month's figures, and the caption must
    /// say whose they are.
    period: Option<(i32, u32)>,
    /// What the previous month's read came to on the pass that produced the
    /// retained usage, for the spike rule's 7-day window. Replaced and dropped
    /// with that usage, so a baseline is never paired with a different month's
    /// figures.
    previous: PreviousMonth,
    /// Whether the last pass failed in a way a retry can fix: the request never
    /// completed, or GitHub itself failed. Never a rejected token, a wrong org
    /// or a rate limit, which a fast retry would only repeat.
    transient_failure: bool,
    /// The organization the retained figures were read for. A pass for another
    /// organization drops them ([`note_org`](Self::note_org), before the
    /// credential is read, and [`begin`](Self::begin)), so neither a failed
    /// first read nor an unreadable credential store for org B shows org A's
    /// headline, bar or attention flag.
    org: Option<String>,
}

impl ActionsBillingState {
    #[must_use]
    pub fn new() -> Self {
        ActionsBillingState::default()
    }

    /// A pass read the credential store and found no token, or the settings
    /// hold no org. Everything is dropped so a stale figure cannot sit behind
    /// the setup sentence and reappear under a different token.
    pub fn unconfigure(&mut self) {
        self.usage.unconfigure();
        self.budget.unconfigure();
        self.period = None;
        self.previous = PreviousMonth::NotNeeded;
        self.transient_failure = false;
        self.org = None;
    }

    /// The settings name `org` for this pass. A different organization than the
    /// retained figures came from clears both halves' figures, but, unlike
    /// [`unconfigure`](Self::unconfigure), records no absence: nothing has been
    /// observed about the credential yet, so a keychain refusal on this pass is
    /// still said as one, never as the setup sentence. Called before the
    /// credential is read, so a pass that cannot read it never shows another
    /// org's figures; the same organization keeps what it has.
    pub fn note_org(&mut self, org: &str) {
        if self.org.as_deref().is_some_and(|kept| kept != org) {
            self.usage.clear_figures();
            self.budget.clear_figures();
            self.period = None;
            self.previous = PreviousMonth::NotNeeded;
            self.transient_failure = false;
            self.org = None;
        }
    }

    /// A token and `org` are configured and a read is in flight.
    pub fn begin(&mut self, org: &str) {
        self.note_org(org);
        self.org = Some(org.to_owned());
        self.usage.begin();
        self.budget.begin();
    }

    /// The credential store would not answer, so whether a token exists is not
    /// known. Deliberately not [`unconfigure`](Self::unconfigure): a locked
    /// keychain must not delete a live figure.
    pub fn unreadable(&mut self, error: &str) {
        self.usage.unreadable(error.to_owned());
        self.budget.unreadable(error.to_owned());
    }

    /// Both reads of one pass, each recorded on its own half.
    pub fn record(&mut self, read: MonthRead, at: u64) {
        self.transient_failure = is_transient(&read.usage) || is_transient(&read.budget);
        match read.usage {
            Ok(summary) => {
                self.usage.succeeded(summary, at, None);
                self.period = Some(read.period);
                self.previous = read.previous;
            }
            Err(error) => self.usage.failed(error.user_message()),
        }
        match read.budget {
            Ok(status) => self.budget.succeeded(BudgetReading { status }, at, None),
            Err(error) => self.budget.failed(error.user_message()),
        }
    }

    /// Whether the loop should ask again before the operator's cadence. Only
    /// while nothing has ever been read, and only after a failure a retry can
    /// fix (the network not yet up at login): a rejected token, a wrong
    /// organization or a rate limit would just be asked again every minute.
    #[must_use]
    pub fn wants_quick_retry(&self) -> bool {
        self.transient_failure
            && self.usage.last_updated.is_none()
            && self.budget.last_updated.is_none()
    }

    fn is_loading(&self) -> bool {
        self.usage.configured.is_unknown()
            || (self.usage.configured.is_present()
                && self.usage.summary.is_none()
                && self.usage.last_error.is_none())
    }
}

// MARK: - View

fn breakdown_rows(rows: impl Iterator<Item = (String, Totals)>) -> Vec<Value> {
    rows.map(|(name, totals)| {
        json!({
            "name": name,
            "value": figures(&totals),
            "dotColor": color::hex(color::GREEN_DIM),
        })
    })
    .collect()
}

/// `1,234 min · $12.34`, or just the dollars for a slice that holds no minutes
/// (Actions storage is billed in gigabyte-hours, and `0 min` beside it would
/// claim a measurement of minutes that were never the unit).
fn figures(totals: &Totals) -> String {
    if totals.minutes > 0.0 {
        format!("{} \u{b7} {}", minutes(totals.minutes), usd(totals.net))
    } else {
        usd(totals.net)
    }
}

/// The budget block: its label (which says whether the budget stops jobs), the
/// spend over the amount, and the bar when there is one to draw.
///
/// The measured spend is printed whenever it was measured. A bar exists for a
/// positive amount (its fraction) and for spend over a zero amount (full and
/// red, the step a bar at 1.0 takes); nothing else draws one.
fn budget_block(reading: &BudgetReading) -> Option<Value> {
    let BudgetStatus::Present {
        amount,
        prevent_further_usage,
        spend,
    } = &reading.status
    else {
        return None;
    };
    let label = if *prevent_further_usage {
        "BUDGET \u{b7} STOPS JOBS"
    } else {
        "BUDGET \u{b7} JOBS CONTINUE"
    };
    let printed = reading
        .status
        .spend()
        .map_or_else(|| UNKNOWN.to_owned(), usd);
    let mut block = json!({
        "label": label,
        "value": format!("{printed} / {}", usd(*amount)),
        "valueColor": color::hex(color::MUTED),
        "bar": Value::Null,
    });
    match spend {
        BudgetSpend::Fraction { fraction, .. } => {
            block["bar"] = progress_bar(*fraction, BUDGET_AMBER_AT, BUDGET_RED_AT);
        }
        // Over a budget of nothing: as full and as red as a bar at 1.0.
        BudgetSpend::OverZeroBudget { .. } => {
            block["bar"] = progress_bar(BUDGET_RED_AT, BUDGET_AMBER_AT, BUDGET_RED_AT);
        }
        BudgetSpend::Unmeasured
        | BudgetSpend::AtZeroBudget
        | BudgetSpend::CreditAgainstZeroBudget { .. }
        | BudgetSpend::Incomparable => {}
    }
    Some(block)
}

// MARK: - Billing alerts (#770)

/// `11-21`: the day a rule row is about, in UTC like the report's own dates.
fn short_day(day: NaiveDate) -> String {
    format!("{:02}-{:02}", day.month(), day.day())
}

/// `4.3×`.
fn multiple_text(multiple: f64) -> String {
    format!("{multiple:.1}\u{d7}")
}

fn alert_row(id: String, label: String, value: String, detail: String, tint: u32) -> Value {
    json!({
        "id": id,
        "label": label,
        "value": value,
        "detail": detail,
        "color": color::hex(tint),
    })
}

/// The rows the two rules produce for a usage report through `through`.
///
/// Evaluated here, at view time, from the retained fold and the settings handed
/// in, so a save re-flags the tile without waiting for a poll. A tripped rule is
/// a red row; a rule that could not be judged is a muted row that says so
/// (never a missing row, which would read as "no spike").
fn alert_rows(
    hosted: &HostedDays,
    previous: &PreviousMonth,
    through: DateTime<Utc>,
    alerts: &ActionsBillingAlerts,
) -> Vec<Value> {
    let through = through.date_naive();
    let reason = match previous {
        PreviousMonth::Failed(error) => error.user_message(),
        PreviousMonth::NotNeeded | PreviousMonth::Read(_) => String::new(),
    };
    let unknown_days = |days: &[NaiveDate]| {
        days.iter()
            .map(|d| short_day(*d))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut rows = Vec::new();

    let spikes = actionsbilling::spike_flags(
        hosted,
        previous,
        through,
        SpikeThresholds {
            multiple: alerts.spike_multiple,
            floor_minutes: f64::from(alerts.spike_floor_minutes),
        },
    );
    for flag in &spikes.flags {
        let ratio = flag
            .multiple
            .map_or_else(|| "from 0".to_owned(), multiple_text);
        rows.push(alert_row(
            format!("spike:{}:{}", flag.repository, flag.day),
            format!("SPIKE \u{b7} {}", flag.repository),
            format!("{} \u{b7} {ratio}", minutes(flag.minutes)),
            format!(
                "on {} against a 7-day average of {}",
                short_day(flag.day),
                minutes(flag.baseline_avg)
            ),
            color::RED,
        ));
    }
    if !spikes.unknown_days.is_empty() {
        rows.push(alert_row(
            "spike-unknown".to_owned(),
            "SPIKE CHECK".to_owned(),
            UNKNOWN.to_owned(),
            format!(
                "7-day baseline unknown for {}: the previous month's usage could not be read. {reason}",
                unknown_days(&spikes.unknown_days)
            ),
            color::MUTED,
        ));
    }

    let macos = actionsbilling::macos_flags(hosted, previous, through, &alerts.self_hosted_repos);
    for flag in &macos.flags {
        rows.push(alert_row(
            format!("macos:{}:{}", flag.repository, flag.day),
            format!("MACOS ON SELF-HOSTED \u{b7} {}", flag.repository),
            format!("{} on {}", minutes(flag.minutes), short_day(flag.day)),
            "hosted macOS minutes on a repo listed as self-hosted only".to_owned(),
            color::RED,
        ));
    }
    if !macos.unknown_days.is_empty() {
        rows.push(alert_row(
            "macos-unknown".to_owned(),
            "MACOS CHECK".to_owned(),
            UNKNOWN.to_owned(),
            format!(
                "macOS minutes unknown for {}: the previous month's usage could not be read. {reason}",
                unknown_days(&macos.unknown_days)
            ),
            color::MUTED,
        ));
    }
    rows
}

/// Configure's **Billing alerts** section. Shared by every Actions billing tile,
/// so it says so and every change saves on its own, never with the tile's Apply.
/// Every string is made here; the frontend only paints.
#[must_use]
pub fn alerts_section(alerts: &ActionsBillingAlerts) -> Value {
    json!({
        "heading": "Billing alerts",
        "shared": "Shared \u{b7} saved immediately \u{b7} applies to every Actions billing tile, its Detail and its Full view.",
        "help": "A repo's day is a spike when it has at least the floor in hosted minutes and more than the multiple times its average over the 7 days before that day. The newest day in the usage report (which may still be partial) and the day before it are checked, so a flag clears by itself after about two days. A repo on the self-hosted list (the bare repo name as the usage report gives it, not owner/repo) is flagged when it has any hosted macOS minutes on those two days; an empty list turns that rule off. Each tripped rule is a red row and raises Needs Attention; no desktop notification is sent. A change saves when you leave the field.",
        "fields": [
            {"id": "spikeMultiple", "kind": "number", "step": "any",
             "label": "Spike multiple (times the 7-day average)",
             "value": alerts.spike_multiple},
            {"id": "spikeFloorMinutes", "kind": "number", "step": "1",
             "label": "Spike floor (hosted minutes in a day)",
             "value": alerts.spike_floor_minutes},
            {"id": "selfHostedRepos", "kind": "text", "step": null,
             "label": "Self-hosted repos (bare names such as headroom, no owner; flag hosted macOS minutes)",
             "value": alerts.self_hosted_text()},
        ],
    })
}

/// Saves the Billing alerts from Configure. Refuses in one sentence per case and
/// writes nothing; a failed write restores what was there.
///
/// # Errors
/// The refusal's sentence, or the store's error.
pub fn save_alerts(store: &mut store::Store, input: &Value) -> Result<(), String> {
    let parsed = ActionsBillingAlerts::parse(input).map_err(str::to_owned)?;
    let previous = std::mem::replace(&mut store.settings_mut().actions_billing_alerts, parsed);
    if let Err(error) = store.save() {
        store.settings_mut().actions_billing_alerts = previous;
        return Err(error.to_string());
    }
    Ok(())
}

/// The panel payload.
///
/// `cadence_secs` is the operator's cadence for this panel, read at render time
/// so a cadence save re-dates the card now rather than on the next pass. Both
/// staleness judgements are measured against it: [`Freshness`] at one cycle, and
/// the footer at one and a half ([`stale_after_for_slow_cadence`]).
#[must_use]
pub fn view(
    state: &ActionsBillingState,
    alerts: &ActionsBillingAlerts,
    cadence_secs: u64,
    now: u64,
) -> Value {
    let kind = PanelKind::ActionsBilling;
    let freshness = |half_updated: Option<u64>, id: &str| {
        attributed_freshness_payload(
            id,
            Freshness::classify(half_updated.map(|at| now.saturating_sub(at)), cadence_secs),
        )
    };
    let usage_fresh = Freshness::classify(
        state.usage.last_updated.map(|at| now.saturating_sub(at)),
        cadence_secs,
    );
    let mut payload = json!({
        "id": kind.id(),
        "title": kind.title(),
        "trailing": "",
        "message": Value::Null,
        "headline": Value::Null,
        "stats": Value::Array(vec![]),
        "budget": Value::Null,
        "budgetNote": Value::Null,
        "dataThrough": Value::Null,
        "alerts": Value::Array(vec![]),
        "trend": Value::Null,
        "trendNote": Value::Null,
        "billingAlerts": alerts_section(alerts),
        "breakdowns": Value::Array(vec![]),
        "footer": Value::Null,
        "freshness": Value::Null,
        "loading": state.is_loading(),
    });

    // Not configured: the one state that may paint a setup instruction, and
    // only once a pass has *observed* the absence.
    if state.usage.configured.is_absent() {
        payload["message"] = json!({
            "text": UNCONFIGURED_MESSAGE,
            "color": color::hex(color::MUTED),
        });
        return payload;
    }

    // The footer and the clock, per half, folded into the header's two lines.
    // A usage failure with no figure behind it is the card's own red sentence,
    // so it is not said twice.
    let stale_after = stale_after_for_slow_cadence(cadence_secs);
    let usage_error_is_the_message = state.usage.summary.is_none();
    let usage_warning = attributed_status_footer(
        USAGE_ID,
        state.usage.last_updated,
        state
            .usage
            .last_error
            .as_deref()
            .filter(|_| !usage_error_is_the_message),
        now,
        stale_after,
    );
    // One credential failing both reads says one thing, once.
    let budget_error = state
        .budget
        .last_error
        .as_deref()
        .filter(|error| state.usage.last_error.as_deref() != Some(*error));
    let budget_warning = attributed_status_footer(
        BUDGET_ID,
        state.budget.last_updated,
        budget_error,
        now,
        stale_after,
    );
    payload["footer"] = merged_footer(&[usage_warning, budget_warning]);
    payload["freshness"] = merged_freshness(&[
        freshness(state.usage.last_updated, USAGE_ID),
        freshness(state.budget.last_updated, BUDGET_ID),
    ]);

    // The budget half stands on its own: a failed usage read must not hide it.
    if let Some(reading) = state.budget.summary.as_ref() {
        match &reading.status {
            BudgetStatus::Absent => {
                payload["budgetNote"] = json!({
                    "text": NO_BUDGET_MESSAGE,
                    "color": color::hex(color::MUTED),
                });
            }
            BudgetStatus::Present { .. } => {
                payload["budget"] = budget_block(reading).unwrap_or(Value::Null);
            }
        }
    }

    // The usage half, in the ladder's order.
    let Some(summary) = state.usage.summary.as_ref() else {
        payload["message"] = match state.usage.last_error.as_deref() {
            // Red, and the failure named: this one *is* broken.
            Some(error) => json!({"text": error, "color": color::hex(color::RED)}),
            None => json!({
                "text": if state.is_loading() { LOADING_MESSAGE } else { NO_DATA_MESSAGE },
                "color": color::hex(color::MUTED),
            }),
        };
        return payload;
    };

    match summary {
        UsageSummary::NoUsage => {
            // A measurement, said in words. Not `0`, not `$0.00`.
            payload["message"] = json!({
                "text": NO_USAGE_MESSAGE,
                "color": color::hex(color::MUTED),
            });
        }
        UsageSummary::Measured {
            total,
            by_sku,
            top_repos,
            data_through,
            hosted,
            ..
        } => {
            let period = state.period.map_or_else(String::new, month_year);
            payload["trailing"] = json!(format!(
                "{} \u{b7} {}",
                minutes(total.minutes),
                usd(total.net)
            ));
            payload["headline"] = json!({
                "value": usd(total.net),
                // Dimmed once the reading is older than one cycle: the figure
                // is real and stays on the card, but must not be painted like a
                // current one.
                "valueColor": color::hex(if usage_fresh.is_live() { color::GREEN } else { color::GREEN_DIM }),
                "caption": format!("net Actions spend \u{b7} {period} to date"),
                "captionColor": color::hex(color::MUTED),
            });
            payload["stats"] = json!([{
                "label": "HOSTED MINUTES",
                "value": minutes(total.minutes),
                "valueColor": color::hex(color::INK),
            }]);
            // The report's own clock: plain words, outside both header lines.
            payload["dataThrough"] = json!({
                "text": format!("usage through {}", through(*data_through)),
                "color": color::hex(color::MUTED),
            });

            let mut breakdowns: Vec<Value> = Vec::new();
            if !by_sku.is_empty() {
                breakdowns.push(json!({
                    "title": "BY SKU",
                    "rows": breakdown_rows(by_sku.iter().map(|s| (s.sku.clone(), s.totals))),
                }));
            }
            if !top_repos.is_empty() {
                breakdowns.push(json!({
                    "title": "TOP REPOS",
                    "rows": breakdown_rows(top_repos.iter().take(TOP_ROWS).map(|r| {
                        (
                            r.repository
                                .clone()
                                .unwrap_or_else(|| NO_REPOSITORY.to_owned()),
                            r.totals,
                        )
                    })),
                }));
            }
            payload["breakdowns"] = json!(breakdowns);
            payload["alerts"] = json!(alert_rows(hosted, &state.previous, *data_through, alerts));
            (payload["trend"], payload["trendNote"]) = trend_view(hosted, *data_through);
        }
    }
    payload
}

// MARK: - Fixtures

/// Which rendering `--dump-actions-billing` should produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fixture {
    /// A normal month: headline, both breakdowns, a budget bar past amber.
    Measured,
    /// A month whose report holds no Actions usage.
    Empty,
    /// A failed read, red, with the failure named.
    Failed,
    /// No token or no org: the setup instruction.
    Unconfigured,
    /// Usage measured, and the org has no Actions budget.
    NoBudget,
    /// Net spend past the budget: the red step, and a budget that does not stop
    /// jobs, so the label's other wording has a fixture too.
    OverBudget,
    /// Net spend over a budget whose amount is 0 (no paid usage) that stops
    /// jobs: the full red bar over `$0.00`.
    ZeroBudgetOver,
    /// One repo's newest day far above its 7-day average: a red `SPIKE` row.
    Spike,
    /// A repo on the self-hosted list with hosted macOS minutes: a red
    /// `MACOS ON SELF-HOSTED` row.
    MacosOnSelfHosted,
    /// The report's newest day is the 1st of the month: no line, and the
    /// sentence that the trend starts on the 2nd.
    FirstDay,
}

/// The fixture's month and the instant it was read, fixed so the file is
/// byte-stable across regenerations. The newest item is a few hours before the
/// read, which is what a lagging report looks like.
pub const FIXTURE_PERIOD: (i32, u32) = (2023, 11);

/// The Billing alerts a fixture is rendered with: the defaults, plus the
/// self-hosted list the macOS fixture needs. Typed in another case than the
/// report spells it, which the rule matches case-insensitively.
#[must_use]
pub fn fixture_alerts(kind: Fixture) -> ActionsBillingAlerts {
    ActionsBillingAlerts {
        self_hosted_repos: if kind == Fixture::MacosOnSelfHosted {
            vec!["Widget-Web".to_owned()]
        } else {
            Vec::new()
        },
        ..ActionsBillingAlerts::default()
    }
}

/// The usage items behind the `Spike` and `MacosOnSelfHosted` fixtures: three
/// repos with a steady 7 days before the report's newest day (Nov 14, 19:00
/// UTC), and on that day either `gadget-api` runs far above its own baseline or
/// `widget-web` runs hosted macOS minutes. Every figure is invented.
fn fixture_items(kind: Fixture) -> Vec<actionsbilling::UsageItem> {
    let item = |date: &str, sku: &str, quantity: f64, repo: &str| actionsbilling::UsageItem {
        date: DateTime::parse_from_rfc3339(date)
            .expect("a valid instant")
            .into(),
        product: "actions".to_owned(),
        sku: sku.to_owned(),
        unit_type: "Minutes".to_owned(),
        quantity,
        gross_amount: quantity * 0.008,
        discount_amount: quantity * 0.002,
        net_amount: quantity * 0.006,
        repository_name: repo.to_owned(),
    };
    let mut items = Vec::new();
    for day in 1..=13 {
        let date = format!("2023-11-{day:02}T00:00:00Z");
        items.push(item(&date, "Actions Linux", 300.0, "gadget-api"));
        items.push(item(&date, "Actions Linux", 200.0, "pipe-fitting"));
        items.push(item(&date, "Actions Linux", 90.0, "widget-web"));
    }
    let newest = "2023-11-14T19:00:00Z";
    items.push(item(newest, "Actions Linux", 220.0, "pipe-fitting"));
    match kind {
        Fixture::Spike => {
            items.push(item(newest, "Actions Linux", 1_500.0, "gadget-api"));
            items.push(item(newest, "Actions Linux", 80.0, "widget-web"));
        }
        _ => {
            items.push(item(newest, "Actions Linux", 320.0, "gadget-api"));
            items.push(item(newest, "Actions Linux", 80.0, "widget-web"));
            items.push(item(newest, "Actions macOS 3-core", 12.0, "widget-web"));
            items.push(item(
                "2023-11-13T00:00:00Z",
                "Actions macOS 3-core",
                4.0,
                "widget-web",
            ));
        }
    }
    items
}

/// One invented Linux item for `repo` on Nov `day` (UTC) of the fixture month.
fn linux_item(day: u32, hour: u32, quantity: f64, repo: &str) -> actionsbilling::UsageItem {
    actionsbilling::UsageItem {
        date: DateTime::parse_from_rfc3339(&format!("2023-11-{day:02}T{hour:02}:00:00Z"))
            .expect("a valid instant")
            .into(),
        product: "actions".to_owned(),
        sku: "Actions Linux".to_owned(),
        unit_type: "Minutes".to_owned(),
        quantity,
        gross_amount: quantity * 0.008,
        discount_amount: quantity * 0.002,
        net_amount: quantity * 0.006,
        repository_name: repo.to_owned(),
    }
}

/// The daily minutes behind the `Measured` fixture's trend, Nov 1 through Nov
/// 14, split over two repos. Nov 9 has no item at all, so the series holds a
/// measured zero. The days sum to the fixture's 7,545 minutes, and none nears
/// the spike floor, so no rule row appears. Every figure is invented.
fn trend_items() -> Vec<actionsbilling::UsageItem> {
    const DAILY: [f64; 14] = [
        540.0, 610.0, 160.0, 120.0, 620.0, 700.0, 650.0, 590.0, 0.0, 630.0, 610.0, 680.0, 820.0,
        815.0,
    ];
    let mut items = Vec::new();
    for (index, minutes) in DAILY.iter().enumerate() {
        if *minutes == 0.0 {
            continue;
        }
        let day = u32::try_from(index).expect("14 days") + 1;
        // The newest day is a few hours before the read, a lagging report.
        let hour = if day == 14 { 19 } else { 0 };
        items.push(linux_item(day, hour, minutes * 0.7, "gadget-api"));
        // Nov 5's second share is an org-level item (no repository), which the
        // line counts as the stat does.
        let second = if day == 5 { "" } else { "pipe-fitting" };
        items.push(linux_item(day, hour, minutes * 0.3, second));
    }
    items
}

/// The `FirstDay` fixture's usage: the report's newest day is Nov 1, so the
/// series has one day and the trend has no line.
fn first_day_items() -> Vec<actionsbilling::UsageItem> {
    vec![
        linux_item(1, 0, 40.0, "gadget-api"),
        linux_item(1, 3, 25.0, "pipe-fitting"),
    ]
}

/// A hand-made state for the offline fixtures. Every figure is invented.
#[must_use]
pub fn fixture_state(kind: Fixture, at: u64) -> ActionsBillingState {
    let mut state = ActionsBillingState::new();
    let totals = |minutes: f64, net: f64| Totals {
        minutes,
        gross: net + minutes * 0.002,
        discount: minutes * 0.002,
        net,
    };
    let sku = |name: &str, minutes: f64, net: f64| actionsbilling::SkuUsage {
        sku: name.to_owned(),
        totals: totals(minutes, net),
    };
    let repo = |name: Option<&str>, minutes: f64, net: f64| actionsbilling::RepoUsage {
        repository: name.map(str::to_owned),
        totals: totals(minutes, net),
    };
    let top_repos = vec![
        repo(Some("gadget-api"), 3_120.0, 18.40),
        repo(Some("pipe-fitting"), 2_240.0, 11.25),
        repo(Some("widget-web"), 1_480.0, 7.90),
        repo(Some("gizmo-docs"), 610.0, 2.15),
        repo(Some("scratch"), 95.0, 0.40),
    ];
    let measured = UsageSummary::Measured {
        total: totals(7_545.0, 40.60),
        by_sku: vec![
            sku("Actions Linux", 5_900.0, 12.30),
            sku("Actions macOS", 1_100.0, 24.00),
            sku("Actions storage", 0.0, 0.80),
            sku("Actions Windows", 545.0, 3.50),
        ],
        by_repo: top_repos.clone(),
        top_repos,
        // 2023-11-14T19:00:00Z, three hours before the fixture's read.
        data_through: DateTime::from_timestamp(1_699_988_400, 0).expect("a valid instant"),
        hosted: HostedDays::from_items(&trend_items()),
    };
    // 40.60 of a 45.00 budget is 90.2%: the amber step, not a flat green bar.
    let budget = BudgetStatus::Present {
        amount: 45.0,
        prevent_further_usage: true,
        spend: BudgetSpend::Fraction {
            spend: 40.60,
            fraction: 40.60 / 45.0,
        },
    };
    let ok = |usage: UsageSummary, budget: BudgetStatus| MonthRead {
        period: FIXTURE_PERIOD,
        usage: Ok(usage),
        previous: PreviousMonth::NotNeeded,
        budget: Ok(budget),
    };
    match kind {
        Fixture::Unconfigured => state.unconfigure(),
        Fixture::Measured => state.record(ok(measured, budget), at),
        Fixture::NoBudget => state.record(ok(measured, BudgetStatus::Absent), at),
        Fixture::OverBudget => state.record(
            ok(
                measured,
                BudgetStatus::Present {
                    amount: 40.0,
                    prevent_further_usage: false,
                    spend: BudgetSpend::Fraction {
                        spend: 40.60,
                        fraction: 40.60 / 40.0,
                    },
                },
            ),
            at,
        ),
        Fixture::ZeroBudgetOver => state.record(
            ok(
                measured,
                BudgetStatus::Present {
                    amount: 0.0,
                    prevent_further_usage: true,
                    spend: BudgetSpend::OverZeroBudget { spend: 40.60 },
                },
            ),
            at,
        ),
        Fixture::Spike | Fixture::MacosOnSelfHosted => state.record(
            ok(
                actionsbilling::summarize_usage(&fixture_items(kind)),
                BudgetStatus::Absent,
            ),
            at,
        ),
        // On the 1st the spike window reaches into October, which a real read
        // fetches; it held nothing for these repos, so no rule row is unknown.
        Fixture::FirstDay => state.record(
            MonthRead {
                period: FIXTURE_PERIOD,
                previous: PreviousMonth::Read(HostedDays::default()),
                usage: Ok(actionsbilling::summarize_usage(&first_day_items())),
                budget: Ok(BudgetStatus::Absent),
            },
            at,
        ),
        Fixture::Empty => state.record(ok(UsageSummary::NoUsage, BudgetStatus::Absent), at),
        Fixture::Failed => {
            state.begin("acme");
            state.record(
                MonthRead {
                    period: FIXTURE_PERIOD,
                    previous: PreviousMonth::NotNeeded,
                    usage: Err(actionsbilling::ActionsBillingError::Forbidden),
                    budget: Err(actionsbilling::ActionsBillingError::Forbidden),
                },
                at,
            );
        }
    }
    state
}

#[cfg(test)]
mod tests;
