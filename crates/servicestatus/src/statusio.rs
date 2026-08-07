//! status.io — Neon.
//!
//! `neonstatus.com` looks like a Statuspage and is not: it is status.io, whose
//! API lives on a different host entirely
//! (`api.status.io/1.0/status/<page_id>`) and whose payload nests each
//! component's real status one level down, inside `containers[]`. Verified live
//! 2026-08-06.
//!
//! ```jsonc
//! { "result": {
//!     "status_overall": { "status": "Operational", "status_code": 100 },
//!     "status": [ { "name": "Database Connectivity",
//!                   "containers": [ { "status": "Operational" } ] } ] } }
//! ```
//!
//! Status words are Title Case prose rather than Statuspage's snake_case
//! tokens, and the ladder is status.io's own — mapped onto [`ComponentStatus`]
//! so the app has one vocabulary to render.

use crate::{get_text, http_client, ComponentStatus, Incident, ServiceStatus, StatusError};

/// Reads one status.io page.
#[derive(Debug)]
pub struct StatusIoClient {
    base_url: String,
    page_id: String,
    /// The component to report, by display name. status.io's component ids are
    /// per-page and not published on the page itself, so unlike
    /// [`crate::statuspage`] the name is the only stable handle available.
    /// `None` reports `status_overall`, which is what the page's own banner
    /// shows.
    component: Option<String>,
    http: reqwest::Client,
}

/// `https://api.status.io`.
const DEFAULT_BASE_URL: &str = "https://api.status.io";

/// Neon's status.io page id, from `neonstatus.com`'s own embedded links.
pub const NEON_PAGE_ID: &str = "6878fc85709daa75be6c7e3c";

impl StatusIoClient {
    #[must_use]
    pub fn new(page_id: impl Into<String>, component: Option<String>) -> Self {
        Self::with_base_url(DEFAULT_BASE_URL, page_id, component)
    }

    /// Exists so tests (and only tests) can point the client at a mock server.
    #[must_use]
    pub fn with_base_url(
        base_url: impl Into<String>,
        page_id: impl Into<String>,
        component: Option<String>,
    ) -> Self {
        StatusIoClient {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            page_id: page_id.into(),
            component,
            http: http_client(),
        }
    }

    /// One status read.
    ///
    /// # Errors
    /// [`StatusError`] when the page is unreachable, answers non-2xx, or sends
    /// something this build cannot decode.
    pub async fn status(&self) -> Result<ServiceStatus, StatusError> {
        let url = format!("{}/1.0/status/{}", self.base_url, self.page_id);
        parse_status(
            &get_text(&self.http, &url).await?,
            self.component.as_deref(),
        )
    }
}

/// status.io's status words, mapped onto the shared ladder.
///
/// Unrecognised is `None`, never `Operational` — same rule as everywhere else.
/// "Planned Maintenance" is deliberately *not* mapped to a degraded level: it
/// is a scheduled state the operator already knows about, and colouring it like
/// an outage would cry wolf on a Tuesday night every month.
#[must_use]
pub fn parse_component_status(raw: &str) -> Option<ComponentStatus> {
    match raw {
        "Operational" => Some(ComponentStatus::Operational),
        "Degraded Performance" => Some(ComponentStatus::DegradedPerformance),
        "Partial Service Disruption" => Some(ComponentStatus::PartialOutage),
        "Service Disruption" | "Security Event" => Some(ComponentStatus::MajorOutage),
        _ => None,
    }
}

