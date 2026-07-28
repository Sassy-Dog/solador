//! Polls a DevCanopy agent over HTTP. Replaces
//! `Services/HostMetrics/RemoteHostMetricsService.swift`.
//!
//! The error variants mirror the Swift `failureTooltip` cases so the shell can
//! keep giving cause-specific guidance instead of a generic failure.

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
    pub fn user_message(&self) -> &'static str {
        match self {
            AgentError::Unreachable(_) => {
                "Couldn't reach the agent. Check the host is up and the agent is running."
            }
            AgentError::AuthFailed => {
                "Agent rejected the bearer token (401). Check the host's token in Settings."
            }
            AgentError::HttpStatus(_) => "The agent responded with an error status.",
            AgentError::DecodeFailed(_) => {
                "Agent responded but the payload didn't decode — likely agent/app version skew after a redeploy."
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

    pub async fn snapshot(&self) -> Result<metrics::Snapshot, AgentError> {
        let url = format!("{}/v1/snapshot", self.base_url);
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

    const FIXTURE: &str = include_str!("../../metrics/tests/fixtures/snapshot.json");

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
    async fn a_500_is_reported_with_its_status() {
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
