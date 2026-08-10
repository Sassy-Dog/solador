//! DevCanopy shell. The frontend receives a finished view-model and paints;
//! all logic lives in `viewmodel`.

use agentclient::{AgentClient, AgentError};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use store::{
    ContainerGroupRule, CredentialStore, Host, HostOverflowMode, KeyringStore, SecretKey, Store,
    StoreError, TrackedRepo,
};
use tauri_plugin_notification::NotificationExt;
use uuid::Uuid;
use viewmodel::card::{host_card, pending_card, Connection, HostHistories, Pending};
use viewmodel::cockpit::{
    host_columns, panel_table, CockpitLayout, PanelKind, PanelSpan, HOST_CARD_MIN_WIDTH, SPACING,
};
use viewmodel::color;

mod azure;
mod containers;
mod crons;
mod github;
mod local;
mod openclaw;
mod panel;
mod resume;
mod services;
mod settings;
mod usage;

use azure::AzureState;
use containers::ContainersState;
use crons::CronsState;
use github::GitHubState;
use local::LocalHostState;
use openclaw::OpenClawState;
use settings::{SecretField, StoredSecrets};
use usage::UsageState;

/// How often each host is polled. Matches the Swift side's
/// `RemoteHostMetricsService.start(interval:)` default of 1s, which is also
/// what the charts assume: one history sample is one fixed time slice
/// (`PX_PER_SAMPLE`), so the cadence is part of the time axis, not a tuning
/// knob to be picked per shell.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Consecutive failed polls before a host's card stops claiming to be current.
///
/// Parity with `RemoteHostMetricsService.failureThreshold` (Swift): a single
/// missed poll on a flappy tailnet must not repaint the cockpit red, and a
/// host that is genuinely down flips on the very next tick anyway. Anything
/// below 2 removes the debounce entirely.
const FAILURE_THRESHOLD: u32 = 2;

/// How often each host's `/v1/health` is read, alongside the 1s snapshot poll.
///
/// Deliberately much slower than [`POLL_INTERVAL`]: this answers "is the agent's
/// sampler alive", which changes on the order of minutes, and paying a second
/// request per host per second to learn it would double the tailnet traffic of
/// the whole cockpit for a flag. Ten seconds is the containers panel's cadence
/// for the same reason — fast enough that a stalled sampler is caught within one
/// glance, slow enough to be free.
const HEALTH_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// The mutable half of one watched host. The `AgentClient` is immutable and is
/// held separately so the poll loop never locks across an await.
struct HostState {
    /// The store's host id. Travels to the frontend so a card can be matched
    /// to its DOM node across polls without relying on the display name, which
    /// is user-editable and not unique.
    id: String,
    name: String,
    histories: HostHistories,
    latest: Option<wire::Snapshot>,
    error: Option<String>,
    /// When the last *successful* poll landed. Together with `error`, this is
    /// what lets the `cockpit` command tell "still live" apart from "dead
    /// since Xs ago" instead of letting a stale `latest` masquerade as
    /// current.
    last_success: Option<Instant>,
    /// Back-to-back failed polls; reset to 0 on any success. `error` is only
    /// published once this reaches [`FAILURE_THRESHOLD`] — see `record_poll`.
    consecutive_failures: u32,
    /// The agent's own verdict on its sampler, from `/v1/health` on
    /// [`HEALTH_POLL_INTERVAL`]. `Some(true)` is the one thing that can tell
    /// this side that a *succeeding* snapshot poll is serving frozen numbers —
    /// or, before the sampler's first sample, `empty_snapshot()`'s zeros behind
    /// a green dot, which is the defect this exists for (#182).
    ///
    /// `None` means nobody has told us: no health poll has landed yet, or the
    /// agent is old enough not to report it (pre-#35). It is deliberately not
    /// `false`-by-default — "we have not heard" is not "the sampler is fine",
    /// and only the second of those may keep a card green on this field's say-so.
    sampler_stale: Option<bool>,
    /// How old the agent says its newest sample is, from the same payload.
    ///
    /// The stale badge's age when [`HostState::sampler_stale`] is `Some(true)`,
    /// because in that case this side's own clocks are useless: the last
    /// successful *request* is a second old however long the sampler has been
    /// dead.
    sample_age_seconds: Option<u64>,
}

/// What identifies a poll task's *subject*: which host, on which endpoint.
///
/// The endpoint is part of the identity because a task that keeps polling the
/// old address after an edit is a task reporting on the wrong machine — under
/// the right host's name, which is the worst version of that bug.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HostKey {
    id: Uuid,
    base_url: String,
}

/// One host's live poll task and the state it writes into.
struct PolledHost {
    key: HostKey,
    state: Arc<Mutex<HostState>>,
    /// Shared with the poll task rather than moved into it, so the containers
    /// loop can reach the same agent — one client (and therefore one
    /// connection pool and one token read) per host, not one per feature.
    client: Arc<AgentClient>,
    /// Whether this host's poll tasks hold a token to authenticate with.
    /// `false` covers both blocked cases — nothing stored, and a credential
    /// store that would not answer — because neither yields a token, and a host
    /// with no token never reaches the network: its container section is simply
    /// not polled, exactly as its snapshot is not. *Why* it has none is the
    /// card's business ([`HostToken`]), not the poll set's.
    token_available: bool,
    task: tokio::task::JoinHandle<()>,
}

/// Everything a command can reach: the persisted configuration, the credential
/// store, and the live poll set.
///
/// Per-host locks inside the poll set, not one lock over everything: a poll
/// that is mid-flight to an unreachable host must not be able to hold up the
/// `cockpit` command or any other host's tick. That isolation is the whole
/// point of one task per host.
struct App {
    /// The store file, in memory. Every command that mutates it saves before
    /// returning, so "applied" and "persisted" are the same event.
    store: Mutex<Store>,
    /// The OS credential store. Boxed as a trait object so the command layer
    /// can be exercised against `MemoryCredentialStore` in tests without
    /// prompting for a keychain unlock.
    credentials: Box<dyn CredentialStore + Send + Sync>,
    /// The poll set, in cockpit display order.
    hosts: Mutex<Vec<PolledHost>>,
    /// This machine's own card — the one that leads the grid.
    ///
    /// Its own lock, like every other subsystem's: sampling blocks (sysinfo
    /// enumerates mounts and processes), and a `cockpit` call must never queue
    /// behind it any more than it queues behind a remote poll.
    local: Arc<Mutex<LocalHostState>>,
    /// The Containers panel's own state, on its own 10s cadence.
    ///
    /// Its own lock, and never held while the store's is: the containers loop
    /// takes them one at a time, in sequence, so there is no order to get
    /// wrong between them.
    containers: Mutex<ContainersState>,
    /// The Repos + GitHub Runners panels' state, on the store's
    /// `refresh_interval_secs` cadence.
    ///
    /// One lock for both panels because they share a credential and a poll
    /// pass; like the containers lock, it is never held while the store's is.
    github: Mutex<GitHubState>,
    /// Cuts the GitHub loop's sleep short.
    ///
    /// This is what makes "applies without a restart" true for a *periodic*
    /// service, where reconciling tasks (as [`reload_hosts`] does for hosts) is
    /// not the mechanism: saving a token, clearing one, editing the portfolio
    /// or picking a new refresh interval all wake the loop instead of waiting
    /// out a cadence that can be five minutes long.
    github_wake: tokio::sync::Notify,
    /// The Usage panel's state: Claude token rollups on the store's refresh
    /// interval, Neon and Sentry on their own fixed hourly cadence.
    usage: Mutex<UsageState>,
    /// Cuts the usage loop's sleep short, for the same reason
    /// [`App::github_wake`] exists.
    usage_wake: tokio::sync::Notify,
    /// Set alongside a wake when the edit changed *provider* configuration (a
    /// Neon key, a Sentry token or slug) rather than the Claude cadence.
    ///
    /// Without it, saving a Neon key would repaint the panel with an empty Neon
    /// section and then wait out the full hour before filling it — which reads
    /// exactly like the key having been rejected.
    usage_providers_due: std::sync::atomic::AtomicBool,
    /// The Sentry Crons panel's state, on the same fixed hourly cadence as the
    /// other Sentry read — not the store's shared refresh interval.
    ///
    /// Its own lock rather than a field on [`UsageState`]: the two share a
    /// credential and nothing else. They are different panels, on different rows,
    /// answering different questions ("how much did we consume" vs "what is
    /// broken"), and one lock would make a slow monitor read hold up a repaint of
    /// the Usage card.
    crons: Mutex<CronsState>,
    /// Cuts the crons loop's sleep short after the Sentry token or org slug
    /// changes. An hourly cadence is long enough that waiting one out is
    /// indistinguishable from the credential having been rejected.
    crons_wake: tokio::sync::Notify,
    /// The Azure Cost panel's state, on its own 4h cadence.
    azure: Mutex<AzureState>,
    /// Cuts the Azure loop's sleep short after a SAS URL is saved or cleared.
    /// A four-hour cadence is long enough that waiting one out is
    /// indistinguishable from the credential not having been accepted.
    azure_wake: tokio::sync::Notify,
    /// The OpenClaw panel's state. **Not on a poll cadence**: it is written by
    /// a live WebSocket session as frames arrive, and the `openclaw` command
    /// only reads whatever the socket has published.
    openclaw: Mutex<OpenClawState>,
    /// Restarts the OpenClaw session — and, unlike every other wake here, cuts
    /// short an in-flight *session* rather than a sleep. A gateway URL edited
    /// while a session is up must not have to wait out that session, which on a
    /// healthy socket never ends.
    openclaw_wake: tokio::sync::Notify,
    /// Which parked runs the needs-approval notifier has already alerted on.
    ///
    /// Its own lock rather than a field on [`GitHubState`], because it is not
    /// something either panel renders — that struct's contract is "everything
    /// both panels render from", and a delivery ledger inside it would make
    /// that sentence false.
    approvals: Mutex<github::notify::ApprovalWatch>,
    /// The last known availability of each watched third-party service, for the
    /// same reason and with the same discipline as `approvals` above: a
    /// delivery ledger, not something a panel renders.
    service_status: Mutex<services::StatusWatch>,
    /// Every watched vendor's availability, as the panels render it.
    services: Mutex<services::ServiceStatuses>,
    /// The last known reachability of each monitored host, for the banner. Same
    /// discipline as `approvals` and `service_status`: a delivery ledger, not
    /// something a card renders.
    host_reachability: Mutex<services::HostWatch>,
    /// The Tauri handle, once the app has started — the notifier's way out to
    /// the OS.
    ///
    /// A `OnceLock` because the poll loops are spawned *before*
    /// `tauri::Builder::run`, and under `--dump-*` they are never spawned at
    /// all: "not up yet" and "not an app at all" are both real, and neither is
    /// a reason to panic.
    handle: std::sync::OnceLock<tauri::AppHandle>,
    /// Where poll tasks are spawned. A `Handle`, not the `Runtime`: the
    /// runtime itself stays owned by `main`, so it can never be dropped from
    /// inside a command (dropping a runtime from an async context panics).
    runtime: tokio::runtime::Handle,
}

/// The whole `(latest, error)` -> view-model decision, pulled out of the
/// `#[tauri::command]` so it's a plain function over `&HostState`: no
/// locking, no `tauri::State`, trivial to unit-test all four combinations
/// against directly. This is deliberately where CRITICAL 1 (a stale
/// snapshot rendering as live forever) would reappear if it ever did — see
/// the tests below, which fail loudly if this match ever grows a wildcard
/// arm again.
///
/// The four states are a fact about *this side's* polling, and #182 is the case
/// where that is not enough: a host whose agent is answering every request with
/// numbers its sampler stopped producing lands in the live arm, and nothing
/// here can see it. The guard below is the agent's own answer to that question,
/// and it splits the live arm rather than adding a fifth state — the card still
/// has real data and a stale badge, which is exactly the second arm's shape.
fn view_for(s: &HostState) -> Value {
    let mut card = match (&s.latest, &s.error) {
        // A snapshot exists, the poll succeeded — and the agent says its own
        // sampler has stopped, so what just arrived is frozen (or, before the
        // sampler's first sample, all zeros). Real data, unmissable badge, and
        // never the green dot the poll's success would otherwise earn it.
        (Some(snap), None) if s.sampler_stale == Some(true) => host_card(
            &s.name,
            snap,
            &s.histories,
            &Connection::SamplerStale {
                sample_age_secs: s.sample_age_seconds,
            },
        ),
        // A snapshot exists and the most recent poll succeeded: live.
        (Some(snap), None) => host_card(&s.name, snap, &s.histories, &Connection::Live),
        // A snapshot exists but the most recent poll failed: **blank the card**
        // and say the host cannot be contacted.
        //
        // This used to render the last snapshot behind a stale badge, on the
        // reasoning that it was real data, just not current. A badge is not
        // enough. Every figure on a host card is a present-tense claim — 12%
        // CPU, 40°C, 3 containers up — and a machine that has not answered in
        // four minutes is none of those things. Reading the card at a glance,
        // which is the only way an always-on cockpit is ever read, the numbers
        // are what you see and the badge is what you do not. It is the em-dash
        // rule at card scale: an unmeasured figure is not shown as a figure.
        //
        // The loss is bounded and the recovery is free — `histories` stays in
        // state, so the sparklines come back intact with the host. What is kept
        // is *when* it went quiet, which is the one fact still true.
        //
        // No sampler guard here on purpose. Both facts produce the same card,
        // and when the link is down the transport failure is the more proximate
        // cause *and* the more recent one: `sampler_stale` is by then whatever
        // the last reachable health poll said, up to ten seconds before the
        // agent went quiet. Naming the sampler would send an operator to
        // restart a daemon they cannot currently reach.
        (Some(_), Some(msg)) => {
            // `None` when a snapshot exists but no successful poll is on
            // record -- unreachable via the real poll loop below (latest and
            // last_success are always set together), but the card must still
            // render it as "unknown", never a fabricated `0s ago`.
            let age_secs = s.last_success.map(|t| t.elapsed().as_secs());
            pending_card(
                &s.name,
                &Pending::Unreachable {
                    message: msg.clone(),
                    age_secs,
                },
            )
        }
        // Never connected, and the most recent attempt failed.
        (None, Some(msg)) => pending_card(&s.name, &Pending::Failed(msg.clone())),
        // Never connected, still waiting on the first tick.
        (None, None) => pending_card(&s.name, &Pending::Connecting),
    };
    card["id"] = json!(s.id);
    card
}

/// The whole cockpit payload: one card per host plus the grid decisions.
///
/// `available` is the width the frontend has for the host grid. The column
/// count is decided *here* rather than in CSS for the same reason every string
/// and colour is: `host_columns` is the tested breakpoint math (a card needs
/// [`HOST_CARD_MIN_WIDTH`]), and a CSS `auto-fit` restating it is a second
/// implementation free to disagree with the first.
fn cockpit_view(
    local: Option<Value>,
    hosts: &[Arc<Mutex<HostState>>],
    available: f64,
    overflow: HostOverflowMode,
    core_row_span: usize,
    layout: &CockpitLayout,
) -> Value {
    let remote: Vec<Value> = hosts
        .iter()
        .map(|host| view_for(&host.lock().expect("host state poisoned")))
        .collect();
    let remote_count = remote.len();
    // Local first, matching `HostsPanel.hosts` in Swift (`[local] + remoteHosts`).
    // This machine is the one you are looking at; it leads.
    let cards: Vec<Value> = local.into_iter().chain(remote).collect();
    cockpit_payload(
        cards,
        remote_count,
        available,
        overflow,
        core_row_span,
        layout,
    )
}

/// The name a tab wears, from an already-rendered card.
///
/// A card that never connected carries its host name under `error` rather than
/// at the top level (`pending_card`), and a tab labelled from the wrong key
/// would be blank for exactly the host you most need to find.
fn card_host_name(card: &Value) -> &str {
    card["hostName"]
        .as_str()
        .or_else(|| card["error"]["hostName"].as_str())
        .unwrap_or_default()
}

/// The tab bar, when the cards collapse into one — otherwise `null`.
///
/// One tab per card, in payload order, so the local card leads the bar exactly
/// as it leads the grid. The labels are the cards' own host names rather than
/// anything re-derived here, and `minHeight` travels with them because the
/// container's floor is `HostsPanel`'s decision, not a CSS guess.
fn host_tab_bar(cards: &[Value], columns: usize, overflow: HostOverflowMode) -> Value {
    let prefers_tabs = overflow == HostOverflowMode::Tabs;
    if !viewmodel::cockpit::host_tabs(columns, cards.len(), prefers_tabs) {
        return Value::Null;
    }
    json!({
        "minHeight": viewmodel::cockpit::HOST_TABS_MIN_HEIGHT,
        "tabs": cards
            .iter()
            .map(|card| {
                // A tab bar shows one card and hides the rest, so a host that
                // went down while you were looking at another one is invisible
                // — which is exactly how ubu-3xdv stayed unnoticed through the
                // 2026-08-06 outage. The alarm therefore has to live on the
                // tab, the only thing on screen that represents a hidden host.
                let state = card["connection"]["state"].as_str().unwrap_or_default();
                let down = matches!(state, "unreachable" | "failed");
                json!({
                    "id": card["id"],
                    "label": card_host_name(card),
                    // Colour and alarm are decided here like every other
                    // colour: the frontend paints `alert`, it does not derive
                    // it from a state string it would have to know the meaning
                    // of.
                    "color": down.then(|| color::hex(color::RED)),
                    "alert": down,
                })
            })
            .collect::<Vec<_>>(),
    })
}

/// Most volumes any card reports — the number of volume slots every card
/// reserves, so the Volumes block (and the `TOP CPU`/`TOP RAM` row beneath it)
/// line up across cards sharing a grid row.
///
/// Slots rather than a pixel height on purpose: cards in a row are all
/// `minmax(0, 1fr)`, so an equal *tile count* at an equal width yields the same
/// column count, the same row count, and therefore the same height — matched by
/// construction, with no constant to drift out of step with the CSS.
///
/// `0` when the cards are not side by side. A single column stacks them, and
/// [`viewmodel::cockpit::host_tabs`] can only fire at `columns <= 1`, so this
/// one test covers the tabs case too; reserving in either would pad a short
/// card with dead space for an alignment nobody can see.
fn volume_slots(cards: &[Value], columns: usize) -> usize {
    if columns < 2 {
        return 0;
    }
    cards
        .iter()
        .map(|card| card["volumes"].as_array().map_or(0, Vec::len))
        .max()
        .unwrap_or(0)
}

/// Rewrites every card's core ladder so the blocks line up across a row.
///
/// [`host_card`] sees one host, so the ladder it emits is that host's own
/// answer — correct alone, and wrong beside a neighbour with a different core
/// count: at a 900pt-class card a 36-core host takes 6 rows (334pt) where a
/// 16-core host takes 4 (220pt), and every section below the block inherits the
/// skew. `viewmodel::layout::aligned_core_ladders` gives each count its own
/// columns but everyone's height, and this is the only place that can call it,
/// because it is the only place that has seen every card.
///
/// Two things happen here, and only one of them is about neighbours:
/// * **the row span** is applied always — it is the user's
///   `coreRowSpan` preference, which `host_card` cannot read;
/// * **the cross-card max** only when `columns >= 2`. A card alone in its row
///   aligns with itself, exactly as `volume_slots` reserves nothing there.
fn align_core_ladders(cards: &mut [Value], columns: usize, row_span: usize) {
    use viewmodel::layout::{
        aligned_core_ladders, core_block_height, CORE_GAP, CORE_MAX_COLUMNS, CORE_MIN_CELL,
    };

    let core_count = |card: &Value| card["cores"].as_array().map_or(0, Vec::len);
    let all: Vec<usize> = cards.iter().map(core_count).collect();

    for card in cards.iter_mut() {
        let n = core_count(card);
        // The base height the CSS falls back to when no rung matches — an
        // error card, which never gets a `data-n` to key a rule off.
        card["coreBlockHeight"] = json!(core_block_height(row_span));
        if n == 0 {
            continue;
        }
        let set: Vec<usize> = if columns >= 2 { all.clone() } else { vec![n] };
        let Some((_, rungs)) =
            aligned_core_ladders(&set, row_span, CORE_MIN_CELL, CORE_GAP, CORE_MAX_COLUMNS)
                .into_iter()
                .find(|(count, _)| *count == n)
        else {
            continue;
        };
        card["coreLadder"] = Value::Array(
            rungs
                .into_iter()
                .map(|r| json!({ "minWidth": r.min_width, "cols": r.cols, "height": r.height }))
                .collect(),
        );
    }
}

/// The payload shape, over already-rendered cards. Split from
/// [`cockpit_view`] so the tests can drive it without locks.
///
/// `remote_count` is deliberately separate from `cards.len()`: the local card is
/// always there, so "is anything configured" is a question about *monitored*
/// hosts and counting cards would answer it wrong forever.
fn cockpit_payload(
    cards: Vec<Value>,
    remote_count: usize,
    available: f64,
    overflow: HostOverflowMode,
    core_row_span: usize,
    layout: &CockpitLayout,
) -> Value {
    let mut cards = cards;
    let columns = host_columns(available, cards.len(), HOST_CARD_MIN_WIDTH, SPACING);
    let tabs = host_tab_bar(&cards, columns, overflow);
    align_core_ladders(&mut cards, columns, core_row_span);
    // A cockpit with no monitored host says so in words made here, like every
    // other string the frontend paints -- the local card alone would read as a
    // finished setup rather than an untouched one.
    let empty = if remote_count == 0 {
        json!({ "message": "No hosts configured. Add one in Settings." })
    } else {
        Value::Null
    };
    json!({
        "hosts": cards,
        "hostColumns": columns,
        "hostCardMinWidth": HOST_CARD_MIN_WIDTH,
        "spacing": SPACING,
        // Null unless the cards actually collapse. A tab bar the frontend has
        // to decide the visibility of would be `host_tabs` re-implemented in
        // JS, free to disagree with the tested one -- the same argument
        // `hostColumns` and `panelRows` are here for.
        "hostTabs": tabs,
        // How many volume tiles each card reserves. Cross-card, so it belongs
        // here beside `hostColumns` rather than inside a card that can only see
        // itself.
        "volumeSlots": volume_slots(&cards, columns),
        "panels": panel_table(),
        "panelRows": panel_rows(layout, available),
        "empty": empty,
        // The Settings surface is opened from the cockpit, so its button's
        // label has to arrive before anything has asked for the settings
        // payload. Same rule as every other string on the page: made in Rust,
        // and in exactly one place -- `settings::OPEN_LABEL` is what the
        // Settings view itself renders too.
        "settingsLabel": settings::OPEN_LABEL,
    })
}

/// One arrangement, reflowed for `available` — which row each panel sits in,
/// how wide its span makes it, and the content columns that width affords.
///
/// The *arrangement* is the user's (`settings::effective_layout`, which is
/// `CockpitLayout::hosts_forward()` until someone edits the Layout tab), the
/// *packing* is `viewmodel::cockpit::reflow` and the *tracks* are
/// `viewmodel::cockpit::panel_widths`, all three already tested there. It travels
/// as data for exactly the reason `hostColumns` does: a CSS `auto-fit` over the
/// panels would be a second implementation of `PanelKind::min_width`, free to
/// disagree with the tested one — and the case that matters (OpenClaw + Usage
/// still sharing a row at a width where Repos + Runners must split) is precisely
/// the case a global breakpoint tier gets wrong.
///
/// Rows the frontend has no section for (`hosts`, which it renders above this
/// block) still travel: a row silently dropped here would be indistinguishable
/// from one this function never produced.
fn panel_rows(layout: &CockpitLayout, available: f64) -> Value {
    Value::Array(
        viewmodel::cockpit::reflow(&layout.rows, available, SPACING)
            .into_iter()
            .map(|row| {
                // Per panel, not per row: the tracks of one row differ once its
                // spans do.
                let widths = viewmodel::cockpit::panel_widths(&row, available, SPACING);
                Value::Array(
                    row.iter()
                        .zip(widths)
                        .map(|(placement, width)| {
                            json!({
                                "id": placement.kind.id(),
                                "title": placement.kind.title(),
                                "minWidth": placement.kind.min_width(),
                                "span": placement.span.as_str(),
                                // The row's track sizes, as the `fr` numbers the
                                // frontend paints: a name alone would make it
                                // re-derive "how much is a quarter" in CSS.
                                "weight": placement.span.weight(),
                                // The width this panel actually gets, and the
                                // content columns that fit in it. Both travel
                                // for the reason `hostColumns` does: the answer
                                // plus the input it was derived from, so the
                                // frontend applies a decision it cannot
                                // re-derive differently.
                                "width": width,
                                "columns": viewmodel::cockpit::panel_columns(
                                    placement.kind,
                                    width,
                                    SPACING,
                                ),
                            })
                        })
                        .collect(),
                )
            })
            .collect(),
    )
}

/// The write side of the poll loop, pulled out of the spawned task for the
/// same reason `view_for` was pulled out of the `#[tauri::command]`: a plain
/// function over `&mut HostState` is testable without a runtime, a mutex or a
/// live agent.
///
/// The invariant it carries is that a failure moves `error` and NOTHING else.
/// `latest` and `last_success` are the record of the last thing we actually
/// heard, so clearing either on failure would either blank a card that still
/// has real (if old) data, or downgrade a dated "last update 4m ago" badge to
/// "last update unknown" — and it would do so silently, since one failed poll
/// looks identical to a hundred from inside the loop.
///
/// A failure also only *publishes* once [`FAILURE_THRESHOLD`] consecutive
/// polls have failed, mirroring the Swift service: the streak is counted from
/// the first failure, but the card keeps its previous badge until the streak
/// is long enough to mean something.
fn record_poll(s: &mut HostState, result: Result<wire::Snapshot, AgentError>, at: Instant) {
    match result {
        Ok(snap) => {
            s.histories.record(&snap);
            s.latest = Some(snap);
            s.error = None;
            s.last_success = Some(at);
            s.consecutive_failures = 0;
        }
        Err(e) => {
            s.consecutive_failures = s.consecutive_failures.saturating_add(1);
            if s.consecutive_failures >= FAILURE_THRESHOLD {
                s.error = Some(e.user_message().into_owned());
            }
        }
    }
}

/// The write side of the health poll, and the whole of its failure policy.
///
/// **A failed health poll must change nothing.** This is the negative that
/// matters more than the feature: `/v1/health` is a *second* request to a host
/// the snapshot loop is already polling, and if a failure here could publish an
/// error, bump the failure streak, or clear the flag it last learned, then a
/// probe nobody asked for would have gained the power to redden a card whose
/// data is arriving perfectly. That is a new failure mode traded for a
/// diagnostic — the opposite of the trade #182 asked for. So the `Err` arm is
/// not written: the signal is simply withheld, and the card goes on saying
/// whatever the snapshot poll (and the last health poll that did land) support.
///
/// Withheld, note, is not *reset*. A sampler known to be stalled keeps its badge
/// through a health poll that fails, because a request we could not make is not
/// evidence the sampler recovered — and the one thing this must never do is put
/// a green dot back over frozen numbers. Recovery arrives the same way the
/// stall did: a health poll that succeeds and says `samplerStale: false`.
fn record_health(s: &mut HostState, result: Result<wire::Health, AgentError>) {
    if let Ok(info) = result {
        // Both fields, together, from the same payload — including their
        // `None`s. An agent that stopped reporting them (a rollback to a
        // pre-#35 build) is telling us it no longer knows, and keeping the
        // previous answer would date a badge from a build that is gone.
        s.sampler_stale = info.sampler_stale;
        s.sample_age_seconds = info.sample_age_seconds;
    }
}

