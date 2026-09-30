//! The pinning contract (#448), against a **real rustls server on a loopback
//! port**: every case runs an actual handshake, so what is asserted is what a
//! cockpit would experience, not what the verifier says when called directly.
//!
//! The acceptance criteria, each with its own test:
//!
//! * the pinned certificate connects;
//! * a different self-signed certificate is refused **before the
//!   `Authorization` header is sent** — the server records every request head it
//!   reads, and the assertion is that it read none;
//! * a certificate copied without its key is refused;
//! * a pinned host is never dialled over `http://` (no fallback, no redirect).
//!
//! The negative control — the same suite against a verifier that accepts every
//! certificate — is recorded on the PR that shipped this: the refusal tests go
//! red and only the positive ones stay green.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::ServerConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

const HEALTH_FIXTURE: &str = include_str!("../../wire/tests/fixtures/health.json");
const TOKEN: &str = "s3cret";

/// A throwaway self-signed identity.
struct Identity {
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
}

fn identity() -> Identity {
    let generated =
        rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).expect("certificate");
    Identity {
        cert_der: generated.cert.der().to_vec(),
        key_der: generated.key_pair.serialize_der(),
    }
}

/// Presents whatever certificate chain it is given, with whatever key — the
/// point of not using `ServerConfig::with_single_cert`, which checks that the
/// two belong together and would refuse to build the "copied certificate
/// without its key" server.
#[derive(Debug)]
struct Presents(Arc<CertifiedKey>);

impl ResolvesServerCert for Presents {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(Arc::clone(&self.0))
    }
}

fn server_config(chain: &[&[u8]], key_der: &[u8]) -> Arc<ServerConfig> {
    let provider = rustls::crypto::ring::default_provider();
    let key = provider
        .key_provider
        .load_private_key(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            key_der.to_vec(),
        )))
        .expect("key");
    let certified = CertifiedKey::new(
        chain
            .iter()
            .map(|der| CertificateDer::from(der.to_vec()))
            .collect(),
        key,
    );
    Arc::new(
        ServerConfig::builder_with_provider(Arc::new(provider))
            .with_protocol_versions(rustls::DEFAULT_VERSIONS)
            .expect("versions")
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(Presents(Arc::new(certified)))),
    )
}

/// The reply every request gets: a valid health payload, unless a test says
/// otherwise.
fn health_reply() -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{HEALTH_FIXTURE}",
        HEALTH_FIXTURE.len()
    )
}

/// A TLS agent on a loopback port that records the head of every request it
/// reads — which is what makes "the token was never sent" checkable.
struct TlsAgent {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
}

impl TlsAgent {
    async fn start(chain: &[&[u8]], key_der: &[u8], reply: String) -> Self {
        let acceptor = TlsAcceptor::from(server_config(chain, key_der));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    break;
                };
                let acceptor = acceptor.clone();
                let seen = Arc::clone(&seen);
                let reply = reply.clone();
                tokio::spawn(async move {
                    // A refused handshake ends here: the client aborted, and
                    // nothing was read.
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let mut head = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match tls.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => head.extend_from_slice(&chunk[..n]),
                        }
                    }
                    if head.is_empty() {
                        return;
                    }
                    seen.lock()
                        .expect("requests poisoned")
                        .push(String::from_utf8_lossy(&head).into_owned());
                    let _ = tls.write_all(reply.as_bytes()).await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        TlsAgent { addr, requests }
    }

    /// A well-behaved agent for `identity`.
    async fn serving(identity: &Identity) -> Self {
        Self::start(&[&identity.cert_der], &identity.key_der, health_reply()).await
    }

    fn url(&self) -> String {
        format!("https://{}", self.addr)
    }

    /// Every request head the server read, lowercased so a header is found
    /// however the client spells it.
    fn heads(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("requests poisoned")
            .iter()
            .map(|head| head.to_lowercase())
            .collect()
    }
}

// MARK: the acceptance criteria

