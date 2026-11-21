//! Per-repo, per-day hosted minutes, and the two rules that read them (#770).
//!
//! Pure: no clock, no network. The report's own newest day (`through`) is an
//! argument, so every case is testable one fixture away.
//!
//! **Unknown is representable.** A spike needs the repo's average over the 7
//! days before the day it judges. Early in a month that window reaches into the
//! previous month, which is read as its own result ([`PreviousMonth`]). When
//! that read failed, the affected day is in [`SpikeReport::unknown_days`]: it is
//! never judged against a shortened window and the missing days are never
//! treated as 0. Inside the days that *were* read, a repo with no item on a day
//! is a measured 0.

use std::collections::BTreeMap;

use chrono::{Datelike, Days, NaiveDate};

use crate::error::ActionsBillingError;
use crate::models::UsageItem;

/// The trailing window a spike is judged against. Fixed, not a setting.
pub const SPIKE_WINDOW_DAYS: u64 = 7;

/// How many of the report's newest days the rules judge: the newest (which may
/// still be partial) and the one before it.
const CHECKED_DAYS: u64 = 2;

/// Hosted minutes of one repo on one day.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DayMinutes {
    /// Every SKU, `unitType == "Minutes"` only.
    pub hosted: f64,
    /// The share of [`hosted`](Self::hosted) whose SKU names macOS.
    pub macos: f64,
}

/// A month's hosted minutes by `(day, repository)`. Items with no repository
/// are org-level and belong to no repo, so they are not in the per-repo map the
/// rules read; they are kept apart, by day, so the daily trend still counts them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostedDays {
    days: BTreeMap<(NaiveDate, String), DayMinutes>,
    org_level: BTreeMap<NaiveDate, f64>,
}

fn is_macos(sku: &str) -> bool {
    sku.to_ascii_lowercase().contains("macos")
}

impl HostedDays {
    /// Fold usage items. Only `product == "actions"` and `Minutes` items count,
    /// so `Actions storage` (`GigabyteHours`) never becomes minutes.
    #[must_use]
    pub fn from_items(items: &[UsageItem]) -> Self {
        let mut days: BTreeMap<(NaiveDate, String), DayMinutes> = BTreeMap::new();
        let mut org_level: BTreeMap<NaiveDate, f64> = BTreeMap::new();
        for item in items {
            if item.product != "actions"
                || item.unit_type != "Minutes"
                || !item.quantity.is_finite()
            {
                continue;
            }
            if item.repository_name.is_empty() {
                *org_level.entry(item.date.date_naive()).or_default() += item.quantity;
                continue;
            }
            let entry = days
                .entry((item.date.date_naive(), item.repository_name.clone()))
                .or_default();
            entry.hosted += item.quantity;
            if is_macos(&item.sku) {
                entry.macos += item.quantity;
            }
        }
        HostedDays { days, org_level }
    }

    /// The repo's minutes on `day`; 0 for a day it has no item on.
    fn on(&self, day: NaiveDate, repo: &str) -> DayMinutes {
        self.days
            .get(&(day, repo.to_owned()))
            .copied()
            .unwrap_or_default()
    }

    pub(crate) fn repos_on(&self, day: NaiveDate) -> impl Iterator<Item = (&str, DayMinutes)> {
        self.days
            .range((day, String::new())..)
            .take_while(move |((d, _), _)| *d == day)
            .map(|((_, repo), minutes)| (repo.as_str(), *minutes))
    }
}

/// The month's total hosted minutes per day, for the Full view's trend (#771).
#[derive(Debug, Clone, PartialEq)]
pub enum DailyTrend {
    /// One value per day from `first` (the 1st) through `last` (the report's
    /// newest day), inclusive. A day with no item is a measured 0; no day after
    /// `last` is in it.
    Series {
        first: NaiveDate,
        last: NaiveDate,
        minutes: Vec<f64>,
    },
    /// The report's newest day is the 1st: a line needs two days, so none is
    /// drawn and the day it starts on is the 2nd.
    StartsOnSecond,
}

