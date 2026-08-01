//! Accepted error events for one Sentry organization over a rolling window.
//! Rust port of `DevCanopy/Services/SentryUsage/`.
//!
//! Distinct from the app's own crash-reporting bootstrap: nothing here touches
//! a Sentry SDK. The token carries only the read-only `org:read` scope and
//! travels in an `Authorization: Bearer` header, never in a URL.

use std::num::NonZeroUsize;
use std::time::Duration;

use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde::Deserialize;
use std::collections::HashMap;

pub const DEFAULT_BASE_URL: &str = "https://sentry.io";

// MARK: - Query vocabulary

/// The exact `stats_v2` query vocabulary this crate sends, and the one outcome
/// it reports. Sentry's outcome axis splits an org's ingest into `accepted`,
/// `filtered`, `rate_limited`, `invalid`, `dropped`… — only `accepted` counts
/// against quota, so that is the single figure the cockpit shows.
pub mod query {
    /// The aggregate field: event *quantity*, not times-seen.
    pub const FIELD: &str = "sum(quantity)";
    /// The data category. Errors only — transactions and attachments bill
    /// separately, and folding them together would produce a number that
    /// matches nothing in Sentry.
    pub const CATEGORY: &str = "error";
    /// The axis we split on, so `accepted` can be isolated.
    pub const GROUP_BY: &str = "outcome";
    /// The outcome whose total is the headline figure.
    pub const ACCEPTED_OUTCOME: &str = "accepted";
    /// Rolling window, in Sentry's own `statsPeriod` grammar.
    pub const STATS_PERIOD: &str = "30d";
    /// Days in [`STATS_PERIOD`], for the panel's label.
    pub const WINDOW_DAYS: u32 = 30;
}

// MARK: - Summary

/// Accepted error events for one Sentry organization over the rolling window.
///
/// # Why this is an enum
///
/// Sentry returns an outcome group only when that outcome had data in the
/// window, so a *missing* `accepted` group among other groups is a real zero —
/// the org ingested errors and none were accepted. But a response with no
/// groups at all measured nothing (wrong slug, brand-new org, no ingest), which
/// is genuinely unknown: the panel must render `—` there rather than a
/// fabricated 0.
///
/// The variants make that structural rather than merely documented:
/// [`SentryUsageSummary::Unmeasured`] holds no count to read, and
/// [`SentryUsageSummary::Measured`] cannot exist without at least one outcome
/// group (`NonZeroUsize`) having come back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SentryUsageSummary {
    /// The API answered, but carried no outcome groups. The count is unknown.
    Unmeasured,
    /// At least one outcome group came back, so the window was measured.
    Measured {
        /// Accepted error events over the window. Zero here is a real zero.
        accepted_error_events: u64,
        /// How many outcome groups the response carried.
        outcome_group_count: NonZeroUsize,
    },
}

impl SentryUsageSummary {
    /// Accepted error events, or `None` when nothing was measured.
    #[must_use]
    pub fn accepted_error_events(&self) -> Option<u64> {
        match self {
            SentryUsageSummary::Unmeasured => None,
            SentryUsageSummary::Measured {
                accepted_error_events,
                ..
            } => Some(*accepted_error_events),
        }
    }

    /// How many outcome groups the response carried. Zero exactly when nothing
    /// was measured.
    #[must_use]
    pub fn outcome_group_count(&self) -> usize {
        match self {
            SentryUsageSummary::Unmeasured => 0,
            SentryUsageSummary::Measured {
                outcome_group_count,
                ..
            } => outcome_group_count.get(),
        }
    }

    /// Whether the API answered successfully but measured nothing — the case
    /// the panel footer explains with [`NO_STATS_MESSAGE`].
    #[must_use]
    pub fn is_unmeasured(&self) -> bool {
        matches!(self, SentryUsageSummary::Unmeasured)
    }
}

/// Shown when the API answers successfully but reports no outcome groups at
/// all — the wrong org slug, or an org that has ingested nothing in the window.
pub const NO_STATS_MESSAGE: &str =
    "no Sentry event stats reported — check the org slug in Settings";

// MARK: - Wire format

/// `GET /api/0/organizations/{org}/stats_v2/` — the org event-count response.
/// Every nested container is optional because this is an external contract we
/// don't control; a response without groups must decode cleanly and fold to
/// "unknown".
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SentryStatsResponse {
    pub start: Option<String>,
    pub end: Option<String>,
    pub groups: Option<Vec<SentryStatsGroup>>,
}

