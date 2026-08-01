//! Status derivation — gateway wire state to a coloured dot.
//!
//! Rust port of `OpenClawStatusMapping` in
//! `DevCanopy/Services/OpenClaw/OpenClawWireModels.swift`, itself ported
//! verbatim from periclaw's `events.rs`. Kept in its own module because these
//! four rules are the entire semantic content of the panel: everything else is
//! plumbing, and a drift here is a wrong colour rather than a visible failure.

use crate::domain::AgentStatus;
use crate::rpc::{Channel, CronState};

/// `running` wins outright; then `ok` → ok, `error|failed|timeout` → error,
/// anything else (including a status this build has never heard of) → unknown.
///
/// Absent state is unknown, not ok: we have no evidence either way, and
/// claiming health we cannot see is the one failure mode a cockpit must not
/// have.
#[must_use]
pub fn cron_status(state: Option<&CronState>) -> AgentStatus {
    let Some(state) = state else {
        return AgentStatus::Unknown;
    };
    if state.running == Some(true) {
        return AgentStatus::Running;
    }
    match state.last_status.as_deref() {
        Some("ok") => AgentStatus::Ok,
        Some("error" | "failed" | "timeout") => AgentStatus::Error,
        _ => AgentStatus::Unknown,
    }
}

/// Not explicitly enabled → disabled; a non-empty `lastError` → error;
/// connected → ok; otherwise unknown.
///
/// Note the first rule tests `enabled != Some(true)`, so a channel that omits
/// the field reads as disabled — matching Swift, and the safer reading of a
/// gateway that stopped reporting a provider.
#[must_use]
pub fn channel_status(channel: &Channel) -> AgentStatus {
    if channel.enabled != Some(true) {
        return AgentStatus::Disabled;
    }
    if channel.last_error.as_deref().is_some_and(|e| !e.is_empty()) {
        return AgentStatus::Error;
    }
    if channel.connected == Some(true) {
        AgentStatus::Ok
    } else {
        AgentStatus::Unknown
    }
}

/// Coarse agent activity from an `agent` event's `stream`.
///
/// `None` means "this stream says nothing about activity" — including
/// `lifecycle`, which the reducer handles through `data.phase` instead.
#[must_use]
pub fn agent_activity(stream: Option<&str>) -> Option<AgentStatus> {
    match stream {
        Some("tool" | "item" | "assistant") => Some(AgentStatus::Running),
        Some("error") => Some(AgentStatus::Error),
        _ => None,
    }
}

/// Parse the agent id out of an `agent:<id>:<sessionId>` session key.
#[must_use]
pub fn agent_id_from_session_key(key: Option<&str>) -> Option<&str> {
    let key = key?;
    if !key.starts_with("agent:") {
        return None;
    }
    // Swift splits on ":" and drops empty components, so `agent::x` yields
    // "x". Mirrored here rather than indexing blindly.
    key.split(':').filter(|part| !part.is_empty()).nth(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(running: Option<bool>, last_status: Option<&str>) -> CronState {
        CronState {
            last_status: last_status.map(str::to_owned),
            running,
            ..CronState::default()
        }
    }

    fn channel(
        enabled: Option<bool>,
        connected: Option<bool>,
        last_error: Option<&str>,
    ) -> Channel {
        Channel {
            name: "c".to_owned(),
            enabled,
            connected,
            last_error: last_error.map(str::to_owned),
        }
    }

    #[test]
    fn cron_status_rules() {
        assert_eq!(cron_status(None), AgentStatus::Unknown);
        // running beats a stale failing lastStatus.
        assert_eq!(
            cron_status(Some(&state(Some(true), Some("error")))),
            AgentStatus::Running
        );
        assert_eq!(cron_status(Some(&state(None, Some("ok")))), AgentStatus::Ok);
        for failed in ["error", "failed", "timeout"] {
            assert_eq!(
                cron_status(Some(&state(None, Some(failed)))),
                AgentStatus::Error,
                "{failed}"
            );
        }
        assert_eq!(
            cron_status(Some(&state(None, Some("weird-future")))),
            AgentStatus::Unknown
        );
        assert_eq!(cron_status(Some(&state(None, None))), AgentStatus::Unknown);
        assert_eq!(
            cron_status(Some(&state(Some(false), Some("ok")))),
            AgentStatus::Ok
        );
    }

    #[test]
    fn channel_status_rules() {
        assert_eq!(
            channel_status(&channel(Some(false), None, None)),
            AgentStatus::Disabled
        );
        assert_eq!(
            channel_status(&channel(None, Some(true), None)),
            AgentStatus::Disabled,
            "an absent `enabled` reads as disabled"
        );
        assert_eq!(
            channel_status(&channel(Some(true), None, Some("x"))),
            AgentStatus::Error
        );
        assert_eq!(
            channel_status(&channel(Some(true), Some(true), Some(""))),
            AgentStatus::Ok,
            "an empty lastError is not an error"
        );
        assert_eq!(
            channel_status(&channel(Some(true), Some(true), None)),
            AgentStatus::Ok
        );
        assert_eq!(
            channel_status(&channel(Some(true), Some(false), None)),
            AgentStatus::Unknown
        );
        assert_eq!(
            channel_status(&channel(Some(true), None, None)),
            AgentStatus::Unknown
        );
    }

    #[test]
    fn agent_activity_rules() {
        assert_eq!(agent_activity(Some("tool")), Some(AgentStatus::Running));
        assert_eq!(agent_activity(Some("item")), Some(AgentStatus::Running));
        assert_eq!(
            agent_activity(Some("assistant")),
            Some(AgentStatus::Running)
        );
        assert_eq!(agent_activity(Some("error")), Some(AgentStatus::Error));
        assert_eq!(
            agent_activity(Some("lifecycle")),
            None,
            "lifecycle is routed through data.phase, not here"
        );
        assert_eq!(agent_activity(Some("unheard-of")), None);
        assert_eq!(agent_activity(None), None);
    }

    #[test]
    fn agent_id_parsing() {
        assert_eq!(
            agent_id_from_session_key(Some("agent:main:sess-1")),
            Some("main")
        );
        assert_eq!(
            agent_id_from_session_key(Some("agent:sebastian:x")),
            Some("sebastian")
        );
        assert_eq!(agent_id_from_session_key(Some("notagent:x")), None);
        assert_eq!(agent_id_from_session_key(None), None);
        // Prefix but nothing after it — no id to route to.
        assert_eq!(agent_id_from_session_key(Some("agent:")), None);
        assert_eq!(agent_id_from_session_key(Some("agent")), None);
    }
}
