//! GitHub's own availability, and the conjunction verdict that makes it useful.
//!
//! A bare "GitHub is down" chip is the less interesting half. The question an
//! operator actually asks during an incident is *is it us?*, and answering it
//! needs both halves: GitHub's published `Actions` status **and** our fleet's
//! state. Only one of the four combinations is a page — GitHub operational
//! while our runners are offline — and the rest are reassurance that used to
//! cost an SSH and three commands to obtain.
//!
//! ```text
//! Actions        fleet     verdict
//! operational    offline   it's us       — investigate the fleet
//! degraded/out   offline   it's GitHub   — expected, self-heals
//! degraded/out   online    GitHub degraded
//! operational    online    all good
//! ```
//!
//! The transport is GitHub's Atlassian Statuspage: unauthenticated, CDN-backed,
//! and a different host from the REST API — so it stays up when the API does
//! not, which is exactly when it is needed.
//!
//! The crate's two rules apply here as everywhere:
//!
//! **Unknown is not zero**, restated as *unreachable is not operational*. A
//! statuspage fetch that fails or times out yields [`Verdict::Unknown`], never
//! a green "GitHub OK". A check that cannot answer must not report the happy
//! path — and it must not suppress the fleet reading either, which is why the
//! verdict annotates the panel rather than gating it.
//!
//! **Nothing here reads the wall clock**, and nothing here polls. This module
//! is a client plus pure mappings; the cadence and the retained state live in
//! the app.

use crate::runners::RunnerSummary;

/// `https://www.githubstatus.com`.
const DEFAULT_BASE_URL: &str = "https://www.githubstatus.com";

/// The `Actions` component's Statuspage id.
///
/// Matched by id, not by name, for two reasons observed in the live payload:
/// `components[]` carries a non-component entry called *"Visit
/// www.githubstatus.com for more information"*, and names are display strings
/// Atlassian is free to re-word. The id is the stable handle.
pub const ACTIONS_COMPONENT_ID: &str = "br0l2tvcx85d";

/// One Statuspage component's health, in Statuspage's own vocabulary.
///
/// Deliberately not collapsed to a bool here: the *panel* only needs
/// degraded-or-not, but the incident tooltip quotes the real word, and folding
/// four states into two at the transport would throw that away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    /// Whether GitHub is admitting to a problem with this component.
    #[must_use]
    pub fn is_degraded(self) -> bool {
        !matches!(self, ComponentStatus::Operational)
    }

    /// The word to show a human, Statuspage's own.
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
    /// `none` / `minor` / `major` / `critical`.
    pub impact: String,
}

/// One successful statuspage read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStatus {
    /// `None` when the Actions component was absent from the payload or
    /// carried a status word this build does not know.
    pub actions: Option<ComponentStatus>,
    /// The most impactful unresolved incident, if any.
    pub incident: Option<Incident>,
}

/// The three platforms the runner panel tracks by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Linux,
    Windows,
}

impl Platform {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Platform::MacOs => "macOS",
            Platform::Linux => "Linux",
            Platform::Windows => "Windows",
        }
    }
}

/// Which tracked platforms have runners registered but **none** online.
///
/// The `_total > 0` guard is the whole point: an org with no Windows runners
/// has `windows_online == 0` forever, and reading that as an outage would pin a
/// permanent red "it's us" on a fleet that is exactly as intended. `os_chips`
/// in the app makes the same distinction for the same reason.
///
/// Per platform rather than in aggregate because that is what the 2026-08-06
/// incident looked like: all ten Linux runners offline while both macs stayed
/// up, because the macs churn far less and their sessions were not being
/// invalidated. An aggregate "some runners are online" would have reported all
/// clear through it.
#[must_use]
pub fn offline_platforms(summary: &RunnerSummary) -> Vec<Platform> {
    let mut offline = Vec::new();
    for (platform, total, online) in [
        (Platform::MacOs, summary.macos_total, summary.macos_online),
        (Platform::Linux, summary.linux_total, summary.linux_online),
        (
            Platform::Windows,
            summary.windows_total,
            summary.windows_online,
        ),
    ] {
        if total > 0 && online == 0 {
            offline.push(platform);
        }
    }
    offline
}

