//! Claude plan usage: Claude Code's saved sign-in, read only, and one `GET`
//! of `api.anthropic.com/api/oauth/usage`.
//!
//! # Where the sign-in is (orca's order)
//!
//! 1. **macOS only:** the keychain generic password `Claude Code-credentials`,
//!    account `$USER` (or `$USERNAME`). An account name that fails
//!    `^[a-zA-Z0-9._-]+$` becomes `claude-code-user`, as Claude Code itself
//!    does (`main/claude-accounts/keychain.ts:7,24-36`).
//! 2. **Every platform**, and the macOS fallback:
//!    `~/.claude/.credentials.json` (`main/rate-limits/claude-oauth-credentials.ts:95-119`).
//!
//! Both hold the same JSON, `{ claudeAiOauth: { accessToken, refreshToken } }`
//! (`claude-oauth-credentials.ts:29-36`). Only `accessToken` is used. The
//! refresh token is looked at only to tell an expired login from no login, and
//! it is never sent anywhere. `expiresAt` is ignored, as orca ignores it: "let
//! the server decide" (`:43`).
//!
//! # Where this parts from orca
//!
//! - **The `User-Agent` is `Solador`**, not orca's `claude-code/2.1.0`.
//! - **Model-scoped weeklies are read for any model name**, not only Fable.
//! - **A percentage above 100 is kept**, and a negative one is unknown. orca
//!   clamps both to 0–100 (`claude-usage-window.ts:71`), which would turn a
//!   corrupt `-5` into a measured 0%.
//! - **`CLAUDE_CONFIG_DIR` profiles are not read.** Their keychain service
//!   carries a hash suffix (`keychain.ts:38-45`). Only the default login is
//!   read.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::keychain::{KeychainError, KeychainReader};
use crate::{
    http, used_percent, AccessToken, PlanErrorKind, PlanUsage, PlanUsageError, PlanWindow,
    Provider, WindowSpan, SESSION_MINUTES, WEEK_MINUTES,
};

/// The keychain service Claude Code stores its default login under.
pub const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// The account Claude Code uses when the login name is not a safe one.
pub const FALLBACK_ACCOUNT: &str = "claude-code-user";

/// Production origin. Scheme and host only: the token travels in a header.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// The beta header the endpoint requires (`claude-oauth-usage-request.ts:85`).
const BETA_HEADER: &str = "oauth-2025-04-20";

/// The keychain account for a login name, as Claude Code derives it.
#[must_use]
pub fn keychain_account(user: Option<&str>) -> String {
    match user {
        Some(user)
            if !user.is_empty()
                && user
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')) =>
        {
            user.to_owned()
        }
        _ => FALLBACK_ACCOUNT.to_owned(),
    }
}

/// `~/.claude/.credentials.json` under `home`.
#[must_use]
pub fn credentials_file(home: &Path) -> PathBuf {
    home.join(".claude").join(".credentials.json")
}

/// What one location held.
enum Found {
    Token(AccessToken),
    /// A login with a refresh token and no access token. Claude Code renews it
    /// on its next run.
    RefreshOnly,
    /// A document with no OAuth login in it: an API-key user, or a logged-out
    /// one whose item still holds other entries.
    NoOauth,
    Absent,
    Unreadable,
    Refused,
}

fn classify_document(raw: &str) -> Found {
    let Ok(doc) = serde_json::from_str::<Value>(raw) else {
        return Found::Unreadable;
    };
    if !doc.is_object() {
        return Found::Unreadable;
    }
    let oauth = doc.get("claudeAiOauth");
    let field = |name: &str| oauth.and_then(|o| o.get(name)).and_then(Value::as_str);
    if let Some(token) = field("accessToken").and_then(AccessToken::new) {
        return Found::Token(token);
    }
    if field("refreshToken").is_some_and(|t| !t.trim().is_empty()) {
        return Found::RefreshOnly;
    }
    Found::NoOauth
}

fn from_keychain(reader: &dyn KeychainReader, account: &str) -> Found {
    match reader.generic_password(KEYCHAIN_SERVICE, Some(account)) {
        Ok(Some(text)) => classify_document(text.expose()),
        Ok(None) => Found::Absent,
        Err(KeychainError::Empty) => Found::Unreadable,
        Err(_) => Found::Refused,
    }
}

