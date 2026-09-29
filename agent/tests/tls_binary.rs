//! `SOLADOR_AGENT_TLS=1` against the REAL built binary (#447 review round
//! after cd26af7): everything else in this crate's suite either drives
//! `agent/src/tls.rs`'s functions directly (its own unit tests) or drives
//! `update`/`rollback` against a FAKE service (`tests/update_flow.rs`).
//! Neither ever starts the actual server and dials it, so neither would
//! have caught a build that quietly served plain HTTP under
//! `SOLADOR_AGENT_TLS=1` — proven red by temporarily forcing the server to
//! answer plain HTTP on the TLS port and watching this test fail at its
//! certificate-never-appeared wait (recorded on the PR that added this file,
//! not kept as a permanent branch here). The later plain-`http://` assertion
//! guards a different regression: a server answering both TLS and plain HTTP
//! on the same port.
//!
//! What's real, and what this test actually proves: the compiled
//! `solador-agent` binary, spawned as the actual HTTPS server; that the
//! served certificate is exactly the one `solador-agent tls-fingerprint`
//! reports — a second, independent raw rustls handshake pulls the
//! certificate the server *actually presented* mid-connection and hashes
//! it, compared against the CLI's own stdout, so this is "what's on the
//! wire" rather than a from-disk read standing in for it; that a
//! kill-and-restart of the same binary leaves the key file untouched
//! (byte-identical) and the served certificate unchanged; and that a plain
//! `http://` request against the same HTTPS-only port fails rather than
//! silently falling back to plain HTTP. The key file is also asserted mode
//! 0600.
//!
//! What this does NOT prove: SAN coverage. Every dial here — the health
//! poll and the raw handshake alike — targets `127.0.0.1`, a host
//! `tls.rs`'s baseline always adds to the SAN list independent of the
//! resolved bind (see its own doc comment), so a build that dropped the
//! *bind host* specifically from the SAN list would pass this test
//! unchanged. That gap is not exercised by anything in this file.
//!
//! `SOLADOR_AGENT_CONFIG_DIR` (#447, this round) is deliberately exercised
//! by pointing `HOME` at an EMPTY decoy directory that is never touched:
//! if the binary fell back to deriving the certificate's location from
//! `$HOME` instead of the env var the launcher/unit now export, the
//! certificate would land in the decoy `$HOME/.config` and every assertion
//! below that reads it from `SOLADOR_AGENT_CONFIG_DIR` would find nothing.

#![cfg(unix)]

use std::fs;
use std::io::Write as _;
use std::net::{IpAddr, Ipv4Addr, TcpStream};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use solador_agent::tls;

const TOKEN: &str = "tls-binary-test-token-MUST-NOT-BE-PRINTED";

/// Kills the child on drop, so a panicking assertion never leaves a stray
/// `solador-agent` process running past this test. This is ordinary test
/// hygiene, not a `free_port()` safeguard: each `tests/*.rs` file is its own
/// process, and the OS never hands out a port that is already in use, so a
/// leaked listener here could not make a later `free_port()` call return
/// one that is not actually free. The guard exists so a panic here does not
/// leave an orphaned agent process behind.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Spawn the real binary as the server, with `SOLADOR_AGENT_TLS=1`.
/// `config_dir` is where the certificate/key land (`SOLADOR_AGENT_CONFIG_DIR`);
/// `home_decoy` is a SEPARATE, always-empty directory handed as `HOME` —
/// see this file's own header comment.
fn spawn_agent(real: &Path, home_decoy: &Path, config_dir: &Path, port: u16) -> Server {
    let child = Command::new(real)
        .env_clear()
        .env("HOME", home_decoy)
        .env("SOLADOR_AGENT_CONFIG_DIR", config_dir)
        .env("SOLADOR_AGENT_TOKEN", TOKEN)
        .env("SOLADOR_AGENT_BIND", "127.0.0.1")
        .env("SOLADOR_AGENT_PORT", port.to_string())
        .env("SOLADOR_AGENT_TLS", "1")
        .env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawning the real solador-agent binary");
    Server(child)
}

/// Poll `/v1/health` over HTTPS, pinned to `trusted_cert_der` as its own
/// root — standard chain-and-hostname verification, never
/// `danger_accept_invalid_certs` — until it answers `200`. This is what
/// proves the agent is actually serving TLS with the certificate it wrote,
/// not merely that SOME server answered.
async fn wait_for_https_health(trusted_cert_der: &[u8], port: u16) {
    let cert = reqwest::Certificate::from_der(trusted_cert_der).unwrap();
    let client = reqwest::Client::builder()
        .add_root_certificate(cert)
        .tls_built_in_root_certs(false)
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("https://127.0.0.1:{port}/v1/health");
    let mut last_err = String::new();
    for _ in 0..60 {
        match client.get(&url).bearer_auth(TOKEN).send().await {
            Ok(resp) if resp.status().is_success() => return,
            Ok(resp) => last_err = format!("HTTP {}", resp.status()),
            Err(e) => last_err = e.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("agent never answered {url} pinned over HTTPS: {last_err}");
}

/// A raw rustls handshake against `port`, trusting exactly `trusted_cert_der`
/// as its own root (same as [`wait_for_https_health`]'s pin, but independent
/// of reqwest entirely). Returns the SHA-256 fingerprint of the certificate
/// the server actually PRESENTED mid-handshake — not merely read back off
/// disk — so a comparison against `tls-fingerprint`'s own stdout is a
/// genuine "what's on the wire matches what the tool reports" assertion.
fn handshake_fingerprint(port: u16, trusted_cert_der: &[u8]) -> String {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls_pki_types::CertificateDer::from(
            trusted_cert_der.to_vec(),
        ))
        .unwrap();
    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name =
        rustls_pki_types::ServerName::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST).into());
    let mut conn = rustls::ClientConnection::new(Arc::new(client_config), server_name)
        .expect("building the client connection");
    let mut sock =
        TcpStream::connect(("127.0.0.1", port)).expect("connecting to the agent's TLS port");
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    sock.set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    {
        let mut stream = rustls::Stream::new(&mut conn, &mut sock);
        // Any bytes drive the handshake; the request itself is never read —
        // only the certificate the handshake presents is wanted here.
        let _ = stream.write_all(b"GET /v1/health HTTP/1.0\r\n\r\n");
    }
    let presented = conn
        .peer_certificates()
        .expect("a completed handshake carries the server's certificate chain");
    tls::fingerprint_hex(&presented[0])
}

