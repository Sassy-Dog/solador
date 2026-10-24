//! The plan's own limit windows for a **Claude** or **Codex** subscription —
//! the 5-hour session and the weekly allowance, with their reset instants —
//! read the way the subscription itself reports them.
//!
//! This is a different reading from `crates/usage`'s Claude token rollups, and
//! it must never be merged with them. The rollups walk `~/.claude/projects` and
//! record what was consumed on this machine, never who paid for it (AGENTS.md,
//! "Usage → Claude"). This crate asks the service, using the sign-in a program
//! on this machine already holds, and gets back that subscription's utilisation
//! of its own plan limits. One source knows the subscription and the other
//! knows the machine.
//!
//! # The one crate that reads a credential another program owns
//!
//! Claude Code keeps its login in the macOS keychain item
//! `Claude Code-credentials`, or in `~/.claude/.credentials.json`. Codex keeps
//! its login in `~/.codex/auth.json`. Reading them is the whole capability,
//! and the rules around it are what make it acceptable:
//!
//! - **Read only. Never refresh, rotate or write.** A refresh would rotate the
//!   owner's refresh token and sign the owning program out. An expired token is
//!   reported as [`PlanErrorKind::SignInExpired`], whose fix is to run the
//!   owning program, which renews its own login.
//! - **Read when needed, used once, dropped.** A token lives in an
//!   [`AccessToken`] for one request. It is never copied into Solador's
//!   credential store, never logged, and never in a `Debug` or an error.
//! - **Nothing here decides *whether* to read.** The opt-in lives in the shell
//!   (`app/src-tauri/src/plan_usage.rs`). This crate only says how.
//!
//! The method is the one `stablyai/orca` uses for the same job. Each module
//! cites the orca file and line it follows, and says where it parts from
//! orca.
//!
//! # Two private endpoints
//!
//! Neither endpoint is documented: `api.anthropic.com/api/oauth/usage` and
//! `chatgpt.com/backend-api/wham/usage`. Every failure is a classified
//! [`PlanUsageError`], which the panel renders as `—` plus one footer reason.
//! It never breaks the rest of the panel.
//!
//! # Unknown is not zero
//!
//! - An absent window is no [`PlanWindow`], never a 0% one.
//! - A missing reset instant is `None`, never "now".
//! - A Codex window that states no length is [`WindowSpan::Unstated`]. It
//!   never borrows a 5-hour or weekly label it did not state.
//!
//! # No wall clock
//!
//! `now` is always an argument, and chrono's `clock` feature is off. That is
//! `crates/usage`'s rule, and for the same reason.

pub mod claude;
pub mod codex;
mod http;
pub mod keychain;
mod retry;
mod secret;

use chrono::{DateTime, Utc};
use fault::Fault;

pub use secret::{AccessToken, SecretText};

/// The `User-Agent` both clients send. It is honest on purpose: orca sends
/// `claude-code/2.1.0` and `codex-cli` / `originator: Codex Desktop`, so the
/// service believes another program is calling. Solador does not.
pub const USER_AGENT: &str = "Solador";

/// What a successful read that reported no window says. It is a measurement,
/// not a failure, and is rendered as the em dash with this reason rather than
/// as a fabricated 0%.
pub const NO_WINDOWS_MESSAGE: &str = "no plan windows reported";

/// Whose subscription a reading or a failure belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Claude,
    Codex,
}

impl Provider {
    /// The program whose saved sign-in is borrowed. Credential states name it,
    /// because the fix for every one of them is in that program.
    #[must_use]
    pub const fn holder(self) -> &'static str {
        match self {
            Provider::Claude => "Claude Code",
            Provider::Codex => "Codex",
        }
    }

    /// The service the request goes to. Transport states name it, because
    /// "couldn't reach Claude Code" would send the operator looking at a local
    /// program that was never asked anything.
    #[must_use]
    pub const fn service(self) -> &'static str {
        match self {
            Provider::Claude => "Anthropic",
            Provider::Codex => "ChatGPT",
        }
    }
}

/// One subscription's limit windows, as the service reported them.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanUsage {
    /// In the order the service presents them: session, then weekly, then any
    /// model-scoped weekly. Empty when the service answered without any window,
    /// which is [`NO_WINDOWS_MESSAGE`] and not an error.
    pub windows: Vec<PlanWindow>,
}

impl PlanUsage {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }
}

