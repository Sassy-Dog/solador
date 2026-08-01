//! Pure, IO-free protocol helpers: the connection constants, the WS upgrade
//! request, the signed `connect` frame, the `Origin` derivation, and the
//! pairing-required classifier.
//!
//! Rust port of `DevCanopy/Services/OpenClaw/OpenClawProtocol.swift` (periclaw's
//! `net/openclaw.rs`). Everything here is deliberately socket-free: these are
//! the parts that silently make or break the handshake against a real gateway,
//! so they are unit-testable without one.

use std::fmt;

use serde_json::{json, Value};

use crate::domain::{PairingKind, PairingState};
use crate::identity::{DeviceIdentity, SignConnectParams};
use crate::rpc::Envelope;

/// Client id sent in the connect frame — the gateway's reference TUI client id,
/// which is what its scope policy is written against.
pub const CLIENT_ID: &str = "openclaw-tui";
/// Human-facing client name.
pub const DISPLAY_NAME: &str = "DevCanopy";
/// Connect as a UI, not a headless worker.
pub const CLIENT_MODE: &str = "ui";
/// The role whose scopes are requested below.
pub const ROLE: &str = "operator";
/// Requested scopes, in the exact order they are joined into the signed
/// payload. Reordering them changes the signature the gateway reconstructs.
pub const SCOPES: [&str; 3] = ["operator.read", "operator.approvals", "operator.admin"];
/// Both bounds of the supported WS protocol range. The gateway requires a
/// UI-mode operator client to cover v4 (`maxProtocol >= 4 && minProtocol <= 4`
/// in its connect gate — v3 tolerance exists only for probe/node modes), and
/// rejects v3-only clients with `PROTOCOL_MISMATCH`. Observed live against
/// OpenClaw 2026.7.1-2 (issue #186); the Swift port's v3 predates that gate
/// and was never live-verified.
pub const PROTOCOL_VERSION: u8 = 4;
/// The fixed id used for the connect request, matching the gateway reference
/// client; the gateway echoes it on the `res`.
pub const CONNECT_ID: &str = "connect-1";
/// The event carrying the nonce to sign.
pub const CHALLENGE_EVENT: &str = "connect.challenge";
/// The `payload.type` of a successful connect response.
pub const HELLO_OK: &str = "hello-ok";
/// `error.details.code` that means a human must approve this device.
pub const PAIRING_REQUIRED: &str = "PAIRING_REQUIRED";
/// `error.details.reason` distinguishing a scope upgrade from a first pairing.
pub const SCOPE_UPGRADE_REASON: &str = "scope-upgrade";

/// One-shot snapshot RPCs sent immediately after `hello-ok`, so the panel has
/// state before any push event arrives. `id == method`, which is how responses
/// route in [`crate::reducer`].
///
/// There is deliberately no cron poll: the gateway broadcasts `cron` events,
/// same as periclaw relies on.
pub const BOOTSTRAP_METHODS: [&str; 4] = [
    "cron.list",
    "channels.status",
    "sessions.list",
    "agents.list",
];

/// Re-snapshotted on the heartbeat. Channels aren't broadcast at all and
/// session usage drifts, so these two are the ones that go stale on a
/// long-lived socket.
pub const HEARTBEAT_METHODS: [&str; 2] = ["channels.status", "sessions.list"];

/// The `client.platform` string for the host this build runs on.
///
/// Swift hardcodes `"macos"` because it only ever runs there. This crate is the
/// cross-platform half, so it reports the truth — the gateway logs it, and a
/// Windows install claiming to be a Mac makes operator-side triage a guess.
#[must_use]
pub fn platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        "linux" => "linux",
        other => other,
    }
}

