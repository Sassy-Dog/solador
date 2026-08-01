//! OpenClaw gateway client: WS protocol v3, Ed25519 device identity, and the
//! frame→snapshot reducer.
//!
//! Rust port of `DevCanopy/Services/OpenClaw/` (itself a port of periclaw's
//! `net/openclaw.rs` + `events.rs`). This is the protocol layer only — there is
//! no panel, no polling service, no persistence and no clock ownership here.
//!
//! Layering, outermost first:
//! - [`identity`] — the Ed25519 device key, the byte-exact signed connect
//!   payload, and the pluggable seed store.
//! - [`protocol`] — the connection constants, the WS upgrade request, the
//!   signed `connect` frame, and the pairing classifier. All IO-free.
//! - [`session`] — one session over a [`session::Transport`]: handshake →
//!   bootstrap → pump, with the 10s deadlines and the heartbeat/ping cadences.
//! - [`ws`] — the real socket (feature `ws`, on by default).
//! - [`rpc`] — the wire shapes, deliberately loose.
//! - [`status`] / [`reducer`] — status derivation and the frame→sections fold.
//! - [`domain`] — the runtime-agnostic snapshot the cockpit will render.
//! - [`backoff`] — reconnect pacing.
//!
//! Three rules run through the whole crate:
//!
//! **The signed payload is the contract.** `v2|{deviceId}|{clientId}|…` is
//! reconstructed byte-for-byte by the gateway. Any drift in a delimiter, in the
//! scope order, or in how an absent token serializes rejects every connect with
//! no useful diagnostic, so [`identity`] pins it against a fixture that was
//! cross-checked with Apple CryptoKit.
//!
//! **Tolerate drift, never invent data.** Every wire field is optional and
//! unknown keys are ignored, because OpenClaw ships frequent gateway updates
//! and a shape change must not blank the panel. But an unreadable payload
//! leaves the previous section alone rather than emptying it, and an absent
//! status is [`domain::AgentStatus::Unknown`] — never a green dot we cannot
//! justify.
//!
//! **Nothing here logs.** Not a token, not a seed, not a signature, not a
//! nonce. The types that could carry key material have hand-written `Debug`
//! impls for exactly that reason.

pub mod backoff;
pub mod domain;
pub mod identity;
pub mod protocol;
pub mod reducer;
pub mod rpc;
pub mod session;
pub mod status;
#[cfg(feature = "ws")]
pub mod ws;

pub use backoff::{Backoff, SessionOutcome};
pub use domain::{
    AgentRollupItem, AgentRuntimeSnapshot, AgentStatus, ChannelStatus, CronSummary, PairingKind,
    PairingState, RuntimeConnectionState, SessionUsageRollup,
};
pub use identity::{
    DeviceIdentity, DeviceKeyStore, DeviceKeyStoreError, DeviceSeed, LoadedIdentity,
    MemoryDeviceKeyStore, SignConnectParams, DEVICE_KEY_ACCOUNT,
};
pub use protocol::{InvalidUrl, UpgradeRequest};
pub use reducer::SnapshotReducer;
pub use rpc::Envelope;
pub use session::{Frame, Session, SessionError, SessionEvent, Transport, TransportError};
#[cfg(feature = "ws")]
pub use ws::WebSocketTransport;

/// The stable runtime id this crate reports under, matching the Swift service.
pub const RUNTIME_ID: &str = "openclaw";
/// The human-facing label for the runtime.
pub const RUNTIME_DISPLAY_NAME: &str = "OpenClaw";

/// Broadcasts that mean "still alive" and nothing else.
///
/// They must bump a freshness stamp *without* rebuilding any section: the
/// gateway sends them on a timer, and treating each one as a data change would
/// churn the whole snapshot several times a minute for no visible reason.
/// [`rpc::Envelope::is_liveness_event`] is the discriminator;
/// [`reducer::SnapshotReducer::ingest`] returns `false` for them.
pub const LIVENESS_EVENTS: [&str; 3] = ["health", "heartbeat", "tick"];

/// An idle snapshot for this runtime.
#[must_use]
pub fn idle_snapshot() -> AgentRuntimeSnapshot {
    AgentRuntimeSnapshot::idle(RUNTIME_ID, RUNTIME_DISPLAY_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_snapshot_identifies_the_runtime() {
        let snapshot = idle_snapshot();
        assert_eq!(snapshot.id, "openclaw");
        assert_eq!(snapshot.display_name, "OpenClaw");
        assert_eq!(snapshot.connection, RuntimeConnectionState::Idle);
    }

    /// The whole point of the transport seam: a frame script in, a rendered
    /// snapshot out, with no socket anywhere.
    #[test]
    fn a_frame_script_folds_into_a_renderable_snapshot() {
        let mut reducer = SnapshotReducer::new();
        let mut snapshot = idle_snapshot();
        let frames = [
            r#"{"type":"res","id":"agents.list","payload":{"agents":[
                {"id":"main","identity":{"name":"Sebastian","emoji":"🦀"},
                 "model":{"primary":"anthropic/claude-opus-4-8"}}]}}"#,
            r#"{"type":"res","id":"cron.list","payload":{"jobs":[
                {"name":"nightly","id":"u1","state":{"lastStatus":"ok"}},
                {"name":"backup","state":{"lastStatus":"error","lastError":"disk full"}}]}}"#,
            r#"{"type":"res","id":"channels.status","payload":{"channels":[
                {"name":"slack","enabled":true,"connected":true}]}}"#,
            r#"{"type":"res","id":"sessions.list","payload":{"sessions":[
                {"key":"agent:main:a","totalTokens":900,"updatedAt":5000}]}}"#,
            r#"{"type":"event","event":"agent","payload":{"stream":"tool","sessionKey":"agent:main:s"}}"#,
        ];

        for frame in frames {
            let env = Envelope::parse(frame).expect("frame");
            assert!(!env.is_liveness_event());
            assert!(reducer.ingest(&env), "{frame}");
        }
        reducer.write_sections(&mut snapshot);

        assert_eq!(snapshot.agents.len(), 1);
        assert_eq!(snapshot.agents[0].name, "Sebastian");
        assert_eq!(snapshot.agents[0].status, AgentStatus::Running);
        assert_eq!(snapshot.cron.total(), 2);
        assert_eq!(snapshot.cron.last_error.as_deref(), Some("disk full"));
        assert_eq!(snapshot.channels.len(), 1);
        assert_eq!(snapshot.usage.expect("usage").total_tokens, 900);
        // Sections only: the caller owns the clock and the connection state.
        assert!(snapshot.last_updated_ms.is_none());
        assert_eq!(snapshot.connection, RuntimeConnectionState::Idle);
    }

    #[test]
    fn liveness_events_change_nothing_in_the_sections() {
        let mut reducer = SnapshotReducer::new();
        let mut snapshot = idle_snapshot();
        reducer.write_sections(&mut snapshot);
        let before = snapshot.clone();

        for event in LIVENESS_EVENTS {
            let json = format!(r#"{{"type":"event","event":"{event}","payload":{{}}}}"#);
            let env = Envelope::parse(&json).expect("frame");
            assert!(env.is_liveness_event(), "{event}");
            assert!(!reducer.ingest(&env), "{event}");
        }
        reducer.write_sections(&mut snapshot);
        assert_eq!(snapshot, before);
    }
}