/// One limit window.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanWindow {
    /// How long the window is.
    pub span: WindowSpan,
    /// The model a window is scoped to, as the service names it (`"Fable"`), or
    /// `None` for a window covering the whole plan.
    pub scope: Option<String>,
    /// Utilisation as a percentage of the window's allowance. It is finite and
    /// non-negative. It is **not** clamped to 100, because a service reporting
    /// 104% is saying something true.
    pub used_percent: f64,
    /// When the window resets. `None` when the service did not say, or said
    /// something unparseable. It is never defaulted to "now".
    pub resets_at: Option<DateTime<Utc>>,
}

/// A window's length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowSpan {
    /// A stated length in whole minutes. 300 is the 5-hour session and 10080
    /// the week. Codex's one-minute drift is folded onto those two, as orca does.
    Minutes(u32),
    /// The service sent a window without saying how long it is, so all that is
    /// known is its slot.
    Unstated(Slot),
}

/// Codex's two window slots, named as the wire names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Primary,
    Secondary,
}

/// A wire percentage: a finite, non-negative number. Anything else, a
/// negative included, is unknown and never a measured 0%.
pub(crate) fn used_percent(value: Option<&serde_json::Value>) -> Option<f64> {
    value
        .and_then(serde_json::Value::as_f64)
        .filter(|p| p.is_finite() && *p >= 0.0)
}

/// The 5-hour session window, in minutes.
pub const SESSION_MINUTES: u32 = 300;
/// The weekly window, in minutes.
pub const WEEK_MINUTES: u32 = 7 * 24 * 60;

/// A failed read, and whose it was.
#[derive(Debug, thiserror::Error)]
#[error("{} plan usage: {kind}", provider.holder())]
pub struct PlanUsageError {
    pub provider: Provider,
    pub kind: PlanErrorKind,
}

impl PlanUsageError {
    #[must_use]
    pub const fn new(provider: Provider, kind: PlanErrorKind) -> Self {
        PlanUsageError { provider, kind }
    }

    /// How long the service asked us to wait, when it throttled the read.
    #[must_use]
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self.kind {
            PlanErrorKind::RateLimited { retry_after_secs } => retry_after_secs,
            _ => None,
        }
    }

    /// The operator's sentence for this failure, from `crates/fault` wherever
    /// that vocabulary has a word, and the crate's own otherwise.
    ///
    /// Written out arm by arm: a `self.to_string()` fallback is how transport
    /// detail used to reach panel footers (#352).
    #[must_use]
    pub fn user_message(&self) -> String {
        let holder = self.provider.holder();
        let service = self.provider.service();
        match &self.kind {
            // "installed and not signed in" is exactly the case the variant
            // documents, and on this machine nothing else can be told apart:
            // no saved sign-in at any location the program uses.
            PlanErrorKind::NotSignedIn => Fault::ToolUnavailable.message(holder),
            // Not a fault: a login with an API key, or none, has no plan to
            // report on. "Signed in with an API key" would be a guess: a
            // logged-out Claude Code item can still hold other entries.
            PlanErrorKind::NoSubscription => {
                format!("{holder} isn't signed in with a subscription")
            }
            PlanErrorKind::KeychainRefused => {
                format!("keychain didn't share {holder}'s sign-in — re-enable to retry")
            }
            PlanErrorKind::CredentialUnreadable => {
                format!("couldn't read {holder}'s saved sign-in")
            }
            PlanErrorKind::SignInExpired => Fault::SignInExpired.message(holder),
            PlanErrorKind::MissingScope => {
                format!("{holder}'s token can't read usage — sign in with /login")
            }
            PlanErrorKind::RateLimited { .. } => Fault::RateLimited.message(service),
            PlanErrorKind::Http { status } => fault::http_status_message(*status, service),
            PlanErrorKind::Unreachable(_) => Fault::Unreachable.message(service),
            PlanErrorKind::Undecodable(_) => Fault::Undecodable.message(service),
        }
    }
}

