//! Vercel spend, from the FOCUS billing-charges export.
//!
//! `GET /v1/billing/charges` answers **FOCUS v1.3 as JSONL** — one charge per
//! line, one line per (day × service × project). It is the only documented
//! endpoint that reports what Vercel is actually costing; everything else in
//! the REST surface is provisioning.
//!
//! Two things about that payload shape the whole module, both measured against
//! the live account on 2026-08-06:
//!
//! **It is enormous and almost entirely zeros.** Seven days is 6,444 lines and
//! 3.4 MB, of which 145 lines (2.3%) carry a non-zero cost — every service the
//! account *could* use gets a row per day whether it used it or not. Gzip takes
//! the wire cost to 85 KB, which is why the client asks for it explicitly; the
//! fold below is what keeps the rest from reaching the panel.
//!
//! **Two costs, and they mean different things.** `BilledCost` is what appears
//! on an invoice — for a plan with included allowance that is the *overage*,
//! and it reads $0.02 on an account spending real money. `EffectiveCost`
//! amortizes the plan itself and is what Vercel costs to run. The panel shows
//! both, headlining the second, because a cockpit that reported two cents would
//! be technically correct and useless.
//!
//! **The plan arrives as a `Usage` line**, not a `Purchase` one: a row named
//! "Pro" at ~$1.29/day, which is the subscription amortized daily. So the
//! top-services list is led by the plan rather than by anything anyone can
//! tune. That is left alone deliberately — it *is* the largest thing Vercel
//! costs, and a list that hid it to look more actionable would be answering a
//! question nobody asked. The `Purchase`/`Tax` filter below still matters:
//! those categories do appear, and folding them in would double-count.
//!
//! Day buckets are Vercel's, not ours: `ChargePeriodStart` lands on 07:00Z —
//! midnight Pacific — so a month-to-date window opens inside the last day of
//! the previous month. The API decides the bucket containing `from`, and this
//! module does not second-guess it.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// `https://api.vercel.com`.
const DEFAULT_BASE_URL: &str = "https://api.vercel.com";

/// How many services the panel names. The rest fold into the total; a cockpit
/// row is a glance, and 56 service names is a spreadsheet.
pub const TOP_N: usize = 3;

/// What the panel says when the read succeeded and measured nothing.
pub const NO_SPEND_MESSAGE: &str = "no charges in this period";

