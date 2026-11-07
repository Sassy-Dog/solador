//! This month's API spend for an organization, from each vendor's own cost API.
//!
//! Both need an **Admin** key, which an ordinary API key is not, and Anthropic's
//! Admin API is unavailable to individual accounts. Both are read-only here.
//!
//! The two wires differ in the way that matters: Anthropic's amount is a decimal
//! string in **cents**, and OpenAI's is a number in the currency's whole unit, so
//! each is converted where it is parsed and [`ApiSpendSummary`] carries dollars.
//!
//! Anthropic's figures lag the requests by hours, up to a day, and exclude a
//! Priority Tier commitment fee. `refreshed_at` is how far they reach, which is
//! not when they were read.

use std::time::Duration;

use chrono::{DateTime, Datelike, TimeZone, Utc};
use fault::Fault;
use serde::Deserialize;

const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
const OPENAI_BASE_URL: &str = "https://api.openai.com";
/// The version every example of the cost API sends.
const ANTHROPIC_VERSION: &str = "2026-10-21";
/// The cost API is in beta and refuses a request without this.
const ANTHROPIC_BETA: &str = "analytics-usage-cost-2026-08-24";
/// One bucket a day, so a month always fits one page. `next_page` is still
/// followed: a partial sum would be a wrong figure with nothing to say so.
const OPENAI_BUCKETS: &str = "31";
/// A cursor that never ends is a broken reply, not a long month.
const MAX_PAGES: usize = 20;

/// A successful read that has no figure yet. Not a failure, and not $0.
pub const NO_SPEND_MESSAGE: &str = "no spend reported for this month yet";

/// What a vendor said about this month.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ApiSpendSummary {
    /// The vendor answered and has nothing for this month yet. Not $0.
    Unmeasured,
    /// Spend in US dollars. `refreshed_at` is how far the figure reaches, for a
    /// vendor that says.
    Measured {
        usd: f64,
        refreshed_at: Option<DateTime<Utc>>,
    },
}

impl ApiSpendSummary {
    /// The figure, or `None` when the vendor has not measured this month.
    #[must_use]
    pub fn usd(&self) -> Option<f64> {
        match self {
            ApiSpendSummary::Unmeasured => None,
            ApiSpendSummary::Measured { usd, .. } => Some(*usd),
        }
    }

    /// How far the figure reaches, for a vendor that says.
    #[must_use]
    pub fn refreshed_at(&self) -> Option<DateTime<Utc>> {
        match self {
            ApiSpendSummary::Unmeasured => None,
            ApiSpendSummary::Measured { refreshed_at, .. } => *refreshed_at,
        }
    }

    #[must_use]
    pub fn is_unmeasured(&self) -> bool {
        matches!(self, ApiSpendSummary::Unmeasured)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiSpendError {
    #[error("{vendor} rejected the Admin API key")]
    Unauthorized { vendor: &'static str },
    #[error("{vendor} returned HTTP {status}")]
    Http { vendor: &'static str, status: u16 },
    #[error("{vendor} unreachable: {reason}")]
    Unreachable {
        vendor: &'static str,
        reason: String,
    },
    #[error("could not decode the {vendor} cost reply: {reason}")]
    DecodeFailed {
        vendor: &'static str,
        reason: String,
    },
}

impl ApiSpendError {
    /// Cause-specific guidance. The auth arm says what kind of key this is,
    /// because an ordinary API key is the mistake this step invites.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            ApiSpendError::Unauthorized { vendor }
            | ApiSpendError::Http {
                vendor,
                status: 403,
            } => format!(
                "{} — this needs an Admin API key",
                Fault::CredentialRejected.message(vendor)
            ),
            ApiSpendError::Http { vendor, status } => fault::http_status_message(*status, vendor),
            ApiSpendError::Unreachable { vendor, .. } => Fault::Unreachable.message(vendor),
            ApiSpendError::DecodeFailed { vendor, .. } => Fault::Undecodable.message(vendor),
        }
    }
}

/// The first instant of `now`'s month, UTC, the day both vendors bucket by.
fn month_start(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .unwrap_or(now)
}

/// A client that never follows a redirect: a `302` would carry the Admin key
/// to wherever it points, and reqwest keeps `x-api-key` across a hop. A redirect
/// is reported as [`ApiSpendError::Http`].
fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("reqwest client")
}

async fn body<T: serde::de::DeserializeOwned>(
    vendor: &'static str,
    request: reqwest::RequestBuilder,
) -> Result<T, ApiSpendError> {
    let unreachable = |e: reqwest::Error| ApiSpendError::Unreachable {
        vendor,
        reason: e.without_url().to_string(),
    };
    let response = request.send().await.map_err(unreachable)?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(ApiSpendError::Unauthorized { vendor });
    }
    if !status.is_success() {
        return Err(ApiSpendError::Http {
            vendor,
            status: status.as_u16(),
        });
    }
    let text = response.text().await.map_err(unreachable)?;
    serde_json::from_str(&text).map_err(|e| ApiSpendError::DecodeFailed {
        vendor,
        reason: e.to_string(),
    })
}

