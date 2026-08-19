//! Failures from reading the Azure cost export, and the operator-facing strings
//! they turn into. Port of `AzureCostError` +
//! `AzureCostService.friendlyMessage(for:)` in
//! `AzureCostService`.
//!
//! The `Display` strings are the original `errorDescription`s verbatim, and
//! they stay that way: they are what a log line and a `Debug` render.
//! [`AzureCostError::user_message`] was the original `friendlyMessage`, and is
//! now the `fault` vocabulary wherever that vocabulary has a word (#354) — the
//! panel shows these, so a drift here is a visible drift.

use fault::Fault;

/// The operator-facing name of what failed, and the only thing
/// [`Fault::message`] interpolates.
///
/// The service, not the panel: the Azure Cost panel's other failure mode is the
/// `az` CLI declining to mint a SAS, which `app/src-tauri/src/azure/sas.rs`
/// reports under its own name. Two things fail here and the footer has to say
/// which.
const VENDOR: &str = "Azure Storage";

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
    /// The original's `.invalidResponse` — a `URLSession`-shaped case for "the
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

    /// The string the panel footer shows.
    ///
    /// Every arm is written out, and the `self.to_string()` fallback this used
    /// to end with is gone (#354). That fallback was not a shortcut, it was a
    /// leak: it sent `Display` to the panel, so `Unreachable` rendered
    /// `unreachable: <whatever reqwest said>` and `NoCsv` put a blob storage
    /// path in a cockpit footer.
    ///
    /// What is left keeps its own wording on purpose, because the `fault`
    /// vocabulary has no word for any of it and must not be stretched to invent
    /// one. None of it is a transport failure: an expired SAS names the one
    /// action that fixes it, a missing column is the export's schema, and the
    /// two empty-export findings are *successful* reads with nothing in them —
    /// which is why they read calmly rather than as errors.
    #[must_use]
    pub fn user_message(&self) -> String {
        // Sharper than `Fault::CredentialRejected`, and the only failure this
        // panel turns into an action rather than a report.
        if self.is_auth_failure() {
            return "SAS expired or invalid — paste a new one in Settings".to_owned();
        }
        match self {
            AzureCostError::InvalidUrl => {
                "Invalid blob URL — check the SAS URL in Settings".to_owned()
            }
            // Auth failures returned above, so what is left is a status. A 404
            // here is a renamed container, never "no such account" — see
            // `fault::http_status_message`.
            AzureCostError::Http { status, .. } => fault::http_status_message(*status, VENDOR),
            // The invariant's second guard: the payload is not interpolated at
            // all, so the request URL cannot reach a footer even if a
            // construction site ever forgets `without_url`.
            AzureCostError::Unreachable(_) => Fault::Unreachable.message(VENDOR),
            // `NoBlobs` only reaches a user when *neither* the current month nor
            // the last completed month has an export — `fetch_summary` falls back
            // to the prior month, and a missing prior-month export is swallowed. So
            // it means "no recent cost data exists", not "this blob path is wrong",
            // and it must read calmly rather than as a scary path error.
            AzureCostError::NoBlobs { .. } => "no recent cost export found".to_owned(),
            // Its sibling, and it never got the same treatment: the run folder
            // exists but holds only a `_manifest.json` so far, which is an
            // export mid-write rather than anything wrong. It said "No CSV
            // partitions in run 202606151800" until #354 — a storage path in a
            // cockpit footer, and an alarming one for a state that resolves
            // itself within the hour.
            AzureCostError::NoCsv { .. } => "cost export has no data yet".to_owned(),
            AzureCostError::MissingColumn(column) => {
                format!("Export CSV missing '{column}' column")
            }
        }
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
        assert_eq!(
            err.user_message(),
            "Azure Storage is failing on its side (HTTP 500)"
        );
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

    /// `NoCsv`'s sibling treatment, and the reason #354 touched this file: the
    /// run folder exists and holds only a `_manifest.json`, which is an export
    /// mid-write. It used to say "No CSV partitions in run 202606151800" — a
    /// storage path in a cockpit footer, alarming for a state that clears
    /// itself — while `NoBlobs` one variant up already read calmly.
    #[test]
    fn no_csv_reads_calm_and_carries_no_path() {
        let err = AzureCostError::NoCsv {
            run: "202606151800".to_owned(),
            prefix: "daily/sub-guid/20260601-20260630/".to_owned(),
        };
        assert_eq!(err.user_message(), "cost export has no data yet");
        for forbidden in ["202606151800", "daily/", "/"] {
            assert!(
                !err.user_message().contains(forbidden),
                "{} carries {forbidden:?}",
                err.user_message()
            );
        }
        assert_ne!(
            err.user_message(),
            AzureCostError::NoBlobs {
                prefix: "daily/".to_owned()
            }
            .user_message(),
            "two findings that read alike are one finding with extra steps"
        );
    }

    /// The invariant's second guard, at the boundary the panel reads. Every
    /// construction site strips the URL with `without_url`; this checks that
    /// the payload is not interpolated *either*, so a site that forgets cannot
    /// put a `sig=` in a footer.
    #[test]
    fn a_transport_failure_renders_the_stock_sentence_and_not_its_payload() {
        let err = AzureCostError::Unreachable(
            "error sending request for url \
             (https://s.blob.core.windows.net/c?sv=2024-11-04&sig=SECRET)"
                .to_owned(),
        );
        assert_eq!(err.user_message(), Fault::Unreachable.message(VENDOR));
        for forbidden in ["://", "http", "sig=", "sv=", "?", "&"] {
            assert!(
                !err.user_message().contains(forbidden),
                "{} carries {forbidden:?}",
                err.user_message()
            );
        }
    }

    #[test]
    fn the_findings_the_vocabulary_has_no_word_for_keep_their_own_sentences() {
        assert_eq!(
            AzureCostError::MissingColumn(COST_COLUMN.to_owned()).user_message(),
            "Export CSV missing 'costInUsd' column"
        );
        assert_eq!(
            AzureCostError::InvalidUrl.user_message(),
            "Invalid blob URL — check the SAS URL in Settings"
        );
    }
}
