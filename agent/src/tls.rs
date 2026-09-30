//! Self-signed TLS for the agent, kept for the host's lifetime (#447, part 1
//! of #445).
//!
//! `SOLADOR_AGENT_TLS=1` in the env file is the opt-in (`flag_enabled`); with
//! it, `main.rs` terminates TLS itself with a self-signed ECDSA P-256
//! certificate instead of serving plain HTTP. The keypair lives beside
//! `~/.config/solador-agent.env` — the same directory
//! `agent/deploy/install.sh` already writes that file into — as
//! `solador-agent.tls.key` (mode 0600) and `solador-agent.tls.crt`,
//! namespaced the same way the env file itself is rather than as bare
//! `tls.key`/`tls.crt` in an XDG root every app shares. Generated **once**
//! by [`load_or_generate`] and never touched again by anything else in this
//! binary: not `solador-agent update`, not `rollback`, not a re-run of
//! `install.sh`. A new certificate would silently break every cockpit that
//! has pinned the old one's fingerprint (the cockpit's pin, #448), so a
//! half-present pair (one file without the other — a crash mid-write, or
//! manual tampering) is refused rather than "repaired" by regenerating.
//!
//! Only the code that will actually *serve* the certificate — the running
//! agent's own startup — ever generates it, because only that call site knows
//! the resolved bind host to put in the certificate's SAN list — so an
//! ordinary client that dials the bind address by name can verify it. (The
//! agent's own local health probes, in `install.sh` and `solador-agent
//! update`, no longer depend on that list: it is fixed at first start while the
//! bind can change afterwards, e.g. all interfaces to a tailnet address (#449),
//! so they check the pinned certificate itself and not the name it is dialled
//! by. A wildcard bind adds nothing to the list.) Every
//! other reader — `tls-fingerprint`, the update/rollback health probes — is
//! read-only: [`read_cert`] never creates anything, so there is no ordering
//! hazard between "the first thing to ask" and "the thing that actually
//! serves".
//!
//! The private key is never logged: nothing in this module or its callers
//! formats [`Material::key_der`], and the only place the bytes travel is into
//! the TLS server config and the 0600 file they were read from or written to.
//!
//! **On disk the files are PEM, not DER** — `curl`'s `cacert` (what
//! `agent/deploy/lib.sh`'s `verify_health` pins the local health check with)
//! defaults to expecting PEM and, verified against the real `curl` on this
//! host, refuses raw DER with exit 77 ("error setting certificate verify
//! locations") before a single byte of the request goes anywhere. [`Material`]
//! still holds DER internally — that is what `rcgen`, `rustls`/`axum-server`
//! and `reqwest::Certificate::from_der` all want — so PEM exists only at the
//! read/write boundary in [`generate`], [`load_or_generate`] and
//! [`read_cert`], via [`to_pem`]/[`from_pem`].

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;

use base64::Engine as _;
use sha2::{Digest, Sha256};

/// The private key, in the agent's config directory, mode 0600. PEM on disk,
/// PKCS#8 inside — what `rcgen::KeyPair::serialize_der()` produces and what
/// `rustls_pki_types::PrivateKeyDer::try_from` recognizes without help, once
/// [`from_pem`] has decoded it back to DER. Namespaced beside the env file
/// (`solador-agent.tls.key`, not a bare `tls.key` in an XDG root every app
/// shares) — the same convention `agent/deploy/install.sh` already uses for
/// `solador-agent.env` itself.
pub const KEY_FILE: &str = "solador-agent.tls.key";

/// The self-signed certificate beside [`KEY_FILE`], PEM-encoded X.509 on
/// disk. Not secret — `tls-fingerprint` exists to hand its hash out.
pub const CERT_FILE: &str = "solador-agent.tls.crt";

const CERT_PEM_LABEL: &str = "CERTIFICATE";
const KEY_PEM_LABEL: &str = "PRIVATE KEY";

/// A load, a generate, or a read failure. One string: every caller already
/// has a `FATAL:` / `ERROR:` prefix of its own to put in front of it.
#[derive(Debug)]
pub struct TlsError(pub String);

impl std::fmt::Display for TlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TlsError {}

