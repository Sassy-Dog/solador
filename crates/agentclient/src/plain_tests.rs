//! The plain-HTTP guard (#449 part 3), end to end: a real client, a real
//! listener, and a name that maps to an address the guard must not dial.
//!
//! The listener is local, so the address policy is substituted with one that
//! counts it as off-tailnet — what is under test here is that a refusal means
//! *no connection and no bytes*, and the policy itself is tested exhaustively
//! in `plain::tests`. The **negative control** runs the identical scenario with
//! a policy that permits everything and expects the listener to receive the
//! token: that is what makes the refusal test able to fail.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

use super::*;

const TOKEN: &str = "s3cret-token-449";

/// The scenario's name resolves to the loopback address its listener is on.
fn name_to_loopback() -> plain::Lookup {
    Arc::new(|_name| Box::pin(async { Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]) }))
}

/// A policy that treats every address as off-tailnet, so a loopback listener
/// stands in for a LAN or public one.
fn everything_is_off_tailnet(_: &[IpAddr]) -> bool {
    false
}

/// The guard switched off — the negative control's policy.
fn guard_disabled(_: &[IpAddr]) -> bool {
    true
}

/// Runs one poll of `http://agent.lan:<port>/v1/health` against a local
/// listener, and reports the poll's result plus every byte the listener
/// received (empty if nothing ever connected).
async fn poll_named_host(policy: plain::Policy) -> (Result<wire::Health, AgentError>, Vec<u8>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let received = tokio::spawn(async move {
        // Long enough for a connect that is going to happen to happen; a
        // refusal returns before this, so it costs the passing case nothing
        // but the wait.
        let Ok(Ok((mut conn, _))) =
            tokio::time::timeout(Duration::from_millis(800), listener.accept()).await
        else {
            return Vec::new();
        };
        let mut buf = vec![0u8; 4096];
        let n = tokio::time::timeout(Duration::from_millis(800), conn.read(&mut buf))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(0);
        buf.truncate(n);
        buf
    });
    let client = AgentClient::plain(
        format!("http://agent.lan:{port}"),
        TOKEN,
        name_to_loopback(),
        policy,
    );
    let result = client.health().await;
    (result, received.await.unwrap())
}

#[tokio::test]
async fn a_name_that_resolves_off_tailnet_is_refused_and_nothing_connects() {
    let (result, received) = poll_named_host(everything_is_off_tailnet).await;
    assert!(
        matches!(result, Err(AgentError::PlainHttpRefused)),
        "{result:?}"
    );
    assert!(
        received.is_empty(),
        "the listener received {} bytes: {}",
        received.len(),
        String::from_utf8_lossy(&received)
    );
}

/// The negative control. The same scenario with the guard off reaches the
/// listener with the token in the request — so the test above is red without
/// the guard, not green for want of a listener that would never have been dialled.
#[tokio::test]
async fn negative_control_without_the_guard_the_token_reaches_the_listener() {
    let (_, received) = poll_named_host(guard_disabled).await;
    let text = String::from_utf8_lossy(&received);
    assert!(text.contains(TOKEN), "the listener received: {text:?}");
}

#[tokio::test]
async fn a_name_with_one_off_tailnet_address_among_good_ones_is_refused() {
    // The real policy, a mixed answer, and no listener at all: refusal is
    // decided from the resolution alone.
    let lookup: plain::Lookup = Arc::new(|_| {
        Box::pin(async {
            Ok(vec![
                "100.64.0.9".parse().unwrap(),
                "fd7a:115c:a1e0::9".parse().unwrap(),
                "192.168.1.20".parse().unwrap(),
            ])
        })
    });
    let client = AgentClient::plain(
        "http://mixed.example:7878",
        TOKEN,
        lookup,
        all_plain_http_permitted,
    );
    assert!(matches!(
        client.health().await,
        Err(AgentError::PlainHttpRefused)
    ));
}

#[tokio::test]
async fn a_name_that_does_not_resolve_sends_nothing_and_is_not_a_refusal() {
    let lookup: plain::Lookup = Arc::new(|_| {
        Box::pin(async {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no such host",
            ))
        })
    });
    let client = AgentClient::plain(
        "http://nowhere.example:7878",
        TOKEN,
        lookup,
        all_plain_http_permitted,
    );
    // Unreachable, not PlainHttpRefused: nothing was found to refuse.
    assert!(matches!(
        client.health().await,
        Err(AgentError::Unreachable(_))
    ));
}

#[tokio::test]
async fn an_ip_literal_off_tailnet_is_refused_before_any_connection() {
    // TEST-NET and RFC 1918 addresses: a connection attempt would sit until
    // the client's 5 s timeout, so an immediate answer is the proof.
    for base in [
        "http://192.0.2.1:7878",
        "http://192.168.1.20:7878",
        "http://10.0.0.5:7878",
        "http://8.8.8.8:7878",
        "http://[2001:db8::1]:7878",
        "http://[::ffff:192.168.1.20]:7878",
    ] {
        let client = AgentClient::new(base, TOKEN);
        let started = std::time::Instant::now();
        let result = client.snapshot().await;
        assert!(
            matches!(result, Err(AgentError::PlainHttpRefused)),
            "{base}: {result:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(2), "{base}");
    }
}

#[tokio::test]
async fn loopback_and_tailscale_literals_are_not_refused_by_the_guard() {
    // Nothing listens on these ports/addresses, so what comes back is a network
    // failure — and the point is that it is not the guard's refusal.
    let client = AgentClient::new("http://127.0.0.1:9", TOKEN);
    assert!(matches!(
        client.health().await,
        Err(AgentError::Unreachable(_))
    ));
    let lookup: plain::Lookup =
        Arc::new(|_| Box::pin(async { Ok(vec!["127.0.0.1".parse().unwrap()]) }));
    let client = AgentClient::plain(
        "http://magic.tailnet.example:9",
        TOKEN,
        lookup,
        all_plain_http_permitted,
    );
    assert!(matches!(
        client.health().await,
        Err(AgentError::Unreachable(_))
    ));
}

#[test]
fn the_refusal_reads_as_neither_unreachable_nor_a_pairing_fault() {
    let refused = AgentError::PlainHttpRefused.user_message();
    for other in [
        AgentError::Unreachable(String::new()),
        AgentError::CertificateChanged,
        AgentError::NoTls,
        AgentError::AuthFailed,
    ] {
        assert_ne!(refused, other.user_message());
    }
    assert!(refused.contains("pair"), "{refused}");
}