/// One period's spend, folded.
///
/// `Unmeasured` is a *successful* read that found no charges — a brand-new
/// team, or a window before the account existed. Distinct from an error, and
/// rendered as the em dash with a reason rather than as `$0.00`, which is a
/// claim the read cannot support.
#[derive(Debug, Clone, PartialEq)]
pub enum VercelUsageSummary {
    Unmeasured,
    Measured {
        /// Amortized total, including the plan. The headline.
        effective_usd: f64,
        /// What an invoice would show beyond the included allowance.
        billed_usd: f64,
        /// The costliest services, most expensive first, at most [`TOP_N`].
        top_services: Vec<ServiceSpend>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServiceSpend {
    pub name: String,
    pub effective_usd: f64,
}

impl VercelUsageSummary {
    #[must_use]
    pub fn effective_usd(&self) -> Option<f64> {
        match self {
            VercelUsageSummary::Unmeasured => None,
            VercelUsageSummary::Measured { effective_usd, .. } => Some(*effective_usd),
        }
    }

    #[must_use]
    pub fn billed_usd(&self) -> Option<f64> {
        match self {
            VercelUsageSummary::Unmeasured => None,
            VercelUsageSummary::Measured { billed_usd, .. } => Some(*billed_usd),
        }
    }

    #[must_use]
    pub fn top_services(&self) -> &[ServiceSpend] {
        match self {
            VercelUsageSummary::Unmeasured => &[],
            VercelUsageSummary::Measured { top_services, .. } => top_services,
        }
    }

    #[must_use]
    pub fn is_unmeasured(&self) -> bool {
        matches!(self, VercelUsageSummary::Unmeasured)
    }
}

/// One FOCUS charge line. Every field is `Option`: this is someone else's
/// export format and a missing key must not fail the whole read.
#[derive(Debug, Deserialize)]
struct Charge {
    #[serde(rename = "BilledCost")]
    billed_cost: Option<f64>,
    #[serde(rename = "EffectiveCost")]
    effective_cost: Option<f64>,
    #[serde(rename = "ServiceName")]
    service_name: Option<String>,
    /// `Usage` / `Purchase` / `Tax` / `Credit` / `Adjustment`.
    #[serde(rename = "ChargeCategory")]
    charge_category: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum VercelUsageError {
    #[error("no Vercel API token configured")]
    MissingToken,
    #[error("Vercel rejected the token")]
    Unauthorized,
    #[error("Vercel returned HTTP {status}")]
    Http { status: u16 },
    #[error("unreachable: {0}")]
    Unreachable(String),
    #[error("could not decode the Vercel charges: {0}")]
    DecodeFailed(String),
}

impl VercelUsageError {
    #[must_use]
    pub fn is_auth_failure(&self) -> bool {
        matches!(self, VercelUsageError::Unauthorized)
            | matches!(self, VercelUsageError::Http { status: 403 })
    }

    /// Cause-specific guidance, so the operator chases the right layer.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            VercelUsageError::MissingToken => "add a Vercel API token in Settings".to_owned(),
            VercelUsageError::Unauthorized | VercelUsageError::Http { status: 403 } => {
                "Vercel rejected the token — check its scope in Settings".to_owned()
            }
            VercelUsageError::Http { status } => format!("Vercel returned HTTP {status}"),
            VercelUsageError::Unreachable(_) => "couldn't reach Vercel".to_owned(),
            VercelUsageError::DecodeFailed(_) => "couldn't read the Vercel charges".to_owned(),
        }
    }
}

/// Fold a JSONL charges export into the panel's three figures.
///
/// Pure, and over the body rather than a response, so the shape that is most
/// likely to drift is testable one `include_str!` away.
///
/// **`Usage` charges only.** A `Purchase` line is the plan itself and a `Tax`
/// line is neither Vercel's doing nor a lever anyone can pull; folding them in
/// would make the top-services list meaningless. `EffectiveCost` already
/// amortizes the plan across the usage rows, which is the number that answers
/// "what is this costing".
///
/// # Errors
/// [`VercelUsageError::DecodeFailed`] if no line parses — a body that is not
/// JSONL at all. Individual unparseable lines are skipped, because one new
/// field in someone else's export must not blank the panel.
pub fn summarize(body: &str) -> Result<VercelUsageSummary, VercelUsageError> {
    let mut parsed = 0usize;
    let mut effective = 0.0;
    let mut billed = 0.0;
    let mut by_service: BTreeMap<String, f64> = BTreeMap::new();

    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(charge) = serde_json::from_str::<Charge>(line) else {
            continue;
        };
        parsed += 1;
        if charge.charge_category.as_deref() != Some("Usage") {
            continue;
        }
        let e = charge.effective_cost.unwrap_or(0.0);
        effective += e;
        billed += charge.billed_cost.unwrap_or(0.0);
        if e != 0.0 {
            if let Some(name) = charge.service_name {
                *by_service.entry(name).or_insert(0.0) += e;
            }
        }
    }

    if parsed == 0 {
        // Distinguished from "no charges": a body with no parseable line is a
        // shape this build does not understand, and reporting $0.00 for it
        // would be the quietest possible way to be wrong.
        if body.trim().is_empty() {
            return Ok(VercelUsageSummary::Unmeasured);
        }
        return Err(VercelUsageError::DecodeFailed(
            "no parseable charge lines".to_owned(),
        ));
    }

    if by_service.is_empty() && effective == 0.0 && billed == 0.0 {
        return Ok(VercelUsageSummary::Unmeasured);
    }

