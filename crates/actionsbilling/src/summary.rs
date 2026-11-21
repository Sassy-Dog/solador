//! The pure folds. No clock, no network: `now` is an argument.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Utc};

use crate::hosted::HostedDays;
use crate::models::{
    ActionsSummary, Budget, BudgetSpend, BudgetStatus, RepoUsage, SkuUsage, Totals, UsageItem,
    UsageSummary,
};

/// How many repos the tile names; the rest fold into the totals.
pub const TOP_N: usize = 5;

const MINUTES: &str = "Minutes";

fn add(totals: &mut Totals, item: &UsageItem) {
    if item.unit_type == MINUTES {
        totals.minutes += item.quantity;
    }
    totals.gross += item.gross_amount;
    totals.discount += item.discount_amount;
    totals.net += item.net_amount;
}

/// Fold usage items into the month's Actions summary.
///
/// Only `product == "actions"` counts. Minutes are `Minutes` items only, so
/// `Actions storage` (`GigabyteHours`) never inflates them, while its spend
/// still counts because it is Actions spend.
#[must_use]
pub fn summarize_usage(items: &[UsageItem]) -> UsageSummary {
    let mut total = Totals::default();
    let mut skus: BTreeMap<&str, Totals> = BTreeMap::new();
    let mut repos: BTreeMap<Option<&str>, Totals> = BTreeMap::new();
    let mut through: Option<DateTime<Utc>> = None;

    for item in items.iter().filter(|i| i.product == "actions") {
        add(&mut total, item);
        add(skus.entry(item.sku.as_str()).or_default(), item);
        let repo = (!item.repository_name.is_empty()).then_some(item.repository_name.as_str());
        add(repos.entry(repo).or_default(), item);
        through = Some(through.map_or(item.date, |t| t.max(item.date)));
    }

    let Some(data_through) = through else {
        return UsageSummary::NoUsage;
    };

    let by_sku = skus
        .into_iter()
        .map(|(sku, totals)| SkuUsage {
            sku: sku.to_owned(),
            totals,
        })
        .collect();
    let mut by_repo: Vec<RepoUsage> = repos
        .into_iter()
        .map(|(repo, totals)| RepoUsage {
            repository: repo.map(str::to_owned),
            totals,
        })
        .collect();
    // Minutes descending, then name, so equal figures keep a stable order
    // between polls.
    by_repo.sort_by(|a, b| {
        b.totals
            .minutes
            .total_cmp(&a.totals.minutes)
            .then_with(|| a.repository.cmp(&b.repository))
    });
    let top_repos = by_repo
        .iter()
        .filter(|r| r.totals.minutes > 0.0)
        .take(TOP_N)
        .cloned()
        .collect();

    UsageSummary::Measured {
        total,
        by_sku,
        by_repo,
        top_repos,
        data_through,
        hosted: HostedDays::from_items(items),
    }
}

/// The organization-scoped Actions budget, folded against the month's spend.
///
/// The first match wins. `spend` is net Actions spend, `None` when the usage
/// read failed or held no Actions item.
#[must_use]
pub fn budget_status(budgets: &[Budget], spend: Option<f64>) -> BudgetStatus {
    let Some(b) = budgets
        .iter()
        .find(|b| b.budget_scope == "organization" && b.budget_product_sku == "actions")
    else {
        return BudgetStatus::Absent;
    };
    BudgetStatus::Present {
        amount: b.budget_amount,
        prevent_further_usage: b.prevent_further_usage,
        spend: budget_spend(b.budget_amount, spend),
    }
}

/// Net spend against one amount. A zero amount is never divided into: spend
/// above it is its own state, and a non-finite figure is not a measurement.
fn budget_spend(amount: f64, spend: Option<f64>) -> BudgetSpend {
    let Some(spend) = spend.filter(|s| s.is_finite()) else {
        return BudgetSpend::Unmeasured;
    };
    // An amount so small the quotient overflows is a zero budget for every
    // purpose the tile has: no finite ratio exists to state.
    let fraction = (amount > 0.0)
        .then(|| spend / amount)
        .filter(|f| f.is_finite());
    if let Some(fraction) = fraction {
        BudgetSpend::Fraction { spend, fraction }
    } else if amount >= 0.0 {
        if spend > 0.0 {
            BudgetSpend::OverZeroBudget { spend }
        } else if spend < 0.0 {
            BudgetSpend::CreditAgainstZeroBudget { spend }
        } else {
            BudgetSpend::AtZeroBudget
        }
    } else {
        BudgetSpend::Incomparable
    }
}

/// Both folds from parsed inputs. `now` names the month; it is never read from
/// a clock.
#[must_use]
pub fn summarize(items: &[UsageItem], budgets: &[Budget], now: DateTime<Utc>) -> ActionsSummary {
    let usage = summarize_usage(items);
    let budget = budget_status(budgets, usage.total().map(|t| t.net));
    ActionsSummary {
        period: (now.year(), now.month()),
        usage,
        budget,
    }
}