/// One pass of the health poll: every host with a token, concurrently.
///
/// Its own loop over the poll set rather than a branch of [`poll_loop`], for the
/// reason the containers loop is one too — but here it is also a latency
/// argument. Folded into the 1s task, a health request would sit in front of a
/// snapshot on the same tick, and the client's 5s timeout would let one
/// unreachable agent stretch its *own* card's chart axis, where one history
/// sample is one fixed time slice.
async fn poll_health(app: &Arc<App>) {
    // The lock is released before anything is awaited, exactly as
    // `poll_containers` does it: a hung agent must not hold the poll set.
    let targets: Vec<(Arc<Mutex<HostState>>, Arc<AgentClient>)> = {
        let hosts = app.hosts.lock().expect("poll set poisoned");
        hosts
            .iter()
            // A host with no token never reaches the network for a snapshot;
            // probing its health would be one guaranteed 401 per tick, and the
            // card already names the real cause.
            .filter(|polled| polled.token_available)
            .map(|polled| (Arc::clone(&polled.state), Arc::clone(&polled.client)))
            .collect()
    };

    let mut requests = Vec::with_capacity(targets.len());
    for (state, client) in targets {
        requests.push(tokio::spawn(async move {
            let result = client.health().await;
            // Taken after the await, never across it.
            record_health(&mut state.lock().expect("host state poisoned"), result);
        }));
    }
    for request in requests {
        // A panicked task is the only thing `JoinHandle` can report here —
        // `record_health` swallows the network failure by design — and it must
        // not take the loop down with it.
        let _ = request.await;
    }
}

/// The health poll's loop: tick, probe every host, record.
async fn health_loop(app: Arc<App>) {
    let mut tick = tokio::time::interval(HEALTH_POLL_INTERVAL);
    // Same reason as every other loop here: `Burst` would fire every missed
    // tick back-to-back after one slow pass.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        poll_health(&app).await;
    }
}

/// The card's line when nothing is stored for this host.
///
/// Distinct from a *wrong* token, which the agent itself rejects with a 401
/// (`AgentError::AuthFailed`). Reusing that message would send the operator to
/// check the wrong layer.
const MISSING_HOST_TOKEN_MESSAGE: &str =
    "No agent token configured for this host. Add one in Settings.";

/// The card's line when the credential store would not hand the token over.
///
/// The same failure [`CREDENTIAL_UNREADABLE_MESSAGE`] names for the panels,
/// worded for a host card: it lands in the error slot beside "Couldn't reach
/// the agent.", which is a sentence, and it must not be confused with the
/// missing-token line above — that one asserts a fact about this host's
/// configuration, and a locked keychain is evidence of nothing of the sort.
const HOST_TOKEN_UNREADABLE_MESSAGE: &str =
    "Couldn't read the credential store for this host's token.";

/// A host whose token could not be obtained — either because there is none, or
/// because the store would not say. Not debounced like a failed poll: neither
/// cause leaves this process to flap, so there is no momentary drop to absorb,
/// and the operator should see the cause on the first tick.
fn record_token_unavailable(s: &mut HostState, message: &str) {
    s.consecutive_failures = FAILURE_THRESHOLD;
    s.error = Some(message.to_string());
}

/// One host's poll loop: tick, poll, record. Lifted out of the spawn site so
/// [`spawn_host`] reads as "make the state, start the loop" and the loop's own
/// rules (the tick behaviour, the missing-token short-circuit) sit together.
///
/// No lock is ever held across the `await`, which is what lets a slow poll on
/// one host leave every other host — and the `cockpit` command — untouched.
async fn poll_loop(
    state: Arc<Mutex<HostState>>,
    client: Arc<AgentClient>,
    blocked: Option<&'static str>,
) {
    let mut tick = tokio::time::interval(POLL_INTERVAL);
    // `interval`'s default (`Burst`) fires every missed tick back-to-back the
    // moment a slow poll releases the executor -- and the client timeout (5s,
    // agentclient) exceeds this period, so one slow poll can trigger several
    // polls in a row. The charts equate one history sample with one fixed time
    // slice (PX_PER_SAMPLE), so a burst silently compresses the time axis
    // instead of just running late. `Delay` waits a full period from
    // completion instead, so a slow poll shifts later polls rather than
    // bunching them.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        // No token, no request: an empty `Authorization` header buys one
        // guaranteed 401 per tick, and the card would report it as a rejected
        // token — the agent's layer, not this one.
        if let Some(message) = blocked {
            record_token_unavailable(&mut state.lock().expect("host state poisoned"), message);
            continue;
        }
        let result = client.snapshot().await;
        let at = Instant::now();
        record_poll(&mut state.lock().expect("host state poisoned"), result, at);
    }
}

/// Notices the machine waking from sleep and refreshes everything at once.
///
/// The five `tokio::time::interval` loops need no help: hosts and the local
/// card tick at 1s, containers and health at 10s, and none of them is reachable
/// from here anyway — `poll_loop` is spawned without an `Arc<App>`. What this
/// exists for is the four *slow* loops, which would otherwise show the previous
/// night's data for up to a full interval after the lid opens.
///
/// **This deliberately breaks the rule the wake channels were built for.**
/// `app/README.md` records that every mutation wakes exactly the loop its data
/// feeds and no other, because a wake spends a whole poll pass. A resume is not
/// an edit to one source: it is every source at once becoming untrustworthy, so
/// it is the one caller entitled to fire all four.
async fn resume_loop(app: Arc<App>) {
    let mut tick = tokio::time::interval(resume::TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick completes immediately, so take the baseline from it rather
    // than from before the loop — otherwise the app's own startup work would
    // read as a gap on the very first comparison.
    tick.tick().await;
    let mut last = resume::Sample::now();

    loop {
        tick.tick().await;
        let now = resume::Sample::now();
        if !now.resumed_since(last) {
            last = now;
            continue;
        }
        last = now;

        eprintln!("resume detected: refreshing every source");
        // The host watch is re-seeded *before* the wakes, so a reconnect that
        // fails on its first attempt cannot slip a banner through in between.
        app.host_reachability
            .lock()
            .expect("host watch poisoned")
            .reset();

        wake_github(&app);
        wake_usage(&app, true);
        app.azure_wake.notify_one();
        wake_openclaw(&app);
    }
}

/// Watches every monitored host's reachability and banners the changes.
///
/// A loop of its own rather than a hook inside [`poll_loop`], because that task
/// is spawned per host with no `Arc<App>` and no way to reach the notification
/// handle — and because the answer is cross-host by nature: one place that sees
/// every host is also the place that can forget one the moment Settings removes
/// it.
///
/// Reads the same `error` field the card renders from, so a banner and a red
/// card can never disagree about whether a host is down. That includes the
/// debounce: [`FAILURE_THRESHOLD`] consecutive failures before `error` is set,
/// so a single dropped packet on a flappy tailnet never reaches the OS.
async fn hosts_watch_loop(app: Arc<App>) {
    let mut tick = tokio::time::interval(POLL_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;

        let enabled = {
            let store = app.store.lock().expect("store poisoned");
            store.settings().notify_on_service_change
        };

        // Snapshotted into owned values first: `observe` borrows them, and the
        // hosts lock must not be held while a banner leaves the process.
        let readings: Vec<(String, String, Option<services::Reachability>)> = {
            let hosts = app.hosts.lock().expect("hosts poisoned");
            hosts
                .iter()
                .map(|h| {
                    let s = h.state.lock().expect("host state poisoned");
                    // `None` until the first poll settles either way — a host
                    // whose task has ticked but never completed is not yet a
                    // verdict, and seeding on one would banner the launch.
                    let state = match (&s.latest, &s.error) {
                        (_, Some(_)) => Some(services::Reachability::Unreachable),
                        (Some(_), None) => Some(services::Reachability::Reachable),
                        (None, None) => None,
                    };
                    (s.id.clone(), s.name.clone(), state)
                })
                .collect()
        };

        let notices = {
            let borrowed: Vec<services::HostReading<'_>> = readings
                .iter()
                .map(|(id, name, state)| services::HostReading {
                    id,
                    name,
                    state: *state,
                })
                .collect();
            let mut watch = app.host_reachability.lock().expect("host watch poisoned");
            watch.observe(&borrowed, enabled)
        };

        deliver_status_notices(&app, &notices);
    }
}

/// Which existing poll task each desired host should keep, or `None` when it
/// needs a new one. Existing indices that appear nowhere in the result are the
/// ones whose task must be aborted.
///
/// Pulled out as a pure function over keys because this is the whole of
/// "settings apply without a restart": adding host B must not restart host A's
/// task, because restarting it throws away the sparkline history that makes
/// the card worth looking at. A reload that just rebuilt the list would pass
/// every test that only checks *which* hosts are polled.
fn reconcile(existing: &[HostKey], desired: &[HostKey]) -> Vec<Option<usize>> {
    let mut taken = vec![false; existing.len()];
    let mut plan = Vec::with_capacity(desired.len());
    for want in desired {
        let found = existing
            .iter()
            .enumerate()
            .find(|(index, have)| !taken[*index] && *have == want)
            .map(|(index, _)| index);
        if let Some(index) = found {
            taken[index] = true;
        }
        plan.push(found);
    }
    plan
}

/// Starts polling one host: fresh state, its token read from the credential
/// store, its own task.
fn spawn_host(app: &App, key: HostKey, name: String) -> PolledHost {
    let state = Arc::new(Mutex::new(HostState {
        id: key.id.to_string(),
        name: name.clone(),
        histories: HostHistories::new(),
        latest: None,
        error: None,
        last_success: None,
        consecutive_failures: 0,
        // Unknown until the first health poll lands — never `Some(false)`,
        // which would be this process asserting a fact about an agent it has
        // not spoken to yet.
        sampler_stale: None,
        sample_age_seconds: None,
    }));

    let (token, blocked) = match host_token(&*app.credentials, key.id) {
        HostToken::Ready(token) => (token, None),
        HostToken::Blocked(message) => (String::new(), Some(message)),
    };
    let token_available = blocked.is_none();
    // Immutable, so it is shared rather than guarded: the snapshot loop and
    // the containers loop both poll this host through this one client.
    let client = Arc::new(AgentClient::new(key.base_url.clone(), token));

    let task_state = Arc::clone(&state);
    let task_client = Arc::clone(&client);
    let task = app
        .runtime
        .spawn(async move { poll_loop(task_state, task_client, blocked).await });
    PolledHost {
        key,
        state,
        client,
        token_available,
        task,
    }
}

/// One host's bearer token, or the line its card carries in place of polling.
///
/// The smallest shape the poll loop needs — it either has a token or it has a
/// sentence — and the smallest that still keeps the two blocked cases apart,
/// which is the whole point: "you never configured this host" and "the keychain
/// would not answer" send the operator to different places, and only one of
/// them is something this process actually learned.
enum HostToken {
    Ready(String),
    Blocked(&'static str),
}

fn host_token<C: CredentialStore + ?Sized>(credentials: &C, id: Uuid) -> HostToken {
    match read_credential(credentials, SecretKey::HostToken(id)) {
        Credential::Present(token) => HostToken::Ready(token),
        Credential::Absent => HostToken::Blocked(MISSING_HOST_TOKEN_MESSAGE),
        Credential::Unreadable => HostToken::Blocked(HOST_TOKEN_UNREADABLE_MESSAGE),
    }
}

/// Rebuilds the poll set from the store, keeping every task whose host is
/// unchanged.
///
/// This is what "takes effect without a restart" means in practice, and it is
/// deliberately *not* a teardown-and-rebuild: an unrelated edit (adding a
/// host, renaming one, toggling another off) must leave every surviving host's
/// history, failure streak and last-success time exactly where they were.
/// Mirrors `RemoteHostsCoordinator.reload()` in Swift.
fn reload_hosts(app: &App) {
    let desired: Vec<(HostKey, String)> = {
        let store = app.store.lock().expect("store poisoned");
        display_order(store.hosts())
            .into_iter()
            .map(|host| {
                (
                    HostKey {
                        id: host.id,
                        base_url: host.base_url(),
                    },
                    host.name.clone(),
                )
            })
            .collect()
    };

    let mut hosts = app.hosts.lock().expect("poll set poisoned");
    let existing: Vec<HostKey> = hosts.iter().map(|polled| polled.key.clone()).collect();
    let wanted: Vec<HostKey> = desired.iter().map(|(key, _)| key.clone()).collect();
    let plan = reconcile(&existing, &wanted);

    let mut slots: Vec<Option<PolledHost>> = hosts.drain(..).map(Some).collect();
    let mut next = Vec::with_capacity(desired.len());
    for ((key, name), keep) in desired.into_iter().zip(plan) {
        match keep.and_then(|index| slots[index].take()) {
            Some(polled) => {
                // A rename has to reach the live card, and it must do so
                // without costing the card its history.
                polled.state.lock().expect("host state poisoned").name = name;
                next.push(polled);
            }
            None => next.push(spawn_host(app, key, name)),
        }
    }
    // Whatever nobody claimed is a host that left the cockpit.
    for leftover in slots.into_iter().flatten() {
        leftover.task.abort();
    }
    *hosts = next;
}

// MARK: containers
//
// Its own loop rather than a branch of the per-host one: the containers view
// is a *panel*, polled on the panel's cadence (10s), and it has a source no
// host task could own — this machine's own docker/podman/tart.

/// One pass over every container source: this machine's runtimes, then each
/// reachable host's agent.
///
/// The three locks it needs are taken **one at a time, in sequence** (poll set,
/// then store, then containers state) and never nested, so there is no lock
/// order for a future caller to get wrong.
async fn poll_containers(app: &Arc<App>) {
    let now = containers::now_unix();

    // Spawning `docker ps` blocks; keep it off the async executor.
    let last_known = app
        .containers
        .lock()
        .expect("containers state poisoned")
        .last_known();
    let local = tokio::task::spawn_blocking(containers::local::poll)
        .await
        .map(|poll| poll.merge_with(last_known))
        .map_err(|e| eprintln!("local container discovery failed: {e}"))
        .ok();

    // One in-flight request per host, so a hung agent delays only itself: the
    // client's own 5s timeout is shorter than the 10s cadence, but two
    // sequential timeouts would not be.
    let (configured, targets) = {
        let hosts = app.hosts.lock().expect("poll set poisoned");
        let mut configured = std::collections::BTreeSet::new();
        let mut targets = Vec::new();
        for polled in hosts.iter() {
            let name = polled
                .state
                .lock()
                .expect("host state poisoned")
                .name
                .clone();
            configured.insert(name.clone());
            if polled.token_available {
                targets.push((name, Arc::clone(&polled.client)));
            }
        }
        (configured, targets)
    };

    let mut requests = Vec::with_capacity(targets.len());
    for (name, client) in targets {
        requests.push(tokio::spawn(
            async move { (name, client.containers().await) },
        ));
    }
    let mut fetched: Vec<(String, Vec<wire::Container>)> = Vec::new();
    for request in requests {
        // A failed fetch is deliberately silent here and leaves the host's
        // previous rows in place (`RemoteHostsCoordinator`, Swift): the panel
        // must not blank a section because one poll missed, and an unreachable
        // host already reports itself on its own card.
        if let Ok((name, Ok(list))) = request.await {
            fetched.push((name, list));
        }
    }

    // Presence is the only thing that touches the store, and it writes only
    // when a poll actually learned something.
    let clocks = {
        let mut store = app.store.lock().expect("store poisoned");
        let rules = store.container_rules().to_vec();
        let mut records = store.container_presence().clone();
        let mut clocks: Vec<String> = Vec::new();

        if let Some((_, outcome)) = local.as_ref() {
            let succeeded: std::collections::BTreeSet<&str> =
                outcome.succeeded.iter().map(|r| r.id()).collect();
            let noted = containers::presence::note(
                &mut records,
                store::LOCAL_HOST_SCOPE,
                &outcome.merged,
                Some(&succeeded),
                &rules,
                now,
            );
            if noted.clock_advances {
                clocks.push(store::LOCAL_HOST_SCOPE.to_owned());
            }
        }
        for (name, list) in &fetched {
            // `None`: a remote fetch is all-or-nothing, so reaching here at
            // all means this host reported.
            let noted = containers::presence::note(&mut records, name, list, None, &rules, now);
            if noted.clock_advances {
                clocks.push(name.clone());
            }
        }

        if store.set_container_presence(records) {
            if let Err(e) = store.save() {
                eprintln!("could not persist container presence: {e}");
            }
        }
        clocks
    };

    let mut state = app.containers.lock().expect("containers state poisoned");
    if let Some((detected, outcome)) = local {
        state.apply_local(detected, outcome, now);
    }
    for (name, list) in fetched {
        state.apply_remote(name, list);
    }
    // After applying, so a host removed mid-poll cannot leave a ghost section.
    state.retain_hosts(&configured);
    for host in clocks {
        state.advance_clock(&host, now);
    }
}

/// The containers panel's poll loop: tick, poll every source, record.
async fn containers_loop(app: Arc<App>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(
        containers::POLL_INTERVAL_SECS,
    ));
    // Same reason as the metrics loop: `Burst` would fire every missed tick
    // back-to-back after one slow pass, turning a 10s cadence into a spawn
    // storm of `docker ps` invocations.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        poll_containers(&app).await;
    }
}

/// Guards the containers surface's counterpart to [`FIRST_REQUEST`].
static FIRST_CONTAINERS_REQUEST: std::sync::Once = std::sync::Once::new();

#[tauri::command]
fn containers(state: tauri::State<'_, Arc<App>>) -> Value {
    // Rules and presence first, then the panel state — one lock at a time, in
    // the same sequence `poll_containers` uses.
    let (rules, presence) = {
        let store = state.store.lock().expect("store poisoned");
        (
            store.container_rules().to_vec(),
            store.container_presence().clone(),
        )
    };
    let payload = {
        let panel = state.containers.lock().expect("containers state poisoned");
        containers::view(&panel, &rules, &presence, containers::now_unix())
    };
    // The same terminal-side signal `cockpit` and `settings_view` print, and
    // for the same reason: this is a third command on an IPC boundary with no
    // automated coverage (#123), and every way it can break from outside Rust
    // looks identical from in here — this function never runs.
    FIRST_CONTAINERS_REQUEST.call_once(|| {
        let sections = payload["sections"].as_array().map_or(0, Vec::len);
        eprintln!("containers: first frontend request ({sections} section(s))");
    });
    payload
}

// MARK: the Repos + GitHub Runners panels
//
// One loop for both, because they share a credential and a cadence: the token
// that authenticates one authenticates the other, and a single pass fills both.
// This is the shell's **first consumer of `refresh_interval_secs`** — until
// now that preference persisted and nothing read it.

/// Applies one GitHub credential read to the panel state, returning the token
/// the pass should go on to poll with. `None` ends the pass.
///
/// A function of its own, over `&mut GitHubState`, because the whole of this
/// decision is which of three states two panels enter — and every one of them
/// is testable here without a token, a keychain or a network.
fn github_token(credential: Credential, state: &mut GitHubState) -> Option<String> {
    match credential {
        // Recorded here, before a single request goes out. This arm used to
        // return the token and write nothing, so the panels went on claiming
        // there was no credential for the whole of the pass that was holding
        // one — several seconds of "connect a GitHub token in Settings" on
        // every launch, at a machine where the token was fine.
        Credential::Present(token) => {
            state.apply_token_present();
            Some(token)
        }
        // The only branch that may claim nobody configured this: we asked, and
        // the store said there is nothing stored.
        Credential::Absent => {
            state.apply_unauthenticated();
            None
        }
        Credential::Unreadable => {
            state.apply_credential_unreadable(CREDENTIAL_UNREADABLE_MESSAGE);
            None
        }
    }
}

/// One read of GitHub's public statuspage, folded into the panel state.
///
/// A failure is recorded but does **not** drop the last good reading: GitHub's
/// status does not change on the timescale of one dropped request, and a panel
/// that flipped from "it's GitHub" back to a red "it's us" on a single timeout
/// would be the exact misdirection this verdict exists to prevent.
async fn poll_service_status(app: &Arc<App>) {
    // Concurrently: five independent hosts, and running them in sequence would
    // make the pass as slow as their sum for no reason. Each carries its own
    // 8s timeout, so the worst case is one timeout rather than five.
    let results = futures_util::future::join_all(
        services::ServiceId::ALL
            .iter()
            .map(|&service| async move { (service, services::read(service).await) }),
    )
    .await;

    // Re-read like the token, so switching the preference off applies on the
    // next pass rather than on the next launch.
    let notify_changes = {
        let store = app.store.lock().expect("store poisoned");
        store.settings().notify_on_service_change
    };

    let notices = {
        let mut statuses = app.services.lock().expect("services poisoned");
        for (service, result) in results {
            match result {
                Ok(status) => {
                    // GitHub's reading also feeds the Repos/Runners conjunction
                    // chip, which reads it from `GitHubState`.
                    if service == services::ServiceId::GitHub {
                        app.github
                            .lock()
                            .expect("github state poisoned")
                            .apply_service_status(status.clone());
                    }
                    statuses.succeeded(service, status);
                }
                Err(e) => {
                    if service == services::ServiceId::GitHub {
                        app.github
                            .lock()
                            .expect("github state poisoned")
                            .apply_service_status_error(e.user_message());
                    }
                    statuses.failed(service, e.user_message());
                }
            }
        }
        // Diffed against what this pass actually obtained. A failed refresh
        // keeps the last good status for *rendering*, and treating that
        // retained value as a fresh observation would make an unreachable page
        // look like a steady state forever — so `readings()` reports `None`
        // wherever the last read failed.
        let mut watch = app.service_status.lock().expect("status watch poisoned");
        watch.observe(&statuses.readings(), notify_changes)
    };

    // Last, and outside every lock: showing a banner is the one thing in this
    // function that leaves the process.
    deliver_status_notices(app, &notices);
}

/// One pass over every GitHub source: each enabled repo's health, this
/// machine's git checkouts, and the org's self-hosted runners.
///
/// No lock is ever held across an `await`; each is taken, used and dropped, in
/// sequence, exactly as [`poll_containers`] does.
async fn poll_github(app: &Arc<App>) {
    // GitHub's own availability first, and deliberately **before** the token
    // gate below. The statuspage needs no credential, and "GitHub Actions is in
    // a major outage" is most useful precisely when this panel is otherwise
    // blank — an unauthenticated cockpit that returned early here would be the
    // one that could not explain itself at all.
    poll_service_status(app).await;

    // Re-read every pass rather than captured at startup: that is what makes a
    // Save or Clear in Settings apply without a relaunch.
    let credential = read_credential(&*app.credentials, SecretKey::GitHubAccessToken);
    let token = {
        let mut state = app.github.lock().expect("github state poisoned");
        github_token(credential, &mut state)
    };
    let Some(token) = token else { return };

    // Re-read every pass alongside the token, and for the same reason: a Save
    // in Settings must apply on the next pass rather than the next launch.
    // Two blocks, not one — taking the store and panel-state locks together
    // here would be the only place in this pass that holds both at once.
    let org = {
        let store = app.store.lock().expect("store poisoned");
        store.settings().github_org.trim().to_owned()
    };
    {
        let mut state = app.github.lock().expect("github state poisoned");
        state.apply_org(&org);
    }

    let repos: Vec<(String, Option<Vec<String>>)> = {
        let store = app.store.lock().expect("store poisoned");
        store
            .repos()
            .iter()
            .filter(|repo| repo.enabled)
            .map(|repo| (repo.slug.clone(), repo.watched_workflows.clone()))
            .collect()
    };
    let roster = {
        let store = app.store.lock().expect("store poisoned");
        github::roster_from_records(store.runner_roster())
    };

    // Walking `~/Repos` and spawning `git` twice per repo blocks; keep it off
    // the async executor, exactly as `docker ps` is.
    let local = tokio::task::spawn_blocking(|| {
        github::git::scan(&github::git::default_roots(), github::git::MAX_DEPTH)
    })
    .await
    .map_err(|e| eprintln!("local git scan failed: {e}"))
    .ok();

    let client = github::GitHubClient::new(token);
    let now = github::now_utc();
    // Sequential per repo, matching `GHWorkflowsService.refresh()`: each
    // `repo_health` already fires its three side counts concurrently, so a
    // six-repo portfolio is 24 requests either way — doing them all at once
    // would only spend the rate-limit budget faster.
    let mut health = Vec::with_capacity(repos.len());
    for (slug, watched) in &repos {
        health.push(client.repo_health(slug, watched.as_deref(), now).await);
    }
    // The roster is only ever advanced by this call, and it only returns `Ok`
    // on a successful fetch — so a failing GitHub leaves every absence clock
    // frozen at the last successful poll instead of ageing a healthy runner
    // into a red alarm.
    // `None` when no org is configured, rather than a fetch that fails: the
    // request would be `GET /orgs//actions/runners`, whose 404 would surface in
    // the footer as "GitHub is unreachable" when the truth is a settings field
    // nobody has filled in. The panel already names that state; this keeps a
    // fabricated transport error from talking over it.
    //
    // The repos half of the pass continues either way — repos are tracked by
    // full `owner/name` slug and need no organization.
    let update = if org.is_empty() {
        None
    } else {
        Some(
            client
                .runner_roster(&org, &roster, now, github::RUNNER_GRACE_SECS)
                .await,
        )
    };

    // Re-read like the token, so switching the preference off applies on the
    // next pass rather than on the next launch.
    let notify_approvals = {
        let store = app.store.lock().expect("store poisoned");
        store.settings().notify_on_approval_needed
    };
    // Diffed before `health` is handed to the panel state, because the diff
    // needs the *previous* pass and `apply_repos` replaces it wholesale. Its
    // own lock: this is delivery memory, not anything either panel renders.
    let notices = {
        let mut watch = app.approvals.lock().expect("approval watch poisoned");
        watch.observe(&health, notify_approvals)
    };

    {
        let mut state = app.github.lock().expect("github state poisoned");
        state.apply_repos(health);
        if let Some(local) = local {
            state.apply_local(local);
        }
        // `None` is the no-org case, and it applies nothing on purpose: the
        // panel's own setup line is the whole of what there is to say, and
        // an error here would bury it.
        if let Some(update) = &update {
            match update {
                Ok(update) => state.apply_runners(update, panel::now_unix()),
                // The transport error is logged, not shown: a 403 for a missing
                // scope is what this almost always is, and "HTTP 403" sends the
                // operator to check the network instead of the PAT.
                Err(e) => {
                    eprintln!("org runners fetch failed: {e}");
                    state.apply_runners_error(github::RUNNERS_ERROR_MESSAGE);
                }
            }
        }
    }

    // Persisted only on success, and only when it actually changed — a steady
    // org produces an identical roster poll after poll, and rewriting the store
    // file every minute for no change is a write nobody asked for.
    if let Some(Ok(update)) = &update {
        let mut store = app.store.lock().expect("store poisoned");
        if store.set_runner_roster(github::roster_to_records(&update.roster)) {
            if let Err(e) = store.save() {
                eprintln!("could not persist the runner roster: {e}");
            }
        }
    }

    // Last, and outside every lock: showing a banner is the one thing in this
    // pass that leaves the process.
    deliver_approval_notices(app, &notices);
}

