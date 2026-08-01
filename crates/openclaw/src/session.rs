//! One gateway session: handshake, bootstrap, then the steady-state pump.
//!
//! Rust port of `DevCanopy/Services/OpenClaw/OpenClawWebSocketClient.swift`
//! (periclaw's `net/openclaw.rs` session): await `connect.challenge` → sign the
//! nonce → send `connect` → await `hello-ok` → bootstrap RPCs → receive loop,
//! with a 30s channel/session re-snapshot and a 20s ping keepalive.
//!
//! The socket sits behind [`Transport`]. That seam is the point: every rule
//! below — which frames the handshake ignores, what counts as a rejection
//! versus a pairing request, what the deadlines are, what the heartbeat sends —
//! is exercised in this file's tests by feeding scripted frames in and reading
//! sent frames out, with no listener, no TLS and no port. The real socket is
//! one small implementation ([`crate::ws`], behind the `ws` feature).
//!
//! This is a *session*, not a service: it runs once and ends by returning an
//! error. Reconnect pacing is [`crate::backoff`]; owning the loop, the
//! published snapshot and the clock is app wiring, deliberately not here.

use std::fmt;
use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;

use crate::domain::PairingState;
use crate::identity::DeviceIdentity;
use crate::protocol;
use crate::rpc::Envelope;

/// How long the client waits for `connect.challenge`, and again for
/// `hello-ok`. Matches the Swift client's two 10s deadlines.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Channels aren't broadcast and session usage drifts, so both are
/// re-snapshotted this often over the live socket.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
/// Application-level ping keepalive, to survive NAT idle timeouts.
pub const PING_INTERVAL: Duration = Duration::from_secs(20);

/// One inbound WS message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Text(String),
    Binary(Vec<u8>),
    /// A control frame (pong, close, …) — carries nothing this layer reads.
    Other,
}

/// What a session hands back to its owner, in arrival order.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// Fired once, immediately after `hello-ok`.
    Connected,
    /// Every steady-state frame, post-handshake.
    Frame(Envelope),
}

/// A transport failure, spelled by whatever implements [`Transport`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("connection closed")]
    Closed,
    #[error("transport failed: {0}")]
    Io(String),
}

/// The bidirectional byte pipe a session runs over.
///
/// Returns `impl Future + Send` rather than using `async fn` so the futures are
/// explicitly `Send` — a session is normally driven from a spawned task, and an
/// accidentally-`!Send` future turns that into a compile error at the far end
/// of the crate instead of here.
pub trait Transport {
    /// Send one text frame.
    fn send_text(
        &mut self,
        text: String,
    ) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Await the next inbound frame.
    ///
    /// Must be cancel-safe: the steady-state pump races this against the
    /// heartbeat and ping timers and drops the loser, so an implementation that
    /// consumes bytes into a future it then discards would lose frames.
    fn recv(&mut self) -> impl Future<Output = Result<Frame, TransportError>> + Send;

    /// Send a WS ping.
    fn send_ping(&mut self) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Close the connection. Best-effort; a failure here has nowhere useful to
    /// go, because the session is already over.
    fn close(&mut self) -> impl Future<Output = ()> + Send;
}

/// Everything that ends a session.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    #[error("invalid gateway URL")]
    InvalidUrl,
    #[error("handshake timed out")]
    HandshakeTimeout,
    #[error("gateway rejected: {0}")]
    HandshakeRejected(String),
    /// A human must approve this device out-of-band before any connect can
    /// succeed. Carries what the operator needs to do it.
    #[error("device pairing required")]
    PairingRequired(PairingState),
    #[error("connection closed")]
    SocketClosed,
    #[error(transparent)]
    Transport(#[from] TransportError),
}

impl From<protocol::InvalidUrl> for SessionError {
    fn from(_: protocol::InvalidUrl) -> Self {
        SessionError::InvalidUrl
    }
}

