//! `fetch_latest` against a loopback server: the transport half of the
//! classification (`src/lib.rs` tests the verify half against the same signed
//! fixtures). Never the network.

use agentrelease::{fetch_latest, Error, Trust};
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/agent-v");
const FEED_PATH: &str = "/releases/download/agent-latest/agent-latest.json";
const SIG_PATH: &str = "/releases/download/agent-latest/agent-latest.json.minisig";

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURES}/{name}")).expect("fixture")
}

fn trust() -> Trust {
    let key = String::from_utf8(fixture("test-agent-key.pub")).unwrap();
    Trust::from_texts(&[&key]).unwrap()
}

/// Serves `routes` (path -> status, body); any other path is a 404. Returns
/// the base URL.
async fn serve(routes: Vec<(&'static str, u16, Vec<u8>)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let routes: HashMap<&'static str, (u16, Vec<u8>)> = routes
        .into_iter()
        .map(|(path, status, body)| (path, (status, body)))
        .collect();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
            let (status, body) = routes
                .get(path.as_str())
                .cloned()
                .unwrap_or((404, b"not found".to_vec()));
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(&body).await;
            let _ = socket.shutdown().await;
        }
    });
    base
}

fn signed_routes() -> Vec<(&'static str, u16, Vec<u8>)> {
    vec![
        (FEED_PATH, 200, fixture("agent-latest.json")),
        (SIG_PATH, 200, fixture("agent-latest.json.minisig")),
    ]
}

#[tokio::test]
async fn a_signed_feed_over_the_wire_verifies() {
    let base = serve(signed_routes()).await;
    let latest = fetch_latest(&base, &trust()).await.expect("verifies");
    assert_eq!(latest.version, "2026.9.9");
}

#[tokio::test]
async fn a_404_on_the_feed_is_not_published_yet() {
    let base = serve(vec![]).await;
    assert_eq!(
        fetch_latest(&base, &trust()).await,
        Err(Error::NotPublished)
    );
}

#[tokio::test]
async fn a_network_failure_is_unreachable_and_not_the_same_state() {
    // Bind then drop: the port is closed, so the connect is refused.
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let down = fetch_latest(&format!("http://127.0.0.1:{port}"), &trust()).await;
    assert_eq!(down, Err(Error::Unreachable));
    assert_ne!(down, Err(Error::NotPublished));
}

#[tokio::test]
async fn a_server_failure_keeps_its_status() {
    let base = serve(vec![(FEED_PATH, 503, b"down".to_vec())]).await;
    assert_eq!(fetch_latest(&base, &trust()).await, Err(Error::Status(503)));
}

#[tokio::test]
async fn a_feed_without_its_signature_is_unsigned_not_unpublished() {
    let base = serve(vec![(FEED_PATH, 200, fixture("agent-latest.json"))]).await;
    assert_eq!(fetch_latest(&base, &trust()).await, Err(Error::Unsigned));
}

#[tokio::test]
async fn a_tampered_feed_over_the_wire_is_refused() {
    let mut feed = fixture("agent-latest.json");
    let at = feed.windows(8).position(|w| w == b"2026.9.9").unwrap();
    feed[at + 7] = b'8';
    let base = serve(vec![
        (FEED_PATH, 200, feed),
        (SIG_PATH, 200, fixture("agent-latest.json.minisig")),
    ])
    .await;
    assert_eq!(
        fetch_latest(&base, &trust()).await,
        Err(Error::SignatureRejected)
    );
}

#[tokio::test]
async fn a_foreign_key_over_the_wire_is_refused() {
    let base = serve(signed_routes()).await;
    // The compiled-in set is the real agent key; the fixture is a throwaway's.
    let foreign = Trust::compiled_in().unwrap();
    assert_eq!(
        fetch_latest(&base, &foreign).await,
        Err(Error::SignatureRejected)
    );
}

#[tokio::test]
async fn plain_http_is_refused_off_loopback() {
    // A non-loopback http base is refused before any socket is opened:
    // `https_only` is on. 192.0.2.1 never answers, so without the guard this
    // would sit out the 10s connect timeout and still say `Unreachable`; the
    // elapsed bound is what tells the two apart.
    let started = std::time::Instant::now();
    assert_eq!(
        fetch_latest("http://192.0.2.1:9", &trust()).await,
        Err(Error::Unreachable)
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "refused promptly, not after a connect timeout: {:?}",
        started.elapsed()
    );
}
