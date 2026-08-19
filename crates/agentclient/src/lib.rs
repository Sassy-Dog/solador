//! Polls a Solador agent over HTTP. Replaces
//! `RemoteHostMetricsService`.
//!
//! The error variants mirror the original `failureTooltip` cases so the shell can
//! keep giving cause-specific guidance instead of a generic failure.

use std::time::Duration;

use fault::Fault;

/// The operator-facing name of what failed, and the only thing
/// [`Fault::message`] interpolates.
///
/// It carries its article because every stock sentence has to read as English
/// around it: "couldn't reach agent" is not a sentence. The host it belongs to
/// is not in the string on purpose — this message lands in one host card's
/// error slot, and the card already says which host it is.
const AGENT: &str = "the agent";

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("unreachable: {0}")]
    Unreachable(String),
    #[error("agent rejected the token")]
    AuthFailed,
    #[error("agent returned HTTP {0}")]
    HttpStatus(u16),
    #[error("could not decode the agent payload: {0}")]
    DecodeFailed(String),
}

impl AgentError {
    /// Cause-specific guidance, so the operator chases the right layer.
    ///
    /// Every state the `fault` vocabulary names renders through it (#354),
    /// with the one thing only this crate knows appended — a stock sentence is
    /// the floor a message may not fall below, never a cap on how specific one
    /// may be. Nothing the transport or the decoder produced is interpolated;
    /// those payloads are for the log and for `Debug`.
    ///
    /// Returns an owned `String` rather than the `Cow` it used to: every arm
    /// allocates now, so the borrowed half of that type was answering a
    /// question nobody was asking.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            AgentError::Unreachable(_) => format!(
                "{} — check the host is up and the agent is running",
                Fault::Unreachable.message(AGENT)
            ),
            // The stock sentence, with no "(401)" and no "the host's": the
            // status code is transport plumbing, and which host's token to fix
            // is the identity of the card this lands in.
            AgentError::AuthFailed => Fault::CredentialRejected.message(AGENT),
            // The code survives whichever branch this takes, which is the
            // property the original relied on
            // (`HostMetricsPanel.failureTooltip`: "Agent returned HTTP 503.")
            // and the reason this variant carries a `u16` rather than a flag.
            // A 404 here is a missing endpoint, never "no such account", which
            // is exactly what `fault::http_status_message` refuses to guess.
            AgentError::HttpStatus(code) => fault::http_status_message(*code, AGENT),
            AgentError::DecodeFailed(_) => format!(
                "{} — likely agent/app version skew after a redeploy",
                Fault::Undecodable.message(AGENT)
            ),
        }
    }
}

pub struct AgentClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl AgentClient {
    /// # Invariant: no credentials in `base_url`
    ///
    /// `base_url` must be scheme/host/port only — never userinfo
    /// (`https://user:token@host`) and never a token in the query string. The
    /// bearer token is a separate argument and travels only via
    /// `.bearer_auth()`, which puts it in a header; it must never be
    /// concatenated into the URL.
    ///
    /// The reason is that URLs leak where headers do not: `reqwest` attaches
    /// the request URL to its errors and the `url` crate does not redact
    /// userinfo, so anything embedded here can reach a log line or an
    /// operator-facing error string. Nothing in this crate violates the
    /// invariant today — this is here so a future caller doesn't.
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
        }
    }

    pub async fn snapshot(&self) -> Result<wire::Snapshot, AgentError> {
        let body = self.get_body("/v1/snapshot").await?;
        serde_json::from_str(&body).map_err(decode_failed)
    }

    /// The Containers panel's source: every podman/docker/tart container and VM
    /// the agent can see. An empty list is a legitimate answer (a host running
    /// none), not a failure.
    pub async fn containers(&self) -> Result<Vec<wire::Container>, AgentError> {
        let body = self.get_body("/v1/containers").await?;
        serde_json::from_str(&body).map_err(decode_failed)
    }

    /// The Settings "Test" probe: agent hostname/version plus sampler liveness.
    pub async fn health(&self) -> Result<wire::Health, AgentError> {
        let body = self.get_body("/v1/health").await?;
        serde_json::from_str(&body).map_err(decode_failed)
    }

    /// The one request path every endpoint goes through: bearer auth, status
    /// classification, body read. Returns the raw body so each endpoint decodes
    /// its own type — everything *above* the decode is shared, which is the
    /// point. An `AgentError` must not depend on which URL produced it: a 401
    /// on `/v1/health` has to read exactly like a 401 on `/v1/snapshot`, or the
    /// operator gets different guidance for the same broken token.
    ///
    /// Deliberately exact `200`, not all-2xx: each `/v1` endpoint answers a
    /// successful request with 200 and a body, so any other 2xx (a 204, say)
    /// means the agent is not speaking the contract this client decodes and is
    /// better surfaced than silently decoded as an empty body. Pinned by
    /// `a_204_is_not_treated_as_success` — loosening this is a contract change,
    /// so it should break that test rather than drift.
    async fn get_body(&self, path: &str) -> Result<String, AgentError> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| AgentError::Unreachable(e.to_string()))?;

        match resp.status().as_u16() {
            200 => {}
            401 | 403 => return Err(AgentError::AuthFailed),
            other => return Err(AgentError::HttpStatus(other)),
        }

        resp.text()
            .await
            .map_err(|e| AgentError::Unreachable(e.to_string()))
    }
}