#[tokio::test]
async fn the_pinned_certificate_connects_and_the_token_is_sent() {
    let id = identity();
    let agent = TlsAgent::serving(&id).await;
    let client = AgentClient::pinned(agent.url(), TOKEN, &fingerprint(&id.cert_der));

    let health = client.health().await.expect("the pinned agent answers");
    assert_eq!(health.hostname, "ubu-01");

    // The recorder is not deaf: on the connection that *was* accepted, the
    // token is there. Without this, the "never sent" assertions below could
    // pass because the recorder sees nothing at all.
    let heads = agent.heads();
    assert_eq!(heads.len(), 1);
    assert!(
        heads[0].contains(&format!("authorization: bearer {TOKEN}")),
        "{heads:?}"
    );
}

#[tokio::test]
async fn a_different_self_signed_certificate_is_refused_before_the_token_is_sent() {
    let pinned = identity();
    let impostor = identity();
    let agent = TlsAgent::serving(&impostor).await;
    let client = AgentClient::pinned(agent.url(), TOKEN, &fingerprint(&pinned.cert_der));

    let err = client.health().await.unwrap_err();

    assert!(matches!(err, AgentError::CertificateChanged), "{err:?}");
    assert!(
        agent.heads().is_empty(),
        "the server read a request from a client that should have aborted the handshake: {:?}",
        agent.heads()
    );
}

/// Every endpoint goes through the one request path, so the refusal cannot
/// depend on which one asked.
#[tokio::test]
async fn every_endpoint_refuses_a_certificate_that_is_not_the_pin() {
    let pinned = identity();
    let impostor = identity();
    let agent = TlsAgent::serving(&impostor).await;
    let client = AgentClient::pinned(agent.url(), TOKEN, &fingerprint(&pinned.cert_der));

    assert!(matches!(
        client.snapshot().await,
        Err(AgentError::CertificateChanged)
    ));
    assert!(matches!(
        client.containers().await,
        Err(AgentError::CertificateChanged)
    ));
    assert!(matches!(
        client.health().await,
        Err(AgentError::CertificateChanged)
    ));
    assert!(agent.heads().is_empty(), "{:?}", agent.heads());
}

#[tokio::test]
async fn a_certificate_copied_without_its_key_is_refused() {
    let pinned = identity();
    let thief = identity();
    // The impostor presents the PINNED certificate — the hash matches — but
    // can only sign with its own key.
    let agent = TlsAgent::start(&[&pinned.cert_der], &thief.key_der, health_reply()).await;
    let client = AgentClient::pinned(agent.url(), TOKEN, &fingerprint(&pinned.cert_der));

    let err = client.health().await.unwrap_err();

    assert!(matches!(err, AgentError::CertificateChanged), "{err:?}");
    assert!(agent.heads().is_empty(), "{:?}", agent.heads());
}

/// "Exactly one certificate": the pinned certificate with anything after it is
/// not what the operator trusted.
#[tokio::test]
async fn a_chain_is_refused_even_when_its_first_certificate_is_the_pin() {
    let pinned = identity();
    let extra = identity();
    let agent = TlsAgent::start(
        &[&pinned.cert_der, &extra.cert_der],
        &pinned.key_der,
        health_reply(),
    )
    .await;
    let client = AgentClient::pinned(agent.url(), TOKEN, &fingerprint(&pinned.cert_der));

    let err = client.health().await.unwrap_err();

    assert!(matches!(err, AgentError::CertificateChanged), "{err:?}");
    assert!(agent.heads().is_empty(), "{:?}", agent.heads());
}

#[tokio::test]
async fn a_pin_is_matched_however_the_operator_or_the_store_spelled_it() {
    let id = identity();
    let agent = TlsAgent::serving(&id).await;
    let canonical = fingerprint(&id.cert_der);
    let bare_lowercase: String = canonical.to_lowercase().replace(':', "");
    for spelling in [canonical.clone(), canonical.to_lowercase(), bare_lowercase] {
        AgentClient::pinned(agent.url(), TOKEN, &spelling)
            .health()
            .await
            .unwrap_or_else(|e| panic!("{spelling}: {e:?}"));
    }
}