fn from_file(path: &Path) -> Found {
    match std::fs::read_to_string(path) {
        Ok(raw) => classify_document(&raw),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Found::Absent,
        Err(_) => Found::Unreadable,
    }
}

/// Claude Code's access token, from the keychain (when given) and then the
/// file (when given). Reads only.
///
/// A token anywhere wins. Otherwise the failure reported is the one that most
/// needs the operator: a refused keychain, then an unreadable sign-in, then an
/// expired one, then a login with no subscription, then no sign-in at all.
///
/// # Errors
/// The [`PlanErrorKind`] for why no token was found.
pub fn read_token(
    keychain: Option<(&dyn KeychainReader, &str)>,
    file: Option<&Path>,
) -> Result<AccessToken, PlanErrorKind> {
    let mut found = Vec::with_capacity(2);
    if let Some((reader, account)) = keychain {
        match from_keychain(reader, account) {
            Found::Token(token) => return Ok(token),
            other => found.push(other),
        }
    }
    if let Some(path) = file {
        match from_file(path) {
            Found::Token(token) => return Ok(token),
            other => found.push(other),
        }
    }
    let any = |pred: fn(&Found) -> bool| found.iter().any(pred);
    Err(if any(|f| matches!(f, Found::Refused)) {
        PlanErrorKind::KeychainRefused
    } else if any(|f| matches!(f, Found::Unreadable)) {
        PlanErrorKind::CredentialUnreadable
    } else if any(|f| matches!(f, Found::RefreshOnly)) {
        PlanErrorKind::SignInExpired
    } else if any(|f| matches!(f, Found::NoOauth)) {
        PlanErrorKind::NoSubscription
    } else {
        PlanErrorKind::NotSignedIn
    })
}

/// This machine's sources: the real keychain on macOS, and the file under the
/// real home directory everywhere.
///
/// The one production entry point. **No test calls it**, because it reads the
/// operator's real credential. The shell calls it only behind the opt-in.
///
/// # Errors
/// See [`read_token`].
pub fn read_this_machine() -> Result<AccessToken, PlanErrorKind> {
    let file = dirs::home_dir().map(|home| credentials_file(&home));
    #[cfg(target_os = "macos")]
    {
        let user = std::env::var("USER")
            .ok()
            .or_else(|| std::env::var("USERNAME").ok());
        let account = keychain_account(user.as_deref());
        let reader = crate::keychain::SecurityCli::default();
        read_token(Some((&reader, &account)), file.as_deref())
    }
    #[cfg(not(target_os = "macos"))]
    {
        read_token(None, file.as_deref())
    }
}

/// `resets_at` as orca reads it (`claude-usage-window.ts:10-30`): a number, or
/// a numeric string, is epoch seconds, or milliseconds above 1e10; anything
/// else is an RFC 3339 instant. Unparseable is `None`, never "now".
fn reset_instant(value: Option<&Value>) -> Option<DateTime<Utc>> {
    let epoch = |n: f64| -> Option<DateTime<Utc>> {
        if !n.is_finite() {
            return None;
        }
        let millis = if n > 1e10 { n } else { n * 1000.0 };
        #[allow(clippy::cast_possible_truncation)]
        DateTime::from_timestamp_millis(millis.round() as i64)
    };
    match value? {
        Value::Number(n) => epoch(n.as_f64()?),
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            match s.parse::<f64>() {
                Ok(n) => epoch(n),
                Err(_) => DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|t| t.with_timezone(&Utc)),
            }
        }
        _ => None,
    }
}

/// One `{ utilization | used_percentage, resets_at }` window
/// (`claude-usage-window.ts:54-69`). No percentage is no window.
fn window(raw: Option<&Value>, scope: Option<&str>) -> Option<PlanWindow> {
    let raw = raw?;
    let used = used_percent(raw.get("utilization"))
        .or_else(|| used_percent(raw.get("used_percentage")))?;
    Some(PlanWindow {
        span: WindowSpan::Minutes(WEEK_MINUTES),
        scope: scope.map(str::to_owned),
        used_percent: used,
        resets_at: reset_instant(raw.get("resets_at")),
    })
}