impl SessionError {
    /// The one-line reason for the panel's connection state.
    ///
    /// Ports `OpenClawService.humanize` plus the two pairing strings the Swift
    /// reconnect loop sets directly. Never includes a token, a nonce, or a
    /// signature — the gateway's own `error.message` is the only free text, and
    /// the gateway does not echo credentials.
    #[must_use]
    pub fn disconnect_reason(&self) -> String {
        match self {
            SessionError::InvalidUrl => "invalid gateway URL".to_owned(),
            SessionError::HandshakeTimeout => "handshake timed out".to_owned(),
            SessionError::HandshakeRejected(message) => format!("gateway rejected: {message}"),
            SessionError::PairingRequired(pairing) => match pairing.kind {
                crate::domain::PairingKind::ScopeUpgrade => "awaiting scope approval".to_owned(),
                crate::domain::PairingKind::FirstPair => "awaiting device pairing".to_owned(),
            },
            SessionError::SocketClosed => "connection closed".to_owned(),
            SessionError::Transport(error) => error.to_string(),
        }
    }
}

/// Milliseconds since the UNIX epoch. A clock behind the epoch yields `0`
/// rather than panicking — the gateway will reject the stale signature, which
/// is a far better outcome than taking the process down.
#[must_use]
pub fn system_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// A single gateway session over `T`.
pub struct Session<T> {
    transport: T,
    identity: DeviceIdentity,
    token: Option<String>,
    app_version: String,
    /// Injected so `signedAt` — which is folded into the signed payload and
    /// echoed in the `device` block — is deterministic under test. A plain
    /// function pointer, so `Session` stays `Send + Sync` without boxing.
    now_ms: fn() -> i64,
}

/// Hand-written because `token` is the gateway bearer credential. A derived
/// `Debug` would print it in any log line or panic message that rendered a
/// session; the identity's own `Debug` is already seed-free.
impl<T> fmt::Debug for Session<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("identity", &self.identity)
            .field("app_version", &self.app_version)
            .field("authenticated", &self.token.is_some())
            .finish_non_exhaustive()
    }
}

impl<T: Transport> Session<T> {
    /// A session that stamps `signedAt` from the system clock.
    #[must_use]
    pub fn new(
        transport: T,
        identity: DeviceIdentity,
        token: Option<String>,
        app_version: impl Into<String>,
    ) -> Self {
        Session {
            transport,
            identity,
            token,
            app_version: app_version.into(),
            now_ms: system_now_ms,
        }
    }

    /// A session with a caller-supplied clock, for deterministic tests.
    #[must_use]
    pub fn with_clock(mut self, now_ms: fn() -> i64) -> Self {
        self.now_ms = now_ms;
        self
    }

    /// The device identity this session authenticates as.
    #[must_use]
    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    /// Run the session to completion. Returns **only** by erroring: a socket
    /// close, a handshake failure, a pairing requirement, or a transport fault.
    ///
    /// `sink` receives [`SessionEvent::Connected`] once after `hello-ok`, then
    /// every steady-state frame in arrival order.
    ///
    /// # Errors
    /// See [`SessionError`]. Callers map the result through
    /// [`SessionError::disconnect_reason`] and [`crate::backoff`].
    pub async fn run<S>(&mut self, mut sink: S) -> Result<(), SessionError>
    where
        S: FnMut(SessionEvent),
    {
        let nonce = self.await_challenge().await?;
        self.send_connect(&nonce).await?;
        self.await_hello_ok().await?;
        sink(SessionEvent::Connected);
        self.send_bootstrap().await?;
        self.pump(&mut sink).await
    }

    /// Best-effort close, for a caller unwinding a failed session.
    pub async fn close(&mut self) {
        self.transport.close().await;
    }

    // MARK: - Handshake