    let mut top: Vec<ServiceSpend> = by_service
        .into_iter()
        .map(|(name, effective_usd)| ServiceSpend {
            name,
            effective_usd,
        })
        .collect();
    // Cost descending, then name, so equal costs keep a stable order rather
    // than shuffling between polls.
    top.sort_by(|a, b| {
        b.effective_usd
            .partial_cmp(&a.effective_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    top.truncate(TOP_N);

    Ok(VercelUsageSummary::Measured {
        effective_usd: effective,
        billed_usd: billed,
        top_services: top,
    })
}

/// Reads Vercel's FOCUS billing charges.
#[derive(Debug)]
pub struct VercelClient {
    base_url: String,
    token: String,
    team_id: String,
    http: reqwest::Client,
}

impl VercelClient {
    /// Builds a client, or `None` when the token is blank.
    ///
    /// "No token" is the absence of a client rather than an error from one:
    /// the panel hides the section entirely when nobody configured Vercel,
    /// which is not a failure to report.
    #[must_use]
    pub fn new(token: &str, team_id: &str) -> Option<Self> {
        Self::with_base_url(DEFAULT_BASE_URL, token, team_id)
    }

    /// # Invariant: no credentials in `base_url`
    ///
    /// `base_url` is scheme/host/port only. The token travels via
    /// `bearer_auth()`, which puts it in a header — URLs leak where headers do
    /// not, since `reqwest` attaches the request URL to its errors.
    ///
    /// Exists so tests (and only tests) can point the client at a mock server.
    #[must_use]
    pub fn with_base_url(base_url: &str, token: &str, team_id: &str) -> Option<Self> {
        if token.trim().is_empty() {
            return None;
        }
        Some(VercelClient {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            team_id: team_id.to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
        })
    }

    /// Month-to-date spend for `now`'s month.
    ///
    /// `now` is an argument, never a clock read — the crate's rule.
    ///
    /// # Errors
    /// [`VercelUsageError`] when Vercel rejects the token, answers non-2xx, is
    /// unreachable, or sends a body with no parseable charge line.
    pub async fn month_to_date(
        &self,
        now: DateTime<Utc>,
    ) -> Result<VercelUsageSummary, VercelUsageError> {
        // Reuses Neon's window rather than computing a second month boundary:
        // two panels disagreeing about when the month started would be a
        // difference nobody could explain from the screen. The `to` bound is
        // the first of next month, which Vercel treats as "up to now".
        let (from, to) = crate::neon::month_to_date_window(now);
        self.charges(from, to).await
    }

    /// One `charges` read over an explicit window.
    ///
    /// # Errors
    /// See [`month_to_date`](Self::month_to_date).
    pub async fn charges(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<VercelUsageSummary, VercelUsageError> {
        let url = format!("{}/v1/billing/charges", self.base_url);
        let mut request = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            // Seven days is 3.4 MB uncompressed and 85 KB gzipped, measured
            // live. `reqwest` is built here with `default-features = false`,
            // so this header is inert without the `gzip` feature — which the
            // manifest enables for exactly this endpoint.
            .header(reqwest::header::ACCEPT_ENCODING, "gzip")
            .query(&[("from", from.to_rfc3339()), ("to", to.to_rfc3339())]);
        // A personal account has no team; sending an empty `teamId` is a 400.
        if !self.team_id.trim().is_empty() {
            request = request.query(&[("teamId", self.team_id.as_str())]);
        }

        let response = request
            .send()
            .await
            .map_err(|e| VercelUsageError::Unreachable(e.without_url().to_string()))?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(VercelUsageError::Unauthorized);
        }
        if !status.is_success() {
            return Err(VercelUsageError::Http {
                status: status.as_u16(),
            });
        }
        let body = response
            .text()
            .await
            .map_err(|e| VercelUsageError::Unreachable(e.without_url().to_string()))?;
        summarize(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Four days of a real export, trimmed to the fields this reads — the
    /// zero-cost majority included, because filtering them is the fold's job.
    const CHARGES: &str = include_str!("../tests/fixtures/vercel_charges.jsonl");

    fn at(ts: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(ts).expect("timestamp").into()
    }

    #[test]
    fn the_capture_folds_to_both_costs_and_the_top_services() {
        let s = summarize(CHARGES).expect("folds");
        let effective = s.effective_usd().expect("effective");
        let billed = s.billed_usd().expect("billed");
        assert!(effective > billed, "amortized cost exceeds the overage");
        assert!(!s.top_services().is_empty());
        assert!(s.top_services().len() <= TOP_N);
        // Descending, which is what makes the first row the one worth reading.
        for pair in s.top_services().windows(2) {
            assert!(pair[0].effective_usd >= pair[1].effective_usd);
        }
    }

    /// 97.7% of a real export is zero-cost rows for services the account never
    /// touched. Naming one in the top-three would be worse than naming none.
    #[test]
    fn zero_cost_services_never_reach_the_top_list() {
        let s = summarize(CHARGES).expect("folds");
        for svc in s.top_services() {
            assert!(svc.effective_usd != 0.0, "{} cost nothing", svc.name);
        }
    }

    /// `Purchase` is the plan and `Tax` is nobody's lever. Folding them into
    /// the service list would put "Pro Plan" at the top of a list meant to
    /// answer "what am I actually running".
    #[test]
    fn only_usage_charges_are_counted() {
        let with_plan = format!(
            "{CHARGES}\n{}",
            r#"{"ChargeCategory":"Purchase","BilledCost":20.0,"EffectiveCost":20.0,"ServiceName":"Pro Plan"}"#
        );
        let base = summarize(CHARGES).expect("folds");
        let s = summarize(&with_plan).expect("folds");
        assert_eq!(
            s.effective_usd(),
            base.effective_usd(),
            "a Purchase line must not move the usage total"
        );
        assert!(s.top_services().iter().all(|x| x.name != "Pro Plan"));
    }

    /// One new field in someone else's export must not blank the panel.
    #[test]
    fn an_unparseable_line_is_skipped_not_fatal() {
        let s = summarize(&format!("not json at all\n{CHARGES}")).expect("still folds");
        assert!(!s.is_unmeasured());
    }

    /// …but a body with *no* parseable line is a shape this build does not
    /// understand, and reporting `$0.00` for it would be the quietest possible
    /// way to be wrong.
    #[test]
    fn a_body_with_nothing_parseable_is_an_error_not_a_zero() {
        let err = summarize("<html>bad gateway</html>").expect_err("not JSONL");
        assert!(matches!(err, VercelUsageError::DecodeFailed(_)));
        assert_eq!(err.user_message(), "couldn't read the Vercel charges");
    }

    /// An empty body is a successful read that measured nothing — a window
    /// before the account existed. The em dash, not a fabricated zero.
    #[test]
    fn an_empty_export_is_unmeasured_rather_than_zero() {
        let s = summarize("").expect("folds");
        assert!(s.is_unmeasured());
        assert_eq!(s.effective_usd(), None);
        assert_eq!(s.billed_usd(), None);
    }

    #[test]
    fn a_blank_token_yields_no_client_at_all() {
        assert!(VercelClient::new("", "team_x").is_none());
        assert!(VercelClient::new("   ", "team_x").is_none());
        assert!(VercelClient::new("tok", "team_x").is_some());
    }

    #[test]
    fn auth_failures_are_classified_apart_from_other_http_errors() {
        assert!(VercelUsageError::Unauthorized.is_auth_failure());
        assert!(VercelUsageError::Http { status: 403 }.is_auth_failure());
        assert!(!VercelUsageError::Http { status: 500 }.is_auth_failure());
        assert!(VercelUsageError::Unauthorized
            .user_message()
            .contains("token"));
    }

    #[tokio::test]
    async fn the_window_and_the_team_travel_as_query_parameters() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/v1/billing/charges"))
            .and(wiremock::matchers::query_param("teamId", "team_x"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer secret-token",
            ))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_raw(CHARGES, "text/plain"))
            .mount(&server)
            .await;

        let s = VercelClient::with_base_url(&server.uri(), "secret-token", "team_x")
            .expect("client")
            .month_to_date(at("2026-08-06T12:00:00Z"))
            .await
            .expect("summary");
        assert!(s.effective_usd().is_some());
    }

    /// A personal account has no team, and an empty `teamId` is a 400.
    #[tokio::test]
    async fn a_blank_team_id_is_omitted_rather_than_sent_empty() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::query_param_is_missing("teamId"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_raw(CHARGES, "text/plain"))
            .mount(&server)
            .await;

        assert!(VercelClient::with_base_url(&server.uri(), "tok", "")
            .expect("client")
            .month_to_date(at("2026-08-06T12:00:00Z"))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn a_401_is_an_auth_failure_not_a_generic_http_error() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = VercelClient::with_base_url(&server.uri(), "tok", "team_x")
            .expect("client")
            .month_to_date(at("2026-08-06T12:00:00Z"))
            .await
            .expect_err("401");
        assert!(err.is_auth_failure(), "{err}");
    }

    #[tokio::test]
    async fn an_unroutable_host_is_unreachable_without_leaking_the_url() {
        let err = VercelClient::with_base_url("http://127.0.0.1:1", "tok", "team_x")
            .expect("client")
            .month_to_date(at("2026-08-06T12:00:00Z"))
            .await
            .expect_err("refused");
        assert!(matches!(err, VercelUsageError::Unreachable(_)));
        assert!(!format!("{err}").contains("127.0.0.1:1"), "{err}");
    }
}
