//! The two org billing reads.
//!
//! Authorization is a bearer token in a header, never in a URL, and the token
//! is held only for the life of the client: this crate takes it as an argument
//! and stores nothing. `Debug` is written by hand so it cannot print it.

use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Datelike, Utc};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

use crate::error::ActionsBillingError;
use crate::models::{Budget, BudgetStatus, BudgetsPage, UsageItem, UsageReport, UsageSummary};
use crate::summary::{budget_status, summarize_usage};

pub const DEFAULT_BASE_URL: &str = "https://api.github.com";
/// The version `crates/github` sends.
pub const API_VERSION: &str = "2022-11-28";

/// Budget pages read before the listing is refused as truncated.
const MAX_BUDGET_PAGES: u32 = 10;
const BUDGETS_PER_PAGE: &str = "100";

/// Everything outside RFC 3986's unreserved set, so a `/` in the org stays one
/// segment rather than retargeting the request (#225 fixed this for Neon).
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// One month's reads. They are separate results on purpose: a failed budget
/// read never hides a successful usage read, or the reverse.
#[derive(Debug, Clone, PartialEq)]
pub struct MonthRead {
    pub period: (i32, u32),
    pub usage: Result<UsageSummary, ActionsBillingError>,
    /// The spend is [`BudgetSpend::Unmeasured`](crate::BudgetSpend::Unmeasured)
    /// whenever the usage read failed or held no Actions item.
    pub budget: Result<BudgetStatus, ActionsBillingError>,
}

pub struct ActionsBillingClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl fmt::Debug for ActionsBillingClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActionsBillingClient")
            .field("base_url", &self.base_url)
            .field("token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl ActionsBillingClient {
    /// `None` when the token is blank: no client, not an error from one.
    #[must_use]
    pub fn new(token: &str) -> Option<Self> {
        Self::with_base_url(DEFAULT_BASE_URL, token)
    }

    /// # Invariant: no credentials in `base_url`
    ///
    /// Scheme/host/port only. Exists so tests can point at a stub server.
    #[must_use]
    pub fn with_base_url(base_url: &str, token: &str) -> Option<Self> {
        if token.trim().is_empty() {
            return None;
        }
        Some(ActionsBillingClient {
            base_url: base_url.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
        })
    }

    /// Both reads for `now`'s month, folded.
    pub async fn month_to_date(&self, org: &str, now: DateTime<Utc>) -> MonthRead {
        let usage = self
            .usage_items(org, now.year(), now.month())
            .await
            .map(|items| summarize_usage(&items));
        let budget = self.budgets(org).await.map(|budgets| {
            let spend = usage.as_ref().ok().and_then(|u| u.total()).map(|t| t.net);
            budget_status(&budgets, spend)
        });
        MonthRead {
            period: (now.year(), now.month()),
            usage,
            budget,
        }
    }

    /// `GET /organizations/{org}/settings/billing/usage?year=&month=`.
    ///
    /// # Errors
    /// [`ActionsBillingError`], classified.
    pub async fn usage_items(
        &self,
        org: &str,
        year: i32,
        month: u32,
    ) -> Result<Vec<UsageItem>, ActionsBillingError> {
        let body = self
            .get(
                org,
                "usage",
                &[("year", year.to_string()), ("month", month.to_string())],
            )
            .await?;
        let report: UsageReport =
            serde_json::from_str(&body).map_err(|_| ActionsBillingError::Undecodable)?;
        Ok(report.usage_items)
    }

    /// `GET /organizations/{org}/settings/billing/budgets`, every page.
    ///
    /// # Errors
    /// [`ActionsBillingError::BudgetsTruncated`] when the listing still claims a
    /// next page after [`MAX_BUDGET_PAGES`] pages; otherwise as for
    /// [`usage_items`](Self::usage_items).
    pub async fn budgets(&self, org: &str) -> Result<Vec<Budget>, ActionsBillingError> {
        let mut all = Vec::new();
        for page in 1..=MAX_BUDGET_PAGES {
            let body = self
                .get(
                    org,
                    "budgets",
                    &[
                        ("page", page.to_string()),
                        ("per_page", BUDGETS_PER_PAGE.to_owned()),
                    ],
                )
                .await?;
            let parsed: BudgetsPage =
                serde_json::from_str(&body).map_err(|_| ActionsBillingError::Undecodable)?;
            all.extend(parsed.budgets);
            if !parsed.has_next_page {
                return Ok(all);
            }
        }
        Err(ActionsBillingError::BudgetsTruncated)
    }

    async fn get(
        &self,
        org: &str,
        leaf: &str,
        query: &[(&str, String)],
    ) -> Result<String, ActionsBillingError> {
        // `url` resolves `.` and `..` segments even when percent-encoded, so
        // these would walk out of `/organizations/`. No such org exists.
        if matches!(org.trim(), "" | "." | "..") {
            return Err(ActionsBillingError::NotFound);
        }
        let org = utf8_percent_encode(org, PATH_SEGMENT);
        let response = self
            .http
            .get(format!(
                "{}/organizations/{org}/settings/billing/{leaf}",
                self.base_url
            ))
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header(reqwest::header::USER_AGENT, "solador")
            .header("X-GitHub-Api-Version", API_VERSION)
            .query(query)
            .send()
            .await
            .map_err(|_| ActionsBillingError::Unreachable)?;
        let status = response.status().as_u16();
        let rate_exhausted = response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim() == "0");
        if let Some(err) = classify(status, rate_exhausted) {
            return Err(err);
        }
        response
            .text()
            .await
            .map_err(|_| ActionsBillingError::Unreachable)
    }
}

/// `None` for a 2xx.
fn classify(status: u16, rate_exhausted: bool) -> Option<ActionsBillingError> {
    match status {
        200..=299 => None,
        401 => Some(ActionsBillingError::Unauthorized),
        403 if rate_exhausted => Some(ActionsBillingError::RateLimited),
        403 => Some(ActionsBillingError::Forbidden),
        404 => Some(ActionsBillingError::NotFound),
        429 => Some(ActionsBillingError::RateLimited),
        500..=599 => Some(ActionsBillingError::VendorFailure { status }),
        _ => Some(ActionsBillingError::UnexpectedStatus { status }),
    }
}