fn undecodable(vendor: &'static str, reason: &str) -> ApiSpendError {
    ApiSpendError::DecodeFailed {
        vendor,
        reason: reason.to_owned(),
    }
}

/// `data` and `has_more` are required, here and in [`OpenAiPage`]: a reply
/// without them isn't an empty month, it is one this can't read, and defaulting
/// them would turn a renamed field into `$0.00`. A vendor's "nothing yet" is a
/// present, empty `data`.
#[derive(Deserialize)]
struct AnthropicPage {
    data: Vec<AnthropicRow>,
    has_more: bool,
    next_page: Option<String>,
    data_refreshed_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct AnthropicRow {
    cost: AnthropicCost,
}

#[derive(Deserialize)]
struct AnthropicCost {
    /// A decimal string in cents: `"41280.000000"` is $412.80.
    amount: String,
    /// `"USD"`. Anything else is refused, as OpenAI's is.
    currency: String,
}

/// Reads an organization's cost from Anthropic.
pub struct AnthropicCostClient {
    base_url: String,
    key: String,
    http: reqwest::Client,
}

/// Hand-written so a `{:?}` in a log line or a panic can never print the key.
impl std::fmt::Debug for AnthropicCostClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicCostClient")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl AnthropicCostClient {
    /// A client, or `None` for a blank key.
    #[must_use]
    pub fn new(key: &str) -> Option<Self> {
        Self::with_base_url(ANTHROPIC_BASE_URL, key)
    }

    /// `base_url` is scheme, host and port only, and exists for tests. The key
    /// travels in a header, because `reqwest` attaches a URL to its errors.
    #[must_use]
    pub fn with_base_url(base_url: &str, key: &str) -> Option<Self> {
        (!key.trim().is_empty()).then(|| AnthropicCostClient {
            base_url: base_url.trim_end_matches('/').to_string(),
            key: key.to_string(),
            http: http(),
        })
    }

