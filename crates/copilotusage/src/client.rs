//! The Copilot usage reads.
//!
//! Authorization is a bearer token in a header, never in a URL, and the token
//! is held only for the life of the client. `Debug` is written by hand so it
//! cannot print it.

use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Datelike, Utc};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

use crate::error::{CopilotUsageError, ErrorKind, Level};
use crate::models::{UsageItem, UsageReport};
use crate::seat::{interpret, Seat, SeatBody, SeatError, SeatErrorKind};
use crate::summary::{summarize, Usage};

pub const DEFAULT_BASE_URL: &str = "https://api.github.com";
/// The version the Copilot billing endpoints document. This crate only:
/// `crates/actionsbilling` keeps the version it was built against.
pub const API_VERSION: &str = "2026-03-10";

/// Everything outside RFC 3986's unreserved set, so a `/` in a login stays one
/// segment rather than retargeting the request.
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Whose Copilot is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Billed<'a> {
    /// An organization's seats (Copilot Business), by login.
    Org(&'a str),
    /// A person's own plan (Copilot Pro/Pro+), by username.
    User(&'a str),
}

impl Billed<'_> {
    #[must_use]
    pub fn level(&self) -> Level {
        match self {
            Billed::Org(_) => Level::Org,
            Billed::User(_) => Level::User,
        }
    }

    fn login(&self) -> &str {
        match self {
            Billed::Org(login) | Billed::User(login) => login,
        }
    }

    fn prefix(&self) -> &'static str {
        match self {
            Billed::Org(_) => "organizations",
            Billed::User(_) => "users",
        }
    }
}

/// One month's reads, as separate results: a failed premium-request read never
/// hides the AI credits, or the reverse.
#[derive(Debug, Clone, PartialEq)]
pub struct CopilotRead {
    pub period: (i32, u32),
    pub ai_credit: Result<Usage, CopilotUsageError>,
    /// `Ok(None)` is "no line, no claim": the endpoint answered 404 (retired
    /// for the account) or the report held no items. A plan that is not on
    /// premium requests has nothing to say here.
    pub premium_request: Result<Option<Usage>, CopilotUsageError>,
}

pub struct CopilotUsageClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl fmt::Debug for CopilotUsageClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CopilotUsageClient")
            .field("base_url", &self.base_url)
            .field("token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl CopilotUsageClient {
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
        Some(CopilotUsageClient {
            base_url: base_url.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
        })
    }

    /// Both reads for `now`'s month, folded.
    pub async fn month_to_date(&self, billed: Billed<'_>, now: DateTime<Utc>) -> CopilotRead {
        let (year, month) = (now.year(), now.month());
        let ai_credit = self
            .items(billed, "ai_credit", year, month)
            .await
            .map(|items| summarize(&items));
        let premium_request = match self.items(billed, "premium_request", year, month).await {
            Ok(items) if items.is_empty() => Ok(None),
            Ok(items) => Ok(Some(summarize(&items))),
            Err(CopilotUsageError {
                kind: ErrorKind::NotFound,
                ..
            }) => Ok(None),
            Err(error) => Err(error),
        };
        CopilotRead {
            period: (year, month),
            ai_credit,
            premium_request,
        }
    }

    /// `GET /{organizations|users}/{login}/settings/billing/{report}/usage?year=&month=`.
    ///
    /// # Errors
    /// [`CopilotUsageError`], classified.
    pub async fn items(
        &self,
        billed: Billed<'_>,
        report: &str,
        year: i32,
        month: u32,
    ) -> Result<Vec<UsageItem>, CopilotUsageError> {
        let level = billed.level();
        let body = self.get(billed, report, year, month).await?;
        let parsed: UsageReport = serde_json::from_str(&body)
            .map_err(|_| CopilotUsageError::new(level, ErrorKind::Undecodable))?;
        Ok(parsed.usage_items)
    }

    /// The token owner's own Copilot limit, from `GET /copilot_internal/user`.
    ///
    /// `username` is the account the operator configured: a response for any
    /// other login is `SeatErrorKind::WrongAccount` and its figures are never
    /// returned. This endpoint is undocumented, so it is asked for with the
    /// bearer token only and **no** `X-GitHub-Api-Version` header (the version
    /// the billing reads pin is not this endpoint's).
    ///
    /// # Errors
    /// [`SeatError`], classified. Never carries the token, the URL or a body.
    pub async fn seat(&self, username: &str) -> Result<Seat, SeatError> {
        let response = self
            .http
            .get(format!("{}/copilot_internal/user", self.base_url))
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::USER_AGENT, "solador")
            .send()
            .await
            .map_err(|_| SeatError::new(SeatErrorKind::Request(ErrorKind::Unreachable)))?;
        let status = response.status().as_u16();
        let rate_exhausted = response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim() == "0");
        if let Some(kind) = classify(status, rate_exhausted) {
            return Err(SeatError::new(SeatErrorKind::Request(kind)));
        }
        let body = response
            .text()
            .await
            .map_err(|_| SeatError::new(SeatErrorKind::Request(ErrorKind::Unreachable)))?;
        let parsed: SeatBody = serde_json::from_str(&body)
            .map_err(|_| SeatError::new(SeatErrorKind::Request(ErrorKind::Undecodable)))?;
        interpret(&parsed, username)
    }

    async fn get(
        &self,
        billed: Billed<'_>,
        report: &str,
        year: i32,
        month: u32,
    ) -> Result<String, CopilotUsageError> {
        let level = billed.level();
        let fail = |kind| CopilotUsageError::new(level, kind);
        let login = billed.login();
        // `url` resolves `.` and `..` segments even when percent-encoded, so
        // these would walk out of the collection. No such login exists.
        if matches!(login.trim(), "" | "." | "..") {
            return Err(fail(ErrorKind::NotFound));
        }
        let login = utf8_percent_encode(login, PATH_SEGMENT);
        let response = self
            .http
            .get(format!(
                "{}/{}/{login}/settings/billing/{report}/usage",
                self.base_url,
                billed.prefix()
            ))
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header(reqwest::header::USER_AGENT, "solador")
            .header("X-GitHub-Api-Version", API_VERSION)
            .query(&[("year", year.to_string()), ("month", month.to_string())])
            .send()
            .await
            .map_err(|_| fail(ErrorKind::Unreachable))?;
        let status = response.status().as_u16();
        let rate_exhausted = response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim() == "0");
        if let Some(kind) = classify(status, rate_exhausted) {
            return Err(fail(kind));
        }
        response
            .text()
            .await
            .map_err(|_| fail(ErrorKind::Unreachable))
    }
}

/// `None` for a 2xx.
fn classify(status: u16, rate_exhausted: bool) -> Option<ErrorKind> {
    match status {
        200..=299 => None,
        401 => Some(ErrorKind::Unauthorized),
        403 if rate_exhausted => Some(ErrorKind::RateLimited),
        403 => Some(ErrorKind::Forbidden),
        404 => Some(ErrorKind::NotFound),
        429 => Some(ErrorKind::RateLimited),
        500..=599 => Some(ErrorKind::VendorFailure { status }),
        _ => Some(ErrorKind::UnexpectedStatus { status }),
    }
}