/// Swift's `content.sound = .default`, spelled the way `notify-rust` wants it.
///
/// macOS-only on purpose. The plugin's `sound()` is a platform-specific
/// *resource name*, not a portable "make a noise" flag, and there is no
/// Windows spelling of "the default one" to pair with this. A name the
/// platform does not know is a name it ignores, so the honest port is to name
/// the sound where the name means something and stay silent where it does not.
#[cfg(target_os = "macos")]
const NOTIFICATION_SOUND: Option<&str> = Some("NSUserNotificationDefaultSoundName");
#[cfg(not(target_os = "macos"))]
const NOTIFICATION_SOUND: Option<&str> = None;

/// Show a pass's banners.
///
/// The only impure half of either notifier — the watches decided *whether* each
/// of these exists, and this decides nothing. `what` names them for the one log
/// line that can fire, so a dropped banner says which kind it was.
///
/// Delivery goes through `tauri-plugin-notification`'s **Rust** API, which is
/// why the ACL grants that plugin nothing at all: the webview is never in the
/// path. A notice arriving before `tauri::Builder::run` has handed us a handle
/// is dropped rather than queued — the poll loops are spawned first, so that
/// window exists, but the pass that opens it is a seeding pass, which by
/// construction produces no notices.
fn deliver_banners(app: &App, what: &str, banners: &[(&str, &str)]) {
    if banners.is_empty() {
        return;
    }
    let Some(handle) = app.handle.get() else {
        eprintln!(
            "dropping {} {what} notification(s): the app is not up yet",
            banners.len()
        );
        return;
    };
    for (title, body) in banners {
        let mut builder = handle.notification().builder().title(*title).body(*body);
        if let Some(sound) = NOTIFICATION_SOUND {
            builder = builder.sound(sound);
        }
        // Logged, not surfaced: this is the same silent no-op Swift takes when
        // the user has denied notification permission, and a cockpit panel is
        // the wrong place to report that the OS declined a banner.
        if let Err(e) = builder.show() {
            eprintln!("could not show a {what} notification: {e}");
        }
    }
}

fn deliver_approval_notices(app: &App, notices: &[github::notify::ApprovalNotice]) {
    let banners: Vec<(&str, &str)> = notices
        .iter()
        .map(|n| (n.title.as_str(), n.body.as_str()))
        .collect();
    deliver_banners(app, "needs-approval", &banners);
}

fn deliver_status_notices(app: &App, notices: &[services::StatusNotice]) {
    let banners: Vec<(&str, &str)> = notices
        .iter()
        .map(|n| (n.title.as_str(), n.body.as_str()))
        .collect();
    deliver_banners(app, "service-status", &banners);
}

/// The GitHub panels' poll loop, on the store's `refresh_interval_secs`.
///
/// The interval is re-read after every pass rather than baked into a
/// `tokio::time::interval`, and the sleep is interruptible — together that is
/// what makes a Settings change apply *now* rather than up to five minutes
/// later, which is the periodic-service equivalent of [`reload_hosts`].
async fn github_loop(app: Arc<App>) {
    loop {
        poll_github(&app).await;
        let secs = {
            let store = app.store.lock().expect("store poisoned");
            u64::from(store.settings().refresh_interval_secs)
        };
        tokio::select! {
            () = tokio::time::sleep(std::time::Duration::from_secs(secs)) => {}
            // `notify_one`, not `notify_waiters`: a wake that lands while a
            // pass is still running stores a permit and fires the moment this
            // sleep begins, instead of being dropped for having no listener.
            () = app.github_wake.notified() => {}
        }
    }
}

/// Cuts the GitHub loop's sleep short after an edit that changes what it should
/// fetch, or how often.
fn wake_github(app: &App) {
    app.github_wake.notify_one();
}

/// Guards the Repos panel's counterpart to [`FIRST_REQUEST`].
static FIRST_REPOS_REQUEST: std::sync::Once = std::sync::Once::new();

#[tauri::command]
fn repos(state: tauri::State<'_, Arc<App>>) -> Value {
    let payload = {
        let panel = state.github.lock().expect("github state poisoned");
        github::repos_view(&panel, github::now_utc())
    };
    // The same terminal-side signal `cockpit`, `settings_view` and
    // `containers` print, for the same reason: the IPC boundary has no
    // automated coverage (#123), and every way it breaks from outside Rust
    // looks identical from in here — this function never runs.
    FIRST_REPOS_REQUEST.call_once(|| {
        let rows = payload["rows"].as_array().map_or(0, Vec::len);
        eprintln!("repos: first frontend request ({rows} repo row(s))");
    });
    payload
}

/// Guards the Runners panel's counterpart to [`FIRST_REQUEST`].
static FIRST_RUNNERS_REQUEST: std::sync::Once = std::sync::Once::new();

#[tauri::command]
fn runners(state: tauri::State<'_, Arc<App>>) -> Value {
    let payload = {
        let panel = state.github.lock().expect("github state poisoned");
        github::runners_view(&panel, panel::now_unix())
    };
    FIRST_RUNNERS_REQUEST.call_once(|| {
        let rows = payload["rows"].as_array().map_or(0, Vec::len);
        eprintln!("runners: first frontend request ({rows} runner row(s))");
    });
    payload
}

/// Which credentials currently hold a value — the "stored" badges, and nothing
/// else. A read failure reads as "not stored": the badge is a hint, and an
/// unreadable keychain must not take the Settings window down with it.
fn stored_secrets(credentials: &dyn CredentialStore, hosts: &[Host]) -> StoredSecrets {
    let present = |key: SecretKey| {
        credentials
            .secret(key)
            .unwrap_or_else(|e| {
                // The account name only — `SecretError` is value-free by
                // construction and this must stay that way.
                eprintln!("could not read a stored credential: {e}");
                None
            })
            .is_some_and(|value| !value.is_empty())
    };
    StoredSecrets {
        github: present(SecretKey::GitHubAccessToken),
        neon: present(SecretKey::NeonApiKey),
        sentry: present(SecretKey::SentryUsageToken),
        vercel: present(SecretKey::VercelApiToken),
        azure: present(SecretKey::AzureCostSasUrl),
        openclaw: present(SecretKey::OpenClawBearerToken),
        hosts: hosts
            .iter()
            .filter(|host| present(SecretKey::HostToken(host.id)))
            .map(|host| host.id)
            .collect(),
    }
}

/// The Settings payload for the app's current state.
fn settings_payload(app: &App) -> Value {
    // Read before the store's lock is taken, and never while it is held: the
    // OpenClaw session loop takes them in the same order, so there is one
    // ordering between the two and no way to invert it here.
    let facts = openclaw_settings_facts(app);
    let store = app.store.lock().expect("store poisoned");
    let stored = stored_secrets(app.credentials.as_ref(), store.hosts());
    settings::view(
        store.settings(),
        store.hosts(),
        store.repos(),
        store.container_rules(),
        store.layout(),
        &stored,
        &facts,
    )
}

/// The live half of the OpenClaw tab.
///
/// Falls back to reading the stored device key when the session has not run yet
/// — a machine with no gateway URL configured never starts a session, and the
/// operator should still be able to see (and pre-approve) the fingerprint from
/// a previous install. `current_device_id` only *reads*: opening Settings must
/// not mint a key as a side effect.
fn openclaw_settings_facts(app: &App) -> openclaw::SettingsFacts {
    let mut facts = app
        .openclaw
        .lock()
        .expect("openclaw state poisoned")
        .settings_facts();
    if facts.device_id.is_none() {
        facts.device_id =
            openclaw::current_device_id(&openclaw::DeviceKeys(app.credentials.as_ref()));
    }
    facts
}

/// Every settings mutation answers in one shape: what happened, plus the whole
/// surface as it now stands.
///
/// One shape rather than per-command payloads so the frontend re-renders from
/// the store's actual state after every edit — it never patches its own copy,
/// so it cannot drift from what was persisted (or quietly show an edit that
/// failed to save).
fn settings_response(app: &App, status: Option<String>) -> Value {
    json!({ "status": status, "settings": settings_payload(app) })
}

/// Persists the store, turning a failure into the status line Swift shows.
fn save_status(store: &Store, ok: impl Into<String>) -> Option<String> {
    match store.save() {
        Ok(()) => Some(ok.into()),
        Err(e) => Some(format!("Failed: {e}")),
    }
}

/// Guards the one-line "the frontend reached us" notice below.
static FIRST_REQUEST: std::sync::Once = std::sync::Once::new();

#[tauri::command]
fn cockpit(width: f64, state: tauri::State<'_, Arc<App>>) -> Value {
    // The IPC boundary has no automated coverage (#123), and every failure
    // mode the manual smoke test in `app/README.md` looks for — a rejected
    // ACL, an unregistered command, a CSP break that stops `app.js` before it
    // ever calls `invoke` — has the identical shape from in here: this
    // function never runs. So one line on the first call is the whole
    // terminal-side signal, and it makes the procedure runnable on a machine
    // whose screen you cannot see. It says nothing about what the frontend
    // then *painted* — that is what the visual read is still for.
    // Cloned out from under the poll-set lock, which is then released: the
    // per-host locks below are the only ones held while cards are built, so a
    // settings edit landing mid-render waits on nothing.
    let hosts: Vec<Arc<Mutex<HostState>>> = state
        .hosts
        .lock()
        .expect("poll set poisoned")
        .iter()
        .map(|polled| Arc::clone(&polled.state))
        .collect();
    FIRST_REQUEST.call_once(|| {
        eprintln!(
            "cockpit: first frontend request ({} host(s), {width}pt)",
            hosts.len()
        );
    });
    let local = state.local.lock().expect("local state poisoned").card();
    // Re-read every frame rather than captured at startup, for the same reason
    // the GitHub loop re-reads its token: that is what makes a Settings edit
    // apply without a relaunch. The store lock is taken after the poll set's is
    // released, matching `poll_containers`'s sequence, and is held for one read.
    //
    // **The width picks the arrangement.** `breakpoint_for` takes the widest
    // band the measured cockpit clears, and that band carries both the panel
    // order and the host-overflow mode — so resizing the window changes the
    // layout on the next 1s frame, with no preference to toggle. Both are
    // laundered on the way out as well as on the way in (`breakpoints`), so a
    // store hand-edited to name a panel this build does not have still renders
    // every panel it does.
    let (overflow, core_row_span, layout) = {
        let store = state.store.lock().expect("store poisoned");
        let settings = store.settings();
        let bands = settings::breakpoints(store.layout(), settings.host_overflow_mode);
        let band = settings::breakpoint_for(&bands, width);
        (
            band.host_overflow,
            settings.core_row_span as usize,
            band.layout(),
        )
    };
    cockpit_view(Some(local), &hosts, width, overflow, core_row_span, &layout)
}

// MARK: the Usage + Azure Cost panels
//
// Two loops rather than one because they share nothing: different credentials,
// different sources, and cadences an order of magnitude apart (an hourly API
// read vs a daily blob export). Both follow `github_loop`'s shape — poll,
// re-read the interval, sleep interruptibly — so a Settings edit applies now
// rather than on the far side of a cadence that can be four hours long.

/// One pass over the Usage panel's sources.
///
/// Claude is a local file walk and runs every pass. Neon and Sentry are network
/// reads on their own hourly cadence, so they run only when `providers` says
/// they are due — either the hour elapsed, or a Settings edit changed what they
/// would fetch.
async fn poll_usage(app: &Arc<App>, providers: bool) {
    let now = panel::now_unix();

    // Blocking: this walks a directory tree that can hold ~1600 files.
    let claude = tokio::task::spawn_blocking(read_claude_usage)
        .await
        .map_err(|e| eprintln!("Claude usage walk failed: {e}"))
        .ok();
    if let Some((summary, error)) = claude {
        app.usage
            .lock()
            .expect("usage state poisoned")
            .apply_claude(summary, now, error);
    }

    if !providers {
        return;
    }

    // Re-read every pass rather than captured at startup: that is what makes a
    // Save or Clear in Settings apply without a relaunch.
    let (neon_key, sentry_token, vercel_token) = (
        read_credential(&*app.credentials, SecretKey::NeonApiKey),
        read_credential(&*app.credentials, SecretKey::SentryUsageToken),
        read_credential(&*app.credentials, SecretKey::VercelApiToken),
    );
    let (neon_org, sentry_slug, vercel_team) = {
        let store = app.store.lock().expect("store poisoned");
        (
            store.settings().neon_org_id.clone(),
            store.settings().sentry_org_slug.clone(),
            store.settings().vercel_team_id.clone(),
        )
    };

    // "No key" hides the section; "the keychain would not answer" must not,
    // because that would delete a live section — and its retained figure — for
    // a full hour with nothing on screen to say why.
    match neon_key {
        Credential::Absent => app
            .usage
            .lock()
            .expect("usage state poisoned")
            .neon_unconfigure(),
        Credential::Unreadable => {
            let mut state = app.usage.lock().expect("usage state poisoned");
            state
                .neon_mut()
                .unreadable(CREDENTIAL_UNREADABLE_MESSAGE.to_owned());
            state
                .neon_invoice_mut()
                .unreadable(CREDENTIAL_UNREADABLE_MESSAGE.to_owned());
        }
        Credential::Present(key) => {
            // `NeonClient::new` returns `None` only for a blank key, which
            // `Credential::Present` has already excluded.
            let Some(client) = usage::NeonClient::new(&key) else {
                return;
            };
            // Before the request, not after it: the section is what tells the
            // operator this provider exists, and learning that from a *failure*
            // made its first ever appearance a row of em dashes under an error.
            {
                let mut state = app.usage.lock().expect("usage state poisoned");
                state.neon_mut().begin();
                state.neon_invoice_mut().begin();
            }
            let result = client.month_to_date(&neon_org, github::now_utc()).await;
            {
                let mut state = app.usage.lock().expect("usage state poisoned");
                match result {
                    // A successful call that measured nothing keeps the `—` and
                    // explains itself: an empty org, the wrong org id, or a plan
                    // without consumption history.
                    Ok(summary) => state.neon_mut().succeeded(
                        summary,
                        now,
                        summary
                            .is_unmeasured()
                            .then(|| usage::NEON_NO_CONSUMPTION_MESSAGE.to_owned()),
                    ),
                    Err(e) => state.neon_mut().failed(e.user_message()),
                }
            }

            // Best-effort: the endpoint is undocumented, so every failure is
            // degradation (footer + retained figure), never breakage — and it
            // must not disturb the consumption rows' state.
            let invoice_result = client.invoices(&neon_org).await;
            let mut state = app.usage.lock().expect("usage state poisoned");
            match invoice_result {
                Ok(summary) => state.neon_invoice_mut().succeeded(summary, now, None),
                Err(e) => state
                    .neon_invoice_mut()
                    .failed(format!("invoices: {}", e.user_message())),
            }
        }
    }

    match sentry_token {
        Credential::Absent => app
            .usage
            .lock()
            .expect("usage state poisoned")
            .sentry_mut()
            .unconfigure(),
        Credential::Unreadable => app
            .usage
            .lock()
            .expect("usage state poisoned")
            .sentry_mut()
            .unreadable(CREDENTIAL_UNREADABLE_MESSAGE.to_owned()),
        Credential::Present(token) => {
            let Some(client) = usage::SentryClient::new(&token) else {
                return;
            };
            // Before the request — see the Neon arm above.
            app.usage
                .lock()
                .expect("usage state poisoned")
                .sentry_mut()
                .begin();
            let result = client.accepted_errors(&sentry_slug).await;
            let mut state = app.usage.lock().expect("usage state poisoned");
            match result {
                Ok(summary) => state.sentry_mut().succeeded(
                    summary,
                    now,
                    summary
                        .is_unmeasured()
                        .then(|| usage::SENTRY_NO_STATS_MESSAGE.to_owned()),
                ),
                Err(e) => state.sentry_mut().failed(e.user_message()),
            }
        }
    }

    // Vercel. Same shape as Sentry above — one endpoint, one summary — and the
    // same lock discipline: taken around `begin()`, dropped across the await,
    // retaken to record the answer.
    match vercel_token {
        Credential::Absent => app
            .usage
            .lock()
            .expect("usage state poisoned")
            .vercel_mut()
            .unconfigure(),
        Credential::Unreadable => app
            .usage
            .lock()
            .expect("usage state poisoned")
            .vercel_mut()
            .unreadable(CREDENTIAL_UNREADABLE_MESSAGE.to_owned()),
        Credential::Present(token) => {
            // A blank team id is legitimate — a personal account has no team,
            // and the client omits the parameter rather than sending it empty.
            let Some(client) = usage::VercelClient::new(&token, &vercel_team) else {
                return;
            };
            // Before the request — see the Neon arm above.
            app.usage
                .lock()
                .expect("usage state poisoned")
                .vercel_mut()
                .begin();
            let result = client.month_to_date(github::now_utc()).await;
            let mut state = app.usage.lock().expect("usage state poisoned");
            match result {
                Ok(summary) => {
                    let empty = summary.is_unmeasured();
                    state.vercel_mut().succeeded(
                        summary,
                        now,
                        empty.then(|| usage::VERCEL_NO_SPEND_MESSAGE.to_owned()),
                    );
                }
                Err(e) => state.vercel_mut().failed(e.user_message()),
            }
        }
    }
}

/// Walks Claude Code's log root, returning the summary and the shell's own
/// "the root isn't there" note.
///
/// The existence check is the *shell's*, exactly as it is in Swift: the walk
/// itself skips what it cannot read rather than failing, so a missing root would
/// otherwise be indistinguishable from a quiet week. An unlocatable home
/// directory yields no summary at all — nothing was read, so there is nothing to
/// report, not even a zero.
fn read_claude_usage() -> (Option<usage::UsageSummary>, Option<String>) {
    let Some(dir) = usage::default_projects_dir() else {
        return (None, Some(usage::NO_LOG_ROOT_MESSAGE.to_owned()));
    };
    if !dir.exists() {
        return (None, Some(usage::NO_LOG_ROOT_MESSAGE.to_owned()));
    }
    // The one place in this shell that reads the machine's timezone: "today" is
    // a *local* calendar day, and `crates/usage` takes the offset as an argument
    // precisely so it never has to.
    let offset = *chrono::Local::now().offset();
    (
        Some(usage::summarize_logs(&dir, github::now_utc(), offset)),
        None,
    )
}

/// The Usage panel's poll loop.
async fn usage_loop(app: Arc<App>) {
    use std::sync::atomic::Ordering;

    let mut providers_last = None::<std::time::Instant>;
    loop {
        let due = app.usage_providers_due.swap(false, Ordering::SeqCst)
            || providers_last
                .is_none_or(|at| at.elapsed().as_secs() >= usage::PROVIDER_POLL_INTERVAL_SECS);
        poll_usage(&app, due).await;
        if due {
            providers_last = Some(std::time::Instant::now());
        }

        let secs = {
            let store = app.store.lock().expect("store poisoned");
            u64::from(store.settings().refresh_interval_secs)
        };
        tokio::select! {
            () = tokio::time::sleep(std::time::Duration::from_secs(secs)) => {}
            () = app.usage_wake.notified() => {}
        }
    }
}

// MARK: the Sentry Crons panel
//
// Its own loop rather than a third provider inside `poll_usage`: it is a separate
// panel with a separate lock, and folding it in would make one keychain read and
// one HTTP failure shared between two cards that answer different questions.
// Same cadence as the Sentry read in there, from the same constant.

/// One pass over the org's cron monitors.
///
/// The credential and the slug are re-read every pass, which is what makes a
/// Settings edit apply without a relaunch — and [`App::crons_wake`] is what makes
/// it apply *now* rather than up to an hour later.
async fn poll_crons(app: &Arc<App>) {
    let token = match read_credential(&*app.credentials, SecretKey::SentryUsageToken) {
        Credential::Present(token) => token,
        // "No token" is the setup state and paints an instruction.
        Credential::Absent => {
            app.crons
                .lock()
                .expect("crons state poisoned")
                .unconfigure();
            return;
        }
        // "The keychain would not answer" is a failure, and must not paint that
        // instruction over a configuration that is perfectly fine.
        Credential::Unreadable => {
            app.crons
                .lock()
                .expect("crons state poisoned")
                .unreadable(CREDENTIAL_UNREADABLE_MESSAGE.to_owned());
            return;
        }
    };
    let slug = {
        let store = app.store.lock().expect("store poisoned");
        store.settings().sentry_org_slug.clone()
    };
    // `SentryClient::new` returns `None` only for a blank token, which
    // `Credential::Present` has already excluded.
    let Some(client) = usage::SentryClient::new(&token) else {
        return;
    };
    // Before the request, not after it: the panel is what tells the operator this
    // watch exists, and learning that from a completed fetch made its first frame
    // indistinguishable from "there is no token".
    app.crons.lock().expect("crons state poisoned").begin();

    let result = client.cron_monitor_status(&slug, github::now_utc()).await;
    let now = panel::now_unix();
    let mut state = app.crons.lock().expect("crons state poisoned");
    match result {
        Ok(summary) => state.succeeded(summary, now),
        Err(e) => state.failed(e.user_message()),
    }
}

/// How soon the crons poll retries while it has never once succeeded.
///
/// Same argument as [`AZURE_FIRST_READ_RETRY`]: the hourly rhythm is right for a
/// watch on a weekly cron and wrong for a first read that failed because the
/// network was not up yet at login, and `crons_wake` only fires on a Settings
/// edit — so that first failure would otherwise sit on the cockpit, red, for an
/// hour.
const CRONS_FIRST_READ_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

/// The Sentry Crons panel's poll loop.
async fn crons_loop(app: Arc<App>) {
    loop {
        poll_crons(&app).await;
        let settled = app
            .crons
            .lock()
            .expect("crons state poisoned")
            .has_succeeded();
        let wait = if settled {
            std::time::Duration::from_secs(usage::PROVIDER_POLL_INTERVAL_SECS)
        } else {
            CRONS_FIRST_READ_RETRY
        };
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = app.crons_wake.notified() => {}
        }
    }
}

/// Guards the Sentry Crons panel's counterpart to [`FIRST_REQUEST`].
static FIRST_CRONS_REQUEST: std::sync::Once = std::sync::Once::new();

#[tauri::command]
fn crons(state: tauri::State<'_, Arc<App>>) -> Value {
    let payload = {
        let panel = state.crons.lock().expect("crons state poisoned");
        crons::view(&panel, panel::now_unix())
    };
    FIRST_CRONS_REQUEST.call_once(|| {
        eprintln!(
            "crons: first frontend request ({})",
            payload["trailing"].as_str().unwrap_or("nothing read yet")
        );
    });
    payload
}

/// What a credential read actually learned.
///
/// The three cases are genuinely different and collapsing them is the
/// unknown-is-not-zero rule in credential form. `unwrap_or_default()` on a
/// `Result<Option<String>>` turns "the keychain would not answer" into an empty
/// string, which every consumer downstream reads as "the user has not set this
/// up" — so a locked keychain silently deletes a configured panel, tells the
/// operator to paste a credential they already pasted, and (for Azure) discards
/// the fingerprint cache that keeps an unchanged export free.
enum Credential {
    /// No credential is stored. The panel's zero-setup state.
    Absent,
    Present(String),
    /// The credential store refused to answer. We do not know either way.
    Unreadable,
}

/// The operator-facing line for [`Credential::Unreadable`]. Names the layer to
/// go and look at, and carries no account name or value — `SecretError` is
/// value-free by construction and this string must stay that way.
const CREDENTIAL_UNREADABLE_MESSAGE: &str = "couldn't read the credential store";

/// Reads one credential, keeping "there is none" apart from "we could not ask".
///
/// Generic over the store rather than taking `&App` so every caller's three
/// branches can be pinned against `MemoryCredentialStore` and a failing double,
/// with no keychain and no Tauri app in the test.
fn read_credential<C: CredentialStore + ?Sized>(credentials: &C, key: SecretKey) -> Credential {
    match credentials.secret(key) {
        Ok(Some(value)) if !value.trim().is_empty() => Credential::Present(value),
        Ok(_) => Credential::Absent,
        Err(e) => {
            eprintln!("could not read a stored credential: {e}");
            Credential::Unreadable
        }
    }
}

/// Cuts the usage loop's sleep short. `providers` also forces the hourly half to
/// run on that pass — a newly-saved Neon key must fill its section now.
fn wake_usage(app: &App, providers: bool) {
    if providers {
        app.usage_providers_due
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    app.usage_wake.notify_one();
}

/// One Azure cost read.
///
/// The SAS URL *is* the credential, so it never leaves this function: it goes
/// straight into the fetcher, whose `Debug` redacts it and whose errors are
/// stripped of their URL before they become strings.
async fn poll_azure(app: &Arc<App>) {
    let sas = match read_credential(&*app.credentials, SecretKey::AzureCostSasUrl) {
        Credential::Present(sas) => sas,
        Credential::Absent => {
            app.azure
                .lock()
                .expect("azure state poisoned")
                .unconfigure();
            return;
        }
        // Not `unconfigure()`: that paints "Add an Azure Cost SAS URL in
        // Settings" — the one state this panel must never confuse with a
        // failure — over a configuration that is perfectly fine, and it throws
        // away the fingerprint cache, so the next good read re-downloads every
        // partition. A locked keychain is a failure, and it says so.
        Credential::Unreadable => {
            app.azure
                .lock()
                .expect("azure state poisoned")
                .unreadable(CREDENTIAL_UNREADABLE_MESSAGE.to_owned());
            return;
        }
    };

    let previous = {
        let mut state = app.azure.lock().expect("azure state poisoned");
        state.begin();
        state.cached()
    };
    // An unchanged export costs one blob listing and no partition bodies — the
    // fingerprint from the last success is what buys that.
    let result = azurecost::fetch_summary(
        &azurecost::SasBlobFetcher::new(&sas),
        github::now_utc(),
        previous.as_ref(),
    )
    .await;

    let now = panel::now_unix();
    let mut state = app.azure.lock().expect("azure state poisoned");
    match result {
        Ok(fetched) => state.succeeded(fetched, now),
        Err(e) => state.failed(e.user_message()),
    }
}

/// How soon the Azure poll retries while it has never once succeeded.
///
/// The export's own rhythm is [`azurecost::POLL_INTERVAL`] — 4h — which is right
/// for a file that is published daily and wrong for a first read that failed
/// because the network was not up yet at login. `azure_wake` only fires when the
/// SAS is re-saved, so that first failure used to sit on the cockpit, red, until
/// the next four-hourly cycle. A minute is short enough to look self-healing and
/// long enough that a genuinely broken SAS is not hammered.
const AZURE_FIRST_READ_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

/// The Azure Cost panel's poll loop, on the reader's own fixed 4h cadence once
/// it has something to show, and a short retry until then.
async fn azure_loop(app: Arc<App>) {
    loop {
        poll_azure(&app).await;
        let settled = app
            .azure
            .lock()
            .expect("azure state poisoned")
            .has_succeeded();
        let wait = if settled {
            azurecost::POLL_INTERVAL
        } else {
            AZURE_FIRST_READ_RETRY
        };
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = app.azure_wake.notified() => {}
        }
    }
}

