//! Polls a DevCanopy agent over HTTP. Replaces
//! `Services/HostMetrics/RemoteHostMetricsService.swift`.
//!
//! The error variants mirror the Swift `failureTooltip` cases so the shell can
//! keep giving cause-specific guidance instead of a generic failure.

use std::borrow::Cow;
use std::time::Duration;

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
    /// Returns `Cow` rather than `&'static str` so the `HttpStatus` arm can
    /// interpolate the code: a 503 and a 500 send an operator to different
    /// places, and a fixed string told them neither. Only that arm allocates.
    pub fn user_message(&self) -> Cow<'static, str> {
        match self {
            AgentError::Unreachable(_) => {
                "Couldn't reach the agent. Check the host is up and the agent is running.".into()
            }
            AgentError::AuthFailed => {
                "Agent rejected the bearer token (401). Check the host's token in Settings.".into()
            }
            // Parity with the Swift this replaces
            // (`HostMetricsPanel.failureTooltip`): "Agent returned HTTP 503."
            AgentError::HttpStatus(code) => format!("Agent returned HTTP {code}.").into(),
            AgentError::DecodeFailed(_) => {
                "Agent responded but the payload didn't decode — likely agent/app version skew after a redeploy.".into()
            }
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
        let url = format!("{}/v1/snapshot", self.base_url);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| AgentError::Unreachable(e.to_string()))?;

        // Deliberately exact `200`, not all-2xx: `/v1/snapshot` answers a
        // successful poll with 200 and a body, so any other 2xx (a 204, say)
        // means the agent is not speaking the contract this client decodes and
        // is better surfaced than silently decoded as an empty body. Pinned by
        // `a_204_is_not_treated_as_success` — loosening this is a contract
        // change, so it should break that test rather than drift.
        match resp.status().as_u16() {
            200 => {}
            401 | 403 => return Err(AgentError::AuthFailed),
            other => return Err(AgentError::HttpStatus(other)),
        }

        let body = resp
            .text()
            .await
            .map_err(|e| AgentError::Unreachable(e.to_string()))?;
        serde_json::from_str(&body).map_err(|e| AgentError::DecodeFailed(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const FIXTURE: &str = include_str!("../../wire/tests/fixtures/snapshot.json");

    #[tokio::test]
    async fn sends_a_bearer_token_and_decodes_the_snapshot() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/snapshot"))
            .and(header("authorization", "Bearer s3cret"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(FIXTURE, "application/json"))
            .mount(&server)
            .await;

        let c = AgentClient::new(server.uri(), "s3cret");
        let snap = c.snapshot().await.expect("should decode");
        assert_eq!(snap.cpu.core_usages.len(), 16);
    }

    #[tokio::test]
    async fn a_401_is_auth_failed_not_a_generic_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/snapshot"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = AgentClient::new(server.uri(), "wrong")
            .snapshot()
            .await
            .unwrap_err();
        assert!(matches!(err, AgentError::AuthFailed));
        assert!(err.user_message().contains("token"));
    }

    #[tokio::test]
    async fn a_503_is_reported_with_its_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/snapshot"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let err = AgentClient::new(server.uri(), "t")
            .snapshot()
            .await
            .unwrap_err();
        assert!(matches!(err, AgentError::HttpStatus(503)));
        assert!(err.user_message().contains("503"));
    }

    /// The whole point of carrying `u16` instead of a flag: two different
    /// failures must not read identically to the operator staring at the card.
    #[test]
    fn a_status_user_message_names_the_code_so_500_and_503_read_differently() {
        assert_eq!(
            AgentError::HttpStatus(503).user_message(),
            "Agent returned HTTP 503."
        );
        assert_eq!(
            AgentError::HttpStatus(500).user_message(),
            "Agent returned HTTP 500."
        );
    }

    /// Pins the exact-`200` success rule in `snapshot()`. A 204 has no body to
    /// decode, so treating it as success would hand `serde_json` an empty
    /// string and report a decode failure — or, worse, silently succeed if the
    /// payload ever gained defaults. Loosening to all-2xx should break here.
    #[tokio::test]
    async fn a_204_is_not_treated_as_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/snapshot"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let err = AgentClient::new(server.uri(), "t")
            .snapshot()
            .await
            .unwrap_err();
        assert!(matches!(err, AgentError::HttpStatus(204)));
        assert!(err.user_message().contains("204"));
    }

    #[tokio::test]
    async fn malformed_json_is_decode_failed_so_skew_is_diagnosable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/snapshot"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw("{\"cpu\":1}", "application/json"),
            )
            .mount(&server)
            .await;

        let err = AgentClient::new(server.uri(), "t")
            .snapshot()
            .await
            .unwrap_err();
        assert!(matches!(err, AgentError::DecodeFailed(_)));
        assert!(err.user_message().contains("version skew"));
    }

    #[tokio::test]
    async fn an_unroutable_host_is_unreachable() {
        let c = AgentClient::new("http://127.0.0.1:1", "t");
        let err = c.snapshot().await.unwrap_err();
        assert!(matches!(err, AgentError::Unreachable(_)));
    }
}
