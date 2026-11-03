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
//!   read, and only the default profile's plan name.
//! - **The spend's currency is never defaulted.** orca falls back to `USD`
//!   (`claude-usage-credits.ts:110`); an unnamed currency here is a bare
//!   amount. Its exponent default of 2 (`:44`) is kept: every amount the
//!   endpoint is known to send is in minor units.
//! - **`extra_usage.utilization` is not read.** orca computes from the amounts
//!   first because that field "has been seen as a 0–1 fraction as well as a
//!   percentage" (`:138`), then falls back to it anyway. A number whose unit
//!   is unknown is not a percentage, so here there is no fallback.
//! - **The plan's name is read**, which orca does not do: from Claude Code's
//!   cached profile, `~/.claude.json`'s `oauthAccount.planDisplayName`. It is
//!   the display name and not `organizationType` or a tier code, because only
//!   the display name followed the 2026-10-30 rename of Max 20x to Ultra.
//!
//! # Enterprise seats
//!
//! An Enterprise seat's reply has no `five_hour` or `seven_day`. Its limit is
//! a spend cap, in `spend` or the older `extra_usage` (`claude-usage-credits.ts:58`:
//! "Enterprise seats have no 5h/7d windows; their monthly spend cap is the
//! only limit that binds"). Beside windows, the same object is spend past the
//! plan's limits instead. Which one it is, is the view's call.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::keychain::{KeychainError, KeychainReader};
use crate::{
    http, plan_name, used_percent, AccessToken, Money, PlanErrorKind, PlanSpend, PlanUsage,
    PlanUsageError, PlanWindow, Provider, WindowSpan, SESSION_MINUTES, WEEK_MINUTES,
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

/// Claude Code's cached profile, `~/.claude.json` under `home` (`%USERPROFILE%`
/// on Windows, where Claude Code writes it too).
#[must_use]
pub fn profile_file(home: &Path) -> PathBuf {
    home.join(".claude.json")
}

/// The largest profile [`read_plan_name`] will parse. The file also holds
/// Claude Code's per-project history, so it grows; this one is 100 KB on a
/// working machine, and a file past the cap is skipped rather than read.
pub const PROFILE_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// The one field read out of the profile. Every other key is skipped by
/// serde without being held: the file can carry `primaryApiKey` and the
/// account's email and name.
#[derive(serde::Deserialize)]
struct Profile {
    #[serde(rename = "oauthAccount")]
    oauth_account: Option<OauthAccount>,
}

#[derive(serde::Deserialize)]
struct OauthAccount {
    #[serde(rename = "planDisplayName")]
    plan_display_name: Option<String>,
}

/// The plan's display name from a profile file (`"Ultra"`, `"Enterprise"`),
/// or `None`: no file, an oversized or unparseable one, or no usable name.
///
/// A half-written file (Claude Code rewrites it often) is `None` for one read.
/// The parse error is dropped unformatted, because serde's messages can quote
/// the value they choked on.
#[must_use]
pub fn read_plan_name(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > PROFILE_MAX_BYTES {
        return None;
    }
    // `take`: a file that grew past the cap since `metadata` is cut short and
    // fails to parse, rather than being read whole.
    let file = std::fs::File::open(path).ok()?.take(PROFILE_MAX_BYTES);
    let profile: Profile = serde_json::from_reader(std::io::BufReader::new(file)).ok()?;
    profile
        .oauth_account?
        .plan_display_name
        .as_deref()
        .and_then(plan_name)
}

/// This machine's plan name, from the profile under the real home directory.
///
/// Like [`read_this_machine`], **no test calls it**, and the shell calls it
/// only behind the opt-in.
#[must_use]
pub fn plan_name_this_machine() -> Option<String> {
    read_plan_name(&profile_file(&dirs::home_dir()?))
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

/// An amount from its three parts (`claude-usage-credits.ts:5-10,37-49`).
///
/// - The amount must be a finite, non-negative number. A fractional count of
///   minor units is rounded to the nearest one.
/// - A missing exponent is 2, as orca's (`:44`). One outside 0–6 makes the
///   amount unknown, as it does there (`:45`).
/// - A currency that is not three ASCII letters is none.
fn money_parts(
    amount: Option<&Value>,
    exponent: Option<&Value>,
    currency: Option<&Value>,
) -> Option<Money> {
    // 9e15 keeps the rounding below exact in an f64 and the result inside i64.
    let amount = amount?
        .as_f64()
        .filter(|a| a.is_finite() && (0.0..9e15).contains(a))?;
    let exponent = match exponent {
        None | Some(Value::Null) => 2,
        Some(v) => v
            .as_u64()
            .and_then(|e| u8::try_from(e).ok())
            .filter(|e| *e <= 6)?,
    };
    let currency = currency
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic()))
        .map(str::to_ascii_uppercase);
    #[allow(clippy::cast_possible_truncation)]
    Some(Money {
        minor: amount.round() as i64,
        exponent,
        currency,
    })
}

