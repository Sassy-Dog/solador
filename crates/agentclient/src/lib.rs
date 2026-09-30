//! Polls a Solador agent over HTTP, or over HTTPS pinned to one certificate.
//! Replaces `RemoteHostMetricsService`.
//!
//! The error variants mirror the original `failureTooltip` cases so the shell can
//! keep giving cause-specific guidance instead of a generic failure.
//!
//! # Two ways to dial
//!
//! [`AgentClient::new`] is plain HTTP, for a host that was never paired — over
//! Tailscale the transport is what carries the encryption. [`AgentClient::pinned`]
//! is for a host the operator paired (#448): HTTPS to one certificate, no
//! authority consulted, and **never** a fall back to `http://` (see `pin`).
//! [`probe_certificate`] is the pairing step itself: it fetches the certificate
//! an agent presents so the operator can compare its fingerprint, and trusts
//! nothing.

mod pin;

use std::time::Duration;

use fault::Fault;

pub use pin::fingerprint;

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
    /// A pinned host presented a certificate other than the pinned one — or
    /// one it could not prove it holds the key for (#448). Refused inside the
    /// handshake, so no request, and no token, was sent.
    #[error("the agent's certificate is not the pinned one")]
    CertificateChanged,
    /// A pinned host answered plain HTTP where TLS was expected (#448). Never
    /// retried over `http://`.
    #[error("the agent does not speak TLS")]
    NoTls,
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
            // Their own sentences (#448), never `Unreachable`'s: the machine
            // answered. "Check the host is up" would send the operator to the
            // network for a problem that is the pairing.
            AgentError::CertificateChanged => Fault::CertificateChanged.message(AGENT),
            AgentError::NoTls => Fault::NoTls.message(AGENT),
        }
    }
}

/// What [`probe_certificate`] found at an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertProbe {
    /// The agent speaks TLS and holds the key for the certificate it
    /// presented. `fingerprint` is that certificate's, in `crates/certpin`'s
    /// form — what the operator compares with `solador-agent tls-fingerprint`.
    /// **Not trusted by having been fetched.**
    Tls { fingerprint: String },
    /// Something answered plain HTTP on that port: an agent with TLS off (or
    /// one that predates it). There is nothing to pin.
    NoTls,
}

/// Why [`probe_certificate`] found no certificate and no plain-HTTP agent.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// Nothing answered, or what answered spoke neither TLS nor HTTP.
    #[error("unreachable: {0}")]
    Unreachable(String),
    /// A certificate was presented but the peer could not prove it holds the
    /// matching key. Something is replaying a certificate it copied, or is
    /// broken; either way there is nothing safe to show the operator to trust.
    #[error("the peer presented a certificate it could not prove it holds")]
    Unproven,
}

impl ProbeError {
    /// One sentence for the pairing form. As with [`AgentError`], nothing the
    /// transport produced is interpolated.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            ProbeError::Unreachable(_) => format!(
                "{} — check the address and port, and that the agent is running",
                Fault::Unreachable.message("that host")
            ),
            // No stock sentence names this; per the fault crate's convention a
            // crate whose failure the vocabulary has no word for keeps its own.
            ProbeError::Unproven => {
                "that host presented a certificate it can't prove it holds — nothing to trust"
                    .to_owned()
            }
        }
    }
}

