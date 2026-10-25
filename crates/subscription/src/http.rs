//! The one request shape both providers share: a bearer `GET`, no redirects,
//! a 10-second budget, and a status classification that never reads a body
//! into an error.

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::{retry, PlanErrorKind, USER_AGENT};

/// orca's budget for both endpoints (`claude-oauth-usage-request.ts:19`,
/// `codex-backend-auth.ts:10`).
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A client that never follows a redirect: a `302` would carry the bearer
/// token to wherever it points. A redirect is reported as [`PlanErrorKind::Http`].
pub(crate) fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(USER_AGENT)
        .build()
        .expect("reqwest client")
}

/// Sends `request` and returns the body of a 2xx.
///
/// `forbidden` classifies a 403 from its body, because Claude's 403 means two
/// different things (`claude-usage-error-classification.ts:11-34`). The body is
/// looked at and dropped; it never reaches an error.
pub(crate) async fn send(
    request: reqwest::RequestBuilder,
    now: DateTime<Utc>,
    forbidden: fn(&str) -> PlanErrorKind,
) -> Result<String, PlanErrorKind> {
    let response = request
        .send()
        .await
        .map_err(|e| PlanErrorKind::Unreachable(e.without_url().to_string()))?;
    let status = response.status();
    if status.is_success() {
        return response
            .text()
            .await
            .map_err(|e| PlanErrorKind::Unreachable(e.without_url().to_string()));
    }
    Err(match status.as_u16() {
        401 => PlanErrorKind::SignInExpired,
        403 => forbidden(&response.text().await.unwrap_or_default()),
        429 => PlanErrorKind::RateLimited {
            retry_after_secs: retry::retry_after_secs(
                response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok()),
                now,
            ),
        },
        status => PlanErrorKind::Http { status },
    })
}
