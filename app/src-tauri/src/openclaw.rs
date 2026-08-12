//! The **OpenClaw** panel and the app-side wiring behind it: the device-key
//! store, the live session state, and every string and colour the frontend
//! paints.
//!
//! Port of `OpenClawPanel` and
//! `OpenClawSettingsView`. The protocol, the
//! identity and the frame→snapshot reducer are [`openclaw`] (#173) — this module
//! consumes that crate and adds nothing to it. What lives here is the *view*:
//! the panel payload, the Settings tab's facts, and the fixtures.
//!
//! Three rules run through it.
//!
//! **The panel is event-driven, so it has no status footer.** Every other panel
//! polls on a cadence and can therefore be stale; this one is fed by a live
//! socket, and its connection line already says whether that socket is up. A
//! staleness footer here would be a second, weaker answer to a question the
//! connection line answers exactly.
//!
//! **Nothing here logs, formats or returns a token, a seed or a signature.**
//! The Settings tab carries a *boolean* for the bearer token, like every other
//! credential, and the device id it shows is a public SHA-256 fingerprint —
//! deliberately not the seed it is derived from.
//!
//! **Setup is not failure.** An empty gateway URL is
//! [`RuntimeConnectionState::Idle`] and paints a muted Settings hint;
//! a failed connect is `Disconnected` and paints red. Collapsing the two would
//! send an operator hunting a break on a machine nobody configured.

use openclaw::{
    AgentRollupItem, AgentRuntimeSnapshot, AgentStatus, ChannelStatus, CronSummary, DeviceKeyStore,
    DeviceKeyStoreError, DeviceSeed, PairingState, SessionUsageRollup,
};
/// Re-exported so `settings.rs` describes the same states this module does,
/// rather than importing the protocol crate under a second name.
pub use openclaw::{PairingKind, RuntimeConnectionState};
use serde_json::{json, Value};
use store::{CredentialStore, SecretKey};
use viewmodel::cockpit::PanelKind;
use viewmodel::color;

/// Shown when the cockpit knows of no agent runtime at all.
///
/// Reachable only with an empty runtime list. This shell always registers the
/// one OpenClaw runtime (as the original app always registers `OpenClawService`),
/// so it is parity for the multi-runtime future rather than a state today's
/// build can enter — and it is rendered, and tested, so the panel cannot trap
/// the day a runtime list becomes configurable.
pub const NO_RUNTIME_MESSAGE: &str = "no agent runtime configured";

/// Configured nothing yet: the runtime is idle because nothing was attempted.
pub const IDLE_HINT: &str = "add a gateway URL in Settings → OpenClaw";

/// The connection line while the socket is being opened.
pub const CONNECTING_REASON: &str = "connecting…";

/// The cron line when no job is known. the original only renders the CRON section
/// once `cron.total > 0`, so this is the summary function's honest answer for a
/// zero state rather than something the shipped panel shows — kept (and tested)
/// because the function is the one place that decides how counts read.
pub const NO_CRON_JOBS: &str = "no jobs";

// MARK: - The device key store

/// Binds [`openclaw`]'s [`DeviceKeyStore`] to the app's credential store.
///
/// The seed travels as **raw bytes** under [`SecretKey::OpenClawDeviceKey`],
/// which is the same account and the same encoding the original app uses. That is
/// the whole point: the operator approves one device id, not one per app.
///
/// A stored value of the wrong length is reported as *absent*, not as an error
/// — an unusable seed and no seed both mean "mint a fresh one", and
/// [`openclaw::identity::load_or_create`] then does exactly that.
pub struct DeviceKeys<'a>(pub &'a dyn CredentialStore);

/// Hand-written for the same reason [`openclaw::DeviceIdentity`]'s is: a `{:?}`
/// of this must never be a route to key material. It has none of its own, and
/// this makes that visible rather than incidental.
impl std::fmt::Debug for DeviceKeys<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceKeys").finish_non_exhaustive()
    }
}

impl DeviceKeyStore for DeviceKeys<'_> {
    fn load_device_key(&self) -> Result<Option<DeviceSeed>, DeviceKeyStoreError> {
        let stored = self
            .0
            .secret_bytes(SecretKey::OpenClawDeviceKey)
            .map_err(DeviceKeyStoreError::new)?;
        Ok(stored.and_then(|bytes| DeviceSeed::try_from(bytes.as_slice()).ok()))
    }

    fn save_device_key(&self, seed: &DeviceSeed) -> Result<(), DeviceKeyStoreError> {
        self.0
            .set_secret_bytes(SecretKey::OpenClawDeviceKey, seed)
            .map_err(DeviceKeyStoreError::new)
    }
}

// MARK: - State

/// Everything the panel and the Settings tab render from.
///
/// One runtime today. The panel's view function still takes a *slice*, because
/// the snapshot type is runtime-agnostic by design and a second runtime must be
/// "one more section", not a panel rewrite.
#[derive(Debug)]
pub struct OpenClawState {
    snapshot: AgentRuntimeSnapshot,
    /// This install's device fingerprint, cached once the session loop has
    /// loaded or minted the key. Public data (SHA-256 of the public key) — the
    /// seed it comes from never leaves the credential store.
    device_id: Option<String>,
}

impl Default for OpenClawState {
    fn default() -> Self {
        OpenClawState::new()
    }
}

impl OpenClawState {
    #[must_use]
    pub fn new() -> Self {
        OpenClawState {
            snapshot: openclaw::idle_snapshot(),
            device_id: None,
        }
    }

    /// The snapshots the panel renders — one per runtime.
    #[must_use]
    pub fn snapshots(&self) -> &[AgentRuntimeSnapshot] {
        std::slice::from_ref(&self.snapshot)
    }

    pub fn set_device_id(&mut self, device_id: impl Into<String>) {
        self.device_id = Some(device_id.into());
    }

    /// Nothing configured, or deliberately stopped. Distinct from a failure.
    pub fn idle(&mut self) {
        self.snapshot.connection = RuntimeConnectionState::Idle;
    }

    pub fn connecting(&mut self) {
        self.snapshot.connection = RuntimeConnectionState::Connecting;
    }

