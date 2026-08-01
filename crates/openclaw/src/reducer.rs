//! Pure state machine folding gateway frames into the data sections of an
//! [`AgentRuntimeSnapshot`].
//!
//! Rust port of `DevCanopy/Services/OpenClaw/OpenClawSnapshotReducer.swift`
//! (periclaw's frame dispatch). Extracted from the session for the same reason
//! Swift extracted it from the socket actor: this is where every visible
//! behaviour lives, and it must be testable without networking.
//!
//! Two properties are load-bearing:
//!
//! **Shape-flexible decoding.** Every RPC payload is accepted as a bare array
//! *or* wrapped in its conventional key (`{jobs:[…]}`, `{crons:[…]}`,
//! `{channels:[…]}`, `{sessions:[…]}`), because OpenClaw ships frequent gateway
//! updates and a shape change must not blank the panel.
//!
//! **A change flag, not a rebuild.** [`SnapshotReducer::ingest`] returns whether
//! the frame touched a data section, so liveness-only broadcasts bump a
//! freshness stamp without the caller reassembling every section.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::domain::{
    AgentRollupItem, AgentRuntimeSnapshot, AgentStatus, ChannelStatus, CronSummary,
    SessionUsageRollup,
};
use crate::rpc::{
    AgentEvent, AgentInfo, AgentsListResponse, Channel, CronEvent, CronJob, CronState, Envelope,
    SessionInfo,
};
use crate::status;

/// Folds frames into renderable sections. Cheap to construct; one per session.
///
/// Cron jobs and channels live in [`BTreeMap`]s rather than hash maps so every
/// derived section — including which error wins [`CronSummary::last_error`] —
/// is deterministic. The Swift original iterates a `Dictionary`, so its choice
/// of surviving error is whatever the hash order happened to be.
#[derive(Debug, Default)]
pub struct SnapshotReducer {
    cron_by_name: BTreeMap<String, CronJob>,
    cron_id_to_name: BTreeMap<String, String>,
    channels: Vec<ChannelStatus>,
    agents: Vec<AgentInfo>,
    /// Transient per-agent activity overriding a resting `Ok`, cleared on a
    /// `lifecycle` end. Keyed by agent id.
    agent_activity: BTreeMap<String, AgentStatus>,
    usage: Option<SessionUsageRollup>,
}

impl SnapshotReducer {
    #[must_use]
    pub fn new() -> Self {
        SnapshotReducer::default()
    }

    /// Fold one frame in.
    ///
    /// Returns `true` when a data section changed (so the caller republishes),
    /// `false` for frames this ignores — including the liveness-only
    /// broadcasts, which are freshness, not data.
    pub fn ingest(&mut self, env: &Envelope) -> bool {
        match env.kind.as_deref() {
            Some("res") => self.reduce_response(env.id.as_deref(), env.payload.as_ref()),
            Some("event") => self.reduce_event(env.event.as_deref(), env.payload.as_ref()),
            _ => false,
        }
    }

    fn reduce_response(&mut self, id: Option<&str>, payload: Option<&Value>) -> bool {
        let (Some(id), Some(payload)) = (id, payload) else {
            return false;
        };
        match id {
            "cron.list" => {
                let jobs = decode_cron_jobs(payload);
                if jobs.is_empty() {
                    // An unreadable or empty payload must not erase what we
                    // already have; a `cron` broadcast can be the only source
                    // of truth between polls.
                    return false;
                }
                for job in jobs {
                    if let Some(job_id) = &job.id {
                        self.cron_id_to_name
                            .insert(job_id.clone(), job.name.clone());
                    }
                    self.cron_by_name.insert(job.name.clone(), job);
                }
                true
            }
            "channels.status" => {
                self.channels = decode_channels(payload)
                    .into_iter()
                    .map(|channel| ChannelStatus {
                        id: channel.name.clone(),
                        status: status::channel_status(&channel),
                        name: channel.name,
                        last_error: channel.last_error,
                    })
                    .collect();
                self.channels.sort_by(|a, b| a.name.cmp(&b.name));
                true
            }
            "sessions.list" => {
                self.usage = decode_usage(payload);
                true
            }
            "agents.list" => match decode_agents(payload) {
                Some(agents) => {
                    self.agents = agents;
                    true
                }
                None => false,
            },
            _ => false,
        }
    }