/// One place that turns a deserialisation failure into `DecodeFailed`, so all
/// three endpoints report agent/app skew identically.
fn decode_failed(e: serde_json::Error) -> AgentError {
    AgentError::DecodeFailed(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SNAPSHOT_FIXTURE: &str = include_str!("../../wire/tests/fixtures/snapshot.json");
    const CONTAINERS_FIXTURE: &str = include_str!("../../wire/tests/fixtures/containers.json");
    const HEALTH_FIXTURE: &str = include_str!("../../wire/tests/fixtures/health.json");

    /// The three endpoints, each erased to `Result<(), AgentError>` so one
    /// table drives the classification tests for all of them. That is the
    /// acceptance criterion in issue #153: the error an operator sees must not
    /// depend on which endpoint failed. A new endpoint added without a row here
    /// is an endpoint whose classification nobody checked.
    #[derive(Clone, Copy)]
    enum Endpoint {
        Snapshot,
        Containers,
        Health,
    }

    impl Endpoint {
        const ALL: [Endpoint; 3] = [Endpoint::Snapshot, Endpoint::Containers, Endpoint::Health];

        fn path(self) -> &'static str {
            match self {
                Endpoint::Snapshot => "/v1/snapshot",
                Endpoint::Containers => "/v1/containers",
                Endpoint::Health => "/v1/health",
            }
        }

        /// Call it, discarding the payload — these tests assert on the error.
        async fn call(self, c: &AgentClient) -> Result<(), AgentError> {
            match self {
                Endpoint::Snapshot => c.snapshot().await.map(|_| ()),
                Endpoint::Containers => c.containers().await.map(|_| ()),
                Endpoint::Health => c.health().await.map(|_| ()),
            }
        }
    }

    /// A mock agent that answers `route` with `template` and nothing else.
    async fn agent_replying(route: &str, template: ResponseTemplate) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(template)
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn sends_a_bearer_token_and_decodes_the_snapshot() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/snapshot"))
            .and(header("authorization", "Bearer s3cret"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(SNAPSHOT_FIXTURE, "application/json"),
            )
            .mount(&server)
            .await;

        let c = AgentClient::new(server.uri(), "s3cret");
        let snap = c.snapshot().await.expect("should decode");
        assert_eq!(snap.cpu.core_usages.len(), 16);
    }

    /// The `header` matcher is the assertion that the token travels: without
    /// it the mock never matches and the call comes back `HttpStatus(404)`.
    #[tokio::test]
    async fn sends_a_bearer_token_and_decodes_the_container_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/containers"))
            .and(header("authorization", "Bearer s3cret"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(CONTAINERS_FIXTURE, "application/json"),
            )
            .mount(&server)
            .await;

        let cs = AgentClient::new(server.uri(), "s3cret")
            .containers()
            .await
            .expect("should decode");
        assert_eq!(cs.len(), 3);
        assert_eq!(cs[0].name, "llm");
        assert!(cs.iter().any(|c| c.runtime == "tart" && c.image.is_none()));
    }

    #[tokio::test]
    async fn sends_a_bearer_token_and_decodes_health() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/health"))
            .and(header("authorization", "Bearer s3cret"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(HEALTH_FIXTURE, "application/json"),
            )
            .mount(&server)
            .await;

        let h = AgentClient::new(server.uri(), "s3cret")
            .health()
            .await
            .expect("should decode");
        assert_eq!(h.status, "ok");
        assert_eq!(h.hostname, "ubu-01");
        assert_eq!(h.sampler_stale, Some(false));
    }

    /// A host running no containers answers `[]`. That is a real answer, not
    /// skew, and must not be classified as `DecodeFailed`.
    #[tokio::test]
    async fn an_empty_container_list_is_a_success_not_a_decode_failure() {
        let server = agent_replying(
            "/v1/containers",
            ResponseTemplate::new(200).set_body_raw("[]", "application/json"),
        )
        .await;

        let cs = AgentClient::new(server.uri(), "t")
            .containers()
            .await
            .expect("an empty list is a valid answer");
        assert!(cs.is_empty());
    }

    #[tokio::test]
    async fn a_401_is_auth_failed_not_a_generic_status() {
        for ep in Endpoint::ALL {
            let server = agent_replying(ep.path(), ResponseTemplate::new(401)).await;
            let err = ep
                .call(&AgentClient::new(server.uri(), "wrong"))
                .await
                .unwrap_err();
            assert!(
                matches!(err, AgentError::AuthFailed),
                "{}: expected AuthFailed, got {err:?}",
                ep.path()
            );
            assert_eq!(err.user_message(), Fault::CredentialRejected.message(AGENT));
        }
    }

    #[tokio::test]
    async fn a_503_is_reported_with_its_status() {
        for ep in Endpoint::ALL {
            let server = agent_replying(ep.path(), ResponseTemplate::new(503)).await;
            let err = ep
                .call(&AgentClient::new(server.uri(), "t"))
                .await
                .unwrap_err();
            assert!(
                matches!(err, AgentError::HttpStatus(503)),
                "{}: expected HttpStatus(503), got {err:?}",
                ep.path()
            );
            assert!(err.user_message().contains("503"));
        }
    }

    /// The whole point of carrying `u16` instead of a flag: two different
    /// failures must not read identically to the operator staring at the card.
    ///
    /// Both are 5xx, so both now name the state as well as the code (#354) —
    /// and the code is still there, which is what keeps them apart.
    #[test]
    fn a_status_user_message_names_the_code_so_500_and_503_read_differently() {
        assert_eq!(
            AgentError::HttpStatus(503).user_message(),
            "the agent is failing on its side (HTTP 503)"
        );
        assert_eq!(
            AgentError::HttpStatus(500).user_message(),
            "the agent is failing on its side (HTTP 500)"
        );
    }

    /// A 404 from an agent is a missing endpoint — version skew after a
    /// redeploy — and never "no such account". The vocabulary's account-shaped
    /// reading of that status is the one thing this arm must not borrow.
    #[test]
    fn a_404_is_not_dressed_up_as_a_missing_account() {
        let message = AgentError::HttpStatus(404).user_message();
        assert_eq!(message, "the agent returned HTTP 404");
        assert!(!message.contains("account"), "{message}");
        assert_ne!(message, Fault::NotFound.message(AGENT));
    }

    /// Pins the exact-`200` success rule in `get_body()`. A 204 has no body to
    /// decode, so treating it as success would hand `serde_json` an empty
    /// string and report a decode failure — or, worse, silently succeed if a
    /// payload ever gained defaults (`Vec<Container>` is one `#[serde(default)]`
    /// away from that). Loosening to all-2xx should break here.
    #[tokio::test]
    async fn a_204_is_not_treated_as_success() {
        for ep in Endpoint::ALL {
            let server = agent_replying(ep.path(), ResponseTemplate::new(204)).await;
            let err = ep
                .call(&AgentClient::new(server.uri(), "t"))
                .await
                .unwrap_err();
            assert!(
                matches!(err, AgentError::HttpStatus(204)),
                "{}: expected HttpStatus(204), got {err:?}",
                ep.path()
            );
            assert!(err.user_message().contains("204"));
        }
    }

    /// One body that is valid JSON but the wrong shape for all three: an object
    /// missing every field `Snapshot`/`Health` require, and not an array at all
    /// for `containers()`.
    #[tokio::test]
    async fn malformed_json_is_decode_failed_so_skew_is_diagnosable() {
        for ep in Endpoint::ALL {
            let server = agent_replying(
                ep.path(),
                ResponseTemplate::new(200).set_body_raw("{\"cpu\":1}", "application/json"),
            )
            .await;
            let err = ep
                .call(&AgentClient::new(server.uri(), "t"))
                .await
                .unwrap_err();
            assert!(
                matches!(err, AgentError::DecodeFailed(_)),
                "{}: expected DecodeFailed, got {err:?}",
                ep.path()
            );
            assert!(err.user_message().contains("version skew"));
        }
    }

    #[tokio::test]
    async fn an_unroutable_host_is_unreachable() {
        let c = AgentClient::new("http://127.0.0.1:1", "t");
        for ep in Endpoint::ALL {
            let err = ep.call(&c).await.unwrap_err();
            assert!(
                matches!(err, AgentError::Unreachable(_)),
                "{}: expected Unreachable, got {err:?}",
                ep.path()
            );
        }
    }
}
