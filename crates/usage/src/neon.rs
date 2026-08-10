//! Month-to-date Neon consumption (compute + branch storage) for one
//! organization. Rust port of `DevCanopy/Services/NeonUsage/`.
//!
//! The API key is an *organization* key, scoped to the org's projects rather
//! than to a user account. It travels only in an `Authorization: Bearer`
//! header, never in a URL — the same rule `crates/agentclient` and
//! `crates/github` follow, and for the same reason: `reqwest` attaches request
//! URLs to its errors and does not redact them.

use std::num::NonZeroU32;
use std::time::Duration;

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, SecondsFormat, Utc};
use percent_encoding::utf8_percent_encode;
use serde::Deserialize;

use crate::urlpath::PATH_SEGMENT;

pub const DEFAULT_BASE_URL: &str = "https://console.neon.tech";
/// Path of the org-wide consumption history read, appended to the base URL.
pub const CONSUMPTION_HISTORY_PATH: &str = "/api/v2/consumption_history/v2/projects";

/// Monthly granularity: one bucket per project for the MTD window, which is all
/// the panel renders — and the smallest response that answers the question.
pub const GRANULARITY: &str = "monthly";

const SECONDS_PER_HOUR: f64 = 3600.0;
const BYTES_PER_GIB: f64 = 1024.0 * 1024.0 * 1024.0;

// MARK: - Metric names

/// The exact `metrics` names the consumption endpoint understands. Only the
/// three the panel renders are requested; the endpoint exposes more (instant
/// restore, snapshots, network transfer, extra branches) that are deliberately
/// left out, keeping the response small and the fold unambiguous.
pub mod metric {
    /// Compute burn, in compute-unit seconds. Rendered as CU-hours.
    pub const COMPUTE_UNIT_SECONDS: &str = "compute_unit_seconds";
    /// Storage held by root branches, in byte-months over the queried window.
    pub const ROOT_BRANCH_BYTES_MONTH: &str = "root_branch_bytes_month";
    /// Storage held by child branches, in byte-months over the queried window.
    pub const CHILD_BRANCH_BYTES_MONTH: &str = "child_branch_bytes_month";

    /// The comma-separated `metrics` query parameter this crate sends.
    pub const REQUESTED: &str =
        "compute_unit_seconds,root_branch_bytes_month,child_branch_bytes_month";
}

// MARK: - Summary

/// Month-to-date Neon consumption for one organization, folded across every
/// project the API key can see.
///
/// # Why this is an enum
///
/// Neon omits a metric from the response when its value is zero *for a
/// timeframe it did return* — that is a real zero. But it returns no timeframes
/// at all when the org has no projects, or when the plan does not include
/// consumption history. That second case is genuinely unknown, and the panel
/// must render `—` for it rather than a fabricated 0.
///
/// Splitting the two into variants makes the distinction unrepresentable-wrong
/// instead of merely documented: [`NeonUsageSummary::Unmeasured`] holds no
/// figures to read, and [`NeonUsageSummary::Measured`] cannot exist without at
/// least one project (`NonZeroU32`) having reported a timeframe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NeonUsageSummary {
    /// The API answered, but nothing was measured. Both figures are unknown.
    Unmeasured,
    /// At least one project reported a consumption timeframe.
    Measured {
        /// Compute burn over the window, in compute-unit hours.
        compute_unit_hours: f64,
        /// Branch storage over the window in GiB (root + child branch bytes).
        /// Neon reports byte-months; over this service's one-month window that
        /// reads as the month's storage footprint.
        storage_gib: f64,
        /// How many projects contributed consumption timeframes.
        project_count: NonZeroU32,
    },
}

impl NeonUsageSummary {
    /// Compute burn in CU-hours, or `None` when nothing was measured.
    #[must_use]
    pub fn compute_unit_hours(&self) -> Option<f64> {
        match self {
            NeonUsageSummary::Unmeasured => None,
            NeonUsageSummary::Measured {
                compute_unit_hours, ..
            } => Some(*compute_unit_hours),
        }
    }

    /// Branch storage in GiB, or `None` when nothing was measured.
    #[must_use]
    pub fn storage_gib(&self) -> Option<f64> {
        match self {
            NeonUsageSummary::Unmeasured => None,
            NeonUsageSummary::Measured { storage_gib, .. } => Some(*storage_gib),
        }
    }

    /// How many projects reported a consumption timeframe. Zero exactly when
    /// nothing was measured.
    #[must_use]
    pub fn project_count(&self) -> u32 {
        match self {
            NeonUsageSummary::Unmeasured => 0,
            NeonUsageSummary::Measured { project_count, .. } => project_count.get(),
        }
    }

    /// Whether the API answered successfully but measured nothing — the case
    /// the panel footer explains with [`NO_CONSUMPTION_MESSAGE`].
    #[must_use]
    pub fn is_unmeasured(&self) -> bool {
        matches!(self, NeonUsageSummary::Unmeasured)
    }
}

/// Shown when the API answers successfully but reports no consumption at all —
/// an empty org, the wrong org id, or a plan without consumption history
/// (Launch / Scale / Agent / Enterprise only).
pub const NO_CONSUMPTION_MESSAGE: &str =
    "no Neon consumption reported — check the org ID, or the plan may not include consumption history";

// MARK: - Wire format

