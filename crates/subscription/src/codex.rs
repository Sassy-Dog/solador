//! Codex plan usage: Codex's saved ChatGPT sign-in, read only, and one `GET`
//! of `chatgpt.com/backend-api/wham/usage`.
//!
//! This is orca's **fallback** path (`main/rate-limits/codex-backend-usage-client.ts`).
//! orca's primary path spawns `codex app-server`
//! (`codex-cli/codex-read-only-app-server-args.ts:12-17`). That is a real Codex
//! process, which refreshes its ChatGPT login and rewrites `auth.json` when it
//! needs to (`codex-rpc-rate-limit-probe.ts:294`). Solador must never be the
//! cause of a write to another program's credential, so it never spawns Codex.
//!
//! # Where the sign-in is
//!
//! `$CODEX_HOME/auth.json`, else `~/.codex/auth.json`, on every platform
//! (`codex-backend-auth.ts:62-64,73`). Fields read:
//! `{ tokens: { access_token, account_id } }` (`codex-backend-auth.ts:14-19`).
//! Codex configured to keep its login in the OS keyring
//! (`cli_auth_credentials_store = "keyring"`) has no file, so it reads as not
//! signed in.
//!
//! # Where this parts from orca
//!
//! - **The `User-Agent` is `Solador`.** orca also sends `User-Agent:
//!   codex-cli` and `originator: Codex Desktop` (`codex-backend-auth.ts:82-84`).
//!   Only the protocol headers are kept: `OpenAI-Beta: codex-1` and
//!   `ChatGPT-Account-Id`.
//! - **401 and 403 are an expired sign-in.** orca has no HTTP classification
//!   here, because any non-2xx falls back to its RPC path (`:70-73`).
//! - **A window whose length is not 5 hours or a week keeps its slot name.**
//!   orca maps an unknown primary to "session" and an unknown secondary to
//!   "weekly" (`codex-rate-limit-window-classification.ts:72-78`). That would
//!   label a window with a length it never stated.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::{
    http, used_percent, AccessToken, PlanErrorKind, PlanUsage, PlanUsageError, PlanWindow,
    Provider, Slot, WindowSpan, SESSION_MINUTES, WEEK_MINUTES,
};

/// Production origin. Scheme and host only: the token travels in a header.
pub const DEFAULT_BASE_URL: &str = "https://chatgpt.com";

/// orca's tolerance for "older Codex bucket lengths"
/// (`codex-rate-limit-window-classification.ts:4-5`).
const TOLERANCE_MINUTES: u32 = 1;

/// A ChatGPT login borrowed from `auth.json` for one request.
pub struct Login {
    token: AccessToken,
    account_id: Option<String>,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login")
            .field("token", &self.token)
            .field("has_account_id", &self.account_id.is_some())
            .finish()
    }
}

/// `auth.json` under `codex_home` if set (`$CODEX_HOME`), else under
/// `home/.codex`. `None` when neither is known.
#[must_use]
pub fn auth_file(codex_home: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    codex_home
        .map(Path::to_path_buf)
        .or_else(|| home.map(|h| h.join(".codex")))
        .map(|dir| dir.join("auth.json"))
}

/// Reads the login from `auth.json`. Reads only.
///
/// # Errors
/// [`PlanErrorKind::NotSignedIn`] when there is no file,
/// [`PlanErrorKind::CredentialUnreadable`] when it cannot be read or parsed,
/// and [`PlanErrorKind::NoSubscription`] when it holds no ChatGPT access token
/// (an API-key login).
pub fn read_login(path: Option<&Path>) -> Result<Login, PlanErrorKind> {
    let path = path.ok_or(PlanErrorKind::NotSignedIn)?;
    let raw = std::fs::read_to_string(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => PlanErrorKind::NotSignedIn,
        _ => PlanErrorKind::CredentialUnreadable,
    })?;
    // Parsed as a `Value` and dropped at the end of this function: the
    // document also holds the refresh token, which is never looked at.
    let doc: Value = serde_json::from_str(&raw).map_err(|_| PlanErrorKind::CredentialUnreadable)?;
    if !doc.is_object() {
        return Err(PlanErrorKind::CredentialUnreadable);
    }
    let field = |name: &str| {
        doc.pointer(&format!("/tokens/{name}"))
            .and_then(Value::as_str)
    };
    let token = field("access_token")
        .and_then(AccessToken::new)
        .ok_or(PlanErrorKind::NoSubscription)?;
    let account_id = field("account_id")
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    Ok(Login { token, account_id })
}