    /// `hello-ok` landed. Clears any pairing banner: the gateway just accepted
    /// this device, so a stale "approve me" line would be a live instruction to
    /// do something already done.
    pub fn connected(&mut self, at_ms: i64) {
        self.snapshot.pairing = None;
        self.snapshot.connection = RuntimeConnectionState::Connected;
        self.snapshot.last_updated_ms = Some(at_ms);
    }

    pub fn disconnected(&mut self, reason: impl Into<String>) {
        self.snapshot.connection = RuntimeConnectionState::Disconnected {
            reason: reason.into(),
        };
    }

    /// A human must approve this device before any connect can succeed.
    pub fn pairing_required(&mut self, pairing: PairingState, reason: impl Into<String>) {
        self.device_id = Some(pairing.device_id.clone());
        self.snapshot.pairing = Some(pairing);
        self.snapshot.connection = RuntimeConnectionState::Disconnected {
            reason: reason.into(),
        };
    }

    /// Bump the freshness stamp without touching a data section — what a
    /// liveness broadcast (`health`/`heartbeat`/`tick`) means.
    pub fn touch(&mut self, at_ms: i64) {
        self.snapshot.last_updated_ms = Some(at_ms);
    }

    /// Copy the reducer's sections in and re-stamp freshness.
    pub fn rebuild(&mut self, reducer: &openclaw::SnapshotReducer, at_ms: i64) {
        reducer.write_sections(&mut self.snapshot);
        self.snapshot.last_updated_ms = Some(at_ms);
    }

    /// Drop every data section, keeping the identity and the connection state.
    ///
    /// For a gateway URL that *changed*: the agents, cron jobs and channels on
    /// screen describe the previous gateway, and carrying them across would
    /// attribute one farm's rows to another.
    pub fn forget_sections(&mut self) {
        let id = std::mem::take(&mut self.snapshot.id);
        let display_name = std::mem::take(&mut self.snapshot.display_name);
        self.snapshot = AgentRuntimeSnapshot::idle(id, display_name);
    }

    /// The facts the Settings tab renders — connection, pairing, device id.
    #[must_use]
    pub fn settings_facts(&self) -> SettingsFacts {
        SettingsFacts {
            connection: self.snapshot.connection.clone(),
            pairing: self.snapshot.pairing.clone(),
            // The pairing block's device id wins, exactly as the original's
            // `service.snapshot.pairing?.deviceID ?? currentDeviceID()` does:
            // it is the id the gateway is actually waiting on.
            device_id: self
                .snapshot
                .pairing
                .as_ref()
                .map(|pairing| pairing.device_id.clone())
                .or_else(|| self.device_id.clone()),
        }
    }
}

/// The live half of the Settings tab: what the session currently knows.
///
/// A plain value, so `settings::view` stays a pure function of data it is
/// handed rather than something that reaches into a running session.
#[derive(Debug, Clone, Default)]
pub struct SettingsFacts {
    pub connection: RuntimeConnectionState,
    pub pairing: Option<PairingState>,
    pub device_id: Option<String>,
}

// MARK: - Formatting

/// `1_234_567` → `1.2M`, `5000` → `5.0k`, `42` → `42`.
///
/// Port of `OpenClawPanel.formatTokens`. Both `String(format:)` and Rust's
/// `{:.1}` round half to even, so the two apps abbreviate the same count
/// identically.
#[must_use]
pub fn tokens(n: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let value = n as f64;
    if n >= 1_000_000 {
        return format!("{:.1}M", value / 1_000_000.0);
    }
    if n >= 1_000 {
        return format!("{:.1}k", value / 1_000.0);
    }
    format!("{n}")
}

/// The em dash a counter the gateway did not report renders as. Same character
/// and same reason as `local.rs`'s and `azure.rs`'s: "we could not find out" is
/// not zero.
const UNKNOWN: &str = "—";

/// One counter for the usage line — an abbreviated figure, or the em dash.
///
/// `Some(0)` is a measured zero and renders `0`; only `None` renders the dash.
/// That distinction is the whole of #184.
fn token_text(count: Option<u64>) -> String {
    count.map_or_else(|| UNKNOWN.to_owned(), tokens)
}

/// The colour of a status dot. Port of `OpenClawPanel.color(for:)`.
///
/// `Unknown` and `Disabled` share muted deliberately: neither is a claim about
/// health, and giving either a green or a red would be inventing one.
#[must_use]
pub fn status_color(status: AgentStatus) -> u32 {
    match status {
        AgentStatus::Running => color::AMBER,
        AgentStatus::Ok => color::GREEN,
        AgentStatus::Error => color::RED,
        AgentStatus::Unknown | AgentStatus::Disabled => color::MUTED,
    }
}

/// A disabled dot is drawn at 40% — `statusDot`'s `.opacity(…)`. It is a dot
/// that is *there but off*, which a hidden dot and a full-strength one both
/// misreport.
#[must_use]
pub fn status_opacity(status: AgentStatus) -> f64 {
    if status == AgentStatus::Disabled {
        0.4
    } else {
        1.0
    }
}

/// A dot the frontend can paint: colour plus opacity, never a status word it
/// would have to map itself.
fn dot(status: AgentStatus) -> Value {
    json!({
        "color": color::hex(status_color(status)),
        "opacity": status_opacity(status),
    })
}

/// `"{title} ({count})"` — `OpenClawPanel.sectionHeader`.
fn section_header(title: &str, count: usize) -> String {
    format!("{title} ({count})")
}

/// The cron counts as one line: `"2 ok · 1 running · 1 error"`.
///
/// Only non-zero buckets appear, in the original's fixed order — a line reading
/// `0 running · 0 error` would draw the eye to two numbers that mean nothing.
#[must_use]
pub fn cron_summary_text(cron: &CronSummary) -> String {
    let parts: Vec<String> = [
        (cron.ok, "ok"),
        (cron.running, "running"),
        (cron.error, "error"),
        (cron.unknown, "unknown"),
        (cron.disabled, "disabled"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, label)| format!("{count} {label}"))
    .collect();
    if parts.is_empty() {
        return NO_CRON_JOBS.to_owned();
    }
    parts.join(" · ")
}

/// The cron section's own dot: error outranks running, which outranks a
/// resting green.
#[must_use]
pub fn cron_status(cron: &CronSummary) -> AgentStatus {
    if cron.error > 0 {
        AgentStatus::Error
    } else if cron.running > 0 {
        AgentStatus::Running
    } else {
        AgentStatus::Ok
    }
}

