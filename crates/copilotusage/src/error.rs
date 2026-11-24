//! Failures of a Copilot usage read, classified. `crates/fault` owns the words.
//!
//! Nothing here carries a string a transport or a response produced: a
//! `reqwest` error can name the URL and a body can echo anything, and this
//! crate holds a bearer token, so the only things that survive are a kind and
//! a status code.

use std::fmt;

use fault::Fault;

/// The operator-facing name of what failed, and the only thing
/// [`Fault::message`] interpolates.
const VENDOR: &str = "GitHub";

/// Which half of Copilot was being read. It decides which permission the hint
/// names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Org,
    User,
}

/// Why a read produced nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// 401: the token is missing, mistyped, expired or revoked.
    Unauthorized,
    /// 403 that is not a rate limit: the token lacks its permission.
    Forbidden,
    /// 404: the org or username is wrong, or the account lacks the feature.
    NotFound,
    /// 429, or a 403 carrying `x-ratelimit-remaining: 0`.
    RateLimited,
    /// 5xx.
    VendorFailure { status: u16 },
    /// Any other non-2xx status.
    UnexpectedStatus { status: u16 },
    /// The body did not parse into the shape this build expects.
    Undecodable,
    /// The request never completed.
    Unreachable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopilotUsageError {
    pub level: Level,
    pub kind: ErrorKind,
}

impl fmt::Display for CopilotUsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.level {
            Level::Org => "organization",
            Level::User => "personal",
        };
        match self.kind {
            ErrorKind::VendorFailure { status } | ErrorKind::UnexpectedStatus { status } => {
                write!(
                    f,
                    "GitHub returned HTTP {status} for the {what} Copilot read"
                )
            }
            _ => write!(f, "the {what} Copilot read failed"),
        }
    }
}

impl std::error::Error for CopilotUsageError {}

impl CopilotUsageError {
    #[must_use]
    pub fn new(level: Level, kind: ErrorKind) -> Self {
        CopilotUsageError { level, kind }
    }

    /// The vocabulary state this failure is.
    #[must_use]
    pub fn fault(&self) -> Fault {
        match self.kind {
            ErrorKind::Unauthorized | ErrorKind::Forbidden => Fault::CredentialRejected,
            ErrorKind::NotFound => Fault::NotFound,
            ErrorKind::RateLimited => Fault::RateLimited,
            ErrorKind::VendorFailure { .. } => Fault::VendorFailure,
            ErrorKind::UnexpectedStatus { .. } => Fault::Unexpected,
            ErrorKind::Undecodable => Fault::Undecodable,
            ErrorKind::Unreachable => Fault::Unreachable,
        }
    }

    /// Whether the fix is a different credential.
    #[must_use]
    pub fn is_auth_failure(&self) -> bool {
        matches!(self.kind, ErrorKind::Unauthorized | ErrorKind::Forbidden)
    }

    /// Operator-facing sentence. Never built from `to_string()`.
    ///
    /// The credential and 404 arms keep the hint only this crate has, and which
    /// one depends on the half: the org read needs Administration: read and an
    /// org admin, the personal read needs Plan: read.
    #[must_use]
    pub fn user_message(&self) -> String {
        let stock = self.fault().message(VENDOR);
        match (self.kind, self.level) {
            (ErrorKind::Forbidden | ErrorKind::Unauthorized, Level::Org) => {
                format!("{stock} — it needs Administration: read, and an org admin")
            }
            (ErrorKind::Forbidden | ErrorKind::Unauthorized, Level::User) => {
                format!("{stock} — it needs the user permission Plan: read")
            }
            (ErrorKind::NotFound, Level::Org) => {
                format!("{stock} — check the org, which needs enhanced billing")
            }
            (ErrorKind::NotFound, Level::User) => {
                format!("{stock} — check the username, whose plan must be self-bought")
            }
            (ErrorKind::UnexpectedStatus { status }, _) => {
                fault::http_status_message(status, VENDOR)
            }
            _ => stock,
        }
    }
}
