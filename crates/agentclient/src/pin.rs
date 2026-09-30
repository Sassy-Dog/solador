//! Certificate pinning for a host's agent (#448, part 2 of #445).
//!
//! An agent that serves TLS (#447) presents a **self-signed** certificate, so
//! there is no authority to ask whether it is the right one. The cockpit
//! answers that itself: the operator compared its fingerprint with what
//! `solador-agent tls-fingerprint` printed and clicked **Trust**, and from then
//! on this verifier accepts exactly that certificate and nothing else.
//!
//! # What "exactly that certificate" means
//!
//! * **One certificate.** A chain — anything beyond the end-entity — is
//!   refused. The agent presents one; a second is somebody else's.
//! * **The SHA-256 of the end-entity DER**, compared with the pin. No system
//!   roots, no webpki chain building, no expiry, no name check: a self-signed
//!   certificate's SAN list is fixed at first start (#457), so the *name* was
//!   never the identity — the certificate is.
//! * **Possession of the key.** Matching the hash is not enough — the
//!   certificate is public, and anyone can present a copy. The handshake
//!   signature is still verified with the key inside the pinned certificate
//!   (rustls' own [`verify_tls13_signature`] / [`verify_tls12_signature`]), so
//!   a copy without the private key fails the handshake.
//!
//! Both checks run **inside the handshake**, before the client sends a byte of
//! HTTP. That ordering is the security property: the `Authorization` header is
//! never written to a peer that has not proven it is the pinned agent.
//!
//! # No downgrade
//!
//! Nothing here falls back to `http://`. That is enforced twice more where the
//! client is built ([`crate::AgentClient::pinned`]): `https_only`, and no
//! redirects — a `302` to an `http://` URL is otherwise a downgrade the client
//! would follow on its own.
//!
//! # The provider
//!
//! `ring`, named explicitly ([`provider`]) rather than through rustls' process
//! default: the workspace resolves exactly one crypto backend (the one
//! `reqwest`'s `rustls-tls` already brings), and a second one — `aws-lc-rs` —
//! is a C/cmake build the Windows and macOS builds should not gain.

use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, Error as TlsError, OtherError,
    SignatureScheme,
};
use sha2::{Digest, Sha256};

/// The SHA-256 fingerprint of a DER certificate, in `crates/certpin`'s
/// canonical form — the string `solador-agent tls-fingerprint` prints and
/// Settings shows next to **Trust**.
#[must_use]
pub fn fingerprint(cert_der: &[u8]) -> String {
    certpin::format(&digest(cert_der))
}

fn digest(cert_der: &[u8]) -> [u8; certpin::DIGEST_LEN] {
    Sha256::digest(cert_der).into()
}

/// The one crypto provider this crate handshakes with. See the module note.
fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn config(verifier: Arc<dyn ServerCertVerifier>) -> ClientConfig {
    ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(rustls::DEFAULT_VERSIONS)
        // The ring provider supports the default protocol versions; this is a
        // build-time fact, not an operator-reachable failure.
        .expect("ring supports rustls' default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth()
}

/// Why a presented certificate was refused. The value travels inside rustls'
/// error, where a `Debug` of the failure shows it; nothing logs it today. The
/// *classification* the cockpit acts on is `rustls::Error::InvalidCertificate`,
/// which is what [`crate::AgentError::CertificateChanged`] is built from.
#[derive(Debug)]
enum Refusal {
    /// The end-entity certificate is not the pinned one.
    NotThePinned,
    /// More than one certificate was presented.
    Chain,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refusal::NotThePinned => "the presented certificate is not the pinned one",
            Refusal::Chain => "the agent presented more than one certificate",
        })
    }
}

impl std::error::Error for Refusal {}

fn refuse(why: Refusal) -> TlsError {
    TlsError::InvalidCertificate(CertificateError::Other(OtherError(Arc::new(why))))
}

/// Accepts exactly one certificate, and only from a peer holding its key.
#[derive(Debug)]
pub(crate) struct PinnedVerifier {
    /// The pinned digest, or `None` when the stored pin does not parse — a
    /// hand-edited `store.json`. `None` matches nothing, so a malformed pin
    /// fails **closed** (every handshake refused, "certificate changed"),
    /// never open and never as plain HTTP.
    expected: Option<[u8; certpin::DIGEST_LEN]>,
    provider: Arc<CryptoProvider>,
}

impl PinnedVerifier {
    pub(crate) fn config(pin: &str) -> ClientConfig {
        config(Arc::new(PinnedVerifier {
            expected: certpin::parse(pin),
            provider: provider(),
        }))
    }
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        if !intermediates.is_empty() {
            return Err(refuse(Refusal::Chain));
        }
        match self.expected {
            Some(pin) if pin == digest(end_entity.as_ref()) => Ok(ServerCertVerified::assertion()),
            _ => Err(refuse(Refusal::NotThePinned)),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Looks at a certificate **without trusting it**: accepts whatever is
/// presented, and remembers the fingerprint of the one whose key the peer
/// proved it holds.
///
/// The identity decision is the operator's, made by comparing the fingerprint
/// this reports with what the host printed — so this verifier decides nothing
/// about *who* the peer is. It still verifies the handshake signature, so the
/// fingerprint it reports belongs to a peer that holds the matching key rather
/// than to one replaying a certificate it copied.
///
/// Used only by [`crate::probe_certificate`], which sends nothing but an
/// unauthenticated `GET /v1/health`: no token ever crosses a connection whose
/// server nobody has verified yet.
#[derive(Debug)]
pub(crate) struct Recorder {
    proven: Mutex<Option<String>>,
    provider: Arc<CryptoProvider>,
}

impl Recorder {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Recorder {
            proven: Mutex::new(None),
            provider: provider(),
        })
    }

    pub(crate) fn config(self: &Arc<Self>) -> ClientConfig {
        config(Arc::clone(self) as Arc<dyn ServerCertVerifier>)
    }

    /// The fingerprint of the certificate the peer proved it holds the key
    /// for, if the handshake got that far.
    pub(crate) fn proven(&self) -> Option<String> {
        self.proven.lock().expect("recorder poisoned").clone()
    }

    fn remember(&self, cert: &CertificateDer<'_>) {
        *self.proven.lock().expect("recorder poisoned") = Some(fingerprint(cert.as_ref()));
    }
}

impl ServerCertVerifier for Recorder {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        let valid = verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )?;
        self.remember(cert);
        Ok(valid)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        let valid = verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )?;
        self.remember(cert);
        Ok(valid)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