/// Guards the Usage panel's counterpart to [`FIRST_REQUEST`].
static FIRST_USAGE_REQUEST: std::sync::Once = std::sync::Once::new();

#[tauri::command]
fn usage(state: tauri::State<'_, Arc<App>>) -> Value {
    // Read at render time, not captured by the poller: changing the quota or
    // the rates must repaint now, and no API call is involved in either.
    let (quota, rates) = {
        let store = state.store.lock().expect("store poisoned");
        let settings = store.settings();
        (
            settings.sentry_monthly_event_quota,
            usage::NeonRates {
                usd_per_cu_hour: settings.neon_usd_per_cu_hour,
                usd_per_gib_month: settings.neon_usd_per_gib_month,
            },
        )
    };
    let payload = {
        let panel = state.usage.lock().expect("usage state poisoned");
        usage::view(&panel, quota, rates, panel::now_unix())
    };
    FIRST_USAGE_REQUEST.call_once(|| {
        let providers = payload["providers"].as_array().map_or(0, Vec::len);
        eprintln!("usage: first frontend request ({providers} provider section(s))");
    });
    payload
}

/// Guards the Services panel's counterpart to [`FIRST_REQUEST`].
static FIRST_SERVICES_REQUEST: std::sync::Once = std::sync::Once::new();

#[tauri::command]
fn services(state: tauri::State<'_, Arc<App>>) -> Value {
    let payload = {
        let statuses = state.services.lock().expect("services poisoned");
        services::view(&statuses)
    };
    FIRST_SERVICES_REQUEST.call_once(|| {
        eprintln!(
            "services: first frontend request ({})",
            payload["trailing"].as_str().unwrap_or_default()
        );
    });
    payload
}

/// Guards the Azure Cost panel's counterpart to [`FIRST_REQUEST`].
static FIRST_AZURE_REQUEST: std::sync::Once = std::sync::Once::new();

#[tauri::command]
fn azure_cost(state: tauri::State<'_, Arc<App>>) -> Value {
    let budget = {
        let store = state.store.lock().expect("store poisoned");
        store.settings().azure_monthly_budget_usd
    };
    let payload = {
        let panel = state.azure.lock().expect("azure state poisoned");
        azure::view(&panel, budget, panel::now_unix())
    };
    FIRST_AZURE_REQUEST.call_once(|| {
        // A headline exists only once a read has landed, so this separates "the
        // round-trip worked and there is data" from "the round-trip worked and
        // the panel is in one of its message states" — both are a pass for the
        // boundary, and the smoke test in app/README.md says which is which.
        let has_headline = !payload["headline"].is_null();
        eprintln!("azure_cost: first frontend request (headline: {has_headline})");
    });
    payload
}

// MARK: the OpenClaw panel
//
// The one subsystem here that is **not** a poll loop. Every other panel asks
// its source on a cadence; this one holds a WebSocket open and is written as
// frames arrive, which is why its state carries no "last polled" clock and its
// payload carries no staleness footer — the connection line is the answer.
//
// What *is* a loop is reconnecting. `crates/openclaw`'s `Backoff` paces it, and
// the two cases it separates are the point: an ordinary drop escalates
// exponentially, while a pending device approval waits on a fixed, quiet timer,
// because hammering a gateway that is waiting on a human helps nobody and
// floods both logs.

/// The gateway URL and bearer token, re-read every pass.
///
/// Re-read rather than captured at startup for the same reason the Usage loop
/// re-reads its credentials: that is what makes a Settings edit apply without a
/// relaunch. An unreadable credential store yields `None` here — a bearer token
/// is optional, and refusing to connect because the keychain hiccuped would be
/// worse than trying without it and letting the gateway decide.
fn openclaw_config(app: &App) -> (String, Option<String>) {
    let url = {
        let store = app.store.lock().expect("store poisoned");
        store.settings().openclaw_gateway_url.trim().to_owned()
    };
    let token = match read_credential(&*app.credentials, SecretKey::OpenClawBearerToken) {
        Credential::Present(token) => Some(token),
        Credential::Absent | Credential::Unreadable => None,
    };
    (url, token)
}

/// The OpenClaw session loop: connect, stream, reconnect.
///
/// The reducer is created once per *gateway*, not once per session. A dropped
/// socket says nothing new about the farm, so reconnecting keeps the agent rows
/// on screen; pointing at a different gateway invalidates every one of them, so
/// that resets both the reducer and the published sections.
async fn openclaw_loop(app: Arc<App>) {
    let mut backoff = openclaw::Backoff::new();
    let mut reducer = openclaw::SnapshotReducer::new();
    let mut identity: Option<openclaw::DeviceIdentity> = None;
    let mut current_url: Option<String> = None;

    loop {
        let (url, token) = openclaw_config(&app);
        if current_url.as_deref() != Some(url.as_str()) {
            if current_url.is_some() {
                reducer = openclaw::SnapshotReducer::new();
                app.openclaw
                    .lock()
                    .expect("openclaw state poisoned")
                    .forget_sections();
            }
            current_url = Some(url.clone());
            // A deliberate change is not a failure, so the next attempt starts
            // at the floor rather than inheriting the previous gateway's
            // escalation.
            backoff.reset();
        }

        if url.is_empty() {
            // Idle, not disconnected: nothing was attempted, so the panel shows
            // the Settings hint rather than a failure nobody caused.
            app.openclaw.lock().expect("openclaw state poisoned").idle();
            app.openclaw_wake.notified().await;
            continue;
        }

        // Minted at most once per process, and only once a gateway is actually
        // configured: generating a device key on a machine that never connects
        // would leave an unapprovable fingerprint in the keychain.
        if identity.is_none() {
            // Bound to a `let` so the borrow of the credential store ends on
            // this line. Holding it into the match below would carry a
            // `!Sync` reference across the awaits in its arms, which makes the
            // whole loop un-spawnable — and the compiler says so a hundred
            // lines away from the cause.
            let loaded = openclaw::load_or_create(&openclaw::DeviceKeys(app.credentials.as_ref()));
            match loaded {
                Ok(loaded) => {
                    if let Some(e) = loaded.persist_error {
                        // Non-fatal by design: the identity works for this run,
                        // it just will not survive a relaunch — which beats
                        // refusing to connect. The error names the account and
                        // carries no key material.
                        eprintln!("openclaw: could not persist the device key: {e}");
                    }
                    if loaded.generated {
                        eprintln!(
                            "openclaw: generated device identity {} — approve it on the gateway to pair",
                            loaded.identity.device_id()
                        );
                    }
                    app.openclaw
                        .lock()
                        .expect("openclaw state poisoned")
                        .set_device_id(loaded.identity.device_id());
                    identity = Some(loaded.identity);
                }
                Err(e) => {
                    // The platform RNG refused. Pairing is impossible until it
                    // does not, and substituting a weak key would be worse.
                    eprintln!("openclaw: {e}");
                    app.openclaw
                        .lock()
                        .expect("openclaw state poisoned")
                        .disconnected("could not create a device key");
                    app.openclaw_wake.notified().await;
                    continue;
                }
            }
        }
        let Some(device) = identity.clone() else {
            continue;
        };

        // The session is raced against the wake so an edit lands *now*. A
        // healthy socket never returns on its own, so without this a new
        // gateway URL would apply only after the old gateway happened to drop.
        let outcome = tokio::select! {
            outcome = openclaw::run_session(
                &app.openclaw,
                &mut reducer,
                &url,
                token,
                device,
                settings::VERSION,
            ) => outcome,
            () = app.openclaw_wake.notified() => continue,
        };

        let delay = backoff.delay_after(outcome);
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = app.openclaw_wake.notified() => backoff.reset(),
        }
    }
}

/// Restarts the OpenClaw session. Cheap and idempotent: a wake with no session
/// waiting is remembered, so a save that lands between sessions is not lost.
fn wake_openclaw(app: &App) {
    app.openclaw_wake.notify_one();
}

/// Guards the OpenClaw panel's counterpart to [`FIRST_REQUEST`].
static FIRST_OPENCLAW_REQUEST: std::sync::Once = std::sync::Once::new();

#[tauri::command]
fn openclaw(state: tauri::State<'_, Arc<App>>) -> Value {
    let payload = {
        let panel = state.openclaw.lock().expect("openclaw state poisoned");
        openclaw::view(panel.snapshots())
    };
    FIRST_OPENCLAW_REQUEST.call_once(|| {
        // The trailing label is the panel's own one-line summary, so it says
        // both that the round-trip worked and which state the session is in —
        // and it can never contain a token: `trailing` is built from counts and
        // connection states only.
        let trailing = payload["trailing"].as_str().unwrap_or_default();
        eprintln!("openclaw: first frontend request (trailing: {trailing:?})");
    });
    payload
}

// MARK: the Settings command surface
//
// All app-defined commands, which Tauri's ACL permits without a grant — so
// this whole surface adds nothing to `capabilities/default.json`, and its
// `permissions` stays empty. That is also the reason Settings is an in-app
// view rather than a second window: a window would need
// `core:webview:allow-create-webview-window` (or `core:default`) granted to
// the webview, widening exactly the seam that has no automated coverage
// (#123). See `app/README.md`.

/// Guards the settings surface's counterpart to [`FIRST_REQUEST`].
static FIRST_SETTINGS_REQUEST: std::sync::Once = std::sync::Once::new();

#[tauri::command]
fn settings_view(state: tauri::State<'_, Arc<App>>) -> Value {
    let payload = settings_payload(&state);
    // The same terminal-side signal `cockpit` prints, for the same reason:
    // every way this surface can be broken from outside Rust -- a rejected
    // ACL, an unregistered command, a script error in settings.js -- looks
    // identical from in here (this never runs), and the smoke test has to be
    // runnable on a machine whose screen you cannot see. Opening Settings is
    // one click, so this is the whole verification of the new command surface.
    FIRST_SETTINGS_REQUEST.call_once(|| {
        let hosts = payload["hosts"]["rows"].as_array().map_or(0, Vec::len);
        let repos = payload["portfolio"]["rows"].as_array().map_or(0, Vec::len);
        eprintln!("settings: first frontend request ({hosts} host(s), {repos} repo(s))");
    });
    payload
}

#[tauri::command]
fn settings_save_general(
    refresh_interval_secs: u32,
    core_row_span: u8,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        let general = settings::normalized_general(refresh_interval_secs, core_row_span);
        let current = store.settings_mut();
        current.refresh_interval_secs = general.refresh_interval_secs;
        current.core_row_span = general.core_row_span;
        save_status(&store, "Saved.")
    };
    // The refresh interval is the GitHub *and* Claude-usage loops' cadence, so
    // picking a new one has to reach both before the *old* one elapses —
    // shortening 5 minutes to 30 seconds and then waiting five minutes for it
    // to take is indistinguishable from the setting doing nothing. The Usage
    // panel's provider half is on its own hourly clock and is deliberately not
    // forced here: nothing about a cadence change alters what Neon returns.
    wake_github(&state);
    wake_usage(&state, false);
    settings_response(&state, status)
}

/// The shared half of every Layout-tab mutation: read every band, let `edit`
/// change them, write them all back.
///
/// Read-modify-write over the *normalised* bands rather than over the stored
/// profiles, which is what makes each of these commands total: whatever is on
/// disk, `breakpoints` hands back at least one band holding every panel exactly
/// once, so an edit can never operate on a layout with a hole in it — and what
/// gets written back is therefore always complete and always renderable.
///
/// It is also the migration's one write path. The bands it hands `edit` already
/// have the legacy General overflow folded in, so the first Layout edit of an
/// upgraded store persists that mode into the profile rather than losing it.
///
/// `edit` returning `false` means the request addressed nothing (an unknown
/// panel or band, a move off the end); nothing is written and the status says
/// so, the same shape `apply_rule_edit`'s caller uses.
fn edit_layout(
    app: &App,
    edit: impl FnOnce(&mut Vec<settings::Breakpoint>) -> bool,
) -> Option<String> {
    let mut store = app.store.lock().expect("store poisoned");
    let seed = store.settings().host_overflow_mode;
    let mut bands = settings::breakpoints(store.layout(), seed);
    if !edit(&mut bands) {
        return Some("Skipped — unknown breakpoint or panel.".to_owned());
    }
    store.set_layout(settings::store_profiles(&bands));
    save_status(&store, "Saved.")
}

/// Applies `edit` to the panel order of the band at `min_width`.
fn edit_band_order(
    app: &App,
    min_width: f64,
    edit: impl FnOnce(&mut Vec<(PanelKind, PanelSpan)>) -> bool,
) -> Option<String> {
    edit_layout(app, |bands| {
        settings::band_mut(bands, min_width).is_some_and(|band| edit(&mut band.order))
    })
}

#[tauri::command]
fn settings_move_panel(
    min_width: f64,
    panel: String,
    direction: String,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    let Some(kind) = PanelKind::parse(&panel) else {
        return settings_response(&state, Some("Skipped — unknown panel.".into()));
    };
    let Some(direction) = settings::PanelMove::parse(&direction) else {
        return settings_response(&state, Some("Skipped — unknown direction.".into()));
    };
    let status = edit_band_order(&state, min_width, |order| {
        settings::move_panel(order, kind, direction)
    });
    // No wake: the cockpit re-reads the layout on its own next frame (one
    // second), and closing Settings repaints it immediately.
    settings_response(&state, status)
}

#[tauri::command]
fn settings_set_panel_span(
    min_width: f64,
    panel: String,
    span: String,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    let Some(kind) = PanelKind::parse(&panel) else {
        return settings_response(&state, Some("Skipped — unknown panel.".into()));
    };
    let Some(span) = PanelSpan::parse(&span) else {
        return settings_response(&state, Some("Skipped — unknown width.".into()));
    };
    let status = edit_band_order(&state, min_width, |order| {
        settings::set_panel_span(order, kind, span)
    });
    settings_response(&state, status)
}

#[tauri::command]
fn settings_set_breakpoint_overflow(
    min_width: f64,
    host_overflow_mode: String,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    // Parsed strictly here rather than through `HostOverflowMode::from`, which
    // reads an unknown string as `Stack`: that tolerance is right for a *file*
    // written by another build and wrong for a command, where an unrecognised
    // mode means the caller sent something no picker can produce.
    let Some(mode) = [HostOverflowMode::Stack, HostOverflowMode::Tabs]
        .into_iter()
        .find(|candidate| candidate.as_str() == host_overflow_mode)
    else {
        return settings_response(&state, Some("Skipped — unknown overflow mode.".into()));
    };
    let status = edit_layout(&state, |bands| {
        settings::band_mut(bands, min_width).is_some_and(|band| {
            band.host_overflow = mode;
            true
        })
    });
    settings_response(&state, status)
}

#[tauri::command]
fn settings_add_breakpoint(min_width: f64, state: tauri::State<'_, Arc<App>>) -> Value {
    let status = edit_layout(&state, |bands| settings::add_breakpoint(bands, min_width));
    settings_response(
        &state,
        status.map(|line| {
            // The one failure a user can hit by typing: two bands cannot claim
            // the same width, and the generic "unknown breakpoint" would read
            // as a bug rather than as an answer.
            if line.starts_with("Skipped") {
                "Skipped — that width already has a breakpoint.".to_owned()
            } else {
                line
            }
        }),
    )
}

#[tauri::command]
fn settings_remove_breakpoint(min_width: f64, state: tauri::State<'_, Arc<App>>) -> Value {
    let status = edit_layout(&state, |bands| {
        settings::remove_breakpoint(bands, min_width)
    });
    settings_response(&state, status)
}

#[tauri::command]
fn settings_reset_layout(state: tauri::State<'_, Arc<App>>) -> Value {
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        // Cleared, not overwritten with the current default: a store that
        // carries no layout is the one that follows a future change to
        // `DEFAULT_ORDER`, which is what "reset to default" should mean a year
        // from now as well as today.
        store.clear_layout();
        save_status(&store, "Layout reset.")
    };
    settings_response(&state, status)
}

/// The GitHub organization the Runners panel queries.
///
/// A command of its own rather than a field on [`ProviderPrefs`]: that struct
/// is sent whole by two tabs precisely because a partial write blanks the
/// fields the sending tab does not show, and the GitHub tab shows none of
/// them. One field, one command, nothing to blank.
#[tauri::command]
fn settings_save_github(org: String, state: tauri::State<'_, Arc<App>>) -> Value {
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        store.settings_mut().github_org = org.trim().to_owned();
        save_status(&store, "Saved.")
    };
    // No wake: unlike the usage loops on their hourly cadence, the GitHub poll
    // re-reads settings on every pass, so this applies within one refresh
    // interval by the same mechanism that makes a re-pasted token apply.
    settings_response(&state, status)
}

/// Every non-secret provider preference, in one argument.
///
/// One struct rather than seven positional parameters: both Settings tabs send
/// the whole set on every Apply (each re-sends the other's fields, or a partial
/// write blanks them), so they travel together by construction — and the fifth
/// provider took the flat signature past clippy's argument limit, which is the
/// shape complaining about itself.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderPrefs {
    neon_org_id: String,
    sentry_org_slug: String,
    sentry_monthly_event_quota: u64,
    azure_monthly_budget_usd: f64,
    neon_usd_per_cu_hour: f64,
    neon_usd_per_gib_month: f64,
    vercel_team_id: String,
}

#[tauri::command]
fn settings_save_providers(prefs: ProviderPrefs, state: tauri::State<'_, Arc<App>>) -> Value {
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        let current = store.settings_mut();
        current.neon_org_id = prefs.neon_org_id.trim().to_owned();
        current.sentry_org_slug = prefs.sentry_org_slug.trim().to_owned();
        current.sentry_monthly_event_quota = prefs.sentry_monthly_event_quota;
        // A non-finite value would make the whole store unserialisable
        // (`StoreError::Serialize`), taking every other preference with it.
        let launder = |v: f64| if v.is_finite() { v.max(0.0) } else { 0.0 };
        current.azure_monthly_budget_usd = launder(prefs.azure_monthly_budget_usd);
        current.neon_usd_per_cu_hour = launder(prefs.neon_usd_per_cu_hour);
        current.neon_usd_per_gib_month = launder(prefs.neon_usd_per_gib_month);
        current.vercel_team_id = prefs.vercel_team_id.trim().to_owned();
        save_status(&store, "Saved.")
    };
    // The Neon org id and the Sentry slug are *what those reads ask for*, so an
    // edit that does not reach the loop leaves both sections describing the
    // previous configuration for up to an hour. The Azure budget and the Sentry
    // quota need no wake — both are read at render time, by design.
    wake_usage(&state, true);
    // The Sentry slug is a path segment of the cron-monitor read as well, so the
    // Crons panel would otherwise ask the wrong org for another hour.
    state.crons_wake.notify_one();
    settings_response(&state, status)
}

#[tauri::command]
fn settings_add_host(
    name: String,
    address: String,
    port: String,
    token: String,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    let name = name.trim().to_owned();
    let address = address.trim().to_owned();
    if name.is_empty() || address.is_empty() {
        return settings_response(
            &state,
            Some("Skipped — name and address are required.".into()),
        );
    }

    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        let mut host = Host::new(&name, address);
        host.port = settings::parse_port(&port);
        let id = host.id;
        store.upsert_host(host);
        let status = save_status(&store, format!("Added {name}."));

        if !token.is_empty() {
            // Keyed by the id the store just minted, and never into the store
            // file. A credential-store failure is reported and not fatal: the
            // host row is saved, and the operator can re-enter the token --
            // losing the row too would be the worse outcome.
            if let Err(e) = state
                .credentials
                .set_secret(SecretKey::HostToken(id), &token)
            {
                eprintln!("could not store the new host's token: {e}");
            }
        }
        status
    };

    reload_hosts(&state);
    settings_response(&state, status)
}

#[tauri::command]
fn settings_set_host_enabled(
    id: String,
    enabled: bool,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    let Ok(id) = Uuid::parse_str(&id) else {
        return settings_response(&state, Some("Skipped — unknown host.".into()));
    };
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        match store.host_mut(id) {
            Some(host) => {
                host.enabled = enabled;
                save_status(&store, "Saved.")
            }
            None => Some("Skipped — unknown host.".to_owned()),
        }
    };
    reload_hosts(&state);
    settings_response(&state, status)
}

#[tauri::command]
fn settings_remove_host(id: String, state: tauri::State<'_, Arc<App>>) -> Value {
    let Ok(id) = Uuid::parse_str(&id) else {
        return settings_response(&state, Some("Skipped — unknown host.".into()));
    };
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        match store.remove_host(id) {
            Some(host) => {
                // The store deliberately does not touch the credential store,
                // so deleting the token is this layer's job -- and it happens
                // whatever the file write does, or a re-added host would
                // inherit a stranger's token.
                if let Err(e) = state.credentials.delete_secret(SecretKey::HostToken(id)) {
                    eprintln!("could not delete the host's token: {e}");
                }
                save_status(&store, format!("Removed {}.", host.name))
            }
            None => Some("Skipped — unknown host.".to_owned()),
        }
    };
    reload_hosts(&state);
    settings_response(&state, status)
}

/// Unhides one mount — on a host when `host_id` is given, otherwise on the
/// local machine's list.
///
/// Deliberately does **not** call [`reload_hosts`]: a volume edit must not
/// cost the host card its sparkline history or its volume debounce. Same
/// distinction the Swift view draws between `applyHiddenMounts()` and
/// `reload()`.
#[tauri::command]
fn settings_unhide_volume(
    host_id: Option<String>,
    mount: String,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        match host_id {
            Some(raw) => {
                let host = Uuid::parse_str(&raw).ok().and_then(|id| store.host_mut(id));
                match host {
                    Some(host) => {
                        host.hidden_volume_mounts.retain(|m| m != &mount);
                        save_status(&store, format!("Unhid {mount}."))
                    }
                    None => Some("Skipped — unknown host.".to_owned()),
                }
            }
            None => {
                store
                    .settings_mut()
                    .local_hidden_volume_mounts
                    .retain(|m| m != &mount);
                save_status(&store, format!("Unhid {mount}."))
            }
        }
    };
    settings_response(&state, status)
}

// MARK: the container group rules editor
//
// Three commands over one persisted list, and every one of them is a
// read-modify-write of the *whole* list under the store's lock. That is the
// port of Swift's re-read-on-access bindings: nothing here trusts a client-side
// copy of the rules, so two edits in flight against the same row cannot clobber
// each other's other fields.
//
// **None of them wakes a loop, on purpose.** The rules are read at *render*
// time by the `containers` command (and by `poll_containers`, for presence), so
// an edit is visible on the panel's very next 10s tick with no fetch involved --
// exactly like the Sentry quota and the Azure budget. A wake here would spend a
// full pass of `docker ps` invocations to change nothing.

/// Reads the persisted rules, hands them to `edit`, and writes them back when
/// it says it changed something.
///
/// One place where the read-modify-write happens, so a future fourth mutation
/// cannot quietly acquire a stale-snapshot bug the other three don't have.
fn mutate_rules(
    app: &App,
    ok: impl Into<String>,
    edit: impl FnOnce(&mut Vec<ContainerGroupRule>) -> bool,
) -> Option<String> {
    let mut store = app.store.lock().expect("store poisoned");
    let mut rules = store.container_rules().to_vec();
    if !edit(&mut rules) {
        return Some("Skipped — unknown rule.".to_owned());
    }
    store.set_container_rules(rules);
    save_status(&store, ok)
}

#[tauri::command]
fn settings_add_container_rule(state: tauri::State<'_, Arc<App>>) -> Value {
    let status = mutate_rules(&state, "Added rule.", |rules| {
        rules.push(settings::new_rule());
        true
    });
    settings_response(&state, status)
}

/// One field of one rule.
///
/// Deliberately *not* a whole-row write: the frontend sends the field that
/// changed and nothing else, so the value in every other field of that row is
/// whatever is on disk rather than whatever the last render happened to paint.
/// Swift gets the same property from a `Binding` per `WritableKeyPath`.
#[tauri::command]
fn settings_set_container_rule(
    index: usize,
    field: String,
    value: String,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    let Some(field) = settings::RuleField::parse(&field) else {
        return settings_response(&state, Some("Skipped — unknown rule field.".into()));
    };
    let status = mutate_rules(&state, "Saved.", |rules| {
        settings::apply_rule_edit(rules, index, field, &value)
    });
    settings_response(&state, status)
}

#[tauri::command]
fn settings_remove_container_rule(index: usize, state: tauri::State<'_, Arc<App>>) -> Value {
    let status = mutate_rules(&state, "Removed rule.", |rules| {
        if index >= rules.len() {
            return false;
        }
        rules.remove(index);
        // An emptied list is respected as an emptied list, never re-seeded:
        // `Store::set_container_rules` writes `Some(vec![])`, which the loader
        // reads back as "the user cleared every rule" rather than "never
        // configured". Deleting the last rule and relaunching must not bring
        // the seeded three back.
        true
    });
    settings_response(&state, status)
}

/// The Test button: one `/v1/health` probe, rendered as the Swift result line.
#[tauri::command]
async fn settings_test_host(
    id: String,
    state: tauri::State<'_, Arc<App>>,
) -> Result<Value, String> {
    let uuid = Uuid::parse_str(&id).map_err(|_| "unknown host".to_owned())?;
    // Scoped so no lock is alive across the await below — the guard is not
    // `Send`, so this is enforced by the compiler rather than by care.
    let base_url = {
        let store = state.store.lock().expect("store poisoned");
        store
            .host(uuid)
            .map(Host::base_url)
            .ok_or_else(|| "unknown host".to_owned())?
    };
    let token = state
        .credentials
        .secret(SecretKey::HostToken(uuid))
        .unwrap_or_default()
        .unwrap_or_default();

    let result = AgentClient::new(base_url, token).health().await;
    Ok(json!({ "id": id, "result": settings::health_result(&result) }))
}

#[tauri::command]
fn settings_add_repo(slug: String, state: tauri::State<'_, Arc<App>>) -> Value {
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        match settings::validated_slug(&slug, store.repos()) {
            Some(slug) => {
                store.upsert_repo(TrackedRepo::new(&slug));
                save_status(&store, format!("Added {slug}."))
            }
            None => Some("Skipped — invalid or already tracked.".to_owned()),
        }
    };
    // The portfolio IS the Repos panel's row set, so an edit that does not
    // reach the loop leaves the panel describing the previous portfolio.
    wake_github(&state);
    settings_response(&state, status)
}

#[tauri::command]
fn settings_remove_repo(slug: String, state: tauri::State<'_, Arc<App>>) -> Value {
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        match store.remove_repo(&slug) {
            Some(repo) => save_status(&store, format!("Removed {}.", repo.slug)),
            None => Some("Skipped — not tracked.".to_owned()),
        }
    };
    wake_github(&state);
    settings_response(&state, status)
}

#[tauri::command]
fn settings_set_repo_enabled(
    slug: String,
    enabled: bool,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        match store.repo_mut(&slug) {
            Some(repo) => {
                repo.enabled = enabled;
                save_status(&store, "Saved.")
            }
            None => Some("Skipped — not tracked.".to_owned()),
        }
    };
    wake_github(&state);
    settings_response(&state, status)
}

#[tauri::command]
fn settings_set_repo_workflows(
    slug: String,
    workflows: String,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        match store.repo_mut(&slug) {
            Some(repo) => {
                repo.watched_workflows = settings::parse_workflows(&workflows);
                save_status(&store, "Saved.")
            }
            None => Some("Skipped — not tracked.".to_owned()),
        }
    };
    wake_github(&state);
    settings_response(&state, status)
}