/// The conjunction: what to tell an operator looking at the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// GitHub is fine and so are we.
    AllGood,
    /// GitHub is degraded; our runners are up regardless.
    GitHubDegraded,
    /// GitHub is degraded **and** a platform is dark — expected, self-heals.
    ItsGitHub,
    /// GitHub says it is operational and a platform is dark anyway. The one
    /// row that is a page rather than reassurance.
    ItsUs,
    /// The statuspage could not be read. Never green: a check that cannot
    /// answer must not report the happy path.
    Unknown,
}

/// The verdict, its short chip label, and the sentence behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conjunction {
    pub verdict: Verdict,
    /// Header-sized. The header already holds a title, a staleness warning and
    /// a count, so this stays a few characters and the detail goes in `detail`.
    pub label: String,
    /// The full explanation, for the chip's `title` — including the platforms
    /// involved and the incident name when there is one.
    pub detail: String,
}

/// Fold GitHub's status and our fleet into one verdict.
///
/// `status` is `None` when the last statuspage read failed. Note the asymmetry
/// that buys: an unreadable statuspage still reports the *fleet* half, because
/// suppressing a real outage because we could not reach a status page would be
/// the worst of both.
#[must_use]
pub fn conjunction(status: Option<&ServiceStatus>, summary: Option<&RunnerSummary>) -> Conjunction {
    let offline = summary.map(offline_platforms).unwrap_or_default();
    let dark = platform_list(&offline);

    let Some(status) = status else {
        return Conjunction {
            verdict: Verdict::Unknown,
            label: "GH ?".to_owned(),
            detail: match dark {
                Some(dark) => format!(
                    "Couldn't read GitHub's status page, so we can't say whether {dark} being \
                     offline is GitHub or us."
                ),
                None => "Couldn't read GitHub's status page.".to_owned(),
            },
        };
    };

    // Same rule one layer in: a status word we could not classify is not a
    // promise that Actions is healthy.
    let Some(actions) = status.actions else {
        return Conjunction {
            verdict: Verdict::Unknown,
            label: "GH ?".to_owned(),
            detail: "GitHub's status page did not report an Actions status.".to_owned(),
        };
    };

    let incident = status
        .incident
        .as_ref()
        .map(|i| format!(" Incident: {} ({}).", i.name, i.impact))
        .unwrap_or_default();

    match (actions.is_degraded(), dark) {
        (false, Some(dark)) => Conjunction {
            verdict: Verdict::ItsUs,
            label: "fleet down".to_owned(),
            detail: format!(
                "GitHub Actions is operational, so {dark} being offline is ours to \
                 investigate.{incident}"
            ),
        },
        (true, Some(dark)) => Conjunction {
            verdict: Verdict::ItsGitHub,
            label: "GH outage".to_owned(),
            detail: format!(
                "GitHub Actions: {}. {dark} being offline is expected while this lasts, and \
                 self-heals.{incident}",
                actions.label()
            ),
        },
        (true, None) => Conjunction {
            verdict: Verdict::GitHubDegraded,
            label: "GH degraded".to_owned(),
            detail: format!(
                "GitHub Actions: {}. Our runners are online regardless.{incident}",
                actions.label()
            ),
        },
        (false, None) => Conjunction {
            verdict: Verdict::AllGood,
            label: "GH ok".to_owned(),
            detail: "GitHub Actions is operational and every registered platform has runners \
                     online."
                .to_owned(),
        },
    }
}