/// `GET /api/v2/consumption_history/v2/projects` — the org-wide consumption
/// history response. Nested arrays are decoded leniently (optional, coalesced
/// to empty when folded) because this is an external contract we don't control:
/// a project with no billing period yet must not fail the whole decode.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NeonConsumptionResponse {
    pub projects: Option<Vec<NeonConsumptionProject>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NeonConsumptionProject {
    #[serde(rename = "project_id")]
    pub project_id: Option<String>,
    pub periods: Option<Vec<NeonConsumptionPeriod>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NeonConsumptionPeriod {
    pub consumption: Option<Vec<NeonConsumptionEntry>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NeonConsumptionEntry {
    #[serde(rename = "timeframe_start")]
    pub timeframe_start: Option<String>,
    #[serde(rename = "timeframe_end")]
    pub timeframe_end: Option<String>,
    pub metrics: Option<Vec<NeonConsumptionMetric>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NeonConsumptionMetric {
    #[serde(rename = "metric_name")]
    pub metric_name: String,
    pub value: f64,
}

// MARK: - Invoices

/// `GET /api/v2/organizations/{org_id}/billing/invoices` — **undocumented**:
/// absent from Neon's public OpenAPI spec but served to org API keys (verified
/// live 2026-08-02). Treated as best-effort everywhere: a failure or the
/// endpoint vanishing degrades the panel to estimate-only, never breaks it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NeonInvoicesResponse {
    #[serde(default)]
    pub invoices: Vec<NeonInvoice>,
}

/// One finalized monthly invoice. Only the fields the panel reads; decode
/// tolerates the rest (`pdf_url`, `hosted_invoice_url`, …).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NeonInvoice {
    /// The endpoint serializes money as a decimal string (`"15.91"`, verified
    /// live 2026-08-02) — the usual dodge around binary-float money. A plain
    /// JSON number is accepted too, so a quiet server-side type fix cannot
    /// break the panel a second time.
    #[serde(deserialize_with = "de_money")]
    pub total: f64,
    pub currency: String,
    /// RFC 3339 string, compared lexically — valid for UTC timestamps of one
    /// vendor, and avoids failing an invoice over an unparseable date.
    #[serde(default)]
    pub issued_at: String,
    #[serde(default)]
    pub status: String,
}

/// Money off the invoice wire: a decimal string today, tolerated as a plain
/// number should Neon ever change the serialization. Anything else is a
/// decode error — never a silent zero.
fn de_money<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Money {
        Number(f64),
        Text(String),
    }
    match Money::deserialize(deserializer)? {
        Money::Number(n) => Ok(n),
        Money::Text(s) => s.trim().parse().map_err(serde::de::Error::custom),
    }
}

/// The panel's invoice reading. Two variants for the same reason
/// [`NeonUsageSummary`] has two: "no invoices yet" is a measurement that must
/// stay distinguishable from "we could not ask".
#[derive(Debug, Clone, PartialEq)]
pub enum NeonInvoiceSummary {
    NoInvoices,
    Latest { total: f64, currency: String },
}

/// Newest invoice by `issued_at` — never array order.
#[must_use]
pub fn summarize_invoices(response: &NeonInvoicesResponse) -> NeonInvoiceSummary {
    response
        .invoices
        .iter()
        .max_by(|a, b| a.issued_at.cmp(&b.issued_at))
        .map_or(NeonInvoiceSummary::NoInvoices, |invoice| {
            NeonInvoiceSummary::Latest {
                total: invoice.total,
                currency: invoice.currency.clone(),
            }
        })
}

// MARK: - Errors

/// Failures from reading Neon consumption. [`NeonUsageError::is_auth_failure`]
/// is the signal the panel turns into a "paste a new key" prompt — a revoked or
/// mistyped org API key answers with 401 / 403.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NeonUsageError {
    #[error("Add your Neon org ID in Settings")]
    MissingOrgId,
    #[error("Invalid Neon API URL — check the org ID in Settings")]
    InvalidUrl,
    #[error("Invalid response from the Neon API")]
    InvalidResponse,
    #[error("Neon API request failed (HTTP {status})")]
    Http { status: u16 },
    #[error("Couldn't read the Neon response ({0})")]
    DecodingFailed(String),
    /// No Swift twin: `URLSession` folds transport failures into a generic
    /// `Error` the service reports via `localizedDescription`.
    #[error("Couldn't reach the Neon API ({0})")]
    Unreachable(String),
}

impl NeonUsageError {
    /// True when the failure looks like a bad or revoked API key.
    #[must_use]
    pub fn is_auth_failure(&self) -> bool {
        matches!(self, NeonUsageError::Http { status: 401 | 403 })
    }

    /// The message the panel footer shows. Cause-specific so the operator fixes
    /// the right thing: a 401 is a key to replace, a 404 is an org id to
    /// correct, and everything else names the failure it actually was.
    #[must_use]
    pub fn user_message(&self) -> String {
        if self.is_auth_failure() {
            return "API key invalid — paste a new one in Settings".to_string();
        }
        if matches!(self, NeonUsageError::Http { status: 404 }) {
            return "Neon org not found — check the org ID in Settings".to_string();
        }
        self.to_string()
    }
}

// MARK: - Window + folding