/// The card header's one-line health across every runtime.
///
/// Ports `OpenClawPanel.trailing`, ladder and all, because the order *is* the
/// meaning: a pending approval is the thing a human must act on, so it outranks
/// a healthy agent count; and "all connected" is checked before "any
/// connecting", so a fleet that is up does not report the one runtime mid-
/// reconnect. An empty runtime list and an all-idle one both yield nothing —
/// the body carries the Settings hint, and a trailing label would repeat it.
#[must_use]
pub fn trailing(snapshots: &[AgentRuntimeSnapshot]) -> String {
    if snapshots.is_empty() {
        return String::new();
    }
    if snapshots.iter().any(|snap| snap.pairing.is_some()) {
        return "pairing required".to_owned();
    }
    if snapshots
        .iter()
        .all(|snap| snap.connection == RuntimeConnectionState::Connected)
    {
        let agents: usize = snapshots.iter().map(|snap| snap.agents.len()).sum();
        let plural = if agents == 1 { "" } else { "s" };
        return format!("{agents} agent{plural}");
    }
    if snapshots
        .iter()
        .any(|snap| snap.connection == RuntimeConnectionState::Connecting)
    {
        return CONNECTING_REASON.to_owned();
    }
    if snapshots
        .iter()
        .any(|snap| matches!(snap.connection, RuntimeConnectionState::Disconnected { .. }))
    {
        return "disconnected".to_owned();
    }
    String::new()
}

// MARK: - View

/// One agent row: dot, optional emoji, name, model, and the `running` badge.
fn agent_row(item: &AgentRollupItem) -> Value {
    json!({
        "dot": dot(item.status),
        "emoji": item.emoji,
        "name": item.name,
        "nameColor": color::hex(color::INK),
        "detail": item.detail,
        "detailColor": color::hex(color::MUTED),
        // Only a *running* agent earns a word here. "ok" on every resting row
        // would be noise on the one line that is meant to be scanned.
        "trailing": (item.status == AgentStatus::Running).then_some("running"),
        "trailingColor": color::hex(color::AMBER),
    })
}

fn channel_row(channel: &ChannelStatus) -> Value {
    json!({
        "dot": dot(channel.status),
        "name": channel.name,
        "nameColor": color::hex(color::INK),
    })
}

/// The token line: `1.2M tokens · ctx 5.0k`, with an em dash per counter the
/// gateway did not report.
///
/// A session that reported **nothing** still renders the line, as
/// `— tokens · ctx —`, rather than being dropped. Dropping it was the other
/// candidate and it is the wrong one here: `runtime_section` already omits this
/// slot when `snapshot.usage` is `None`, which is "there is no session". A live
/// session with unreported counters is a different fact, and hiding it behind
/// the no-session rendering is exactly the conflation the em dash exists to
/// prevent — the same call `github::counts` and `usage::row` make, keeping the
/// row and swapping the value.
fn usage_row(usage: SessionUsageRollup) -> Value {
    json!({
        "text": format!(
            "{} tokens · ctx {}",
            token_text(usage.total_tokens),
            token_text(usage.context_tokens)
        ),
        "color": color::hex(color::MUTED),
    })
}

/// The pairing banner: a blinking amber dot, what kind of approval is pending,
/// the command to run **verbatim**, and which device it applies to.
///
/// The command is the whole point of the banner, so it travels as its own
/// string the frontend renders selectable — an operator has to be able to copy
/// it, and a banner that only *describes* the command is a banner that makes
/// them go and look it up.
fn pairing_banner(pairing: &PairingState) -> Value {
    json!({
        "dotColor": color::hex(color::AMBER),
        // The dot pulses because a human, not a retry, is what clears this.
        "blinking": true,
        "title": match pairing.kind {
            PairingKind::ScopeUpgrade => "scope approval required",
            PairingKind::FirstPair => "device pairing required",
        },
        "titleColor": color::hex(color::AMBER),
        "command": pairing
            .request_id
            .as_ref()
            .map(|request| format!("openclaw devices approve {request}")),
        "commandColor": color::hex(color::INK),
        // The fingerprint is 64 hex chars; the glance shows enough of it to
        // match against the gateway's list, and the Settings tab shows all of it.
        "device": format!("device {}…", truncated_device(&pairing.device_id)),
        "deviceColor": color::hex(color::MUTED),
    })
}

/// The first 16 characters of the device fingerprint, `prefix(16)` in the original.
///
/// Characters, not bytes: the id is lowercase hex today, but slicing a string by
/// byte index is a panic waiting for the first non-ASCII id a gateway invents.
fn truncated_device(device_id: &str) -> String {
    device_id.chars().take(16).collect()
}

/// The `● reason` line under the header, for connecting and disconnected.
fn connection_line(reason: &str, dot_color: u32) -> Value {
    json!({
        "dotColor": color::hex(dot_color),
        "text": reason,
        // The original paints the *dot* with the state's colour and leaves the text
        // muted; the state is the dot, and a red sentence would shout twice.
        "color": color::hex(color::MUTED),
    })
}

