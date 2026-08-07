//! Azure — an RSS feed of active incidents, and nothing else.
//!
//! The odd one out, and worth being explicit about because it is weaker than
//! the other two. Microsoft publishes no per-component health endpoint without
//! authentication; `azure.status.microsoft/en-us/status/feed/` is an RSS
//! document whose `<item>`s are **currently-active incidents**. Verified live
//! 2026-08-06: a healthy Azure serves a well-formed feed with zero items.
//!
//! So this adapter can express exactly two things:
//!
//! - **items present** → [`ComponentStatus::PartialOutage`], named by the first
//!   item's title. Not `MajorOutage`: the feed does not grade severity, and
//!   claiming the worst level from an ungraded source would out-shout the
//!   vendors that *do* grade.
//! - **no items** → `None`.
//!
//! `None`, not `Operational`. An empty feed is the absence of a report, not a
//! report of health — Azure never says "everything is fine", and rendering
//! silence as a green tick would be the vacuous-green this whole module exists
//! to avoid. Callers show it as *no known incidents*, which is the true and
//! weaker claim.

use crate::{get_text, http_client, ComponentStatus, Incident, ServiceStatus, StatusError};

/// `https://azure.status.microsoft/en-us/status/feed/`.
const DEFAULT_URL: &str = "https://azure.status.microsoft/en-us/status/feed/";

/// Reads Azure's status RSS.
#[derive(Debug)]
pub struct AzureFeedClient {
    url: String,
    http: reqwest::Client,
}

impl Default for AzureFeedClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AzureFeedClient {
    #[must_use]
    pub fn new() -> Self {
        Self::with_url(DEFAULT_URL)
    }

    /// Exists so tests (and only tests) can point the client at a mock server.
    #[must_use]
    pub fn with_url(url: impl Into<String>) -> Self {
        AzureFeedClient {
            url: url.into(),
            http: http_client(),
        }
    }

    /// One feed read.
    ///
    /// # Errors
    /// [`StatusError`] when the feed is unreachable, answers non-2xx, or is not
    /// parseable as the RSS this expects.
    pub async fn status(&self) -> Result<ServiceStatus, StatusError> {
        parse_feed(&get_text(&self.http, &self.url).await?)
    }
}

/// Decode the incident feed.
///
/// Hand-parsed rather than pulling in an RSS crate: two tags are read, the
/// document is small and vendor-fixed, and a dependency whose whole job is
/// `<item>` and `<title>` would be more surface than the thing it replaces.
///
/// # Errors
/// [`StatusError::DecodeFailed`] if the body is not an RSS document at all —
/// which is what an error page served with a 200 looks like, and must not be
/// mistaken for "no incidents".
pub fn parse_feed(body: &str) -> Result<ServiceStatus, StatusError> {
    if !body.contains("<rss") && !body.contains("<channel") {
        return Err(StatusError::DecodeFailed("not an RSS document".to_owned()));
    }

    let titles: Vec<String> = body
        .split("<item>")
        .skip(1)
        .filter_map(|item| {
            let item = item.split("</item>").next()?;
            let start = item.find("<title>")? + "<title>".len();
            let end = item[start..].find("</title>")? + start;
            Some(unescape(item[start..end].trim()))
        })
        .filter(|t| !t.is_empty())
        .collect();

    let Some(first) = titles.first() else {
        // No items. Deliberately `None` and not `Operational` — see the module
        // doc: Azure never claims health, so neither may we on its behalf.
        return Ok(ServiceStatus {
            component: None,
            incident: None,
        });
    };

    Ok(ServiceStatus {
        // Ungraded, so the middle of the ladder. See the module doc.
        component: Some(ComponentStatus::PartialOutage),
        incident: Some(Incident {
            name: if titles.len() > 1 {
                format!("{first} (+{} more)", titles.len() - 1)
            } else {
                first.clone()
            },
            impact: "minor".to_owned(),
        }),
    })
}

/// The five XML entities an RSS title can carry. `CDATA` is unwrapped too —
/// Microsoft uses it for titles containing an ampersand.
fn unescape(raw: &str) -> String {
    let raw = raw
        .strip_prefix("<![CDATA[")
        .and_then(|r| r.strip_suffix("]]>"))
        .unwrap_or(raw);
    raw.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        // Last: an earlier pass would turn `&amp;lt;` into `<`.
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured live 2026-08-06, when Azure had nothing wrong: a well-formed
    /// feed with zero `<item>`s.
    const HEALTHY: &str = include_str!("../tests/fixtures/azure_feed_healthy.xml");

    /// The rule this adapter exists to get right. An empty feed is the absence
    /// of a report, not a report of health — Azure never says "operational",
    /// and inventing it on their behalf is exactly the vacuous green the whole
    /// module refuses.
    #[test]
    fn an_empty_feed_is_unknown_and_never_operational() {
        let s = parse_feed(HEALTHY).expect("decodes");
        assert_eq!(s.component, None);
        assert_ne!(s.component, Some(ComponentStatus::Operational));
        assert!(s.incident.is_none());
    }

    #[test]
    fn an_active_incident_is_reported_and_named() {
        let feed = HEALTHY.replace(
            "</channel>",
            "<item><title>Storage - East US - Service degradation</title></item></channel>",
        );
        let s = parse_feed(&feed).expect("decodes");
        assert_eq!(s.component, Some(ComponentStatus::PartialOutage));
        assert_eq!(
            s.incident.expect("incident").name,
            "Storage - East US - Service degradation"
        );
    }

    /// The feed does not grade severity, so neither do we. Claiming the top of
    /// the ladder from an ungraded source would out-shout the vendors that do
    /// grade — a single Azure advisory would look worse than a real GitHub
    /// major outage sitting beside it.
    #[test]
    fn an_ungraded_feed_never_claims_the_top_of_the_ladder() {
        let feed = HEALTHY.replace(
            "</channel>",
            "<item><title>Anything</title></item></channel>",
        );
        assert_ne!(
            parse_feed(&feed).expect("decodes").component,
            Some(ComponentStatus::MajorOutage)
        );
    }

    #[test]
    fn several_incidents_name_the_first_and_count_the_rest() {
        let feed = HEALTHY.replace(
            "</channel>",
            "<item><title>One</title></item><item><title>Two</title></item>\
             <item><title>Three</title></item></channel>",
        );
        assert_eq!(
            parse_feed(&feed)
                .expect("decodes")
                .incident
                .expect("i")
                .name,
            "One (+2 more)"
        );
    }

    #[test]
    fn entities_and_cdata_are_unwrapped() {
        let feed = HEALTHY.replace(
            "</channel>",
            "<item><title><![CDATA[Compute &amp; Storage]]></title></item></channel>",
        );
        assert_eq!(
            parse_feed(&feed)
                .expect("decodes")
                .incident
                .expect("i")
                .name,
            "Compute & Storage"
        );
    }

    /// An error page served with a 200 is the failure mode that would otherwise
    /// read as "no incidents" — the quietest possible way to be wrong.
    #[test]
    fn a_body_that_is_not_rss_is_a_decode_failure_not_an_all_clear() {
        let err = parse_feed("<html><body>Sorry</body></html>").expect_err("not RSS");
        assert!(matches!(err, StatusError::DecodeFailed(_)));
    }

    #[tokio::test]
    async fn a_successful_read_decodes_the_feed() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_raw(HEALTHY, "application/rss+xml"),
            )
            .mount(&server)
            .await;
        let s = AzureFeedClient::with_url(server.uri())
            .status()
            .await
            .expect("status");
        assert_eq!(s.component, None);
    }
}