    fn reduce_event(&mut self, name: Option<&str>, payload: Option<&Value>) -> bool {
        let (Some(name), Some(payload)) = (name, payload) else {
            return false;
        };
        match name {
            "cron" => match cron_job_from_event(payload, &self.cron_id_to_name) {
                Some(job) => {
                    self.cron_by_name.insert(job.name.clone(), job);
                    true
                }
                None => false,
            },
            "agent" => match serde_json::from_value::<AgentEvent>(payload.clone()) {
                Ok(event) => {
                    self.apply_agent_event(&event);
                    true
                }
                Err(_) => false,
            },
            _ => false,
        }
    }

    fn apply_agent_event(&mut self, event: &AgentEvent) {
        let Some(agent_id) = status::agent_id_from_session_key(event.session_key.as_deref()) else {
            return;
        };
        if event.stream.as_deref() == Some("lifecycle") {
            let phase = event.data.as_ref().and_then(|d| d.phase.as_deref());
            match phase {
                Some("end") => {
                    self.agent_activity.remove(agent_id);
                }
                Some("error") => {
                    self.agent_activity
                        .insert(agent_id.to_owned(), AgentStatus::Error);
                }
                Some("start") => {
                    self.agent_activity
                        .insert(agent_id.to_owned(), AgentStatus::Running);
                }
                _ => {}
            }
            return;
        }
        if let Some(activity) = status::agent_activity(event.stream.as_deref()) {
            self.agent_activity.insert(agent_id.to_owned(), activity);
        }
    }

    // MARK: - Section assembly

    /// One row per known agent, in the order the gateway listed them.
    #[must_use]
    pub fn agent_rows(&self) -> Vec<AgentRollupItem> {
        self.agents
            .iter()
            .map(|info| {
                let status = self
                    .agent_activity
                    .get(&info.id)
                    .copied()
                    .unwrap_or(AgentStatus::Ok);
                AgentRollupItem::new(&info.id, info.display_name(), status)
                    .with_emoji(info.display_emoji().map(str::to_owned))
                    .with_detail(info.primary_model().map(str::to_owned))
            })
            .collect()
    }

    /// Counts by status, the surviving error line, and per-job rows sorted by
    /// name.
    #[must_use]
    pub fn cron_summary(&self) -> CronSummary {
        let mut cron = CronSummary::default();
        for job in self.cron_by_name.values() {
            let status = status::cron_status(job.state.as_ref());
            cron.count(status);
            let last_error = job
                .state
                .as_ref()
                .and_then(|s| s.last_error.as_deref())
                .filter(|e| !e.is_empty());
            if let Some(error) = last_error {
                cron.last_error = Some(error.to_owned());
            }
            cron.jobs.push(
                AgentRollupItem::new(&job.name, &job.name, status)
                    .with_detail(job.state.as_ref().and_then(|s| s.last_error.clone())),
            );
        }
        // The map is already name-ordered, so this is a no-op that documents
        // the contract rather than relying on the container.
        cron.jobs.sort_by(|a, b| a.name.cmp(&b.name));
        cron
    }

    /// Channel rows, sorted by name.
    #[must_use]
    pub fn channel_statuses(&self) -> &[ChannelStatus] {
        &self.channels
    }

    /// Usage for the most-recently-updated session, or `None` when the gateway
    /// reported no sessions at all.
    #[must_use]
    pub fn usage_rollup(&self) -> Option<SessionUsageRollup> {
        self.usage
    }

    /// Copy every data section into `snapshot` — the port of Swift's
    /// `rebuildSnapshot`.
    ///
    /// Deliberately leaves `connection`, `pairing` and `last_updated_ms` alone:
    /// those are session state, and the freshness stamp needs a clock this
    /// crate does not own. A liveness broadcast bumps that stamp *without*
    /// calling this at all.
    pub fn write_sections(&self, snapshot: &mut AgentRuntimeSnapshot) {
        snapshot.agents = self.agent_rows();
        snapshot.cron = self.cron_summary();
        snapshot.channels = self.channels.clone();
        snapshot.usage = self.usage;
    }
}

// MARK: - Payload decoding (shape-flexible, mirrors periclaw)

/// Try a bare array first, then each conventional wrapper key in turn.
fn decode_list<T: serde::de::DeserializeOwned>(payload: &Value, keys: &[&str]) -> Option<Vec<T>> {
    if let Ok(list) = serde_json::from_value::<Vec<T>>(payload.clone()) {
        return Some(list);
    }
    for key in keys {
        if let Some(inner) = payload.get(key) {
            if let Ok(list) = serde_json::from_value::<Vec<T>>(inner.clone()) {
                return Some(list);
            }
        }
    }
    None
}