/// A hand-edited `store.json` with a pin that is not a fingerprint. It must
/// fail closed — refuse everything — not read as "no pin" and connect, and not
/// as "no pin" and drop to HTTP.
#[tokio::test]
async fn a_pin_that_is_not_a_fingerprint_matches_nothing() {
    let id = identity();
    let agent = TlsAgent::serving(&id).await;
    for pin in ["", "not a fingerprint", "AB:CD"] {
        let err = AgentClient::pinned(agent.url(), TOKEN, pin)
            .health()
            .await
            .unwrap_err();
        assert!(
            matches!(err, AgentError::CertificateChanged),
            "{pin:?}: {err:?}"
        );
    }
    assert!(agent.heads().is_empty(), "{:?}", agent.heads());
}

// MARK: never http

/// The scheme in `base_url` is not the client's to honour once it is pinned:
/// even handed an `http://` URL, it dials TLS. The plain-HTTP server here
/// counts the requests it *parses*; a pinned client that downgraded would
/// produce one.
#[tokio::test]
async fn a_pinned_client_never_dials_http_even_when_told_to() {
    let plain = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/health"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(HEALTH_FIXTURE, "application/json"))
        .mount(&plain)
        .await;
    let id = identity();
    assert!(plain.uri().starts_with("http://"));

    let client = AgentClient::pinned(plain.uri(), TOKEN, &fingerprint(&id.cert_der));
    let err = client.health().await.unwrap_err();

    // A plain-HTTP peer where TLS was expected: its own state, not a downgrade
    // and not "unreachable".
    assert!(matches!(err, AgentError::NoTls), "{err:?}");
    assert_eq!(
        plain.received_requests().await.expect("recording on").len(),
        0,
        "a pinned client sent an HTTP request"
    );
}

/// …and again after a failure: the state that follows an error is the next
/// poll dialling https again, never a retry over http.
#[tokio::test]
async fn a_pinned_client_stays_on_https_after_a_failure() {
    let plain = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(HEALTH_FIXTURE, "application/json"))
        .mount(&plain)
        .await;
    let id = identity();
    let client = AgentClient::pinned(plain.uri(), TOKEN, &fingerprint(&id.cert_der));

    for _ in 0..3 {
        assert!(client.health().await.is_err());
    }

    assert_eq!(
        plain.received_requests().await.expect("recording on").len(),
        0
    );
}

#[tokio::test]
async fn a_redirect_to_http_is_not_followed() {
    let plain = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(HEALTH_FIXTURE, "application/json"))
        .mount(&plain)
        .await;
    let id = identity();
    let redirect = format!(
        "HTTP/1.1 302 Found\r\nlocation: {}/v1/health\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        plain.uri()
    );
    let agent = TlsAgent::start(&[&id.cert_der], &id.key_der, redirect).await;
    let client = AgentClient::pinned(agent.url(), TOKEN, &fingerprint(&id.cert_der));

    let err = client.health().await.unwrap_err();

    assert!(matches!(err, AgentError::HttpStatus(302)), "{err:?}");
    assert_eq!(
        plain.received_requests().await.expect("recording on").len(),
        0,
        "the redirect target was fetched"
    );
}

/// An unpinned client is the client it always was: plain HTTP, nothing checked.
/// (The rest of the crate's tests are this claim at length.)
#[tokio::test]
async fn an_unpinned_client_still_speaks_plain_http() {
    let plain = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/health"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(HEALTH_FIXTURE, "application/json"))
        .mount(&plain)
        .await;
    AgentClient::new(plain.uri(), TOKEN)
        .health()
        .await
        .expect("plain http");
}

// MARK: the words

