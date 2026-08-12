//! Wire shapes for the OpenClaw gateway WS protocol.
//!
//! Rust port of `OpenClawWireModels` (itself a
//! port of periclaw's `net/rpc.rs`). Intentionally loose — **every** field is
//! optional and unknown keys are ignored — because OpenClaw ships frequent
//! gateway updates and tolerating drift beats hard-failing a frame parse and
//! blanking the panel.
//!
//! Where the original needed a hand-rolled `OCJSON` to capture a payload before knowing
//! its type, this uses [`serde_json::Value`]: same "decode lazily, re-decode
//! once routed by `id`/`event`" strategy, no bespoke enum.

use serde::Deserialize;
use serde_json::Value;

/// A top-level WS frame: `{type, id?, event?, ok?, error?, payload?}`.
///
/// `type` is spelled `kind` here because `type` is a Rust keyword; the wire name
/// is preserved by the rename.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Envelope {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub id: Option<String>,
    pub event: Option<String>,
    pub ok: Option<bool>,
    pub payload: Option<Value>,
    /// Boxed because it is the rarest field and by far the largest: an
    /// unboxed `ErrorBody` roughly doubles every envelope, and envelopes are
    /// moved once per frame on a socket that streams them continuously.
    pub error: Option<Box<ErrorBody>>,
}

impl Envelope {
    /// Parse one text frame. Returns `None` for anything that isn't a decodable
    /// JSON object — a frame we cannot read is skipped, never fatal.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }

    /// `payload.<key>` as a string, for the handful of places that route on one
    /// field (`connect.challenge`'s `nonce`, `hello-ok`'s `type`).
    #[must_use]
    pub fn payload_str(&self, key: &str) -> Option<&str> {
        self.payload.as_ref()?.get(key)?.as_str()
    }