/// `cron.list` arrives as `{jobs:[…]}`, `{crons:[…]}`, or a bare array.
#[must_use]
pub fn decode_cron_jobs(payload: &Value) -> Vec<CronJob> {
    decode_list(payload, &["jobs", "crons"]).unwrap_or_default()
}

/// `channels.status` arrives as `{channels:[…]}` or a bare array.
#[must_use]
pub fn decode_channels(payload: &Value) -> Vec<Channel> {
    decode_list(payload, &["channels"]).unwrap_or_default()
}

/// `agents.list` arrives as `{agents:[…]}` or a bare array.
///
/// Distinct from the others in returning `None` for "no list found": Swift only
/// replaces the roster when the payload actually carried one, so a shape it
/// cannot read leaves the previous agents on screen rather than emptying the
/// section.
#[must_use]
pub fn decode_agents(payload: &Value) -> Option<Vec<AgentInfo>> {
    if let Ok(list) = serde_json::from_value::<Vec<AgentInfo>>(payload.clone()) {
        return Some(list);
    }
    serde_json::from_value::<AgentsListResponse>(payload.clone())
        .ok()
        .and_then(|response| response.agents)
}

/// `sessions.list` arrives as `{sessions:[…]}` or a bare array. The glance
/// shows the most-recently-updated session's totals.
#[must_use]
pub fn decode_usage(payload: &Value) -> Option<SessionUsageRollup> {
    let sessions: Vec<SessionInfo> = decode_list(payload, &["sessions"])?;
    let latest = sessions
        .into_iter()
        .max_by_key(|session| session.updated_at.unwrap_or(0))?;
    Some(SessionUsageRollup {
        total_tokens: latest.total_tokens.unwrap_or(0),
        context_tokens: latest.context_tokens.unwrap_or(0),
        input_tokens: latest.input_tokens.unwrap_or(0),
        output_tokens: latest.output_tokens.unwrap_or(0),
        updated_at_ms: latest.updated_at,
    })
}