/// One runtime's section of the panel.
fn runtime_section(snapshot: &AgentRuntimeSnapshot, multi_runtime: bool) -> Value {
    let mut section = json!({
        "id": snapshot.id,
        // Only when a second runtime exists: a lone "OPENCLAW" above a panel
        // already titled OpenClaw is a heading that says nothing.
        "heading": multi_runtime.then(|| json!({
            "text": snapshot.display_name.to_uppercase(),
            "color": color::hex(color::GREEN),
        })),
        "pairing": Value::Null,
        "connection": Value::Null,
        "hint": Value::Null,
        "agents": Value::Null,
        "cron": Value::Null,
        "channels": Value::Null,
        "usage": Value::Null,
    });

    // The original's if/else-if chain, in the original's order — exactly one of these three
    // renders, and a *connected* runtime renders none of them.
    if let Some(pairing) = snapshot.pairing.as_ref() {
        section["pairing"] = pairing_banner(pairing);
    } else {
        match &snapshot.connection {
            RuntimeConnectionState::Idle => {
                section["hint"] = json!({
                    "text": IDLE_HINT,
                    "color": color::hex(color::MUTED),
                });
            }
            RuntimeConnectionState::Disconnected { reason } => {
                section["connection"] = connection_line(reason, color::RED);
            }
            RuntimeConnectionState::Connecting => {
                section["connection"] = connection_line(CONNECTING_REASON, color::AMBER);
            }
            RuntimeConnectionState::Connected => {}
        }
    }

    if !snapshot.agents.is_empty() {
        section["agents"] = json!({
            "header": section_header("AGENTS", snapshot.agents.len()),
            "headerColor": color::hex(color::MUTED),
            "rows": snapshot.agents.iter().map(agent_row).collect::<Vec<_>>(),
        });
    }

    if snapshot.cron.total() > 0 {
        section["cron"] = json!({
            "header": section_header("CRON", snapshot.cron.total()),
            "headerColor": color::hex(color::MUTED),
            "dot": dot(cron_status(&snapshot.cron)),
            "summary": cron_summary_text(&snapshot.cron),
            "summaryColor": color::hex(color::INK),
            // One error line, in red, and only when there is one. An empty
            // string here would reserve a row for a failure that has not
            // happened.
            "error": snapshot
                .cron
                .last_error
                .as_deref()
                .filter(|error| !error.is_empty())
                .map(|error| json!({ "text": error, "color": color::hex(color::RED) })),
        });
    }

    if !snapshot.channels.is_empty() {
        section["channels"] = json!({
            "header": section_header("CHANNELS", snapshot.channels.len()),
            "headerColor": color::hex(color::MUTED),
            "rows": snapshot.channels.iter().map(channel_row).collect::<Vec<_>>(),
        });
    }

    if let Some(usage) = snapshot.usage {
        section["usage"] = usage_row(usage);
    }

    section
}

/// The panel payload.
///
/// Deliberately carries **no footer**: this panel is fed by a live socket, not a
/// poll, so "how old is this" is answered by the connection line rather than by
/// a staleness clock. See the module docs.
#[must_use]
pub fn view(snapshots: &[AgentRuntimeSnapshot]) -> Value {
    let kind = PanelKind::OpenclawAgents;
    json!({
        "id": kind.id(),
        "title": kind.title(),
        "trailing": trailing(snapshots),
        "message": snapshots.is_empty().then(|| json!({
            "text": NO_RUNTIME_MESSAGE,
            "color": color::hex(color::MUTED),
        })),
        "runtimes": snapshots
            .iter()
            .map(|snapshot| runtime_section(snapshot, snapshots.len() > 1))
            .collect::<Vec<_>>(),
    })
}

// MARK: - The session

/// Re-exported so the shell's loop can talk about sessions without importing
/// the protocol crate a second time — the same shape `usage.rs` uses for
/// `crates/usage`. One spelling of these names in this binary, not two.
pub use openclaw::identity::{current_device_id, load_or_create};
pub use openclaw::{Backoff, DeviceIdentity, SessionOutcome, SnapshotReducer};

/// One session, start to finish, writing everything it learns into `state`.
///
/// Returns how the session ended, in the vocabulary [`Backoff`] paces on:
/// a pending approval waits on a human and is deliberately *not* an escalating
/// failure, so it must not push the exponential state up.
///
/// The reducer outlives the session on purpose. A dropped socket is not new
/// information about the farm, so reconnecting must not blank the agent list
/// and repaint it a second later; the caller resets the reducer only when the
/// *gateway* changes, which genuinely invalidates every row.
pub async fn run_session(
    state: &std::sync::Mutex<OpenClawState>,
    reducer: &mut openclaw::SnapshotReducer,
    gateway_url: &str,
    token: Option<String>,
    identity: openclaw::DeviceIdentity,
    app_version: &str,
) -> SessionOutcome {
    use openclaw::{SessionError, SessionEvent};

    /// Writes a session failure into the panel state.
    ///
    /// A pairing requirement is recorded as a *pairing* rather than as a plain
    /// disconnect: it is the one failure no retry can clear, and the operator
    /// needs the request id it carries.
    fn fail(state: &std::sync::Mutex<OpenClawState>, error: &SessionError) {
        let reason = error.disconnect_reason();
        let mut state = state.lock().expect("openclaw state poisoned");
        match error {
            SessionError::PairingRequired(pairing) => {
                state.pairing_required(pairing.clone(), reason);
            }
            _ => state.disconnected(reason),
        }
    }

    state.lock().expect("openclaw state poisoned").connecting();

    let request = match openclaw::protocol::upgrade_request(gateway_url, token.as_deref()) {
        Ok(request) => request,
        Err(_) => {
            fail(state, &SessionError::InvalidUrl);
            return SessionOutcome::Ended {
                connected_for: None,
            };
        }
    };
    let transport = match openclaw::WebSocketTransport::connect(&request).await {
        Ok(transport) => transport,
        Err(error) => {
            fail(state, &SessionError::Transport(error));
            return SessionOutcome::Ended {
                connected_for: None,
            };
        }
    };

    let mut session = openclaw::Session::new(transport, identity, token, app_version.to_owned());
    let mut connected_at: Option<tokio::time::Instant> = None;
    let result = session
        .run(|event| match event {
            SessionEvent::Connected => {
                connected_at = Some(tokio::time::Instant::now());
                state
                    .lock()
                    .expect("openclaw state poisoned")
                    .connected(openclaw::session::system_now_ms());
            }
            SessionEvent::Frame(envelope) => {
                // A liveness broadcast is freshness, not data: it bumps the
                // stamp without rebuilding a section, or the whole snapshot
                // would churn several times a minute for no visible reason.
                let now = openclaw::session::system_now_ms();
                let mut state = state.lock().expect("openclaw state poisoned");
                if envelope.is_liveness_event() {
                    state.touch(now);
                } else if reducer.ingest(&envelope) {
                    state.rebuild(reducer, now);
                }
            }
        })
        .await;
    session.close().await;

    match result {
        // `run` only ever returns by erroring; this arm exists because the
        // signature allows it, and a clean return is an ordinary drop.
        Ok(()) => state
            .lock()
            .expect("openclaw state poisoned")
            .disconnected("connection closed"),
        Err(error) => {
            fail(state, &error);
            if let SessionError::PairingRequired(_) = error {
                return SessionOutcome::PairingRequired;
            }
        }
    }
    SessionOutcome::Ended {
        connected_for: connected_at.map(|at| at.elapsed()),
    }
}

// MARK: - Fixtures