/// The keypair and certificate, DER-encoded, exactly as read from or written
/// to disk. `key_der` is PKCS#8.
pub struct Material {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

/// Redacts `key_der` the same way `update::Serving` redacts its token: a
/// `{:?}` that leaked the private key into a log or a test failure message
/// would be exactly the mistake this module exists to avoid.
impl std::fmt::Debug for Material {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Material")
            .field("cert_der", &format!("<{} bytes>", self.cert_der.len()))
            .field("key_der", &"<redacted>")
            .finish()
    }
}

/// Is `SOLADOR_AGENT_TLS` on? The one place this is decided, so `main.rs`
/// (reading the process environment) and `update::read_serving` (reading the
/// parsed env file) cannot disagree about what "on" means.
#[must_use]
pub fn flag_enabled(raw: Option<&str>) -> bool {
    raw.map(str::trim) == Some("1")
}

/// The certificate's SHA-256 fingerprint, colon-hex, uppercase — the form
/// `solador-agent tls-fingerprint` prints and the form the cockpit shows next
/// to its **Trust** button, so an operator can compare the two by eye.
///
/// The digest is this crate's (`sha2`); how it is *written* is
/// `crates/certpin`'s, shared with the cockpit's client (#448) so the two ends
/// cannot disagree about case or separators.
#[must_use]
pub fn fingerprint_hex(cert_der: &[u8]) -> String {
    certpin::format(&Sha256::digest(cert_der).into())
}

/// Load [`KEY_FILE`] / [`CERT_FILE`] from `dir`, generating a self-signed ECDSA
/// P-256 keypair **once** if both are absent. Never regenerates while a key
/// exists. `extra_sans` are additional Subject Alternative Names beyond the
/// baseline `localhost` / `127.0.0.1` / `::1` — the caller's resolved bind
/// host, so the health probes that dial it (not loopback, in the common
/// Tailscale-bind case) can verify the certificate by standard hostname
/// matching against the pinned file, rather than by disabling verification.
/// Wildcards (`0.0.0.0`, `::`, `[::]`) are dropped: nothing ever dials them
/// literally.
///
/// Call this ONLY from the code that is about to serve the certificate.
/// Everything else reads with [`read_cert`].
pub fn load_or_generate(dir: &Path, extra_sans: &[String]) -> Result<Material, TlsError> {
    let key_path = dir.join(KEY_FILE);
    let cert_path = dir.join(CERT_FILE);
    match (fs::read(&key_path), fs::read(&cert_path)) {
        (Ok(key_pem), Ok(cert_pem)) => {
            let key_der = decode_pem_file(&key_path, &key_pem, KEY_PEM_LABEL)?;
            let cert_der = decode_pem_file(&cert_path, &cert_pem, CERT_PEM_LABEL)?;
            Ok(Material { cert_der, key_der })
        }
        (Err(ke), Err(ce)) if not_found(&ke) && not_found(&ce) => {
            generate(dir, &key_path, &cert_path, extra_sans)
        }
        (key_result, cert_result) => Err(TlsError(format!(
            "{} and {} must both exist or both be absent, but exactly one does ({}). Refusing \
             to regenerate: a new certificate would break every cockpit that has pinned the old \
             one's fingerprint. Restore the missing file from backup, or delete both to let the \
             agent generate a fresh pair.",
            key_path.display(),
            cert_path.display(),
            describe_pair(&key_result, &cert_result, &key_path, &cert_path),
        ))),
    }
}

/// Read [`CERT_FILE`] only — never creates anything. What `tls-fingerprint` and
/// the update/rollback health probes use: they must never be the reason a
/// certificate exists, or the SAN list the *serving* agent chose (see
/// [`load_or_generate`]) could be for a different bind than the one this
/// process is about to read. Returns DER (decoded from the on-disk PEM) —
/// the form `fingerprint_hex` and `reqwest::Certificate::from_der` both want.
pub fn read_cert(dir: &Path) -> Result<Vec<u8>, TlsError> {
    let path = dir.join(CERT_FILE);
    let pem = fs::read(&path).map_err(|e| {
        if not_found(&e) {
            TlsError(format!(
                "{} does not exist yet. Start the agent once with SOLADOR_AGENT_TLS=1 (it \
                 generates the certificate on its first start) before this can read it.",
                path.display()
            ))
        } else {
            TlsError(format!("reading {}: {e}", path.display()))
        }
    })?;
    decode_pem_file(&path, &pem, CERT_PEM_LABEL)
}