/// The OpenClaw gateway URL. Validation is deliberately the *session's*, not
/// this command's: `upgrade_request` is the one place that decides what a usable
/// `ws(s)://` address is, and a second rule here could reject a URL the client
/// would have accepted — or accept one it then refuses, with the panel left to
/// explain a failure Settings had already blessed.
#[tauri::command]
fn settings_save_openclaw(gateway_url: String, state: tauri::State<'_, Arc<App>>) -> Value {
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        store.settings_mut().openclaw_gateway_url = gateway_url.trim().to_owned();
        save_status(&store, "Saved.")
    };
    // The URL *is* what the session connects to, so a save that does not reach
    // the loop leaves a live socket pointed at the previous gateway.
    wake_openclaw(&state);
    settings_response(&state, status)
}

/// The pairing block's "Retry now": reconnect immediately instead of waiting out
/// the 15s pairing backoff.
///
/// The button exists because the operator knows something the app cannot: that
/// they have just run the approve command. Making them wait for a timer after
/// that is the difference between "it worked" and "did it work?".
#[tauri::command]
fn settings_openclaw_retry(state: tauri::State<'_, Arc<App>>) -> Value {
    wake_openclaw(&state);
    settings_response(&state, Some("Reconnecting…".to_owned()))
}

#[tauri::command]
fn settings_save_secret(key: String, value: String, state: tauri::State<'_, Arc<App>>) -> Value {
    let Some(field) = SecretField::parse(&key) else {
        return settings_response(&state, Some("Skipped — unknown credential.".into()));
    };
    // An empty Save would store an empty credential that reads as "configured"
    // everywhere downstream. Clear is the way to remove one.
    if value.is_empty() {
        return settings_response(&state, Some("Skipped — nothing to save.".into()));
    }
    let status = match state.credentials.set_secret(field.key(), &value) {
        Ok(()) => "Saved.".to_owned(),
        // `SecretError` carries the account name and never the value; still,
        // this string reaches the window, so it says what failed, not what was
        // being written.
        Err(e) => {
            eprintln!("could not store a credential: {e}");
            "Failed to save — the credential store rejected the write.".to_owned()
        }
    };
    wake_for(&state, field);
    settings_response(&state, Some(status))
}

#[tauri::command]
fn settings_clear_secret(key: String, state: tauri::State<'_, Arc<App>>) -> Value {
    let Some(field) = SecretField::parse(&key) else {
        return settings_response(&state, Some("Skipped — unknown credential.".into()));
    };
    let status = match state.credentials.delete_secret(field.key()) {
        Ok(()) => "Cleared.".to_owned(),
        Err(e) => {
            eprintln!("could not clear a credential: {e}");
            "Failed to clear — the credential store rejected the delete.".to_owned()
        }
    };
    // Clearing matters as much as saving: a panel must drop back to its
    // zero-credential state now, not on the next cadence, or a revoked token
    // keeps painting data it can no longer refresh.
    wake_for(&state, field);
    settings_response(&state, Some(status))
}

/// Wakes exactly the loop a credential feeds.
///
/// Each panel has its own poll pass and its own cadence, and a wake spends one:
/// nudging the GitHub loop after a Neon save would burn a full portfolio fetch
/// on a credential it has no use for. The host token is the exception with no
/// loop to wake — its caller reconciles poll *tasks* instead.
fn wake_for(app: &App, field: SecretField) {
    match field {
        SecretField::GitHub => wake_github(app),
        SecretField::Neon | SecretField::Vercel => wake_usage(app, true),
        // One credential, two panels: the Usage panel's Sentry section and the
        // Sentry Crons panel are separate loops, so a save has to reach both or
        // one of them keeps describing the previous token for up to an hour.
        SecretField::Sentry => {
            wake_usage(app, true);
            app.crons_wake.notify_one();
        }
        SecretField::Azure => app.azure_wake.notify_one(),
        // The bearer token is folded into the *signed connect payload*, so it
        // cannot be swapped on a live socket — the session has to be torn down
        // and re-handshaked, which is exactly what this wake does.
        SecretField::OpenClaw => wake_openclaw(app),
    }
}

/// Test-fixture generation only, decoupled from the live-agent path below
/// (which starts with empty history and only ever records real samples).
/// Built from the committed agent-contract fixture so this is reproducible on
/// a clean checkout with no live agent involved. Shared by every `--dump*`
/// flag so a live and a stale view-model always carry the exact same
/// underlying data -- staleness is meant to change only the connection badge,
/// never the numbers (see `card::tests::a_stale_host_card_keeps_its_data_but_
/// turns_red_and_says_how_old_it_is`), and the Playwright suite depends on
/// that holding for its dumped fixtures too.
fn dump_card(host_name: &str, connection: &Connection) -> Value {
    const FIXTURE: &str = include_str!("../../../crates/wire/tests/fixtures/snapshot.json");
    let snap: wire::Snapshot = serde_json::from_str(FIXTURE).expect("fixture");
    let mut h = HostHistories::new();
    // A full history buffer is what lets the Playwright suite assert "wider
    // charts show proportionally more samples" without needing hundreds of
    // real polls first — see HISTORY_CAPACITY's doc comment.
    for _ in 0..viewmodel::layout::HISTORY_CAPACITY {
        h.record(&snap);
    }
    let mut card = host_card(host_name, &snap, &h, connection);
    card["id"] = json!(host_name);
    card
}

/// The stale message the agent client produces for an unreachable host, so a
/// dumped fixture carries the real string rather than a copy of it.
fn unreachable_message() -> String {
    AgentError::Unreachable("connection refused".into())
        .user_message()
        .into_owned()
}

/// One live host, the offline fallback `app/ui/app.js` fetches when
/// `window.__TAURI__` is absent.
///
/// Deliberately *without* the local card, unlike [`dump_cockpit`]: this fixture
/// exists to exercise one remote host's live/stale transition, and a second card
/// on the page would only make every locator in that test ambiguous.
/// The same envelope as [`dump_single`], for a host with nothing to plot.
fn dump_pending(pending: &Pending) -> Value {
    let mut card = pending_card("ubu-3xdv", pending);
    card["id"] = json!("ubu-3xdv");
    cockpit_payload(
        vec![card],
        1,
        1000.0,
        HostOverflowMode::Stack,
        viewmodel::layout::CORE_ROW_SPAN_DEFAULT,
        &CockpitLayout::hosts_forward(),
    )
}

fn dump_single(connection: &Connection) -> Value {
    cockpit_payload(
        vec![dump_card("ubu-3xdv", connection)],
        1,
        1000.0,
        HostOverflowMode::Stack,
        viewmodel::layout::CORE_ROW_SPAN_DEFAULT,
        // The shipped arrangement: the fixtures are what the Playwright suite
        // renders, and a dump carrying someone's edited layout would make the
        // suite depend on a store this repo does not have.
        &CockpitLayout::hosts_forward(),
    )
}

/// This machine's card as a fixture — hand-made, at a fixed shape, so the file
/// is byte-stable across regenerations and reproduces on any machine.
///
/// The unknowns are the *real* ones the shipped card carries on macOS today:
/// memory pressure has no portable source and the GPU has no dependency-free
/// read, so both render "—" on every run. Baking them in is what lets the
/// Playwright suite assert the em-dash rule against a payload Rust built rather
/// than one hand-written in JS.
fn dump_local_card() -> Value {
    let snapshot = localhost::LocalSnapshot {
        timestamp: "2026-07-31T12:00:00Z".to_string(),
        cpu: localhost::LocalCpu {
            usage: Some(localhost::CpuUsage {
                total: 21.5,
                per_core: vec![18.0, 24.0, 9.5, 34.0, 12.0, 7.5, 41.0, 15.0],
            }),
            model: "Apple M4 Pro".to_string(),
            thermal_state: Some(localhost::ThermalState::Nominal),
        },
        memory: localhost::LocalMemory {
            used_gb: 22.4,
            total_gb: 64.0,
            swap_used_gb: 0.0,
            // Unknown on every platform this shell runs on — see
            // `localhost::LocalMemory::pressure`.
            pressure: None,
        },
        disk: wire::Disk {
            read_mbps: Some(3.2),
            write_mbps: Some(14.8),
        },
        network: wire::Network {
            download_mbps: Some(1.4),
            upload_mbps: Some(0.6),
        },
        // No portable read on either platform; renders "—" via `Gpu::unknown()`.
        gpu: wire::Gpu::unknown(),
        battery: None,
        volumes: vec![wire::Volume {
            mount: "/".to_string(),
            fstype: Some("apfs".to_string()),
            used_gb: 412.0,
            total_gb: 994.0,
        }],
        processes: vec![
            wire::Process {
                pid: 501,
                name: "Xcode".to_string(),
                cpu_percent: 62.0,
                memory_mb: 4096.0,
            },
            wire::Process {
                pid: 733,
                name: "rust-analyzer".to_string(),
                cpu_percent: 18.5,
                memory_mb: 2048.0,
            },
        ],
    };

    let mut histories = HostHistories::new();
    let wired = snapshot.to_wire();
    for _ in 0..viewmodel::layout::HISTORY_CAPACITY {
        histories.record(&wired);
    }
    // The same function the live card goes through, so the fixture cannot
    // diverge from what the app actually paints.
    local::card_from("mac-studio", &snapshot, &histories)
}

/// Three hosts in three different connection states, so the Playwright suite
/// can assert per-host failure isolation against a payload the *shell* built
/// rather than one hand-assembled in JS. A hand-built envelope could not
/// notice `host_columns` or the payload's own key names drifting.
///
/// `hosts` takes the first N of them — 0 is the unconfigured cockpit, which is
/// a real state with its own rendering. The **local card always leads**, exactly
/// as it does in the live payload, so `--hosts 0` is a fresh install (one local
/// card plus the "add a host" line) rather than a blank page.
///
/// `overflow` is the General tab's preference, so `--tabs` at a stacked width
/// dumps the tab bar — the one host-grid rendering no other fixture reaches.
fn dump_cockpit(available: f64, hosts: usize, overflow: HostOverflowMode) -> Value {
    let cards = vec![
        dump_card("ubu-3xdv", &Connection::Live),
        {
            // A host that answered once and can no longer be reached: a blanked
            // card, which is what `view_for` produces for it. This used to dump
            // the last snapshot behind a stale badge, and kept doing so for a
            // while after the app stopped — a fixture depicting a rendering the
            // app cannot produce is worse than no fixture.
            let mut card = pending_card(
                "mac-mini",
                &Pending::Unreachable {
                    message: unreachable_message(),
                    age_secs: Some(42),
                },
            );
            card["id"] = json!("mac-mini");
            card
        },
        {
            let mut card = pending_card("nuc-spare", &Pending::Failed(unreachable_message()));
            card["id"] = json!("nuc-spare");
            card
        },
    ];
    let remote: Vec<Value> = cards.into_iter().take(hosts).collect();
    let remote_count = remote.len();
    cockpit_payload(
        std::iter::once(dump_local_card()).chain(remote).collect(),
        remote_count,
        available,
        overflow,
        viewmodel::layout::CORE_ROW_SPAN_DEFAULT,
        &CockpitLayout::hosts_forward(),
    )
}

/// The Usage and Azure Cost panels as fixtures, at a fixed `now` for the same
/// reason `--dump-containers` is: every relative age in a footer would otherwise
/// drift on each regeneration and no test could assert one.
fn dump_usage(kind: usage::Fixture) -> Value {
    const NOW: u64 = 1_700_000_000;
    // A quota the fixture's own count lands at 94% of, so the amber step is
    // exercised rather than a flat green bar.
    usage::view(
        &usage::fixture_state(kind, NOW),
        10_000,
        usage::NeonRates {
            usd_per_cu_hour: 0.106,
            usd_per_gib_month: 0.35,
        },
        NOW,
    )
}

fn dump_azure(kind: azure::Fixture) -> Value {
    const NOW: u64 = 1_700_000_000;
    azure::view(&azure::fixture_state(kind, NOW), 2_000.0, NOW)
}

/// The Sentry Crons panel as a fixture.
///
/// Both clocks are fixed so the file is byte-stable across regenerations: the
/// panel clock keeps the footer from drifting, and the *wire* clock is what makes
/// the ages exactly `7d 22h` and `0d 22h` — the two figures the age rule is
/// about — on every machine and at every hour.
fn dump_crons(kind: crons::Fixture) -> Value {
    const NOW: u64 = 1_700_000_000;
    let wire_now = chrono::DateTime::parse_from_rfc3339("2026-08-04T15:00:00Z")
        .expect("a fixed wire clock")
        .with_timezone(&chrono::Utc);
    crons::view(&crons::fixture_state(kind, NOW, wire_now), NOW)
}

/// The Settings payload as a fixture, built from a hand-made configuration
/// rather than a real store.
///
/// Fixed uuids (not `Host::new`'s v4) so the file is byte-stable across dumps:
/// a fixture that changes on every regeneration is a fixture a test cannot
/// assert an id against. Deliberately mixed — one enabled host with a token
/// and a hidden volume, one disabled host with neither, two credentials stored
/// and two not — so the Playwright suite exercises both sides of every badge.
fn dump_settings() -> Value {
    let settings = store::Settings {
        local_hidden_volume_mounts: vec!["/Volumes/Time Machine".into()],
        ..store::Settings::default()
    };

    let mut live = Host::new("ubu-3xdv", "100.87.202.125");
    live.id = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0001);
    live.hidden_volume_mounts = vec!["/mnt/scratch".into()];
    let mut spare = Host::new("nuc-spare", "100.64.0.7");
    spare.id = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0002);
    spare.enabled = false;

    let stored = StoredSecrets {
        github: true,
        neon: false,
        sentry: true,
        vercel: true,
        azure: false,
        openclaw: false,
        hosts: [live.id].into_iter().collect(),
    };
    // A gateway configured *and* waiting on approval, so the fixture carries the
    // pairing block — the one part of this tab that only exists in a live
    // state, and therefore the one part a static fixture would otherwise never
    // cover.
    let settings = store::Settings {
        openclaw_gateway_url: "ws://gateway.local:7878".into(),
        // Populated so the frontend suite sees the GitHub org field in its
        // filled state; its empty state is covered by the Rust view tests.
        github_org: "acme".into(),
        ..settings
    };
    let facts = openclaw::fixture_state(openclaw::Fixture::Pairing).settings_facts();
    // A *customised* layout at TWO breakpoints, not the default: the fixture
    // has to carry the Reset button in its enabled state, a removable band, a
    // narrow band that tabs its host cards and a wide one that does not, and a
    // preview whose rows are not the shipped ones — otherwise the Playwright
    // suite only ever sees half the tab.
    let layout = vec![
        store::LayoutProfile::new(
            0.0,
            HostOverflowMode::Tabs.as_str(),
            settings::layout_slots(&[
                (PanelKind::Hosts, PanelSpan::Full),
                (PanelKind::AzureCost, PanelSpan::Half),
                (PanelKind::ClaudeUsage, PanelSpan::Half),
                (PanelKind::GhWorkflows, PanelSpan::Half),
                (PanelKind::GhRunners, PanelSpan::Quarter),
                (PanelKind::OpenclawAgents, PanelSpan::Quarter),
                (PanelKind::Containers, PanelSpan::Full),
            ]),
        ),
        store::LayoutProfile::new(
            1816.0,
            HostOverflowMode::Stack.as_str(),
            settings::layout_slots(&CockpitLayout::DEFAULT_ORDER),
        ),
    ];
    settings::view(
        &settings,
        &[live, spare],
        // Explicit, not `seeded_repos()`: nothing is seeded any more, and this
        // fixture is what the Playwright suite renders the Portfolio tab from
        // — an empty list would silently stop covering it.
        &[
            store::TrackedRepo::new("acme/widget"),
            store::TrackedRepo::new("acme/gadget"),
        ],
        &dump_container_rules(),
        Some(&layout),
        &stored,
        &facts,
    )
}

/// The rules the Settings fixture carries — the seeded three, plus the two
/// renderings seeding alone never reaches.
///
/// The seeds give a scoped Collapse, a second Collapse, and an all-hosts Hide.
/// What they do not give is an **Expect** row (whose Collapse-only fields must
/// therefore be absent), a live **expected count** (the field's non-empty
/// state), or a rule scoped to a host that no longer exists — the case
/// `rule_host_options` grows an extra option for, and the one where a picker
/// silently renders blank if it doesn't.
fn dump_container_rules() -> Vec<ContainerGroupRule> {
    let mut rules = store::seeded_rules();
    rules[1].expected_count = Some(4);
    rules.push(
        ContainerGroupRule::new("build-vm", "", store::ContainerRuleAction::Expect)
            .on_host(store::LOCAL_HOST_SCOPE),
    );
    rules.push(
        ContainerGroupRule::new(
            "legacy-*",
            "legacy jobs",
            store::ContainerRuleAction::Collapse,
        )
        .on_host("retired-box"),
    );
    rules
}

/// The OpenClaw panel as a fixture, one per rendering it has.
fn dump_openclaw(kind: openclaw::Fixture) -> Value {
    openclaw::fixture_view(kind)
}

/// The Containers panel as a fixture.
///
/// Built from [`containers::fixture_state`] — a hand-made state rather than a
/// real poll, because the states worth testing (a collapsed group, a VM
/// recycling, one missing beyond grace, a remote section beside the local one)
/// cannot be produced on demand by whatever machine happens to run this.
///
/// `now` is fixed, not `now_unix()`, so the file is byte-stable across
/// regenerations: relative ages ("recycling 40s") would otherwise drift on
/// every dump and no test could assert one.
fn dump_containers(empty: bool) -> Value {
    const NOW: u64 = 1_700_000_000;
    if empty {
        // The zero-setup machine, plus a failed runtime: one sentence and a
        // footer, which is the other half of the panel's rendering.
        let mut state = containers::ContainersState::new();
        state.apply_local(
            Vec::new(),
            containers::parse::merge(
                vec![(containers::parse::LocalRuntime::Docker, None)],
                std::collections::BTreeMap::new(),
            ),
            NOW - 90,
        );
        return containers::view(&state, &[], &std::collections::BTreeMap::new(), NOW);
    }
    let (state, rules, presence) = containers::fixture_state(NOW);
    containers::view(&state, &rules, &presence, NOW)
}

/// The Repos and GitHub Runners panels as fixtures.
///
/// Built from [`github::fixture_state`] at a **fixed** `now`, for the same
/// reason `--dump-containers` is: every relative age in these payloads
/// (`3h37m` running, `recycling 40s`) would otherwise drift on every
/// regeneration and no test could assert one. `--empty` dumps the
/// no-credential state, which is the other half of both panels' rendering.
fn dump_github(empty: bool, runners: bool) -> Value {
    // 2026-05-29T12:05:00Z — the same instant `crates/github`'s own tests use
    // as "now", so a fixture and a unit test can be read side by side.
    let now = chrono::DateTime::from_timestamp(1_780_056_300, 0).expect("valid timestamp");
    let state = if empty {
        // `apply_unauthenticated`, not a fresh `new()`: a fresh state is the
        // *loading* one now, and dumping that under a name the suite reads as
        // "the no-credential rendering" would quietly re-point every assertion
        // at the wrong copy.
        let mut state = github::GitHubState::new();
        state.apply_unauthenticated();
        state
    } else {
        github::fixture_state(now)
    };
    if runners {
        github::runners_view(&state, u64::try_from(now.timestamp()).unwrap_or(0))
    } else {
        github::repos_view(&state, now)
    }
}

/// Returns the path argument following `flag` if `flag` is present, falling
/// back to `default` when the flag is given with no path.
fn dump_flag_path(args: &[String], flag: &str, default: &str) -> Option<String> {
    let i = args.iter().position(|a| a == flag)?;
    Some(args.get(i + 1).cloned().unwrap_or_else(|| default.into()))
}