/// What went wrong. Classifies; [`PlanUsageError::user_message`] owns the
/// words.
///
/// No variant carries a token, a response body or a URL. The two `String`s
/// are transport and decode detail for the log, and `reqwest`'s URL is
/// stripped before it becomes one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanErrorKind {
    /// No saved sign-in at any location the owning program uses.
    #[error("no saved sign-in found")]
    NotSignedIn,
    /// A saved sign-in document exists and holds no subscription login: an
    /// API-key user, or a logged-out one. Either way there is no plan.
    #[error("signed in without a subscription")]
    NoSubscription,
    /// macOS keychain access failed — refused at a prompt, timed out, or
    /// denied — and no file fallback held a token. The shell parks the
    /// provider on this one, so a "Deny" is not asked again every poll.
    #[error("the keychain did not answer")]
    KeychainRefused,
    /// The saved sign-in exists and could not be read or parsed.
    #[error("the saved sign-in is unreadable")]
    CredentialUnreadable,
    /// The service answered 401, or 403 for a reason other than scope: the
    /// owner's sign-in expired or was revoked.
    #[error("the sign-in has expired")]
    SignInExpired,
    /// Claude only: a token without the `user:profile` scope, such as one from
    /// `claude setup-token`.
    #[error("the token lacks the usage scope")]
    MissingScope,
    /// HTTP 429. `retry_after_secs` is the service's own `Retry-After`,
    /// capped at a day.
    #[error("rate limited")]
    RateLimited { retry_after_secs: Option<u64> },
    /// Any other non-2xx, including a redirect, which is never followed with a
    /// bearer token.
    #[error("HTTP {status}")]
    Http { status: u16 },
    #[error("unreachable: {0}")]
    Unreachable(String),
    #[error("undecodable: {0}")]
    Undecodable(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(provider: Provider, kind: PlanErrorKind) -> String {
        PlanUsageError::new(provider, kind).user_message()
    }

    /// Credential states name the program, transport states the service.
    #[test]
    fn each_failure_names_the_party_that_can_fix_it() {
        assert_eq!(
            message(Provider::Claude, PlanErrorKind::NotSignedIn),
            "Claude Code not found — install it and sign in"
        );
        assert_eq!(
            message(Provider::Codex, PlanErrorKind::SignInExpired),
            "Codex's sign-in has expired — run it to refresh"
        );
        assert_eq!(
            message(Provider::Claude, PlanErrorKind::Unreachable("x".into())),
            "couldn't reach Anthropic"
        );
        assert_eq!(
            message(
                Provider::Codex,
                PlanErrorKind::RateLimited {
                    retry_after_secs: Some(60)
                }
            ),
            "ChatGPT is rate-limiting this read"
        );
        assert_eq!(
            message(Provider::Claude, PlanErrorKind::Http { status: 503 }),
            "Anthropic is failing on its side (HTTP 503)"
        );
        assert_eq!(
            message(Provider::Codex, PlanErrorKind::Undecodable("x".into())),
            "couldn't read ChatGPT's response"
        );
    }

    /// An expired sign-in is its own state: never "unreachable", and never
    /// "update it in Settings", because Settings holds no copy of it.
    #[test]
    fn an_expired_sign_in_never_reads_as_a_network_or_settings_problem() {
        for provider in [Provider::Claude, Provider::Codex] {
            let expired = message(provider, PlanErrorKind::SignInExpired);
            assert_ne!(
                expired,
                message(provider, PlanErrorKind::Unreachable(String::new()))
            );
            assert!(!expired.contains("Settings"), "{expired}");
        }
    }

    /// Every sentence fits a panel header, carries no URL and no detail, and
    /// no two of them read alike.
    #[test]
    fn every_sentence_is_short_distinct_and_detail_free() {
        let kinds = [
            PlanErrorKind::NotSignedIn,
            PlanErrorKind::NoSubscription,
            PlanErrorKind::KeychainRefused,
            PlanErrorKind::CredentialUnreadable,
            PlanErrorKind::SignInExpired,
            PlanErrorKind::MissingScope,
            PlanErrorKind::RateLimited {
                retry_after_secs: None,
            },
            PlanErrorKind::Http { status: 400 },
            PlanErrorKind::Unreachable("https://secret.example/?token=abc".into()),
            PlanErrorKind::Undecodable("expected value at line 1".into()),
        ];
        for provider in [Provider::Claude, Provider::Codex] {
            let sentences: Vec<String> =
                kinds.iter().map(|k| message(provider, k.clone())).collect();
            for s in &sentences {
                assert!(s.chars().count() <= 72, "{s}");
                for forbidden in ["://", "token=", "line 1"] {
                    assert!(!s.contains(forbidden), "{s}");
                }
            }
            for (i, a) in sentences.iter().enumerate() {
                for b in &sentences[i + 1..] {
                    assert_ne!(a, b);
                }
            }
        }
    }

    #[test]
    fn only_a_throttled_read_carries_a_retry_after() {
        let throttled = PlanUsageError::new(
            Provider::Claude,
            PlanErrorKind::RateLimited {
                retry_after_secs: Some(90),
            },
        );
        assert_eq!(throttled.retry_after_secs(), Some(90));
        let expired = PlanUsageError::new(Provider::Claude, PlanErrorKind::SignInExpired);
        assert_eq!(expired.retry_after_secs(), None);
    }
}