/// `"Linux"`, `"Linux and Windows"`, `"macOS, Linux and Windows"` — or `None`
/// when nothing is dark, which is what the match above branches on.
fn platform_list(offline: &[Platform]) -> Option<String> {
    let names: Vec<&str> = offline.iter().map(|p| p.label()).collect();
    match names.as_slice() {
        [] => None,
        [one] => Some((*one).to_owned()),
        [rest @ .., last] => Some(format!("{} and {last}", rest.join(", "))),
    }
}

// MARK: - Transport

#[derive(Debug, thiserror::Error)]
pub enum StatusError {
    #[error("unreachable: {0}")]
    Unreachable(String),
    #[error("GitHub's status page returned HTTP {0}")]
    HttpStatus(u16),
    #[error("could not decode the status payload: {0}")]
    DecodeFailed(String),
}

impl StatusError {
    /// What the panel shows. Deliberately blames the *status page*, never
    /// GitHub itself — the two fail independently, and an unreachable page
    /// says nothing about whether Actions is up.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            StatusError::Unreachable(_) => "couldn't reach GitHub's status page".to_owned(),
            StatusError::HttpStatus(code) => {
                format!("GitHub's status page returned HTTP {code}")
            }
            StatusError::DecodeFailed(_) => "couldn't read GitHub's status page".to_owned(),
        }
    }
}

/// Reads `summary.json` — status, components and unresolved incidents in one
/// request, which is why it is the endpoint used rather than the three
/// narrower ones.
#[derive(Debug)]
pub struct StatusPageClient {
    base_url: String,
    http: reqwest::Client,
}