impl HostedDays {
    /// Total hosted minutes per day, every repo and the org-level items summed, from the 1st of
    /// `through`'s month through `through`. Storage is never in the fold, so it
    /// never counts. `through` is the report's own newest day, an argument and
    /// never a clock.
    #[must_use]
    pub fn daily_total(&self, through: NaiveDate) -> DailyTrend {
        let first = month_start(through);
        let days = through.day();
        if days < 2 {
            return DailyTrend::StartsOnSecond;
        }
        let minutes = (0..u64::from(days))
            .map(|offset| {
                first
                    .checked_add_days(Days::new(offset))
                    .map_or(0.0, |day| {
                        let repos: f64 = self.repos_on(day).map(|(_, m)| m.hosted).sum();
                        repos + self.org_level.get(&day).copied().unwrap_or(0.0)
                    })
            })
            .collect();
        DailyTrend::Series {
            first,
            last: through,
            minutes,
        }
    }
}

/// What the previous month's read came to, for a window that crosses a month
/// start. Its own state beside the current month's, so a failure here never
/// hides that month or the budget.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum PreviousMonth {
    /// Both checked days' windows lie inside the current month: nothing to read.
    #[default]
    NotNeeded,
    Read(HostedDays),
    /// The window needed it and the read failed: the baseline is unknown.
    Failed(ActionsBillingError),
}

fn month_start(day: NaiveDate) -> NaiveDate {
    day.with_day(1).unwrap_or(day)
}

/// The days the rules judge for a report through `through`, newest first.
fn checked_days(through: NaiveDate) -> impl Iterator<Item = NaiveDate> {
    (0..CHECKED_DAYS).filter_map(move |back| through.checked_sub_days(Days::new(back)))
}

/// Whether the current month's report alone holds every day the rules need: the
/// oldest day of any window is the day before the older checked day's window.
#[must_use]
pub fn needs_previous_month(through: NaiveDate) -> bool {
    through
        .checked_sub_days(Days::new(CHECKED_DAYS - 1 + SPIKE_WINDOW_DAYS))
        .is_some_and(|oldest| oldest < month_start(through))
}

/// The `(year, month)` before the one `through` is in.
#[must_use]
pub fn previous_period(through: NaiveDate) -> (i32, u32) {
    if through.month() == 1 {
        (through.year() - 1, 12)
    } else {
        (through.year(), through.month() - 1)
    }
}

/// The earliest day the folds actually cover.
fn coverage_start(through: NaiveDate, previous: &PreviousMonth) -> NaiveDate {
    let this = month_start(through);
    match previous {
        PreviousMonth::Read(_) => this
            .checked_sub_days(Days::new(1))
            .map_or(this, month_start),
        PreviousMonth::NotNeeded | PreviousMonth::Failed(_) => this,
    }
}

/// The repo's minutes on `day` across both months.
fn on(current: &HostedDays, previous: &PreviousMonth, day: NaiveDate, repo: &str) -> DayMinutes {
    let mut found = current.on(day, repo);
    if let PreviousMonth::Read(prior) = previous {
        let other = prior.on(day, repo);
        found.hosted += other.hosted;
        found.macos += other.macos;
    }
    found
}

/// Every repo with an item on `day`, across both months.
fn repos_on(current: &HostedDays, previous: &PreviousMonth, day: NaiveDate) -> Vec<String> {
    let mut repos: Vec<String> = current.repos_on(day).map(|(r, _)| r.to_owned()).collect();
    if let PreviousMonth::Read(prior) = previous {
        repos.extend(prior.repos_on(day).map(|(r, _)| r.to_owned()));
    }
    repos.sort();
    repos.dedup();
    repos
}

// MARK: - Spike

