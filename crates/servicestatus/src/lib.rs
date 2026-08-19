//! Third-party availability, read from whatever each vendor publishes.
//!
//! Five vendors this stack depends on, and three different transports between
//! them — so the *vocabulary* lives here and each adapter's job is to speak it:
//!
//! | Vendor | Transport | Module |
//! |---|---|---|
//! | GitHub, Anthropic, Vercel | Atlassian Statuspage | [`statuspage`] |
//! | Neon | status.io | [`statusio`] |
//! | Azure | RSS, incidents only | [`azurefeed`] |
//!
//! Extracted from `crates/github` once it stopped being about GitHub. The
//! *conjunction* — folding a vendor's status with our own fleet into an "is it
//! us?" verdict — stays there, because it needs `RunnerSummary` and only
//! GitHub has one.
//!
//! Two rules, both inherited from the crates around it:
//!
//! **Unknown is not zero**, restated as *unreachable is not operational*. Every
//! adapter yields `Option<ComponentStatus>` and answers `None` rather than
//! guessing: a page we could not read, a status word this build does not
//! recognise, and a component that has vanished from the payload are all
//! "we do not know", and a caller that renders that as green is reporting a
//! happy path nobody observed.
//!
//! **Nothing here reads the wall clock**, and nothing here polls. Adapters are
//! clients plus pure decoders; cadence and retained state live in the app.
//!
//! [`discover`] is the one module that is not an adapter: it probes a
//! *user-supplied* host for the components it publishes, so a vendor this build
//! has never heard of can be added without a rebuild. It reports what it found
//! and never guesses — including telling a `neonstatus.com` apart from a
//! Statuspage, which is the trap [`statusio`] documents.

pub mod azurefeed;
pub mod discover;
pub mod statusio;
pub mod statuspage;

use fault::Fault;

pub use azurefeed::AzureFeedClient;
pub use discover::{Component, ProbeError};
pub use statusio::StatusIoClient;
pub use statuspage::StatusPageClient;

/// What every failure in this crate blames, and the only thing
/// [`Fault::message`] interpolates.
///
/// **Not the vendor.** A status page and the service it describes fail
/// independently, and "couldn't reach GitHub" from an unreadable
/// `githubstatus.com` would be a claim about GitHub that nothing here observed.
/// It carries its article so every stock sentence reads as English around it.
const STATUS_PAGE: &str = "the status page";

/// One component's health, in Atlassian Statuspage's vocabulary.
///
/// The other two transports map onto this rather than carrying their own, so
/// the app has one ladder to render and one set of words to colour. Where a
/// vendor cannot express a level — Azure publishes incidents and never says
/// "operational" — the adapter says so in its own docs and answers the nearest
/// honest value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ComponentStatus {
    Operational,
    DegradedPerformance,
    PartialOutage,
    MajorOutage,
}

impl ComponentStatus {
    /// Statuspage's wire strings. An unrecognised value is `None` rather than
    /// a defaulted `Operational` — a word we do not know is not a promise that
    /// things are fine.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "operational" => Some(ComponentStatus::Operational),
            "degraded_performance" => Some(ComponentStatus::DegradedPerformance),
            "partial_outage" => Some(ComponentStatus::PartialOutage),
            "major_outage" => Some(ComponentStatus::MajorOutage),
            _ => None,
        }
    }

    /// Whether the vendor is admitting to a problem with this component.
    #[must_use]
    pub fn is_degraded(self) -> bool {
        !matches!(self, ComponentStatus::Operational)
    }

    /// The word to show a human — Statuspage's own, lower-cased for prose.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ComponentStatus::Operational => "operational",
            ComponentStatus::DegradedPerformance => "degraded performance",
            ComponentStatus::PartialOutage => "partial outage",
            ComponentStatus::MajorOutage => "major outage",
        }
    }
}

/// An unresolved incident, reduced to the two fields worth showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incident {
    pub name: String,
    /// `none` / `minor` / `major` / `critical`. Statuspage's ladder; the other
    /// adapters map onto it.
    pub impact: String,
}

impl Incident {
    /// Statuspage's `impact` ladder, so "most impactful" is not alphabetical.
    #[must_use]
    pub fn rank(&self) -> u8 {
        match self.impact.as_str() {
            "critical" => 4,
            "major" => 3,
            "minor" => 2,
            "none" => 1,
            _ => 0,
        }
    }
}