    /// Spend since the first of `now`'s month. With no bucket width the vendor
    /// answers one total for the range, up to now.
    ///
    /// # Errors
    /// [`ApiSpendError`] when the key is refused, the vendor answers non-2xx, is
    /// unreachable, or sends a body this cannot read.
    pub async fn month_to_date(
        &self,
        now: DateTime<Utc>,
    ) -> Result<ApiSpendSummary, ApiSpendError> {
        const VENDOR: &str = "Anthropic";
        let url = format!("{}/v1/organizations/analytics/cost", self.base_url);
        let starting_at = month_start(now).format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let mut cents = 0.0;
        let mut page: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut request = self
                .http
                .get(&url)
                .header("x-api-key", &self.key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("anthropic-beta", ANTHROPIC_BETA)
                .query(&[("starting_at", starting_at.as_str())]);
            if let Some(cursor) = &page {
                request = request.query(&[("page", cursor.as_str())]);
            }
            let reply: AnthropicPage = body(VENDOR, request).await?;
            for row in &reply.data {
                if !row.cost.currency.eq_ignore_ascii_case("usd") {
                    return Err(undecodable(VENDOR, "a cost was not in USD"));
                }
                cents += row
                    .cost
                    .amount
                    .parse::<f64>()
                    .ok()
                    .filter(|amount| amount.is_finite())
                    .ok_or_else(|| undecodable(VENDOR, "a cost amount was not a decimal string"))?;
            }
            if reply.has_more {
                page = Some(
                    reply
                        .next_page
                        .ok_or_else(|| undecodable(VENDOR, "more pages, but no cursor"))?,
                );
                continue;
            }
            // Nothing yet is the vendor's own statement that it has not counted
            // this period, so the figure is unknown rather than zero.
            if reply.data_refreshed_at.is_none() && cents == 0.0 {
                return Ok(ApiSpendSummary::Unmeasured);
            }
            return Ok(ApiSpendSummary::Measured {
                usd: cents / 100.0,
                refreshed_at: reply.data_refreshed_at,
            });
        }
        Err(undecodable(VENDOR, "too many pages"))
    }
}

#[derive(Deserialize)]
struct OpenAiPage {
    data: Vec<OpenAiBucket>,
    has_more: bool,
    next_page: Option<String>,
}

/// `results` is what OpenAI's endpoint example and cookbook send, though its
/// published `UsageTimeBucket` schema spells it `result`. If a live reply ever
/// follows the schema, this fails out loud rather than reading as a zero day.
#[derive(Deserialize)]
struct OpenAiBucket {
    results: Vec<OpenAiResult>,
}

#[derive(Deserialize)]
struct OpenAiResult {
    amount: OpenAiAmount,
}

#[derive(Deserialize)]
struct OpenAiAmount {
    value: f64,
    currency: String,
}

/// Reads an organization's cost from OpenAI.
pub struct OpenAiCostClient {
    base_url: String,
    key: String,
    http: reqwest::Client,
}