fn decode_pem_file(path: &Path, bytes: &[u8], label: &str) -> Result<Vec<u8>, TlsError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| TlsError(format!("{} is not valid UTF-8 PEM: {e}", path.display())))?;
    from_pem(label, text).map_err(|e| {
        TlsError(format!(
            "{} is not a valid PEM {label}: {}",
            path.display(),
            e.0
        ))
    })
}

/// DER to PEM: `-----BEGIN <label>-----`, base64 wrapped at 64 columns (the
/// conventional PEM width — RFC 7468 does not mandate one, but every tool
/// that writes PEM, including `openssl` and `rcgen`'s own `pem` feature,
/// wraps there), `-----END <label>-----`.
fn to_pem(label: &str, der: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        // ASCII (base64's own alphabet), so this cannot fail.
        out.push_str(std::str::from_utf8(chunk).expect("base64 output is ASCII"));
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

/// The inverse of [`to_pem`]: find the `label`'s BEGIN/END markers, strip
/// all whitespace from between them (line breaks included), and base64-decode
/// what remains. Deliberately minimal — this reads back exactly what
/// [`to_pem`] writes, not arbitrary third-party PEM (headers, CRLF, multiple
/// blocks); this module is the only writer of these two files.
fn from_pem(label: &str, pem: &str) -> Result<Vec<u8>, TlsError> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let body_start = pem
        .find(&begin)
        .ok_or_else(|| TlsError(format!("no '{begin}' marker")))?
        + begin.len();
    let body_end = pem[body_start..]
        .find(&end)
        .ok_or_else(|| TlsError(format!("no '{end}' marker")))?;
    let body: String = pem[body_start..body_start + body_end]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(&body)
        .map_err(|e| TlsError(format!("invalid base64 between the PEM markers: {e}")))
}

fn not_found(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::NotFound
}

fn describe_pair(
    key_result: &io::Result<Vec<u8>>,
    cert_result: &io::Result<Vec<u8>>,
    key_path: &Path,
    cert_path: &Path,
) -> String {
    let side = |result: &io::Result<Vec<u8>>, path: &Path, what: &str| match result {
        Ok(_) => format!("{what} present ({})", path.display()),
        Err(e) if not_found(e) => format!("{what} absent"),
        Err(e) => format!("{what} unreadable: {e}"),
    };
    format!(
        "{}, {}",
        side(key_result, key_path, "key"),
        side(cert_result, cert_path, "cert"),
    )
}

/// Generate the keypair and certificate, and install them atomically-per-file
/// (`.new`, then `rename()`): a crash between the two renames leaves a
/// half-present pair, which the next start refuses rather than "fixes" by
/// regenerating — see [`load_or_generate`]'s doc comment.
fn generate(
    dir: &Path,
    key_path: &Path,
    cert_path: &Path,
    extra_sans: &[String],
) -> Result<Material, TlsError> {
    fs::create_dir_all(dir).map_err(|e| TlsError(format!("creating {}: {e}", dir.display())))?;

    let key_pair = rcgen::KeyPair::generate()
        .map_err(|e| TlsError(format!("generating an ECDSA P-256 key pair: {e}")))?;

    let mut sans = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    for host in extra_sans {
        let host = host.trim();
        if host.is_empty() || matches!(host, "0.0.0.0" | "::" | "[::]") {
            continue;
        }
        if !sans.iter().any(|s| s == host) {
            sans.push(host.to_string());
        }
    }
    let mut params = rcgen::CertificateParams::new(sans)
        .map_err(|e| TlsError(format!("building certificate parameters: {e}")))?;
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "solador-agent");

    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| TlsError(format!("self-signing the certificate: {e}")))?;
    let cert_der = cert.der().to_vec();
    let key_der = key_pair.serialize_der();

    let key_new = dir.join(format!("{KEY_FILE}.new"));
    let cert_new = dir.join(format!("{CERT_FILE}.new"));
    write_new_file(&key_new, to_pem(KEY_PEM_LABEL, &key_der).as_bytes(), true)?;
    write_new_file(
        &cert_new,
        to_pem(CERT_PEM_LABEL, &cert_der).as_bytes(),
        false,
    )?;
    fs::rename(&key_new, key_path)
        .map_err(|e| TlsError(format!("installing {}: {e}", key_path.display())))?;
    fs::rename(&cert_new, cert_path)
        .map_err(|e| TlsError(format!("installing {}: {e}", cert_path.display())))?;

    Ok(Material { cert_der, key_der })
}