    async fn await_challenge(&mut self) -> Result<String, SessionError> {
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            let Some(env) = self.recv_envelope_until(deadline).await? else {
                continue;
            };
            if env.kind.as_deref() == Some("event")
                && env.event.as_deref() == Some(protocol::CHALLENGE_EVENT)
            {
                if let Some(nonce) = env.payload_str("nonce").filter(|n| !n.is_empty()) {
                    return Ok(nonce.to_owned());
                }
            }
            // Any other pre-connect frame is ignored, not fatal: gateways send
            // banner/health frames before the challenge and the set grows.
        }
    }

    async fn send_connect(&mut self, nonce: &str) -> Result<(), SessionError> {
        let frame = protocol::connect_frame(
            nonce,
            &self.identity,
            self.token.as_deref(),
            (self.now_ms)(),
            &self.app_version,
        );
        self.send_json(&frame).await
    }

    async fn await_hello_ok(&mut self) -> Result<(), SessionError> {
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            let Some(env) = self.recv_envelope_until(deadline).await? else {
                continue;
            };
            if env.kind.as_deref() != Some("res") || env.id.as_deref() != Some(protocol::CONNECT_ID)
            {
                continue;
            }
            if env.ok == Some(true) && env.payload_str("type") == Some(protocol::HELLO_OK) {
                return Ok(());
            }
            // Distinguish a pairing request from a generic rejection: one waits
            // on a human, the other should retry soon.
            if let Some(pairing) = protocol::classify_pairing(&env, self.identity.device_id()) {
                return Err(SessionError::PairingRequired(pairing));
            }
            let message = env
                .error
                .as_ref()
                .and_then(|error| error.message.clone().or_else(|| error.code.clone()))
                .unwrap_or_else(|| "handshake rejected".to_owned());
            return Err(SessionError::HandshakeRejected(message));
        }
    }

    // MARK: - Bootstrap + steady state

    async fn send_bootstrap(&mut self) -> Result<(), SessionError> {
        for method in protocol::BOOTSTRAP_METHODS {
            self.send_json(&protocol::rpc_frame(method)).await?;
        }
        Ok(())
    }

    async fn pump<S>(&mut self, sink: &mut S) -> Result<(), SessionError>
    where
        S: FnMut(SessionEvent),
    {
        // `interval_at` rather than `interval` so neither timer fires
        // immediately: `interval`'s first tick completes at once, which would
        // duplicate the bootstrap we just sent and ping before the socket has
        // been idle for a moment.
        let mut heartbeat =
            tokio::time::interval_at(Instant::now() + HEARTBEAT_INTERVAL, HEARTBEAT_INTERVAL);
        let mut ping = tokio::time::interval_at(Instant::now() + PING_INTERVAL, PING_INTERVAL);

        loop {
            let tick = tokio::select! {
                received = self.transport.recv() => Tick::Frame(received?),
                _ = heartbeat.tick() => Tick::Heartbeat,
                _ = ping.tick() => Tick::Ping,
            };
            match tick {
                Tick::Frame(frame) => {
                    if let Some(env) = envelope_from(&frame) {
                        sink(SessionEvent::Frame(env));
                    }
                }
                Tick::Heartbeat => {
                    for method in protocol::HEARTBEAT_METHODS {
                        self.send_json(&protocol::rpc_frame(method)).await?;
                    }
                }
                Tick::Ping => self.transport.send_ping().await?,
            }
        }
    }

    // MARK: - Frame primitives

    async fn send_json(&mut self, value: &serde_json::Value) -> Result<(), SessionError> {
        // `Value` always serializes, so this cannot realistically fail; treat
        // the impossible case as a dead socket rather than panicking.
        let text = serde_json::to_string(value).map_err(|_| SessionError::SocketClosed)?;
        self.transport.send_text(text).await?;
        Ok(())
    }

    /// Receive the next frame, racing it against `deadline`.
    ///
    /// `Ok(None)` means the frame was not a decodable envelope (a control
    /// frame, non-UTF-8 bytes, malformed JSON) — skip it, don't fail the
    /// session.
    async fn recv_envelope_until(
        &mut self,
        deadline: Instant,
    ) -> Result<Option<Envelope>, SessionError> {
        let frame = tokio::time::timeout_at(deadline, self.transport.recv())
            .await
            .map_err(|_| SessionError::HandshakeTimeout)??;
        Ok(envelope_from(&frame))
    }
}

