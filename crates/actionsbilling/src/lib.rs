//! GitHub org billing reader: month-to-date Actions minutes and spend, and the
//! org's Actions budget.
//!
//! Two reads per pass ([`ActionsBillingClient`]) feed two pure folds
//! ([`summarize_usage`], [`budget_status`], combined by [`summarize`]). The
//! panel that shows them is a later child of #762; this crate is the reader.
//!
//! Rules that run through the crate:
//!
//! **The token is an argument.** A dedicated fine-grained PAT (org resource
//! owner, Organization permissions: Administration: read). Nothing here stores
//! it, and neither `Debug` nor any error carries it.
//!
//! **Nothing reads the wall clock.** `now` is always an argument, so the month
//! and the summary are testable one fixture away.
//!
//! **Unknown is representable.** A month with no Actions item is
//! [`UsageSummary::NoUsage`], not `$0.00`; no budget is
//! [`BudgetStatus::Absent`], not zero; a zero budget has no fraction. The two
//! reads are separate results, so a failed one never hides the other.
//!
//! **Errors classify; `crates/fault` owns the words.** See [`error`].
//!
//! App-only: `agent/` must never resolve this crate.

pub mod client;
pub mod error;
pub mod models;
pub mod summary;

pub use client::{ActionsBillingClient, MonthRead, API_VERSION, DEFAULT_BASE_URL};
pub use error::ActionsBillingError;
pub use models::{
    ActionsSummary, Budget, BudgetStatus, RepoUsage, SkuUsage, Totals, UsageItem, UsageSummary,
};
pub use summary::{budget_status, summarize, summarize_usage, TOP_N};

#[cfg(test)]
mod tests;