/// Synthesize a job from a push `cron` event: `started` → running, `finished` →
/// status/error. Every other action implies no live status change.
#[must_use]
pub fn cron_job_from_event(
    payload: &Value,
    id_to_name: &BTreeMap<String, String>,
) -> Option<CronJob> {
    let event: CronEvent = serde_json::from_value(payload.clone()).ok()?;
    let action = event.action.as_deref()?;
    let name = event
        .job_name
        .clone()
        .or_else(|| {
            event
                .job_id
                .as_ref()
                .and_then(|id| id_to_name.get(id).cloned())
        })
        .or_else(|| event.job_id.clone())
        .unwrap_or_else(|| "unknown".to_owned());

    let state = match action {
        "started" => CronState {
            running: Some(true),
            ..CronState::default()
        },
        "finished" => CronState {
            next_run_at_ms: event.next_run_at_ms,
            last_run_at_ms: event.run_at_ms,
            last_status: event.status,
            last_duration_ms: event.duration_ms,
            last_error: event.error,
            running: Some(false),
        },
        _ => return None,
    };
    Some(CronJob {
        name,
        id: event.job_id,
        state: Some(state),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(json: &str) -> Envelope {
        Envelope::parse(json).expect("decodable envelope")
    }

    fn ingest(reducer: &mut SnapshotReducer, json: &str) -> bool {
        reducer.ingest(&envelope(json))
    }

    // MARK: - cron.list

    #[test]
    fn cron_list_object_shape_populates_the_summary() {
        let mut reducer = SnapshotReducer::new();
        assert!(ingest(
            &mut reducer,
            r#"{"type":"res","id":"cron.list","payload":{"jobs":[
              {"name":"nightly","id":"u1","state":{"lastStatus":"ok"}},
              {"name":"sync","state":{"running":true}},
              {"name":"backup","state":{"lastStatus":"error","lastError":"disk full"}}
            ]}}"#
        ));
        let cron = reducer.cron_summary();
        assert_eq!(cron.ok, 1);
        assert_eq!(cron.running, 1);
        assert_eq!(cron.error, 1);
        assert_eq!(cron.total(), 3);
        assert_eq!(cron.last_error.as_deref(), Some("disk full"));
        assert_eq!(
            cron.jobs
                .iter()
                .map(|j| j.name.as_str())
                .collect::<Vec<_>>(),
            ["backup", "nightly", "sync"],
            "rows are sorted by name"
        );
    }

    #[test]
    fn cron_list_bare_array_shape() {
        let mut reducer = SnapshotReducer::new();
        assert!(ingest(
            &mut reducer,
            r#"{"type":"res","id":"cron.list","payload":[{"name":"a","state":{"lastStatus":"ok"}}]}"#
        ));
        assert_eq!(reducer.cron_summary().ok, 1);
    }

    #[test]
    fn cron_list_crons_key_shape() {
        let mut reducer = SnapshotReducer::new();
        assert!(ingest(
            &mut reducer,
            r#"{"type":"res","id":"cron.list","payload":{"crons":[{"name":"a","state":{"lastStatus":"timeout"}}]}}"#
        ));
        assert_eq!(reducer.cron_summary().error, 1);
    }

    #[test]
    fn an_unreadable_cron_payload_does_not_erase_known_jobs() {
        let mut reducer = SnapshotReducer::new();
        ingest(
            &mut reducer,
            r#"{"type":"res","id":"cron.list","payload":[{"name":"a","state":{"lastStatus":"ok"}}]}"#,
        );
        assert!(!ingest(
            &mut reducer,
            r#"{"type":"res","id":"cron.list","payload":{"somethingNew":[{"name":"b"}]}}"#
        ));
        assert_eq!(reducer.cron_summary().total(), 1);
    }

    // MARK: - channels.status

    #[test]
    fn channels_status_maps_every_dot() {
        let mut reducer = SnapshotReducer::new();
        assert!(ingest(
            &mut reducer,
            r#"{"type":"res","id":"channels.status","payload":{"channels":[
              {"name":"slack","enabled":true,"connected":true},
              {"name":"telegram","enabled":true,"connected":false},
              {"name":"whatsapp","enabled":false},
              {"name":"discord","enabled":true,"lastError":"oops"}
            ]}}"#
        ));
        let by_name: BTreeMap<&str, AgentStatus> = reducer
            .channel_statuses()
            .iter()
            .map(|c| (c.name.as_str(), c.status))
            .collect();
        assert_eq!(by_name["slack"], AgentStatus::Ok);
        assert_eq!(by_name["telegram"], AgentStatus::Unknown);
        assert_eq!(by_name["whatsapp"], AgentStatus::Disabled);
        assert_eq!(by_name["discord"], AgentStatus::Error);
        assert_eq!(
            reducer
                .channel_statuses()
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["discord", "slack", "telegram", "whatsapp"],
            "rows are sorted by name"
        );
        assert_eq!(
            reducer.channel_statuses()[0].id,
            "discord",
            "id mirrors the name, as the panel keys rows on it"
        );
    }

    #[test]
    fn channels_status_bare_array_shape() {
        let mut reducer = SnapshotReducer::new();
        assert!(ingest(
            &mut reducer,
            r#"{"type":"res","id":"channels.status","payload":[{"name":"slack","enabled":true,"connected":true}]}"#
        ));
        assert_eq!(reducer.channel_statuses().len(), 1);
    }

    // MARK: - sessions.list (usage)

    #[test]
    fn usage_picks_the_most_recent_session() {
        let mut reducer = SnapshotReducer::new();
        assert!(ingest(
            &mut reducer,
            r#"{"type":"res","id":"sessions.list","payload":{"sessions":[
              {"key":"agent:main:a","totalTokens":100,"updatedAt":1000},
              {"key":"agent:main:b","totalTokens":900,"contextTokens":42,"updatedAt":5000}
            ]}}"#
        ));
        let usage = reducer.usage_rollup().expect("usage");
        assert_eq!(usage.total_tokens, 900, "the updatedAt=5000 one");
        assert_eq!(usage.context_tokens, 42);
        assert_eq!(usage.updated_at_ms, Some(5000));
        // Absent counters currently collapse to zero — which *is* a fabricated
        // figure, and the one place in the app that still produces one. The
        // gateway sends these as `Option<i64>` (`rpc.rs`), `usage_rollup`
        // flattens them with `unwrap_or(0)` above, and the panel then paints
        // "0 tokens · ctx 0" for a session that simply did not report. Pinned
        // here as the known-wrong behaviour so the fix — `Option` through
        // `SessionUsageRollup` and an em dash in `openclaw::usage_row` — has to
        // come past this assertion and rewrite it deliberately. Tracked in the
        // #178 deferred register (#150 close-out).
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
    }

    #[test]
    fn usage_is_none_when_there_are_no_sessions() {
        let mut reducer = SnapshotReducer::new();
        assert!(ingest(
            &mut reducer,
            r#"{"type":"res","id":"sessions.list","payload":{"sessions":[]}}"#
        ));
        assert!(reducer.usage_rollup().is_none());
    }

    #[test]
    fn usage_bare_array_shape_and_session_key_alias() {
        let mut reducer = SnapshotReducer::new();
        assert!(ingest(
            &mut reducer,
            r#"{"type":"res","id":"sessions.list","payload":[{"sessionKey":"agent:main:a","totalTokens":7}]}"#
        ));
        assert_eq!(reducer.usage_rollup().expect("usage").total_tokens, 7);
    }

    // MARK: - agents.list

    #[test]
    fn agent_rows_use_identity_then_model() {
        let mut reducer = SnapshotReducer::new();
        assert!(ingest(
            &mut reducer,
            r#"{"type":"res","id":"agents.list","payload":{"defaultId":"main","agents":[
              {"id":"main","name":"fallback","identity":{"name":"Sebastian","emoji":"🦀"},
               "model":{"primary":"anthropic/claude-opus-4-8"}}
            ]}}"#
        ));
        let rows = reducer.agent_rows();
        let row = rows.first().expect("one row");
        assert_eq!(row.id, "main");
        assert_eq!(row.name, "Sebastian");
        assert_eq!(row.emoji.as_deref(), Some("🦀"));
        assert_eq!(row.detail.as_deref(), Some("anthropic/claude-opus-4-8"));
        assert_eq!(row.status, AgentStatus::Ok, "resting");
    }

    #[test]
    fn agents_list_bare_array_shape() {
        let mut reducer = SnapshotReducer::new();
        assert!(ingest(
            &mut reducer,
            r#"{"type":"res","id":"agents.list","payload":[{"id":"main"}]}"#
        ));
        assert_eq!(reducer.agent_rows().len(), 1);
    }

    #[test]
    fn an_unreadable_agents_payload_leaves_the_roster_alone() {
        let mut reducer = SnapshotReducer::new();
        ingest(
            &mut reducer,
            r#"{"type":"res","id":"agents.list","payload":[{"id":"main"}]}"#,
        );
        assert!(!ingest(
            &mut reducer,
            r#"{"type":"res","id":"agents.list","payload":{"defaultId":"main"}}"#
        ));
        assert_eq!(reducer.agent_rows().len(), 1);
    }

    // MARK: - cron push event

    #[test]
    fn cron_finished_event_updates_status() {
        let mut reducer = SnapshotReducer::new();
        assert!(ingest(
            &mut reducer,
            r#"{"type":"event","event":"cron","payload":{"jobId":"u","jobName":"nightly",
             "action":"finished","status":"error","error":"boom"}}"#
        ));
        let cron = reducer.cron_summary();
        assert_eq!(cron.error, 1);
        assert_eq!(cron.last_error.as_deref(), Some("boom"));
    }

    #[test]
    fn cron_started_event_flips_a_known_job_to_running_by_id() {
        let mut reducer = SnapshotReducer::new();
        ingest(
            &mut reducer,
            r#"{"type":"res","id":"cron.list","payload":[{"name":"nightly","id":"u1","state":{"lastStatus":"ok"}}]}"#,
        );
        // No jobName: the id learned from cron.list must resolve the name, or
        // the event would create a second row keyed on the raw id.
        assert!(ingest(
            &mut reducer,
            r#"{"type":"event","event":"cron","payload":{"jobId":"u1","action":"started"}}"#
        ));
        let cron = reducer.cron_summary();
        assert_eq!(cron.total(), 1, "the event updated the existing job");
        assert_eq!(cron.running, 1);
        assert_eq!(cron.jobs[0].name, "nightly");
    }

    #[test]
    fn cron_event_for_an_unknown_id_falls_back_to_the_id_then_unknown() {
        let mut reducer = SnapshotReducer::new();
        ingest(
            &mut reducer,
            r#"{"type":"event","event":"cron","payload":{"jobId":"ghost","action":"started"}}"#,
        );
        assert_eq!(reducer.cron_summary().jobs[0].name, "ghost");

        let mut reducer = SnapshotReducer::new();
        ingest(
            &mut reducer,
            r#"{"type":"event","event":"cron","payload":{"action":"started"}}"#,
        );
        assert_eq!(reducer.cron_summary().jobs[0].name, "unknown");
    }

    #[test]
    fn cron_added_event_is_ignored() {
        let mut reducer = SnapshotReducer::new();
        assert!(!ingest(
            &mut reducer,
            r#"{"type":"event","event":"cron","payload":{"jobId":"u","action":"added"}}"#
        ));
        assert_eq!(reducer.cron_summary().total(), 0);
    }

    // MARK: - agent activity events

    #[test]
    fn agent_activity_flips_running_then_clears_on_lifecycle_end() {
        let mut reducer = SnapshotReducer::new();
        ingest(
            &mut reducer,
            r#"{"type":"res","id":"agents.list","payload":{"defaultId":"main","agents":[{"id":"main"}]}}"#,
        );
        assert_eq!(reducer.agent_rows()[0].status, AgentStatus::Ok);

        assert!(ingest(
            &mut reducer,
            r#"{"type":"event","event":"agent","payload":{"stream":"tool","sessionKey":"agent:main:s"}}"#
        ));
        assert_eq!(reducer.agent_rows()[0].status, AgentStatus::Running);

        assert!(ingest(
            &mut reducer,
            r#"{"type":"event","event":"agent","payload":{"stream":"lifecycle","sessionKey":"agent:main:s","data":{"phase":"end"}}}"#
        ));
        assert_eq!(reducer.agent_rows()[0].status, AgentStatus::Ok);
    }

    #[test]
    fn lifecycle_start_and_error_phases_set_activity() {
        let mut reducer = SnapshotReducer::new();
        ingest(
            &mut reducer,
            r#"{"type":"res","id":"agents.list","payload":[{"id":"main"}]}"#,
        );
        ingest(
            &mut reducer,
            r#"{"type":"event","event":"agent","payload":{"stream":"lifecycle","sessionKey":"agent:main:s","data":{"phase":"start"}}}"#,
        );
        assert_eq!(reducer.agent_rows()[0].status, AgentStatus::Running);

        ingest(
            &mut reducer,
            r#"{"type":"event","event":"agent","payload":{"stream":"lifecycle","sessionKey":"agent:main:s","data":{"phase":"error"}}}"#,
        );
        assert_eq!(reducer.agent_rows()[0].status, AgentStatus::Error);

        // An unrecognised phase leaves the previous activity alone rather than
        // guessing.
        ingest(
            &mut reducer,
            r#"{"type":"event","event":"agent","payload":{"stream":"lifecycle","sessionKey":"agent:main:s","data":{"phase":"paused"}}}"#,
        );
        assert_eq!(reducer.agent_rows()[0].status, AgentStatus::Error);
    }

    #[test]
    fn an_agent_event_without_a_routable_session_key_changes_nothing() {
        let mut reducer = SnapshotReducer::new();
        ingest(
            &mut reducer,
            r#"{"type":"res","id":"agents.list","payload":[{"id":"main"}]}"#,
        );
        ingest(
            &mut reducer,
            r#"{"type":"event","event":"agent","payload":{"stream":"tool","sessionKey":"notagent:x"}}"#,
        );
        assert_eq!(reducer.agent_rows()[0].status, AgentStatus::Ok);
    }

    // MARK: - frames the reducer ignores

    #[test]
    fn liveness_events_are_not_data_changes() {
        let mut reducer = SnapshotReducer::new();
        for event in ["health", "heartbeat", "tick"] {
            let json = format!(r#"{{"type":"event","event":"{event}","payload":{{}}}}"#);
            assert!(!ingest(&mut reducer, &json), "{event}");
        }
    }

    #[test]
    fn unknown_frames_are_ignored() {
        let mut reducer = SnapshotReducer::new();
        assert!(!ingest(&mut reducer, r#"{"type":"req","id":"x"}"#));
        assert!(!ingest(&mut reducer, r#"{"type":"res","id":"cron.list"}"#));
        assert!(!ingest(
            &mut reducer,
            r#"{"type":"res","id":"who.knows","payload":{}}"#
        ));
        assert!(!ingest(&mut reducer, r#"{"id":"cron.list","payload":[]}"#));
    }
}