/// The windows in one `/api/oauth/usage` body.
///
/// # Errors
/// [`PlanErrorKind::Undecodable`] for a body that is not a JSON object.
pub fn parse_usage(body: &str) -> Result<PlanUsage, PlanErrorKind> {
    let doc: Value =
        serde_json::from_str(body).map_err(|e| PlanErrorKind::Undecodable(e.to_string()))?;
    if !doc.is_object() {
        return Err(PlanErrorKind::Undecodable("not a JSON object".into()));
    }
    let mut windows = Vec::new();

    // `five_hour` and `seven_day` (`claude-oauth-usage-request.ts:30-31`).
    if let Some(mut w) = window(doc.get("five_hour"), None) {
        w.span = WindowSpan::Minutes(SESSION_MINUTES);
        windows.push(w);
    }
    windows.extend(window(doc.get("seven_day"), None));

    // `limits[]`'s weekly_scoped entries (`claude-oauth-usage-request.ts:46-54`),
    // for any model name, first entry per name.
    let mut scoped: Vec<String> = Vec::new();
    for limit in doc
        .get("limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if limit.get("kind").and_then(Value::as_str) != Some("weekly_scoped") {
            continue;
        }
        let Some(name) = limit
            .pointer("/scope/model/display_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty())
        else {
            continue;
        };
        let Some(used) = used_percent(limit.get("percent")) else {
            continue;
        };
        if scoped.iter().any(|seen| seen.eq_ignore_ascii_case(name)) {
            continue;
        }
        scoped.push(name.to_owned());
        windows.push(PlanWindow {
            span: WindowSpan::Minutes(WEEK_MINUTES),
            scope: Some(name.to_owned()),
            used_percent: used,
            resets_at: reset_instant(limit.get("resets_at")),
        });
    }

    // orca's three legacy Fable keys, only when `limits[]` named no Fable
    // window (`claude-oauth-usage-request.ts:55-63`).
    if !scoped.iter().any(|n| n.eq_ignore_ascii_case("fable")) {
        let legacy = ["fable_weekly", "fable_seven_day", "seven_day_fable"]
            .iter()
            .find_map(|key| window(doc.get(*key), Some("Fable")));
        windows.extend(legacy);
    }

    Ok(PlanUsage { windows })
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
    /// Scheme, host and port only. Exists so tests (and only tests) can point
    /// the client at a mock server.
    #[must_use]
    pub fn with_base_url(base_url: &str) -> Self {
        Client {
            base_url: base_url.trim_end_matches('/').to_owned(),
            http: http::client(),
        }
    }

    /// One read of the plan's windows. `token` is borrowed for this request.
    ///
    /// # Errors
    /// [`PlanUsageError`] for a rejected, throttled, failed or unreadable read.
    pub async fn usage(
        &self,
        token: &AccessToken,
        now: DateTime<Utc>,
    ) -> Result<PlanUsage, PlanUsageError> {
        let request = self
            .http
            .get(format!("{}/api/oauth/usage", self.base_url))
            .bearer_auth(token.expose())
            .header("anthropic-beta", BETA_HEADER);
        let body = http::send(request, now, forbidden)
            .await
            .map_err(|kind| PlanUsageError::new(Provider::Claude, kind))?;
        parse_usage(&body).map_err(|kind| PlanUsageError::new(Provider::Claude, kind))
    }
}