/// The month-to-date window: first instant of the UTC calendar month containing
/// `now`, through the first instant of the following month.
///
/// UTC because Neon bills and buckets in UTC — using local time would slide the
/// boundary by the timezone offset and bill part of one month into another.
///
/// `to` is the month's *exclusive end*, never `now`: under `granularity=monthly`
/// the endpoint floors both bounds to the bucket boundary, so a mid-month `to`
/// comes back equal to `from` and the whole read fails with an HTTP 400
/// ("'from' must be before 'to'") — on every day of the month. A `to` in the
/// future is fine; the returned timeframe still only covers what was consumed.
#[must_use]
pub fn month_to_date_window(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let first_of = |year: i32, month: u32| {
        NaiveDate::from_ymd_opt(year, month, 1)
            .map(|day| DateTime::from_naive_utc_and_offset(day.and_time(NaiveTime::MIN), Utc))
    };
    // Unreachable fallbacks: every valid date's month has a first day, as does
    // the month after it. Mirrors the Swift's own `?? now`.
    let start = first_of(now.year(), now.month()).unwrap_or(now);
    let end = if now.month() == 12 {
        first_of(now.year() + 1, 1)
    } else {
        first_of(now.year(), now.month() + 1)
    }
    .unwrap_or(now);
    (start, end)
}