/// Everything needed to open the WS upgrade: the URL as the operator typed it,
/// plus the headers that must ride along.
///
/// The URL is carried through **verbatim** rather than re-serialized from the
/// parsed form, so a gateway address is never quietly rewritten (a normalizer
/// adding a trailing slash is exactly the kind of change that breaks a strict
/// reverse proxy and looks like a server bug).
#[derive(Clone, PartialEq, Eq)]
pub struct UpgradeRequest {
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
}

impl UpgradeRequest {
    /// The value of `name`, or `None` when it was not set.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Hand-written because `headers` carries `Authorization: Bearer <token>`. A
/// derived `Debug` would put the gateway token in any log line, panic message
/// or error chain that rendered a request.
impl fmt::Debug for UpgradeRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(name, _)| *name).collect();
        f.debug_struct("UpgradeRequest")
            .field("url", &self.url)
            .field("headers", &names)
            .finish()
    }
}

/// A gateway URL that isn't a usable `ws(s)://` address.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("gateway URL must be ws:// or wss://")]
pub struct InvalidUrl;

/// Build the WS upgrade request with the optional bearer token and the `Origin`
/// derived from the gateway URL.
///
/// # Errors
/// Returns [`InvalidUrl`] for anything that isn't a parseable `ws`/`wss` URL.
pub fn upgrade_request(
    gateway_url: &str,
    token: Option<&str>,
) -> Result<UpgradeRequest, InvalidUrl> {
    let parsed = url::Url::parse(gateway_url).map_err(|_| InvalidUrl)?;
    let scheme = parsed.scheme().to_ascii_lowercase();
    if scheme != "ws" && scheme != "wss" {
        return Err(InvalidUrl);
    }

    let mut headers = Vec::new();
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        headers.push(("Authorization", format!("Bearer {token}")));
    }
    // The gateway enforces `controlUi.allowedOrigins` on the WS upgrade. Derive
    // the Origin from the gateway URL itself so the operator's allowedOrigins
    // only needs the gateway's own hostname.
    if let Some(origin) = derive_origin(&scheme, parsed.host_str(), parsed.port()) {
        headers.push(("Origin", origin));
    }
    Ok(UpgradeRequest {
        url: gateway_url.to_owned(),
        headers,
    })
}

/// `ws://host:port` → `http://host:port`; `wss://` → `https://`.
///
/// `port` is the *explicitly written* port. A default port (`ws` on 80, `wss`
/// on 443) is absent by the time [`url::Url::port`] returns, which is the same
/// omission a browser makes when it builds an `Origin`.
#[must_use]
pub fn derive_origin(scheme: &str, host: Option<&str>, port: Option<u16>) -> Option<String> {
    let host = host.filter(|h| !h.is_empty())?;
    let http_scheme = if scheme == "wss" { "https" } else { "http" };
    Some(match port {
        Some(port) => format!("{http_scheme}://{host}:{port}"),
        None => format!("{http_scheme}://{host}"),
    })
}

/// The full signed `connect` request frame.
///
/// `signed_at_ms` is injected rather than read from the clock, so the value
/// echoed in the `device` block is exactly the one folded into the signed
/// payload — and so this is deterministically testable.
#[must_use]
pub fn connect_frame(
    nonce: &str,
    identity: &DeviceIdentity,
    token: Option<&str>,
    signed_at_ms: i64,
    app_version: &str,
) -> Value {
    let token = token.filter(|t| !t.is_empty());
    let signature = identity.sign_connect(&SignConnectParams {
        client_id: CLIENT_ID,
        client_mode: CLIENT_MODE,
        role: ROLE,
        scopes: &SCOPES,
        token,
        nonce,
        signed_at_ms,
    });

    let mut params = json!({
        "minProtocol": PROTOCOL_VERSION,
        "maxProtocol": PROTOCOL_VERSION,
        "client": {
            "id": CLIENT_ID,
            "displayName": DISPLAY_NAME,
            "version": app_version,
            "platform": platform(),
            "mode": CLIENT_MODE,
            "instanceId": identity.device_id(),
        },
        "role": ROLE,
        "scopes": SCOPES,
        "caps": [],
        "device": {
            "id": identity.device_id(),
            "publicKey": identity.public_key_base64url(),
            "signature": signature,
            "signedAt": signed_at_ms,
            "nonce": nonce,
        },
    });
    if let Some(token) = token {
        // `params` is built as an object literal directly above, so this cannot
        // be anything else.
        if let Some(object) = params.as_object_mut() {
            object.insert("auth".to_owned(), json!({ "token": token }));
        }
    }

    json!({
        "type": "req",
        "id": CONNECT_ID,
        "method": "connect",
        "params": params,
    })
}