#[test]
fn the_pairing_errors_have_their_own_sentences_and_none_is_unreachable() {
    let unreachable = AgentError::Unreachable("connection refused".into()).user_message();
    let changed = AgentError::CertificateChanged.user_message();
    let no_tls = AgentError::NoTls.user_message();

    assert_eq!(changed, Fault::CertificateChanged.message(AGENT));
    assert_eq!(no_tls, Fault::NoTls.message(AGENT));
    assert!(changed.contains("re-pair"), "{changed}");
    assert!(no_tls.contains("TLS"), "{no_tls}");
    for message in [&changed, &no_tls] {
        assert_ne!(message, &unreachable);
        assert!(!message.contains("check the host is up"), "{message}");
    }
    assert_ne!(changed, no_tls);
}

// MARK: the probe

#[tokio::test]
async fn the_probe_reports_the_fingerprint_of_the_certificate_presented() {
    let id = identity();
    let agent = TlsAgent::serving(&id).await;

    let found = probe_certificate("127.0.0.1", agent.addr.port())
        .await
        .expect("probe");

    assert_eq!(
        found,
        CertProbe::Tls {
            fingerprint: fingerprint(&id.cert_der)
        }
    );
    // The same string the agent's `tls-fingerprint` prints.
    let CertProbe::Tls { fingerprint: shown } = found else {
        unreachable!()
    };
    assert_eq!(certpin::normalise(&shown).as_deref(), Some(shown.as_str()));
}

/// The probe trusts nothing, so the one thing it must not do is hand the
/// bearer token to a peer nobody has verified. It carries none to hand over,
/// and what it does send is checked here.
#[tokio::test]
async fn the_probe_never_sends_a_credential() {
    let id = identity();
    let agent = TlsAgent::serving(&id).await;

    probe_certificate("127.0.0.1", agent.addr.port())
        .await
        .expect("probe");

    let heads = agent.heads();
    assert!(
        !heads.is_empty(),
        "the probe sent nothing, so this proved nothing"
    );
    for head in heads {
        assert!(!head.contains("authorization"), "{head}");
    }
}

#[tokio::test]
async fn the_probe_finds_no_tls_on_a_plain_http_agent() {
    let plain = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/health"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&plain)
        .await;
    let port = plain.address().port();

    assert_eq!(
        probe_certificate("127.0.0.1", port).await.expect("probe"),
        CertProbe::NoTls
    );
    // What reached the plain server was unauthenticated.
    let received = plain.received_requests().await.expect("recording on");
    assert!(
        !received.is_empty(),
        "nothing reached the server, so this proved nothing"
    );
    for request in received {
        assert!(!request.headers.contains_key("authorization"));
    }
}

#[tokio::test]
async fn the_probe_of_a_closed_port_is_unreachable_not_no_tls() {
    // Bind and drop: a port that was free a moment ago.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();

    let err = probe_certificate("127.0.0.1", port).await.unwrap_err();

    assert!(matches!(err, ProbeError::Unreachable(_)), "{err:?}");
    assert!(err.user_message().starts_with("couldn't reach that host"));
}

/// A peer replaying a certificate it copied has nothing worth showing an
/// operator to trust: the fingerprint would be real and the machine would not
/// be the one it names.
#[tokio::test]
async fn the_probe_will_not_offer_a_certificate_the_peer_cannot_prove_it_holds() {
    let copied = identity();
    let thief = identity();
    let agent = TlsAgent::start(&[&copied.cert_der], &thief.key_der, health_reply()).await;

    let err = probe_certificate("127.0.0.1", agent.addr.port())
        .await
        .unwrap_err();

    assert!(matches!(err, ProbeError::Unproven), "{err:?}");
}

// MARK: the shared fingerprint vector

/// The same certificate and expected string `agent/src/tls.rs` asserts with
/// *its* SHA-256. Two implementations of the digest, one file: if either
/// drifts from `openssl x509 -fingerprint -sha256`, one of the two suites is
/// red.
#[test]
fn the_shared_fixture_certificate_fingerprints_to_its_expected_string() {
    const CERT: &[u8] = include_bytes!("../../../tests/fixtures/tls/pinned-cert.der");
    const EXPECTED: &str = include_str!("../../../tests/fixtures/tls/pinned-cert.sha256");
    assert_eq!(fingerprint(CERT), EXPECTED);
}