    /// `true` for the liveness-only broadcasts. They bump the freshness stamp
    /// without rebuilding any data section — see [`crate::LIVENESS_EVENTS`].
    #[must_use]
    pub fn is_liveness_event(&self) -> bool {
        self.kind.as_deref() == Some("event")
            && self
                .event
                .as_deref()
                .is_some_and(|e| crate::LIVENESS_EVENTS.contains(&e))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErrorBody {
    pub code: Option<String>,
    pub message: Option<String>,
    pub details: Option<ErrorDetails>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDetails {
    pub code: Option<String>,
    pub request_id: Option<String>,
    pub reason: Option<String>,
    pub device_id: Option<String>,
    pub remediation_hint: Option<String>,
}

// MARK: - RPC payload shapes

/// One scheduled job. `name` is required: a nameless job has nothing to key or
/// render, and a payload carrying one is drift we'd rather drop than display.
#[derive(Debug, Clone, Deserialize)]
pub struct CronJob {
    pub name: String,
    pub id: Option<String>,
    pub state: Option<CronState>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CronState {
    pub next_run_at_ms: Option<i64>,
    pub last_run_at_ms: Option<i64>,
    pub last_status: Option<String>,
    pub last_duration_ms: Option<i64>,
    pub last_error: Option<String>,
    pub running: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub name: String,
    pub enabled: Option<bool>,
    pub connected: Option<bool>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentIdentityInfo {
    pub name: Option<String>,
    pub emoji: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentModelRef {
    pub primary: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentInfo {
    pub id: String,
    pub name: Option<String>,
    pub identity: Option<AgentIdentityInfo>,
    pub model: Option<AgentModelRef>,
    pub workspace: Option<String>,
}

impl AgentInfo {
    /// `identity.name` → `name` → `id`, skipping empties at each step.
    #[must_use]
    pub fn display_name(&self) -> &str {
        let identity_name = self
            .identity
            .as_ref()
            .and_then(|i| i.name.as_deref())
            .filter(|n| !n.is_empty());
        identity_name
            .or_else(|| self.name.as_deref().filter(|n| !n.is_empty()))
            .unwrap_or(&self.id)
    }

    #[must_use]
    pub fn display_emoji(&self) -> Option<&str> {
        self.identity
            .as_ref()
            .and_then(|i| i.emoji.as_deref())
            .filter(|e| !e.is_empty())
    }

    #[must_use]
    pub fn primary_model(&self) -> Option<&str> {
        self.model.as_ref().and_then(|m| m.primary.as_deref())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentsListResponse {
    pub default_id: Option<String>,
    pub agents: Option<Vec<AgentInfo>>,
}

/// One session's token accounting.
///
/// `sessions.list` spells the identifier `key`; `session.message` spells the
/// same thing `sessionKey`. A serde `alias` would reject a payload carrying
/// both, so this goes through [`SessionInfoRaw`] and prefers `key`, matching
/// The original's `decodeIfPresent(.key) ?? decodeIfPresent(.sessionKey) ?? ""`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(from = "SessionInfoRaw")]
pub struct SessionInfo {
    pub key: String,
    pub total_tokens: Option<i64>,
    pub context_tokens: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub updated_at: Option<i64>,
    pub age_ms: Option<i64>,
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionInfoRaw {
    key: Option<String>,
    session_key: Option<String>,
    total_tokens: Option<i64>,
    context_tokens: Option<i64>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    updated_at: Option<i64>,
    age_ms: Option<i64>,
    agent_id: Option<String>,
}

impl From<SessionInfoRaw> for SessionInfo {
    fn from(raw: SessionInfoRaw) -> Self {
        SessionInfo {
            key: raw.key.or(raw.session_key).unwrap_or_default(),
            total_tokens: raw.total_tokens,
            context_tokens: raw.context_tokens,
            input_tokens: raw.input_tokens,
            output_tokens: raw.output_tokens,
            updated_at: raw.updated_at,
            age_ms: raw.age_ms,
            agent_id: raw.agent_id,
        }
    }
}

/// Gateway broadcast `cron` event payload.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CronEvent {
    pub job_id: Option<String>,
    pub job_name: Option<String>,
    pub action: Option<String>,
    pub run_at_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub status: Option<String>,
    pub error: Option<String>,
    pub next_run_at_ms: Option<i64>,
}

/// Gateway broadcast `agent` event payload — `stream` classifies activity,
/// `sessionKey` (`agent:<id>:<sid>`) routes it to an agent.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEvent {
    pub stream: Option<String>,
    pub session_key: Option<String>,
    pub data: Option<AgentEventData>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentEventData {
    pub phase: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(json: &str) -> Envelope {
        Envelope::parse(json).expect("decodable envelope")
    }

    #[test]
    fn envelope_decodes_every_field_and_ignores_unknown_keys() {
        let env = envelope(
            r#"{"type":"res","id":"connect-1","ok":true,"payload":{"type":"hello-ok"},"futureKey":1}"#,
        );
        assert_eq!(env.kind.as_deref(), Some("res"));
        assert_eq!(env.id.as_deref(), Some("connect-1"));
        assert_eq!(env.ok, Some(true));
        assert_eq!(env.payload_str("type"), Some("hello-ok"));
        assert_eq!(env.payload_str("missing"), None);
        assert!(env.error.is_none());
    }

    #[test]
    fn envelope_tolerates_a_missing_payload_and_a_non_object_one() {
        let env = envelope(r#"{"type":"event","event":"heartbeat"}"#);
        assert!(env.payload.is_none());
        assert_eq!(env.payload_str("nonce"), None);

        let env = envelope(r#"{"type":"res","id":"cron.list","payload":[1,2,3]}"#);
        assert_eq!(env.payload_str("anything"), None);
    }

    #[test]
    fn undecodable_text_is_skipped_not_fatal() {
        assert!(Envelope::parse("not json").is_none());
        assert!(Envelope::parse("").is_none());
        // A JSON scalar is valid JSON but not an envelope.
        assert!(Envelope::parse("42").is_none());
    }

    #[test]
    fn liveness_events_are_recognised() {
        for event in ["health", "heartbeat", "tick"] {
            let json = format!(r#"{{"type":"event","event":"{event}"}}"#);
            assert!(envelope(&json).is_liveness_event(), "{event}");
        }
        assert!(!envelope(r#"{"type":"event","event":"cron"}"#).is_liveness_event());
        // Same name arriving as a response is not a liveness broadcast.
        assert!(!envelope(r#"{"type":"res","id":"heartbeat"}"#).is_liveness_event());
    }

    #[test]
    fn error_details_decode_from_camel_case() {
        let env = envelope(
            r#"{"type":"res","ok":false,"error":{"code":"X","message":"m","details":
             {"code":"PAIRING_REQUIRED","requestId":"r","reason":"scope-upgrade",
              "deviceId":"d","remediationHint":"run approve"}}}"#,
        );
        let details = env.error.expect("error").details.expect("details");
        assert_eq!(details.code.as_deref(), Some("PAIRING_REQUIRED"));
        assert_eq!(details.request_id.as_deref(), Some("r"));
        assert_eq!(details.reason.as_deref(), Some("scope-upgrade"));
        assert_eq!(details.device_id.as_deref(), Some("d"));
        assert_eq!(details.remediation_hint.as_deref(), Some("run approve"));
    }

    #[test]
    fn agent_display_name_falls_back_through_identity_then_name_then_id() {
        let full: AgentInfo = serde_json::from_str(
            r#"{"id":"main","name":"fallback","identity":{"name":"Sebastian","emoji":"🦀"},
                "model":{"primary":"anthropic/claude-opus-4-8"}}"#,
        )
        .expect("agent");
        assert_eq!(full.display_name(), "Sebastian");
        assert_eq!(full.display_emoji(), Some("🦀"));
        assert_eq!(full.primary_model(), Some("anthropic/claude-opus-4-8"));

        let named: AgentInfo =
            serde_json::from_str(r#"{"id":"main","name":"fallback"}"#).expect("agent");
        assert_eq!(named.display_name(), "fallback");
        assert_eq!(named.display_emoji(), None);
        assert_eq!(named.primary_model(), None);

        // Empty strings are skipped, not rendered as blank rows.
        let empty: AgentInfo =
            serde_json::from_str(r#"{"id":"main","name":"","identity":{"name":"","emoji":""}}"#)
                .expect("agent");
        assert_eq!(empty.display_name(), "main");
        assert_eq!(empty.display_emoji(), None);
    }

    #[test]
    fn session_info_accepts_key_or_session_key() {
        let a: SessionInfo =
            serde_json::from_str(r#"{"key":"k1","totalTokens":5}"#).expect("session");
        assert_eq!(a.key, "k1");
        assert_eq!(a.total_tokens, Some(5));

        let b: SessionInfo = serde_json::from_str(r#"{"sessionKey":"k2"}"#).expect("session");
        assert_eq!(b.key, "k2");

        // Both present: `key` wins, and it must not be a decode error.
        let both: SessionInfo =
            serde_json::from_str(r#"{"key":"k1","sessionKey":"k2"}"#).expect("session");
        assert_eq!(both.key, "k1");

        // Neither present is tolerated, same as the original's `?? ""`.
        let neither: SessionInfo = serde_json::from_str(r#"{"totalTokens":1}"#).expect("session");
        assert_eq!(neither.key, "");
    }

    #[test]
    fn a_job_without_a_name_fails_its_element_decode() {
        // The reducer relies on this: a malformed element sinks the whole array
        // decode, which falls through to the next candidate shape.
        assert!(serde_json::from_str::<Vec<CronJob>>(r#"[{"id":"u1"}]"#).is_err());
        assert!(serde_json::from_str::<Vec<Channel>>(r#"[{"enabled":true}]"#).is_err());
    }
}