/// One `groupBy` bucket. `by` carries the axis values (here
/// `{"outcome": "accepted"}`) and `totals` the aggregates keyed by the requested
/// field name.
///
/// Both maps are typed narrowly for the one query this crate sends: `outcome`
/// values are strings, and `sum(quantity)` is numeric. Decoding `totals` as
/// `f64` (rather than an integer) tolerates a JSON number written with a
/// fractional part; the fold rounds it back to a whole event count.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SentryStatsGroup {
    pub by: Option<HashMap<String, String>>,
    pub totals: Option<HashMap<String, f64>>,
}

// MARK: - Errors

/// Failures from reading Sentry org stats.
/// [`SentryUsageError::is_auth_failure`] is the signal the panel turns into a
/// "paste a new token" prompt — a revoked or under-scoped token answers with
/// 401 / 403.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SentryUsageError {
    #[error("Add your Sentry org slug in Settings")]
    MissingOrgSlug,
    #[error("Invalid Sentry API URL — check the org slug in Settings")]
    InvalidUrl,
    #[error("Invalid response from the Sentry API")]
    InvalidResponse,
    #[error("Sentry API request failed (HTTP {status})")]
    Http { status: u16 },
    #[error("Couldn't read the Sentry response ({0})")]
    DecodingFailed(String),
    /// No Swift twin: `URLSession` folds transport failures into a generic
    /// `Error` the service reports via `localizedDescription`.
    #[error("Couldn't reach the Sentry API ({0})")]
    Unreachable(String),
}

impl SentryUsageError {
    /// True when the failure looks like a bad, revoked, or under-scoped token.
    #[must_use]
    pub fn is_auth_failure(&self) -> bool {
        matches!(self, SentryUsageError::Http { status: 401 | 403 })
    }

    /// The message the panel footer shows. Cause-specific so the operator fixes
    /// the right thing: a 403 is a token to replace, a 404 is a slug to
    /// correct, and everything else names the failure it actually was.
    #[must_use]
    pub fn user_message(&self) -> String {
        if self.is_auth_failure() {
            return "token invalid — paste a new one in Settings".to_string();
        }
        if matches!(self, SentryUsageError::Http { status: 404 }) {
            return "Sentry org not found — check the org slug in Settings".to_string();
        }
        self.to_string()
    }
}

// MARK: - Folding

/// Pick the `accepted` outcome's `sum(quantity)` total out of the grouped
/// response.
///
/// A response with no groups yields [`SentryUsageSummary::Unmeasured`] — the
/// `—` path, because nothing was measured. A response *with* groups but no
/// `accepted` bucket folds to a real 0: Sentry measured the window and accepted
/// nothing.
#[must_use]
pub fn summarize(response: &SentryStatsResponse) -> SentryUsageSummary {
    let groups = response.groups.as_deref().unwrap_or_default();
    let Some(outcome_group_count) = NonZeroUsize::new(groups.len()) else {
        return SentryUsageSummary::Unmeasured;
    };

    let accepted: f64 = groups
        .iter()
        .filter(|group| {
            group
                .by
                .as_ref()
                .and_then(|by| by.get(query::GROUP_BY))
                .is_some_and(|outcome| outcome == query::ACCEPTED_OUTCOME)
        })
        // A group that carries no `sum(quantity)` contributes nothing rather
        // than a value guessed out of the series.
        .filter_map(|group| group.totals.as_ref()?.get(query::FIELD).copied())
        .sum();

    SentryUsageSummary::Measured {
        // Negative or non-finite totals are not event counts; clamping to 0
        // keeps a nonsense payload from wrapping into an enormous figure.
        accepted_error_events: accepted.round().max(0.0) as u64,
        outcome_group_count,
    }
}

// MARK: - Client