/// This machine's `auth.json`. The one production entry point; **no test
/// calls it**. The shell calls it only behind the opt-in.
///
/// # Errors
/// See [`read_login`].
pub fn read_this_machine() -> Result<Login, PlanErrorKind> {
    let codex_home = std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let home = dirs::home_dir();
    read_login(auth_file(codex_home.as_deref(), home.as_deref()).as_deref())
}

/// The windows in one `/backend-api/wham/usage` body.
///
/// # Errors
/// [`PlanErrorKind::Undecodable`] for a body that is not a JSON object or has
/// no string `plan_type`: orca treats that as "not a usage reply"
/// (`codex-backend-usage-client.ts:75-77`).
pub fn parse_usage(body: &str) -> Result<PlanUsage, PlanErrorKind> {
    let doc: Value =
        serde_json::from_str(body).map_err(|e| PlanErrorKind::Undecodable(e.to_string()))?;
    if doc.get("plan_type").and_then(Value::as_str).is_none() {
        return Err(PlanErrorKind::Undecodable("no plan_type".into()));
    }
    let windows = [
        ("primary_window", Slot::Primary),
        ("secondary_window", Slot::Secondary),
    ]
    .into_iter()
    .filter_map(|(key, slot)| window(doc.get("rate_limit")?.get(key)?, slot))
    .collect();
    Ok(PlanUsage { windows })
}

/// One `{ used_percent, limit_window_seconds, reset_at }` window
/// (`codex-backend-usage-client.ts:19-23,34-48`). No percentage is no window
/// (`codex-rate-limit-window-classification.ts:27-31`).
fn window(raw: &Value, slot: Slot) -> Option<PlanWindow> {
    let used = used_percent(raw.get("used_percent"))?;
    Some(PlanWindow {
        span: span(
            raw.get("limit_window_seconds").and_then(Value::as_f64),
            slot,
        ),
        scope: None,
        used_percent: used,
        // Unix seconds (`codex-rate-limit-window-mapper.ts:14-16`).
        resets_at: raw
            .get("reset_at")
            .and_then(Value::as_i64)
            .and_then(|secs| DateTime::from_timestamp(secs, 0)),
    })
}