/// One successful read of a vendor's status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStatus {
    /// `None` when the component was absent from the payload or carried a
    /// status word this build does not know.
    pub component: Option<ComponentStatus>,
    /// The most impactful unresolved incident, if any.
    pub incident: Option<Incident>,
}

#[derive(Debug, thiserror::Error)]
pub enum StatusError {
    #[error("unreachable: {0}")]
    Unreachable(String),
    #[error("the status page returned HTTP {0}")]
    HttpStatus(u16),
    #[error("could not decode the status payload: {0}")]
    DecodeFailed(String),
}

impl StatusError {
    /// What the panel shows. Deliberately blames the *status page*, never the
    /// vendor itself — the two fail independently, and an unreachable page says
    /// nothing about whether the service is up.
    ///
    /// That distinction is carried entirely by [`STATUS_PAGE`], the only thing
    /// the `fault` vocabulary interpolates (#354): pass "GitHub" here instead
    /// and every one of these sentences becomes a claim about GitHub.
    ///
    /// These reads carry no credential, which is why the status arm goes
    /// through `fault::http_status_message` rather than
    /// `Fault::from_http_status`: a 401 or a 404 from a public status page is a
    /// wrong URL, and "rejected the credential" would name a credential that
    /// does not exist.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            StatusError::Unreachable(_) => Fault::Unreachable.message(STATUS_PAGE),
            StatusError::HttpStatus(code) => fault::http_status_message(*code, STATUS_PAGE),
            StatusError::DecodeFailed(_) => Fault::Undecodable.message(STATUS_PAGE),
        }
    }
}

/// One HTTP GET, with the error classification every adapter shares.
///
/// Returns the body as text rather than JSON: two of the three transports are
/// JSON and one is RSS, and the decode step is each adapter's own.
///
/// # Errors
/// [`StatusError`] when the host is unreachable or answers non-2xx.
pub(crate) async fn get_text(http: &reqwest::Client, url: &str) -> Result<String, StatusError> {
    let response = http
        .get(url)
        .send()
        .await
        // `without_url` so a status-page URL never reaches an operator-facing
        // string — the same discipline the credentialed clients follow, kept
        // here even though none of these carry a secret.
        .map_err(|e| StatusError::Unreachable(e.without_url().to_string()))?;
    if !response.status().is_success() {
        return Err(StatusError::HttpStatus(response.status().as_u16()));
    }
    response
        .text()
        .await
        .map_err(|e| StatusError::Unreachable(e.without_url().to_string()))
}

/// Eight seconds, shorter than the REST clients' fifteen: this is an annotation
/// on panels that render fine without it, so it must never be what makes a poll
/// pass slow.
///
/// Named rather than inlined because [`discover`] builds its own client — it
/// needs a redirect policy the adapters do not — and a probe drifting to a
/// different budget than the poll it is configuring would be its own surprise.
pub(crate) const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// The shared `reqwest` client every adapter builds.
#[must_use]
pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(concat!("Solador/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest client")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unrecognised_status_word_is_unknown_rather_than_operational() {
        assert_eq!(
            ComponentStatus::parse("major_outage"),
            Some(ComponentStatus::MajorOutage)
        );
        assert_eq!(ComponentStatus::parse("under_maintenance"), None);
        assert_eq!(ComponentStatus::parse(""), None);
    }

    #[test]
    fn only_operational_is_undegraded() {
        assert!(!ComponentStatus::Operational.is_degraded());
        for s in [
            ComponentStatus::DegradedPerformance,
            ComponentStatus::PartialOutage,
            ComponentStatus::MajorOutage,
        ] {
            assert!(s.is_degraded(), "{s:?}");
        }
    }

    /// Statuspage orders incidents newest-first, which is not worst-first.
    #[test]
    fn incident_impact_ranks_by_severity_not_alphabetically() {
        let rank = |impact: &str| {
            Incident {
                name: String::new(),
                impact: impact.to_owned(),
            }
            .rank()
        };
        assert!(rank("critical") > rank("major"));
        assert!(rank("major") > rank("minor"));
        assert!(rank("minor") > rank("none"));
        assert_eq!(rank("something-new"), 0, "an unknown impact ranks lowest");
    }

    #[test]
    fn every_error_blames_the_status_page_not_the_vendor() {
        for e in [
            StatusError::Unreachable("dns".to_owned()),
            StatusError::HttpStatus(503),
            StatusError::DecodeFailed("bad".to_owned()),
        ] {
            let msg = e.user_message();
            assert!(msg.contains("status page"), "{msg}");
        }
    }
}