/// Decode a status.io payload.
///
/// `component` selects one entry of `result.status[]` by display name; `None`
/// reads `result.status_overall`.
///
/// # Errors
/// [`StatusError::DecodeFailed`] if the body is not JSON.
pub fn parse_status(body: &str, component: Option<&str>) -> Result<ServiceStatus, StatusError> {
    let root: serde_json::Value =
        serde_json::from_str(body).map_err(|e| StatusError::DecodeFailed(e.to_string()))?;
    let result = root.get("result");

    let word = match component {
        None => result
            .and_then(|r| r.get("status_overall"))
            .and_then(|s| s.get("status"))
            .and_then(serde_json::Value::as_str),
        Some(name) => result
            .and_then(|r| r.get("status"))
            .and_then(|s| s.as_array())
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|e| e.get("name").and_then(serde_json::Value::as_str) == Some(name))
            })
            // The component's real status lives one level down, per container;
            // the entry itself carries only a name and an id.
            .and_then(|e| e.get("containers"))
            .and_then(|c| c.as_array())
            .and_then(|containers| containers.first())
            .and_then(|c| c.get("status"))
            .and_then(serde_json::Value::as_str),
    };

    // status.io publishes incidents under `result.incidents`, each with a
    // `name`. There is no `impact` field, so severity is taken from the status
    // word we already decoded rather than invented.
    let incident = result
        .and_then(|r| r.get("incidents"))
        .and_then(|i| i.as_array())
        .and_then(|incidents| incidents.first())
        .and_then(|i| i.get("name"))
        .and_then(serde_json::Value::as_str)
        .map(|name| Incident {
            name: name.to_owned(),
            impact: match word.and_then(parse_component_status) {
                Some(ComponentStatus::MajorOutage) => "major".to_owned(),
                Some(s) if s.is_degraded() => "minor".to_owned(),
                _ => "none".to_owned(),
            },
        });

    Ok(ServiceStatus {
        component: word.and_then(parse_component_status),
        incident,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured live from Neon's status.io page, 2026-08-06.
    const HEALTHY: &str = include_str!("../tests/fixtures/statusio_healthy.json");

    #[test]
    fn the_live_capture_decodes_to_operational() {
        let s = parse_status(HEALTHY, None).expect("decodes");
        assert_eq!(s.component, Some(ComponentStatus::Operational));
        assert!(s.incident.is_none());
    }

    /// The real status lives inside `containers[]`, not on the entry — reading
    /// the entry's own fields yields nothing at all.
    #[test]
    fn a_named_component_is_read_from_its_container() {
        let s = parse_status(HEALTHY, Some("Database Connectivity")).expect("decodes");
        assert_eq!(s.component, Some(ComponentStatus::Operational));
    }

    #[test]
    fn a_component_this_page_does_not_publish_is_unknown() {
        assert_eq!(
            parse_status(HEALTHY, Some("Nonexistent"))
                .expect("decodes")
                .component,
            None
        );
    }

    #[test]
    fn status_io_words_map_onto_the_shared_ladder() {
        assert_eq!(
            parse_component_status("Service Disruption"),
            Some(ComponentStatus::MajorOutage)
        );
        assert_eq!(
            parse_component_status("Partial Service Disruption"),
            Some(ComponentStatus::PartialOutage)
        );
        assert_eq!(
            parse_component_status("Degraded Performance"),
            Some(ComponentStatus::DegradedPerformance)
        );
        // Scheduled work the operator already knows about is not an outage, and
        // colouring it like one would cry wolf every maintenance window.
        assert_eq!(parse_component_status("Planned Maintenance"), None);
        assert_eq!(parse_component_status("something new"), None);
    }

    #[test]
    fn a_disrupted_page_reports_the_outage_and_names_its_incident() {
        let down = HEALTHY
            .replace(
                r#""status": "Operational""#,
                r#""status": "Service Disruption""#,
            )
            .replace(
                r#""incidents": []"#,
                r#""incidents": [{"name": "Elevated errors"}]"#,
            );
        let s = parse_status(&down, None).expect("decodes");
        assert_eq!(s.component, Some(ComponentStatus::MajorOutage));
        let incident = s.incident.expect("incident");
        assert_eq!(incident.name, "Elevated errors");
        assert_eq!(
            incident.impact, "major",
            "severity comes from the status word"
        );
    }

    #[test]
    fn a_body_that_is_not_json_is_a_decode_failure_not_a_panic() {
        assert!(matches!(
            parse_status("<html>", None).expect_err("not JSON"),
            StatusError::DecodeFailed(_)
        ));
    }

    #[tokio::test]
    async fn a_successful_read_hits_the_page_id_path() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(format!(
                "/1.0/status/{NEON_PAGE_ID}"
            )))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_raw(HEALTHY, "application/json"),
            )
            .mount(&server)
            .await;

        let status = StatusIoClient::with_base_url(server.uri(), NEON_PAGE_ID, None)
            .status()
            .await
            .expect("status");
        assert_eq!(status.component, Some(ComponentStatus::Operational));
    }
}
