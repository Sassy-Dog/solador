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
        let mut builder = http::Request::builder()
            .method("GET")
            .uri(request.url.as_str());
        for (name, value) in &request.headers {
            builder = builder.header(*name, value);
        }
        let http_request = builder
            .body(())
            .map_err(|error| TransportError::Io(error.to_string()))?;

        let (socket, _response) = tokio_tungstenite::connect_async(http_request)
            .await
            .map_err(classify)?;
        Ok(WebSocketTransport { socket })
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
