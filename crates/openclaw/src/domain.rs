//! Runtime-agnostic value types describing a monitored AI-agent runtime.
//!
//! Rust port of `DevCanopy/Models/AgentRuntime/AgentRuntimeModels.swift`. The
//! cockpit reads **only** [`AgentRuntimeSnapshot`]; no gateway wire type from
//! [`crate::rpc`] ever crosses this boundary, so adding a second runtime is
//! "produce the same snapshot from a different protocol" rather than a panel
//! change.
//!
//! Nothing here reads a clock. `last_updated` is set by whoever owns the
//! session, from a time it already had — the same rule `crates/github` follows,
//! for the same reason: an outage must not age healthy data.

use serde::Serialize;

/// Operational state of an agent / cron job / channel — the colored status dot.
///
/// Mirrors periclaw's `domain::AgentStatus` so the derivation rules in
/// [`crate::status`] port across verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Running,
    Ok,
    Error,
    Unknown,
    Disabled,
}

impl AgentStatus {
    /// The Swift enum's raw value, which is what the frontend keys colours on.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            AgentStatus::Running => "running",
            AgentStatus::Ok => "ok",
            AgentStatus::Error => "error",
            AgentStatus::Unknown => "unknown",
            AgentStatus::Disabled => "disabled",
        }
    }
}

/// One renderable row in a runtime section (an agent, a cron job, …).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentRollupItem {
    pub id: String,
    pub name: String,
    pub emoji: Option<String>,
    pub status: AgentStatus,
    /// Secondary line — model ref, last error, "next run in 12m", …
    pub detail: Option<String>,
}

impl AgentRollupItem {
    #[must_use]
    pub fn new(id: impl Into<String>, name: impl Into<String>, status: AgentStatus) -> Self {
        AgentRollupItem {
            id: id.into(),
            name: name.into(),
            emoji: None,
            status,
            detail: None,
        }
    }

    #[must_use]
    pub fn with_emoji(mut self, emoji: Option<String>) -> Self {
        self.emoji = emoji;
        self
    }

    #[must_use]
    pub fn with_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail;
        self
    }
}

/// Glanceable cron rollup: counts by status plus the surviving error line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CronSummary {
    pub ok: usize,
    pub running: usize,
    pub error: usize,
    pub unknown: usize,
    pub disabled: usize,
    /// A non-empty cron `lastError`, surfaced as the one error line.
    pub last_error: Option<String>,
    /// Per-job rows, sorted by name (kept for a drill-down; the glance uses
    /// the counts).
    pub jobs: Vec<AgentRollupItem>,
}

impl CronSummary {
    #[must_use]
    pub fn total(&self) -> usize {
        self.ok + self.running + self.error + self.unknown + self.disabled
    }

    /// Bump the counter for `status`.
    pub(crate) fn count(&mut self, status: AgentStatus) {
        let slot = match status {
            AgentStatus::Ok => &mut self.ok,
            AgentStatus::Running => &mut self.running,
            AgentStatus::Error => &mut self.error,
            AgentStatus::Unknown => &mut self.unknown,
            AgentStatus::Disabled => &mut self.disabled,
        };
        *slot += 1;
    }
}

/// A messaging channel provider (Slack / Telegram / WhatsApp) plus its dot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelStatus {
    pub id: String,
    pub name: String,
    pub status: AgentStatus,
    pub last_error: Option<String>,
}

/// Token-usage rollup for the most relevant session.
///
/// Every counter is optional because the gateway sends them optionally: a
/// session that did not report `totalTokens` is a session whose token count is
/// **unknown**, which is a different fact from a session that burned none.
/// `None` renders the em dash; `Some(0)` renders `0`. Flattening the two here
/// is what made this the one panel that could paint a figure nobody measured.
///
/// `u64` rather than the wire's `i64`: a token count cannot be negative, so the
/// domain refuses to represent one. [`crate::reducer::decode_usage`] does the
/// narrowing and treats an out-of-range value as unknown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SessionUsageRollup {
    pub total_tokens: Option<u64>,
    pub context_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// Milliseconds since the UNIX epoch, kept in the gateway's own unit. The
    /// presentation layer decides how to render it; converting here would bake
    /// in a timezone and a clock this crate does not have.
    pub updated_at_ms: Option<i64>,
}