/// The value following `flag`, parsed, or `default` when it is absent or
/// unparseable. Backs `--width` (the grid width a `--dump-cockpit` payload is
/// computed for) and `--hosts` (how many of the dumped hosts to include), so
/// one dump flag can produce the wide, stacked and empty fixtures.
fn value_flag<T: std::str::FromStr>(args: &[String], flag: &str, default: T) -> T {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn write_json(path: &str, value: &Value) {
    std::fs::write(path, serde_json::to_string_pretty(value).unwrap()).expect("write view-model");
    println!("wrote {path}");
}

/// Handles every `--dump*` flag, returning `true` when one was given and the
/// process should exit without starting a window.
fn run_dump(args: &[String]) -> bool {
    if let Some(path) = dump_flag_path(args, "--dump", "sample.json") {
        write_json(&path, &dump_single(&Connection::Live));
        return true;
    }
    // The unreachable counterpart to --dump, so tests/frontend/layout.spec.js
    // can assert "a host we cannot contact renders nothing at all" against two
    // Rust-derived fixtures rather than hand-building the second in JS -- a
    // hand-built fixture can't notice viewmodel's own state string or message
    // format drifting out from under it.
    if let Some(path) = dump_flag_path(args, "--dump-unreachable", "sample-unreachable.json") {
        write_json(
            &path,
            &dump_pending(&Pending::Unreachable {
                message: unreachable_message(),
                age_secs: Some(2),
            }),
        );
        return true;
    }
    // The #182 rendering, and the one no other fixture can reach: the poll
    // SUCCEEDED and the card is stale anyway, because the agent's own
    // `/v1/health` says its sampler stopped. Same underlying snapshot as
    // `--dump` again, so the Playwright suite asserts the same "only the badge
    // changes" rule against a case whose badge nothing on this side produced.
    if let Some(path) = dump_flag_path(args, "--dump-sampler-stale", "sample-sampler-stale.json") {
        write_json(
            &path,
            &dump_single(&Connection::SamplerStale {
                // The agent's own `sampleAgeSeconds`, at a fixed value for the
                // same byte-stability reason every other fixture pins its
                // clocks: a relative age computed at dump time would drift on
                // every regeneration.
                sample_age_secs: Some(300),
            }),
        );
        return true;
    }
    if let Some(path) = dump_flag_path(args, "--dump-cockpit", "sample-cockpit.json") {
        // 3 * 900 + 2 * 16 — exactly the width three cards need side by side.
        let default_width = 3.0 * HOST_CARD_MIN_WIDTH + 2.0 * SPACING;
        // `--tabs` is the General tab's `Show as tabs`, not a width: it only
        // changes the payload where the cards were going to stack anyway, so
        // it is dumped alongside `--width` rather than instead of it.
        let overflow = if args.iter().any(|arg| arg == "--tabs") {
            HostOverflowMode::Tabs
        } else {
            HostOverflowMode::Stack
        };
        write_json(
            &path,
            &dump_cockpit(
                value_flag(args, "--width", default_width),
                value_flag(args, "--hosts", 3),
                overflow,
            ),
        );
        return true;
    }
    if let Some(path) = dump_flag_path(args, "--dump-settings", "sample-settings.json") {
        write_json(&path, &dump_settings());
        return true;
    }
    if let Some(path) = dump_flag_path(args, "--dump-containers", "sample-containers.json") {
        write_json(
            &path,
            &dump_containers(args.iter().any(|arg| arg == "--empty")),
        );
        return true;
    }
    let empty = args.iter().any(|arg| arg == "--empty");
    if let Some(path) = dump_flag_path(args, "--dump-repos", "sample-repos.json") {
        write_json(&path, &dump_github(empty, false));
        return true;
    }
    if let Some(path) = dump_flag_path(args, "--dump-runners", "sample-runners.json") {
        write_json(&path, &dump_github(empty, true));
        return true;
    }
    if let Some(path) = dump_flag_path(args, "--dump-usage", "sample-usage.json") {
        let kind = if empty {
            usage::Fixture::Empty
        } else if args.iter().any(|arg| arg == "--unmeasured") {
            usage::Fixture::Unmeasured
        } else {
            usage::Fixture::Measured
        };
        write_json(&path, &dump_usage(kind));
        return true;
    }
    if let Some(path) = dump_flag_path(args, "--dump-azure", "sample-azure.json") {
        let kind = if empty {
            azure::Fixture::Unconfigured
        } else if args.iter().any(|arg| arg == "--fallback") {
            azure::Fixture::Fallback
        } else if args.iter().any(|arg| arg == "--error") {
            azure::Fixture::Failed
        } else {
            azure::Fixture::Measured
        };
        write_json(&path, &dump_azure(kind));
        return true;
    }
    if let Some(path) = dump_flag_path(args, "--dump-services", "sample-services.json") {
        write_json(&path, &services::view(&services::fixture_statuses()));
        return true;
    }
    if let Some(path) = dump_flag_path(args, "--dump-crons", "sample-crons.json") {
        let kind = if empty {
            crons::Fixture::Healthy
        } else if args.iter().any(|arg| arg == "--blind") {
            crons::Fixture::Blind
        } else if args.iter().any(|arg| arg == "--error") {
            crons::Fixture::Failed
        } else if args.iter().any(|arg| arg == "--unconfigured") {
            crons::Fixture::Unconfigured
        } else {
            crons::Fixture::Alerting
        };
        write_json(&path, &dump_crons(kind));
        return true;
    }
    if let Some(path) = dump_flag_path(args, "--dump-openclaw", "sample-openclaw.json") {
        let kind = if empty {
            openclaw::Fixture::Empty
        } else if args.iter().any(|arg| arg == "--pairing") {
            openclaw::Fixture::Pairing
        } else if args.iter().any(|arg| arg == "--idle") {
            openclaw::Fixture::Idle
        } else if args.iter().any(|arg| arg == "--error") {
            openclaw::Fixture::Disconnected
        } else if args.iter().any(|arg| arg == "--unmeasured") {
            openclaw::Fixture::Unmeasured
        } else {
            openclaw::Fixture::Connected
        };
        write_json(&path, &dump_openclaw(kind));
        return true;
    }
    false
}

/// Where the store lives. `DEVCANOPY_STORE_DIR` overrides the platform default
/// so a smoke run or a throwaway experiment can seed a scratch store instead
/// of editing the real one (see the manual IPC smoke test in `app/README.md`).
fn open_store() -> Result<Store, StoreError> {
    match std::env::var_os("DEVCANOPY_STORE_DIR") {
        Some(dir) => Store::open_in(dir),
        None => Store::open(),
    }
}

/// One host as `DEVCANOPY_SEED_HOST` spells it.
#[derive(Debug, PartialEq, Eq)]
struct SeedHost {
    name: String,
    address: String,
    port: u16,
    token: String,
}

/// Parses `"name|address|port|token"`, byte-for-byte the same rules as
/// `RemoteHostsCoordinator.seedFromEnvironmentIfNeeded()` in Swift: empty
/// fields are kept (not skipped) when splitting, name and address are
/// required and must be non-empty, an unparseable or absent port falls back
/// to the agent default, and an absent token is the empty string.
fn parse_seed_host(raw: &str) -> Option<SeedHost> {
    let parts: Vec<&str> = raw.split('|').collect();
    let (name, address) = (*parts.first()?, *parts.get(1)?);
    if name.is_empty() || address.is_empty() {
        return None;
    }
    Some(SeedHost {
        name: name.to_string(),
        address: address.to_string(),
        port: parts
            .get(2)
            .and_then(|p| p.parse().ok())
            .unwrap_or(store::DEFAULT_AGENT_PORT),
        token: parts.get(3).map_or(String::new(), ToString::to_string),
    })
}

/// Provisions a host from `DEVCANOPY_SEED_HOST` if one with that address is
/// not already configured — headless/first-run setup, exactly as in Swift.
///
/// The same-address no-op is what makes this safe to leave set: relaunching
/// with the variable still exported must not accumulate duplicate hosts, and
/// address (not name) is the identity because the name is the editable field.
///
/// Returns the id of the host it added, or `None` when it did nothing.
///
/// The Swift app skips this entirely under `DemoMode.isEnabled`
/// (`DevCanopyApp.swift`), so a synthetic demo host is never written to real
/// storage. This shell has no demo mode yet; when it gains one, the skip goes
/// at this function's only call site, not inside it.
fn seed_from_env(
    store: &mut Store,
    credentials: &dyn CredentialStore,
    raw: Option<&str>,
) -> Result<Option<String>, StoreError> {
    let Some(seed) = raw.and_then(parse_seed_host) else {
        return Ok(None);
    };
    if store.hosts().iter().any(|h| h.address == seed.address) {
        return Ok(None);
    }

    let mut host = Host::new(seed.name, seed.address);
    host.port = seed.port;
    let id = host.id;
    store.upsert_host(host);
    store.save()?;

    if !seed.token.is_empty() {
        // The token goes to the OS credential store keyed by the id above,
        // never into the store file. A credential-store failure is reported
        // and not fatal: the host row is already saved, and the operator can
        // re-enter the token -- losing the row too would be the worse outcome.
        if let Err(e) = credentials.set_secret(SecretKey::HostToken(id), &seed.token) {
            eprintln!("could not store the seeded host's token: {e}");
        }
    }
    Ok(Some(id.to_string()))
}

/// Enabled **remote** hosts in the cockpit's display order.
///
/// Sorted by name, matching the Swift coordinator's `SortDescriptor(\.name)`.
/// The local machine is not in this list and never sorts against it: the Swift
/// cockpit puts it first unconditionally (`HostsPanel.hosts` is
/// `[local] + remoteHosts.hosts`), and so does [`cockpit_view`].
fn display_order(hosts: &[Host]) -> Vec<&Host> {
    let mut enabled: Vec<&Host> = hosts.iter().filter(|h| h.enabled).collect();
    enabled.sort_by(|a, b| a.name.cmp(&b.name));
    enabled
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if run_dump(&args) {
        return;
    }

    let mut store = match open_store() {
        Ok(store) => store,
        Err(e) => {
            eprintln!("could not open the DevCanopy store: {e}");
            std::process::exit(1);
        }
    };
    // Tokens live in the OS credential store, never in the store file. The
    // *service* string matches the Swift `KeychainHelper`
    // (`com.sassydog.devcanopy`), but the *account* does not: Swift stores
    // each host's token under `host_token_<UUID>`
    // (`DevCanopy/Services/KeychainHelper.swift`), `store::SecretKey` stores
    // it under `host-<UUID>`. Nothing is actually reused today -- a token
    // saved by one app is invisible to the other; unifying the account scheme
    // is separate work.
    let credentials = KeyringStore::new();

    // One-shot: copy legacy per-item secrets into the consolidated blob
    // (secrets_v1) so steady state reads exactly one keychain item. Must run
    // before anything can write a secret -- including the seed path just
    // below, the one write that happens before `App` exists -- since an
    // early write would create the blob and shadow unmigrated legacy values.
    // Host ids come from the store as loaded, before seeding: a seeded host
    // is new by construction (`seed_from_env` only adds one when its address
    // isn't already tracked), so its id never had a legacy item to miss.
    // Count only; never values.
    //
    // Skipped entirely under `DEVCANOPY_STORE_DIR`: that variable points
    // `store.json` at a scratch directory, but the credential *service*
    // stays the real one (see `open_store`) -- so a scratch/smoke run would
    // migrate against whatever host list the scratch store happens to have
    // (typically none), write the real `secrets_v1` blob from that host
    // list, and permanently freeze migration (it no-ops once the blob
    // exists), leaving every real host's token unreadable on the next real
    // launch. `migrate_legacy`'s own "blob already exists" guard can't catch
    // this: an empty or wrong host list still looks like "nothing to copy",
    // not a scratch run.
    if std::env::var_os("DEVCANOPY_STORE_DIR").is_none() {
        let mut migrate_keys = SecretKey::static_migration_keys();
        migrate_keys.extend(store.hosts().iter().map(|h| SecretKey::HostToken(h.id)));
        match credentials.migrate_legacy(&migrate_keys) {
            Ok(0) => {}
            Ok(n) => {
                eprintln!(
                    "secrets: migrated {n} credential(s) into the consolidated keychain item"
                );
            }
            Err(e) => eprintln!("secrets: migration failed (legacy items still readable): {e}"),
        }
    }

    let seed = std::env::var("DEVCANOPY_SEED_HOST").ok();
    if let Err(e) = seed_from_env(&mut store, &credentials, seed.as_deref()) {
        eprintln!("could not seed a host from DEVCANOPY_SEED_HOST: {e}");
    }

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let app = Arc::new(App {
        store: Mutex::new(store),
        credentials: Box::new(credentials),
        hosts: Mutex::new(Vec::new()),
        local: Arc::new(Mutex::new(LocalHostState::new())),
        containers: Mutex::new(ContainersState::new()),
        github: Mutex::new(GitHubState::new()),
        github_wake: tokio::sync::Notify::new(),
        usage: Mutex::new(UsageState::new()),
        usage_wake: tokio::sync::Notify::new(),
        usage_providers_due: std::sync::atomic::AtomicBool::new(false),
        crons: Mutex::new(CronsState::new()),
        crons_wake: tokio::sync::Notify::new(),
        azure: Mutex::new(AzureState::new()),
        azure_wake: tokio::sync::Notify::new(),
        openclaw: Mutex::new(OpenClawState::new()),
        openclaw_wake: tokio::sync::Notify::new(),
        approvals: Mutex::new(github::notify::ApprovalWatch::new()),
        service_status: Mutex::new(services::StatusWatch::new()),
        services: Mutex::new(services::ServiceStatuses::new()),
        host_reachability: Mutex::new(services::HostWatch::new()),
        handle: std::sync::OnceLock::new(),
        runtime: rt.handle().clone(),
    });
    // One task per host: an unreachable host's 5s client timeout must not hold
    // up any other host's tick. Startup is the same code path a settings edit
    // takes, so there is one definition of "which hosts are polled".
    reload_hosts(&app);
    // The containers panel polls on its own 10s cadence and reads the poll set
    // rather than owning one, so a host added in Settings joins it on the next
    // tick with no reload of its own.
    rt.spawn(containers_loop(Arc::clone(&app)));
    // `/v1/health` on its own slow cadence, over the same poll set: the only
    // source that can tell a *succeeding* poll it is being served frozen
    // numbers. It reads the poll set rather than owning one, so a host added in
    // Settings joins it on the next tick.
    rt.spawn(health_loop(Arc::clone(&app)));
    rt.spawn(hosts_watch_loop(Arc::clone(&app)));
    rt.spawn(resume_loop(Arc::clone(&app)));
    // The GitHub panels run on the store's refresh interval and read the
    // portfolio and the token on every pass, so an edit in Settings joins them
    // on the next pass — which `wake_github` makes immediate.
    rt.spawn(github_loop(Arc::clone(&app)));
    // This machine, on the host cadence — the card that leads the grid.
    rt.spawn(local::poll_loop(Arc::clone(&app.local), POLL_INTERVAL));
    // Usage: Claude on the store's refresh interval, Neon and Sentry hourly
    // inside the same loop.
    rt.spawn(usage_loop(Arc::clone(&app)));
    // Sentry cron monitors, on the same fixed hourly cadence as the Sentry read
    // inside the usage loop — a persistence watch, not a real-time alarm.
    rt.spawn(crons_loop(Arc::clone(&app)));
    // Azure cost, on the reader's own 4h cadence.
    rt.spawn(azure_loop(Arc::clone(&app)));
    // OpenClaw: a live WebSocket, not a cadence. It idles at once when no
    // gateway is configured and wakes the moment one is saved.
    rt.spawn(openclaw_loop(Arc::clone(&app)));

    tauri::Builder::default()
        // Opening a repo row's Actions page. The webview's grant is one
        // command, scoped to one URL shape — see `capabilities/default.json`.
        .plugin(tauri_plugin_opener::init())
        // Needs-approval banners. Registered so `NotificationExt` resolves in
        // `deliver_approval_notices`; granted nothing, because only Rust ever
        // calls it.
        .plugin(tauri_plugin_notification::init())
        .manage(Arc::clone(&app))
        .setup({
            let app = Arc::clone(&app);
            move |shell| {
                // The poll loops are already running; this is the moment they
                // gain a way to reach the OS.
                let _ = app.handle.set(shell.handle().clone());
                Ok(())
            }
        })
        .invoke_handler(tauri::generate_handler![
            cockpit,
            containers,
            repos,
            runners,
            usage,
            azure_cost,
            services,
            crons,
            openclaw,
            settings_view,
            settings_save_general,
            settings_move_panel,
            settings_set_panel_span,
            settings_set_breakpoint_overflow,
            settings_add_breakpoint,
            settings_remove_breakpoint,
            settings_reset_layout,
            settings_save_github,
            settings_save_providers,
            settings_add_host,
            settings_set_host_enabled,
            settings_remove_host,
            settings_unhide_volume,
            settings_add_container_rule,
            settings_set_container_rule,
            settings_remove_container_rule,
            settings_test_host,
            settings_add_repo,
            settings_remove_repo,
            settings_set_repo_enabled,
            settings_set_repo_workflows,
            settings_save_openclaw,
            settings_openclaw_retry,
            settings_save_secret,
            settings_clear_secret,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start");

    // Keep the runtime alive for the lifetime of the app.
    drop(rt);
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::{MemoryCredentialStore, SecretError};

    /// [`cockpit_view`] / [`cockpit_payload`] / [`dump_cockpit`] with the
    /// **default** (stacking) overflow preference and the **shipped** layout —
    /// what every test that is not about the tab bar or the Layout tab wants.
    /// The tab-bar tests call the real functions with `HostOverflowMode::Tabs`;
    /// the layout tests pass an arrangement of their own.
    fn stacked_view(
        local: Option<Value>,
        hosts: &[Arc<Mutex<HostState>>],
        available: f64,
    ) -> Value {
        cockpit_view(
            local,
            hosts,
            available,
            HostOverflowMode::Stack,
            viewmodel::layout::CORE_ROW_SPAN_DEFAULT,
            &CockpitLayout::hosts_forward(),
        )
    }

    fn stacked_payload(cards: Vec<Value>, remote_count: usize, available: f64) -> Value {
        layout_payload(
            cards,
            remote_count,
            available,
            &CockpitLayout::hosts_forward(),
        )
    }

    /// [`stacked_payload`] over an arrangement the caller chose — the Layout
    /// tab's half of the cockpit contract.
    fn layout_payload(
        cards: Vec<Value>,
        remote_count: usize,
        available: f64,
        layout: &CockpitLayout,
    ) -> Value {
        cockpit_payload(
            cards,
            remote_count,
            available,
            HostOverflowMode::Stack,
            viewmodel::layout::CORE_ROW_SPAN_DEFAULT,
            layout,
        )
    }

    /// The tabbed twin of [`stacked_payload`] — the core row span defaults
    /// here too, so a test that is about tabs says nothing about core grids.
    fn tabbed_payload(cards: Vec<Value>, remote_count: usize, available: f64) -> Value {
        cockpit_payload(
            cards,
            remote_count,
            available,
            HostOverflowMode::Tabs,
            viewmodel::layout::CORE_ROW_SPAN_DEFAULT,
            &CockpitLayout::hosts_forward(),
        )
    }

    fn stacked_dump(available: f64, hosts: usize) -> Value {
        dump_cockpit(available, hosts, HostOverflowMode::Stack)
    }

    fn fixture() -> wire::Snapshot {
        const FIXTURE: &str = include_str!("../../../crates/wire/tests/fixtures/snapshot.json");
        serde_json::from_str(FIXTURE).unwrap()
    }

    fn state_with(
        latest: Option<wire::Snapshot>,
        error: Option<&str>,
        last_success: Option<Instant>,
    ) -> HostState {
        named_state("test-host", latest, error, last_success)
    }

    fn named_state(
        name: &str,
        latest: Option<wire::Snapshot>,
        error: Option<&str>,
        last_success: Option<Instant>,
    ) -> HostState {
        HostState {
            id: format!("id-{name}"),
            name: name.to_string(),
            histories: HostHistories::new(),
            latest,
            error: error.map(str::to_string),
            last_success,
            consecutive_failures: if error.is_some() {
                FAILURE_THRESHOLD
            } else {
                0
            },
            sampler_stale: None,
            sample_age_seconds: None,
        }
    }

    /// A host whose agent has answered `/v1/health` with this verdict — what
    /// [`record_health`] would have written. Built through the real recorder
    /// rather than by setting the fields, so a test cannot pin a combination
    /// the health poll could never produce.
    fn with_health(mut state: HostState, stale: Option<bool>, age: Option<u64>) -> HostState {
        let info = wire::Health {
            // The agent's own pairing: `/v1/health` answers "degraded" exactly
            // when it sets `samplerStale` (agent/src/server.rs).
            status: if stale == Some(true) {
                "degraded".to_string()
            } else {
                "ok".to_string()
            },
            hostname: state.name.clone(),
            version: "0.0.0-test".to_string(),
            sample_age_seconds: age,
            sampler_stale: stale,
        };
        record_health(&mut state, Ok(info));
        state
    }

    fn live_state() -> HostState {
        state_with(Some(fixture()), None, Some(Instant::now()))
    }

    fn shared(state: HostState) -> Arc<Mutex<HostState>> {
        Arc::new(Mutex::new(state))
    }

    /// The fixture's CPU reading as a card renders it. Derived from the
    /// parsed fixture rather than written out as `"34%"`, so editing
    /// `snapshot.json` can't leave a literal behind that has to be chased
    /// across this file.
    fn fixture_cpu_value() -> String {
        format!("{}%", fixture().cpu.total_usage.round() as i64)
    }

    // MARK: cadences

    /// Which clock each data source runs on, pinned against the Swift service
    /// that owns it.
    ///
    /// The user-facing refresh interval governs the two GitHub panels and
    /// Claude usage **and nothing else** — every other source has a fixed
    /// cadence chosen by what it is polling, and wiring one of them to the
    /// user's setting would be a real bug that no other test would catch. A
    /// 4h Azure export polled every 30s is 480 pointless blob listings an
    /// hour; a 1s host poll slowed to 300s is a dead chart axis, since one
    /// history sample is one fixed time slice.
    ///
    /// | source | cadence | governed by `refresh_interval_secs`? | Swift |
    /// |---|---|---|---|
    /// | Hosts (local + remote) | 1s | no | `RemoteHostMetricsService.swift:94` |
    /// | Containers | 10s | no | `LocalContainerService.swift:132` |
    /// | Repos + Runners | user's | **yes** — `github_loop` | `DevCanopyApp.swift:136-137` |
    /// | Claude usage | user's | **yes** — `usage_loop` | `DevCanopyApp.swift:134` |
    /// | Neon + Sentry | 1h | no | `NeonUsageService.swift:81` |
    /// | Sentry crons | 1h | no | no Swift twin — `crons_loop` |
    /// | Azure Cost | 4h | no | `AzureCostService.swift:343` |
    /// | OpenClaw | none — event-driven | no | `OpenClawService.swift:72` |
    ///
    /// The "yes" rows are structural rather than constants, so they are pinned
    /// by where they read the store rather than here: `github_loop` reads
    /// `refresh_interval_secs` at its top and `usage_loop` at its own, and no
    /// other loop in this file reads it at all.
    ///
    /// The crons row shares `PROVIDER_POLL_INTERVAL_SECS` with the Sentry read
    /// inside `usage_loop` rather than declaring a second hour of its own: it is
    /// the same API on the same rhythm, and two constants would be free to
    /// drift. The consequence to accept is that a newly-red monitor can be
    /// invisible for up to an hour — this is a *persistence* watch, not a
    /// real-time alarm, and the daily Slack digest remains the prompt signal.
    #[test]
    fn every_data_source_polls_on_its_swift_services_cadence() {
        let cadences: [(&str, u64, u64); 4] = [
            ("hosts", POLL_INTERVAL.as_secs(), 1),
            ("containers", containers::POLL_INTERVAL_SECS, 10),
            ("neon + sentry", usage::PROVIDER_POLL_INTERVAL_SECS, 60 * 60),
            (
                "azure cost",
                azurecost::POLL_INTERVAL.as_secs(),
                4 * 60 * 60,
            ),
        ];
        for (source, cadence, swift) in cadences {
            assert_eq!(cadence, swift, "{source} drifted from its Swift service");
        }

        // The user's choices, which the two governed loops read. Pinned here
        // too because a fourth choice — or a changed default — silently
        // redefines what "the refresh interval" means for those loops.
        assert_eq!(
            store::settings::REFRESH_INTERVAL_CHOICES,
            [30, 60, 300],
            "the refresh-interval choices are Swift's RefreshInterval cases"
        );
        assert_eq!(store::settings::DEFAULT_REFRESH_INTERVAL_SECS, 60);

        // No fixed cadence may coincide with the default refresh interval:
        // that is what would let a loop be wired to the wrong clock and still
        // look correct out of the box.
        for (source, cadence, _) in cadences {
            assert_ne!(
                cadence,
                u64::from(store::settings::DEFAULT_REFRESH_INTERVAL_SECS),
                "{source}'s fixed cadence must not be confusable with the default refresh interval"
            );
        }
    }

    // MARK: view_for

    #[test]
    fn live_when_a_snapshot_exists_and_the_last_poll_succeeded() {
        let s = state_with(Some(fixture()), None, Some(Instant::now()));
        let vm = view_for(&s);
        assert_eq!(vm["connection"]["state"], "live");
        assert_eq!(vm["connection"]["color"], "#33d17a");
        assert!(vm["connection"]["message"].is_null());
        assert_eq!(vm["cpuValue"], fixture_cpu_value());
    }

    /// CRITICAL 1's exact regression case: a snapshot exists, but the most
    /// recent poll failed. The old `(Some(snap), _) => host_card(...,
    /// &Connection::Live)` wildcard landed here too and reported "live"
    /// forever. Reintroducing that wildcard must fail this test — see the
    /// task report for the round where that was confirmed by hand.
    ///
    /// A host we can no longer reach now renders **blank**, not as its last
    /// snapshot behind a badge — the same defect one step further.
    ///
    /// This asserted the opposite until ubu-3xdv went down during the
    /// 2026-08-06 GitHub outage and the card sat there showing four-minute-old
    /// numbers as if they were now. Every figure on a host card is a
    /// present-tense claim, and at a glance the numbers are what you read while
    /// the badge is what you do not — so the whole card goes, which is the
    /// em-dash rule applied at card scale rather than per figure.
    #[test]
    fn an_unreachable_host_blanks_its_card_and_dates_the_outage() {
        let s = state_with(
            Some(fixture()),
            Some("Couldn't reach the agent. Check the host is up and the agent is running."),
            Some(Instant::now()),
        );
        let vm = view_for(&s);
        assert_eq!(vm["connection"]["state"], "unreachable");
        assert_eq!(vm["connection"]["color"], "#e05a4f");

        // Not one figure survives: the card is an error, not a reading.
        let live = view_for(&state_with(Some(fixture()), None, Some(Instant::now())));
        assert!(!live["cpuValue"].is_null(), "the live card has figures");
        for field in ["cpuValue", "cores", "volumes", "memValue"] {
            assert!(
                vm[field].is_null(),
                "{field} is a present-tense claim about a host that is not answering"
            );
        }

        let msg = vm["error"]["message"].as_str().expect("error message");
        assert!(msg.contains("Couldn't reach the agent"), "{msg:?}");
        assert!(
            msg.contains("last update") && msg.contains("ago"),
            "when it went quiet is the one fact still true: {msg:?}"
        );
        assert_eq!(vm["error"]["hostName"], "test-host");
    }

    /// …and the loss is only on screen. `latest` and `histories` stay in state,
    /// so the sparklines come back intact with the host rather than restarting
    /// from an empty buffer.
    #[test]
    fn blanking_the_card_does_not_discard_the_state_behind_it() {
        let s = state_with(
            Some(fixture()),
            Some("Couldn't reach the agent."),
            Some(Instant::now()),
        );
        assert!(
            s.latest.is_some(),
            "the snapshot is retained, just not shown"
        );

        let recovered = state_with(Some(fixture()), None, Some(Instant::now()));
        let vm = view_for(&recovered);
        assert!(
            !vm["cpuValue"].is_null(),
            "and returns the moment it answers"
        );
    }

    #[test]
    fn failed_when_never_connected_and_the_last_attempt_failed() {
        let s = state_with(None, Some("Couldn't reach the agent."), None);
        let vm = view_for(&s);
        assert_eq!(vm["connection"]["state"], "failed");
        assert_eq!(vm["connection"]["color"], "#e05a4f");
        assert_eq!(vm["error"]["message"], "Couldn't reach the agent.");
        assert_eq!(vm["error"]["hostName"], "test-host");
        assert!(
            vm.get("cpuValue").is_none(),
            "a host that never connected must never carry data fields"
        );
    }

    #[test]
    fn connecting_when_never_connected_and_still_waiting() {
        let s = state_with(None, None, None);
        let vm = view_for(&s);
        assert_eq!(vm["connection"]["state"], "connecting");
        assert_eq!(vm["connection"]["color"], "#e09a26");
        assert_eq!(vm["error"]["message"], "waiting for first sample…");
        assert!(vm.get("cpuValue").is_none());
    }

    /// Every card carries its host's store id. The frontend keys its DOM
    /// nodes on this, and the display name cannot stand in for it: names are
    /// user-editable and nothing stops two hosts sharing one.
    #[test]
    fn every_card_carries_its_host_id_whatever_its_connection_state() {
        for state in [
            state_with(Some(fixture()), None, Some(Instant::now())),
            state_with(Some(fixture()), Some("down"), Some(Instant::now())),
            state_with(None, Some("down"), None),
            state_with(None, None, None),
        ] {
            assert_eq!(view_for(&state)["id"], "id-test-host");
        }
    }

    // MARK: sampler staleness (#182)

    /// The defect, exactly: every poll succeeds, so all four `(latest, error)`
    /// states say "live" — while the agent is serving whatever its dead sampler
    /// last produced, or `empty_snapshot()`'s zeros if it never produced one.
    /// A green dot over that is the cockpit lying at its most confident.
    #[test]
    fn a_stalled_sampler_is_stale_not_live_even_though_every_poll_succeeded() {
        let s = with_health(live_state(), Some(true), Some(300));
        let vm = view_for(&s);

        assert_eq!(vm["connection"]["state"], "stale");
        assert_eq!(vm["connection"]["color"], "#e05a4f");
        let msg = vm["connection"]["message"].as_str().unwrap();
        assert!(msg.contains("sampler"), "got {msg:?}");

        // Dated by the AGENT's clock. This side's last successful request is a
        // second old — the number that would make five-minute-old data read as
        // current — so a badge saying anything but "5m" here is measuring the
        // wrong thing.
        assert!(msg.contains("5m ago"), "got {msg:?}");

        // Real data, kept: the numbers are whatever last arrived, identical to
        // what the live arm would have painted. Staleness changes the badge.
        let live = view_for(&live_state());
        assert_eq!(vm["cpuValue"], live["cpuValue"]);
        assert_eq!(vm["cores"], live["cores"]);
        assert_ne!(vm["connection"], live["connection"]);
    }

    /// Only `Some(true)` may redden a card. `Some(false)` is a healthy agent
    /// and `None` is "no health poll has landed yet" — and a cockpit that
    /// treated the second as a stall would paint every host red for the first
    /// ten seconds after launch.
    #[test]
    fn a_healthy_or_unheard_from_sampler_leaves_the_card_live() {
        for (stale, label) in [(Some(false), "healthy"), (None, "not yet heard from")] {
            let s = with_health(live_state(), stale, Some(1));
            assert_eq!(
                view_for(&s)["connection"]["state"],
                "live",
                "a {label} sampler must not redden a card"
            );
        }
        // …and the untouched state — no health poll at all — is the same case.
        assert_eq!(view_for(&live_state())["connection"]["state"], "live");
    }

    /// The critical negative from #182: `/v1/health` is a second request to a
    /// host whose data is arriving fine, and a probe nobody asked for must not
    /// be able to redden the card. Whole-payload equality rather than a badge
    /// check, because "changes nothing visible" is a claim about the entire
    /// view-model — a failure that quietly blanked a figure would pass a
    /// narrower assertion.
    #[test]
    fn a_failed_health_poll_changes_nothing_visible_while_snapshots_flow() {
        let mut s = live_state();
        let before = view_for(&s);

        for err in [
            AgentError::Unreachable("connection refused".into()),
            AgentError::AuthFailed,
            AgentError::HttpStatus(503),
            AgentError::DecodeFailed("expected value".into()),
        ] {
            record_health(&mut s, Err(err));
            assert_eq!(
                view_for(&s),
                before,
                "a failed health poll repainted a card whose snapshots are fine"
            );
        }

        // …and none of the snapshot loop's own state moved, so the *next*
        // failed snapshot poll is still the first of its streak rather than the
        // one that trips the debounce.
        assert!(s.error.is_none());
        assert_eq!(s.consecutive_failures, 0);
        assert!(s.latest.is_some());
    }

    /// Withheld is not reset. A health poll that fails is not evidence the
    /// sampler recovered, and putting the green dot back over frozen numbers on
    /// the strength of a request we could not make would reintroduce the defect
    /// on any flappy link.
    #[test]
    fn a_failed_health_poll_does_not_clear_a_known_stall() {
        let mut s = with_health(live_state(), Some(true), Some(300));
        assert_eq!(view_for(&s)["connection"]["state"], "stale");

        record_health(&mut s, Err(AgentError::Unreachable("blip".into())));
        assert_eq!(
            view_for(&s)["connection"]["state"],
            "stale",
            "a health poll we could not make said nothing about the sampler"
        );

        // Recovery arrives the one way it can: a health poll that lands and
        // says so.
        let s = with_health(s, Some(false), Some(1));
        assert_eq!(view_for(&s)["connection"]["state"], "live");
    }

    /// When the link itself is down, the transport failure is the more
    /// proximate cause *and* the fresher fact — `sampler_stale` is by then up
    /// to a health cadence old. Naming the sampler would send an operator to
    /// restart a daemon they cannot reach.
    #[test]
    fn a_poll_failure_names_the_link_even_when_the_sampler_was_last_seen_stalled() {
        let s = with_health(
            state_with(
                Some(fixture()),
                Some("Couldn't reach the agent. Check the host is up and the agent is running."),
                Some(Instant::now()),
            ),
            Some(true),
            Some(300),
        );
        let vm = view_for(&s);
        assert_eq!(vm["connection"]["state"], "unreachable");
        // The blanked card carries the reason in `error`, not in the badge.
        let msg = vm["error"]["message"].as_str().unwrap().to_string();
        assert!(msg.contains("Couldn't reach the agent"), "got {msg:?}");
        assert!(!msg.contains("sampler"), "got {msg:?}");
    }

    /// A host with no snapshot has nothing to freeze, so a stalled sampler must
    /// not conjure a card for it: "connecting" and "failed" are still the whole
    /// truth, and inventing a data card here would be the zeros-behind-a-badge
    /// version of the same bug.
    #[test]
    fn a_stalled_sampler_never_fabricates_a_card_for_a_host_with_no_snapshot() {
        let connecting = with_health(state_with(None, None, None), Some(true), Some(300));
        assert_eq!(view_for(&connecting)["connection"]["state"], "connecting");
        assert!(view_for(&connecting).get("cpuValue").is_none());

        let failed = with_health(
            state_with(None, Some("Couldn't reach the agent."), None),
            Some(true),
            Some(300),
        );
        assert_eq!(view_for(&failed)["connection"]["state"], "failed");
        assert!(view_for(&failed).get("cpuValue").is_none());
    }

    /// An agent too old to report the flag (pre-#35) must decode and be
    /// believed about what it *did* say: nothing. It stays live, and if it is
    /// ever reported stalled without an age the badge says "unknown" rather
    /// than a fabricated `0s`.
    #[test]
    fn an_agent_that_reports_no_sampler_fields_is_not_treated_as_stalled() {
        let quiet = with_health(live_state(), None, None);
        assert_eq!(view_for(&quiet)["connection"]["state"], "live");

        let ageless = with_health(live_state(), Some(true), None);
        let msg = view_for(&ageless)["connection"]["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(msg.contains("last update unknown"), "got {msg:?}");
        assert!(!msg.contains("0s ago"), "got {msg:?}");
    }

    /// The health poll is a second cadence over the same hosts, and its whole
    /// justification is being cheap: at the snapshot cadence it would double
    /// the cockpit's tailnet traffic to learn a flag that moves on the order of
    /// minutes. It is also not the user's refresh interval — this is a
    /// correctness probe, not a panel.
    #[test]
    fn the_health_poll_is_slower_than_the_snapshot_poll_and_is_not_the_users_interval() {
        assert!(
            HEALTH_POLL_INTERVAL > POLL_INTERVAL,
            "a health probe on the metrics cadence is a request per host per second"
        );
        assert_eq!(HEALTH_POLL_INTERVAL.as_secs(), 10);
        assert_ne!(
            HEALTH_POLL_INTERVAL.as_secs(),
            u64::from(store::settings::DEFAULT_REFRESH_INTERVAL_SECS)
        );
    }

    // MARK: record_poll

    #[test]
    fn a_successful_poll_records_the_snapshot_its_history_and_the_success_time() {
        let mut s = state_with(None, Some("Couldn't reach the agent."), None);
        let at = Instant::now();
        record_poll(&mut s, Ok(fixture()), at);
        assert!(s.latest.is_some());
        assert_eq!(s.last_success, Some(at));
        assert!(s.error.is_none(), "a success must clear the prior error");
        assert_eq!(s.histories.cpu.len(), 1);
        assert_eq!(view_for(&s)["connection"]["state"], "live");
    }

    /// Parity with `RemoteHostMetricsService.failureThreshold`: one missed
    /// poll is a blip, not an outage. A card that flipped red on the first
    /// failure would strobe on any flappy link, which is exactly why the
    /// Swift side debounces.
    #[test]
    fn one_failed_poll_does_not_flip_a_live_host() {
        let mut s = state_with(None, None, None);
        record_poll(&mut s, Ok(fixture()), Instant::now());

        record_poll(
            &mut s,
            Err(AgentError::Unreachable("connection refused".into())),
            Instant::now(),
        );
        assert_eq!(s.consecutive_failures, 1);
        assert!(s.error.is_none(), "one failure must not publish an error");
        assert_eq!(view_for(&s)["connection"]["state"], "live");

        record_poll(
            &mut s,
            Err(AgentError::Unreachable("connection refused".into())),
            Instant::now(),
        );
        assert_eq!(s.consecutive_failures, FAILURE_THRESHOLD);
        assert_eq!(view_for(&s)["connection"]["state"], "unreachable");
    }

    /// The debounce must not survive a success: a host that flaps
    /// fail/succeed/fail must never accumulate its way to "down" on two
    /// failures that were never consecutive.
    #[test]
    fn a_success_resets_the_failure_streak() {
        let mut s = state_with(None, None, None);
        record_poll(&mut s, Ok(fixture()), Instant::now());
        record_poll(
            &mut s,
            Err(AgentError::Unreachable("blip".into())),
            Instant::now(),
        );
        record_poll(&mut s, Ok(fixture()), Instant::now());
        assert_eq!(s.consecutive_failures, 0);

        record_poll(
            &mut s,
            Err(AgentError::Unreachable("blip".into())),
            Instant::now(),
        );
        assert!(
            s.error.is_none(),
            "two non-consecutive failures must not read as an outage"
        );
        assert_eq!(view_for(&s)["connection"]["state"], "live");
    }

    /// A never-connected host stays "connecting" through its first failure
    /// too — the debounce applies before the first sample, exactly as the
    /// Swift service starts in `.connecting` and only flips on the streak.
    #[test]
    fn a_never_connected_host_stays_connecting_through_one_failure() {
        let mut s = state_with(None, None, None);
        record_poll(
            &mut s,
            Err(AgentError::Unreachable("no route".into())),
            Instant::now(),
        );
        assert_eq!(view_for(&s)["connection"]["state"], "connecting");
        record_poll(
            &mut s,
            Err(AgentError::Unreachable("no route".into())),
            Instant::now(),
        );
        assert_eq!(view_for(&s)["connection"]["state"], "failed");
    }

    /// A host that goes down stays down: the poll loop keeps ticking and each
    /// failure runs the same `Err` arm. `last_success` must record when we
    /// last actually heard from the host, not when we last tried, so the
    /// stale badge's age keeps GROWING across a run of failures. One failure
    /// is not enough to prove that — an implementation that stamped
    /// `last_success` on every poll, or cleared it on failure, passes a
    /// single-failure test and breaks here.
    #[test]
    fn last_success_survives_a_run_of_consecutive_poll_failures() {
        let mut s = state_with(None, None, None);
        let first_success = Instant::now();
        record_poll(&mut s, Ok(fixture()), first_success);

        for i in 1..=5 {
            record_poll(
                &mut s,
                Err(AgentError::Unreachable("connection refused".into())),
                Instant::now(),
            );
            assert_eq!(
                s.last_success,
                Some(first_success),
                "failure {i} moved last_success off the last real sample"
            );
            assert!(
                s.latest.is_some(),
                "failure {i} discarded the retained snapshot"
            );
            assert_eq!(
                s.histories.cpu.len(),
                1,
                "failure {i} recorded a sample it never received"
            );
        }

        // …so the blanked card still dates the outage instead of falling back
        // to the "unknown" branch `view_for` reserves for a missing
        // `last_success`.
        let vm = view_for(&s);
        assert_eq!(vm["connection"]["state"], "unreachable");
        let msg = vm["error"]["message"].as_str().unwrap();
        assert!(msg.contains("ago"), "expected a relative age, got {msg:?}");
        assert!(
            !msg.contains("unknown"),
            "the age is known -- a run of failures must not erase it: {msg:?}"
        );
        // The snapshot is retained in state, so recovery is instant -- it is
        // simply not rendered while the host is unreachable.
        assert!(
            vm["cpuValue"].is_null(),
            "the card is blanked, not stale-badged"
        );
        assert!(s.latest.is_some(), "…and the reading behind it survives");
    }

    #[test]
    fn a_success_after_failures_clears_the_error_and_re_dates_the_host() {
        let mut s = state_with(None, None, None);
        let first_success = Instant::now();
        record_poll(&mut s, Ok(fixture()), first_success);
        record_poll(&mut s, Err(AgentError::AuthFailed), Instant::now());
        record_poll(&mut s, Err(AgentError::AuthFailed), Instant::now());
        assert_eq!(view_for(&s)["connection"]["state"], "unreachable");

        let recovered = Instant::now();
        record_poll(&mut s, Ok(fixture()), recovered);
        assert_eq!(s.last_success, Some(recovered));
        assert!(s.error.is_none());
        assert_eq!(view_for(&s)["connection"]["state"], "live");
    }

    /// A missing token is a locally-known configuration fact, not a flapping
    /// link, so it publishes on the first tick rather than waiting out the
    /// debounce.
    #[test]
    fn a_missing_token_reports_immediately_and_names_the_right_layer() {
        let mut s = state_with(None, None, None);
        record_token_unavailable(&mut s, MISSING_HOST_TOKEN_MESSAGE);
        let vm = view_for(&s);
        assert_eq!(vm["connection"]["state"], "failed");
        assert_eq!(
            vm["error"]["message"],
            "No agent token configured for this host. Add one in Settings."
        );
    }

    /// The card the bug produced: a credential store that would not answer used
    /// to render as a host nobody had configured, sending the operator to add a
    /// token that is already there. Same immediacy, different layer named.
    #[test]
    fn an_unreadable_token_reports_the_store_and_never_claims_the_host_is_unconfigured() {
        let mut s = state_with(None, None, None);
        record_token_unavailable(&mut s, HOST_TOKEN_UNREADABLE_MESSAGE);
        let vm = view_for(&s);
        assert_eq!(vm["connection"]["state"], "failed");
        let message = vm["error"]["message"].as_str().expect("a message");
        assert_eq!(message, HOST_TOKEN_UNREADABLE_MESSAGE);
        assert!(
            message.contains("credential store"),
            "the operator has to be told which layer to look at: {message:?}"
        );
        assert_ne!(message, MISSING_HOST_TOKEN_MESSAGE);
    }

    // MARK: the cockpit payload

    #[test]
    fn the_cockpit_payload_carries_one_card_per_host_in_order() {
        let hosts = vec![
            shared(named_state(
                "alpha",
                Some(fixture()),
                None,
                Some(Instant::now()),
            )),
            shared(named_state(
                "beta",
                Some(fixture()),
                None,
                Some(Instant::now()),
            )),
        ];
        let vm = stacked_view(None, &hosts, 2000.0);
        let cards = vm["hosts"].as_array().expect("hosts array");
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0]["hostName"], "alpha");
        assert_eq!(cards[1]["hostName"], "beta");
        assert_eq!(cards[0]["id"], "id-alpha");
    }

    /// Per-host failure isolation, at the level the frontend actually reads:
    /// one dead host must render its own error card while every other host
    /// keeps its live numbers. A shared error field (or one shared lock held
    /// across a poll) is what this would catch.
    #[test]
    fn one_dead_host_does_not_disturb_the_others() {
        let hosts = vec![
            shared(named_state(
                "alpha",
                Some(fixture()),
                None,
                Some(Instant::now()),
            )),
            shared(named_state(
                "beta",
                Some(fixture()),
                Some("Couldn't reach the agent."),
                Some(Instant::now()),
            )),
            shared(named_state(
                "gamma",
                None,
                Some("Agent returned HTTP 503."),
                None,
            )),
        ];
        let vm = stacked_view(None, &hosts, 3000.0);
        let cards = vm["hosts"].as_array().expect("hosts array");

        assert_eq!(cards[0]["connection"]["state"], "live");
        assert_eq!(cards[0]["cpuValue"], fixture_cpu_value());

        assert_eq!(cards[1]["connection"]["state"], "unreachable");
        assert!(
            cards[1]["cpuValue"].is_null(),
            "an unreachable host shows no figures at all -- see \
             `an_unreachable_host_blanks_its_card_and_dates_the_outage`"
        );
        assert_eq!(cards[1]["error"]["hostName"], "beta");

        assert_eq!(cards[2]["connection"]["state"], "failed");
        assert_eq!(cards[2]["error"]["message"], "Agent returned HTTP 503.");
        assert!(
            cards[2].get("cpuValue").is_none(),
            "a host that never connected must never borrow another host's data"
        );
    }

    /// The grid decision is Rust's, and it is the tested breakpoint math —
    /// not a CSS `auto-fit` restating the same rule with its own numbers.
    #[test]
    fn the_payload_decides_the_column_count_from_the_available_width() {
        let hosts: Vec<_> = ["a", "b", "c"]
            .iter()
            .map(|n| shared(named_state(n, Some(fixture()), None, Some(Instant::now()))))
            .collect();

        // 3 * 900 + 2 * 16 = 2732
        assert_eq!(stacked_view(None, &hosts, 2732.0)["hostColumns"], 3);
        assert_eq!(stacked_view(None, &hosts, 2731.0)["hostColumns"], 2);
        assert_eq!(stacked_view(None, &hosts, 1000.0)["hostColumns"], 1);
        // Unknown width stacks rather than assuming wide -- assuming wide is
        // what let a dead measurement masquerade as a deliberate layout.
        assert_eq!(stacked_view(None, &hosts, 0.0)["hostColumns"], 1);
    }

    // MARK: reserved volume slots

    /// N cards carrying only volume counts — enough of a card for
    /// [`volume_slots`], which reads nothing else.
    fn volume_cards(counts: &[usize]) -> Vec<Value> {
        counts
            .iter()
            .map(|n| json!({ "volumes": vec![json!({ "mount": "/" }); *n] }))
            .collect()
    }

    /// Side by side, every card reserves the busiest card's volume count — the
    /// whole point: a 1-volume card and an 8-volume card render the same number
    /// of tiles, so the sections below the block start at the same height.
    #[test]
    fn side_by_side_cards_reserve_the_busiest_cards_volume_count() {
        let cards = volume_cards(&[1, 8, 3]);
        // 3 * 900 + 2 * 16 = 2732, wide enough for all three abreast.
        let payload = stacked_payload(cards, 2, 2732.0);
        assert_eq!(payload["hostColumns"], 3);
        assert_eq!(payload["volumeSlots"], 8);
    }

    /// Stacked, nothing is reserved: cards in one column never share a row, so
    /// padding a short card would be dead space for an alignment nobody sees.
    #[test]
    fn a_stacked_column_reserves_no_volume_slots() {
        let payload = stacked_payload(volume_cards(&[1, 8, 3]), 2, 1000.0);
        assert_eq!(payload["hostColumns"], 1);
        assert_eq!(payload["volumeSlots"], 0);
    }

    /// Tabs show one card at a time, and `host_tabs` can only fire at
    /// `columns <= 1` — so the column test covers this case too, and this pins
    /// that it really does rather than leaving it to the reader.
    #[test]
    fn tabbed_cards_reserve_no_volume_slots() {
        let payload = tabbed_payload(volume_cards(&[1, 8, 3]), 2, 1000.0);
        assert!(!payload["hostTabs"].is_null(), "expected the tab bar");
        assert_eq!(payload["volumeSlots"], 0);
    }

    /// A card whose volumes key is missing or empty counts as zero rather than
    /// vanishing from the maximum — an unreachable host still has to line up
    /// with its neighbours.
    #[test]
    fn cards_without_volumes_count_as_zero_and_still_take_the_reservation() {
        let mut cards = volume_cards(&[0, 5]);
        cards.push(json!({ "error": { "hostName": "nuc-spare" } }));
        let payload = stacked_payload(cards, 2, 2732.0);
        assert_eq!(payload["volumeSlots"], 5);
    }

    // MARK: the shared core block

    /// N cards carrying only core arrays and a ladder, which is all
    /// [`align_core_ladders`] reads or rewrites.
    fn core_cards(counts: &[usize]) -> Vec<Value> {
        counts
            .iter()
            .map(|n| {
                json!({
                    "cores": vec![json!({"label": "Core 0"}); *n],
                    "coreBlockHeight": 220.0,
                    "coreLadder": [],
                })
            })
            .collect()
    }

    /// The height each card's ladder reports at `width`. Cards with no ladder
    /// — an unreachable host reports no cores — contribute no reading rather
    /// than a fabricated one.
    fn block_at(payload: &Value, width: f64) -> Vec<f64> {
        payload["hosts"]
            .as_array()
            .expect("hosts")
            .iter()
            .filter_map(|card| {
                card["coreLadder"]
                    .as_array()?
                    .iter()
                    .rfind(|r| r["minWidth"].as_f64().expect("minWidth") <= width)
                    .map(|r| r["height"].as_f64().expect("height"))
            })
            .collect()
    }

    /// The case this exists for: a 10-core Mac beside the 36-core ubu-3xdv.
    /// Alone the Mac's block is 220 and ubu's is 334; sharing a row they are
    /// both 334, so every section below the block starts at the same height.
    #[test]
    fn side_by_side_cards_share_the_busiest_cards_core_block() {
        // 2 * 900 + 16 = 1816, the width two cards need to sit abreast.
        let payload = stacked_payload(core_cards(&[10, 36]), 1, 1816.0);
        assert_eq!(payload["hostColumns"], 2);
        let heights = block_at(&payload, 899.0);
        assert_eq!(heights, vec![334.0, 334.0], "both cards take ubu's block");
    }

    /// Stacked, each card keeps its own answer — there is no neighbour to line
    /// up with, and padding a short card would be dead space.
    #[test]
    fn a_stacked_column_leaves_each_core_block_alone() {
        let payload = stacked_payload(core_cards(&[10, 36]), 1, 1000.0);
        assert_eq!(payload["hostColumns"], 1);
        assert_eq!(block_at(&payload, 899.0), vec![220.0, 334.0]);
    }

    /// Tabs show one card at a time; same reasoning as stacked.
    #[test]
    fn tabbed_cards_leave_each_core_block_alone() {
        let payload = tabbed_payload(core_cards(&[10, 36]), 1, 1000.0);
        assert!(!payload["hostTabs"].is_null(), "expected the tab bar");
        assert_eq!(block_at(&payload, 899.0), vec![220.0, 334.0]);
    }

    /// A card with no cores — an unreachable host — contributes nothing to the
    /// maximum and keeps a ladder-free base height rather than breaking the
    /// rewrite for its neighbours.
    #[test]
    fn a_coreless_card_neither_lifts_nor_breaks_the_shared_block() {
        let mut cards = core_cards(&[10, 36]);
        cards.push(json!({ "error": { "hostName": "nuc-spare" } }));
        let payload = stacked_payload(cards, 2, 2732.0);
        let hosts = payload["hosts"].as_array().expect("hosts");
        assert_eq!(hosts[2]["coreBlockHeight"], 220.0);
        assert!(hosts[2]["coreLadder"].is_null(), "no cores, no ladder");
        assert_eq!(block_at(&payload, 899.0), vec![334.0, 334.0]);
    }

    /// `coreRowSpan` was persisted and editable in Settings but never reached
    /// the card — `host_card` hardcoded the default. It arrives through the
    /// same rewrite that carries the shared height.
    #[test]
    fn the_core_row_span_preference_reaches_the_block() {
        let payload = cockpit_payload(
            core_cards(&[8]),
            0,
            1000.0,
            HostOverflowMode::Stack,
            3, // three section-rows rather than the default two
            &CockpitLayout::hosts_forward(),
        );
        let card = &payload["hosts"][0];
        assert_eq!(card["coreBlockHeight"], 330.0, "3 * CORE_ROW_UNIT");
        let tallest = card["coreLadder"]
            .as_array()
            .expect("ladder")
            .iter()
            .map(|r| r["height"].as_f64().expect("height"))
            .fold(0.0_f64, f64::max);
        assert!(
            tallest >= 330.0,
            "the span raises the block, not just the base"
        );
    }

    // MARK: the tab-bar decision

    /// N host cards, named, in payload order — enough of a card for the tab
    /// bar, which reads only `id` and the host name.
    fn tab_cards(names: &[&str]) -> Vec<Value> {
        names
            .iter()
            .map(|name| json!({ "id": *name, "hostName": *name }))
            .collect()
    }

    /// The mode's whole purpose: below the side-by-side breakpoint, `tabs`
    /// produces a tab bar where `stack` produces nothing — with the *same*
    /// cards and the same width, so nothing but the preference moved.
    #[test]
    fn the_tabs_preference_collapses_the_stacked_grid_into_a_tab_bar() {
        let cards = tab_cards(&["mac-studio", "ubu-3xdv", "nuc-spare"]);

        let tabbed = tabbed_payload(cards.clone(), 2, 1000.0);
        assert_eq!(tabbed["hostColumns"], 1, "1000pt cannot pair 900pt cards");
        let tabs = tabbed["hostTabs"]["tabs"].as_array().expect("tabs");
        // One per card, in payload order, so this machine leads the bar exactly
        // as it leads the grid.
        assert_eq!(
            tabs.iter()
                .map(|tab| tab["label"].as_str().expect("label"))
                .collect::<Vec<_>>(),
            vec!["mac-studio", "ubu-3xdv", "nuc-spare"]
        );
        assert_eq!(tabs[1]["id"], "ubu-3xdv");
        // The container's floor is Rust's, matching HostsPanel's
        // `.frame(minHeight: 780)`: only one card is on screen at a time, so
        // nothing else is sizing it.
        assert_eq!(tabbed["hostTabs"]["minHeight"], 780.0);

        // Stack is the default and is unchanged — every card stays on the page.
        let stacked = stacked_payload(cards, 2, 1000.0);
        assert!(stacked["hostTabs"].is_null());
        assert_eq!(stacked["hostColumns"], 1);
    }

    /// Above the breakpoint the preference is inert: the cards fit side by
    /// side, so there is no overflow to resolve. A frontend deciding this for
    /// itself would be `host_tabs` re-implemented in JS.
    #[test]
    fn the_tabs_preference_does_nothing_while_the_cards_still_fit() {
        let cards = tab_cards(&["mac-studio", "ubu-3xdv"]);
        // 2 * 900 + 16 = 1816: exactly enough for two cards.
        let paired = tabbed_payload(cards.clone(), 1, 1816.0);
        assert_eq!(paired["hostColumns"], 2);
        assert!(paired["hostTabs"].is_null());

        // One point narrower and they stack — which is where tabs take over.
        let narrow = tabbed_payload(cards, 1, 1815.0);
        assert_eq!(narrow["hostColumns"], 1);
        assert!(!narrow["hostTabs"].is_null());
    }

    /// A tab bar over a single card is chrome around nothing — and a fresh
    /// install is exactly that: the local card, alone.
    #[test]
    fn a_lone_host_gets_no_tab_bar_however_narrow_the_window() {
        let vm = tabbed_payload(tab_cards(&["mac-studio"]), 0, 320.0);
        assert_eq!(vm["hostColumns"], 1);
        assert!(vm["hostTabs"].is_null());
    }

    /// A host that never connected carries its name under `error`, not at the
    /// top level. Labelling from the wrong key leaves the tab blank for
    /// precisely the host you opened the cockpit to find.
    #[test]
    fn a_failed_host_still_gets_a_labelled_tab() {
        let mut failed = pending_card("nuc-spare", &Pending::Failed("down".to_owned()));
        failed["id"] = json!("nuc-spare");
        assert!(failed.get("hostName").is_none(), "the shape this guards");

        let cards = vec![json!({ "id": "local", "hostName": "mac-studio" }), failed];
        let vm = tabbed_payload(cards, 1, 1000.0);
        let tabs = vm["hostTabs"]["tabs"].as_array().expect("tabs");
        assert_eq!(tabs[1]["label"], "nuc-spare");
        assert_eq!(tabs[1]["id"], "nuc-spare");
    }

    #[test]
    fn the_payload_carries_the_grid_constants_and_the_panel_table() {
        let vm = stacked_payload(vec![], 0, 1000.0);
        assert_eq!(vm["hostCardMinWidth"], 900.0);
        assert_eq!(vm["spacing"], 16.0);
        let panels = vm["panels"].as_array().expect("panel table");
        assert_eq!(panels[0]["id"], "hosts");
        assert_eq!(panels[0]["title"], "Hosts");
    }

    /// Rows as ids — what the assertions below are about.
    fn row_ids(vm: &Value) -> Vec<Vec<String>> {
        vm["panelRows"]
            .as_array()
            .expect("panelRows")
            .iter()
            .map(|row| {
                row.as_array()
                    .expect("row")
                    .iter()
                    .map(|p| p["id"].as_str().expect("id").to_owned())
                    .collect()
            })
            .collect()
    }

    /// One rendered row's spans, as the payload carries them — the widths a
    /// reader can name.
    fn vm_spans(vm: &Value, row: usize) -> Vec<String> {
        vm["panelRows"][row]
            .as_array()
            .expect("row")
            .iter()
            .map(|p| p["span"].as_str().expect("span").to_owned())
            .collect()
    }

    /// The payload carries the *reflowed* arrangement, not the authored one, and
    /// carries it as data. A frontend re-deriving these rows from `minWidth`
    /// would be a second implementation of `PanelKind::min_width`.
    #[test]
    fn the_payload_carries_the_reflowed_panel_rows() {
        // Wide enough for every authored row.
        let wide = row_ids(&stacked_payload(vec![], 0, 3000.0));
        assert_eq!(
            wide,
            vec![
                vec!["hosts"],
                vec!["ghWorkflows", "ghRunners"],
                vec!["containers", "openclawAgents", "claudeUsage"],
                vec!["azureCost", "services", "sentryCrons"],
            ]
        );

        // The case the whole per-panel breakpoint model exists for: at 840pt
        // every authored row breaks apart except one pair — Containers keeps
        // OpenClaw, because widening the two of them from Half + Quarter to two
        // halves puts both on 412pt tracks and above their minimums. A global
        // sm/md/lg tier cannot express that.
        let narrow = row_ids(&stacked_payload(vec![], 0, 840.0));
        assert_eq!(narrow[3], ["containers", "openclawAgents"]);
        assert_eq!(
            vm_spans(&stacked_payload(vec![], 0, 840.0), 3),
            ["half", "half"],
            "a rendered row is always whole quarters, never two thirds and a third"
        );
        assert_eq!(
            narrow.len(),
            7,
            "the halves and the quarters both split, and so does the last row"
        );

        // Every panel still travels, exactly once, at any width — a row silently
        // dropped here is a panel the frontend can never render.
        for width in [0.0, 100.0, 840.0, 976.0, 3000.0] {
            let mut flat: Vec<String> = row_ids(&stacked_payload(vec![], 0, width))
                .into_iter()
                .flatten()
                .collect();
            flat.sort();
            let mut expected: Vec<String> = viewmodel::cockpit::PanelKind::ALL
                .iter()
                .map(|k| k.id().to_owned())
                .collect();
            expected.sort();
            assert_eq!(flat, expected, "at {width}pt");
        }
    }

    /// Each entry carries what the frontend needs to place, size and title it —
    /// including the span's `weight`, which is the `fr` track the frontend paints
    /// rather than a name it would have to translate into a fraction in CSS.
    #[test]
    fn every_panel_row_entry_carries_its_id_title_min_width_and_span() {
        let vm = stacked_payload(vec![], 0, 3000.0);
        let first = &vm["panelRows"][0][0];
        assert_eq!(first["id"], "hosts");
        assert_eq!(first["title"], "Hosts");
        assert_eq!(first["minWidth"], 900.0);
        assert_eq!(first["span"], "full");
        assert_eq!(first["weight"], 4);
        // The renamed panel travels under the same id it always had.
        assert_eq!(vm["panelRows"][1][0]["id"], "ghWorkflows");
        assert_eq!(vm["panelRows"][1][0]["title"], "GitHub Repos");
        // The quarter row: a half and two quarters, in that order.
        assert_eq!(vm["panelRows"][2][0]["span"], "half");
        assert_eq!(vm["panelRows"][2][0]["weight"], 2);
        assert_eq!(vm["panelRows"][2][2]["id"], "claudeUsage");
        assert_eq!(vm["panelRows"][2][2]["span"], "quarter");
        assert_eq!(vm["panelRows"][2][2]["weight"], 1);
        assert_eq!(vm["panelRows"][3][0]["id"], "azureCost");
        assert_eq!(vm["panelRows"][3][0]["title"], "Azure Cost");
        // The width each panel actually gets, and the content columns that fit
        // in it — a lone Hosts row takes the whole 3000.
        assert_eq!(first["width"], 3000.0);
        assert_eq!(first["columns"], 2);
    }

    /// The configuration actually shipped on a 1890pt display: every half at
    /// 937pt whichever row it is in — Azure Cost included — the quarters at
    /// 460.5, and the content columns each of those affords.
    ///
    /// Repos clears its split by 41pt (896 of 937) and only because its numeric
    /// columns are sized to their labels; the same panel with the Swift
    /// originals needed 1136 and stayed single-column on this display.
    #[test]
    fn a_1890pt_cockpit_gives_every_list_panel_two_columns() {
        let vm = stacked_payload(vec![], 0, 1890.0);
        let mut seen = std::collections::BTreeMap::new();
        for row in vm["panelRows"].as_array().expect("rows") {
            for panel in row.as_array().expect("row") {
                seen.insert(
                    panel["id"].as_str().expect("id").to_owned(),
                    (panel["width"].clone(), panel["columns"].clone()),
                );
            }
        }
        assert_eq!(seen["ghRunners"], (json!(937.0), json!(2)));
        assert_eq!(
            seen["ghWorkflows"],
            (json!(937.0), json!(2)),
            "Repos pairs at 896pt; widening a column past that costs it the split"
        );
        assert_eq!(
            seen["containers"],
            (json!(937.0), json!(2)),
            "the same half as the row above it — one grid, one gridline"
        );
        assert_eq!(
            seen["azureCost"],
            (json!(937.0), json!(2)),
            "a half still buys the breakdowns their column beside the costs — by \
             121pt now that Sentry Crons shares its row, where three quarters had 597"
        );
        assert_eq!(seen["openclawAgents"], (json!(460.5), json!(1)));
        assert_eq!(seen["claudeUsage"], (json!(460.5), json!(1)));
        assert_eq!(seen["services"], (json!(460.5), json!(1)));
        assert_eq!(seen["sentryCrons"], (json!(460.5), json!(1)));
    }

    /// The Layout tab's whole point, at the payload: the rows are the *stored*
    /// arrangement, not the shipped one, and the tracks are the spans the user
    /// chose. The completing rule shows here too — three named panels, seven
    /// rendered.
    #[test]
    fn a_stored_layout_is_what_the_panel_rows_carry() {
        let stored = vec![store::LayoutProfile::new(
            0.0,
            HostOverflowMode::Stack.as_str(),
            settings::layout_slots(&[
                (PanelKind::ClaudeUsage, PanelSpan::Quarter),
                (PanelKind::AzureCost, PanelSpan::Quarter),
                (PanelKind::Hosts, PanelSpan::Half),
            ]),
        )];
        let bands = settings::breakpoints(Some(&stored), HostOverflowMode::Stack);
        let vm = layout_payload(
            vec![],
            0,
            3000.0,
            &settings::breakpoint_for(&bands, 3000.0).layout(),
        );
        assert_eq!(
            row_ids(&vm),
            vec![
                // The user's three, packed into one row (1 + 1 + 2 quarters)…
                vec!["claudeUsage", "azureCost", "hosts"],
                // …then the five the layout never named, in default order.
                vec!["ghWorkflows", "ghRunners"],
                vec!["containers", "openclawAgents", "services"],
                vec!["sentryCrons"],
            ]
        );
        let first = &vm["panelRows"][0];
        assert_eq!(first[0]["span"], "quarter");
        assert_eq!(first[0]["width"], 738.0, "(3000 - 3 * 16) / 4");
        assert_eq!(first[2]["id"], "hosts");
        assert_eq!(
            first[2]["width"], 1492.0,
            "two of those tracks plus the gutter a half spans"
        );
    }

    /// No hosts is a configuration state, not a broken app — so it arrives as
    /// a sentence made here, like every other string the frontend paints.
    #[test]
    fn an_empty_cockpit_says_so_instead_of_rendering_nothing() {
        let vm = stacked_payload(vec![], 0, 1000.0);
        assert!(vm["hosts"].as_array().expect("hosts array").is_empty());
        assert_eq!(
            vm["empty"]["message"],
            "No hosts configured. Add one in Settings."
        );

        let populated = stacked_payload(vec![view_for(&state_with(None, None, None))], 1, 1000.0);
        assert!(
            populated["empty"].is_null(),
            "a populated cockpit must not carry an empty-state message"
        );
    }

    // MARK: display order

    #[test]
    fn hosts_render_sorted_by_name_with_the_disabled_ones_dropped() {
        let mut zed = Host::new("zed", "10.0.0.3");
        let mut off = Host::new("aardvark", "10.0.0.9");
        off.enabled = false;
        zed.enabled = true;
        let hosts = vec![
            zed,
            Host::new("mac-mini", "10.0.0.1"),
            off,
            Host::new("ubu-3xdv", "10.0.0.2"),
        ];
        let names: Vec<&str> = display_order(&hosts)
            .iter()
            .map(|h| h.name.as_str())
            .collect();
        assert_eq!(names, vec!["mac-mini", "ubu-3xdv", "zed"]);
    }

    // MARK: DEVCANOPY_SEED_HOST

    #[test]
    fn a_seed_string_parses_the_way_swift_parses_it() {
        assert_eq!(
            parse_seed_host("ubu-3xdv|100.87.202.125|9000|tok"),
            Some(SeedHost {
                name: "ubu-3xdv".into(),
                address: "100.87.202.125".into(),
                port: 9000,
                token: "tok".into(),
            })
        );
        // Port and token are both optional, and an unparseable port falls
        // back to the agent default rather than rejecting the whole seed.
        assert_eq!(
            parse_seed_host("ubu-3xdv|100.87.202.125"),
            Some(SeedHost {
                name: "ubu-3xdv".into(),
                address: "100.87.202.125".into(),
                port: store::DEFAULT_AGENT_PORT,
                token: String::new(),
            })
        );
        assert_eq!(
            parse_seed_host("ubu-3xdv|100.87.202.125|not-a-port|tok")
                .expect("seed")
                .port,
            store::DEFAULT_AGENT_PORT
        );
        // An empty port field keeps the empty token field addressable -- the
        // Swift split does not omit empty subsequences, and neither does this.
        assert_eq!(
            parse_seed_host("ubu-3xdv|100.87.202.125||tok")
                .expect("seed")
                .token,
            "tok"
        );
    }

    #[test]
    fn a_seed_string_missing_a_name_or_address_is_rejected() {
        for raw in ["", "just-a-name", "|100.87.202.125", "ubu-3xdv|", "|"] {
            assert_eq!(parse_seed_host(raw), None, "raw {raw:?}");
        }
    }

    fn scratch_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let store = Store::open_in(dir.path()).expect("open");
        (dir, store)
    }

    #[test]
    fn seeding_adds_the_host_and_files_its_token_under_the_new_id() {
        let (_dir, mut store) = scratch_store();
        let credentials = MemoryCredentialStore::new();

        let id = seed_from_env(
            &mut store,
            &credentials,
            Some("ubu-3xdv|100.87.202.125|9000|agent-token"),
        )
        .expect("seed")
        .expect("a host was added");

        assert_eq!(store.hosts().len(), 1);
        let host = &store.hosts()[0];
        assert_eq!(host.name, "ubu-3xdv");
        assert_eq!(host.address, "100.87.202.125");
        assert_eq!(host.port, 9000);
        assert_eq!(host.id.to_string(), id);

        assert_eq!(
            credentials
                .secret(SecretKey::HostToken(host.id))
                .expect("read token")
                .as_deref(),
            Some("agent-token"),
            "the token must be filed under the id the store just minted"
        );
        // …and nowhere near the store file.
        let raw = std::fs::read_to_string(store.path()).expect("read store file");
        assert!(!raw.contains("agent-token"));
    }

    /// The no-op that makes `DEVCANOPY_SEED_HOST` safe to leave exported:
    /// relaunching must not accumulate duplicate hosts. Address, not name, is
    /// the identity — the name is the field the user edits.
    #[test]
    fn seeding_an_address_that_is_already_configured_is_a_no_op() {
        let (_dir, mut store) = scratch_store();
        let credentials = MemoryCredentialStore::new();
        let seed = "ubu-3xdv|100.87.202.125|7878|agent-token";

        seed_from_env(&mut store, &credentials, Some(seed)).expect("first seed");
        let first = store.hosts()[0].clone();

        // Same address, different name and token: still a no-op.
        assert_eq!(
            seed_from_env(
                &mut store,
                &credentials,
                Some("renamed|100.87.202.125|7878|other-token"),
            )
            .expect("second seed"),
            None
        );
        assert_eq!(store.hosts().len(), 1);
        assert_eq!(store.hosts()[0], first);
        assert_eq!(
            credentials
                .secret(SecretKey::HostToken(first.id))
                .expect("read token")
                .as_deref(),
            Some("agent-token"),
            "a no-op seed must not overwrite the configured token"
        );
    }

    #[test]
    fn no_seed_variable_means_no_host_and_no_credential() {
        let (_dir, mut store) = scratch_store();
        let credentials = MemoryCredentialStore::new();
        assert_eq!(
            seed_from_env(&mut store, &credentials, None).expect("seed"),
            None
        );
        // A malformed value is equally inert -- it must not half-create a host.
        assert_eq!(
            seed_from_env(&mut store, &credentials, Some("no-address")).expect("seed"),
            None
        );
        assert!(store.hosts().is_empty());
        assert!(credentials.accounts().is_empty());
    }

    #[test]
    fn a_seed_without_a_token_writes_no_credential() {
        let (_dir, mut store) = scratch_store();
        let credentials = MemoryCredentialStore::new();
        seed_from_env(&mut store, &credentials, Some("ubu-3xdv|100.87.202.125")).expect("seed");
        assert_eq!(store.hosts().len(), 1);
        assert!(
            credentials.accounts().is_empty(),
            "an absent token must not write an empty credential"
        );
    }

    #[test]
    fn a_seeded_host_survives_a_reopen() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let credentials = MemoryCredentialStore::new();
        {
            let mut store = Store::open_in(dir.path()).expect("open");
            seed_from_env(&mut store, &credentials, Some("ubu-3xdv|100.87.202.125")).expect("seed");
        }
        let reopened = Store::open_in(dir.path()).expect("reopen");
        assert_eq!(reopened.hosts().len(), 1);
        assert_eq!(reopened.hosts()[0].name, "ubu-3xdv");
    }

    // MARK: what a credential read learned, and what each answer is allowed to
    // claim
    //
    // Three answers, and the middle one is the whole point: a store that would
    // not answer used to be collapsed into "there is nothing stored", which is
    // the one thing this process cannot know when the read failed.

    /// A credential store that fails every read — a keychain that will not
    /// unlock, or the consolidated item (#223) refusing to parse.
    /// `CorruptBlob` because it needs no `keyring::Error` to construct and is
    /// value-free by design: it carries an account name and nothing else.
    struct UnreadableCredentialStore;

    impl UnreadableCredentialStore {
        fn refusal() -> SecretError {
            SecretError::CorruptBlob {
                account: "devcanopy-secrets".to_owned(),
            }
        }
    }

    impl CredentialStore for UnreadableCredentialStore {
        fn secret(&self, _key: SecretKey) -> Result<Option<String>, SecretError> {
            Err(Self::refusal())
        }

        fn set_secret(&self, _key: SecretKey, _value: &str) -> Result<(), SecretError> {
            unreachable!("no path under test writes a credential")
        }

        fn delete_secret(&self, _key: SecretKey) -> Result<(), SecretError> {
            unreachable!("no path under test deletes a credential")
        }

        fn secret_bytes(&self, _key: SecretKey) -> Result<Option<Vec<u8>>, SecretError> {
            Err(Self::refusal())
        }

        fn set_secret_bytes(&self, _key: SecretKey, _value: &[u8]) -> Result<(), SecretError> {
            unreachable!("no path under test writes a credential")
        }
    }

    fn store_holding(key: SecretKey, value: &str) -> MemoryCredentialStore {
        let credentials = MemoryCredentialStore::new();
        credentials.set_secret(key, value).expect("write");
        credentials
    }

    #[test]
    fn a_credential_read_keeps_there_is_none_apart_from_we_could_not_ask() {
        assert!(matches!(
            read_credential(&MemoryCredentialStore::new(), SecretKey::GitHubAccessToken),
            Credential::Absent
        ));
        assert!(matches!(
            read_credential(
                &store_holding(SecretKey::GitHubAccessToken, "ghp_stored"),
                SecretKey::GitHubAccessToken,
            ),
            Credential::Present(token) if token == "ghp_stored"
        ));
        // A stored blank is the zero-setup state too — a token of spaces
        // authenticates nothing, and every consumer would have to re-check.
        assert!(matches!(
            read_credential(
                &store_holding(SecretKey::GitHubAccessToken, "   "),
                SecretKey::GitHubAccessToken,
            ),
            Credential::Absent
        ));
        assert!(matches!(
            read_credential(&UnreadableCredentialStore, SecretKey::GitHubAccessToken),
            Credential::Unreadable
        ));
    }

    // MARK: the GitHub token's three branches

    /// The Repos and GitHub Runners payloads a pass would leave on screen.
    fn github_views(state: &GitHubState) -> (Value, Value) {
        (
            github::repos_view(state, github::now_utc()),
            github::runners_view(state, panel::now_unix()),
        )
    }

    fn populated_github_state() -> GitHubState {
        github::fixture_state(github::now_utc())
    }

    #[test]
    fn a_github_token_the_store_hands_over_polls_on_with_the_panels_untouched() {
        let mut state = populated_github_state();
        let (repos_before, runners_before) = github_views(&state);

        let token = github_token(
            read_credential(
                &store_holding(SecretKey::GitHubAccessToken, "ghp_stored"),
                SecretKey::GitHubAccessToken,
            ),
            &mut state,
        );

        assert_eq!(token.as_deref(), Some("ghp_stored"));
        let (repos_after, runners_after) = github_views(&state);
        assert_eq!(repos_after, repos_before, "a good read changes nothing");
        assert_eq!(runners_after, runners_before);
    }

    #[test]
    fn no_stored_github_token_clears_the_panels_and_asks_for_one() {
        let mut state = populated_github_state();
        let token = github_token(
            read_credential(&MemoryCredentialStore::new(), SecretKey::GitHubAccessToken),
            &mut state,
        );

        assert!(token.is_none(), "there is nothing to poll with");
        let (repos, runners) = github_views(&state);
        assert_eq!(repos["message"]["text"], github::UNAUTHENTICATED_MESSAGE);
        assert_eq!(runners["message"]["text"], github::UNAUTHENTICATED_MESSAGE);
        assert!(runners["rows"].as_array().expect("rows").is_empty());
    }

    /// The bug, from the shell's side: the same collapse used to run
    /// `apply_unauthenticated` here, so one locked keychain wiped both panels
    /// and told the operator to connect a token that was already connected.
    #[test]
    fn an_unreadable_github_credential_keeps_the_panels_and_names_the_store() {
        let mut state = populated_github_state();
        let (_, runners_before) = github_views(&state);

        let token = github_token(
            read_credential(&UnreadableCredentialStore, SecretKey::GitHubAccessToken),
            &mut state,
        );

        assert!(token.is_none(), "there is nothing to poll with");
        let (repos, runners) = github_views(&state);
        assert_eq!(repos["message"]["text"], CREDENTIAL_UNREADABLE_MESSAGE);
        assert_eq!(runners["message"]["text"], CREDENTIAL_UNREADABLE_MESSAGE);
        assert_ne!(repos["message"]["text"], github::UNAUTHENTICATED_MESSAGE);
        assert_eq!(
            runners["rows"], runners_before["rows"],
            "a read that never happened is not news about the runners"
        );
        assert_eq!(runners["stats"], runners_before["stats"]);
    }

    // MARK: a host's token, same three branches

    #[test]
    fn a_host_token_the_store_hands_over_is_what_the_loop_polls_with() {
        let id = Uuid::from_u128(7);
        let credentials = store_holding(SecretKey::HostToken(id), "agent-token");
        assert!(matches!(
            host_token(&credentials, id),
            HostToken::Ready(token) if token == "agent-token"
        ));
    }

    #[test]
    fn a_host_with_no_stored_token_is_told_to_configure_one() {
        let id = Uuid::from_u128(7);
        let HostToken::Blocked(message) = host_token(&MemoryCredentialStore::new(), id) else {
            panic!("an empty store must not yield a token");
        };
        assert_eq!(message, MISSING_HOST_TOKEN_MESSAGE);
    }

    /// The host half of the bug: an unreadable store used to produce a card
    /// asserting nobody had configured this host.
    #[test]
    fn a_host_token_the_store_will_not_read_blames_the_store_not_the_operator() {
        let id = Uuid::from_u128(7);
        let HostToken::Blocked(message) = host_token(&UnreadableCredentialStore, id) else {
            panic!("a failed read must not yield a token");
        };
        assert_eq!(message, HOST_TOKEN_UNREADABLE_MESSAGE);
        assert_ne!(message, MISSING_HOST_TOKEN_MESSAGE);
    }

    // MARK: applying a settings edit to the live poll set

    fn key(id: u128, base_url: &str) -> HostKey {
        HostKey {
            id: Uuid::from_u128(id),
            base_url: base_url.to_owned(),
        }
    }

    /// The point of reconciling instead of rebuilding: an edit to one host
    /// must leave every other host's task — and therefore its sparkline
    /// history, its failure streak, its last-success time — untouched.
    #[test]
    fn adding_a_host_keeps_every_existing_task() {
        let a = key(1, "http://10.0.0.1:7878");
        let b = key(2, "http://10.0.0.2:7878");
        let existing = vec![a.clone()];
        let desired = vec![a, b];
        assert_eq!(reconcile(&existing, &desired), vec![Some(0), None]);
    }

    #[test]
    fn removing_or_disabling_a_host_drops_only_that_task() {
        let a = key(1, "http://10.0.0.1:7878");
        let b = key(2, "http://10.0.0.2:7878");
        let c = key(3, "http://10.0.0.3:7878");
        let existing = vec![a.clone(), b, c.clone()];
        // `b` left the enabled set; the survivors keep their own tasks even
        // though their positions shifted.
        let plan = reconcile(&existing, &[a, c]);
        assert_eq!(plan, vec![Some(0), Some(2)]);
        // Index 1 is claimed by nobody, which is exactly what the caller
        // aborts.
        assert!(!plan.contains(&Some(1)));
    }

    /// A task polling the old address would keep reporting numbers under the
    /// right host's name — the worst shape of this bug, since the card looks
    /// fine. The endpoint is part of the key precisely so that cannot happen.
    #[test]
    fn moving_a_host_to_a_new_address_replaces_its_task() {
        let before = key(1, "http://10.0.0.1:7878");
        let after = key(1, "http://10.0.0.1:9000");
        assert_eq!(reconcile(&[before], &[after]), vec![None]);
    }

    #[test]
    fn an_unchanged_set_starts_nothing_and_stops_nothing() {
        let hosts = vec![
            key(1, "http://10.0.0.1:7878"),
            key(2, "http://10.0.0.2:7878"),
        ];
        assert_eq!(reconcile(&hosts, &hosts), vec![Some(0), Some(1)]);
    }

    #[test]
    fn a_first_reload_spawns_everything_and_an_emptied_set_keeps_nothing() {
        let a = key(1, "http://10.0.0.1:7878");
        assert_eq!(reconcile(&[], std::slice::from_ref(&a)), vec![None]);
        assert!(reconcile(std::slice::from_ref(&a), &[]).is_empty());
    }

    // MARK: the dumped fixtures the Playwright suite runs against

    #[test]
    fn the_single_host_dump_is_a_cockpit_payload_with_one_live_card() {
        let vm = dump_single(&Connection::Live);
        let cards = vm["hosts"].as_array().expect("hosts array");
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0]["connection"]["state"], "live");
        assert_eq!(vm["hostColumns"], 1);
        assert!(vm["empty"].is_null());
    }

    /// The multi-host fixture must actually be mixed — three live cards would
    /// let the frontend's per-card state handling regress unnoticed — and the
    /// local card must lead it, because that is where the live payload puts it.
    #[test]
    fn the_cockpit_dump_leads_with_the_local_card_then_three_remote_states() {
        let vm = stacked_dump(3.0 * HOST_CARD_MIN_WIDTH + 2.0 * SPACING, 3);
        let cards = vm["hosts"].as_array().expect("hosts array");
        assert_eq!(cards.len(), 4, "one local card plus three remotes");
        // Four cards, three columns' worth of width: the count is the grid's,
        // not the payload's.
        assert_eq!(vm["hostColumns"], 3);

        let states: Vec<&str> = cards
            .iter()
            .map(|c| c["connection"]["state"].as_str().expect("state"))
            .collect();
        assert_eq!(states, vec!["live", "live", "unreachable", "failed"]);

        let ids: Vec<&str> = cards
            .iter()
            .map(|c| c["id"].as_str().expect("id"))
            .collect();
        assert_eq!(
            ids,
            vec![local::CARD_ID, "ubu-3xdv", "mac-mini", "nuc-spare"]
        );

        // The local card carries the em dashes the shipped one really does —
        // no portable memory-pressure source and no dependency-free GPU read —
        // so the Playwright suite exercises that rule against Rust's own output.
        assert_eq!(cards[0]["pressureText"], "Pressure: —");
        assert_eq!(cards[0]["gpuValue"], "—");
        assert_eq!(cards[0]["vramText"], "VRAM: —");
        // …and everything the sampler *did* measure is still a number.
        assert_eq!(cards[0]["cpuValue"], "22%");
        assert_eq!(cards[0]["diskRead"], "3.2 MB/s");

        // The same payload at a narrow width is the stacked fixture -- same
        // cards, one column, so the frontend's grid can be tested both ways
        // against numbers Rust produced.
        assert_eq!(stacked_dump(1000.0, 3)["hostColumns"], 1);

        // The same cards, the same width, and the tabs preference: the fixture
        // the Playwright suite drives the tab bar with. If this ever stopped
        // producing one, that suite would be asserting against a stacked grid
        // and passing.
        let tabs = dump_cockpit(1000.0, 3, HostOverflowMode::Tabs);
        assert_eq!(tabs["hostColumns"], 1);
        assert_eq!(
            tabs["hostTabs"]["tabs"]
                .as_array()
                .expect("the tabs fixture must carry a tab bar")
                .len(),
            4,
            "one tab per card, the local one included"
        );

        // …and no *remote* hosts at all is the unconfigured cockpit: the local
        // card still leads it, because this machine is always there.
        let none = stacked_dump(1000.0, 0);
        let cards = none["hosts"].as_array().expect("hosts array");
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0]["id"], local::CARD_ID);
        assert_eq!(
            none["empty"]["message"],
            "No hosts configured. Add one in Settings."
        );
    }

    /// The fixture must reproduce byte-for-byte on any machine, or the
    /// Playwright suite is asserting against whatever the dumping box happened
    /// to be doing.
    #[test]
    fn the_local_fixture_card_does_not_vary_per_run() {
        assert_eq!(dump_local_card(), dump_local_card());
        assert_eq!(dump_local_card()["hostName"], "mac-studio");
    }

    /// The settings fixture must be stable across dumps (ids the Playwright
    /// suite can address) and mixed (both sides of every badge), or the specs
    /// it drives are only ever exercising one branch.
    #[test]
    fn the_settings_dump_is_stable_and_covers_both_sides_of_every_badge() {
        let vm = dump_settings();
        assert_eq!(vm, dump_settings(), "the fixture must not vary per run");

        let rows = vm["hosts"]["rows"].as_array().expect("host rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["tokenStored"], true);
        assert_eq!(rows[0]["enabled"], true);
        assert!(!rows[0]["hiddenVolumes"]
            .as_array()
            .expect("hidden volumes")
            .is_empty());
        assert_eq!(rows[1]["tokenStored"], false);
        assert_eq!(rows[1]["enabled"], false);

        assert_eq!(vm["github"]["secret"]["stored"], true);
        assert_eq!(vm["azure"]["secret"]["stored"], false);
        assert!(!vm["portfolio"]["rows"]
            .as_array()
            .expect("repo rows")
            .is_empty());
    }

    /// The rules half of the same fixture, and for the same reason: the
    /// Playwright suite must not be able to pass against a payload that
    /// quietly lost the rendering it claims to exercise. Between them these
    /// rows carry every action, both sides of the Collapse-only fields, an
    /// expectation set and unset, an unscoped rule, and a scope naming a host
    /// that is not configured.
    #[test]
    fn the_settings_fixture_covers_every_rule_rendering_the_editor_has() {
        let rows = dump_settings()["hosts"]["rules"]["rows"]
            .as_array()
            .expect("rule rows")
            .clone();

        let actions: Vec<&str> = rows
            .iter()
            .map(|row| row["action"].as_str().expect("action"))
            .collect();
        for action in store::ContainerRuleAction::ALL {
            assert!(
                actions.contains(&action.as_str()),
                "no {} rule in the fixture",
                action.as_str()
            );
        }

        // Collapse-only fields show for Collapse and for nothing else.
        for row in &rows {
            assert_eq!(
                row["collapseOnly"],
                row["action"] == "collapse",
                "collapseOnly disagrees with the action on {row}"
            );
        }

        // The expected-count field, both ways: a set expectation is a string
        // (never a number), and an unset one is empty (never "0").
        assert!(
            rows.iter().any(|row| row["expected"] == "4"),
            "no rule carries an expected count"
        );
        assert!(
            rows.iter().any(|row| row["expected"] == ""),
            "every rule carries an expected count"
        );

        // An unscoped rule, a scoped one, and one whose host no longer exists —
        // the case `rule_host_options` grows an extra option for, and the one
        // where a picker renders blank if it doesn't.
        assert!(rows.iter().any(|row| row["host"] == ""), "none unscoped");
        let orphan = rows
            .iter()
            .find(|row| row["host"] == "retired-box")
            .expect("no orphaned host scope in the fixture");
        let options: Vec<&str> = orphan["hostOptions"]
            .as_array()
            .expect("host options")
            .iter()
            .map(|option| option["value"].as_str().expect("value"))
            .collect();
        assert!(
            options.contains(&"retired-box"),
            "the orphaned scope is missing from its own picker: {options:?}"
        );
    }

    #[test]
    fn the_dump_flags_override_their_defaults() {
        let args: Vec<String> = [
            "app",
            "--dump-cockpit",
            "out.json",
            "--width",
            "1000",
            "--hosts",
            "0",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(value_flag(&args, "--width", 2732.0), 1000.0);
        assert_eq!(value_flag(&args, "--hosts", 3), 0);

        let bare: Vec<String> = ["app", "--dump-cockpit", "out.json"]
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(value_flag(&bare, "--width", 2732.0), 2732.0);
        assert_eq!(value_flag(&bare, "--hosts", 3), 3);
    }
}
