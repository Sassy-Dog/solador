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
//! fraction was measured against, so a retained bar and a fresher headline can
//! never be printed as one ratio.
//!
//! ## States that must not be conflated
//!
//! **No token or no org** is a muted setup sentence: nothing is wrong. **A failed
//! read** is red and is the [`Fault`](fault::Fault) sentence #768 classified.
//! **No Actions usage this month** is a measurement and says so in words, not
//! `0` or `$0.00`. **No Actions budget** is [`BudgetStatus::Absent`] and says
//! so, with no bar: it is never a 0% bar and never a bar against a defaulted
//! zero.

use actionsbilling::{BudgetStatus, MonthRead, Totals, UsageSummary};
use chrono::{DateTime, Datelike, Timelike, Utc};
use serde_json::{json, Value};
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

/// What the budget half holds: the status, and the net spend its fraction was
/// measured against. Kept together so a retained bar is never painted beside a
/// newer headline as if they were one ratio.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetReading {
    pub status: BudgetStatus,
    pub spend: Option<f64>,
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
    /// Whether the last pass failed in a way a retry can fix: the request never
    /// completed, or GitHub itself failed. Never a rejected token, a wrong org
    /// or a rate limit, which a fast retry would only repeat.
    transient_failure: bool,
    /// The organization the retained figures were read for. A pass for another
    /// organization drops them, so a failed first read for org B never shows
    /// org A's headline, bar or attention flag.
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
        self.transient_failure = false;
        self.org = None;
    }

    /// A token and `org` are configured and a read is in flight. A different
    /// organization than the retained figures came from clears both halves
    /// first, as [`unconfigure`](Self::unconfigure) does.
    pub fn begin(&mut self, org: &str) {
        if self.org.as_deref().is_some_and(|kept| kept != org) {
            self.unconfigure();
        }
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
            }
            Err(error) => self.usage.failed(error.user_message()),
        }
        // The spend the crate divided by: this pass's usage, when it read.
        let spend = self.usage_total_if_read_now(&read.budget);
        match read.budget {
            Ok(status) => self
                .budget
                .succeeded(BudgetReading { status, spend }, at, None),
            Err(error) => self.budget.failed(error.user_message()),
        }
    }

    /// The net spend the budget's fraction was computed from: present exactly
    /// when the fraction is, because the crate has no fraction without a spend.
    fn usage_total_if_read_now(
        &self,
        budget: &Result<BudgetStatus, actionsbilling::ActionsBillingError>,
    ) -> Option<f64> {
        let has_fraction = matches!(
            budget,
            Ok(BudgetStatus::Present {
                fraction: Some(_),
                ..
            })
        );
        if !has_fraction {
            return None;
        }
        // The usage half was just written by `record` when it succeeded; a
        // fraction exists only when it did.
        self.usage
            .summary
            .as_ref()
            .and_then(UsageSummary::total)
            .map(|t| t.net)
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
/// spend over the amount, and the bar when there is a fraction to draw.
fn budget_block(reading: &BudgetReading) -> Option<Value> {
    let BudgetStatus::Present {
        amount,
        prevent_further_usage,
        fraction,
    } = &reading.status
    else {
        return None;
    };
    let label = if *prevent_further_usage {
        "BUDGET \u{b7} STOPS JOBS AT LIMIT"
    } else {
        "BUDGET \u{b7} JOBS CONTINUE PAST LIMIT"
    };
    // A spend is printed only beside a fraction that was measured from it; with
    // neither, the numerator is the em dash and there is no bar.
    let spend = fraction
        .and(reading.spend)
        .map_or_else(|| UNKNOWN.to_owned(), usd);
    let mut block = json!({
        "label": label,
        "value": format!("{spend} / {}", usd(*amount)),
        "valueColor": color::hex(color::MUTED),
        "bar": Value::Null,
    });
    if let Some(fraction) = fraction {
        block["bar"] = progress_bar(*fraction, BUDGET_AMBER_AT, BUDGET_RED_AT);
    }
    Some(block)
}

/// The panel payload.
///
/// `cadence_secs` is the operator's cadence for this panel, read at render time
/// so a cadence save re-dates the card now rather than on the next pass. Both
/// staleness judgements are measured against it: [`Freshness`] at one cycle, and
/// the footer at one and a half ([`stale_after_for_slow_cadence`]).
#[must_use]
pub fn view(state: &ActionsBillingState, cadence_secs: u64, now: u64) -> Value {
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
}

/// The fixture's month and the instant it was read, fixed so the file is
/// byte-stable across regenerations. The newest item is a few hours before the
/// read, which is what a lagging report looks like.
pub const FIXTURE_PERIOD: (i32, u32) = (2023, 11);

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
    };
    // 40.60 of a 45.00 budget is 90.2%: the amber step, not a flat green bar.
    let budget = BudgetStatus::Present {
        amount: 45.0,
        prevent_further_usage: true,
        fraction: Some(40.60 / 45.0),
    };
    let ok = |usage: UsageSummary, budget: BudgetStatus| MonthRead {
        period: FIXTURE_PERIOD,
        usage: Ok(usage),
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
                    fraction: Some(40.60 / 40.0),
                },
            ),
            at,
        ),
        Fixture::Empty => state.record(ok(UsageSummary::NoUsage, BudgetStatus::Absent), at),
        Fixture::Failed => {
            state.begin("acme");
            state.record(
                MonthRead {
                    period: FIXTURE_PERIOD,
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