/// A bootstrap/heartbeat RPC frame. `id == method`, which is what lets
/// [`crate::reducer`] route responses without a pending-request table.
#[must_use]
pub fn rpc_frame(method: &str) -> Value {
    json!({ "type": "req", "id": method, "method": method, "params": {} })
}

/// `error.details.code == "PAIRING_REQUIRED"` → a pairing request;
/// `reason == "scope-upgrade"` distinguishes an upgrade from a first pair.
///
/// Returns `None` for every other rejection, which the caller then treats as a
/// generic (fast-retry) handshake failure.
#[must_use]
pub fn classify_pairing(env: &Envelope, fallback_device_id: &str) -> Option<PairingState> {
    let details = env.error.as_ref()?.details.as_ref()?;
    if details.code.as_deref() != Some(PAIRING_REQUIRED) {
        return None;
    }
    let kind = if details.reason.as_deref() == Some(SCOPE_UPGRADE_REASON) {
        PairingKind::ScopeUpgrade
    } else {
        PairingKind::FirstPair
    };
    Some(PairingState {
        device_id: details
            .device_id
            .clone()
            .unwrap_or_else(|| fallback_device_id.to_owned()),
        request_id: details.request_id.clone(),
        kind,
        remediation_hint: details.remediation_hint.clone(),
    })
}