/// Fetches the certificate the agent at `address:port` presents, **without
/// trusting it** — the first half of pairing (#448).
///
/// A handshake is made that accepts any certificate and reports the
/// fingerprint of the one whose key the peer proved it holds. The only request
/// sent is an unauthenticated `GET /v1/health`: no bearer token ever crosses a
/// connection nobody has verified. Trusting what this returns is the
/// operator's decision, made by comparing the fingerprint with what the host
/// printed, and nothing is persisted here.
///
/// If TLS fails because the peer is not speaking it, one plain-HTTP
/// unauthenticated request decides between "an agent with TLS off" and
/// "nothing there". That fallback exists only because no pin exists yet: a
/// host that has one is never dialled over `http://`.
pub async fn probe_certificate(address: &str, port: u16) -> Result<CertProbe, ProbeError> {
    let recorder = pin::Recorder::new();
    let tls = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .use_preconfigured_tls(recorder.config())
        .build()
        .expect("reqwest client");
    let outcome = tls
        .get(format!("https://{address}:{port}/v1/health"))
        .send()
        .await;

    // The handshake may have completed and the request still failed (a 401 is
    // not a failure at all; a reset after it is). What matters is whether the
    // peer proved it holds a certificate's key.
    if let Some(fingerprint) = recorder.proven() {
        return Ok(CertProbe::Tls { fingerprint });
    }
    let Err(error) = outcome else {
        // A response with no proven certificate cannot happen over TLS.
        return Err(ProbeError::Unreachable(
            "no certificate presented".to_owned(),
        ));
    };
    match tls_failure(&error) {
        Some(rustls::Error::InvalidCertificate(_)) => Err(ProbeError::Unproven),
        Some(rustls::Error::InvalidMessage(_)) => speaks_plain_http(address, port).await,
        _ => Err(ProbeError::Unreachable(error.to_string())),
    }
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The "no TLS" half of the probe: does anything answer HTTP there at all?
/// Unauthenticated, and any HTTP response — a 401 is the agent's normal answer
/// to it — counts.
async fn speaks_plain_http(address: &str, port: u16) -> Result<CertProbe, ProbeError> {
    let http = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("reqwest client");
    match http
        .get(format!("http://{address}:{port}/v1/health"))
        .send()
        .await
    {
        Ok(_) => Ok(CertProbe::NoTls),
        Err(error) => Err(ProbeError::Unreachable(error.to_string())),
    }
}

/// The `rustls` error at the bottom of a `reqwest` failure, if there is one.
///
/// Walks the `source()` chain, and looks *inside* each `io::Error` as well:
/// `io::Error::source()` skips the error it wraps and reports that error's own
/// source, so a `rustls::Error` boxed into an `io::Error` — which is how every
/// tokio TLS stream reports a handshake failure — is invisible to a plain
/// `source()` walk. They also nest — measured: an `io::Error` of kind `Other`
/// around an `io::Error` of kind `InvalidData` around the `rustls::Error` —
/// hence the recursion.
fn tls_failure(error: &reqwest::Error) -> Option<&rustls::Error> {
    fn inside<'a>(err: &'a (dyn std::error::Error + 'static)) -> Option<&'a rustls::Error> {
        if let Some(tls) = err.downcast_ref::<rustls::Error>() {
            return Some(tls);
        }
        let wrapped = err.downcast_ref::<std::io::Error>()?.get_ref()?;
        inside(wrapped)
    }
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(err) = current {
        if let Some(tls) = inside(err) {
            return Some(tls);
        }
        current = err.source();
    }
    None
}

/// How a failed send is classified: a certificate the pin refused, a peer that
/// is not speaking TLS, or — for everything else — unreachable.
fn send_failed(error: &reqwest::Error) -> AgentError {
    match tls_failure(error) {
        Some(rustls::Error::InvalidCertificate(_)) => AgentError::CertificateChanged,
        Some(rustls::Error::InvalidMessage(_)) => AgentError::NoTls,
        _ => AgentError::Unreachable(error.to_string()),
    }
}

pub struct AgentClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl AgentClient {
    /// A client for a host that was **never paired**: plain HTTP, no
    /// certificate checks — over Tailscale the transport is what carries the
    /// encryption. A paired host is [`AgentClient::pinned`].
    ///
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

    /// A client for a host the operator **paired** (#448): HTTPS to the one
    /// certificate whose SHA-256 is `fingerprint`, and nothing else.
    ///
    /// * The certificate is checked inside the handshake, so a peer that
    ///   presents a different one — or a copy of the right one without its
    ///   key — is refused before a single byte of HTTP, `Authorization`
    ///   included, is written ([`AgentError::CertificateChanged`]).
    /// * **It never dials `http://`.** Whatever scheme `base_url` names is
    ///   replaced with `https://`; the underlying client refuses any other
    ///   scheme outright (`https_only`) and follows no redirects, because a
    ///   `302` to an `http://` URL is a downgrade the client would otherwise
    ///   take on its own. A peer that answers plain HTTP is reported as
    ///   [`AgentError::NoTls`], not followed.
    /// * A `fingerprint` that does not parse (a hand-edited store) matches no
    ///   certificate: every handshake is refused. Failing closed, as a pin
    ///   that could be blanked into "off" would not be one.
    ///
    /// The same no-credentials-in-`base_url` invariant as [`AgentClient::new`].
    pub fn pinned(
        base_url: impl Into<String>,
        token: impl Into<String>,
        fingerprint: &str,
    ) -> Self {
        let base_url = base_url.into();
        let host = base_url
            .split_once("://")
            .map_or(base_url.as_str(), |(_, rest)| rest)
            .trim_end_matches('/');
        Self {
            base_url: format!("https://{host}"),
            token: token.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .use_preconfigured_tls(pin::PinnedVerifier::config(fingerprint))
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
            .map_err(|e| send_failed(&e))?;

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
mod pinning_tests;

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