impl Default for StatusPageClient {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusPageClient {
    #[must_use]
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    /// Exists so tests (and only tests) can point the client at a mock server.
    ///
    /// No credential travels here at all — this endpoint is public — so unlike
    /// [`crate::GitHubClient::with_base_url`] there is nothing to redact and no
    /// invariant to uphold beyond the trailing slash.
    #[must_use]
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        StatusPageClient {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                // Shorter than the REST client's 15s: this is an annotation on
                // a panel that renders fine without it, so it must never be
                // what makes a poll pass slow.
                .timeout(std::time::Duration::from_secs(8))
                .user_agent(concat!("DevCanopy/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
        }
    }

    /// One `summary.json` read.
    ///
    /// # Errors
    /// [`StatusError`] when the page is unreachable, answers non-2xx, or sends
    /// something this build cannot decode.
    pub async fn summary(&self) -> Result<ServiceStatus, StatusError> {
        let url = format!("{}/api/v2/summary.json", self.base_url);
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| StatusError::Unreachable(e.without_url().to_string()))?;
        let code = response.status().as_u16();
        if !response.status().is_success() {
            return Err(StatusError::HttpStatus(code));
        }
        let body = response
            .text()
            .await
            .map_err(|e| StatusError::Unreachable(e.without_url().to_string()))?;
        parse_summary(&body)
    }
}

/// Decode `summary.json` into the two fields the panel needs.
///
/// A free function over the body so the fixtures can be parsed in a unit test
/// with no mock server involved — the shape of Atlassian's payload is the part
/// most likely to drift, and it deserves tests that are one `include_str!`
/// away rather than a wiremock round trip.
///
/// # Errors
/// [`StatusError::DecodeFailed`] if the body is not JSON.
pub fn parse_summary(body: &str) -> Result<ServiceStatus, StatusError> {
    let root: serde_json::Value =
        serde_json::from_str(body).map_err(|e| StatusError::DecodeFailed(e.to_string()))?;

    let actions = root
        .get("components")
        .and_then(|c| c.as_array())
        .and_then(|components| {
            components
                .iter()
                .find(|c| c.get("id").and_then(|id| id.as_str()) == Some(ACTIONS_COMPONENT_ID))
        })
        .and_then(|c| c.get("status"))
        .and_then(|s| s.as_str())
        .and_then(ComponentStatus::parse);

    // The most impactful unresolved incident. Statuspage orders newest-first,
    // which is not the same as worst-first.
    let incident = root
        .get("incidents")
        .and_then(|i| i.as_array())
        .and_then(|incidents| {
            incidents
                .iter()
                .filter_map(|i| {
                    Some(Incident {
                        name: i.get("name")?.as_str()?.to_owned(),
                        impact: i.get("impact")?.as_str()?.to_owned(),
                    })
                })
                .max_by_key(|i| impact_rank(&i.impact))
        });

    Ok(ServiceStatus { actions, incident })
}

/// Statuspage's `impact` ladder, so "most impactful" is not alphabetical.
fn impact_rank(impact: &str) -> u8 {
    match impact {
        "critical" => 4,
        "major" => 3,
        "minor" => 2,
        "none" => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runners::RunnerSummary;

    /// Captured live from `https://www.githubstatus.com/api/v2/summary.json`
    /// during the 2026-08-06 Actions outage that motivated this panel.
    const OUTAGE: &str = include_str!("../tests/fixtures/statuspage_outage.json");
    /// The same shape with everything operational and no incidents.
    const HEALTHY: &str = include_str!("../tests/fixtures/statuspage_healthy.json");

    fn fleet(
        macos: (usize, usize),
        linux: (usize, usize),
        windows: (usize, usize),
    ) -> RunnerSummary {
        RunnerSummary {
            total: macos.1 + linux.1 + windows.1,
            online: macos.0 + linux.0 + windows.0,
            busy: 0,
            idle: macos.0 + linux.0 + windows.0,
            macos_online: macos.0,
            macos_total: macos.1,
            linux_online: linux.0,
            linux_total: linux.1,
            windows_online: windows.0,
            windows_total: windows.1,
            other_online: 0,
            other_total: 0,
        }
    }

    // MARK: parsing

    #[test]
    fn the_outage_capture_decodes_to_a_major_actions_outage_and_its_incident() {
        let status = parse_summary(OUTAGE).expect("decodes");
        assert_eq!(status.actions, Some(ComponentStatus::MajorOutage));
        let incident = status.incident.expect("an unresolved incident");
        assert_eq!(incident.name, "Incident with Actions");
        assert_eq!(incident.impact, "critical");
    }

    #[test]
    fn a_healthy_page_decodes_to_operational_and_no_incident() {
        let status = parse_summary(HEALTHY).expect("decodes");
        assert_eq!(status.actions, Some(ComponentStatus::Operational));
        assert!(status.incident.is_none());
    }

    /// The component list carries an entry called "Visit
    /// www.githubstatus.com for more information", which is not a component at
    /// all. Matching on the name would have to know to skip it; matching on the
    /// id never sees it.
    #[test]
    fn the_actions_component_is_found_by_id_not_by_name() {
        assert!(OUTAGE.contains("Visit www.githubstatus.com"));
        let renamed = OUTAGE.replace("\"name\": \"Actions\"", "\"name\": \"GitHub Actions\"");
        assert_eq!(
            parse_summary(&renamed).expect("decodes").actions,
            Some(ComponentStatus::MajorOutage),
            "a re-worded display name must not lose the component"
        );
    }

    /// Unknown is not zero, at the transport: a status word this build does not
    /// recognise decodes to `None`, and `conjunction` turns that into
    /// `Unknown` rather than into a green chip.
    #[test]
    fn an_unrecognised_status_word_is_unknown_rather_than_operational() {
        let odd = OUTAGE.replace("\"major_outage\"", "\"under_maintenance\"");
        assert_eq!(parse_summary(&odd).expect("decodes").actions, None);
        assert_eq!(
            conjunction(
                Some(&parse_summary(&odd).expect("decodes")),
                Some(&fleet((2, 2), (10, 10), (0, 0)))
            )
            .verdict,
            Verdict::Unknown
        );
    }

    #[test]
    fn a_missing_actions_component_is_unknown() {
        let gone = OUTAGE.replace(ACTIONS_COMPONENT_ID, "not-the-actions-id");
        assert_eq!(parse_summary(&gone).expect("decodes").actions, None);
    }

    #[test]
    fn a_body_that_is_not_json_is_a_decode_failure_not_a_panic() {
        let err = parse_summary("<html>503</html>").expect_err("not JSON");
        assert!(matches!(err, StatusError::DecodeFailed(_)));
        assert_eq!(err.user_message(), "couldn't read GitHub's status page");
    }

    /// Newest-first is not worst-first: Statuspage orders by recency, so the
    /// tooltip must pick the incident that matters, not the latest one.
    #[test]
    fn the_most_impactful_incident_wins_not_the_first_listed() {
        let two = OUTAGE.replace(
            r#""incidents": ["#,
            r#""incidents": [{"name": "Minor thing", "impact": "minor"}, "#,
        );
        assert_eq!(
            parse_summary(&two)
                .expect("decodes")
                .incident
                .expect("one")
                .name,
            "Incident with Actions"
        );
    }

    // MARK: offline_platforms

    /// The `_total > 0` guard. An org with no Windows runners has
    /// `windows_online == 0` forever; reading that as an outage would pin a
    /// permanent red "it's us" on a fleet that is exactly as configured.
    #[test]
    fn a_platform_with_no_runners_at_all_is_not_offline() {
        let summary = fleet((2, 2), (10, 10), (0, 0));
        assert!(offline_platforms(&summary).is_empty());
    }

    /// The 2026-08-06 shape: every Linux runner dark, both macs up.
    #[test]
    fn a_platform_with_runners_but_none_online_is_offline() {
        let summary = fleet((2, 2), (0, 10), (0, 0));
        assert_eq!(offline_platforms(&summary), vec![Platform::Linux]);
    }

    #[test]
    fn several_dark_platforms_are_all_reported() {
        let summary = fleet((0, 2), (0, 10), (0, 3));
        assert_eq!(
            offline_platforms(&summary),
            vec![Platform::MacOs, Platform::Linux, Platform::Windows]
        );
    }

    // MARK: the matrix

    fn status(actions: ComponentStatus, incident: Option<&str>) -> ServiceStatus {
        ServiceStatus {
            actions: Some(actions),
            incident: incident.map(|name| Incident {
                name: name.to_owned(),
                impact: "critical".to_owned(),
            }),
        }
    }

    /// The row that earns the feature: GitHub says it is fine, and a platform
    /// is dark anyway. This is the only red one.
    #[test]
    fn operational_github_plus_a_dark_platform_is_our_problem() {
        let c = conjunction(
            Some(&status(ComponentStatus::Operational, None)),
            Some(&fleet((2, 2), (0, 10), (0, 0))),
        );
        assert_eq!(c.verdict, Verdict::ItsUs);
        assert_eq!(c.label, "fleet down");
        assert!(
            c.detail.contains("Linux"),
            "names the dark platform: {}",
            c.detail
        );
        assert!(c.detail.contains("ours to investigate"));
    }

    /// The 2026-08-06 reading. Same dark fleet, but GitHub is admitting to it,
    /// so nobody needs to SSH anywhere.
    #[test]
    fn degraded_github_plus_a_dark_platform_is_githubs_problem() {
        let c = conjunction(
            Some(&status(
                ComponentStatus::MajorOutage,
                Some("Incident with Actions"),
            )),
            Some(&fleet((2, 2), (0, 10), (0, 0))),
        );
        assert_eq!(c.verdict, Verdict::ItsGitHub);
        assert_eq!(c.label, "GH outage");
        assert!(c.detail.contains("major outage"));
        assert!(c.detail.contains("expected"));
        assert!(c.detail.contains("Incident with Actions"), "{}", c.detail);
    }

    #[test]
    fn degraded_github_with_a_healthy_fleet_says_so() {
        let c = conjunction(
            Some(&status(ComponentStatus::PartialOutage, None)),
            Some(&fleet((2, 2), (10, 10), (0, 0))),
        );
        assert_eq!(c.verdict, Verdict::GitHubDegraded);
        assert_eq!(c.label, "GH degraded");
        assert!(c.detail.contains("online regardless"));
    }

    #[test]
    fn operational_github_with_a_healthy_fleet_is_all_good() {
        let c = conjunction(
            Some(&status(ComponentStatus::Operational, None)),
            Some(&fleet((2, 2), (10, 10), (0, 0))),
        );
        assert_eq!(c.verdict, Verdict::AllGood);
        assert_eq!(c.label, "GH ok");
    }

    /// Unreachable is not operational. The fleet half survives, because
    /// suppressing a real outage over an unreachable status page would be the
    /// worst of both.
    #[test]
    fn an_unreadable_status_page_is_unknown_and_still_reports_the_fleet() {
        let c = conjunction(None, Some(&fleet((2, 2), (0, 10), (0, 0))));
        assert_eq!(c.verdict, Verdict::Unknown);
        assert_ne!(c.verdict, Verdict::AllGood);
        assert_eq!(c.label, "GH ?");
        assert!(c.detail.contains("Linux"), "{}", c.detail);
        assert!(c.detail.contains("can't say"), "{}", c.detail);
    }

    /// …and with nothing dark it simply admits it does not know, rather than
    /// inventing a fleet claim to go with it.
    #[test]
    fn an_unreadable_status_page_with_a_healthy_fleet_claims_nothing() {
        let c = conjunction(None, Some(&fleet((2, 2), (10, 10), (0, 0))));
        assert_eq!(c.verdict, Verdict::Unknown);
        assert_eq!(c.detail, "Couldn't read GitHub's status page.");
    }

    /// Before the first runners fetch there is no fleet half either. The
    /// verdict must not read that as "every platform is online".
    #[test]
    fn no_runner_summary_yet_never_reads_as_a_healthy_fleet() {
        let c = conjunction(Some(&status(ComponentStatus::Operational, None)), None);
        assert_eq!(c.verdict, Verdict::AllGood);
        assert!(
            !c.detail.contains("offline"),
            "nothing is claimed dark without a summary: {}",
            c.detail
        );
    }

    #[test]
    fn the_platform_list_reads_as_a_sentence() {
        assert_eq!(platform_list(&[]), None);
        assert_eq!(platform_list(&[Platform::Linux]).as_deref(), Some("Linux"));
        assert_eq!(
            platform_list(&[Platform::Linux, Platform::Windows]).as_deref(),
            Some("Linux and Windows")
        );
        assert_eq!(
            platform_list(&[Platform::MacOs, Platform::Linux, Platform::Windows]).as_deref(),
            Some("macOS, Linux and Windows")
        );
    }

    // MARK: transport

    #[tokio::test]
    async fn a_successful_read_decodes_the_actions_component() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/v2/summary.json"))
            .and(wiremock::matchers::header_exists("user-agent"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_raw(OUTAGE, "application/json"),
            )
            .mount(&server)
            .await;

        let status = StatusPageClient::with_base_url(server.uri())
            .summary()
            .await
            .expect("summary");
        assert_eq!(status.actions, Some(ComponentStatus::MajorOutage));
    }

    #[tokio::test]
    async fn a_non_2xx_is_an_http_status_error() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let err = StatusPageClient::with_base_url(server.uri())
            .summary()
            .await
            .expect_err("503");
        assert!(matches!(err, StatusError::HttpStatus(503)));
    }

    /// The error string must not carry the URL — the same discipline the REST
    /// client follows, even though there is no credential in this one.
    #[tokio::test]
    async fn an_unroutable_host_is_unreachable_without_leaking_the_url() {
        let err = StatusPageClient::with_base_url("http://127.0.0.1:1")
            .summary()
            .await
            .expect_err("connection refused");
        assert!(matches!(err, StatusError::Unreachable(_)));
        assert!(!format!("{err}").contains("127.0.0.1:1"), "{err}");
        assert_eq!(err.user_message(), "couldn't reach GitHub's status page");
    }
}