/// See [`AnthropicCostClient`]'s.
impl std::fmt::Debug for OpenAiCostClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCostClient")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl OpenAiCostClient {
    /// A client, or `None` for a blank key.
    #[must_use]
    pub fn new(key: &str) -> Option<Self> {
        Self::with_base_url(OPENAI_BASE_URL, key)
    }

    /// See [`AnthropicCostClient::with_base_url`].
    #[must_use]
    pub fn with_base_url(base_url: &str, key: &str) -> Option<Self> {
        (!key.trim().is_empty()).then(|| OpenAiCostClient {
            base_url: base_url.trim_end_matches('/').to_string(),
            key: key.to_string(),
            http: http(),
        })
    }

    /// Spend since the first of `now`'s month, summed over its daily buckets.
    ///
    /// # Errors
    /// See [`AnthropicCostClient::month_to_date`]. A currency other than USD is
    /// a decode failure too: this has no rate, and a `$` on it would be false.
    pub async fn month_to_date(
        &self,
        now: DateTime<Utc>,
    ) -> Result<ApiSpendSummary, ApiSpendError> {
        const VENDOR: &str = "OpenAI";
        let url = format!("{}/v1/organization/costs", self.base_url);
        let start_time = month_start(now).timestamp().to_string();
        let mut usd = 0.0;
        let mut buckets = 0usize;
        let mut page: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut request = self.http.get(&url).bearer_auth(&self.key).query(&[
                ("start_time", start_time.as_str()),
                ("bucket_width", "1d"),
                ("limit", OPENAI_BUCKETS),
            ]);
            if let Some(cursor) = &page {
                request = request.query(&[("page", cursor.as_str())]);
            }
            let reply: OpenAiPage = body(VENDOR, request).await?;
            for bucket in &reply.data {
                buckets += 1;
                for result in &bucket.results {
                    if !result.amount.currency.eq_ignore_ascii_case("usd") {
                        return Err(undecodable(VENDOR, "a cost was not in USD"));
                    }
                    usd += result.amount.value;
                }
            }
            if reply.has_more {
                page = Some(
                    reply
                        .next_page
                        .ok_or_else(|| undecodable(VENDOR, "more pages, but no cursor"))?,
                );
                continue;
            }
            // Buckets with no results are measured days with no spend. No
            // buckets at all is no answer about the month.
            return Ok(if buckets == 0 {
                ApiSpendSummary::Unmeasured
            } else {
                ApiSpendSummary::Measured {
                    usd,
                    refreshed_at: None,
                }
            });
        }
        Err(undecodable(VENDOR, "too many pages"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn at(ts: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(ts).expect("timestamp").into()
    }

    const NOW: &str = "2026-11-07T15:30:00Z";

    /// The documented envelope, with the documented cost object verbatim.
    fn anthropic_reply(amount: &str) -> serde_json::Value {
        serde_json::json!({
            "data": [{
                "starting_at": "2026-11-01T00:00:00Z",
                "ending_at": "2026-11-07T15:00:00Z",
                "group": {},
                "cost": {"currency": "USD", "amount": amount, "list_amount": "51600.000000", "requests": null},
            }],
            "has_more": false,
            "next_page": null,
            "data_refreshed_at": "2026-11-07T12:00:00Z",
        })
    }

    /// Hand-written `Debug`, so a `{:?}` in a log line or a panic can never
    /// print an Admin key. A derived one would.
    #[test]
    fn a_client_never_prints_its_key() {
        let anthropic = AnthropicCostClient::new("sk-ant-admin01-typed").expect("client");
        let openai = OpenAiCostClient::new("sk-admin-typed").expect("client");
        assert!(!format!("{anthropic:?}").contains("typed"));
        assert!(!format!("{openai:?}").contains("typed"));
    }

    #[tokio::test]
    async fn anthropic_sends_the_admin_key_and_reads_cents_as_dollars() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/organizations/analytics/cost"))
            .and(header("x-api-key", "sk-ant-admin01-test"))
            .and(header("anthropic-version", ANTHROPIC_VERSION))
            .and(header("anthropic-beta", ANTHROPIC_BETA))
            .and(query_param("starting_at", "2026-11-01T00:00:00Z"))
            .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_reply("41280.000000")))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            AnthropicCostClient::with_base_url(&server.uri(), "sk-ant-admin01-test").unwrap();
        assert_eq!(
            client.month_to_date(at(NOW)).await.unwrap(),
            ApiSpendSummary::Measured {
                usd: 412.80,
                refreshed_at: Some(at("2026-11-07T12:00:00Z")),
            }
        );
    }

    /// The vendor's own way of saying it has not counted this period yet.
    #[tokio::test]
    async fn anthropic_with_nothing_refreshed_is_unmeasured_not_zero() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [], "has_more": false, "next_page": null, "data_refreshed_at": null,
            })))
            .mount(&server)
            .await;
        let client = AnthropicCostClient::with_base_url(&server.uri(), "k").unwrap();
        assert_eq!(
            client.month_to_date(at(NOW)).await.unwrap(),
            ApiSpendSummary::Unmeasured
        );
    }

    /// Counted, and nothing was spent: that one is a real zero.
    #[tokio::test]
    async fn anthropic_with_a_refreshed_empty_month_is_a_measured_zero() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [], "has_more": false, "next_page": null,
                "data_refreshed_at": "2026-11-07T12:00:00Z",
            })))
            .mount(&server)
            .await;
        let client = AnthropicCostClient::with_base_url(&server.uri(), "k").unwrap();
        assert!(matches!(
            client.month_to_date(at(NOW)).await.unwrap(),
            ApiSpendSummary::Measured { usd, .. } if usd == 0.0
        ));
    }

    #[tokio::test]
    async fn anthropic_follows_every_page_rather_than_summing_part_of_a_month() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(query_param("page", "page_2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_reply("100.000000")))
            .mount(&server)
            .await;
        let mut first = anthropic_reply("200.000000");
        first["has_more"] = true.into();
        first["next_page"] = "page_2".into();
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(first))
            .mount(&server)
            .await;
        let client = AnthropicCostClient::with_base_url(&server.uri(), "k").unwrap();
        assert!(matches!(
            client.month_to_date(at(NOW)).await.unwrap(),
            ApiSpendSummary::Measured { usd, .. } if (usd - 3.0).abs() < 1e-9
        ));
    }

    #[tokio::test]
    async fn anthropic_refuses_an_amount_it_cannot_read() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_reply("a lot")))
            .mount(&server)
            .await;
        let client = AnthropicCostClient::with_base_url(&server.uri(), "k").unwrap();
        assert!(matches!(
            client.month_to_date(at(NOW)).await,
            Err(ApiSpendError::DecodeFailed { .. })
        ));
    }

    /// The OpenAI reader's rule, on Anthropic's wire: no rate means no `$` on
    /// another currency. And `str::parse` reads `"NaN"` and `"inf"` as numbers,
    /// so the string amount needs a finite check that serde's never does.
    #[tokio::test]
    async fn anthropic_refuses_a_currency_other_than_usd_and_an_amount_that_is_not_finite() {
        let mut euros = anthropic_reply("41280.000000");
        euros["data"][0]["cost"]["currency"] = "EUR".into();
        for reply in [euros, anthropic_reply("NaN"), anthropic_reply("inf")] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(200).set_body_json(&reply))
                .mount(&server)
                .await;
            let client = AnthropicCostClient::with_base_url(&server.uri(), "k").unwrap();
            let read = client.month_to_date(at(NOW)).await;
            assert!(
                matches!(read, Err(ApiSpendError::DecodeFailed { .. })),
                "{}: {read:?}",
                reply["data"][0]["cost"]
            );
        }
    }

    /// An ordinary API key is the likely mistake, so the sentence names the kind
    /// of key, and it never repeats the key.
    #[tokio::test]
    async fn a_refused_key_says_an_admin_key_is_needed_and_never_echoes_it() {
        for status in [401, 403] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
            let client = AnthropicCostClient::with_base_url(&server.uri(), "sk-secret").unwrap();
            let error = client.month_to_date(at(NOW)).await.unwrap_err();
            let sentence = error.user_message();
            assert!(sentence.contains("Admin API key"), "{status}: {sentence}");
            assert!(!sentence.contains("sk-secret"));
            assert!(!format!("{error:?}").contains("sk-secret"));
        }
    }

    #[tokio::test]
    async fn openai_sums_its_daily_buckets_in_dollars() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/organization/costs"))
            .and(header("authorization", "Bearer sk-admin-test"))
            .and(query_param("start_time", "1793491200"))
            .and(query_param("bucket_width", "1d"))
            .and(query_param("limit", "31"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "object": "page",
                "data": [
                    {"object": "bucket", "start_time": 1793491200, "end_time": 1793577600, "results": [
                        {"object": "organization.costs.result", "amount": {"value": 0.06, "currency": "usd"},
                         "line_item": null, "project_id": null},
                    ]},
                    {"object": "bucket", "start_time": 1793577600, "end_time": 1793664000, "results": [
                        {"object": "organization.costs.result", "amount": {"value": 1.5, "currency": "usd"}},
                    ]},
                ],
                "has_more": false,
                "next_page": null,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = OpenAiCostClient::with_base_url(&server.uri(), "sk-admin-test").unwrap();
        assert!(matches!(
            client.month_to_date(at(NOW)).await.unwrap(),
            ApiSpendSummary::Measured { usd, refreshed_at: None } if (usd - 1.56).abs() < 1e-9
        ));
    }

    #[tokio::test]
    async fn openai_with_days_and_no_spend_is_a_zero_and_with_no_days_is_unknown() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"results": []}], "has_more": false, "next_page": null,
            })))
            .mount(&server)
            .await;
        let client = OpenAiCostClient::with_base_url(&server.uri(), "k").unwrap();
        assert!(matches!(
            client.month_to_date(at(NOW)).await.unwrap(),
            ApiSpendSummary::Measured { usd, .. } if usd == 0.0
        ));

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [], "has_more": false, "next_page": null,
            })))
            .mount(&server)
            .await;
        let client = OpenAiCostClient::with_base_url(&server.uri(), "k").unwrap();
        assert_eq!(
            client.month_to_date(at(NOW)).await.unwrap(),
            ApiSpendSummary::Unmeasured
        );
    }

    /// There is no rate to convert with, and a `$` on a euro figure is false.
    #[tokio::test]
    async fn openai_refuses_a_currency_other_than_usd() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"results": [{"amount": {"value": 3.0, "currency": "eur"}}]}],
                "has_more": false, "next_page": null,
            })))
            .mount(&server)
            .await;
        let client = OpenAiCostClient::with_base_url(&server.uri(), "k").unwrap();
        assert!(matches!(
            client.month_to_date(at(NOW)).await,
            Err(ApiSpendError::DecodeFailed { .. })
        ));
    }

    fn openai_reply(value: f64) -> serde_json::Value {
        serde_json::json!({
            "data": [{"results": [{"amount": {"value": value, "currency": "usd"}}]}],
            "has_more": false,
            "next_page": null,
        })
    }

    #[tokio::test]
    async fn openai_follows_every_page_rather_than_summing_part_of_a_month() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(query_param("page", "page_2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(openai_reply(1.0)))
            .mount(&server)
            .await;
        let mut first = openai_reply(2.0);
        first["has_more"] = true.into();
        first["next_page"] = "page_2".into();
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(first))
            .mount(&server)
            .await;
        let client = OpenAiCostClient::with_base_url(&server.uri(), "k").unwrap();
        assert!(matches!(
            client.month_to_date(at(NOW)).await.unwrap(),
            ApiSpendSummary::Measured { usd, .. } if (usd - 3.0).abs() < 1e-9
        ));
    }

    /// `has_more` with no cursor would otherwise stop at a partial month, and
    /// a partial month is a figure that looks right.
    #[tokio::test]
    async fn more_pages_with_no_cursor_is_a_failure_for_both_vendors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/organizations/analytics/cost"))
            .respond_with(ResponseTemplate::new(200).set_body_json({
                let mut reply = anthropic_reply("100.000000");
                reply["has_more"] = true.into();
                reply
            }))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/organization/costs"))
            .respond_with(ResponseTemplate::new(200).set_body_json({
                let mut reply = openai_reply(1.0);
                reply["has_more"] = true.into();
                reply
            }))
            .mount(&server)
            .await;
        let anthropic = AnthropicCostClient::with_base_url(&server.uri(), "k").unwrap();
        let openai = OpenAiCostClient::with_base_url(&server.uri(), "k").unwrap();
        assert!(matches!(
            anthropic.month_to_date(at(NOW)).await,
            Err(ApiSpendError::DecodeFailed { .. })
        ));
        assert!(matches!(
            openai.month_to_date(at(NOW)).await,
            Err(ApiSpendError::DecodeFailed { .. })
        ));
    }

    /// A vendor that always has more is cut off at [`MAX_PAGES`] with a
    /// failure, never a figure for however many pages were read.
    #[tokio::test]
    async fn a_month_that_never_ends_is_a_failure_for_both_vendors() {
        let endless = |mut reply: serde_json::Value| {
            reply["has_more"] = true.into();
            reply["next_page"] = "again".into();
            ResponseTemplate::new(200).set_body_json(reply)
        };
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/organizations/analytics/cost"))
            .respond_with(endless(anthropic_reply("100.000000")))
            .expect(MAX_PAGES as u64)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/organization/costs"))
            .respond_with(endless(openai_reply(1.0)))
            .expect(MAX_PAGES as u64)
            .mount(&server)
            .await;
        let anthropic = AnthropicCostClient::with_base_url(&server.uri(), "k").unwrap();
        let openai = OpenAiCostClient::with_base_url(&server.uri(), "k").unwrap();
        assert!(matches!(
            anthropic.month_to_date(at(NOW)).await,
            Err(ApiSpendError::DecodeFailed { .. })
        ));
        assert!(matches!(
            openai.month_to_date(at(NOW)).await,
            Err(ApiSpendError::DecodeFailed { .. })
        ));
    }

    /// Neither reader has met a live key, so this is what stands between a field
    /// the vendor renamed and a wrong figure. Every reply here holds real money
    /// under a key the reader doesn't know: a default would show `$0.00`, or "no
    /// spend yet", stamped fresh and with nothing logged.
    #[tokio::test]
    async fn a_reply_missing_what_the_reader_needs_fails_rather_than_reading_as_empty() {
        let spent = serde_json::json!([{"cost": {"amount": "41280.000000"}}]);
        for reply in [
            serde_json::json!({}),
            serde_json::json!({"rows": spent, "has_more": false, "next_page": null}),
            serde_json::json!({"rows": spent, "has_more": false, "next_page": null,
                "data_refreshed_at": "2026-11-07T12:00:00Z"}),
            serde_json::json!({"data": [], "next_page": null, "data_refreshed_at": null}),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(200).set_body_json(&reply))
                .mount(&server)
                .await;
            let client = AnthropicCostClient::with_base_url(&server.uri(), "k").unwrap();
            let read = client.month_to_date(at(NOW)).await;
            assert!(
                matches!(read, Err(ApiSpendError::DecodeFailed { .. })),
                "{reply}: {read:?}"
            );
        }

        let spent = serde_json::json!([{"amount": {"value": 512.0, "currency": "usd"}}]);
        for reply in [
            serde_json::json!({}),
            serde_json::json!({"buckets": [{"results": spent}], "has_more": false, "next_page": null}),
            serde_json::json!({"data": [{"result": spent}], "has_more": false, "next_page": null}),
            serde_json::json!({"data": [{}], "has_more": false, "next_page": null}),
            serde_json::json!({"data": [], "next_page": null}),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(200).set_body_json(&reply))
                .mount(&server)
                .await;
            let client = OpenAiCostClient::with_base_url(&server.uri(), "k").unwrap();
            let read = client.month_to_date(at(NOW)).await;
            assert!(
                matches!(read, Err(ApiSpendError::DecodeFailed { .. })),
                "{reply}: {read:?}"
            );
        }
    }

    /// Reqwest keeps a custom header across a hop, so a followed redirect would
    /// hand the Anthropic key to whatever the `Location` names. A hop the
    /// reader never makes is a hop that can't.
    #[tokio::test]
    async fn neither_reader_follows_a_redirect() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/elsewhere"))
            .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_reply("100.000000")))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/elsewhere", server.uri())),
            )
            .mount(&server)
            .await;
        let anthropic = AnthropicCostClient::with_base_url(&server.uri(), "k").unwrap();
        let openai = OpenAiCostClient::with_base_url(&server.uri(), "k").unwrap();
        assert!(matches!(
            anthropic.month_to_date(at(NOW)).await,
            Err(ApiSpendError::Http { status: 302, .. })
        ));
        assert!(matches!(
            openai.month_to_date(at(NOW)).await,
            Err(ApiSpendError::Http { status: 302, .. })
        ));
    }

    #[test]
    fn a_blank_key_builds_no_client() {
        assert!(AnthropicCostClient::new("  ").is_none());
        assert!(OpenAiCostClient::new("").is_none());
    }
}
