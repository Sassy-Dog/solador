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

// --- What the unpaired client does NOT do (#449) -----------------------------
//
// `AgentClient::plain` sets `.no_proxy()` and `.redirect(Policy::none())`. Both
// exist because each would hand the token to a destination the guard never
// vetted, and neither is visible from any other test: a client without them
// passes every test above. Each test below has a control — delete the line it
// pins and it goes red (recorded on the PR that added them).

/// Bytes a listener received within `wait`, or `None` if nothing connected.
async fn accepted_bytes(listener: TcpListener, wait: Duration) -> Option<Vec<u8>> {
    let (mut conn, _) = tokio::time::timeout(wait, listener.accept())
        .await
        .ok()?
        .ok()?;
    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_millis(800), conn.read(&mut buf))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(0);
    buf.truncate(n);
    Some(buf)
}

/// A `302` from an allowed listener to a second listener must not reach the
/// second: no connection at all, so no token (`reqwest` would strip
/// `Authorization` on a cross-origin hop, but the destination was still
/// contacted, which is a request nobody vetted).
#[tokio::test]
async fn a_redirect_is_not_followed_and_the_second_listener_is_never_contacted() {
    use tokio::io::AsyncWriteExt;

    let allowed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let allowed_port = allowed.local_addr().unwrap().port();
    let elsewhere = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let elsewhere_port = elsewhere.local_addr().unwrap().port();

    let redirector = tokio::spawn(async move {
        let (mut conn, _) = allowed.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = conn.read(&mut buf).await;
        let reply = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{elsewhere_port}/v1/health\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
        );
        conn.write_all(reply.as_bytes()).await.unwrap();
    });
    let second = tokio::spawn(accepted_bytes(elsewhere, Duration::from_millis(1500)));

    // Everything permitted: the FIRST listener is an allowed destination; the
    // redirect target is what must not be followed.
    let client = AgentClient::plain(
        format!("http://127.0.0.1:{allowed_port}"),
        TOKEN,
        name_to_loopback(),
        guard_disabled,
    );
    let _ = client.health().await;
    redirector.await.unwrap();

    let got = second.await.unwrap();
    assert!(
        got.is_none(),
        "the redirect target was contacted and received: {:?}",
        got.map(|b| String::from_utf8_lossy(&b).into_owned())
    );
}

/// Set only in the child process of the proxy test below.
const PROXY_CHILD: &str = "SOLADOR_PLAIN_PROXY_CHILD";

/// The child half of [`the_environment_proxy_is_never_used`]: runs with
/// `HTTP_PROXY`/`http_proxy` pointing at the parent's listener, and reports by
/// exit status whether its own target received the request directly. A no-op
/// unless [`PROXY_CHILD`] is set. It is a separate process because a proxy
/// variable is process-global, and reqwest reads it when a client is built:
/// setting it in-process would race every other test that builds a client.
#[tokio::test]
async fn proxy_test_child() {
    if std::env::var_os(PROXY_CHILD).is_none() {
        return;
    }
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = target.local_addr().unwrap().port();
    let client = AgentClient::plain(
        format!("http://127.0.0.1:{port}"),
        TOKEN,
        name_to_loopback(),
        guard_disabled,
    );
    let poll = tokio::spawn(async move { client.health().await });
    let got = accepted_bytes(target, Duration::from_millis(2500)).await;
    poll.abort();
    let text = got.map(|b| String::from_utf8_lossy(&b).into_owned());
    assert!(
        text.as_deref().is_some_and(|t| t.contains(TOKEN)),
        "the request did not go straight to its target: {text:?}"
    );
}

/// With `HTTP_PROXY` and `http_proxy` set, the proxy's listener receives
/// nothing: a proxy would be handed the token and would resolve the name where
/// the guard cannot see it.
#[test]
fn the_environment_proxy_is_never_used() {
    if std::env::var_os(PROXY_CHILD).is_some() {
        return; // we are the child; the child test does the work.
    }
    let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    proxy.set_nonblocking(true).unwrap();
    let proxy_url = format!("http://127.0.0.1:{}", proxy.local_addr().unwrap().port());
    // `module_path!()` is `agentclient::plain_tests`; libtest names are
    // relative to the crate.
    let name = format!(
        "{}::proxy_test_child",
        module_path!().split_once("::").unwrap().1
    );
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &name, "--nocapture", "--test-threads=1"])
        .env(PROXY_CHILD, "1")
        .env("HTTP_PROXY", &proxy_url)
        .env("http_proxy", &proxy_url)
        .env("ALL_PROXY", &proxy_url)
        .env("all_proxy", &proxy_url)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "child failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // The child ran the real test (not the no-op path).
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "the child did not run its test:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    match proxy.accept() {
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        other => panic!("the proxy listener was contacted: {other:?}"),
    }
}