/// Write `bytes` to a fresh `path`, 0600 when `private` (the key), 0644
/// otherwise (the certificate — not secret). Best-effort on a platform with
/// no Unix permission bits; the file still exists and still holds the right
/// bytes there, which is what every other caller in this codebase does for a
/// permission it cannot express (see `dir_writable` in `update.rs`).
fn write_new_file(path: &Path, bytes: &[u8], private: bool) -> Result<(), TlsError> {
    let open = |opts: &mut OpenOptions| {
        opts.write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|e| TlsError(format!("creating {}: {e}", path.display())))
    };
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mode = if private { 0o600 } else { 0o644 };
        open(OpenOptions::new().mode(mode))?
    };
    #[cfg(not(unix))]
    let mut file = {
        let _ = private;
        open(&mut OpenOptions::new())?
    };
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| TlsError(format!("writing {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn flag_enabled_requires_exactly_one() {
        assert!(flag_enabled(Some("1")));
        assert!(flag_enabled(Some("  1  ")));
        assert!(!flag_enabled(Some("true")));
        assert!(!flag_enabled(Some("0")));
        assert!(!flag_enabled(Some("")));
        assert!(!flag_enabled(None));
    }

    #[test]
    fn fingerprint_hex_is_colon_separated_uppercase_sha256() {
        let fp = fingerprint_hex(b"hello world");
        // SHA-256("hello world"), known-answer.
        assert_eq!(
            fp,
            "B9:4D:27:B9:93:4D:3E:08:A5:2E:52:D7:DA:7D:AB:FA:C4:84:EF:E3:\
             7A:53:80:EE:90:88:F7:AC:E2:EF:CD:E9"
        );
        assert_eq!(fp.split(':').count(), 32);
    }

    /// The shared vector (#448): the same certificate and expected string
    /// `crates/agentclient` asserts with ITS SHA-256. `tls-fingerprint`'s
    /// output must stay byte-identical to what it printed before the format
    /// moved into `crates/certpin`, and the string is independently the one
    /// `openssl x509 -noout -fingerprint -sha256` prints for this file.
    #[test]
    fn the_shared_fixture_certificate_fingerprints_to_its_expected_string() {
        const CERT: &[u8] = include_bytes!("../../tests/fixtures/tls/pinned-cert.der");
        const EXPECTED: &str = include_str!("../../tests/fixtures/tls/pinned-cert.sha256");
        assert_eq!(fingerprint_hex(CERT), EXPECTED);
    }

    #[test]
    fn generates_once_then_loads_the_same_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_generate(dir.path(), &[]).unwrap();
        assert!(dir.path().join(KEY_FILE).is_file());
        assert!(dir.path().join(CERT_FILE).is_file());

        let second = load_or_generate(dir.path(), &[]).unwrap();
        assert_eq!(
            first.cert_der, second.cert_der,
            "the certificate must not change"
        );
        assert_eq!(first.key_der, second.key_der, "the key must not change");

        // Different extra_sans on the second call changes nothing either —
        // "never regenerate while a key exists" has no exception for a
        // different bind host being asked for the second time.
        let third = load_or_generate(dir.path(), &["100.64.1.2".to_string()]).unwrap();
        assert_eq!(first.cert_der, third.cert_der);
    }

    #[cfg(unix)]
    #[test]
    fn the_key_file_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        load_or_generate(dir.path(), &[]).unwrap();
        let mode = fs::metadata(dir.path().join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_half_present_pair_is_refused_not_repaired() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(KEY_FILE), b"not really a key").unwrap();
        let err = load_or_generate(dir.path(), &[]).unwrap_err();
        assert!(err.0.contains("key present"), "{}", err.0);
        assert!(err.0.contains("cert absent"), "{}", err.0);
        // And it must not have written a certificate to pair with the
        // leftover key: that would be exactly the silent regeneration this
        // refuses.
        assert!(!dir.path().join(CERT_FILE).exists());
    }

    #[test]
    fn read_cert_never_creates_one() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_cert(dir.path()).unwrap_err();
        assert!(err.0.contains("SOLADOR_AGENT_TLS=1"), "{}", err.0);
        assert!(!dir.path().join(KEY_FILE).exists());
        assert!(!dir.path().join(CERT_FILE).exists());

        let material = load_or_generate(dir.path(), &[]).unwrap();
        let read_back = read_cert(dir.path()).unwrap();
        assert_eq!(material.cert_der, read_back);
    }

    /// End to end: a real TLS handshake against the generated material,
    /// verified the way `install.sh` and `solador-agent update` verify it —
    /// the exact certificate file as the sole trust root, standard hostname
    /// matching (no verification disabled) — and the certificate the
    /// handshake actually presented has the same fingerprint
    /// `tls-fingerprint` would print for this material.
    #[test]
    fn tls_fingerprint_matches_the_certificate_the_handshake_presents() {
        let _ = rustls::crypto::ring::default_provider().install_default();

        let dir = tempfile::tempdir().unwrap();
        let material = load_or_generate(dir.path(), &[]).unwrap();
        let expected_fingerprint = fingerprint_hex(&material.cert_der);

        let cert_der = rustls_pki_types::CertificateDer::from(material.cert_der.clone());
        let key_der = rustls_pki_types::PrivateKeyDer::try_from(material.key_der.clone()).unwrap();
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)
            .unwrap();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut conn = rustls::ServerConnection::new(Arc::new(server_config)).unwrap();
            let mut stream = rustls::Stream::new(&mut conn, &mut sock);
            let mut buf = [0u8; 5];
            std::io::Read::read_exact(&mut stream, &mut buf).unwrap();
            assert_eq!(&buf, b"hello");
        });

        // Trust exactly the pinned certificate — nothing else — and dial it
        // as "127.0.0.1", the IP SAN every generated certificate carries.
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls_pki_types::CertificateDer::from(
                material.cert_der.clone(),
            ))
            .unwrap();
        let client_config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let server_name = rustls_pki_types::ServerName::IpAddress(
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST).into(),
        );
        let mut client_conn =
            rustls::ClientConnection::new(Arc::new(client_config), server_name).unwrap();
        let mut sock = std::net::TcpStream::connect(addr).unwrap();
        {
            let mut stream = rustls::Stream::new(&mut client_conn, &mut sock);
            std::io::Write::write_all(&mut stream, b"hello").unwrap();
        }

        let presented = client_conn
            .peer_certificates()
            .expect("a completed handshake carries the server's certificate chain");
        assert_eq!(presented.len(), 1);
        let handshake_fingerprint = fingerprint_hex(&presented[0]);
        assert_eq!(handshake_fingerprint, expected_fingerprint);
        assert_eq!(presented[0].as_ref(), material.cert_der.as_slice());

        server.join().unwrap();
    }

    /// `extra_sans` actually lands in the certificate: standard hostname
    /// verification (`rustls::client::danger::ServerCertVerifier`'s stock
    /// implementation, via a `RootCertStore` holding only this certificate)
    /// accepts a `ServerName` for the extra host and refuses one that was
    /// never listed. `ServerName` is what the *client* asserts it wants —
    /// rustls checks it against the certificate's SAN list, independent of
    /// which address the TCP connection actually reached — so this proves
    /// the SAN list itself without needing a second bindable loopback
    /// address (`127.0.0.2` and friends are not routable in every
    /// sandbox this runs in).
    #[test]
    fn extra_sans_are_verifiable_hostnames_on_the_certificate() {
        use rustls::client::danger::ServerCertVerifier as _;

        let _ = rustls::crypto::ring::default_provider().install_default();

        let dir = tempfile::tempdir().unwrap();
        let material = load_or_generate(dir.path(), &["100.64.1.2".to_string()]).unwrap();

        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls_pki_types::CertificateDer::from(
                material.cert_der.clone(),
            ))
            .unwrap();
        let verifier = rustls::client::WebPkiServerVerifier::builder(Arc::new(roots))
            .build()
            .unwrap();

        let cert = rustls_pki_types::CertificateDer::from(material.cert_der.clone());
        let now = rustls_pki_types::UnixTime::now();

        let extra: std::net::IpAddr = "100.64.1.2".parse().unwrap();
        let named = rustls_pki_types::ServerName::IpAddress(extra.into());
        assert!(
            verifier
                .verify_server_cert(&cert, &[], &named, &[], now)
                .is_ok(),
            "the extra SAN must verify"
        );

        let baseline = rustls_pki_types::ServerName::IpAddress(
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST).into(),
        );
        assert!(
            verifier
                .verify_server_cert(&cert, &[], &baseline, &[], now)
                .is_ok(),
            "the baseline 127.0.0.1 SAN must still verify"
        );

        let unlisted: std::net::IpAddr = "100.64.9.9".parse().unwrap();
        let unlisted_name = rustls_pki_types::ServerName::IpAddress(unlisted.into());
        assert!(
            verifier
                .verify_server_cert(&cert, &[], &unlisted_name, &[], now)
                .is_err(),
            "a host that was never listed must not verify"
        );
    }

    /// The exact failure the review found: a genuinely DER
    /// `solador-agent.tls.crt` makes curl's `cacert` config option refuse it
    /// (exit 77, "error setting certificate verify locations") before a
    /// request is even sent — which is how `agent/deploy/lib.sh`'s
    /// `verify_health` and `install.sh`'s printed "Verify locally" command
    /// both use it. Runs the REAL `curl` on this machine against a real TLS
    /// handshake, not a stub — skips (rather than fails) when `curl` is not
    /// on `PATH`.
    ///
    /// `cfg(unix)` deliberately: `lib.sh`/`install.sh` — what this guards —
    /// are unix shell scripts with no Windows counterpart, but `cargo test
    /// --workspace` (this repo's Windows CI job included) still compiles and
    /// runs every `#[test]` in this crate regardless. On Windows,
    /// `Command::new("curl")` resolves to the bundled `curl.exe`, which uses
    /// Schannel rather than the OpenSSL/LibreSSL family this test's finding
    /// is about, and Schannel's own `--cacert` handling (revocation
    /// checking, IP-SAN matching) is a different, untested question this
    /// test was never meant to answer.
    #[cfg(unix)]
    #[test]
    fn a_real_curl_cacert_accepts_the_written_certificate() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        if std::process::Command::new("curl")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!(
                "skipping a_real_curl_cacert_accepts_the_written_certificate: no curl on PATH"
            );
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let material = load_or_generate(dir.path(), &[]).unwrap();
        let cert_path = dir.path().join(CERT_FILE);

        // The bug in one assertion: the file `verify_health` points curl's
        // `cacert` at must actually be PEM.
        let on_disk = fs::read_to_string(&cert_path).unwrap();
        assert!(
            on_disk.starts_with("-----BEGIN CERTIFICATE-----"),
            "solador-agent.tls.crt must be PEM, not DER, for curl's cacert to accept it"
        );

        let cert_der = rustls_pki_types::CertificateDer::from(material.cert_der.clone());
        let key_der = rustls_pki_types::PrivateKeyDer::try_from(material.key_der.clone()).unwrap();
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)
            .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut conn = rustls::ServerConnection::new(Arc::new(server_config)).unwrap();
            let mut stream = rustls::Stream::new(&mut conn, &mut sock);
            let mut buf = [0u8; 1024];
            let _ = std::io::Read::read(&mut stream, &mut buf);
            let _ = std::io::Write::write_all(
                &mut stream,
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
            );
        });

        let url = format!("https://127.0.0.1:{}/", addr.port());
        let out = std::process::Command::new("curl")
            .args(["-sS", "--cacert"])
            .arg(&cert_path)
            .arg(&url)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "curl --cacert {} {url} must succeed: {}",
            cert_path.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "ok");

        server.join().unwrap();
    }
}