/// Percent-encode everything outside RFC 3986's unreserved set.
///
/// Deliberately stricter than the Swift's `.urlPathAllowed`, which *permits*
/// `/` and so leaves the retargeting it warns about possible: a slug of
/// `a/../b` would walk out of the organizations path. Encoding the separator is
/// the only way the segment stays one segment.
const SLUG_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// The one Sentry REST read the Usage panel needs.
pub struct SentryClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl SentryClient {
    /// Builds a client, or `None` when the token is blank.
    ///
    /// "No token" is modelled as the absence of a client rather than as an
    /// error from one: the panel hides the Sentry section entirely when the
    /// user has not configured a token, which is not a failure to report.
    #[must_use]
    pub fn new(token: &str) -> Option<Self> {
        Self::with_base_url(DEFAULT_BASE_URL, token)
    }

    /// # Invariant: no credentials in `base_url`
    ///
    /// `base_url` must be scheme/host/port only. The token is a separate
    /// argument and travels only via `bearer_auth()`, which puts it in a
    /// header. URLs leak where headers do not: `reqwest` attaches the request
    /// URL to its errors and does not redact userinfo.
    ///
    /// Exists so tests (and only tests) can point the client at a mock server.
    #[must_use]
    pub fn with_base_url(base_url: &str, token: &str) -> Option<Self> {
        if token.trim().is_empty() {
            return None;
        }
        Some(SentryClient {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
        })
    }