/// A `{ amount_minor, currency, exponent }` object (`claude-usage-credits.ts:5-10`).
fn money(raw: Option<&Value>) -> Option<Money> {
    let raw = raw.filter(|r| r.is_object())?;
    money_parts(
        raw.get("amount_minor"),
        raw.get("exponent"),
        raw.get("currency"),
    )
}

/// The reply's spend: `spend` (`claude-usage-credits.ts:12-21,87-121`), else
/// the older `extra_usage` (`:23-31,123-151`). `None` when neither carries an
/// amount.
///
/// `spend.balance` is not read: it is a prepaid credit, not a cap, and a
/// budget meter has no place for it.
fn spend(doc: &Value) -> Option<PlanSpend> {
    if let Some(s) = doc.get("spend").filter(|s| s.is_object()) {
        let used = money(s.get("used"));
        // The cap is `limit`, else `cap.money` (`:92`).
        let limit = money(s.get("limit")).or_else(|| money(s.pointer("/cap/money")));
        if used.is_some() || limit.is_some() {
            return Some(PlanSpend {
                used,
                limit,
                stated_percent: used_percent(s.get("percent")),
                enabled: s.get("enabled").and_then(Value::as_bool),
            });
        }
    }
    // One exponent and one currency for both amounts (`:127-137`).
    let e = doc.get("extra_usage").filter(|e| e.is_object())?;
    let (exponent, currency) = (e.get("decimal_places"), e.get("currency"));
    let used = money_parts(e.get("used_credits"), exponent, currency);
    let limit = money_parts(e.get("monthly_limit"), exponent, currency);
    (used.is_some() || limit.is_some()).then(|| PlanSpend {
        used,
        limit,
        // Never `utilization`: its unit is ambiguous (see the module doc).
        stated_percent: None,
        enabled: e.get("is_enabled").and_then(Value::as_bool),
    })
}