/// Which rendering `--dump-openclaw` should produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fixture {
    /// A live farm: agents, cron, channels and a usage line.
    Connected,
    /// The same live farm, but the session reported no token counters at all —
    /// the case that used to paint `0 tokens · ctx 0` (#184).
    Unmeasured,
    /// The banner an operator has to act on, with the approve command.
    Pairing,
    /// A failed connect — red, and the reason named.
    Disconnected,
    /// No gateway URL yet: the Settings hint, muted.
    Idle,
    /// No runtime at all, which is the panel's own empty state.
    Empty,
}

/// A hand-made state for the offline fixtures.
///
/// Hand-made for the same reason the other panels' are: a pending pairing and a
/// rejected handshake cannot be produced on demand by whichever machine runs
/// the dump, and a fixture that needs a live gateway is a fixture nobody can
/// regenerate.
#[must_use]
pub fn fixture_state(kind: Fixture) -> OpenClawState {
    let mut state = OpenClawState::new();
    // 64 lowercase hex chars, the shape a real SHA-256 fingerprint has — so the
    // banner's `prefix(16)…` truncation is actually exercised.
    let device_id = "9f2c41ab7e05d3806b1caa4f77e9d21c3b58e0a6f4d29c7b8e15330af6cd9b42";
    state.set_device_id(device_id);

    match kind {
        Fixture::Empty | Fixture::Idle => state.idle(),
        Fixture::Disconnected => state.disconnected("gateway rejected: unknown client"),
        Fixture::Pairing => state.pairing_required(
            PairingState {
                device_id: device_id.to_owned(),
                request_id: Some("req-7f31".to_owned()),
                kind: PairingKind::FirstPair,
                remediation_hint: Some(
                    "Run the command on the gateway host, then this device reconnects on its own."
                        .to_owned(),
                ),
            },
            "awaiting device pairing",
        ),
        Fixture::Connected | Fixture::Unmeasured => {
            state.connected(1_700_000_000_000);
            state.snapshot.agents = vec![
                AgentRollupItem::new("main", "Sebastian", AgentStatus::Running)
                    .with_emoji(Some("🦀".to_owned()))
                    .with_detail(Some("anthropic/claude-opus-4-8".to_owned())),
                AgentRollupItem::new("helper", "Helper", AgentStatus::Ok)
                    .with_detail(Some("anthropic/claude-sonnet-4-8".to_owned())),
                AgentRollupItem::new("scribe", "Scribe", AgentStatus::Disabled),
            ];
            state.snapshot.cron = CronSummary {
                ok: 2,
                running: 1,
                error: 1,
                last_error: Some("backup: disk full".to_owned()),
                ..CronSummary::default()
            };
            state.snapshot.channels = vec![
                ChannelStatus {
                    id: "slack".to_owned(),
                    name: "slack".to_owned(),
                    status: AgentStatus::Ok,
                    last_error: None,
                },
                ChannelStatus {
                    id: "telegram".to_owned(),
                    name: "telegram".to_owned(),
                    status: AgentStatus::Unknown,
                    last_error: None,
                },
                ChannelStatus {
                    id: "whatsapp".to_owned(),
                    name: "whatsapp".to_owned(),
                    status: AgentStatus::Disabled,
                    last_error: None,
                },
            ];
            // The unmeasured fixture keeps the session — and therefore the
            // line — and only drops the counters. `SessionUsageRollup::default`
            // is four `None`s, which is precisely "a session that reported
            // nothing".
            state.snapshot.usage = Some(if kind == Fixture::Unmeasured {
                SessionUsageRollup {
                    updated_at_ms: Some(1_700_000_000_000),
                    ..SessionUsageRollup::default()
                }
            } else {
                SessionUsageRollup {
                    total_tokens: Some(1_234_567),
                    context_tokens: Some(5_000),
                    input_tokens: Some(900_000),
                    output_tokens: Some(334_567),
                    updated_at_ms: Some(1_700_000_000_000),
                }
            });
        }
    }
    state
}

