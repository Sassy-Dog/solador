//! Failures of a billing read, classified. `crates/fault` owns the words.
//!
//! No variant carries a string a transport or a response produced: a `reqwest`
//! error can name the URL and a body can echo anything, and this crate holds a
//! bearer token, so the only things that survive are a kind and a status code.

use fault::Fault;

/// The operator-facing name of what failed, and the only thing
/// [`Fault::message`] interpolates.
const VENDOR: &str = "GitHub";

/// Why a billing read produced nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActionsBillingError {
    /// The caller passed an empty token. Never reaches the network.
    #[error("no GitHub billing token configured")]
    MissingToken,
    /// 401: the token is missing, mistyped, expired or revoked.
    #[error("GitHub rejected the billing token")]
    Unauthorized,
    /// 403 that is not a rate limit: the token lacks Administration: read, or
    /// the caller is not an org admin (a billing manager for budgets).
    #[error("GitHub refused the billing read")]
    Forbidden,
    /// 404: the org is wrong, or it lacks the enhanced billing platform.
    #[error("GitHub has no such org billing endpoint")]
    NotFound,
    /// 429, or a 403 carrying `x-ratelimit-remaining: 0`.
    #[error("GitHub is rate-limiting the billing read")]
    RateLimited,
    /// 5xx.
    #[error("GitHub returned HTTP {status}")]
    VendorFailure { status: u16 },
    /// Any other non-2xx status. Kept apart from the named states.
    #[error("GitHub returned HTTP {status}")]
    UnexpectedStatus { status: u16 },
    /// The body did not parse into the shape this build expects.
    #[error("could not decode the GitHub billing response")]
    Undecodable,
    /// The request never completed.
    #[error("could not reach GitHub")]
    Unreachable,
    /// The budgets listing said there were more pages than this build will
    /// read. Refused rather than reading a prefix, which could miss the very
    /// budget that stops jobs.
    #[error("the GitHub budgets listing is truncated")]
    BudgetsTruncated,
}

impl ActionsBillingError {
    /// The vocabulary state this failure is.
    #[must_use]
    pub fn fault(&self) -> Fault {
        match self {
            ActionsBillingError::MissingToken => Fault::NotConfigured,
            ActionsBillingError::Unauthorized | ActionsBillingError::Forbidden => {
                Fault::CredentialRejected
            }
            ActionsBillingError::NotFound => Fault::NotFound,
            ActionsBillingError::RateLimited => Fault::RateLimited,
            ActionsBillingError::VendorFailure { .. } => Fault::VendorFailure,
            ActionsBillingError::UnexpectedStatus { .. } => Fault::Unexpected,
            ActionsBillingError::Undecodable | ActionsBillingError::BudgetsTruncated => {
                Fault::Undecodable
            }
            ActionsBillingError::Unreachable => Fault::Unreachable,
        }
    }

    /// Whether the fix is a different credential.
    #[must_use]
    pub fn is_auth_failure(&self) -> bool {
        matches!(
            self,
            ActionsBillingError::Unauthorized | ActionsBillingError::Forbidden
        )
    }

    /// Operator-facing sentence. Never built from `to_string()`.
    ///
    /// The 403 and 404 arms keep the hint only this crate has: a token that
    /// authenticates but cannot read billing is the live failure, and "update
    /// it in Settings" alone would send the operator to paste the same token.
    #[must_use]
    pub fn user_message(&self) -> String {
        let stock = self.fault().message(VENDOR);
        match self {
            ActionsBillingError::Forbidden => {
                format!("{stock} — it needs Administration: read, and an org admin")
            }
            ActionsBillingError::NotFound => {
                format!("{stock} — check the org, which needs enhanced billing")
            }
            ActionsBillingError::UnexpectedStatus { status } => {
                fault::http_status_message(*status, VENDOR)
            }
            ActionsBillingError::BudgetsTruncated => {
                format!("{stock} — the budgets list is longer than this reads")
            }
            _ => stock,
        }
    }
}
