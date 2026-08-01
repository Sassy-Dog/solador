//! The values a cost read produces. Port of
//! `DevCanopy/Services/AzureCost/AzureCostModels.swift`, plus the two cache
//! types from `AzureCostService.swift` (`ExportFingerprint`,
//! `AzureCostFetchResult`).

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Spend for one grouping key (a resource group, or a `meterCategory`), in USD.
///
/// Resource-group names arrive lowercased — Azure treats them
/// case-insensitively and the export can spell the same group both ways — so
/// the name doubles as a stable identity for a keyed list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResourceCost {
    pub name: String,
    pub cost: f64,
}

/// One or more export CSV partitions folded together: the total USD and the two
/// breakdowns, each sorted by cost descending. Produced by
/// [`crate::aggregate_cost_csv`] / [`crate::reader::read_export`] and folded
/// into a [`CostSummary`].
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CostAggregate {
    pub total: f64,
    pub by_resource: Vec<ResourceCost>,
    pub by_type: Vec<ResourceCost>,
}

/// Month-to-date Azure spend (USD), the prior calendar month's total for a
/// month-over-month reference, the MTD figure projected to a full month, and
/// the top resource groups + resource types.
///
/// Unlike Claude usage (a subscription, so USD is hidden), Azure spend *is* the
/// headline figure — this carries dollars.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CostSummary {
    pub spend_mtd: f64,
    /// Best-effort: a missing prior-month export leaves this 0 rather than
    /// failing the whole read.
    pub spend_prior_month: f64,
    /// Computed at fetch time and **frozen** here (see
    /// [`crate::project_monthly_spend`]), so a carried-forward stale snapshot
    /// keeps a correct projection instead of re-extrapolating against a month
    /// it does not cover.
    pub spend_projected: f64,
    pub by_resource: Vec<ResourceCost>,
    pub by_type: Vec<ResourceCost>,
    /// `None` in the normal case — the figures are the current month to date.
    ///
    /// Set to the first instant of the covered month when the read fell back to
    /// the last completed month because the current one has not been exported
    /// yet (the 1st-of-month rollover gap), so the panel can stamp which month
    /// the figures actually cover.
    pub as_of_month: Option<DateTime<Utc>>,
    /// Set only when the whole fetch fails. A missing prior-month export is
    /// best-effort and leaves this `None`, so a partial outage never blanks the
    /// card.
    pub error: Option<String>,
}

/// Identity of the data a summary was built from: the CSV partition paths of
/// the newest run of each export.
///
/// Cost Management writes every run to a fresh timestamped folder, so a changed
/// path set means new data landed. Unchanged, the cheap blob *list* is all a
/// poll needs — no partition bodies, which is the whole point (a poll that
/// re-downloaded every partition every four hours moved ~161 GB/month).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportFingerprint {
    pub mtd: Vec<String>,
    pub prior: Vec<String>,
}

/// A summary plus the fingerprint of the export it came from, so the next poll
/// can compare and skip re-downloading unchanged partitions.
#[derive(Debug, Clone, PartialEq)]
pub struct FetchResult {
    pub summary: CostSummary,
    pub fingerprint: ExportFingerprint,
}