/// The payload `--dump-openclaw` writes.
#[must_use]
pub fn fixture_view(kind: Fixture) -> Value {
    let state = fixture_state(kind);
    match kind {
        // The one state with no runtime at all.
        Fixture::Empty => view(&[]),
        _ => view(state.snapshots()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::MemoryCredentialStore;

    fn connected() -> OpenClawState {
        fixture_state(Fixture::Connected)
    }

    fn section(payload: &Value) -> Value {
        payload["runtimes"][0].clone()
    }

    // MARK: the device key store

    /// The round trip that has to hold for pairing to survive a relaunch: the
    /// seed goes in as 32 raw bytes and comes back as the same 32 raw bytes, so
    /// the device id — and therefore the approval — is stable.
    #[test]
    fn a_device_seed_round_trips_through_the_credential_store() {
        let credentials = MemoryCredentialStore::new();
        let keys = DeviceKeys(&credentials);
        assert_eq!(keys.load_device_key().expect("load"), None);

        let loaded = openclaw::identity::load_or_create(&keys).expect("entropy");
        assert!(loaded.generated, "nothing was stored, so one was minted");
        assert!(loaded.persist_error.is_none(), "and it was persisted");

        let again = openclaw::identity::load_or_create(&keys).expect("entropy");
        assert!(!again.generated, "the second run reuses the stored seed");
        assert_eq!(
            again.identity.device_id(),
            loaded.identity.device_id(),
            "a relaunch must keep the id the operator approved"
        );
        assert_eq!(
            openclaw::identity::current_device_id(&keys).as_deref(),
            Some(loaded.identity.device_id()),
            "and Settings can read it without minting a second one"
        );
    }

    /// Bytes of the wrong length are not a seed. Reporting them as an *error*
    /// would leave `load_or_create` no way forward; reporting them as absent
    /// mints a fresh key, which is the only useful outcome.
    #[test]
    fn a_stored_value_that_is_not_a_seed_reads_as_absent() {
        let credentials = MemoryCredentialStore::new();
        credentials
            .set_secret_bytes(SecretKey::OpenClawDeviceKey, b"too short")
            .expect("set");
        assert_eq!(
            DeviceKeys(&credentials).load_device_key().expect("load"),
            None
        );
        assert_eq!(
            openclaw::identity::SEED_LEN,
            32,
            "the length this store is checking against"
        );
    }

    /// Nothing in this module may become a route to the seed. `DeviceKeys`
    /// borrows a credential store, so a derived `Debug` would have printed
    /// whatever that store's own `Debug` prints.
    #[test]
    fn the_device_key_store_debug_carries_no_key_material() {
        let credentials = MemoryCredentialStore::new();
        let keys = DeviceKeys(&credentials);
        let _ = openclaw::identity::load_or_create(&keys).expect("entropy");
        let seed = keys.load_device_key().expect("load").expect("a seed");

        let rendered = format!("{keys:?}");
        assert_eq!(rendered, "DeviceKeys { .. }");
        assert!(!rendered.contains(&hex_of(&seed)));
    }

    fn hex_of(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    // MARK: formatting

    #[test]
    fn tokens_abbreviate_at_a_thousand_and_at_a_million() {
        assert_eq!(tokens(0), "0");
        assert_eq!(tokens(999), "999");
        assert_eq!(tokens(1_000), "1.0k");
        assert_eq!(tokens(5_000), "5.0k");
        assert_eq!(tokens(999_999), "1000.0k");
        assert_eq!(tokens(1_000_000), "1.0M");
        assert_eq!(tokens(1_234_567), "1.2M");
    }

    #[test]
    fn every_status_has_a_colour_and_only_disabled_is_dimmed() {
        assert_eq!(status_color(AgentStatus::Running), color::AMBER);
        assert_eq!(status_color(AgentStatus::Ok), color::GREEN);
        assert_eq!(status_color(AgentStatus::Error), color::RED);
        assert_eq!(status_color(AgentStatus::Unknown), color::MUTED);
        assert_eq!(status_color(AgentStatus::Disabled), color::MUTED);

        for status in [
            AgentStatus::Running,
            AgentStatus::Ok,
            AgentStatus::Error,
            AgentStatus::Unknown,
        ] {
            assert_eq!(status_opacity(status), 1.0, "{status:?}");
        }
        assert_eq!(status_opacity(AgentStatus::Disabled), 0.4);
    }

    /// `unknown` and `disabled` share a colour, so the dot alone cannot tell
    /// them apart — the opacity is what does, and dropping it would render a
    /// switched-off channel identically to one nobody has heard from.
    #[test]
    fn a_disabled_dot_is_distinguishable_from_an_unknown_one() {
        assert_eq!(
            dot(AgentStatus::Unknown)["color"],
            dot(AgentStatus::Disabled)["color"]
        );
        assert_ne!(
            dot(AgentStatus::Unknown)["opacity"],
            dot(AgentStatus::Disabled)["opacity"]
        );
    }

    #[test]
    fn the_cron_line_names_only_the_buckets_that_have_jobs() {
        let cron = CronSummary {
            ok: 2,
            running: 1,
            error: 1,
            ..CronSummary::default()
        };
        assert_eq!(cron_summary_text(&cron), "2 ok · 1 running · 1 error");
        assert_eq!(
            cron_summary_text(&CronSummary {
                unknown: 3,
                disabled: 1,
                ..CronSummary::default()
            }),
            "3 unknown · 1 disabled"
        );
        assert_eq!(cron_summary_text(&CronSummary::default()), NO_CRON_JOBS);
    }

    #[test]
    fn the_cron_dot_lets_an_error_outrank_a_running_job() {
        let with = |ok, running, error| CronSummary {
            ok,
            running,
            error,
            ..CronSummary::default()
        };
        assert_eq!(cron_status(&with(1, 1, 1)), AgentStatus::Error);
        assert_eq!(cron_status(&with(1, 1, 0)), AgentStatus::Running);
        assert_eq!(cron_status(&with(1, 0, 0)), AgentStatus::Ok);
    }

    // MARK: the trailing ladder

    #[test]
    fn the_trailing_label_follows_the_original_precedence() {
        assert_eq!(trailing(&[]), "", "no runtime claims nothing");

        let mut idle = openclaw::idle_snapshot();
        assert_eq!(trailing(std::slice::from_ref(&idle)), "");

        idle.connection = RuntimeConnectionState::Connecting;
        assert_eq!(trailing(std::slice::from_ref(&idle)), CONNECTING_REASON);

        idle.connection = RuntimeConnectionState::Disconnected {
            reason: "connection closed".to_owned(),
        };
        assert_eq!(trailing(std::slice::from_ref(&idle)), "disconnected");

        // A pending approval beats every other state, including a disconnect
        // it is itself the cause of.
        idle.pairing = Some(PairingState {
            device_id: "abc".to_owned(),
            request_id: None,
            kind: PairingKind::FirstPair,
            remediation_hint: None,
        });
        assert_eq!(trailing(std::slice::from_ref(&idle)), "pairing required");
    }

    #[test]
    fn a_connected_runtime_counts_its_agents_and_pluralises() {
        let state = connected();
        assert_eq!(trailing(state.snapshots()), "3 agents");

        let mut one = openclaw::idle_snapshot();
        one.connection = RuntimeConnectionState::Connected;
        one.agents = vec![AgentRollupItem::new("a", "A", AgentStatus::Ok)];
        assert_eq!(trailing(std::slice::from_ref(&one)), "1 agent");

        one.agents.clear();
        assert_eq!(
            trailing(std::slice::from_ref(&one)),
            "0 agents",
            "connected with nothing to show is still connected"
        );
    }

    /// "all connected" is checked before "any connecting", so one runtime
    /// mid-reconnect must not be able to report the whole fleet as connecting —
    /// and one that is genuinely down must not read as connected either.
    #[test]
    fn a_mixed_fleet_reports_the_least_healthy_state_that_matters() {
        let mut up = openclaw::idle_snapshot();
        up.connection = RuntimeConnectionState::Connected;
        up.agents = vec![AgentRollupItem::new("a", "A", AgentStatus::Ok)];
        let mut coming_up = openclaw::idle_snapshot();
        coming_up.id = "hermes".to_owned();
        coming_up.connection = RuntimeConnectionState::Connecting;

        assert_eq!(
            trailing(&[up.clone(), coming_up.clone()]),
            CONNECTING_REASON
        );
        assert_eq!(trailing(&[up.clone(), up.clone()]), "2 agents");
    }

    // MARK: the panel

    #[test]
    fn an_empty_runtime_list_is_a_message_and_no_sections() {
        let payload = view(&[]);
        assert_eq!(payload["message"]["text"], NO_RUNTIME_MESSAGE);
        assert_eq!(payload["message"]["color"], color::hex(color::MUTED));
        assert_eq!(payload["runtimes"].as_array().expect("array").len(), 0);
        assert_eq!(payload["trailing"], "");
    }

    #[test]
    fn the_panel_carries_the_layouts_own_id_and_title() {
        let payload = view(fixture_state(Fixture::Idle).snapshots());
        assert_eq!(payload["id"], "openclawAgents");
        assert_eq!(payload["title"], "OpenClaw");
        assert!(payload["message"].is_null(), "one runtime is not none");
    }

    /// The panel is event-driven; a staleness footer would be a second, weaker
    /// answer to what the connection line already says exactly.
    #[test]
    fn the_panel_has_no_status_footer() {
        for kind in [
            Fixture::Connected,
            Fixture::Pairing,
            Fixture::Disconnected,
            Fixture::Idle,
            Fixture::Empty,
        ] {
            let payload = fixture_view(kind);
            assert!(payload.get("footer").is_none(), "{kind:?}");
        }
    }

    /// Exactly one of the three banner slots renders, matching the original's
    /// if/else-if chain. Two at once would stack a Settings hint under a
    /// pairing banner and read as two unrelated problems.
    #[test]
    fn at_most_one_banner_renders_per_runtime() {
        for kind in [
            Fixture::Connected,
            Fixture::Pairing,
            Fixture::Disconnected,
            Fixture::Idle,
        ] {
            let runtime = section(&fixture_view(kind));
            let shown = ["pairing", "connection", "hint"]
                .into_iter()
                .filter(|slot| !runtime[*slot].is_null())
                .count();
            assert!(shown <= 1, "{kind:?} rendered {shown} banners");
            // …and a connected runtime renders none of them.
            assert_eq!(shown, usize::from(kind != Fixture::Connected), "{kind:?}");
        }
    }

    #[test]
    fn an_idle_runtime_points_at_settings_rather_than_reporting_a_failure() {
        let runtime = section(&fixture_view(Fixture::Idle));
        assert_eq!(runtime["hint"]["text"], IDLE_HINT);
        assert_eq!(runtime["hint"]["color"], color::hex(color::MUTED));
        assert!(runtime["connection"].is_null(), "nothing was attempted");
    }

    /// Red for a failure, amber for a connect in flight — the distinction an
    /// operator triages on. The reason is the gateway's own words.
    #[test]
    fn a_disconnect_is_red_and_a_connect_in_flight_is_amber() {
        let down = section(&fixture_view(Fixture::Disconnected));
        assert_eq!(
            down["connection"]["text"],
            "gateway rejected: unknown client"
        );
        assert_eq!(down["connection"]["dotColor"], color::hex(color::RED));

        let mut state = OpenClawState::new();
        state.connecting();
        let up = section(&view(state.snapshots()));
        assert_eq!(up["connection"]["text"], CONNECTING_REASON);
        assert_eq!(up["connection"]["dotColor"], color::hex(color::AMBER));
    }

    /// The approve command is the banner's reason to exist, and it has to be
    /// the literal line an operator pastes into a shell.
    #[test]
    fn the_pairing_banner_carries_the_approve_command_verbatim() {
        let runtime = section(&fixture_view(Fixture::Pairing));
        assert_eq!(runtime["pairing"]["title"], "device pairing required");
        assert_eq!(runtime["pairing"]["titleColor"], color::hex(color::AMBER));
        assert_eq!(runtime["pairing"]["blinking"], true);
        assert_eq!(
            runtime["pairing"]["command"],
            "openclaw devices approve req-7f31"
        );
        assert_eq!(
            runtime["pairing"]["device"], "device 9f2c41ab7e05d380…",
            "the first 16 of the fingerprint, then an ellipsis"
        );
    }

    /// A scope upgrade is not a first pair: the device is already known, and
    /// telling the operator to pair it again sends them looking for a request
    /// that does not exist.
    #[test]
    fn a_scope_upgrade_says_so_instead_of_asking_for_a_first_pairing() {
        let mut state = OpenClawState::new();
        state.pairing_required(
            PairingState {
                device_id: "abc".to_owned(),
                request_id: None,
                kind: PairingKind::ScopeUpgrade,
                remediation_hint: None,
            },
            "awaiting scope approval",
        );
        let runtime = section(&view(state.snapshots()));
        assert_eq!(runtime["pairing"]["title"], "scope approval required");
        assert!(
            runtime["pairing"]["command"].is_null(),
            "no requestId means no command to show, not an empty one"
        );
        assert_eq!(runtime["pairing"]["device"], "device abc…");
    }

    #[test]
    fn a_connected_runtime_renders_every_section_it_has_data_for() {
        let runtime = section(&fixture_view(Fixture::Connected));

        assert_eq!(runtime["agents"]["header"], "AGENTS (3)");
        let rows = runtime["agents"]["rows"].as_array().expect("rows");
        assert_eq!(rows[0]["name"], "Sebastian");
        assert_eq!(rows[0]["emoji"], "🦀");
        assert_eq!(rows[0]["detail"], "anthropic/claude-opus-4-8");
        assert_eq!(rows[0]["dot"]["color"], color::hex(color::AMBER));
        assert_eq!(rows[0]["trailing"], "running");
        assert!(
            rows[1]["trailing"].is_null(),
            "only a running agent is badged"
        );
        assert!(
            rows[1]["emoji"].is_null(),
            "and an emoji-less agent has none"
        );
        assert_eq!(rows[2]["dot"]["opacity"], 0.4, "the disabled agent");

        assert_eq!(runtime["cron"]["header"], "CRON (4)");
        assert_eq!(runtime["cron"]["summary"], "2 ok · 1 running · 1 error");
        assert_eq!(runtime["cron"]["dot"]["color"], color::hex(color::RED));
        assert_eq!(runtime["cron"]["error"]["text"], "backup: disk full");
        assert_eq!(runtime["cron"]["error"]["color"], color::hex(color::RED));

        assert_eq!(runtime["channels"]["header"], "CHANNELS (3)");
        let channels = runtime["channels"]["rows"].as_array().expect("rows");
        assert_eq!(channels[0]["name"], "slack");
        assert_eq!(channels[0]["dot"]["color"], color::hex(color::GREEN));
        assert_eq!(channels[2]["dot"]["opacity"], 0.4);

        assert_eq!(runtime["usage"]["text"], "1.2M tokens · ctx 5.0k");
        assert_eq!(runtime["usage"]["color"], color::hex(color::MUTED));
    }

    /// #184: a session that reported no counters renders em dashes, never a
    /// `0` nobody measured — and the line stays, because dropping it would
    /// read as "there is no session", which is a different fact.
    #[test]
    fn unreported_counters_render_em_dashes_and_keep_the_line() {
        let runtime = section(&fixture_view(Fixture::Unmeasured));
        assert_eq!(runtime["usage"]["text"], "— tokens · ctx —");
        assert_eq!(runtime["usage"]["color"], color::hex(color::MUTED));
        assert!(
            !runtime["usage"]["text"]
                .as_str()
                .expect("text")
                .contains('0'),
            "no fabricated zero anywhere in the line"
        );
    }

    /// The other half of the rule: a counter that really is zero says `0`. The
    /// em dash would be just as much of a lie in this direction.
    #[test]
    fn a_measured_zero_still_renders_as_zero() {
        let mut state = connected();
        state.snapshot.usage = Some(SessionUsageRollup {
            total_tokens: Some(0),
            context_tokens: Some(0),
            ..SessionUsageRollup::default()
        });
        assert_eq!(
            section(&view(state.snapshots()))["usage"]["text"],
            "0 tokens · ctx 0"
        );
    }

    /// Per-counter, not all-or-nothing: one reported figure and one absent
    /// renders one of each.
    #[test]
    fn a_partially_reported_session_dashes_only_the_missing_counter() {
        let mut state = connected();
        state.snapshot.usage = Some(SessionUsageRollup {
            total_tokens: Some(900),
            context_tokens: None,
            ..SessionUsageRollup::default()
        });
        assert_eq!(
            section(&view(state.snapshots()))["usage"]["text"],
            "900 tokens · ctx —"
        );
    }

    /// An absent section is absent, not an empty header. `AGENTS (0)` over
    /// nothing is a heading for a list that does not exist.
    #[test]
    fn sections_with_nothing_in_them_are_omitted_entirely() {
        let runtime = section(&fixture_view(Fixture::Idle));
        for slot in ["agents", "cron", "channels", "usage"] {
            assert!(runtime[slot].is_null(), "{slot}");
        }
    }

    /// The sub-header exists to disambiguate two runtimes; with one there is
    /// nothing to disambiguate and the panel title already says OpenClaw.
    #[test]
    fn the_runtime_sub_header_appears_only_when_a_second_runtime_does() {
        let one = fixture_state(Fixture::Connected);
        assert!(section(&view(one.snapshots()))["heading"].is_null());

        let mut hermes = openclaw::idle_snapshot();
        hermes.id = "hermes".to_owned();
        hermes.display_name = "Hermes".to_owned();
        let payload = view(&[one.snapshots()[0].clone(), hermes]);
        assert_eq!(payload["runtimes"][0]["heading"]["text"], "OPENCLAW");
        assert_eq!(
            payload["runtimes"][0]["heading"]["color"],
            color::hex(color::GREEN)
        );
        assert_eq!(payload["runtimes"][1]["heading"]["text"], "HERMES");
    }

    // MARK: state transitions

    /// The gateway just accepted this device, so an "approve me" banner left on
    /// screen would be a live instruction to do something already done.
    #[test]
    fn connecting_successfully_clears_the_pairing_banner() {
        let mut state = fixture_state(Fixture::Pairing);
        assert!(!section(&view(state.snapshots()))["pairing"].is_null());

        state.connected(1_700_000_000_000);
        assert!(section(&view(state.snapshots()))["pairing"].is_null());
        assert_eq!(trailing(state.snapshots()), "0 agents");
    }

    /// Pointing at a different gateway must not attribute the previous farm's
    /// agents to the new one.
    #[test]
    fn forgetting_sections_keeps_the_runtime_identity_and_drops_its_data() {
        let mut state = connected();
        state.forget_sections();

        let payload = view(state.snapshots());
        assert_eq!(payload["runtimes"][0]["id"], "openclaw");
        assert_eq!(payload["title"], "OpenClaw");
        for slot in ["agents", "cron", "channels", "usage"] {
            assert!(payload["runtimes"][0][slot].is_null(), "{slot}");
        }
    }

    /// The Settings tab must be able to show the fingerprint the gateway is
    /// waiting on, which is the pairing block's — not a cached one from an
    /// earlier identity.
    #[test]
    fn the_settings_facts_prefer_the_device_the_gateway_named() {
        let mut state = OpenClawState::new();
        state.set_device_id("cached-id");
        assert_eq!(
            state.settings_facts().device_id.as_deref(),
            Some("cached-id")
        );

        state.pairing_required(
            PairingState {
                device_id: "gateway-id".to_owned(),
                request_id: Some("r".to_owned()),
                kind: PairingKind::FirstPair,
                remediation_hint: None,
            },
            "awaiting device pairing",
        );
        let facts = state.settings_facts();
        assert_eq!(facts.device_id.as_deref(), Some("gateway-id"));
        assert!(facts.pairing.is_some());
    }

    /// The Playwright suite renders these payloads, so a fixture that lost the
    /// case it claims to exercise would leave that suite green while covering
    /// nothing.
    #[test]
    fn the_fixtures_cover_every_rendering_the_panel_has() {
        assert_eq!(
            fixture_view(Fixture::Empty)["message"]["text"],
            NO_RUNTIME_MESSAGE
        );
        assert_eq!(fixture_view(Fixture::Connected)["trailing"], "3 agents");
        assert_eq!(
            fixture_view(Fixture::Pairing)["trailing"],
            "pairing required"
        );
        assert_eq!(
            fixture_view(Fixture::Disconnected)["trailing"],
            "disconnected"
        );
        assert_eq!(fixture_view(Fixture::Idle)["trailing"], "");
        // Same live farm as Connected, so `trailing` matches — the fixture
        // exists for its usage line, which must be the unknown rendering.
        assert_eq!(fixture_view(Fixture::Unmeasured)["trailing"], "3 agents");
        assert_eq!(
            section(&fixture_view(Fixture::Unmeasured))["usage"]["text"],
            "— tokens · ctx —"
        );
    }
}