    /// Error-category event counts for `org_slug` over `stats_period`, grouped
    /// by outcome.
    ///
    /// A blank slug short-circuits to [`SentryUsageError::MissingOrgSlug`]
    /// without a request: the slug is a path segment, so without it there is
    /// nothing to ask for.
    pub async fn error_outcomes(
        &self,
        org_slug: &str,
        stats_period: &str,
    ) -> Result<SentryStatsResponse, SentryUsageError> {
        let org_slug = org_slug.trim();
        if org_slug.is_empty() {
            return Err(SentryUsageError::MissingOrgSlug);
        }
        let slug = utf8_percent_encode(org_slug, SLUG_SEGMENT);

        let resp = self
            .http
            .get(format!(
                "{}/api/0/organizations/{slug}/stats_v2/",
                self.base_url
            ))
            .query(&[
                ("field", query::FIELD),
                ("category", query::CATEGORY),
                ("groupBy", query::GROUP_BY),
                ("statsPeriod", stats_period),
            ])
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| SentryUsageError::Unreachable(e.to_string()))?;

        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(SentryUsageError::Http { status });
        }

        let body = resp
            .text()
            .await
            .map_err(|e| SentryUsageError::Unreachable(e.to_string()))?;
        serde_json::from_str(&body).map_err(|e| SentryUsageError::DecodingFailed(e.to_string()))
    }

    /// The read the panel makes: accepted error events over the default
    /// rolling window, folded to a summary.
    pub async fn accepted_errors(
        &self,
        org_slug: &str,
    ) -> Result<SentryUsageSummary, SentryUsageError> {
        let response = self.error_outcomes(org_slug, query::STATS_PERIOD).await?;
        Ok(summarize(&response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // MARK: - Fixtures
    // Twins of `DevCanopyTests/SentryUsageMappingTests.swift`.

    const OUTCOMES: &str = r#"
    {
      "start": "2026-07-01T00:00:00Z",
      "end": "2026-07-31T00:00:00Z",
      "intervals": ["2026-07-01T00:00:00Z"],
      "groups": [
        {
          "by": { "outcome": "accepted" },
          "totals": { "sum(quantity)": 12345 },
          "series": { "sum(quantity)": [12345] }
        },
        {
          "by": { "outcome": "rate_limited" },
          "totals": { "sum(quantity)": 500 },
          "series": { "sum(quantity)": [500] }
        },
        {
          "by": { "outcome": "invalid" },
          "totals": { "sum(quantity)": 165665 },
          "series": { "sum(quantity)": [165665] }
        }
      ]
    }
    "#;

    fn fixture(json: &str) -> SentryStatsResponse {
        serde_json::from_str(json).expect("fixture decodes")
    }

    // MARK: - summarize

    /// Twin of `testSummarizePicksOnlyTheAcceptedOutcome`: rate_limited (500)
    /// and invalid (165665) must not land in the headline — they don't count
    /// against quota, and folding them in would inflate the figure.
    #[test]
    fn picks_only_the_accepted_outcome() {
        let summary = summarize(&fixture(OUTCOMES));
        assert_eq!(summary.accepted_error_events(), Some(12_345));
        assert_eq!(summary.outcome_group_count(), 3);
    }

    /// Twin of `testSummarizeTreatsAMissingAcceptedGroupAsARealZero`: Sentry
    /// returns an outcome group only when that outcome had data, so a response
    /// that measured *something* but accepted nothing is a real zero.
    #[test]
    fn a_missing_accepted_group_among_others_is_a_real_zero() {
        let json = r#"
        {
          "groups": [
            { "by": { "outcome": "rate_limited" }, "totals": { "sum(quantity)": 42 } }
          ]
        }
        "#;
        let summary = summarize(&fixture(json));
        assert_eq!(
            summary.accepted_error_events(),
            Some(0),
            "the window was measured and accepted nothing"
        );
        assert_eq!(summary.outcome_group_count(), 1);
    }

    /// Twin of `testSummarizeReportsUnknownWhenNoGroupsComeBack`: nothing was
    /// measured — the wrong slug or an org with no ingest. Genuinely unknown,
    /// so the figure must be `None`, never 0.
    #[test]
    fn no_groups_at_all_is_unmeasured_not_zero() {
        let summary = summarize(&fixture(r#"{ "groups": [] }"#));
        assert_eq!(summary, SentryUsageSummary::Unmeasured);
        assert_eq!(summary.accepted_error_events(), None);
        assert_eq!(summary.outcome_group_count(), 0);
    }

    /// Twin of `testDecodeToleratesAMissingGroupsKey`.
    #[test]
    fn a_missing_groups_key_decodes_and_folds_to_unmeasured() {
        let summary = summarize(&fixture("{}"));
        assert_eq!(summary.accepted_error_events(), None);
        assert_eq!(summary.outcome_group_count(), 0);
    }

    /// Twin of `testSummarizeToleratesAnAcceptedGroupWithoutTheRequestedField`:
    /// the group exists but carries no `sum(quantity)`, so it contributes
    /// nothing rather than a value guessed from the series.
    #[test]
    fn an_accepted_group_without_the_requested_field_contributes_nothing() {
        let json = r#"{ "groups": [ { "by": { "outcome": "accepted" }, "totals": {} } ] }"#;
        assert_eq!(
            summarize(&fixture(json)).accepted_error_events(),
            Some(0),
            "the window was still measured — one group came back"
        );
    }

    /// The distinction the whole enum exists for, asserted side by side: same
    /// headline intent, two different answers, and they must not be equal.
    #[test]
    fn unmeasured_and_a_real_zero_are_different_answers() {
        let unmeasured = summarize(&fixture(r#"{ "groups": [] }"#));
        let real_zero = summarize(&fixture(
            r#"{ "groups": [ { "by": { "outcome": "invalid" }, "totals": { "sum(quantity)": 9 } } ] }"#,
        ));

        assert_eq!(unmeasured.accepted_error_events(), None);
        assert_eq!(real_zero.accepted_error_events(), Some(0));
        assert_ne!(unmeasured, real_zero);
        assert!(unmeasured.is_unmeasured());
        assert!(!real_zero.is_unmeasured());
    }

    /// A fractional total is rounded back to a whole event count.
    #[test]
    fn a_fractional_total_rounds_to_a_whole_event_count() {
        let json = r#"{ "groups": [ { "by": { "outcome": "accepted" }, "totals": { "sum(quantity)": 12.6 } } ] }"#;
        assert_eq!(summarize(&fixture(json)).accepted_error_events(), Some(13));
    }

    /// Several `accepted` buckets (a shape the axis shouldn't produce, but the
    /// contract permits) sum rather than overwrite.
    #[test]
    fn repeated_accepted_groups_sum() {
        let json = r#"
        {
          "groups": [
            { "by": { "outcome": "accepted" }, "totals": { "sum(quantity)": 10 } },
            { "by": { "outcome": "accepted" }, "totals": { "sum(quantity)": 5 } }
          ]
        }
        "#;
        let summary = summarize(&fixture(json));
        assert_eq!(summary.accepted_error_events(), Some(15));
        assert_eq!(summary.outcome_group_count(), 2);
    }

    // MARK: - Query vocabulary

    /// Twin of `testQueryVocabularyMatchesTheDocumentedStatsV2Contract`.
    #[test]
    fn the_query_vocabulary_matches_the_documented_contract() {
        assert_eq!(query::FIELD, "sum(quantity)");
        assert_eq!(query::CATEGORY, "error");
        assert_eq!(query::GROUP_BY, "outcome");
        assert_eq!(query::ACCEPTED_OUTCOME, "accepted");
        assert_eq!(query::STATS_PERIOD, "30d");
        assert_eq!(query::WINDOW_DAYS, 30);
    }

    /// The footer text for a successful-but-empty read, carried over verbatim
    /// so the panel keeps saying why it shows `—` rather than a 0.
    #[test]
    fn the_no_stats_message_matches_the_swift_verbatim() {
        assert_eq!(
            NO_STATS_MESSAGE,
            "no Sentry event stats reported — check the org slug in Settings"
        );
    }

    // MARK: - Errors

    /// Twin of `testAuthFailuresAreDetectedFor401And403`.
    #[test]
    fn auth_failures_are_401_and_403_only() {
        assert!(SentryUsageError::Http { status: 401 }.is_auth_failure());
        assert!(SentryUsageError::Http { status: 403 }.is_auth_failure());
        assert!(!SentryUsageError::Http { status: 500 }.is_auth_failure());
        assert!(!SentryUsageError::InvalidResponse.is_auth_failure());
        assert!(!SentryUsageError::MissingOrgSlug.is_auth_failure());
    }

    /// Twin of `testFriendlyMessageAsksForANewTokenOnAuthFailure`.
    #[test]
    fn an_auth_failure_asks_for_a_new_token() {
        assert_eq!(
            SentryUsageError::Http { status: 403 }.user_message(),
            "token invalid — paste a new one in Settings"
        );
        assert_eq!(
            SentryUsageError::Http { status: 401 }.user_message(),
            "token invalid — paste a new one in Settings"
        );
    }

    /// Twin of `testFriendlyMessagePointsAtTheOrgSlugOn404`.
    #[test]
    fn a_404_points_at_the_org_slug() {
        assert_eq!(
            SentryUsageError::Http { status: 404 }.user_message(),
            "Sentry org not found — check the org slug in Settings"
        );
    }

    /// Twin of `testATransientFailureKeepsTheLastGoodFigure`'s footer text: a
    /// transient failure names its own status so it reads differently from an
    /// auth problem. (Keeping the last-good summary is the shell's job — this
    /// crate holds no state.)
    #[test]
    fn other_failures_name_themselves() {
        assert_eq!(
            SentryUsageError::Http { status: 503 }.user_message(),
            "Sentry API request failed (HTTP 503)"
        );
        assert_eq!(
            SentryUsageError::MissingOrgSlug.user_message(),
            "Add your Sentry org slug in Settings"
        );
        assert_eq!(
            SentryUsageError::DecodingFailed("bad token".into()).user_message(),
            "Couldn't read the Sentry response (bad token)"
        );
        assert_eq!(
            SentryUsageError::InvalidUrl.user_message(),
            "Invalid Sentry API URL — check the org slug in Settings"
        );
    }

    // MARK: - Client

    fn json(body: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(body, "application/json")
    }

    fn client(base_url: &str) -> SentryClient {
        SentryClient::with_base_url(base_url, "sentry_token_value").expect("a non-blank token")
    }

    /// Twin of `testServiceRequestsTheRollingWindowForTheConfiguredOrg` — the
    /// matchers ARE the assertion: without them the mock never matches and the
    /// call comes back `Http { status: 404 }`.
    #[tokio::test]
    async fn requests_the_rolling_window_for_the_configured_org() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/0/organizations/sassy-dog/stats_v2/"))
            .and(header("authorization", "Bearer sentry_token_value"))
            .and(query_param("field", query::FIELD))
            .and(query_param("category", "error"))
            .and(query_param("groupBy", "outcome"))
            .and(query_param("statsPeriod", "30d"))
            .respond_with(json(OUTCOMES))
            .mount(&server)
            .await;

        let summary = client(&server.uri())
            .accepted_errors("sassy-dog")
            .await
            .expect("should decode");
        assert_eq!(summary.accepted_error_events(), Some(12_345));
    }

    /// Twin of `testAMissingOrgSlugIsReportedWithoutCallingTheAPI`.
    #[tokio::test]
    async fn a_blank_org_slug_is_reported_without_calling_the_api() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json(OUTCOMES))
            .mount(&server)
            .await;

        let err = client(&server.uri())
            .accepted_errors("   ")
            .await
            .unwrap_err();
        assert_eq!(err, SentryUsageError::MissingOrgSlug);
        assert_eq!(err.user_message(), "Add your Sentry org slug in Settings");
        assert!(
            server
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty(),
            "no slug means nothing to ask for"
        );
    }

    /// Twin of `testNoTokenLeavesTheServiceUnconfiguredSoThePanelHidesTheSection`:
    /// with no token there is no client to fail, so the panel has nothing to
    /// show rather than an error to render.
    #[test]
    fn a_blank_token_yields_no_client_at_all() {
        assert!(SentryClient::new("").is_none());
        assert!(SentryClient::new("   ").is_none());
        assert!(SentryClient::new("sentry_token_value").is_some());
    }

    /// The slug lands in a path segment, so a separator inside it must be
    /// encoded or the request retargets. `a/../..%2Fusers` would otherwise walk
    /// out of `/api/0/organizations/`.
    #[tokio::test]
    async fn a_slug_containing_a_separator_cannot_retarget_the_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/0/organizations/evil%2F..%2F..%2Fusers/stats_v2/",
            ))
            .respond_with(json(OUTCOMES))
            .mount(&server)
            .await;

        let summary = client(&server.uri())
            .accepted_errors("evil/../../users")
            .await
            .expect("the slug stays one segment");
        assert_eq!(summary.accepted_error_events(), Some(12_345));

        let requests = server.received_requests().await.expect("recorded requests");
        assert_eq!(
            requests.first().map(|r| r.url.path()),
            Some("/api/0/organizations/evil%2F..%2F..%2Fusers/stats_v2/"),
            "the separator must survive as an escape, not as a path boundary"
        );
    }

    /// Twin of `testASuccessfulButEmptyResponseKeepsTheValueUnknown`.
    #[tokio::test]
    async fn a_successful_but_empty_response_stays_unmeasured() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json(r#"{ "groups": [] }"#))
            .mount(&server)
            .await;

        let summary = client(&server.uri())
            .accepted_errors("sassy-dog")
            .await
            .expect("an org with no ingest is a successful answer");
        assert_eq!(summary, SentryUsageSummary::Unmeasured);
    }

    /// Twin of `testTransportFailureLeavesTheValueUnknownAndSurfacesTheError`:
    /// the failure is an `Err`, so there is no summary to mistake for a zero.
    #[tokio::test]
    async fn a_401_is_an_auth_failure_carrying_the_token_prompt() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = client(&server.uri())
            .accepted_errors("sassy-dog")
            .await
            .unwrap_err();
        assert!(err.is_auth_failure());
        assert_eq!(
            err.user_message(),
            "token invalid — paste a new one in Settings"
        );
    }

    #[tokio::test]
    async fn a_503_is_reported_with_its_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let err = client(&server.uri())
            .accepted_errors("sassy-dog")
            .await
            .unwrap_err();
        assert_eq!(err, SentryUsageError::Http { status: 503 });
        assert_eq!(err.user_message(), "Sentry API request failed (HTTP 503)");
    }

    #[tokio::test]
    async fn malformed_json_is_a_decode_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json("{\"groups\": 7}"))
            .mount(&server)
            .await;

        let err = client(&server.uri())
            .accepted_errors("sassy-dog")
            .await
            .unwrap_err();
        assert!(
            matches!(err, SentryUsageError::DecodingFailed(_)),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn an_unroutable_host_is_unreachable() {
        let err = client("http://127.0.0.1:1")
            .accepted_errors("sassy-dog")
            .await
            .unwrap_err();
        assert!(
            matches!(err, SentryUsageError::Unreachable(_)),
            "got {err:?}"
        );
    }

    /// The token must never reach the URL, or it would ride along in
    /// `reqwest`'s error strings and any log line built from them.
    #[tokio::test]
    async fn the_token_travels_in_a_header_and_never_in_the_url() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(json(OUTCOMES))
            .mount(&server)
            .await;

        client(&server.uri())
            .accepted_errors("sassy-dog")
            .await
            .expect("decodes");

        let requests = server.received_requests().await.expect("recorded requests");
        let request = requests.first().expect("one request");
        assert!(
            !request.url.as_str().contains("sentry_token_value"),
            "the token must not appear in the URL: {}",
            request.url
        );
        assert_eq!(
            request
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok()),
            Some("Bearer sentry_token_value")
        );
    }
}
