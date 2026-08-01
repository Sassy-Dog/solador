//! The real socket: a [`Transport`] over `tokio-tungstenite`.
//!
//! Deliberately thin. Everything interesting about the protocol lives in
//! [`crate::session`] and [`crate::protocol`], which know nothing about this
//! module; all that happens here is frame-shape translation and error
//! classification. That is what lets the whole protocol layer be tested with no
//! listener, no TLS and no port — and it is why this module has almost no
//! tests of its own: there is almost nothing here to get wrong.
//!
//! Behind the `ws` feature (on by default) so a consumer that only wants the
//! identity/reducer layers doesn't link a websocket stack.

use futures_util::{SinkExt as _, StreamExt as _};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::protocol::UpgradeRequest;
use crate::session::{Frame, Transport, TransportError};

/// A connected gateway websocket.
#[derive(Debug)]
pub struct WebSocketTransport {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl WebSocketTransport {
    /// Open the socket described by `request`, carrying its `Authorization` and
    /// `Origin` headers through the upgrade.
    ///
    /// # Errors
    /// Returns [`TransportError`] when the TCP connect, the TLS handshake, or
    /// the HTTP upgrade fails. The gateway enforces `controlUi.allowedOrigins`
    /// at the upgrade, so a rejected `Origin` surfaces here rather than as a
    /// protocol error later.
    pub async fn connect(request: &UpgradeRequest) -> Result<Self, TransportError> {
        let http_request = build_http_request(request)?;

        let (socket, _response) = tokio_tungstenite::connect_async(http_request)
            .await
            .map_err(classify)?;
        Ok(WebSocketTransport { socket })
    }
}

fn build_http_request(request: &UpgradeRequest) -> Result<http::Request<()>, TransportError> {
    let uri: http::Uri = request
        .url
        .parse()
        .map_err(|error: http::uri::InvalidUri| TransportError::Io(error.to_string()))?;
    // tungstenite passes a prebuilt `http::Request` through verbatim: the
    // upgrade headers a bare URL would get generated for free must be supplied
    // by hand, or the handshake is rejected before any frame is exchanged.
    let host = uri
        .authority()
        .ok_or_else(|| TransportError::Io("gateway URL has no host".to_string()))?
        .as_str()
        .to_string();
    let mut builder = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("Host", host)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        );
    for (name, value) in &request.headers {
        builder = builder.header(*name, value);
    }
    builder
        .body(())
        .map_err(|error| TransportError::Io(error.to_string()))
}

#[cfg(test)]
mod upgrade_request_tests {
    use super::*;

    fn upgrade() -> UpgradeRequest {
        UpgradeRequest {
            url: "ws://127.0.0.1:18789/".into(),
            headers: vec![
                ("Origin", "http://127.0.0.1:18789".into()),
                ("Authorization", "Bearer test-token".into()),
            ],
        }
    }

    /// tungstenite passes a prebuilt `http::Request` through verbatim — the
    /// upgrade headers a bare URL would get for free must be supplied by hand,
    /// or the handshake dies with "Missing, duplicated or incorrect header
    /// sec-websocket-key" (observed live against a real gateway, issue #186).
    #[test]
    fn prebuilt_request_carries_every_mandatory_upgrade_header() {
        let req = build_http_request(&upgrade()).expect("request must build");
        let h = req.headers();
        assert_eq!(
            h.get("Host").map(|v| v.to_str().unwrap()),
            Some("127.0.0.1:18789"),
            "Host must come from the URL authority"
        );
        assert_eq!(
            h.get("Connection").map(|v| v.to_str().unwrap()),
            Some("Upgrade")
        );
        assert_eq!(
            h.get("Upgrade").map(|v| v.to_str().unwrap()),
            Some("websocket")
        );
        assert_eq!(
            h.get("Sec-WebSocket-Version").map(|v| v.to_str().unwrap()),
            Some("13")
        );
        let key = h
            .get("Sec-WebSocket-Key")
            .expect("Sec-WebSocket-Key must be present")
            .to_str()
            .unwrap();
        assert_eq!(key.len(), 24, "key must be a base64 16-byte nonce");
        assert_eq!(
            h.get_all("Sec-WebSocket-Key").iter().count(),
            1,
            "exactly one key — duplicates are rejected too"
        );
    }