/// A 403 naming the `user:profile` scope is a token that cannot read usage
/// at all (a `claude setup-token` one). Any other 403 is a revoked or
/// expired login (`claude-usage-error-classification.ts:11-34`).
fn forbidden(body: &str) -> PlanErrorKind {
    if body.contains("user:profile") {
        PlanErrorKind::MissingScope
    } else {
        PlanErrorKind::SignInExpired
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::SecretText;
    use std::cell::RefCell;

    /// The raw fields `fetchClaudeOAuthUsage` reads, and only those:
    /// `five_hour`/`seven_day` windows (`claude-oauth-usage-request.ts:29-38`,
    /// window shape `claude-usage-window.ts:4-8`) and a `weekly_scoped`
    /// `limits[]` entry (`claude-oauth-usage-request.ts:21-27,46-54`). The
    /// five-hour reset is a sub-second ISO string and the scoped reset is
    /// epoch seconds, the two forms `parseClaudeUsageResetTimestamp` accepts.
    const USAGE: &str = include_str!("../tests/fixtures/claude_usage.json");

    fn at(ts: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(ts).expect("timestamp").into()
    }

    // ---- parsing ---------------------------------------------------------

    #[test]
    fn the_fixture_reads_as_session_week_and_the_fable_weekly() {
        let usage = parse_usage(USAGE).expect("parses");
        assert_eq!(
            usage.windows,
            vec![
                PlanWindow {
                    span: WindowSpan::Minutes(SESSION_MINUTES),
                    scope: None,
                    used_percent: 42.0,
                    resets_at: Some(at("2026-10-24T17:00:00.123456Z")),
                },
                PlanWindow {
                    span: WindowSpan::Minutes(WEEK_MINUTES),
                    scope: None,
                    used_percent: 31.5,
                    resets_at: Some(at("2026-10-29T09:00:00Z")),
                },
                PlanWindow {
                    span: WindowSpan::Minutes(WEEK_MINUTES),
                    scope: Some("Fable".into()),
                    used_percent: 12.0,
                    resets_at: Some(at("2026-10-30T09:00:00Z")),
                },
            ]
        );
    }

    /// `used_percentage` is the fallback key; neither key is no window, never 0%.
    #[test]
    fn a_window_without_a_percentage_is_absent_not_zero() {
        let usage = parse_usage(
            r#"{"five_hour":{"used_percentage":7,"resets_at":null},
                "seven_day":{"resets_at":"2026-10-29T09:00:00Z"}}"#,
        )
        .expect("parses");
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.windows[0].used_percent, 7.0);
        assert_eq!(usage.windows[0].resets_at, None);
    }

    #[test]
    fn null_windows_and_an_empty_body_are_no_windows_not_an_error() {
        let usage =
            parse_usage(r#"{"five_hour":null,"seven_day":null,"limits":null}"#).expect("parses");
        assert!(usage.is_empty());
        assert!(parse_usage("{}").expect("parses").is_empty());
    }

    /// A negative percentage is corrupt, so unknown. Above 100 is kept: the
    /// service saying 104% is saying something true.
    #[test]
    fn a_negative_percentage_is_unknown_and_an_overrun_is_kept() {
        let usage =
            parse_usage(r#"{"five_hour":{"utilization":-5},"seven_day":{"utilization":104}}"#)
                .expect("parses");
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.windows[0].span, WindowSpan::Minutes(WEEK_MINUTES));
        assert_eq!(usage.windows[0].used_percent, 104.0);
    }

    /// `parseClaudeUsageResetTimestamp`'s three forms, plus what it rejects.
    #[test]
    fn reset_instants_read_seconds_milliseconds_and_iso_and_nothing_else() {
        let reset = |v: &str| reset_instant(Some(&serde_json::from_str::<Value>(v).unwrap()));
        let expected = Some(at("2026-10-30T09:00:00Z"));
        assert_eq!(reset("1793350800"), expected);
        assert_eq!(reset("1793350800000"), expected);
        assert_eq!(reset(r#""1793350800""#), expected);
        assert_eq!(reset(r#""2026-10-30T09:00:00Z""#), expected);
        for unknown in [r#""""#, r#""soon""#, "null", "true", "{}"] {
            assert_eq!(reset(unknown), None, "{unknown}");
        }
        assert_eq!(reset_instant(None), None);
    }

    #[test]
    fn scoped_weeklies_are_read_for_any_model_and_other_kinds_are_skipped() {
        let usage = parse_usage(
            r#"{"limits":[
                {"kind":"weekly_scoped","percent":55,"scope":{"model":{"display_name":"Opus"}}},
                {"kind":"weekly_scoped","percent":60,"scope":{"model":{"display_name":"opus"}}},
                {"kind":"weekly_scoped","percent":5,"scope":{"model":null}},
                {"kind":"weekly_scoped","scope":{"model":{"display_name":"Haiku"}}},
                {"kind":"daily","percent":9,"scope":{"model":{"display_name":"Fable"}}}
            ]}"#,
        )
        .expect("parses");
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.windows[0].scope.as_deref(), Some("Opus"));
        assert_eq!(usage.windows[0].used_percent, 55.0);
    }

    /// orca's legacy keys are honoured only when `limits[]` named no Fable.
    #[test]
    fn the_legacy_fable_keys_are_a_fallback_only() {
        let legacy = parse_usage(r#"{"seven_day_fable":{"utilization":20}}"#).expect("parses");
        assert_eq!(legacy.windows.len(), 1);
        assert_eq!(legacy.windows[0].scope.as_deref(), Some("Fable"));

        let both = parse_usage(
            r#"{"fable_weekly":{"utilization":99},
                "limits":[{"kind":"weekly_scoped","percent":12,"scope":{"model":{"display_name":"Fable"}}}]}"#,
        )
        .expect("parses");
        assert_eq!(both.windows.len(), 1);
        assert_eq!(both.windows[0].used_percent, 12.0);
    }

    #[test]
    fn a_body_that_is_not_an_object_is_undecodable() {
        for body in ["", "not json", "[]", "42"] {
            assert!(
                matches!(parse_usage(body), Err(PlanErrorKind::Undecodable(_))),
                "{body}"
            );
        }
    }

    // ---- credential discovery -------------------------------------------

    /// A keychain that answers from a script and records what it was asked.
    struct FakeKeychain {
        answer: fn() -> Result<Option<SecretText>, KeychainError>,
        asked: RefCell<Vec<(String, Option<String>)>>,
    }

    impl FakeKeychain {
        fn new(answer: fn() -> Result<Option<SecretText>, KeychainError>) -> Self {
            FakeKeychain {
                answer,
                asked: RefCell::new(Vec::new()),
            }
        }
    }

    impl KeychainReader for FakeKeychain {
        fn generic_password(
            &self,
            service: &str,
            account: Option<&str>,
        ) -> Result<Option<SecretText>, KeychainError> {
            self.asked
                .borrow_mut()
                .push((service.to_owned(), account.map(str::to_owned)));
            (self.answer)()
        }
    }

    fn doc(raw: &str) -> Result<Option<SecretText>, KeychainError> {
        Ok(Some(SecretText::new(raw.to_owned())))
    }

    fn write(dir: &Path, raw: &str) -> PathBuf {
        let path = credentials_file(dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, raw).unwrap();
        path
    }

    const KEYCHAIN_LOGIN: &str =
        r#"{"claudeAiOauth":{"accessToken":"from-keychain","refreshToken":"rt"}}"#;
    const FILE_LOGIN: &str = r#"{"claudeAiOauth":{"accessToken":"from-file"}}"#;

    #[test]
    fn the_account_is_the_login_name_unless_it_is_unsafe() {
        assert_eq!(keychain_account(Some("alice.b-c_1")), "alice.b-c_1");
        for unsafe_name in [None, Some(""), Some("alice@example.com"), Some("a b")] {
            assert_eq!(keychain_account(unsafe_name), FALLBACK_ACCOUNT);
        }
    }

    #[test]
    fn the_keychain_is_asked_for_claude_codes_item_and_wins_over_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(dir.path(), FILE_LOGIN);
        let keychain = FakeKeychain::new(|| doc(KEYCHAIN_LOGIN));
        let token = read_token(Some((&keychain, "alice")), Some(&file)).expect("token");
        assert_eq!(token.expose(), "from-keychain");
        assert_eq!(
            keychain.asked.borrow().as_slice(),
            [(KEYCHAIN_SERVICE.to_owned(), Some("alice".to_owned()))]
        );
    }

    /// A refused keychain still lets a file login through, as orca does.
    #[test]
    fn the_file_is_the_fallback_when_the_keychain_has_no_token() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(dir.path(), FILE_LOGIN);
        for answer in [
            (|| Ok(None)) as fn() -> _,
            || Err(KeychainError::Refused(Some(128))),
            || doc(r#"{"mcpOAuth":{}}"#),
        ] {
            let keychain = FakeKeychain::new(answer);
            let token = read_token(Some((&keychain, "alice")), Some(&file)).expect("token");
            assert_eq!(token.expose(), "from-file");
        }
        // No keychain at all (every platform but macOS).
        assert_eq!(
            read_token(None, Some(&file)).expect("token").expose(),
            "from-file"
        );
    }

    #[test]
    fn with_no_token_the_failure_named_is_the_one_that_needs_the_operator() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nowhere.json");
        let case = |answer: fn() -> Result<Option<SecretText>, KeychainError>, file: &Path| {
            read_token(Some((&FakeKeychain::new(answer), "alice")), Some(file))
                .expect_err("no token")
        };

        assert_eq!(case(|| Ok(None), &missing), PlanErrorKind::NotSignedIn);
        assert_eq!(
            read_token(None, None).expect_err("none"),
            PlanErrorKind::NotSignedIn
        );
        assert_eq!(
            case(|| Err(KeychainError::TimedOut), &missing),
            PlanErrorKind::KeychainRefused
        );
        assert_eq!(
            case(|| Err(KeychainError::Empty), &missing),
            PlanErrorKind::CredentialUnreadable
        );
        assert_eq!(
            case(|| doc("not json"), &missing),
            PlanErrorKind::CredentialUnreadable
        );
        assert_eq!(
            case(
                || doc(r#"{"claudeAiOauth":{"refreshToken":"rt"}}"#),
                &missing
            ),
            PlanErrorKind::SignInExpired
        );
        assert_eq!(
            case(
                || doc(r#"{"claudeAiOauth":{"accessToken":"  "}}"#),
                &missing
            ),
            PlanErrorKind::NoSubscription
        );
        // A refusal outranks what the file says, because the keychain is where
        // Claude Code keeps its login on a Mac.
        let unreadable = write(dir.path(), "{");
        assert_eq!(
            case(|| Err(KeychainError::Refused(None)), &unreadable),
            PlanErrorKind::KeychainRefused
        );
        assert_eq!(
            case(|| Ok(None), &unreadable),
            PlanErrorKind::CredentialUnreadable
        );
    }

    // ---- the request -----------------------------------------------------

    fn token(raw: &str) -> AccessToken {
        AccessToken::new(raw).expect("token")
    }

    const NOW: &str = "2026-10-24T12:00:00Z";

    #[tokio::test]
    async fn the_request_carries_the_bearer_the_beta_header_and_an_honest_agent() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/oauth/usage"))
            .and(wiremock::matchers::header("authorization", "Bearer tok-1"))
            .and(wiremock::matchers::header("anthropic-beta", BETA_HEADER))
            .and(wiremock::matchers::header("user-agent", crate::USER_AGENT))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_raw(USAGE, "application/json"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let usage = Client::with_base_url(&server.uri())
            .usage(&token("tok-1"), at(NOW))
            .await
            .expect("usage");
        assert_eq!(usage.windows.len(), 3);
    }

    async fn failure(template: wiremock::ResponseTemplate) -> PlanErrorKind {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::any())
            .respond_with(template)
            .mount(&server)
            .await;
        let err = Client::with_base_url(&server.uri())
            .usage(&token("tok-SECRET"), at(NOW))
            .await
            .expect_err("fails");
        assert_eq!(err.provider, Provider::Claude);
        for printed in [format!("{err}"), format!("{err:?}"), err.user_message()] {
            assert!(!printed.contains("tok-SECRET"), "{printed}");
        }
        err.kind
    }

    #[tokio::test]
    async fn each_rejection_is_classified() {
        use wiremock::ResponseTemplate as R;
        assert_eq!(failure(R::new(401)).await, PlanErrorKind::SignInExpired);
        assert_eq!(
            failure(R::new(403).set_body_string(
                r#"{"error":{"message":"OAuth token does not meet scope requirement user:profile"}}"#
            ))
            .await,
            PlanErrorKind::MissingScope
        );
        assert_eq!(
            failure(R::new(403).set_body_string("revoked")).await,
            PlanErrorKind::SignInExpired
        );
        assert_eq!(
            failure(R::new(429).insert_header("retry-after", "120")).await,
            PlanErrorKind::RateLimited {
                retry_after_secs: Some(120)
            }
        );
        assert_eq!(
            failure(R::new(503)).await,
            PlanErrorKind::Http { status: 503 }
        );
        assert!(matches!(
            failure(R::new(200).set_body_string("<html>")).await,
            PlanErrorKind::Undecodable(_)
        ));
    }

    /// A redirect is never followed with the bearer token.
    #[tokio::test]
    async fn a_redirect_is_reported_not_followed() {
        let elsewhere = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::any())
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_raw(USAGE, "application/json"),
            )
            .expect(0)
            .mount(&elsewhere)
            .await;
        assert_eq!(
            failure(
                wiremock::ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/api/oauth/usage", elsewhere.uri()))
            )
            .await,
            PlanErrorKind::Http { status: 302 }
        );
    }

    #[tokio::test]
    async fn an_unroutable_host_is_unreachable_without_the_url() {
        let err = Client::with_base_url("http://127.0.0.1:1")
            .usage(&token("tok"), at(NOW))
            .await
            .expect_err("refused");
        assert!(matches!(err.kind, PlanErrorKind::Unreachable(_)));
        assert!(!format!("{err:?}").contains("127.0.0.1:1"), "{err:?}");
    }
}