/// `solador-agent tls-fingerprint`, run against the SAME
/// `SOLADOR_AGENT_CONFIG_DIR`/`HOME` the server above was started with —
/// read-only (see `tls::read_cert`'s own doc comment), so it cannot race
/// the server over which certificate exists.
fn cli_fingerprint(real: &Path, home_decoy: &Path, config_dir: &Path) -> String {
    let out = Command::new(real)
        .arg("tls-fingerprint")
        .env_clear()
        .env("HOME", home_decoy)
        .env("SOLADOR_AGENT_CONFIG_DIR", config_dir)
        .output()
        .expect("running solador-agent tls-fingerprint");
    assert!(
        out.status.success(),
        "tls-fingerprint failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("tls-fingerprint prints UTF-8")
        .trim()
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn solador_agent_tls_serves_and_survives_a_restart() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let real = PathBuf::from(env!("CARGO_BIN_EXE_solador-agent"));
    let home_decoy = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let key_path = config_dir.path().join(tls::KEY_FILE);
    let cert_path = config_dir.path().join(tls::CERT_FILE);

    // ---- first start: the certificate lands in SOLADOR_AGENT_CONFIG_DIR,
    // ---- never the decoy HOME --------------------------------------------
    let port_a = free_port();
    let server = spawn_agent(&real, home_decoy.path(), config_dir.path(), port_a);
    // Wait for the certificate to exist (the server generates it on its
    // first start, after reading settings and spawning the sampler — the
    // same startup race agent/deploy/lib.sh's verify_health accounts for).
    let mut waited = 0;
    while !cert_path.exists() {
        std::thread::sleep(Duration::from_millis(100));
        waited += 1;
        assert!(
            waited < 100,
            "the certificate never appeared at {}",
            cert_path.display()
        );
    }
    assert!(
        !home_decoy.path().join(".config").exists(),
        "the certificate must come from SOLADOR_AGENT_CONFIG_DIR, not a $HOME fallback \
         the launcher/unit both now override"
    );

    let cert_der = tls::read_cert(config_dir.path()).unwrap();

    // 1. wait for /v1/health over HTTPS, pinned to the generated cert.
    wait_for_https_health(&cert_der, port_a).await;

    // 2. the handshake certificate's SHA-256 equals `tls-fingerprint`'s own
    //    stdout, run against the same config.
    let handshake_fp = handshake_fingerprint(port_a, &cert_der);
    let cli_fp = cli_fingerprint(&real, home_decoy.path(), config_dir.path());
    assert_eq!(
        handshake_fp, cli_fp,
        "the certificate on the wire must be exactly the one tls-fingerprint reports"
    );
    // Sanity: also agrees with a from-disk hash (never generated by the
    // fingerprint command itself, per tls::read_cert's contract).
    assert_eq!(handshake_fp, tls::fingerprint_hex(&cert_der));

    // 5. the key file is 0600.
    let mode = fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "{} must be mode 0600", key_path.display());

    let key_bytes_before = fs::read(&key_path).unwrap();

    // 4. a plain http:// request to the same port fails — the agent must
    //    not silently accept plain HTTP on an HTTPS-only listener.
    let plain = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let plain_result = plain
        .get(format!("http://127.0.0.1:{port_a}/v1/health"))
        .send()
        .await;
    assert!(
        plain_result.is_err(),
        "a plain http:// request to an HTTPS-only port must fail, got: {plain_result:?}"
    );

    // ---- 3. kill and restart: same key (byte-identical), same fingerprint ----
    drop(server); // Drop kills and waits for the old process.
    let port_b = free_port(); // A fresh port avoids any TIME_WAIT flakiness.
    let server2 = spawn_agent(&real, home_decoy.path(), config_dir.path(), port_b);
    let cert_der_after = tls::read_cert(config_dir.path()).unwrap();
    wait_for_https_health(&cert_der_after, port_b).await;

    let key_bytes_after = fs::read(&key_path).unwrap();
    assert_eq!(
        key_bytes_before, key_bytes_after,
        "the key file must be byte-identical across a restart — never regenerated"
    );
    assert_eq!(
        cert_der, cert_der_after,
        "the certificate must not change across a restart either"
    );

    let handshake_fp_after = handshake_fingerprint(port_b, &cert_der_after);
    let cli_fp_after = cli_fingerprint(&real, home_decoy.path(), config_dir.path());
    assert_eq!(
        handshake_fp, handshake_fp_after,
        "the fingerprint must survive a restart"
    );
    assert_eq!(
        cli_fp, cli_fp_after,
        "tls-fingerprint must report the same value after a restart"
    );

    drop(server2);
}