/// Liveness of a runtime's connection, for the panel's connection line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum RuntimeConnectionState {
    /// Not configured, or deliberately stopped. Distinct from `Disconnected`:
    /// nothing has been attempted, so the panel shows a Settings hint rather
    /// than a failure.
    #[default]
    Idle,
    Connecting,
    Connected,
    Disconnected {
        reason: String,
    },
}

/// Which kind of approval the gateway is waiting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PairingKind {
    FirstPair,
    ScopeUpgrade,
}

/// Surfaced while the gateway waits for the operator to approve this device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingState {
    /// SHA-256(pubkey) hex fingerprint of this install's device key.
    pub device_id: String,
    /// `requestId` the operator passes to `openclaw devices approve <id>`.
    pub request_id: Option<String>,
    pub kind: PairingKind,
    /// Server-authored hint, to be shown verbatim when present.
    pub remediation_hint: Option<String>,
}

/// The single type the cockpit panel knows about. Every runtime reports this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeSnapshot {
    /// Stable runtime id ("openclaw", "hermes").
    pub id: String,
    /// Human-facing label ("OpenClaw").
    pub display_name: String,
    pub connection: RuntimeConnectionState,
    pub agents: Vec<AgentRollupItem>,
    pub cron: CronSummary,
    pub channels: Vec<ChannelStatus>,
    pub usage: Option<SessionUsageRollup>,
    /// Non-`None` while pairing approval is pending.
    pub pairing: Option<PairingState>,
    /// Milliseconds since the UNIX epoch, supplied by the caller — never read
    /// from a clock in here.
    pub last_updated_ms: Option<i64>,
}

impl AgentRuntimeSnapshot {
    /// An empty, idle snapshot for a runtime that hasn't connected yet.
    #[must_use]
    pub fn idle(id: impl Into<String>, display_name: impl Into<String>) -> Self {
        AgentRuntimeSnapshot {
            id: id.into(),
            display_name: display_name.into(),
            connection: RuntimeConnectionState::Idle,
            agents: Vec::new(),
            cron: CronSummary::default(),
            channels: Vec::new(),
            usage: None,
            pairing: None,
            last_updated_ms: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_raw_values_match_the_swift_enum() {
        assert_eq!(AgentStatus::Running.as_str(), "running");
        assert_eq!(AgentStatus::Ok.as_str(), "ok");
        assert_eq!(AgentStatus::Error.as_str(), "error");
        assert_eq!(AgentStatus::Unknown.as_str(), "unknown");
        assert_eq!(AgentStatus::Disabled.as_str(), "disabled");
        assert_eq!(
            serde_json::to_string(&AgentStatus::Disabled).expect("json"),
            "\"disabled\""
        );
    }

    #[test]
    fn cron_total_sums_every_bucket() {
        let mut cron = CronSummary::default();
        assert_eq!(cron.total(), 0);
        for status in [
            AgentStatus::Ok,
            AgentStatus::Running,
            AgentStatus::Error,
            AgentStatus::Unknown,
            AgentStatus::Disabled,
        ] {
            cron.count(status);
        }
        assert_eq!(cron.total(), 5);
        assert_eq!((cron.ok, cron.running, cron.error), (1, 1, 1));
        assert_eq!((cron.unknown, cron.disabled), (1, 1));
    }

    #[test]
    fn idle_snapshot_is_empty_and_not_disconnected() {
        let snapshot = AgentRuntimeSnapshot::idle("openclaw", "OpenClaw");
        assert_eq!(snapshot.id, "openclaw");
        assert_eq!(snapshot.display_name, "OpenClaw");
        assert_eq!(snapshot.connection, RuntimeConnectionState::Idle);
        assert!(snapshot.agents.is_empty());
        assert!(snapshot.channels.is_empty());
        assert_eq!(snapshot.cron.total(), 0);
        assert!(snapshot.usage.is_none());
        assert!(snapshot.pairing.is_none());
        assert!(snapshot.last_updated_ms.is_none());
    }

    #[test]
    fn connection_state_serializes_its_reason() {
        let json = serde_json::to_string(&RuntimeConnectionState::Disconnected {
            reason: "handshake timed out".to_owned(),
        })
        .expect("json");
        assert_eq!(
            json,
            r#"{"state":"disconnected","reason":"handshake timed out"}"#
        );
        assert_eq!(
            serde_json::to_string(&RuntimeConnectionState::Idle).expect("json"),
            r#"{"state":"idle"}"#
        );
    }
}
