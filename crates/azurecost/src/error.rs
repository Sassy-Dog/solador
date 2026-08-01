//! Failures from reading the Azure cost export, and the operator-facing strings
//! they turn into. Port of `AzureCostError` +
//! `AzureCostService.friendlyMessage(for:)` in
//! `DevCanopy/Services/AzureCost/AzureCostService.swift`.
//!
//! The `Display` strings are the Swift `errorDescription`s verbatim, and
//! [`AzureCostError::user_message`] is the Swift `friendlyMessage` — the panel
//! shows these, so a drift here is a visible drift.

/// Column the aggregation cannot do without; named here so the error and the
/// parser cannot disagree about its spelling.
pub const COST_COLUMN: &str = "costInUsd";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AzureCostError {
    /// The SAS URL did not compose into a valid request URL.
    #[error("Invalid blob URL — check the SAS URL in Settings")]
    InvalidUrl,
    /// Blob storage answered non-2xx.
    ///
    /// `body` is captured for [`AzureCostError::is_auth_failure`] only — Azure
    /// spells an expired SAS as an `AuthenticationFailed` body, sometimes
    /// behind a status that is not 401/403. It is deliberately absent from the
    /// `Display` string: what the panel shows is the status, never a service
    /// error document.
    #[error("Blob request failed (HTTP {status})")]
    Http { status: u16, body: Option<String> },
    /// The request never completed (DNS, TLS, timeout, connection reset).
    ///
    /// Swift's `.invalidResponse` — a `URLSession`-shaped case for "the
    /// response was not an `HTTPURLResponse`" — has no reqwest counterpart;
    /// transport failure arrives here instead.
    ///
    /// # Invariant: no SAS in the message
    ///
    /// `reqwest` attaches the request URL to its errors, and for this crate the
    /// URL *is* the credential (the SAS query carries `sig=`). Every
    /// construction site must strip it with
    /// [`reqwest::Error::without_url`] before the error becomes a string.
    #[error("unreachable: {0}")]
    Unreachable(String),
    /// The month folder listed empty — no export run for that month.
    #[error("No export found under {prefix}")]
    NoBlobs { prefix: String },
    /// The newest run exists but holds no `.csv` partitions (e.g. only a
    /// `_manifest.json` has landed so far).
    #[error("No CSV partitions in run {run}")]
    NoCsv { run: String, prefix: String },
    /// The export CSV's header is missing a column the aggregation requires.
    #[error("Export CSV missing '{0}' column")]
    MissingColumn(String),
}

impl AzureCostError {
    /// True when the failure looks like an expired or revoked SAS. A container
    /// SAS that has aged out answers with 403 — or with an
    /// `AuthenticationFailed` body — and the only fix is a fresh URL, so this
    /// is the one failure the panel turns into an action rather than a report.
    #[must_use]
    pub fn is_auth_failure(&self) -> bool {
        match self {
            AzureCostError::Http { status, body } => {
                *status == 401
                    || *status == 403
                    || body
                        .as_deref()
                        .is_some_and(|b| b.contains("AuthenticationFailed"))
            }
            _ => false,
        }
    }

    /// The string the panel footer shows. Two failures get a hand-written line
    /// because the raw one would misdirect; everything else reads its
    /// `Display`.
    #[must_use]
    pub fn user_message(&self) -> String {
        if self.is_auth_failure() {
            return "SAS expired or invalid — paste a new one in Settings".to_owned();
        }
        // `NoBlobs` only reaches a user when *neither* the current month nor
        // the last completed month has an export — `fetch_summary` falls back
        // to the prior month, and a missing prior-month export is swallowed. So
        // it means "no recent cost data exists", not "this blob path is wrong",
        // and it must read calmly rather than as a scary path error.
        if matches!(self, AzureCostError::NoBlobs { .. }) {
            return "no recent cost export found".to_owned();
        }
        self.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_403_is_an_auth_failure_whatever_the_body_says() {
        let err = AzureCostError::Http {
            status: 403,
            body: None,
        };
        assert!(err.is_auth_failure());
        assert_eq!(
            err.user_message(),
            "SAS expired or invalid — paste a new one in Settings"
        );
    }

    #[test]
    fn a_401_is_an_auth_failure() {
        assert!(AzureCostError::Http {
            status: 401,
            body: None
        }
        .is_auth_failure());
    }

    /// Azure can answer an aged-out SAS with a non-401/403 status but an
    /// `AuthenticationFailed` document — the body is what settles it.
    #[test]
    fn an_authentication_failed_body_is_an_auth_failure_at_any_status() {
        let err = AzureCostError::Http {
            status: 400,
            body: Some("<Error><Code>AuthenticationFailed</Code></Error>".to_owned()),
        };
        assert!(err.is_auth_failure());
        assert_eq!(
            err.user_message(),
            "SAS expired or invalid — paste a new one in Settings"
        );
    }

    /// A plain server error is not an auth failure — telling the operator to
    /// paste a new SAS would send them to fix the wrong thing.
    #[test]
    fn a_500_is_not_an_auth_failure() {
        let err = AzureCostError::Http {
            status: 500,
            body: Some("<Error><Code>InternalError</Code></Error>".to_owned()),
        };
        assert!(!err.is_auth_failure());
        assert_eq!(err.user_message(), "Blob request failed (HTTP 500)");
    }

    /// The service error document must never reach the string the panel shows.
    #[test]
    fn the_response_body_never_reaches_the_displayed_message() {
        let err = AzureCostError::Http {
            status: 409,
            body: Some("<Error><Detail>sv=2024-11-04&se=…</Detail></Error>".to_owned()),
        };
        assert_eq!(err.to_string(), "Blob request failed (HTTP 409)");
        assert!(!err.user_message().contains("sv="));
    }

    #[test]
    fn no_blobs_reads_calm() {
        let err = AzureCostError::NoBlobs {
            prefix: "daily/x/20260701-20260731/".to_owned(),
        };
        assert_eq!(err.user_message(), "no recent cost export found");
    }

    #[test]
    fn the_other_messages_are_their_display_strings() {
        assert_eq!(
            AzureCostError::MissingColumn(COST_COLUMN.to_owned()).user_message(),
            "Export CSV missing 'costInUsd' column"
        );
        assert_eq!(
            AzureCostError::NoCsv {
                run: "202606151800".to_owned(),
                prefix: "daily/x/".to_owned(),
            }
            .user_message(),
            "No CSV partitions in run 202606151800"
        );
        assert_eq!(
            AzureCostError::InvalidUrl.user_message(),
            "Invalid blob URL — check the SAS URL in Settings"
        );
    }
}