    /// The caller's own headers survive alongside the mandatory ones.
    #[test]
    fn caller_headers_survive() {
        let req = build_http_request(&upgrade()).expect("request must build");
        assert_eq!(
            req.headers().get("Origin").map(|v| v.to_str().unwrap()),
            Some("http://127.0.0.1:18789")
        );
        assert_eq!(
            req.headers()
                .get("Authorization")
                .map(|v| v.to_str().unwrap()),
            Some("Bearer test-token")
        );
    }
}

/// A closed socket is its own outcome, not an I/O fault: the session's
/// reconnect pacing treats it as an ordinary drop rather than an error to
/// report. Everything else keeps the underlying message.
fn classify(error: WsError) -> TransportError {
    match error {
        WsError::ConnectionClosed | WsError::AlreadyClosed => TransportError::Closed,
        other => TransportError::Io(other.to_string()),
    }
}

// The trait spells these `-> impl Future + Send`; an impl is free to write
// them as `async fn`, and the compiler still checks the `Send` bound here.
impl Transport for WebSocketTransport {
    async fn send_text(&mut self, text: String) -> Result<(), TransportError> {
        self.socket
            .send(Message::Text(text.into()))
            .await
            .map_err(classify)
    }

    /// Cancel-safe, as [`Transport::recv`] requires: `StreamExt::next` on a
    /// `Framed` stream leaves any partially-read message in the codec's buffer,
    /// so the pump dropping this future when a timer wins loses nothing.
    async fn recv(&mut self) -> Result<Frame, TransportError> {
        match self.socket.next().await {
            Some(Ok(message)) => Ok(frame_from(message)),
            Some(Err(error)) => Err(classify(error)),
            None => Err(TransportError::Closed),
        }
    }

    async fn send_ping(&mut self) -> Result<(), TransportError> {
        self.socket
            .send(Message::Ping(Default::default()))
            .await
            .map_err(classify)
    }

    async fn close(&mut self) {
        // Best-effort: the session is already over, so a failure to send the
        // close frame has nowhere useful to go.
        let _ = self.socket.close(None).await;
    }
}

/// Map a tungstenite message onto the session's frame vocabulary. Control
/// frames carry nothing the protocol layer reads, so they collapse to
/// [`Frame::Other`] and are skipped.
fn frame_from(message: Message) -> Frame {
    match message {
        Message::Text(text) => Frame::Text(text.to_string()),
        Message::Binary(bytes) => Frame::Binary(bytes.to_vec()),
        Message::Ping(_) | Message::Pong(_) | Message::Close(_) | Message::Frame(_) => Frame::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_binary_map_across_control_frames_do_not() {
        assert_eq!(
            frame_from(Message::Text("hi".into())),
            Frame::Text("hi".to_owned())
        );
        assert_eq!(
            frame_from(Message::Binary(vec![1, 2, 3].into())),
            Frame::Binary(vec![1, 2, 3])
        );
        for control in [
            Message::Ping(Default::default()),
            Message::Pong(Default::default()),
            Message::Close(None),
        ] {
            assert_eq!(frame_from(control), Frame::Other);
        }
    }

    #[test]
    fn a_closed_socket_is_not_reported_as_an_io_fault() {
        assert_eq!(classify(WsError::ConnectionClosed), TransportError::Closed);
        assert_eq!(classify(WsError::AlreadyClosed), TransportError::Closed);
        assert!(matches!(
            classify(WsError::Io(std::io::Error::other("connection reset"))),
            TransportError::Io(_)
        ));
    }

    #[tokio::test]
    async fn connecting_to_a_dead_address_fails_rather_than_hanging() {
        // Port 0 is never listening, so this exercises the connect error path
        // without binding anything or reaching the network.
        let request = crate::protocol::upgrade_request("ws://127.0.0.1:0", None).expect("url");
        let error = WebSocketTransport::connect(&request)
            .await
            .expect_err("nothing listens on port 0");
        assert!(matches!(error, TransportError::Io(_)));
    }
}
