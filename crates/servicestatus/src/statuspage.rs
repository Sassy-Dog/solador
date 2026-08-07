//! Atlassian Statuspage — GitHub, Anthropic and Vercel.
//!
//! One `summary.json` read gives page status, every component and every
//! unresolved incident, which is why it is used rather than the three narrower
//! endpoints. Unauthenticated and CDN-backed, on a different host from each
//! vendor's API — so it tends to stay up exactly when it is needed.
//!
//! A vendor is a base URL plus the id of the one component worth watching. The
//! **id**, not the name: `components[]` carries entries that are not components
//! at all (GitHub's list includes *"Visit www.githubstatus.com for more
//! information"*), and display names are the vendor's to re-word. Verified live
//! 2026-08-06 against all three.

use crate::{get_text, http_client, ComponentStatus, Incident, ServiceStatus, StatusError};

/// Reads one vendor's `summary.json`.
#[derive(Debug)]
pub struct StatusPageClient {
    base_url: String,
    component_id: String,
    http: reqwest::Client,
}

impl StatusPageClient {
    /// `base_url` is scheme/host only — e.g. `https://www.githubstatus.com`.
    ///
    /// Note for Anthropic: `status.anthropic.com` **302s** to
    /// `status.claude.com`. Point at the destination. A redirect is a
    /// convenience a vendor can retire, and following one on every poll is a
    /// round trip spent re-learning something that is already known.
    #[must_use]
    pub fn new(base_url: impl Into<String>, component_id: impl Into<String>) -> Self {
        StatusPageClient {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            component_id: component_id.into(),
            http: http_client(),
        }
    }

    /// One `summary.json` read.
    ///
    /// # Errors
    /// [`StatusError`] when the page is unreachable, answers non-2xx, or sends
    /// something this build cannot decode.
    pub async fn status(&self) -> Result<ServiceStatus, StatusError> {
        let url = format!("{}/api/v2/summary.json", self.base_url);
        parse_summary(&get_text(&self.http, &url).await?, &self.component_id)
    }
}

/// Decode `summary.json` down to the component and the incident that matter.
///
/// A free function over the body so fixtures can be decoded in a unit test with
/// no mock server involved — Atlassian's payload shape is the part most likely
/// to drift, and it deserves tests one `include_str!` away.
///
/// # Errors
/// [`StatusError::DecodeFailed`] if the body is not JSON.
pub fn parse_summary(body: &str, component_id: &str) -> Result<ServiceStatus, StatusError> {
    let root: serde_json::Value =
        serde_json::from_str(body).map_err(|e| StatusError::DecodeFailed(e.to_string()))?;

    let component = root
        .get("components")
        .and_then(|c| c.as_array())
        .and_then(|components| {
            components
                .iter()
                .find(|c| c.get("id").and_then(serde_json::Value::as_str) == Some(component_id))
        })
        .and_then(|c| c.get("status"))
        .and_then(serde_json::Value::as_str)
        .and_then(ComponentStatus::parse);

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
                .max_by_key(Incident::rank)
        });

    Ok(ServiceStatus {
        component,
        incident,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured live from `https://www.githubstatus.com/api/v2/summary.json`
    /// during the 2026-08-06 Actions outage.
    const OUTAGE: &str = include_str!("../tests/fixtures/statuspage_outage.json");
    const HEALTHY: &str = include_str!("../tests/fixtures/statuspage_healthy.json");
    const ACTIONS: &str = "br0l2tvcx85d";

    #[test]
    fn the_outage_capture_decodes_to_a_major_outage_and_its_incident() {
        let s = parse_summary(OUTAGE, ACTIONS).expect("decodes");
        assert_eq!(s.component, Some(ComponentStatus::MajorOutage));
        let incident = s.incident.expect("an unresolved incident");
        assert_eq!(incident.name, "Incident with Actions");
        assert_eq!(incident.impact, "critical");
    }

    #[test]
    fn a_healthy_page_decodes_to_operational_and_no_incident() {
        let s = parse_summary(HEALTHY, ACTIONS).expect("decodes");
        assert_eq!(s.component, Some(ComponentStatus::Operational));
        assert!(s.incident.is_none());
    }

    /// The component list carries an entry called "Visit
    /// www.githubstatus.com for more information", which is not a component at
    /// all. Matching on the name would have to know to skip it; matching on the
    /// id never sees it.
    #[test]
    fn the_component_is_found_by_id_not_by_name() {
        assert!(OUTAGE.contains("Visit www.githubstatus.com"));
        let renamed = OUTAGE.replace("\"name\": \"Actions\"", "\"name\": \"GitHub Actions\"");
        assert_eq!(
            parse_summary(&renamed, ACTIONS).expect("decodes").component,
            Some(ComponentStatus::MajorOutage),
            "a re-worded display name must not lose the component"
        );
    }

    #[test]
    fn an_unrecognised_status_word_decodes_to_unknown() {
        let odd = OUTAGE.replace("\"major_outage\"", "\"under_maintenance\"");
        assert_eq!(
            parse_summary(&odd, ACTIONS).expect("decodes").component,
            None
        );
    }

    #[test]
    fn a_component_this_vendor_does_not_publish_is_unknown() {
        assert_eq!(
            parse_summary(OUTAGE, "not-a-real-component")
                .expect("decodes")
                .component,
            None
        );
    }

    #[test]
    fn a_body_that_is_not_json_is_a_decode_failure_not_a_panic() {
        let err = parse_summary("<html>503</html>", ACTIONS).expect_err("not JSON");
        assert!(matches!(err, StatusError::DecodeFailed(_)));
    }

    /// Statuspage orders newest-first, so the tooltip must pick the incident
    /// that matters rather than the latest one.
    #[test]
    fn the_most_impactful_incident_wins_not_the_first_listed() {
        let two = OUTAGE.replace(
            r#""incidents": ["#,
            r#""incidents": [{"name": "Minor thing", "impact": "minor"}, "#,
        );
        assert_eq!(
            parse_summary(&two, ACTIONS)
                .expect("decodes")
                .incident
                .expect("one")
                .name,
            "Incident with Actions"
        );
    }

    #[tokio::test]
    async fn a_successful_read_decodes_the_named_component() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/v2/summary.json"))
            .and(wiremock::matchers::header_exists("user-agent"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_raw(OUTAGE, "application/json"),
            )
            .mount(&server)
            .await;

        let status = StatusPageClient::new(server.uri(), ACTIONS)
            .status()
            .await
            .expect("summary");
        assert_eq!(status.component, Some(ComponentStatus::MajorOutage));
    }

    #[tokio::test]
    async fn a_non_2xx_is_an_http_status_error() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let err = StatusPageClient::new(server.uri(), ACTIONS)
            .status()
            .await
            .expect_err("503");
        assert!(matches!(err, StatusError::HttpStatus(503)));
    }

    #[tokio::test]
    async fn an_unroutable_host_is_unreachable_without_leaking_the_url() {
        let err = StatusPageClient::new("http://127.0.0.1:1", ACTIONS)
            .status()
            .await
            .expect_err("connection refused");
        assert!(matches!(err, StatusError::Unreachable(_)));
        assert!(!format!("{err}").contains("127.0.0.1:1"), "{err}");
    }
}