/// `ceil(seconds / 60)` minutes, with one minute of drift folded onto the
/// session and the week. No positive length is [`WindowSpan::Unstated`].
fn span(seconds: Option<f64>, slot: Slot) -> WindowSpan {
    let Some(seconds) = seconds.filter(|s| s.is_finite() && *s > 0.0) else {
        return WindowSpan::Unstated(slot);
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let minutes = (seconds / 60.0).ceil().min(f64::from(u32::MAX)) as u32;
    for canonical in [SESSION_MINUTES, WEEK_MINUTES] {
        if minutes.abs_diff(canonical) <= TOLERANCE_MINUTES {
            return WindowSpan::Minutes(canonical);
        }
    }
    WindowSpan::Minutes(minutes)
}

/// A client for the usage endpoint.
pub struct Client {
    base_url: String,
    http: reqwest::Client,
}

impl Default for Client {
    fn default() -> Self {
        Self::with_base_url(DEFAULT_BASE_URL)
    }
}

impl Client {
    /// # Invariant: no credentials in `base_url`
    ///
    /// Exists so tests (and only tests) can point the client at a mock server.
    #[must_use]
    pub fn with_base_url(base_url: &str) -> Self {
        Client {
            base_url: base_url.trim_end_matches('/').to_owned(),
            http: http::client(),
        }
    }

    /// One read of the plan's windows. `login` is borrowed for this request.
    ///
    /// # Errors
    /// [`PlanUsageError`] for a rejected, throttled, failed or unreadable read.
    pub async fn usage(
        &self,
        login: &Login,
        now: DateTime<Utc>,
    ) -> Result<PlanUsage, PlanUsageError> {
        let mut request = self
            .http
            .get(format!("{}/backend-api/wham/usage", self.base_url))
            .bearer_auth(login.token.expose())
            .header("OpenAI-Beta", "codex-1");
        if let Some(account_id) = &login.account_id {
            request = request.header("ChatGPT-Account-Id", account_id);
        }
        let body = http::send(request, now, |_| PlanErrorKind::SignInExpired)
            .await
            .map_err(|kind| PlanUsageError::new(Provider::Codex, kind))?;
        parse_usage(&body).map_err(|kind| PlanUsageError::new(Provider::Codex, kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The raw fields `fetchCodexRateLimitsViaBackend` reads, and only those:
    /// `plan_type`, `rate_limit.primary_window` / `secondary_window` with
    /// `used_percent`, `limit_window_seconds` and `reset_at` in Unix seconds,
    /// and `rate_limit_reset_credits` (`codex-backend-usage-client.ts:19-32`).
    const USAGE: &str = include_str!("../tests/fixtures/codex_usage.json");

    fn at(ts: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(ts).expect("timestamp").into()
    }

    // ---- parsing ---------------------------------------------------------

    #[test]
    fn the_fixture_reads_as_a_session_and_a_week() {
        let usage = parse_usage(USAGE).expect("parses");
        assert_eq!(
            usage.windows,
            vec![
                PlanWindow {
                    span: WindowSpan::Minutes(SESSION_MINUTES),
                    scope: None,
                    used_percent: 18.0,
                    resets_at: Some(at("2026-10-24T16:30:00Z")),
                },
                PlanWindow {
                    span: WindowSpan::Minutes(WEEK_MINUTES),
                    scope: None,
                    used_percent: 9.0,
                    resets_at: Some(at("2026-10-28T12:00:00Z")),
                },
            ]
        );
    }

    /// `ceil(seconds / 60)`, then ±1 minute onto 300 or 10080
    /// (`codex-backend-usage-client.ts:40-46`,
    /// `codex-rate-limit-window-classification.ts:33-47`).
    #[test]
    fn window_lengths_round_up_to_minutes_and_fold_one_minute_of_drift() {
        let span = |secs: &str| {
            let body = format!(
                r#"{{"plan_type":"pro","rate_limit":{{"primary_window":{{"used_percent":1,"limit_window_seconds":{secs}}}}}}}"#
            );
            parse_usage(&body).expect("parses").windows[0].span
        };
        assert_eq!(span("17940"), WindowSpan::Minutes(SESSION_MINUTES));
        assert_eq!(span("18059"), WindowSpan::Minutes(SESSION_MINUTES));
        assert_eq!(span("604860"), WindowSpan::Minutes(WEEK_MINUTES));
        // 61 s is two minutes, by `Math.ceil`.
        assert_eq!(span("61"), WindowSpan::Minutes(2));
        assert_eq!(span("86400"), WindowSpan::Minutes(1440));
    }

    /// No stated length keeps the slot name; it never borrows "5h" or "week".
    #[test]
    fn a_window_with_no_stated_length_keeps_its_slot() {
        let usage = parse_usage(
            r#"{"plan_type":"plus","rate_limit":{
                "primary_window":{"used_percent":3,"limit_window_seconds":0},
                "secondary_window":{"used_percent":4}}}"#,
        )
        .expect("parses");
        let spans: Vec<_> = usage.windows.iter().map(|w| w.span).collect();
        assert_eq!(
            spans,
            [
                WindowSpan::Unstated(Slot::Primary),
                WindowSpan::Unstated(Slot::Secondary)
            ]
        );
    }

    #[test]
    fn a_window_without_a_percentage_is_absent_and_a_missing_reset_is_unknown() {
        let usage = parse_usage(
            r#"{"plan_type":"plus","rate_limit":{
                "primary_window":{"limit_window_seconds":18000,"reset_at":1792859400},
                "secondary_window":{"used_percent":-1,"limit_window_seconds":604800},
                "extra":{"used_percent":50}}}"#,
        )
        .expect("parses");
        assert!(usage.is_empty());

        let no_reset = parse_usage(
            r#"{"plan_type":"plus","rate_limit":{"primary_window":{"used_percent":5,"reset_at":"soon"}}}"#,
        )
        .expect("parses");
        assert_eq!(no_reset.windows[0].resets_at, None);
    }

    #[test]
    fn a_reply_with_no_rate_limit_is_no_windows_not_an_error() {
        for body in [
            r#"{"plan_type":"free"}"#,
            r#"{"plan_type":"free","rate_limit":null}"#,
            r#"{"plan_type":"free","rate_limit":{"primary_window":null,"secondary_window":null}}"#,
        ] {
            assert!(parse_usage(body).expect("parses").is_empty(), "{body}");
        }
    }

    #[test]
    fn a_reply_without_a_plan_type_is_not_a_usage_reply() {
        for body in [
            "",
            "[]",
            r#"{"rate_limit":{"primary_window":{"used_percent":5}}}"#,
            r#"{"plan_type":7}"#,
        ] {
            assert!(
                matches!(parse_usage(body), Err(PlanErrorKind::Undecodable(_))),
                "{body}"
            );
        }
    }

    // ---- credential discovery -------------------------------------------

    #[test]
    fn codex_home_wins_over_the_home_directory() {
        assert_eq!(
            auth_file(Some(Path::new("/c")), Some(Path::new("/h"))),
            Some(PathBuf::from("/c/auth.json"))
        );
        assert_eq!(
            auth_file(None, Some(Path::new("/h"))),
            Some(PathBuf::from("/h/.codex/auth.json"))
        );
        assert_eq!(auth_file(None, None), None);
    }

    fn write(dir: &Path, raw: &str) -> PathBuf {
        let path = dir.join("auth.json");
        std::fs::write(&path, raw).unwrap();
        path
    }

    #[test]
    fn the_login_is_the_access_token_and_the_account_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            r#"{"OPENAI_API_KEY":null,"tokens":{"id_token":"x","access_token":"at-1","refresh_token":"rt","account_id":"acct-9"},"last_refresh":"2026-10-01T00:00:00Z"}"#,
        );
        let login = read_login(Some(&path)).expect("login");
        assert_eq!(login.token.expose(), "at-1");
        assert_eq!(login.account_id.as_deref(), Some("acct-9"));
        let printed = format!("{login:?}");
        assert!(
            !printed.contains("at-1") && !printed.contains("acct-9"),
            "{printed}"
        );
    }

    #[test]
    fn with_no_token_the_failure_says_why() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read_login(Some(&dir.path().join("missing.json"))).expect_err("none"),
            PlanErrorKind::NotSignedIn
        );
        assert_eq!(
            read_login(None).expect_err("none"),
            PlanErrorKind::NotSignedIn
        );
        assert_eq!(
            read_login(Some(&write(dir.path(), "{"))).expect_err("bad"),
            PlanErrorKind::CredentialUnreadable
        );
        assert_eq!(
            read_login(Some(&write(
                dir.path(),
                r#"{"OPENAI_API_KEY":"sk-x","tokens":null}"#
            )))
            .expect_err("api key"),
            PlanErrorKind::NoSubscription
        );
        // A directory where the file should be is unreadable, not absent.
        let as_dir = dir.path().join("dir");
        std::fs::create_dir(&as_dir).unwrap();
        assert_eq!(
            read_login(Some(&as_dir)).expect_err("dir"),
            PlanErrorKind::CredentialUnreadable
        );
    }

    // ---- the request -----------------------------------------------------

    fn login(account_id: Option<&str>) -> Login {
        Login {
            token: AccessToken::new("at-SECRET").expect("token"),
            account_id: account_id.map(str::to_owned),
        }
    }

    const NOW: &str = "2026-10-24T12:00:00Z";

    #[tokio::test]
    async fn the_request_carries_the_bearer_the_account_and_the_beta_header() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/backend-api/wham/usage"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer at-SECRET",
            ))
            .and(wiremock::matchers::header("chatgpt-account-id", "acct-9"))
            .and(wiremock::matchers::header("openai-beta", "codex-1"))
            .and(wiremock::matchers::header("user-agent", crate::USER_AGENT))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_raw(USAGE, "application/json"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let usage = Client::with_base_url(&server.uri())
            .usage(&login(Some("acct-9")), at(NOW))
            .await
            .expect("usage");
        assert_eq!(usage.windows.len(), 2);
    }

    /// orca sends the account header only when there is one (`:86-88`).
    #[tokio::test]
    async fn no_account_id_sends_no_account_header() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::header_exists("chatgpt-account-id"))
            .respond_with(wiremock::ResponseTemplate::new(500))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_raw(USAGE, "application/json"),
            )
            .mount(&server)
            .await;
        assert!(Client::with_base_url(&server.uri())
            .usage(&login(None), at(NOW))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn each_rejection_is_classified_and_none_carries_the_token() {
        use wiremock::ResponseTemplate as R;
        for (template, expected) in [
            (R::new(401), PlanErrorKind::SignInExpired),
            (R::new(403), PlanErrorKind::SignInExpired),
            (
                R::new(429).insert_header("retry-after", "30"),
                PlanErrorKind::RateLimited {
                    retry_after_secs: Some(30),
                },
            ),
            (R::new(502), PlanErrorKind::Http { status: 502 }),
            (
                R::new(301).insert_header("location", "https://example.com/"),
                PlanErrorKind::Http { status: 301 },
            ),
        ] {
            let server = wiremock::MockServer::start().await;
            wiremock::Mock::given(wiremock::matchers::any())
                .respond_with(template)
                .mount(&server)
                .await;
            let err = Client::with_base_url(&server.uri())
                .usage(&login(Some("acct-9")), at(NOW))
                .await
                .expect_err("fails");
            assert_eq!(err.provider, Provider::Codex);
            assert_eq!(err.kind, expected);
            for printed in [format!("{err}"), format!("{err:?}"), err.user_message()] {
                assert!(!printed.contains("at-SECRET"), "{printed}");
            }
        }
    }
}