/// The windows and the spend in one `/api/oauth/usage` body. The plan's name
/// is not in it; the shell adds that from [`plan_name_this_machine`].
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

    Ok(PlanUsage {
        plan: None,
        windows,
        spend: spend(&doc),
    })
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

    // ---- Ultra, Enterprise and spend --------------------------------------

    /// Hand-written, like `USAGE`: a verified Ultra account's shape, with no
    /// session window, plus the legacy `extra_usage` spend.
    const ULTRA: &str = include_str!("../tests/fixtures/claude_usage_ultra.json");
    /// Hand-written: an Enterprise seat, with no window and a `spend` cap.
    const ENTERPRISE: &str = include_str!("../tests/fixtures/claude_usage_enterprise.json");

    fn usd(minor: i64) -> Money {
        Money {
            minor,
            exponent: 2,
            currency: Some("USD".into()),
        }
    }

    #[test]
    fn a_verified_ultra_reply_has_no_session_window_and_reads_its_extra_usage() {
        let usage = parse_usage(ULTRA).expect("parses");
        assert!(usage
            .windows
            .iter()
            .all(|w| w.span != WindowSpan::Minutes(SESSION_MINUTES)));
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[1].scope.as_deref(), Some("Fable"));
        // Amounts, not the ambiguous `utilization` beside them.
        assert_eq!(
            usage.spend,
            Some(PlanSpend {
                used: Some(usd(1250)),
                limit: Some(usd(20000)),
                stated_percent: None,
                enabled: Some(true),
            })
        );
    }

    #[test]
    fn an_enterprise_reply_is_a_spend_cap_and_no_windows_and_is_not_empty() {
        let usage = parse_usage(ENTERPRISE).expect("parses");
        assert!(usage.windows.is_empty());
        assert!(!usage.is_empty(), "a spend is a reading");
        assert_eq!(
            usage.spend,
            Some(PlanSpend {
                used: Some(usd(31245)),
                limit: Some(usd(50000)),
                stated_percent: Some(62.49),
                enabled: Some(true),
            })
        );
    }

    #[test]
    fn spend_wins_over_extra_usage_and_the_cap_can_be_cap_money() {
        let usage = parse_usage(
            r#"{"spend":{"used":{"amount_minor":100,"currency":"EUR"},
                         "cap":{"money":{"amount_minor":5000,"currency":"EUR","exponent":2}}},
                "extra_usage":{"monthly_limit":99999,"used_credits":1}}"#,
        )
        .expect("parses");
        let spend = usage.spend.expect("spend");
        assert_eq!(spend.limit.expect("cap").minor, 5000);
        // No exponent is 2, as orca reads it.
        assert_eq!(spend.used.expect("used").exponent, 2);
    }

    #[test]
    fn a_spend_with_no_amount_falls_back_and_none_anywhere_is_no_spend() {
        let usage = parse_usage(
            r#"{"spend":{"balance":{"amount_minor":1000},"enabled":true},
                "extra_usage":{"monthly_limit":5000,"currency":"usd"}}"#,
        )
        .expect("parses");
        let spend = usage.spend.expect("from extra_usage");
        assert_eq!(spend.limit.expect("limit").currency.as_deref(), Some("USD"));
        assert_eq!(spend.used, None);

        for body in [
            r#"{"spend":null}"#,
            r#"{"spend":{"balance":{"amount_minor":1}}}"#,
            r#"{"extra_usage":{"is_enabled":false,"utilization":40}}"#,
            r#"{"extra_usage":"on"}"#,
        ] {
            let usage = parse_usage(body).expect("parses");
            assert_eq!(usage.spend, None, "{body}");
            assert!(usage.is_empty(), "{body}");
        }
    }

    /// Corrupt money is unknown, never zero; an unnamed currency is unnamed.
    #[test]
    fn corrupt_amounts_are_unknown_and_a_missing_currency_is_never_dollars() {
        let money = |raw: &str| money(Some(&serde_json::from_str::<Value>(raw).unwrap()));
        for unknown in [
            r#"{"amount_minor":-1}"#,
            r#"{"amount_minor":"500"}"#,
            r#"{"amount_minor":500,"exponent":7}"#,
            r#"{"amount_minor":500,"exponent":-1}"#,
            r#"{"amount_minor":500,"exponent":1.5}"#,
            r#"{"currency":"USD"}"#,
            "500",
        ] {
            assert_eq!(money(unknown), None, "{unknown}");
        }
        let bare = money(r#"{"amount_minor":499.6,"exponent":0}"#).expect("money");
        assert_eq!(
            bare,
            Money {
                minor: 500,
                exponent: 0,
                currency: None
            }
        );
        let odd = money(r#"{"amount_minor":1,"currency":"dollars"}"#).expect("money");
        assert_eq!(odd.currency, None);
    }

    // ---- the plan's name -------------------------------------------------

    const PROFILE: &str = include_str!("../tests/fixtures/claude_profile.json");

    fn profile(dir: &Path, raw: &str) -> PathBuf {
        let path = profile_file(dir);
        std::fs::write(&path, raw).unwrap();
        path
    }

    #[test]
    fn the_plan_name_is_the_display_name_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(profile_file(dir.path()), dir.path().join(".claude.json"));
        let name = read_plan_name(&profile(dir.path(), PROFILE));
        assert_eq!(name.as_deref(), Some("Ultra"));
    }

    /// The fixture's other fields (an API key, the account's email and name,
    /// the tier code `claude_max` that did not follow the Ultra rename) are
    /// never a fallback: without a usable display name there is no name.
    #[test]
    fn no_other_profile_field_stands_in_for_a_missing_plan_name() {
        let dir = tempfile::tempdir().unwrap();
        let named = r#""planDisplayName": "  Ultra  ","#;
        assert!(PROFILE.contains(named), "the fixture carries the name");
        for without in [
            "",
            r#""planDisplayName": null,"#,
            r#""planDisplayName": " ","#,
        ] {
            let raw = PROFILE.replace(named, without);
            assert_eq!(
                read_plan_name(&profile(dir.path(), &raw)),
                None,
                "{without}"
            );
        }
    }

    #[test]
    fn a_missing_or_unusable_plan_name_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_plan_name(&dir.path().join("absent.json")), None);
        assert_eq!(read_plan_name(dir.path()), None, "a directory");
        let long = format!(
            r#"{{"oauthAccount":{{"planDisplayName":"{}"}}}}"#,
            "x".repeat(crate::PLAN_NAME_MAX_CHARS + 1)
        );
        for raw in [
            "{",
            "[]",
            "{}",
            r#"{"oauthAccount":null}"#,
            r#"{"oauthAccount":"Ultra"}"#,
            r#"{"oauthAccount":{}}"#,
            r#"{"oauthAccount":{"planDisplayName":null}}"#,
            r#"{"oauthAccount":{"planDisplayName":42}}"#,
            r#"{"oauthAccount":{"planDisplayName":"   "}}"#,
            r#"{"oauthAccount":{"planDisplayName":"Ul\ntra"}}"#,
            &long,
        ] {
            assert_eq!(read_plan_name(&profile(dir.path(), raw)), None, "{raw}");
        }
    }

    #[test]
    fn a_profile_past_the_size_cap_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = profile(dir.path(), PROFILE);
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(PROFILE_MAX_BYTES + 1).unwrap();
        assert_eq!(read_plan_name(&path), None);
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