/// Which of the three racing sources won one pass of the pump. Named so the
/// `select!` arms hand back a value instead of holding a borrow of the
/// transport across the work each one implies.
enum Tick {
    Frame(Frame),
    Heartbeat,
    Ping,
}

/// Decode a frame into an envelope, or `None` if it isn't one.
fn envelope_from(frame: &Frame) -> Option<Envelope> {
    match frame {
        Frame::Text(text) => Envelope::parse(text),
        Frame::Binary(bytes) => Envelope::parse(std::str::from_utf8(bytes).ok()?),
        Frame::Other => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use serde_json::Value;

    use super::*;
    use crate::domain::PairingKind;

    /// A scripted [`Transport`]: inbound frames are queued up front, outbound
    /// ones are recorded. Once the script runs dry, `recv` blocks forever, so
    /// a test that expects a timeout gets one and a test that expects progress
    /// hangs visibly rather than passing by accident.
    #[derive(Debug, Default)]
    struct MockTransport {
        inbound: VecDeque<Frame>,
        /// When true, a drained script closes the socket instead of blocking.
        close_when_drained: bool,
        sent: Vec<String>,
        pings: usize,
        closed: bool,
    }

    impl MockTransport {
        fn new(inbound: impl IntoIterator<Item = Frame>) -> Self {
            MockTransport {
                inbound: inbound.into_iter().collect(),
                ..MockTransport::default()
            }
        }

        fn closing(inbound: impl IntoIterator<Item = Frame>) -> Self {
            MockTransport {
                inbound: inbound.into_iter().collect(),
                close_when_drained: true,
                ..MockTransport::default()
            }
        }

        fn sent_json(&self) -> Vec<Value> {
            self.sent
                .iter()
                .map(|text| serde_json::from_str(text).expect("sent frames are JSON"))
                .collect()
        }
    }

    impl Transport for MockTransport {
        fn send_text(
            &mut self,
            text: String,
        ) -> impl Future<Output = Result<(), TransportError>> + Send {
            self.sent.push(text);
            std::future::ready(Ok(()))
        }

        fn recv(&mut self) -> impl Future<Output = Result<Frame, TransportError>> + Send {
            let next = self.inbound.pop_front();
            let close = self.close_when_drained;
            async move {
                match next {
                    Some(frame) => Ok(frame),
                    None if close => Err(TransportError::Closed),
                    // Never resolves: the deadline is what must end the wait.
                    None => std::future::pending().await,
                }
            }
        }

        fn send_ping(&mut self) -> impl Future<Output = Result<(), TransportError>> + Send {
            self.pings += 1;
            std::future::ready(Ok(()))
        }

        fn close(&mut self) -> impl Future<Output = ()> + Send {
            self.closed = true;
            std::future::ready(())
        }
    }

    fn text(json: &str) -> Frame {
        Frame::Text(json.to_owned())
    }

    fn challenge(nonce: &str) -> Frame {
        text(&format!(
            r#"{{"type":"event","event":"connect.challenge","payload":{{"nonce":"{nonce}"}}}}"#
        ))
    }

    fn hello_ok() -> Frame {
        text(r#"{"type":"res","id":"connect-1","ok":true,"payload":{"type":"hello-ok"}}"#)
    }

    fn identity() -> DeviceIdentity {
        DeviceIdentity::from_seed(&[9u8; 32])
    }

    const FIXED_NOW_MS: i64 = 5;
    fn fixed_clock() -> i64 {
        FIXED_NOW_MS
    }

    fn session(transport: MockTransport) -> Session<MockTransport> {
        Session::new(transport, identity(), Some("tok".to_owned()), "1.2.3").with_clock(fixed_clock)
    }

    /// Drive a session to its (always erroring) end, collecting the events.
    async fn run(
        mut session: Session<MockTransport>,
    ) -> (Vec<SessionEvent>, SessionError, MockTransport) {
        let mut events = Vec::new();
        let error = session
            .run(|event| events.push(event))
            .await
            .expect_err("a session only ends by erroring");
        (events, error, session.transport)
    }

    #[test]
    fn constants_match_the_swift_client() {
        assert_eq!(HANDSHAKE_TIMEOUT, Duration::from_secs(10));
        assert_eq!(HEARTBEAT_INTERVAL, Duration::from_secs(30));
        assert_eq!(PING_INTERVAL, Duration::from_secs(20));
    }

    // MARK: - Handshake

    #[tokio::test(start_paused = true)]
    async fn happy_path_signs_the_challenge_and_bootstraps() {
        let (events, error, transport) = run(session(MockTransport::closing([
            challenge("n"),
            hello_ok(),
        ])))
        .await;

        assert!(matches!(events.first(), Some(SessionEvent::Connected)));
        assert_eq!(error, SessionError::Transport(TransportError::Closed));

        let sent = transport.sent_json();
        assert_eq!(sent.len(), 1 + protocol::BOOTSTRAP_METHODS.len());

        // The connect frame is byte-for-byte the one `protocol` builds for this
        // nonce and this injected clock — including the signature.
        let expected =
            protocol::connect_frame("n", &identity(), Some("tok"), FIXED_NOW_MS, "1.2.3");
        assert_eq!(sent[0], expected);

        let bootstrap: Vec<&str> = sent[1..]
            .iter()
            .map(|frame| frame["method"].as_str().expect("method"))
            .collect();
        assert_eq!(bootstrap, protocol::BOOTSTRAP_METHODS);
        assert!(
            sent[1..].iter().all(|f| f["id"] == f["method"]),
            "responses route by id, so id must equal method"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn pre_challenge_frames_are_ignored() {
        let (events, _, transport) = run(session(MockTransport::closing([
            text(r#"{"type":"event","event":"health"}"#),
            Frame::Other,
            Frame::Binary(b"\xff\xfe not utf8".to_vec()),
            text("not json at all"),
            // A challenge with an empty nonce is not a challenge.
            text(r#"{"type":"event","event":"connect.challenge","payload":{"nonce":""}}"#),
            challenge("real-nonce"),
            hello_ok(),
        ])))
        .await;

        assert!(matches!(events.first(), Some(SessionEvent::Connected)));
        let sent = transport.sent_json();
        assert_eq!(sent[0]["params"]["device"]["nonce"], "real-nonce");
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_gateway_times_out_the_challenge() {
        let (events, error, transport) = run(session(MockTransport::new([]))).await;
        assert_eq!(error, SessionError::HandshakeTimeout);
        assert!(events.is_empty());
        assert!(
            transport.sent.is_empty(),
            "nothing is signed before a nonce"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_gateway_that_never_answers_connect_times_out() {
        let (events, error, transport) = run(session(MockTransport::new([challenge("n")]))).await;
        assert_eq!(error, SessionError::HandshakeTimeout);
        assert!(events.is_empty());
        assert_eq!(transport.sent.len(), 1, "connect was sent, nothing after");
    }

    #[tokio::test(start_paused = true)]
    async fn the_challenge_deadline_is_ten_seconds() {
        // Pinned by observing the clock the runtime auto-advanced to, which is
        // only possible because time is paused.
        let start = Instant::now();
        let (_, error, _) = run(session(MockTransport::new([]))).await;
        assert_eq!(error, SessionError::HandshakeTimeout);
        assert_eq!(start.elapsed(), HANDSHAKE_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn the_hello_ok_deadline_is_a_fresh_ten_seconds() {
        let start = Instant::now();
        let (_, error, _) = run(session(MockTransport::new([challenge("n")]))).await;
        assert_eq!(error, SessionError::HandshakeTimeout);
        assert_eq!(
            start.elapsed(),
            HANDSHAKE_TIMEOUT,
            "the second deadline starts when the challenge arrives, not at connect"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn responses_for_another_id_do_not_satisfy_hello_ok() {
        let (_, error, _) = run(session(MockTransport::new([
            challenge("n"),
            // Right shape, wrong id — the gateway echoes `connect-1` and only
            // that frame is the handshake answer.
            text(r#"{"type":"res","id":"cron.list","ok":true,"payload":{"type":"hello-ok"}}"#),
        ])))
        .await;
        assert_eq!(error, SessionError::HandshakeTimeout);
    }

    #[tokio::test(start_paused = true)]
    async fn a_pairing_rejection_surfaces_the_request_id() {
        let (events, error, _) = run(session(MockTransport::new([
            challenge("n"),
            text(
                r#"{"type":"res","id":"connect-1","ok":false,"error":{"details":
                 {"code":"PAIRING_REQUIRED","requestId":"req-7"}}}"#,
            ),
        ])))
        .await;
        assert!(events.is_empty(), "hello-ok never happened");
        let SessionError::PairingRequired(pairing) = &error else {
            panic!("expected pairing, got {error:?}");
        };
        assert_eq!(pairing.request_id.as_deref(), Some("req-7"));
        assert_eq!(pairing.kind, PairingKind::FirstPair);
        assert_eq!(
            pairing.device_id,
            identity().device_id(),
            "no deviceId in the payload -> our own fingerprint"
        );
        assert_eq!(error.disconnect_reason(), "awaiting device pairing");
    }

    #[tokio::test(start_paused = true)]
    async fn a_scope_upgrade_rejection_reads_differently() {
        let (_, error, _) = run(session(MockTransport::new([
            challenge("n"),
            text(
                r#"{"type":"res","id":"connect-1","ok":false,"error":{"details":
                 {"code":"PAIRING_REQUIRED","reason":"scope-upgrade"}}}"#,
            ),
        ])))
        .await;
        assert_eq!(error.disconnect_reason(), "awaiting scope approval");
    }

    #[tokio::test(start_paused = true)]
    async fn a_generic_rejection_carries_the_gateway_message() {
        let (_, error, _) = run(session(MockTransport::new([
            challenge("n"),
            text(
                r#"{"type":"res","id":"connect-1","ok":false,"error":
                 {"code":"AUTH","message":"bad token"}}"#,
            ),
        ])))
        .await;
        assert_eq!(
            error,
            SessionError::HandshakeRejected("bad token".to_owned())
        );
        assert_eq!(error.disconnect_reason(), "gateway rejected: bad token");
    }

    #[tokio::test(start_paused = true)]
    async fn a_rejection_without_a_message_falls_back_to_the_code_then_a_default() {
        let (_, error, _) = run(session(MockTransport::new([
            challenge("n"),
            text(r#"{"type":"res","id":"connect-1","ok":false,"error":{"code":"AUTH"}}"#),
        ])))
        .await;
        assert_eq!(error, SessionError::HandshakeRejected("AUTH".to_owned()));

        let (_, error, _) = run(session(MockTransport::new([
            challenge("n"),
            text(r#"{"type":"res","id":"connect-1","ok":false}"#),
        ])))
        .await;
        assert_eq!(
            error,
            SessionError::HandshakeRejected("handshake rejected".to_owned())
        );
    }

    // MARK: - Steady state

    #[tokio::test(start_paused = true)]
    async fn steady_state_frames_reach_the_sink_in_order() {
        let (events, error, _) = run(session(MockTransport::closing([
            challenge("n"),
            hello_ok(),
            text(r#"{"type":"event","event":"cron","payload":{"action":"started"}}"#),
            Frame::Other,
            text("not json"),
            text(r#"{"type":"event","event":"heartbeat"}"#),
        ])))
        .await;

        assert_eq!(error, SessionError::Transport(TransportError::Closed));
        let names: Vec<Option<String>> = events
            .iter()
            .map(|event| match event {
                SessionEvent::Connected => None,
                SessionEvent::Frame(env) => Some(env.event.clone().unwrap_or_default()),
            })
            .collect();
        assert_eq!(
            names,
            [None, Some("cron".to_owned()), Some("heartbeat".to_owned())],
            "undecodable frames are skipped, not fatal"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_heartbeat_re_snapshots_channels_and_sessions_every_thirty_seconds() {
        let mut session = session(MockTransport::new([challenge("n"), hello_ok()]));
        // The transport blocks forever once drained, so the only thing that can
        // make progress is the timers. Give it just past two heartbeats.
        let run = tokio::time::timeout(
            HEARTBEAT_INTERVAL * 2 + Duration::from_secs(1),
            session.run(|_| {}),
        )
        .await;
        assert!(run.is_err(), "the session outlives the observation window");

        let sent = session.transport.sent_json();
        let after_bootstrap: Vec<&str> = sent[1 + protocol::BOOTSTRAP_METHODS.len()..]
            .iter()
            .map(|frame| frame["method"].as_str().expect("method"))
            .collect();
        assert_eq!(
            after_bootstrap,
            [
                "channels.status",
                "sessions.list",
                "channels.status",
                "sessions.list"
            ],
            "two heartbeats, and cron is never re-polled (it is broadcast)"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn ping_keepalive_fires_every_twenty_seconds() {
        let mut session = session(MockTransport::new([challenge("n"), hello_ok()]));
        let _ = tokio::time::timeout(
            PING_INTERVAL * 3 + Duration::from_secs(1),
            session.run(|_| {}),
        )
        .await;
        assert_eq!(session.transport.pings, 3);
    }

    #[tokio::test(start_paused = true)]
    async fn neither_timer_fires_before_its_first_interval() {
        let mut session = session(MockTransport::new([challenge("n"), hello_ok()]));
        let _ =
            tokio::time::timeout(PING_INTERVAL - Duration::from_secs(1), session.run(|_| {})).await;
        assert_eq!(session.transport.pings, 0);
        assert_eq!(
            session.transport.sent.len(),
            1 + protocol::BOOTSTRAP_METHODS.len(),
            "bootstrap only — the heartbeat has not come round yet"
        );
    }

    // MARK: - Error rendering

    #[test]
    fn disconnect_reasons_match_the_swift_humanizer() {
        assert_eq!(
            SessionError::InvalidUrl.disconnect_reason(),
            "invalid gateway URL"
        );
        assert_eq!(
            SessionError::HandshakeTimeout.disconnect_reason(),
            "handshake timed out"
        );
        assert_eq!(
            SessionError::SocketClosed.disconnect_reason(),
            "connection closed"
        );
        assert_eq!(
            SessionError::Transport(TransportError::Io("reset".to_owned())).disconnect_reason(),
            "transport failed: reset"
        );
    }

    #[test]
    fn an_invalid_url_converts_into_a_session_error() {
        let error: SessionError = protocol::upgrade_request("https://gw", None)
            .expect_err("rejected")
            .into();
        assert_eq!(error, SessionError::InvalidUrl);
    }

    #[test]
    fn session_debug_never_prints_the_token() {
        let session = Session::new(
            MockTransport::new([]),
            identity(),
            Some("super-secret".to_owned()),
            "1.2.3",
        );
        let rendered = format!("{session:?}");
        assert!(
            !rendered.contains("super-secret"),
            "Debug leaked the gateway token: {rendered}"
        );
        assert!(rendered.contains("authenticated: true"));
        assert!(rendered.contains(identity().device_id()));
    }

    #[tokio::test]
    async fn close_is_forwarded_to_the_transport() {
        let mut session = session(MockTransport::new([]));
        session.close().await;
        assert!(session.transport.closed);
    }

    #[test]
    fn system_clock_is_epoch_milliseconds() {
        let now = system_now_ms();
        // Sanity band rather than a pinned value: after 2020, before 2100.
        assert!(now > 1_577_836_800_000, "{now}");
        assert!(now < 4_102_444_800_000, "{now}");
    }
}