/// The spike rule's two settings. The 7-day window is not one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpikeThresholds {
    /// A day spikes when it is more than this many times the 7-day average.
    pub multiple: f64,
    /// ... and at least this many hosted minutes.
    pub floor_minutes: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpikeFlag {
    pub repository: String,
    pub day: NaiveDate,
    pub minutes: f64,
    /// The repo's average over the 7 days before `day`.
    pub baseline_avg: f64,
    /// `minutes / baseline_avg`; `None` when the baseline is 0, where no ratio
    /// exists to state.
    pub multiple: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpikeReport {
    /// Newest day first, then most minutes.
    pub flags: Vec<SpikeFlag>,
    /// Checked days whose 7-day window could not be read: not "no spike".
    pub unknown_days: Vec<NaiveDate>,
}

/// Judge the report's newest day and the day before it.
///
/// A repo's day spikes when its hosted minutes are at least the floor AND more
/// than `multiple` times its average over the 7 days before that day. Exactly
/// `multiple` times does not.
#[must_use]
pub fn spike_flags(
    current: &HostedDays,
    previous: &PreviousMonth,
    through: NaiveDate,
    thresholds: SpikeThresholds,
) -> SpikeReport {
    let covered_from = coverage_start(through, previous);
    let mut report = SpikeReport::default();
    for day in checked_days(through) {
        let window: Option<Vec<NaiveDate>> = (1..=SPIKE_WINDOW_DAYS)
            .map(|back| day.checked_sub_days(Days::new(back)))
            .collect();
        let readable = day >= covered_from
            && window
                .as_ref()
                .is_some_and(|w| w.iter().all(|d| *d >= covered_from));
        let Some(window) = window.filter(|_| readable) else {
            report.unknown_days.push(day);
            continue;
        };
        for repo in repos_on(current, previous, day) {
            let minutes = on(current, previous, day, &repo).hosted;
            if !minutes.is_finite() || minutes < thresholds.floor_minutes {
                continue;
            }
            let prior: f64 = window
                .iter()
                .map(|d| on(current, previous, *d, &repo).hosted)
                .sum();
            // Compared without dividing, so "exactly 3x" is exact.
            let window_days = SPIKE_WINDOW_DAYS as f64;
            if minutes * window_days > thresholds.multiple * prior {
                let baseline_avg = prior / window_days;
                report.flags.push(SpikeFlag {
                    repository: repo,
                    day,
                    minutes,
                    baseline_avg,
                    multiple: (baseline_avg > 0.0).then(|| minutes / baseline_avg),
                });
            }
        }
    }
    report.flags.sort_by(|a, b| {
        b.day
            .cmp(&a.day)
            .then_with(|| b.minutes.total_cmp(&a.minutes))
            .then_with(|| a.repository.cmp(&b.repository))
    });
    report
}

// MARK: - macOS on self-hosted

#[derive(Debug, Clone, PartialEq)]
pub struct MacosFlag {
    /// As the usage report names it.
    pub repository: String,
    pub day: NaiveDate,
    pub minutes: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MacosReport {
    /// Newest day first, then most minutes.
    pub flags: Vec<MacosFlag>,
    /// Checked days the fold does not cover (the previous month failed to
    /// read): not "no macOS minutes".
    pub unknown_days: Vec<NaiveDate>,
}

fn same_repo(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// Flag a repo on `self_hosted` with any hosted macOS minutes on the report's
/// newest day or the day before. Names match `repositoryName` case-insensitively
/// after trimming. An empty list is the rule off: no flags, nothing unknown.
#[must_use]
pub fn macos_flags(
    current: &HostedDays,
    previous: &PreviousMonth,
    through: NaiveDate,
    self_hosted: &[String],
) -> MacosReport {
    let mut report = MacosReport::default();
    if self_hosted.is_empty() {
        return report;
    }
    let covered_from = coverage_start(through, previous);
    for day in checked_days(through) {
        if day < covered_from {
            report.unknown_days.push(day);
            continue;
        }
        for repo in repos_on(current, previous, day) {
            if !self_hosted.iter().any(|listed| same_repo(listed, &repo)) {
                continue;
            }
            let minutes = on(current, previous, day, &repo).macos;
            if minutes > 0.0 {
                report.flags.push(MacosFlag {
                    repository: repo,
                    day,
                    minutes,
                });
            }
        }
    }
    report.flags.sort_by(|a, b| {
        b.day
            .cmp(&a.day)
            .then_with(|| b.minutes.total_cmp(&a.minutes))
            .then_with(|| a.repository.cmp(&b.repository))
    });
    report
}