/// RFC 3339 in UTC, e.g. `2026-07-01T00:00:00Z` — the format the consumption
/// endpoint's `from`/`to` parameters require.
#[must_use]
pub fn rfc3339(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Fold a `consumption_history` response into the panel's summary: sum
/// `compute_unit_seconds` (to CU-hours) and root + child branch bytes (to GiB)
/// across every project and timeframe returned.
///
/// A response carrying no consumption timeframes at all yields
/// [`NeonUsageSummary::Unmeasured`] — the `—` path. That is the plan-gated /
/// no-projects case, and folding it to 0 would be a fabricated number. A metric
/// *missing from a timeframe that was returned* is a documented real zero and
/// simply contributes nothing to the sum.
#[must_use]
pub fn summarize(response: &NeonConsumptionResponse) -> NeonUsageSummary {
    let mut compute_unit_seconds = 0.0;
    let mut storage_bytes = 0.0;
    let mut project_count: u32 = 0;

    for project in response.projects.iter().flatten() {
        let mut saw_timeframe = false;
        for period in project.periods.iter().flatten() {
            for entry in period.consumption.iter().flatten() {
                saw_timeframe = true;
                for metric in entry.metrics.iter().flatten() {
                    match metric.metric_name.as_str() {
                        metric::COMPUTE_UNIT_SECONDS => compute_unit_seconds += metric.value,
                        metric::ROOT_BRANCH_BYTES_MONTH | metric::CHILD_BRANCH_BYTES_MONTH => {
                            storage_bytes += metric.value;
                        }
                        // A metric we didn't ask for — ignored rather than
                        // guessing which bucket it belongs in.
                        _ => {}
                    }
                }
            }
        }
        if saw_timeframe {
            project_count = project_count.saturating_add(1);
        }
    }

    match NonZeroU32::new(project_count) {
        None => NeonUsageSummary::Unmeasured,
        Some(project_count) => NeonUsageSummary::Measured {
            compute_unit_hours: compute_unit_seconds / SECONDS_PER_HOUR,
            storage_gib: storage_bytes / BYTES_PER_GIB,
            project_count,
        },
    }
}

/// Estimated month-to-date charges: consumption × the operator's rates.
///
/// This reproduces the console's own "Charges to date" arithmetic (verified
/// against it 2026-08-02: 5.19 CU-h × $0.106 = $0.55). The rates come from
/// Settings, never a shipped price table — the app must not invent a price.
/// `None` when both rates are unset (≤ 0) or the summary is unmeasured;
/// negative rates count as unset.
#[must_use]
pub fn estimate_usd(
    summary: NeonUsageSummary,
    usd_per_cu_hour: f64,
    usd_per_gib_month: f64,
) -> Option<f64> {
    if usd_per_cu_hour <= 0.0 && usd_per_gib_month <= 0.0 {
        return None;
    }
    let compute = summary.compute_unit_hours()?;
    let storage = summary.storage_gib()?;
    Some(compute * usd_per_cu_hour.max(0.0) + storage * usd_per_gib_month.max(0.0))
}

// MARK: - Client

/// The one Neon REST read the Usage panel needs.
pub struct NeonClient {
    base_url: String,
    api_key: String,
    http: reqwest::Client,
}

impl NeonClient {
    /// Builds a client, or `None` when the key is blank.
    ///
    /// "No key" is modelled as the absence of a client rather than as an error
    /// from one: the panel hides the Neon section entirely when the user has
    /// not configured a key, which is not a failure to report.
    #[must_use]
    pub fn new(api_key: &str) -> Option<Self> {
        Self::with_base_url(DEFAULT_BASE_URL, api_key)
    }

    /// # Invariant: no credentials in `base_url`
    ///
    /// `base_url` must be scheme/host/port only. The API key is a separate
    /// argument and travels only via `bearer_auth()`, which puts it in a
    /// header. URLs leak where headers do not: `reqwest` attaches the request
    /// URL to its errors and does not redact userinfo, so anything embedded
    /// here can reach a log line or an operator-facing string.
    ///
    /// Exists so tests (and only tests) can point the client at a mock server.
    #[must_use]
    pub fn with_base_url(base_url: &str, api_key: &str) -> Option<Self> {
        if api_key.trim().is_empty() {
            return None;
        }
        Some(NeonClient {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
        })
    }

    /// Org-wide consumption history over `[from, to]`, one monthly bucket per
    /// project.
    ///
    /// A blank `org_id` short-circuits to [`NeonUsageError::MissingOrgId`]
    /// without a request: the endpoint requires it, so there is nothing to ask
    /// for, and firing anyway would trade a precise message for whatever Neon
    /// says about a malformed query.
    pub async fn consumption_history(
        &self,
        org_id: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<NeonConsumptionResponse, NeonUsageError> {
        let org_id = org_id.trim();
        if org_id.is_empty() {
            return Err(NeonUsageError::MissingOrgId);
        }

        let resp = self
            .http
            .get(format!("{}{CONSUMPTION_HISTORY_PATH}", self.base_url))
            .query(&[
                ("from", rfc3339(from).as_str()),
                ("to", rfc3339(to).as_str()),
                ("granularity", GRANULARITY),
                ("org_id", org_id),
                ("metrics", metric::REQUESTED),
            ])
            .bearer_auth(&self.api_key)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| NeonUsageError::Unreachable(e.to_string()))?;

        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(NeonUsageError::Http { status });
        }

        let body = resp
            .text()
            .await
            .map_err(|e| NeonUsageError::Unreachable(e.to_string()))?;
        serde_json::from_str(&body).map_err(|e| NeonUsageError::DecodingFailed(e.to_string()))
    }

    /// The month-to-date read the panel makes: window from `now`, fold to a
    /// summary.
    pub async fn month_to_date(
        &self,
        org_id: &str,
        now: DateTime<Utc>,
    ) -> Result<NeonUsageSummary, NeonUsageError> {
        let (from, to) = month_to_date_window(now);
        let response = self.consumption_history(org_id, from, to).await?;
        Ok(summarize(&response))
    }

    /// The org's finalized invoices, folded to the newest.
    ///
    /// Undocumented endpoint (see [`NeonInvoicesResponse`]); callers must treat
    /// every failure as degradation, not breakage.
    pub async fn invoices(&self, org_id: &str) -> Result<NeonInvoiceSummary, NeonUsageError> {
        let org_id = org_id.trim();
        if org_id.is_empty() {
            return Err(NeonUsageError::MissingOrgId);
        }
        // The one request in this crate that puts the org id in a *path* rather
        // than a query parameter, where `reqwest` would encode it. See
        // `crate::urlpath`.
        let org_id = utf8_percent_encode(org_id, PATH_SEGMENT);

        let resp = self
            .http
            .get(format!(
                "{}/api/v2/organizations/{org_id}/billing/invoices",
                self.base_url
            ))
            .bearer_auth(&self.api_key)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| NeonUsageError::Unreachable(e.to_string()))?;

        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(NeonUsageError::Http { status });
        }

        let body = resp
            .text()
            .await
            .map_err(|e| NeonUsageError::Unreachable(e.to_string()))?;
        let response: NeonInvoicesResponse = serde_json::from_str(&body)
            .map_err(|e| NeonUsageError::DecodingFailed(e.to_string()))?;
        Ok(summarize_invoices(&response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // MARK: - Fixtures
    // Twins of `DevCanopyTests/NeonUsageMappingTests.swift`.

    const TWO_PROJECTS: &str = r#"
    {
      "projects": [
        {
          "project_id": "pipe-fitting",
          "periods": [
            {
              "period_id": "p1",
              "period_plan": "scale",
              "period_start": "2026-07-01T00:00:00Z",
              "consumption": [
                {
                  "timeframe_start": "2026-07-01T00:00:00Z",
                  "timeframe_end": "2026-08-01T00:00:00Z",
                  "metrics": [
                    { "metric_name": "compute_unit_seconds", "value": 7200 },
                    { "metric_name": "root_branch_bytes_month", "value": 1073741824 },
                    { "metric_name": "child_branch_bytes_month", "value": 536870912 }
                  ]
                }
              ]
            }
          ]
        },
        {
          "project_id": "gadget",
          "periods": [
            {
              "consumption": [
                {
                  "timeframe_start": "2026-07-01T00:00:00Z",
                  "timeframe_end": "2026-08-01T00:00:00Z",
                  "metrics": [
                    { "metric_name": "compute_unit_seconds", "value": 3600 },
                    { "metric_name": "root_branch_bytes_month", "value": 536870912 },
                    { "metric_name": "public_network_transfer_bytes", "value": 999 }
                  ]
                }
              ]
            }
          ]
        }
      ],
      "pagination": { "cursor": "" }
    }
    "#;

    fn fixture(json: &str) -> NeonConsumptionResponse {
        serde_json::from_str(json).expect("fixture decodes")
    }

    fn utc(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        DateTime::from_naive_utc_and_offset(
            NaiveDate::from_ymd_opt(year, month, day)
                .expect("valid date")
                .and_hms_opt(hour, minute, 0)
                .expect("valid time"),
            Utc,
        )
    }

    // MARK: - summarize

    /// Twin of `testSummarizeSumsMetricsAcrossProjectsAndConvertsUnits`.
    #[test]
    fn sums_metrics_across_projects_and_converts_units() {
        let summary = summarize(&fixture(TWO_PROJECTS));

        // 7200 + 3600 compute-unit seconds = 3 CU-hours.
        assert_eq!(summary.compute_unit_hours(), Some(3.0));
        // 1 GiB + 0.5 GiB on the first project, 0.5 GiB root on the second.
        assert_eq!(summary.storage_gib(), Some(2.0));
        assert_eq!(summary.project_count(), 2);
    }

    /// Twin of `testSummarizeIgnoresMetricsThePanelDidNotRequest`:
    /// `public_network_transfer_bytes` (999) is in the fixture and must not
    /// land in either bucket — guessing which one would fabricate a figure.
    #[test]
    fn ignores_metrics_that_were_not_requested() {
        assert_eq!(summarize(&fixture(TWO_PROJECTS)).storage_gib(), Some(2.0));
    }

    /// Twin of `testSummarizeTreatsAnOmittedMetricInAReturnedTimeframeAsZero`.
    /// Neon documents omitting a metric whose value is zero for a timeframe it
    /// *did* return — that is a real zero, not an unknown.
    #[test]
    fn an_omitted_metric_in_a_returned_timeframe_is_a_real_zero() {
        let json = r#"
        {
          "projects": [
            {
              "project_id": "idle",
              "periods": [
                {
                  "consumption": [
                    {
                      "timeframe_start": "2026-07-01T00:00:00Z",
                      "timeframe_end": "2026-08-01T00:00:00Z",
                      "metrics": [
                        { "metric_name": "compute_unit_seconds", "value": 1800 }
                      ]
                    }
                  ]
                }
              ]
            }
          ]
        }
        "#;
        let summary = summarize(&fixture(json));
        assert_eq!(summary.compute_unit_hours(), Some(0.5));
        assert_eq!(
            summary.storage_gib(),
            Some(0.0),
            "a measured timeframe without the metric is zero storage, not unknown"
        );
        assert_eq!(summary.project_count(), 1);
    }

    /// Twin of `testSummarizeReportsUnknownWhenNoTimeframesComeBack`: the
    /// plan-gated / empty-org shape. Nothing was measured, so both figures must
    /// be unknown — never 0.
    #[test]
    fn no_timeframes_at_all_is_unmeasured_not_zero() {
        let summary = summarize(&fixture(
            r#"{ "projects": [ { "project_id": "gated", "periods": [] } ] }"#,
        ));
        assert_eq!(summary, NeonUsageSummary::Unmeasured);
        assert_eq!(summary.compute_unit_hours(), None);
        assert_eq!(summary.storage_gib(), None);
        assert_eq!(summary.project_count(), 0);
    }

    /// Twin of `testSummarizeReportsUnknownForAnEmptyProjectList`.
    #[test]
    fn an_empty_project_list_is_unmeasured() {
        let summary = summarize(&fixture(r#"{ "projects": [] }"#));
        assert!(summary.is_unmeasured());
        assert_eq!(summary.compute_unit_hours(), None);
    }

    /// Twin of `testDecodeToleratesAMissingProjectsKey`.
    #[test]
    fn a_missing_projects_key_decodes_and_folds_to_unmeasured() {
        let summary = summarize(&fixture("{}"));
        assert_eq!(summary, NeonUsageSummary::Unmeasured);
        assert_eq!(summary.project_count(), 0);
    }

    /// The structural half of the None-vs-0 rule: the unmeasured variant holds
    /// no figures at all, so a `—` cannot decay into a 0, and a measured
    /// summary cannot claim zero projects.
    #[test]
    fn the_unmeasured_variant_carries_no_figures_to_mistake_for_zero() {
        let unmeasured = NeonUsageSummary::Unmeasured;
        assert!(unmeasured.compute_unit_hours().is_none());
        assert!(unmeasured.storage_gib().is_none());

        // A real zero is only expressible as a *measured* summary, which by
        // construction had at least one project report a timeframe.
        let real_zero = NeonUsageSummary::Measured {
            compute_unit_hours: 0.0,
            storage_gib: 0.0,
            project_count: NonZeroU32::new(1).expect("1 is non-zero"),
        };
        assert_eq!(real_zero.compute_unit_hours(), Some(0.0));
        assert_ne!(real_zero, unmeasured);
    }

    /// The query literal must stay in step with the three metric names it is
    /// built from — `concat!` cannot join consts, so this asserts what the
    /// compiler cannot.
    #[test]
    fn the_requested_metrics_parameter_lists_exactly_the_three_metrics() {
        assert_eq!(
            metric::REQUESTED,
            [
                metric::COMPUTE_UNIT_SECONDS,
                metric::ROOT_BRANCH_BYTES_MONTH,
                metric::CHILD_BRANCH_BYTES_MONTH,
            ]
            .join(",")
        );
    }

    /// The footer text for a successful-but-empty read, carried over verbatim
    /// so the panel keeps saying why it shows `—` rather than a 0.
    #[test]
    fn the_no_consumption_message_matches_the_swift_verbatim() {
        assert_eq!(
            NO_CONSUMPTION_MESSAGE,
            "no Neon consumption reported — check the org ID, or the plan may not include consumption history"
        );
    }

    // MARK: - Window + RFC 3339

    /// Twin of `testMonthToDateWindowStartsAtTheFirstInstantOfTheUTCMonth`.
    /// `to` is the month's exclusive end, not `now`: under monthly granularity
    /// Neon floors both bounds to the bucket boundary, so a mid-month `to`
    /// becomes equal to `from` and the API answers 400.
    #[test]
    fn the_window_starts_at_the_first_instant_of_the_utc_month() {
        let (from, to) = month_to_date_window(utc(2026, 7, 31, 18, 45));
        assert_eq!(rfc3339(from), "2026-07-01T00:00:00Z");
        assert_eq!(rfc3339(to), "2026-08-01T00:00:00Z");
    }

    /// Twin of `testMonthToDateWindowUsesUTCNotLocalTime`: 00:30 UTC on the 1st
    /// is still the 1st in UTC; a local-time boundary would slide it back into
    /// the previous month for negative offsets.
    #[test]
    fn the_window_boundary_is_utc_not_local() {
        let (from, _) = month_to_date_window(utc(2026, 1, 1, 0, 30));
        assert_eq!(rfc3339(from), "2026-01-01T00:00:00Z");
    }

    /// December's exclusive end is the next *year's* January — the rollover a
    /// naive `month + 1` would turn into an invalid month 13.
    #[test]
    fn the_window_handles_a_december_start() {
        let (from, to) = month_to_date_window(utc(2026, 12, 25, 6, 0));
        assert_eq!(rfc3339(from), "2026-12-01T00:00:00Z");
        assert_eq!(rfc3339(to), "2027-01-01T00:00:00Z");
    }

    // MARK: - Errors

    /// Twin of `testAuthFailuresAreDetectedFor401And403`.
    #[test]
    fn auth_failures_are_401_and_403_only() {
        assert!(NeonUsageError::Http { status: 401 }.is_auth_failure());
        assert!(NeonUsageError::Http { status: 403 }.is_auth_failure());
        assert!(!NeonUsageError::Http { status: 500 }.is_auth_failure());
        assert!(!NeonUsageError::InvalidResponse.is_auth_failure());
        assert!(!NeonUsageError::MissingOrgId.is_auth_failure());
    }

    /// Twin of `testFriendlyMessageAsksForANewKeyOnAuthFailure`.
    #[test]
    fn an_auth_failure_asks_for_a_new_key() {
        assert_eq!(
            NeonUsageError::Http { status: 403 }.user_message(),
            "API key invalid — paste a new one in Settings"
        );
        assert_eq!(
            NeonUsageError::Http { status: 401 }.user_message(),
            "API key invalid — paste a new one in Settings"
        );
    }

    /// Twin of `testFriendlyMessagePointsAtTheOrgIDOn404`.
    #[test]
    fn a_404_points_at_the_org_id() {
        assert_eq!(
            NeonUsageError::Http { status: 404 }.user_message(),
            "Neon org not found — check the org ID in Settings"
        );
    }

    /// Every other failure names itself, so a 503 and a decode failure don't
    /// send the operator to the same place.
    #[test]
    fn other_failures_name_themselves() {
        assert_eq!(
            NeonUsageError::Http { status: 503 }.user_message(),
            "Neon API request failed (HTTP 503)"
        );
        assert_eq!(
            NeonUsageError::MissingOrgId.user_message(),
            "Add your Neon org ID in Settings"
        );
        assert_eq!(
            NeonUsageError::DecodingFailed("bad token".into()).user_message(),
            "Couldn't read the Neon response (bad token)"
        );
        assert_eq!(
            NeonUsageError::InvalidUrl.user_message(),
            "Invalid Neon API URL — check the org ID in Settings"
        );
    }

    // MARK: - Invoices

    /// Totals are quoted on purpose: the real endpoint serializes money as
    /// decimal strings (`"total": "15.91"`, verified live 2026-08-02), and a
    /// fixture with JSON numbers let a `total: f64` wire type pass every test
    /// while failing against production.
    const FOUR_INVOICES: &str = r#"
    {
      "invoices": [
        { "invoice_number": "A-3", "status": "paid", "issued_at": "2026-07-01T00:10:00Z",
          "due_date": "2026-08-01T00:00:00Z", "total": "15.91", "currency": "USD" },
        { "invoice_number": "A-1", "status": "paid", "issued_at": "2026-05-01T00:10:00Z",
          "due_date": "2026-06-01T00:00:00Z", "total": "21.52", "currency": "USD" },
        { "invoice_number": "A-2", "status": "paid", "issued_at": "2026-06-01T00:10:00Z",
          "due_date": "2026-07-01T00:00:00Z", "total": "22.41", "currency": "USD" }
      ]
    }"#;

    /// Newest by `issued_at`, never by array order — the API's ordering is
    /// not part of any contract we read.
    #[test]
    fn the_latest_invoice_is_picked_by_issued_at_not_array_order() {
        let response: NeonInvoicesResponse = serde_json::from_str(FOUR_INVOICES).expect("decode");
        assert_eq!(
            summarize_invoices(&response),
            NeonInvoiceSummary::Latest {
                total: 15.91,
                currency: "USD".into()
            }
        );
    }

    /// A young org has no invoices; that is a measurement, not a failure.
    #[test]
    fn an_empty_invoice_list_is_no_invoices() {
        let response: NeonInvoicesResponse =
            serde_json::from_str(r#"{"invoices":[]}"#).expect("decode");
        assert_eq!(
            summarize_invoices(&response),
            NeonInvoiceSummary::NoInvoices
        );
    }

    /// Unknown keys (pdf_url, hosted_invoice_url, …) must not break decode.
    /// The plain-number `total` here is deliberate: the wire sends strings
    /// today, but a number must keep decoding if Neon ever changes its mind.
    #[test]
    fn invoice_decode_tolerates_extra_keys_and_a_numeric_total() {
        let response: NeonInvoicesResponse = serde_json::from_str(
            r#"{"invoices":[{"status":"paid","issued_at":"2026-07-01T00:10:00Z",
                "total":1.0,"currency":"USD","pdf_url":"https://x","extra":1}]}"#,
        )
        .expect("decode");
        assert_eq!(response.invoices.len(), 1);
        assert_eq!(response.invoices[0].total, 1.0);
    }

    /// A total that parses as neither number nor decimal string is a decode
    /// failure — the client maps it to `DecodingFailed`, never a silent 0.
    #[test]
    fn an_unparseable_total_is_a_decode_error() {
        let result: Result<NeonInvoicesResponse, _> = serde_json::from_str(
            r#"{"invoices":[{"status":"paid","issued_at":"2026-07-01T00:10:00Z",
                "total":"fifteen","currency":"USD"}]}"#,
        );
        assert!(result.is_err());
    }

    // MARK: - Client

    fn json(body: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(body, "application/json")
    }

    fn client(base_url: &str) -> NeonClient {
        NeonClient::with_base_url(base_url, "neon_api_key_value").expect("a non-blank key")
    }

    /// Twin of `testServiceRequestsTheMonthToDateWindowForTheConfiguredOrg` —
    /// the matchers ARE the assertion: without them the mock never matches and
    /// the call comes back `Http { status: 404 }`.
    #[tokio::test]
    async fn requests_the_month_to_date_window_for_the_configured_org() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(CONSUMPTION_HISTORY_PATH))
            .and(header("authorization", "Bearer neon_api_key_value"))
            .and(query_param("from", "2026-07-01T00:00:00Z"))
            .and(query_param("to", "2026-08-01T00:00:00Z"))
            .and(query_param("granularity", "monthly"))
            .and(query_param("org_id", "org-abc"))
            .and(query_param("metrics", metric::REQUESTED))
            .respond_with(json(TWO_PROJECTS))
            .mount(&server)
            .await;

        let summary = client(&server.uri())
            .month_to_date("org-abc", utc(2026, 7, 20, 9, 15))
            .await
            .expect("should decode");
        assert_eq!(summary.compute_unit_hours(), Some(3.0));
    }

    /// Twin of `testAMissingOrgIDIsReportedWithoutCallingTheAPI`.
    #[tokio::test]
    async fn a_blank_org_id_is_reported_without_calling_the_api() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json(TWO_PROJECTS))
            .mount(&server)
            .await;

        let err = client(&server.uri())
            .month_to_date("   ", utc(2026, 7, 31, 12, 0))
            .await
            .unwrap_err();
        assert_eq!(err, NeonUsageError::MissingOrgId);
        assert_eq!(err.user_message(), "Add your Neon org ID in Settings");
        assert!(
            server
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty(),
            "no org id means nothing to ask for"
        );
    }

    /// Twin of `testNoAPIKeyLeavesTheServiceUnconfiguredSoThePanelHidesTheSection`:
    /// with no key there is no client to fail, so the panel has nothing to show
    /// rather than an error to render.
    #[test]
    fn a_blank_api_key_yields_no_client_at_all() {
        assert!(NeonClient::new("").is_none());
        assert!(NeonClient::new("   ").is_none());
        assert!(NeonClient::new("neon_api_key_value").is_some());
    }

    /// Twin of `testASuccessfulButEmptyResponseKeepsTheValuesUnknown`.
    #[tokio::test]
    async fn a_successful_but_empty_response_stays_unmeasured() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json(r#"{ "projects": [] }"#))
            .mount(&server)
            .await;

        let summary = client(&server.uri())
            .month_to_date("org-abc", utc(2026, 7, 31, 12, 0))
            .await
            .expect("an empty org is a successful answer");
        assert_eq!(summary, NeonUsageSummary::Unmeasured);
        assert!(summary.is_unmeasured());
    }

    /// Twin of `testTransportFailureLeavesTheValueUnknownAndSurfacesTheError`:
    /// the failure is an `Err`, so there is no summary to mistake for a zero.
    #[tokio::test]
    async fn a_401_is_an_auth_failure_carrying_the_key_prompt() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = client(&server.uri())
            .month_to_date("org-abc", utc(2026, 7, 31, 12, 0))
            .await
            .unwrap_err();
        assert!(err.is_auth_failure());
        assert_eq!(
            err.user_message(),
            "API key invalid — paste a new one in Settings"
        );
    }

    #[tokio::test]
    async fn a_500_is_reported_with_its_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let err = client(&server.uri())
            .month_to_date("org-abc", utc(2026, 7, 31, 12, 0))
            .await
            .unwrap_err();
        assert_eq!(err, NeonUsageError::Http { status: 500 });
    }

    #[tokio::test]
    async fn malformed_json_is_a_decode_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json("{\"projects\": 12}"))
            .mount(&server)
            .await;

        let err = client(&server.uri())
            .month_to_date("org-abc", utc(2026, 7, 31, 12, 0))
            .await
            .unwrap_err();
        assert!(
            matches!(err, NeonUsageError::DecodingFailed(_)),
            "got {err:?}"
        );
        assert!(err
            .user_message()
            .starts_with("Couldn't read the Neon response"));
    }

    #[tokio::test]
    async fn an_unroutable_host_is_unreachable() {
        let err = client("http://127.0.0.1:1")
            .month_to_date("org-abc", utc(2026, 7, 31, 12, 0))
            .await
            .unwrap_err();
        assert!(matches!(err, NeonUsageError::Unreachable(_)), "got {err:?}");
    }

    /// The key must never reach the URL, or it would ride along in `reqwest`'s
    /// error strings and any log line built from them.
    #[tokio::test]
    async fn the_api_key_travels_in_a_header_and_never_in_the_url() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json(TWO_PROJECTS))
            .mount(&server)
            .await;

        client(&server.uri())
            .month_to_date("org-abc", utc(2026, 7, 31, 12, 0))
            .await
            .expect("decodes");

        let requests = server.received_requests().await.expect("recorded requests");
        let request = requests.first().expect("one request");
        assert!(
            !request.url.as_str().contains("neon_api_key_value"),
            "the key must not appear in the URL: {}",
            request.url
        );
        assert_eq!(
            request
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok()),
            Some("Bearer neon_api_key_value")
        );
    }

    /// The matchers ARE the assertion, as in
    /// `requests_the_month_to_date_window_for_the_configured_org`.
    #[tokio::test]
    async fn invoices_hits_the_org_billing_path_with_the_bearer_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/organizations/org-abc/billing/invoices"))
            .and(header("authorization", "Bearer neon_api_key_value"))
            .respond_with(json(FOUR_INVOICES))
            .mount(&server)
            .await;

        let summary = client(&server.uri())
            .invoices("org-abc")
            .await
            .expect("should decode");
        assert_eq!(
            summary,
            NeonInvoiceSummary::Latest {
                total: 15.91,
                currency: "USD".into()
            }
        );
    }

    /// Twin of `a_slug_containing_a_separator_cannot_retarget_the_request` in
    /// the Sentry suite. The org id is a free-text Settings field landing in a
    /// path segment, so a separator inside it must be encoded or the request
    /// retargets: `evil/../../users` would otherwise walk out of
    /// `/api/v2/organizations/`.
    #[tokio::test]
    async fn an_org_id_containing_a_separator_cannot_retarget_the_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v2/organizations/evil%2F..%2F..%2Fusers/billing/invoices",
            ))
            .respond_with(json(FOUR_INVOICES))
            .mount(&server)
            .await;

        let summary = client(&server.uri())
            .invoices("evil/../../users")
            .await
            .expect("the org id stays one segment");
        assert_eq!(
            summary,
            NeonInvoiceSummary::Latest {
                total: 15.91,
                currency: "USD".into()
            }
        );

        let requests = server.received_requests().await.expect("recorded requests");
        assert_eq!(
            requests.first().map(|r| r.url.path()),
            Some("/api/v2/organizations/evil%2F..%2F..%2Fusers/billing/invoices"),
            "the separator must survive as an escape, not as a path boundary"
        );
    }

    #[tokio::test]
    async fn a_blank_org_id_skips_the_invoice_request_too() {
        let server = MockServer::start().await;
        let err = client(&server.uri()).invoices("  ").await.unwrap_err();
        assert_eq!(err, NeonUsageError::MissingOrgId);
    }

    /// The endpoint is undocumented; a 404 (vanished, or older plans) must map
    /// to the same typed Http error every other failure does.
    #[tokio::test]
    async fn a_missing_invoices_endpoint_is_an_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let err = client(&server.uri()).invoices("org-abc").await.unwrap_err();
        assert_eq!(err, NeonUsageError::Http { status: 404 });
    }

    // MARK: - estimate_usd

    /// Rates unset is setup, not measurement: no estimate, not $0.00.
    #[test]
    fn no_rates_means_no_estimate() {
        let s = summarize(&serde_json::from_str(TWO_PROJECTS).expect("decode"));
        assert_eq!(estimate_usd(s, 0.0, 0.0), None);
        assert_eq!(estimate_usd(s, -1.0, 0.0), None);
    }

    /// An unmeasured summary has no figures to price; an estimate over it
    /// would be a fabricated number wearing a dollar sign.
    #[test]
    fn an_unmeasured_summary_has_no_estimate() {
        assert_eq!(
            estimate_usd(NeonUsageSummary::Unmeasured, 0.106, 0.35),
            None
        );
    }

    /// One rate is enough — the other contributes zero, not None.
    #[test]
    fn a_single_rate_prices_only_its_metric() {
        let s = summarize(&serde_json::from_str(TWO_PROJECTS).expect("decode"));
        let compute = s.compute_unit_hours().expect("measured");
        assert_eq!(estimate_usd(s, 2.0, 0.0), Some(compute * 2.0));
    }

    #[test]
    fn both_rates_sum_compute_and_storage() {
        let s = summarize(&serde_json::from_str(TWO_PROJECTS).expect("decode"));
        let expected = s.compute_unit_hours().expect("measured") * 0.106
            + s.storage_gib().expect("measured") * 0.35;
        let estimate = estimate_usd(s, 0.106, 0.35).expect("estimate");
        assert!((estimate - expected).abs() < 1e-12);
    }
}
