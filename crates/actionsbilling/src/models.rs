//! Wire shapes of the two billing reads, and the summary folded from them.

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// `{ "usageItems": [ … ] }`.
#[derive(Debug, Deserialize)]
pub(crate) struct UsageReport {
    #[serde(rename = "usageItems")]
    pub(crate) usage_items: Vec<UsageItem>,
}

/// One line of the usage report. Field names are GitHub's.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UsageItem {
    /// RFC 3339; the newest items carry a time.
    pub date: DateTime<Utc>,
    pub product: String,
    pub sku: String,
    #[serde(rename = "unitType")]
    pub unit_type: String,
    pub quantity: f64,
    #[serde(rename = "grossAmount")]
    pub gross_amount: f64,
    #[serde(rename = "discountAmount")]
    pub discount_amount: f64,
    #[serde(rename = "netAmount")]
    pub net_amount: f64,
    /// Empty for org-level items; kept as its own row, never dropped.
    #[serde(rename = "repositoryName", default)]
    pub repository_name: String,
}

/// `{ "budgets": [ … ], "has_next_page": bool, "total_count": n }`.
#[derive(Debug, Deserialize)]
pub(crate) struct BudgetsPage {
    pub(crate) budgets: Vec<Budget>,
    pub(crate) has_next_page: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Budget {
    #[serde(default)]
    pub id: serde_json::Value,
    #[serde(default)]
    pub budget_type: String,
    pub budget_scope: String,
    #[serde(default)]
    pub budget_entity_name: String,
    pub budget_product_sku: String,
    pub budget_amount: f64,
    pub prevent_further_usage: bool,
}

/// Money and minutes for one slice (the whole month, one SKU or one repo).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Totals {
    /// Hosted minutes: `unitType == "Minutes"` only.
    pub minutes: f64,
    pub gross: f64,
    /// The included-minutes allowance.
    pub discount: f64,
    /// What is billed.
    pub net: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkuUsage {
    pub sku: String,
    pub totals: Totals,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RepoUsage {
    /// `None` is the row for items with an empty `repositoryName`.
    pub repository: Option<String>,
    pub totals: Totals,
}

/// The month's Actions usage, or the statement that the read held none.
#[derive(Debug, Clone, PartialEq)]
pub enum UsageSummary {
    /// The report read fine and holds no `actions` item. Not `$0.00`.
    NoUsage,
    Measured {
        total: Totals,
        /// Sorted by SKU name.
        by_sku: Vec<SkuUsage>,
        /// Every repo row, minutes descending then name.
        by_repo: Vec<RepoUsage>,
        /// The most minutes first, at most [`crate::TOP_N`], zero-minute rows
        /// (storage only) excluded.
        top_repos: Vec<RepoUsage>,
        /// The newest Actions item's `date`: the report's own clock.
        data_through: DateTime<Utc>,
    },
}

impl UsageSummary {
    #[must_use]
    pub fn total(&self) -> Option<&Totals> {
        match self {
            UsageSummary::NoUsage => None,
            UsageSummary::Measured { total, .. } => Some(total),
        }
    }
}

/// The org's Actions budget, if it has one.
#[derive(Debug, Clone, PartialEq)]
pub enum BudgetStatus {
    /// No organization-scoped `actions` budget. Not zero, not an error.
    Absent,
    Present {
        amount: f64,
        prevent_further_usage: bool,
        /// Net Actions spend over `amount`. `None` when the amount is not
        /// positive or the spend is unmeasured.
        fraction: Option<f64>,
    },
}

/// Both folds for one month, from already-parsed inputs.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionsSummary {
    /// The `(year, month)` of `now`, which the report was asked for.
    pub period: (i32, u32),
    pub usage: UsageSummary,
    pub budget: BudgetStatus,
}