#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    use super::*;

    /// The Swift twin's fixed key: `Data(repeating: 9, count: 32)`.
    fn identity() -> DeviceIdentity {
        DeviceIdentity::from_seed(&[9u8; 32])
    }

    /// Pinned alongside `identity::tests` and confirmed against CryptoKit.
    const SEED9_DEVICE_ID: &str =
        "dbc298251c51321b7266e78d1c151c2b62aff8cb95b293096d3463018544face";
    const SEED9_CONNECT_PAYLOAD: &str = "v2|dbc298251c51321b7266e78d1c151c2b62aff8cb95b293096d3463018544face|openclaw-tui|ui|operator|operator.read,operator.approvals,operator.admin|5|tok|n";
    const SEED9_CONNECT_SIGNATURE: &str =
        "rN-dU_f72lWMyE1aT1spoeyO_vaukOdbymupyBBQSMwcoOQVLnACQXHiYk74IWwibikhiQq4sm7r6D78AY7XCA";

    fn envelope(json: &str) -> Envelope {
        Envelope::parse(json).expect("decodable envelope")
    }

    // MARK: - Origin

    #[test]
    fn derive_origin_maps_scheme_and_keeps_the_port() {
        assert_eq!(
            derive_origin("ws", Some("host"), Some(7878)).as_deref(),
            Some("http://host:7878")
        );
        assert_eq!(
            derive_origin("wss", Some("host"), None).as_deref(),
            Some("https://host")
        );
        assert_eq!(
            derive_origin("wss", Some("h"), Some(443)).as_deref(),
            Some("https://h:443")
        );
        assert_eq!(derive_origin("ws", None, Some(1)), None);
        assert_eq!(derive_origin("ws", Some(""), Some(1)), None);
    }

    // MARK: - Upgrade request

    #[test]
    fn upgrade_request_sets_headers() {
        let request = upgrade_request("ws://gw.local:7878", Some("tok")).expect("valid url");
        assert_eq!(request.url, "ws://gw.local:7878", "url carried verbatim");
        assert_eq!(request.header("Authorization"), Some("Bearer tok"));
        assert_eq!(request.header("Origin"), Some("http://gw.local:7878"));
    }

    #[test]
    fn upgrade_request_omits_auth_without_a_token() {
        let request = upgrade_request("wss://gw.local", None).expect("valid url");
        assert_eq!(request.header("Authorization"), None);
        assert_eq!(request.header("Origin"), Some("https://gw.local"));

        // An empty token is the same as none — never `Bearer ` with nothing
        // after it, which some gateways reject differently from no header.
        let request = upgrade_request("wss://gw.local", Some("")).expect("valid url");
        assert_eq!(request.header("Authorization"), None);
    }

    #[test]
    fn upgrade_request_debug_never_prints_the_token() {
        let request = upgrade_request("ws://gw.local:7878", Some("super-secret")).expect("url");
        let rendered = format!("{request:?}");
        assert!(
            !rendered.contains("super-secret"),
            "Debug leaked the gateway token: {rendered}"
        );
        assert!(rendered.contains("ws://gw.local:7878"));
        assert!(rendered.contains("Authorization"), "header names are safe");
    }

    #[test]
    fn upgrade_request_rejects_non_websocket_urls() {
        for bad in ["https://gw.local", "http://gw.local", "not a url", ""] {
            assert_eq!(
                upgrade_request(bad, None),
                Err(InvalidUrl),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn upgrade_request_accepts_an_uppercase_scheme() {
        let request = upgrade_request("WSS://gw.local", None).expect("valid url");
        assert_eq!(request.header("Origin"), Some("https://gw.local"));
    }

    // MARK: - Connect frame

    #[test]
    fn connect_frame_structure_and_signature() {
        let identity = identity();
        assert_eq!(identity.device_id(), SEED9_DEVICE_ID);

        let frame = connect_frame("n", &identity, Some("tok"), 5, "1.2.3");
        assert_eq!(frame["type"], "req");
        assert_eq!(frame["id"], CONNECT_ID);
        assert_eq!(frame["method"], "connect");

        let params = &frame["params"];
        assert_eq!(params["minProtocol"], 4);
        assert_eq!(params["maxProtocol"], 4);
        assert_eq!(params["role"], "operator");
        assert_eq!(
            params["scopes"],
            json!(["operator.read", "operator.approvals", "operator.admin"])
        );
        assert_eq!(params["caps"], json!([]));
        assert_eq!(params["auth"]["token"], "tok");

        let client = &params["client"];
        assert_eq!(client["id"], "openclaw-tui");
        assert_eq!(client["displayName"], "DevCanopy");
        assert_eq!(client["version"], "1.2.3");
        assert_eq!(client["mode"], "ui");
        assert_eq!(client["instanceId"], SEED9_DEVICE_ID);
        assert_eq!(client["platform"], platform());

        let device = &params["device"];
        assert_eq!(device["id"], SEED9_DEVICE_ID);
        assert_eq!(device["publicKey"], identity.public_key_base64url());
        assert_eq!(device["nonce"], "n");
        assert_eq!(device["signedAt"], 5);

        // End-to-end: the signature in the frame is byte-identical to the
        // pinned fixture AND verifies against the public key over the exact v2
        // payload — this pins the whole connect signing path, including the
        // scope order baked into `SCOPES`.
        assert_eq!(device["signature"], SEED9_CONNECT_SIGNATURE);
        let raw: [u8; 32] = URL_SAFE_NO_PAD
            .decode(identity.public_key_base64url())
            .expect("base64url")
            .try_into()
            .expect("32-byte key");
        let raw_sig: [u8; 64] = URL_SAFE_NO_PAD
            .decode(SEED9_CONNECT_SIGNATURE)
            .expect("base64url")
            .try_into()
            .expect("64-byte signature");
        VerifyingKey::from_bytes(&raw)
            .expect("valid key")
            .verify(
                SEED9_CONNECT_PAYLOAD.as_bytes(),
                &Signature::from_bytes(&raw_sig),
            )
            .expect("frame signature must cover the exact v2 payload");
    }

    #[test]
    fn connect_frame_omits_auth_without_a_token() {
        let frame = connect_frame("n", &identity(), None, 1, "0");
        assert!(frame["params"].get("auth").is_none());

        // An empty token must behave identically — including in the signature,
        // where it is the empty field either way.
        let empty = connect_frame("n", &identity(), Some(""), 1, "0");
        assert!(empty["params"].get("auth").is_none());
        assert_eq!(
            empty["params"]["device"]["signature"],
            frame["params"]["device"]["signature"]
        );
    }

    #[test]
    fn rpc_frames_use_the_method_as_the_id() {
        for method in BOOTSTRAP_METHODS {
            let frame = rpc_frame(method);
            assert_eq!(frame["type"], "req");
            assert_eq!(frame["id"], method);
            assert_eq!(frame["method"], method);
            assert_eq!(frame["params"], json!({}));
        }
    }

    #[test]
    fn heartbeat_methods_are_the_ones_the_gateway_never_broadcasts() {
        // Cron *is* broadcast, so re-polling it on the heartbeat would be pure
        // waste; this pins that decision rather than leaving it implicit.
        assert_eq!(HEARTBEAT_METHODS, ["channels.status", "sessions.list"]);
        assert!(!HEARTBEAT_METHODS.contains(&"cron.list"));
        assert!(HEARTBEAT_METHODS
            .iter()
            .all(|m| BOOTSTRAP_METHODS.contains(m)));
    }

    // MARK: - Pairing classifier

    #[test]
    fn classify_first_pair() {
        let env = envelope(
            r#"{"type":"res","id":"connect-1","ok":false,"error":{"code":"X","details":
             {"code":"PAIRING_REQUIRED","requestId":"req-7","deviceId":"dev-9"}}}"#,
        );
        let pairing = classify_pairing(&env, "fb").expect("pairing");
        assert_eq!(pairing.kind, PairingKind::FirstPair);
        assert_eq!(pairing.request_id.as_deref(), Some("req-7"));
        assert_eq!(pairing.device_id, "dev-9");
        assert_eq!(pairing.remediation_hint, None);
    }

    #[test]
    fn classify_scope_upgrade_and_fallback_device_id() {
        let env = envelope(
            r#"{"type":"res","id":"connect-1","ok":false,"error":{"details":
             {"code":"PAIRING_REQUIRED","requestId":"r","reason":"scope-upgrade",
              "remediationHint":"openclaw devices approve r"}}}"#,
        );
        let pairing = classify_pairing(&env, "fallback-id").expect("pairing");
        assert_eq!(pairing.kind, PairingKind::ScopeUpgrade);
        assert_eq!(
            pairing.device_id, "fallback-id",
            "no deviceId in payload -> fallback"
        );
        assert_eq!(
            pairing.remediation_hint.as_deref(),
            Some("openclaw devices approve r")
        );
    }

    #[test]
    fn classify_returns_none_for_a_non_pairing_error() {
        let env = envelope(
            r#"{"type":"res","id":"connect-1","ok":false,"error":
             {"code":"NOPE","details":{"code":"AUTH_RATE_LIMITED"}}}"#,
        );
        assert!(classify_pairing(&env, "fb").is_none());

        // No error block, and no details block, are both "not pairing".
        assert!(classify_pairing(&envelope(r#"{"type":"res","ok":true}"#), "fb").is_none());
        assert!(classify_pairing(
            &envelope(r#"{"type":"res","ok":false,"error":{"code":"NOPE"}}"#),
            "fb"
        )
        .is_none());
    }
}
