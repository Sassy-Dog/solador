//! Solador shell. The frontend receives a finished view-model and paints;
//! all logic lives in `viewmodel`.

use agentclient::{AgentClient, AgentError};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use store::settings::PanelInterval;
use store::{
    ContainerGroupRule, CredentialStore, Host, HostOverflowMode, KeyringStore, SecretKey, Store,
    StoreError, TrackedRepo, VendorAccount, VendorKind,
};
use tauri_plugin_notification::NotificationExt;
use uuid::Uuid;
use viewmodel::card::{host_card, pending_card, Connection, HostHistories, Pending};
use viewmodel::cockpit::{host_columns, HOST_CARD_MIN_WIDTH, SPACING};
use viewmodel::color;

mod azure;
mod cadence;
mod containers;
mod crons;
mod dashboard;
mod github;
mod handoff;
mod local;
mod machine_alerts;
mod openclaw;
mod panel;
mod plan_usage;
mod resume;
mod runner_groups;
mod services;
mod settings;
mod tile_scope;
mod update;
mod usage;
mod volumes;
mod window_fit;
mod window_state;

use azure::AzureState;
use containers::ContainersState;
use crons::CronsState;
use github::GitHubState;
use local::LocalHostState;
use openclaw::OpenClawState;
use settings::{SecretField, StoredSecrets};
use update::UpdateState;
use usage::UsageState;

/// How often each host is polled. Matches the original side's
/// `RemoteHostMetricsService.start(interval:)` default of 1s, which is also
/// what the charts assume: one history sample is one fixed time slice
/// (`PX_PER_SAMPLE`), so the cadence is part of the time axis, not a tuning
/// knob to be picked per shell.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Consecutive failed polls before a host's card stops claiming to be current.
///
/// Parity with `RemoteHostMetricsService.failureThreshold` (ported): a single
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

/// How long one panel's loop waits before its next pass, read from the store
/// **now** rather than captured when the task was spawned.
///
/// Every call site is in a loop body, after the pass, and that placement is the
/// whole point. An interval baked into a `tokio::time::interval` when the task
/// starts keeps handing out the number that was configured *then*, so choosing
/// a new cadence in Settings takes up to one full interval to have any visible
/// effect — four hours, on the Azure row — which is indistinguishable from the
/// setting doing nothing at all. `github_loop` has re-read its cadence every
/// pass for exactly this reason since it was written; this is that same
/// discipline for the panels whose cadence became a setting in #300, and since
/// #615 `github_loop`, `services_loop` and `usage_loop` read theirs through here
/// too.
///
/// [`store::settings::Settings::panel_interval_secs`] applies the panel's floor
/// on read, so what comes back here is already the enforced value — callers
/// must not clamp it again, and a hand-edited `store.json` gets no say in it.
///
/// [`POLL_INTERVAL`] and [`HEALTH_POLL_INTERVAL`] are deliberately out of
/// reach: there is no [`PanelInterval`] variant that names either, because the
/// 1 Hz host poll is the time axis of the sparklines and agent health is
/// liveness rather than a panel.
fn panel_interval(store: &Mutex<Store>, panel: PanelInterval) -> std::time::Duration {
    assert_store_not_held(store);
    let secs = {
        let store = store.lock().expect("store poisoned");
        u64::from(store.settings().panel_interval_secs(panel))
    };
    std::time::Duration::from_secs(secs)
}

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
    /// Which pairing failure `error` is, when it is one (#448, #449): a
    /// certificate that is not the pinned one, a pinned host that answers plain
    /// HTTP, or an unpaired host the cockpit refused to send the token to over
    /// plain HTTP because its address is neither loopback nor Tailscale.
    /// `None` for everything else, including every network failure.
    ///
    /// Carried beside `error` rather than recovered from its sentence, because
    /// two consumers need the *kind* and neither may parse words: the dashboard
    /// paints these hosts as their own state instead of `Unreachable`, and its
    /// "is this machine's network or VPN up?" hint must not count them as the
    /// network being down. Always set and cleared together with `error`.
    error_kind: Option<&'static str>,
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
    /// The agent's own version, from the same `/v1/health` payload (#489): what
    /// Settings compares against the newest verified release without anyone
    /// pressing Test.
    ///
    /// The same lifecycle as [`HostState::sampler_stale`], deliberately. `None`
    /// is "nobody has told us" — no health read has landed, or an agent that
    /// cannot name itself omitted the key — never an empty string and never
    /// the cockpit's own version. A failed health poll leaves it alone (a
    /// request we could not make is not evidence the host changed); a
    /// successful one overwrites it, `None` included, because an agent rolled
    /// back to a build that cannot name itself no longer has the version we
    /// last heard.
    version: Option<String>,
    /// Whether the last poll was the plain-HTTP guard's refusal (#461). What
    /// makes the log line fire on the *transition* into that state rather
    /// than on every 1 Hz poll; cleared by anything else, so a host that
    /// recovers and is refused again is logged again.
    plain_refused: bool,
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
    /// The pinned certificate, in the key because a **re-pair** changes it
    /// while leaving `base_url` (already `https://`) exactly as it was — a task
    /// keyed on the URL alone would keep polling with the old client, and the
    /// old pin, after the operator replaced it.
    tls_fingerprint: Option<String>,
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
    /// Cuts the containers loop's sleep short.
    ///
    /// It had no need of one while it ticked every ten seconds: waiting out a
    /// cadence that short is not something anyone can see. Its cadence is the
    /// operator's now, so "it comes round again immediately anyway" stopped
    /// being true, and the resume path is the caller that noticed first.
    containers_wake: tokio::sync::Notify,
    /// The Repos + GitHub Runners panels' state, on [`PanelInterval::GitHub`].
    ///
    /// One lock for both panels because they share a credential and a poll
    /// pass; like the containers lock, it is never held while the store's is.
    github: Mutex<GitHubState>,
    /// Cuts the GitHub loop's sleep short.
    ///
    /// This is what makes "applies without a restart" true for a *periodic*
    /// service, where reconciling tasks (as [`reload_hosts`] does for hosts) is
    /// not the mechanism: saving a token, clearing one, editing the portfolio
    /// or picking a new GitHub cadence all wake the loop instead of waiting
    /// out a cadence that can be five minutes long.
    github_wake: tokio::sync::Notify,
    /// Cuts the Services loop's sleep short. A loop of its own since #615, on
    /// [`PanelInterval::Services`]: it used to ride at the top of
    /// [`poll_github`], and an edit to the watched set woke the GitHub loop to
    /// reach it. The watched set is derived from the status-page list and the
    /// credentials and accounts already in the cockpit, so those edits wake
    /// this one (and the GitHub one too, where they also change what it fetches).
    services_wake: tokio::sync::Notify,
    /// The Usage panel's state: Claude token rollups on
    /// [`PanelInterval::ClaudeRollups`], Neon, Sentry and Vercel on
    /// [`PanelInterval::UsageProviders`] (hourly by default).
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
    /// Set alongside a wake that must refresh the Claude rollups now: resume
    /// from sleep. The walk otherwise runs only when its own cadence has
    /// elapsed (#635), and the `Instant` that cadence is measured on may not
    /// count the sleep (app/README.md, "Waking up with the machine"), so a
    /// resume would not reliably find it due.
    usage_rollups_due: std::sync::atomic::AtomicBool,
    /// Cuts the plan-usage loop's sleep short: a Settings toggle or a cadence
    /// edit. The plan rows live in [`App::usage`]; their loop is separate
    /// because it runs on its own cadence and reads another program's sign-in,
    /// which nothing else on the usage loop does.
    plan_usage_wake: tokio::sync::Notify,
    /// The Sentry Crons panel's state, on the same default hourly cadence as the
    /// other Sentry read — not the Claude rollups' cadence.
    ///
    /// Its own lock rather than a field on [`UsageState`]: the two share a
    /// credential and nothing else. They are different panels, on different rows,
    /// answering different questions ("how much did we consume" vs "what is
    /// broken"), and one lock would make a slow monitor read hold up a repaint of
    /// the Usage card.
    crons: Mutex<CronsState>,
    /// Cuts the crons loop's sleep short after the Sentry token or org slug
    /// changes, after a Crons cadence edit from its Configure, and on a resume
    /// from sleep. A long cadence is long enough that waiting one out is
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
    /// What the app knows about a newer version of itself (#308).
    ///
    /// **On no cadence at all**, unlike every other field above: it is written
    /// by the launch check in `setup` and by the operator's own re-check, and
    /// read by the `update_*` commands. A background poll would be a cockpit
    /// that quietly reaches a release server all day; once at launch is what
    /// the Settings surface promises, so it is what happens.
    update: Mutex<UpdateState>,
    /// The newest *verified* agent release, as the last check left it (#489):
    /// what each host's Settings row compares its own version against.
    ///
    /// Written only by [`agent_release_loop`] — once at startup, then on
    /// [`AGENT_RELEASE_CHECK_INTERVAL`] — and read by `settings_payload`. It
    /// starts as `NotChecked`, which is a state of its own and not a verdict:
    /// a feed nothing has read must never look like "up to date". Its own
    /// lock, never held with another: the loop awaits the network first and
    /// takes this one only to store the answer.
    agent_release: Mutex<viewmodel::agent_release::Latest>,
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
    /// The crash reporter, live or deliberately dead.
    ///
    /// Held here because holding it is what keeps the Sentry client alive for
    /// the process — and because the Settings command has to reach it to
    /// withdraw consent, which takes effect immediately rather than on the next
    /// launch. On a store that has never opted in this holds no client at all,
    /// which is the state a fresh install is in and the state it stays in until
    /// somebody ticks the box.
    crash_reporting: crashreport::Reporting,
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
    // The kind rides beside the sentence (#448), only while `error` is what
    // published it: a card that recovered has neither.
    if let (Some(kind), Some(_)) = (s.error_kind, &s.error) {
        card["error"]["kind"] = json!(kind);
    }
    card
}

/// One card per host, this machine first: the build every cockpit payload
/// starts from, and the part of it that reads host state under lock.
///
/// Local first, matching `HostsPanel.hosts` in the original (`[local] + remoteHosts`).
/// This machine is the one you are looking at; it leads.
fn cockpit_cards(local: Option<Value>, hosts: &[Arc<Mutex<HostState>>]) -> Vec<Value> {
    #[cfg(test)]
    CARD_BUILDS.with(|n| n.set(n.get() + 1));
    let remote: Vec<Value> = hosts
        .iter()
        .map(|host| view_for(&host.lock().expect("host state poisoned")))
        .collect();
    local.into_iter().chain(remote).collect()
}

// How many times `cockpit_cards` has built the cards on this thread, so a
// test can prove a scoped Machines tile builds them once per poll (#641).
#[cfg(test)]
thread_local! {
    static CARD_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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
                // — which is exactly how ubu-01 stayed unnoticed through the
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

/// Drops the operator's hidden volumes from every card, then re-derives the
/// one payload figure computed from those lists: [`volume_slots`] pads cards to
/// the longest *visible* list. Before anything else reads the payload, so the
/// Hosts panel and the dashboard cannot disagree about which volumes exist.
fn hide_volumes(payload: &mut Value, hidden: &volumes::Hidden) {
    let columns = payload["hostColumns"].as_u64().unwrap_or(0) as usize;
    if let Some(cards) = payload["hosts"].as_array_mut() {
        hidden.apply(cards);
        let slots = volume_slots(cards, columns);
        payload["volumeSlots"] = json!(slots);
    }
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

/// How a Machines tile's Full view lays its host cards out: the two per-tile
/// settings that shape them (#647). `Default` is what a read that names no tile
/// uses: the overview's rows and the boot read never paint the cards' core
/// ladders, so they take the defaults rather than any one tile's choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HostLook {
    /// What the cards do when they no longer fit side by side.
    overflow: HostOverflowMode,
    /// How many grid rows tall each card's per-core CPU chart is.
    core_row_span: usize,
}

impl Default for HostLook {
    fn default() -> Self {
        HostLook {
            overflow: HostOverflowMode::default(),
            core_row_span: usize::from(store::settings::DEFAULT_CORE_ROW_SPAN),
        }
    }
}

impl From<&store::DashboardTile> for HostLook {
    fn from(tile: &store::DashboardTile) -> Self {
        HostLook {
            overflow: tile.host_overflow,
            core_row_span: usize::from(tile.core_row_span),
        }
    }
}

/// Everything [`cockpit_payload`] reads, as one value: the cards already
/// rendered, how many hosts are monitored, the width they are sized from and
/// how the tile wants them laid out.
///
/// `remote_count` is deliberately separate from `cards.len()`: the local card is
/// always there, so "is anything configured" is a question about *monitored*
/// hosts and counting cards would answer it wrong forever.
struct CockpitInput {
    cards: Vec<Value>,
    remote_count: usize,
    /// The window's width, which a tile's Full view is sized from.
    available: f64,
    look: HostLook,
}

/// The payload shape, over already-rendered cards. Split from
/// [`cockpit_cards`] so the tests can drive it without locks.
fn cockpit_payload(input: CockpitInput) -> Value {
    let CockpitInput {
        mut cards,
        remote_count,
        available,
        look,
    } = input;
    let columns = host_columns(available, cards.len(), HOST_CARD_MIN_WIDTH, SPACING);
    let tabs = host_tab_bar(&cards, columns, look.overflow);
    align_core_ladders(&mut cards, columns, look.core_row_span);
    // A cockpit with no monitored host says so in words made here, like every
    // other string the frontend paints -- the local card alone would read as a
    // finished setup rather than an untouched one. The overview's Machines tile
    // reads it (`dashboard::source_view`); a tile's Full view paints no copy.
    let empty = if remote_count == 0 {
        json!({ "message": "No hosts configured. Add one in Settings." })
    } else {
        Value::Null
    };
    json!({
        "hosts": cards,
        "hostColumns": columns,
        "hostCardMinWidth": HOST_CARD_MIN_WIDTH,
        // Null unless the cards actually collapse. A tab bar the frontend has
        // to decide the visibility of would be `host_tabs` re-implemented in
        // JS, free to disagree with the tested one -- the same argument
        // `hostColumns` is here for.
        "hostTabs": tabs,
        // How many volume tiles each card reserves. Cross-card, so it belongs
        // here beside `hostColumns` rather than inside a card that can only see
        // itself.
        "volumeSlots": volume_slots(&cards, columns),
        // The content columns each panel affords at this width. A panel's Full
        // view fills the window, so this is `panel_columns` over the window's
        // width; it travels as data for the reason `hostColumns` does.
        "panelColumns": viewmodel::cockpit::PanelKind::ALL
            .iter()
            .map(|kind| (kind.id().to_owned(), json!(viewmodel::cockpit::panel_columns(*kind, available, SPACING))))
            .collect::<serde_json::Map<String, Value>>(),
        "empty": empty,
        // What the overview says when `dashboard_view` fails. A failed call has no
        // words of its own, and this is the other command the page is already
        // polling, so the sentence is made here, in one place.
        "overviewLoadFailed": dashboard::OVERVIEW_LOAD_FAILED,
    })
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
/// polls have failed, mirroring the original service: the streak is counted from
/// the first failure, but the card keeps its previous badge until the streak
/// is long enough to mean something.
fn record_poll(s: &mut HostState, result: Result<wire::Snapshot, AgentError>, at: Instant) {
    // The shell's logging convention is `eprintln!`; the decision and the words
    // are `refusal_transition`'s, so they can be tested without a stderr.
    if let Some(line) = refusal_transition(s, result.as_ref().err()) {
        eprintln!("{line}");
    }
    match result {
        Ok(snap) => {
            s.histories.record(&snap);
            s.latest = Some(snap);
            s.error = None;
            s.error_kind = None;
            s.last_success = Some(at);
            s.consecutive_failures = 0;
        }
        Err(e) => {
            s.consecutive_failures = s.consecutive_failures.saturating_add(1);
            if s.consecutive_failures >= FAILURE_THRESHOLD {
                s.error = Some(e.user_message());
                s.error_kind = error_kind(&e);
            }
        }
    }
}

/// Tracks the plain-HTTP guard's refusal across polls and returns the one log
/// line for the poll that *enters* the refused state (#461): previously not
/// refused, now refused. Later refused polls return `None`; any other outcome
/// (a success, or a different failure) leaves the state, so a fresh refusal
/// after it is a new entry and logs again. The same log-on-transition rule
/// `crates/localhost` follows.
fn refusal_transition(s: &mut HostState, error: Option<&AgentError>) -> Option<String> {
    match error {
        Some(AgentError::PlainHttpRefused(refused)) => {
            let entering = !s.plain_refused;
            s.plain_refused = true;
            entering.then(|| refusal_log_line(&s.name, refused))
        }
        _ => {
            s.plain_refused = false;
            None
        }
    }
}

/// The log line for a host entering the refused state: the host and the
/// addresses the guard refused. Built from the evidence alone — no client, no
/// credential is in reach — so the bearer token cannot appear in it.
fn refusal_log_line(host: &str, refused: &agentclient::Refused) -> String {
    let addrs = if refused.addrs().is_empty() {
        "no addresses (the name resolved to nothing)".to_owned()
    } else {
        refused
            .addrs()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let target = refused
        .name()
        .map_or_else(String::new, |name| format!(" ({name})"));
    format!(
        "host {host:?}{target}: plain HTTP refused, nothing sent — \
         not loopback or Tailscale: {addrs}"
    )
}

/// Which of the pairing failures (#448, #449) an error is, or `None` — see
/// [`HostState::error_kind`]. A wildcard is the right shape here and not a
/// hidden default: `None` is "this is an ordinary failure", which is every
/// other variant's answer by definition.
fn error_kind(e: &AgentError) -> Option<&'static str> {
    match e {
        AgentError::CertificateChanged => Some(viewmodel::card::ERROR_KIND_CERTIFICATE_CHANGED),
        AgentError::NoTls => Some(viewmodel::card::ERROR_KIND_NO_TLS),
        AgentError::PlainHttpRefused(_) => Some(viewmodel::card::ERROR_KIND_PLAIN_HTTP_REFUSED),
        AgentError::Unreachable(_)
        | AgentError::AuthFailed
        | AgentError::HttpStatus(_)
        | AgentError::DecodeFailed(_) => None,
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
        // Same payload, same rule, `None` included (#489).
        s.version = info.version;
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

/// How often the newest verified agent release is looked up (#489), after the
/// check made at startup.
///
/// A fixed constant and not an operator setting, by decision: the feed changes
/// when someone cuts an agent release, which is hours apart at the fastest, and
/// one small request an hour to a public release page is the cost of the Settings
/// row never being more than an hour stale. It is **not** one of
/// `store.json`'s `panel_intervals`, and adding it there would be a control for
/// a number nobody has a reason to turn.
const AGENT_RELEASE_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// What a finished agent-release lookup becomes in the view layer.
///
/// A pure map so each arm is a test: the one verdict is a verified feed, a 404
/// on `agent-latest` is the neutral "nothing published yet" (the truth until the
/// first `agent-v*` release exists, never a failure), and **every other error
/// is `Failed` carrying `Error::user_message()`** — the classified sentence and
/// nothing a transport or a decoder said.
fn agent_release_outcome(
    result: Result<agentrelease::Latest, agentrelease::Error>,
) -> viewmodel::agent_release::Latest {
    use viewmodel::agent_release::Latest;
    match result {
        Ok(found) => Latest::Published(found.version),
        Err(agentrelease::Error::NotPublished) => Latest::NonePublished,
        Err(other) => Latest::Failed(other.user_message()),
    }
}

/// The agent-release check's loop: once immediately, then every
/// [`AGENT_RELEASE_CHECK_INTERVAL`].
///
/// **A failed check replaces the previous answer rather than keeping it.** An
/// hour-old "2026.11.2 available" that outlived a feed that stopped verifying
/// would be a claim the cockpit can no longer back, and the Settings line is a
/// verified claim or it is a stated failure. The network is awaited before the
/// lock is taken.
async fn agent_release_loop(app: Arc<App>) {
    let mut tick = tokio::time::interval(AGENT_RELEASE_CHECK_INTERVAL);
    // Same reason as every other loop here: `Burst` would fire every missed
    // tick back-to-back after a suspend.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        // The first tick completes at once, which is the startup check.
        tick.tick().await;
        let result = agentrelease::latest().await;
        // One line per check, hourly, so a run with no screen can still tell a
        // loop that ran from one that never did (the smoke checklist reads it).
        // Never the transport's own text (a URL, a socket address): the
        // classified sentence is what an operator reads, here as in the row.
        match &result {
            Ok(found) => eprintln!("agent release: newest verified is {}", found.version),
            Err(agentrelease::Error::NotPublished) => {
                eprintln!("agent release: none published yet");
            }
            Err(e) => eprintln!("agent release: the check failed: {e}"),
        }
        *app.agent_release.lock().expect("agent release poisoned") = agent_release_outcome(result);
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
    s.error_kind = None;
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
/// The remaining `tokio::time::interval` loops need no help: hosts and the
/// local card tick at 1s and health at 10s, and none of them is reachable from
/// here anyway — `poll_loop` is spawned without an `Arc<App>`. What this exists
/// for is the *slow* loops, which would otherwise show the previous night's
/// data for up to a full interval after the lid opens.
///
/// **Containers joined them in #301.** It used to be in the paragraph above, on
/// the strength of a 10s tick nobody could see themselves waiting out — but its
/// cadence is a setting now, and an operator who stretched it to ten minutes
/// has exactly the problem this loop exists to solve.
///
/// **Sentry Crons joined them in #635.** Its staleness window became 1.5
/// cycles of its own cadence, so at a short cadence a nap of more than half a
/// cycle would read stale on a healthy loop until the late poll landed. It
/// costs one Sentry request per resume, and none while no token is saved.
///
/// **This deliberately breaks the rule the wake channels were built for.**
/// `app/README.md` records that every mutation wakes exactly the loop its data
/// feeds and no other, because a wake spends a whole poll pass. A resume is not
/// an edit to one source: it is every source at once becoming untrustworthy, so
/// it is the one caller entitled to fire all of them.
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

        wake_after_resume(&app);
    }
}

/// Every slow loop [`resume_loop`] wakes, apart from the loop itself so the
/// set can be tested without a clock that jumps.
fn wake_after_resume(app: &App) {
    wake_github(app);
    wake_services(app);
    wake_usage(app, UsageWake::Everything);
    app.azure_wake.notify_one();
    app.containers_wake.notify_one();
    app.crons_wake.notify_one();
    // Costs nothing unless a read is due: the plan loop's gate compares the
    // wall clock, which kept running through the sleep, against each
    // provider's last attempt, and a provider that is off is never read.
    app.plan_usage_wake.notify_one();
    wake_openclaw(app);
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
        error_kind: None,
        // Unknown until the first health poll lands — never `Some(false)`,
        // which would be this process asserting a fact about an agent it has
        // not spoken to yet.
        sampler_stale: None,
        sample_age_seconds: None,
        // Unknown until the first health poll lands, for the same reason.
        version: None,
        plain_refused: false,
    }));

    let (token, blocked) = match host_token(&*app.credentials, key.id) {
        HostToken::Ready(token) => (token, None),
        HostToken::Blocked(message) => (String::new(), Some(message)),
    };
    let token_available = blocked.is_none();
    // Immutable, so it is shared rather than guarded: the snapshot loop and
    // the containers loop both poll this host through this one client.
    let client = Arc::new(agent_client(&key, token));

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

/// The client for one host: pinned to its certificate when it has one (#448),
/// plain HTTP otherwise. The one place that decides, so the poll loops, the
/// Test button and any future caller cannot disagree about which hosts get
/// which — the pinned constructor is what guarantees a host with a fingerprint
/// is never dialled over `http://`.
fn agent_client(key: &HostKey, token: String) -> AgentClient {
    match &key.tls_fingerprint {
        Some(fingerprint) => AgentClient::pinned(key.base_url.clone(), token, fingerprint),
        None => AgentClient::new(key.base_url.clone(), token),
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
/// Mirrors `RemoteHostsCoordinator.reload()` in the original.
fn reload_hosts(app: &App) {
    assert_store_not_held(&app.store);
    let desired: Vec<(HostKey, String)> = {
        let store = app.store.lock().expect("store poisoned");
        display_order(store.hosts())
            .into_iter()
            .map(|host| {
                (
                    HostKey {
                        id: host.id,
                        base_url: host.base_url(),
                        tls_fingerprint: host.tls_fingerprint.clone(),
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
        // previous rows in place (`RemoteHostsCoordinator`, the original): the panel
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

/// The containers panel's poll loop: poll every source, record, then wait out
/// the operator's cadence for this panel.
///
/// Sleeping *after* the pass rather than ticking a `tokio::time::interval` is
/// also what retired the `MissedTickBehavior::Delay` this loop used to need:
/// there are no missed ticks to queue up when the next wait does not begin
/// until this pass has returned, so one slow pass can no longer be followed by
/// a back-to-back spawn storm of `docker ps` invocations.
async fn containers_loop(app: Arc<App>) {
    loop {
        poll_containers(&app).await;
        let wait = panel_interval(&app.store, PanelInterval::Containers);
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            // `notify_one`, not `notify_waiters`: a wake that lands while a
            // pass is still running stores a permit and fires the moment this
            // sleep begins, instead of being dropped for having no listener.
            () = app.containers_wake.notified() => {}
        }
    }
}

/// Guards the containers surface's counterpart to [`FIRST_REQUEST`].
static FIRST_CONTAINERS_REQUEST: std::sync::Once = std::sync::Once::new();

#[tauri::command]
fn containers(tile: Option<String>, state: tauri::State<'_, Arc<App>>) -> Result<Value, String> {
    scope_to_tile(&state, "containers", tile.as_deref(), || {
        containers_unscoped(&state)
    })
}

/// [`containers`] without a tile: the whole source, as the dashboard reads it.
fn containers_unscoped(state: &App) -> Value {
    let payload = containers_view_in(state, containers::now_unix());
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

/// The panel's payload as of `now`, over a plain `&App` so a test can drive it.
fn containers_view_in(state: &App, now: u64) -> Value {
    // Rules and presence first, then the panel state — one lock at a time, in
    // the same sequence `poll_containers` uses.
    let (rules, presence, hosts) = {
        let store = state.store.lock().expect("store poisoned");
        (
            store.container_rules().to_vec(),
            store.container_presence().clone(),
            store.hosts().to_vec(),
        )
    };
    // The operator's cadence, read at render time like Crons' and Azure's: it
    // sets the footer's staleness window, so a save or clear moves it at once
    // (#633), with nothing for `sync_cadences` to cache.
    let cadence_secs = panel_interval(&state.store, PanelInterval::Containers).as_secs();
    let mut payload = {
        let panel = state.containers.lock().expect("containers state poisoned");
        containers::view(&panel, &rules, &presence, now, cadence_secs)
    };
    payload["ruleEditor"] = settings::rules_section(&rules, &hosts);
    payload
}

// MARK: the Repos + GitHub Runners panels
//
// One loop for both, because they share a credential and a cadence: the token
// that authenticates one authenticates the other, and a single pass fills both.
// Its cadence is `PanelInterval::GitHub`, edited from either tile's Configure
// (#615): the retired shared Refresh Interval used to drive this, the Services
// pass and the Claude rollups from one number.

/// Why a disabled account fetched nothing. Not an error — the operator turned
/// it off — but still reported, because the repos it owns are still tracked
/// and their rows going blank with no explanation is the surprising half.
const ACCOUNT_DISABLED_MESSAGE: &str = "disabled in Settings";

/// Why an enabled account with an empty credential fetched nothing.
const ACCOUNT_TOKEN_MISSING_MESSAGE: &str = "no token saved — add one in Settings";

/// Why an account whose [`VendorAccount::secret_account`] this build cannot
/// resolve fetched nothing. See [`account_secret_key`] for the refusal.
const ACCOUNT_CREDENTIAL_UNKNOWN_MESSAGE: &str =
    "unrecognised credential item — re-save this account's token in Settings";

/// The credential-store item one vendor account's token lives in, or `None`
/// when this build cannot resolve the name the account carries.
///
/// [`VendorAccount::secret_account`] is a **stored string**, not a derivation,
/// so exactly two spellings are legitimate: the pre-existing
/// [`SecretKey::GitHubAccessToken`] item the account `store::migrate_v1_to_v2`
/// mints keeps pointing at, and the `vendor-<uuid>` item a freshly created
/// account gets. Anything else is refused rather than resolved — an account
/// naming *another* account's item is precisely the "a row fetched with
/// somebody else's token" this epic exists to make impossible, and a name
/// nothing recognises is a misconfiguration to report, not to guess at.
///
/// Fail-closed on purpose: the caller turns `None` into a named failure the
/// operator can see, never into a silent fallback to whatever credential is
/// nearest.
fn account_secret_key(account: &VendorAccount) -> Option<SecretKey> {
    let own = SecretKey::VendorToken(account.id);
    if account.secret_account == own.account() {
        return Some(own);
    }
    if account.secret_account == SecretKey::GitHubAccessToken.account() {
        return Some(SecretKey::GitHubAccessToken);
    }
    None
}

/// One identity a GitHub pass polls with: an account from the store, or the v1
/// credential when the store has no account at all.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GitHubIdentity {
    /// The account's id, or `None` for the v1 credential. See
    /// [`github::AccountResult::Ok`] for why that one needs none.
    account: Option<Uuid>,
    label: String,
    enabled: bool,
    /// Where this identity's token lives, or `None` when the stored item name
    /// is unresolvable ([`account_secret_key`]).
    key: Option<SecretKey>,
    /// Orgs whose runners this identity watches: the account's selection —
    /// trimmed, blanks dropped — or, for the legacy identity only, the
    /// unmigrated global `github_org`.
    orgs: Vec<String>,
}

impl GitHubIdentity {
    /// Whether this identity is the v1 credential rather than an account.
    ///
    /// It is the one identity whose failure is **not** a per-account footer
    /// line: with no account configured there is no account to name, and
    /// "connect a GitHub token in Settings" is the whole of what a first launch
    /// has to say. Its three states are the panel's, via [`PassCredential`].
    fn is_legacy(&self) -> bool {
        self.account.is_none()
    }
}

/// The identities one pass polls, in store order.
///
/// A store with no GitHub account yields the v1 credential alone. That is not a
/// fallback picked for convenience: a v1 store held exactly one GitHub token,
/// so every repo in it provably belongs to that one — the same proof
/// `store::migrate_v1_to_v2` relies on, and the state the app is in between a
/// first launch with no token and the relaunch that migrates — the GitHub tab
/// still writes that one credential and mints no account for it. Settings →
/// **Accounts** is where an account is created.
///
/// Non-GitHub accounts are filtered out rather than polled: a credential minted
/// for another vendor's API must never be sent to GitHub's.
fn github_identities(accounts: &[VendorAccount], legacy_org: &str) -> Vec<GitHubIdentity> {
    let selected_orgs = |account: &VendorAccount| {
        account
            .orgs
            .iter()
            .flatten()
            .map(|org| org.trim().to_owned())
            .filter(|org| !org.is_empty())
            .collect()
    };
    let identities: Vec<GitHubIdentity> = accounts
        .iter()
        .filter(|account| account.vendor == VendorKind::GitHub)
        .map(|account| GitHubIdentity {
            account: Some(account.id),
            label: account.label.clone(),
            enabled: account.enabled,
            key: account_secret_key(account),
            orgs: selected_orgs(account),
        })
        .collect();
    if identities.is_empty() {
        // The unmigrated store's org rides the legacy identity — without this,
        // every not-yet-migrated store's Runners panel would go dark between
        // the v3 upgrade and its migration completing. Self-eliminating:
        // `migrate_v2_to_v3` clears `github_org` when it adopts it.
        let legacy_org = legacy_org.trim();
        return vec![GitHubIdentity {
            account: None,
            label: String::new(),
            enabled: true,
            key: Some(SecretKey::GitHubAccessToken),
            orgs: if legacy_org.is_empty() {
                Vec::new()
            } else {
                vec![legacy_org.to_owned()]
            },
        }];
    }
    identities
}

/// What one pass learned about the panels' credentials **as a whole** — the
/// claim both panels' setup lines are made of.
///
/// The first three are the branches the single-credential
/// read has always had. [`Unlooked`](PassCredential::Unlooked) is the fourth a
/// multi-account pass can be in: every account disabled, or every one of them
/// naming a credential item this build cannot resolve, so nothing was read at
/// all. Neither "there is no token" nor "we could not ask" is true then, and a
/// defaulted state is as much a fabrication as a defaulted number — so it says
/// nothing, and the per-account footer carries the story.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PassCredential {
    Present,
    Absent,
    Unreadable,
    Unlooked,
}

/// One identity that has a token, and the repos it owns.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GitHubFetch {
    account: Option<Uuid>,
    label: String,
    token: String,
    /// `(slug, watched workflows)`, in portfolio order.
    repos: Vec<(String, Option<Vec<String>>)>,
    /// Orgs whose runners this fetch polls, each with this fetch's own token —
    /// the per-account cardinality the single global `github_org` never had.
    orgs: Vec<String>,
}

/// One selected org a pass cannot poll, with the owning account's label and
/// the same blocked reason its repos got. Reported rather than silently
/// unpolled: a selected org whose rows quietly stop updating is the
/// runners-side of "a tracked repo that quietly stops being fetched".
#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockedOrg {
    org: String,
    label: String,
    reason: String,
}

/// Everything a pass can decide before its first request: who to poll with, who
/// already cannot be polled with, and what the panels should say about the
/// credential store.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GitHubPass {
    fetches: Vec<GitHubFetch>,
    /// The results already known — blocked accounts and unattributed repos.
    /// The fetches' results are appended to these.
    reports: Vec<github::AccountResult>,
    /// Selected orgs that cannot be polled this pass. The legacy identity
    /// never lands here: with no account configured there is nothing to name,
    /// and the panel's own setup line is the whole story.
    blocked_orgs: Vec<BlockedOrg>,
    credential: PassCredential,
}

/// Plans one GitHub pass: reads every enabled identity's credential and
/// attributes every enabled repo to one of them.
///
/// **Every credential read happens here, before the first request** — the same
/// discipline `apply_token_present` exists for, once per account rather than
/// once per pass: a panel that waits for a fetch to finish before admitting it
/// has a token spends the whole first pass telling the operator to connect one.
///
/// A repo whose `account_id` names nothing this pass knows about is
/// **unattributed**, never reassigned. That is the security half of this
/// function: the only token a repo is ever fetched with is the one its own
/// account names.
///
/// Takes the whole portfolio and filters it: a *disabled* repo is not fetched
/// and must not be reported as unattributed either — it is not on the board at
/// all, so there is nothing missing from it to explain.
fn plan_github_pass<C: CredentialStore + ?Sized>(
    credentials: &C,
    accounts: &[VendorAccount],
    repos: &[TrackedRepo],
    legacy_org: &str,
) -> GitHubPass {
    let identities = github_identities(accounts, legacy_org);
    let legacy = identities.iter().all(GitHubIdentity::is_legacy);
    let tracked: Vec<&TrackedRepo> = repos.iter().filter(|repo| repo.enabled).collect();

    let mut fetches = Vec::new();
    let mut reports = Vec::new();
    let mut blocked_orgs = Vec::new();
    let (mut present, mut unreadable, mut absent) = (false, false, false);

    for identity in &identities {
        // In a store with no account, the repos that name none are the v1
        // credential's. In a store with accounts they belong to nobody, and
        // stay that way: attribution is decided once, when the repo is added
        // (`settings::new_repo`) or when the Portfolio row's picker is used.
        // A pass adopting one would be "the first account" by another name.
        let owned: Vec<&&TrackedRepo> = tracked
            .iter()
            .filter(|repo| {
                repo.account_id == identity.account || (legacy && repo.account_id.is_none())
            })
            .collect();

        let blocked = if !identity.enabled {
            Some(ACCOUNT_DISABLED_MESSAGE.to_owned())
        } else {
            match identity.key {
                None => Some(ACCOUNT_CREDENTIAL_UNKNOWN_MESSAGE.to_owned()),
                Some(key) => match read_credential(credentials, key) {
                    Credential::Present(token) => {
                        present = true;
                        fetches.push(GitHubFetch {
                            account: identity.account,
                            label: identity.label.clone(),
                            token,
                            repos: owned
                                .iter()
                                .map(|repo| (repo.slug.clone(), repo.watched_workflows.clone()))
                                .collect(),
                            orgs: identity.orgs.clone(),
                        });
                        None
                    }
                    Credential::Absent => {
                        absent = true;
                        Some(ACCOUNT_TOKEN_MISSING_MESSAGE.to_owned())
                    }
                    Credential::Unreadable => {
                        unreadable = true;
                        Some(CREDENTIAL_UNREADABLE_MESSAGE.to_owned())
                    }
                },
            }
        };

        // A disabled account with nothing tracked is not news: switching one
        // off is a deliberate act, and the footer is for what it took with it.
        // An *enabled* one that cannot authenticate is reported either way.
        let worth_reporting = identity.enabled || !owned.is_empty();
        if let Some(reason) = blocked {
            if !identity.is_legacy() {
                // The org report has no `worth_reporting` gate: a disabled
                // account with no repos may still watch an org, and its rows
                // going stale with no explanation is the same silence.
                blocked_orgs.extend(identity.orgs.iter().map(|org| BlockedOrg {
                    org: org.clone(),
                    label: identity.label.clone(),
                    reason: reason.clone(),
                }));
            }
            if !identity.is_legacy() && worth_reporting {
                reports.push(github::AccountResult::Failed {
                    account: identity.account,
                    label: identity.label.clone(),
                    reason,
                    slugs: owned.iter().map(|repo| repo.slug.clone()).collect(),
                });
            }
        }
    }

    // Whatever no identity claimed. Reported last so the footer reads
    // accounts-then-orphans, and reported at all because a tracked repo that
    // quietly stops being fetched is the failure this replaces.
    let claimed: Vec<Option<Uuid>> = identities.iter().map(|i| i.account).collect();
    let unattributed: Vec<String> = tracked
        .iter()
        .filter(|repo| !claimed.contains(&repo.account_id))
        .map(|repo| repo.slug.clone())
        .collect();
    if !unattributed.is_empty() {
        reports.push(github::AccountResult::Unattributed {
            slugs: unattributed,
        });
    }

    GitHubPass {
        fetches,
        reports,
        blocked_orgs,
        // The best news any identity brought, and `Unreadable` outranks
        // `Absent`: one account holding a good token means the panels *are*
        // configured whatever the others are doing, and a keychain that would
        // not answer for one of them is never "there is no token" — that claim
        // belongs to a pass that asked and was told nothing is stored.
        credential: if present {
            PassCredential::Present
        } else if unreadable {
            PassCredential::Unreadable
        } else if absent {
            PassCredential::Absent
        } else {
            PassCredential::Unlooked
        },
    }
}

/// Applies one pass's credential verdict to the panel state.
///
/// A function of its own, over `&mut GitHubState`, because the whole of this
/// decision is which state two panels enter — and every one of them is testable
/// here without a token, a keychain or a network.
///
/// [`PassCredential::Present`] is recorded **before a single request goes
/// out**. That arm used to write nothing until a fetch completed, so the panels
/// went on claiming there was no credential for the whole of the pass that was
/// holding one — several seconds of "connect a GitHub token in Settings" on
/// every launch, at a machine where the token was fine.
fn apply_pass_credential(credential: PassCredential, state: &mut GitHubState) {
    match credential {
        PassCredential::Present => state.apply_token_present(),
        // The only branch that may claim nobody configured this: we asked, and
        // the store said there is nothing stored.
        PassCredential::Absent => state.apply_unauthenticated(),
        PassCredential::Unreadable => {
            state.apply_credential_unreadable(CREDENTIAL_UNREADABLE_MESSAGE)
        }
        // Nothing was read, so there is nothing to report — and no claim about
        // a credential store this pass never opened.
        PassCredential::Unlooked => {}
    }
}

/// One read of every **watched** vendor's status page, folded into the panel
/// state.
///
/// The watched set is [`services::active_vendors`]'s, derived from this store's
/// own configuration (#284) and **re-derived every pass** — never the five
/// built-ins this file used to walk. That is what makes a status page added in
/// Settings actually watched (#375): before it, an operator could add a vendor,
/// watch the probe succeed and see it listed, while the poll pass went on
/// reading the same five hardcoded pages and the panel went on rendering only
/// those. A panel silently claiming coverage it does not have is the failure
/// `crates/servicestatus` exists to remove.
///
/// A failure is recorded but does **not** drop the last good reading: a
/// vendor's status does not change on the timescale of one dropped request, and
/// a panel that flipped from "it's GitHub" back to a red "it's us" on a single
/// timeout would be the exact misdirection that verdict exists to prevent.
async fn poll_service_status(app: &Arc<App>) {
    // Anthropic is the one vendor with no credential to look for, so its
    // evidence is whether the Usage panel has actually found Claude rollups.
    // Read here rather than assumed: "not walked yet" must answer `false`, so
    // the tile appears when the data does rather than ahead of it.
    let claude_usage_present = {
        let usage = app.usage.lock().expect("usage state poisoned");
        usage.claude_present()
    };
    // One credential read per vendor per pass — the same re-read discipline as
    // the token below, and the reason a Save or Clear in Settings applies on the
    // next pass rather than on the next launch.
    let vendors = {
        let store = app.store.lock().expect("store poisoned");
        services::active_vendors(&store, &*app.credentials, claude_usage_present)
    };

    // Concurrently: independent hosts, and running them in sequence would make
    // the pass as slow as their sum for no reason. Each carries its own 8s
    // timeout, so the worst case is one timeout rather than all of them.
    let results = futures_util::future::join_all(vendors.iter().map(|vendor| async move {
        // `read_vendor`, never `read`: an operator-added vendor's page lives on
        // its own record and nowhere else, and `read` panics rather than
        // fabricate one.
        (vendor.service, services::read_vendor(vendor).await)
    }))
    .await;

    // Re-read like the token, so switching the preference off applies on the
    // next pass rather than on the next launch.
    let notify_changes = {
        let store = app.store.lock().expect("store poisoned");
        store.settings().notify_on_service_change
    };

    let watched = vendors.len();
    let labels: Vec<(services::ServiceId, String)> = vendors
        .iter()
        .map(|vendor| (vendor.service, vendor.label.clone()))
        .collect();
    let mut failures: Vec<(String, &'static str)> = Vec::new();
    let (notices, transition) = {
        let mut statuses = app.services.lock().expect("services poisoned");
        // Before the results are folded in, so a vendor that left the watched
        // set takes its last reading with it rather than lingering in the map
        // to be rendered as fresh if it is ever added back.
        statuses.watching(vendors);
        let (mut read, mut failed) = (0_usize, 0_usize);
        for (service, result) in results {
            match result {
                Ok(status) => {
                    read += 1;
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
                    failed += 1;
                    let label = labels
                        .iter()
                        .find(|(id, _)| *id == service)
                        .map_or_else(|| service.id(), |(_, label)| label.clone());
                    failures.push((label, e.kind()));
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
        // The footer's last-success clock; what counts as a success is
        // `pass_finished`'s doc.
        let transition = statuses.pass_finished(panel::now_unix(), read, failed);
        // Diffed against what this pass actually obtained. A failed refresh
        // keeps the last good status for *rendering*, and treating that
        // retained value as a fresh observation would make an unreachable page
        // look like a steady state forever — so `readings()` reports `None`
        // wherever the last read failed.
        let mut watch = app.service_status.lock().expect("status watch poisoned");
        (
            watch.observe(&statuses.readings(), notify_changes),
            transition,
        )
    };

    // One line on entering "every vendor failed" and one on leaving it, never
    // one per failed pass (the `refusal_transition` rule). Labels and fixed
    // kind words only.
    match transition {
        Some(services::PassTransition::Entered) => {
            eprintln!("{}", services::pass_failed_log_line(watched, &failures));
        }
        Some(services::PassTransition::Recovered) => {
            eprintln!("services: status pages are readable again");
        }
        None => {}
    }

    // Last, and outside every lock: showing a banner is the one thing in this
    // function that leaves the process.
    deliver_status_notices(app, &notices);
}

/// One pass over every GitHub source: each enabled repo's health **fetched by
/// the account that owns it**, this machine's git checkouts, and the org's
/// self-hosted runners.
///
/// The credential is one per account (#283), so the repos half of this pass is
/// N fetches with N tokens, merged by [`github::merged_health`] and reported
/// per account. One account failing costs that account's rows and nothing else;
/// [`plan_github_pass`] decides all of it before the first request goes out.
///
/// No lock is ever held across an `await`; each is taken, used and dropped, in
/// sequence, exactly as [`poll_containers`] does.
async fn poll_github(app: &Arc<App>) {
    // The Services pass is not here any more (#615): it has a loop and a
    // cadence of its own, `services_loop`, which runs with no GitHub token for
    // the reason it always did — no status page needs a credential, and
    // "GitHub Actions is in a major outage" is most useful precisely when this
    // panel is otherwise blank.
    //
    // This pass is told the staleness window of *its* cadence, so a 5-minute
    // GitHub cadence does not read as a stuck poller between two healthy passes.
    sync_app_cadences(app);

    // Re-read every pass rather than captured at startup: that is what makes a
    // Save or Clear in Settings apply without a relaunch. `legacy_org` is the
    // unmigrated store's `github_org`; the migrated store carries orgs on its
    // accounts and this reads as "".
    let (accounts, repos, legacy_org, records) = {
        let store = app.store.lock().expect("store poisoned");
        (
            store.accounts().to_vec(),
            store.repos().to_vec(),
            store.settings().github_org.trim().to_owned(),
            store.runner_roster().to_vec(),
        )
    };

    // One credential read per account, every one of them before the first
    // request.
    let pass = plan_github_pass(&*app.credentials, &accounts, &repos, &legacy_org);
    let mut results = pass.reports;
    // Every selected org, pollable or not, with its polling account's label —
    // known before the first request, exactly like the credential verdict.
    let selection: Vec<(String, String)> = pass
        .fetches
        .iter()
        .flat_map(|fetch| {
            fetch
                .orgs
                .iter()
                .map(|org| (org.clone(), fetch.label.clone()))
        })
        .chain(
            pass.blocked_orgs
                .iter()
                .map(|blocked| (blocked.org.clone(), blocked.label.clone())),
        )
        .collect();
    {
        let mut state = app.github.lock().expect("github state poisoned");
        apply_pass_credential(pass.credential, &mut state);
        state.apply_runner_selection(&selection);
        // A blocked account's orgs are reported per org, so one account's
        // missing token cannot silently freeze another account's rows.
        for blocked in &pass.blocked_orgs {
            state.apply_org_runners_error(&blocked.org, &blocked.reason);
        }
    }
    // Nothing to poll with. The pass still records what it knows — which
    // accounts are blocked, and which repos no account claims — because that
    // is the whole of what this pass found out, and dropping it would leave
    // two panels with no explanation for rows that are not there.
    if pass.fetches.is_empty() {
        let mut state = app.github.lock().expect("github state poisoned");
        if !results.is_empty() {
            state.apply_repo_accounts(&results, panel::now_unix());
        }
        return;
    }

    // Walking `~/Repos` and spawning `git` twice per repo blocks; keep it off
    // the async executor, exactly as `docker ps` is.
    let local = tokio::task::spawn_blocking(|| {
        github::git::scan(&github::git::default_roots(), github::git::MAX_DEPTH)
    })
    .await
    .map_err(|e| eprintln!("local git scan failed: {e}"))
    .ok();

    let now = github::now_utc();
    // One client per account, each holding that account's own token — the
    // whole point of #290. A repo is only ever fetched by the account that
    // owns it, so a private repo can never be read with a credential its
    // operator never pointed at it.
    //
    // Sequential per repo, matching `GHWorkflowsService.refresh()`: each
    // `repo_health` already fires its four side counts concurrently (three
    // REST reads and the board walk, one GraphQL POST per hundred open
    // issues), so a six-repo portfolio is 30 requests either way — doing
    // them all at once would only spend the rate-limit budget faster.
    // Sequential per *account* for the same reason.
    for fetch in &pass.fetches {
        let client = github::GitHubClient::new(fetch.token.clone());
        let mut health = Vec::with_capacity(fetch.repos.len());
        for (slug, watched) in &fetch.repos {
            health.push(client.repo_health(slug, watched.as_deref(), now).await);
        }
        results.push(github::AccountResult::Ok {
            account: fetch.account,
            label: fetch.label.clone(),
            health,
        });
    }

    // The roster is only ever advanced by these calls, and each only returns
    // `Ok` on a successful fetch — so a failing GitHub leaves every absence
    // clock frozen at the last successful poll instead of ageing a healthy
    // runner into a red alarm. Per (account, org): every selected org is
    // polled with its **owning account's** token — the cardinality the single
    // global `github_org` never had — and each org's fetch sees only its own
    // slice of the roster, so one org's answer can never mark another org's
    // runners absent. Sequential for the same rate-limit reason as the repos
    // half above. An account with no selected orgs contributes nothing here,
    // and the repos half of the pass runs either way — repos are tracked by
    // full `owner/name` slug and need no organization.
    let mut org_outcomes = Vec::new();
    for fetch in &pass.fetches {
        if fetch.orgs.is_empty() {
            continue;
        }
        let client = github::GitHubClient::new(fetch.token.clone());
        for org in &fetch.orgs {
            let org_roster = github::roster_for_org(&records, org);
            org_outcomes.push((
                org.clone(),
                client
                    .runner_roster(org, &org_roster, now, github::RUNNER_GRACE_SECS)
                    .await,
            ));
        }
    }

    // Re-read like the token, so switching the preference off applies on the
    // next pass rather than on the next launch.
    let notify_approvals = {
        let store = app.store.lock().expect("store poisoned");
        store.settings().notify_on_approval_needed
    };
    // Diffed before the results are handed to the panel state, because the diff
    // needs the *previous* pass and `apply_repo_accounts` replaces it
    // wholesale. Its own lock: this is delivery memory, not anything either
    // panel renders. `merged_health` rather than a second fold of the same
    // results, so what the watch diffs and what the panel shows cannot drift.
    let notices = {
        let mut watch = app.approvals.lock().expect("approval watch poisoned");
        watch.observe(&github::merged_health(&results), notify_approvals)
    };

    {
        let mut state = app.github.lock().expect("github state poisoned");
        state.apply_repo_accounts(&results, panel::now_unix());
        if let Some(local) = local {
            state.apply_local(local);
        }
        // Per org, so one org's failure never blanks or discredits another's
        // rows — the AccountResult discipline, one panel over. An empty
        // selection applies nothing on purpose: the panel's own setup line is
        // the whole of what there is to say, and an error here would bury it.
        for (org, outcome) in &org_outcomes {
            match outcome {
                Ok(update) => state.apply_org_runners(org, update, panel::now_unix()),
                // The transport error is logged, not shown: a 403 for a missing
                // scope is what this almost always is, and "HTTP 403" sends the
                // operator to check the network instead of the PAT. A walk
                // refused for size is the exception and says so itself.
                Err(e) => {
                    eprintln!("org runners fetch failed for {org}: {e}");
                    state.apply_org_runners_error(org, github::runners_error_message(e));
                }
            }
        }
    }

    // Persisted only on success, per successful org — a failed org's records
    // stay exactly as they were, which is its absence clocks staying frozen —
    // and only when something actually changed: a steady fleet produces an
    // identical roster poll after poll, and rewriting the store file every
    // minute for no change is a write nobody asked for.
    {
        let mut store = app.store.lock().expect("store poisoned");
        let mut changed = false;
        for (org, outcome) in &org_outcomes {
            if let Ok(update) = outcome {
                changed |= store
                    .set_runner_roster_for_org(org, github::roster_to_records(&update.roster, org));
            }
        }
        if changed {
            if let Err(e) = store.save() {
                eprintln!("could not persist the runner roster: {e}");
            }
        }
    }

    // Last, and outside every lock: showing a banner is the one thing in this
    // pass that leaves the process.
    deliver_approval_notices(app, &notices);
}

/// Claim the notification application identity before the plugin can take it.
///
/// **`tauri-plugin-notification` posts every banner as `com.apple.Terminal`
/// here, in release builds too.** Its desktop `show()` does:
///
/// ```ignore
/// let _ = notify_rust::set_application(if tauri::is_dev() {
///     "com.apple.Terminal"
/// } else {
///     &self.identifier
/// });
/// ```
///
/// and `tauri::is_dev()` is `!cfg!(feature = "custom-protocol")`. That feature
/// is enabled by the **Tauri CLI**, which this repo deliberately does not have
/// (`app/README.md`: "There is no Tauri CLI in this repo") — `scripts/run.sh`
/// and `scripts/build.sh` are both plain `cargo build`. So the branch is never
/// taken and the identity is *always* Terminal's, `./prd` included. The
/// "unbundled dev build notifies as Terminal" note this repo carried was
/// therefore wrong about the cause and about the scope: packaging (#15) would
/// not have fixed it.
///
/// **That is a dropped notification, not a cosmetic attribution bug.** macOS
/// authorises delivery against the *posting* bundle, so the banner inherits
/// whatever Terminal.app was granted — and on a machine whose operator uses a
/// different terminal, Terminal.app has been granted nothing and never appears
/// in Notification settings to grant. Measured 2026-08-13 on this machine, same
/// title and body, same bundle, one process each: posted as
/// `app.solador.desktop` the banner renders in full; posted as
/// `com.apple.Terminal` **it does not appear at all**. It also means the
/// operator's own per-app "Solador" notification settings never governed the
/// app, because the app never posted under that identity.
///
/// The fix is one call, and it works *with* the library rather than around it:
/// `set_application` is `call_once`, so the **first** caller wins and every
/// later one gets `AlreadySet` — which the plugin discards with `let _ =`.
/// Claiming ours here makes the plugin's Terminal call a no-op, with nothing
/// patched or vendored.
///
/// Ordering is guaranteed, not hoped for: `deliver_banners` drops every notice
/// until `app.handle` is set, and this runs in the same `setup` hook
/// immediately before that. No banner can be delivered ahead of it.
///
/// The identifier is read from the Tauri config rather than written out, so it
/// cannot drift from `tauri.conf.json`.
///
/// > Enabling `custom-protocol` would also flip `is_dev()`, but that feature
/// > additionally switches asset loading to the embedded-protocol path this
/// > app does not use (`frontendDist` points at a static directory). This is
/// > the surgical half.
///
/// **A failed claim is not a neutral outcome, which is why it is logged.**
/// `call_once` fires whether or not the underlying `setApplication` succeeded,
/// so an unresolvable identifier consumes the `Once` *and* leaves the library
/// on its own fallback — which `mac-notification-sys` spells, in the error
/// text for that very case, as `com.apple.Terminal`. Failing here therefore
/// reproduces exactly the bug this function exists to remove, silently. The
/// identifier resolves via LaunchServices, so the case to watch is a build
/// whose `.app` wrapper was never created (`scripts/run.sh` and
/// `scripts/build.sh` both build one on macOS).
#[cfg(target_os = "macos")]
fn claim_notification_identity(identifier: &str) {
    match mac_notification_sys::set_application(identifier) {
        Ok(()) => {}
        // Logged rather than surfaced, and never fatal: a cockpit panel is the
        // wrong place to report this, and the app is fully usable without
        // banners. It is logged at all because a silent failure here is
        // indistinguishable from "no notifications happened to fire".
        Err(e) => eprintln!("could not claim the notification identity {identifier}: {e}"),
    }
}

/// The original's `content.sound = .default`, spelled the way `notify-rust` wants it.
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
        // Logged, not surfaced: this is the same silent no-op the original takes when
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

// MARK: - Updates (#308)

/// Ask the feed whether there is a newer Solador, and record the answer.
///
/// Runs once from the `setup` hook and again whenever the operator presses
/// **Check for updates**. There is no loop: the Settings surface promises a
/// check at launch and nothing else, and a cockpit that reaches a release
/// server on a cadence all day would be a different promise.
///
/// **Every outcome is recorded as itself.** `Ok(None)` is up to date, `Err` is
/// a failed check, and the two are separate states all the way to the screen
/// (`viewmodel::update`) — a check that could not run must never paint as one
/// that ran and found nothing, which is the update-shaped version of an
/// unmeasured metric rendering as zero.
///
/// **And some builds are not asked at all.** The first thing this does is put a
/// question to the *build* rather than to the feed (#353): a `./dev` cockpit has
/// no installed release for a `.app.tar.gz` to be unpacked over, so it gets no
/// verdict about being behind one. Returning here rather than filtering the
/// answer afterwards is what makes that concrete — a development build sends no
/// request to the release server whatsoever.
async fn run_update_check(app: Arc<App>) {
    use tauri_plugin_updater::UpdaterExt as _;

    if let Some(why) = update::not_updatable(settings::VERSION) {
        // Logged like the other two non-trivial outcomes below, and for a
        // sharper reason: "no banner appeared" is also what a check that never
        // ran looks like. This line is the difference, and it is what the
        // manual smoke checklist reads.
        eprintln!("update: not checked ({why:?})");
        app.update
            .lock()
            .expect("update state poisoned")
            .not_updatable(why);
        return;
    }

    let Some(handle) = app.handle.get().cloned() else {
        // Only reachable if this is ever called before `setup`. Recorded as a
        // failure rather than silently skipped: "we did not look" is not "there
        // is nothing there".
        app.update
            .lock()
            .expect("update state poisoned")
            .check_failed(update::NO_HANDLE.to_string());
        return;
    };

    app.update.lock().expect("update state poisoned").checking();

    let outcome = match handle.updater() {
        Ok(updater) => updater.check().await,
        Err(e) => Err(e),
    };

    // Decided under the lock, delivered outside it — the same shape every other
    // notifier in this file takes.
    let notice = {
        let mut state = app.update.lock().expect("update state poisoned");
        match outcome {
            Ok(Some(found)) => {
                eprintln!("update: {} is available", found.version);
                state.found(found.version, found.body);
                state.notice()
            }
            Ok(None) => {
                state.up_to_date();
                None
            }
            Err(e) => {
                // The plugin's own wording. It names the transport failure
                // ("Network Error", a status code, a malformed manifest), which
                // is more use to an operator than anything this layer could
                // invent on top of it.
                eprintln!("update: the check failed: {e}");
                state.check_failed(e.to_string());
                None
            }
        }
    };

    if let Some((title, body)) = notice {
        deliver_banners(&app, "update-available", &[(&title, &body)]);
    }
}

/// Download and apply the offered update, on the operator's say-so.
///
/// The offer is re-fetched rather than held from the check, and that is
/// deliberate: keeping the plugin's `Update` in [`App`] would put a live HTTP
/// client and an `AppHandle` inside the state every test constructs, and the
/// second request costs one round trip against a feed we are about to download
/// ~30 MB from. If the feed has moved on in between, this installs what is
/// actually offered now and says so.
///
/// **Nothing restarts.** The bundle on disk is replaced; the running process
/// keeps running the old one until the operator opens the app again. A cockpit
/// living full-screen on a second monitor does not get to close itself.
async fn run_update_install(app: Arc<App>) {
    use tauri_plugin_updater::UpdaterExt as _;

    let Some(handle) = app.handle.get().cloned() else {
        app.update
            .lock()
            .expect("update state poisoned")
            .install_failed(update::NO_HANDLE.to_string());
        return;
    };

    let outcome = match handle.updater() {
        Ok(updater) => updater.check().await,
        Err(e) => Err(e),
    };

    let found = match outcome {
        Ok(Some(found)) => found,
        Ok(None) => {
            // Refused rather than reported as success: there is nothing to
            // install, and "installed" would be a claim about a file that was
            // never written.
            app.update
                .lock()
                .expect("update state poisoned")
                .install_failed("the update is no longer offered".to_string());
            return;
        }
        Err(e) => {
            app.update
                .lock()
                .expect("update state poisoned")
                .install_failed(e.to_string());
            return;
        }
    };

    let version = found.version.clone();
    eprintln!("update: installing {version}");
    // The two callbacks are progress hooks. Left empty on purpose: the
    // frontend polls `update_status`, and a byte counter written from this
    // task would be state nothing renders.
    match found.download_and_install(|_, _| {}, || {}).await {
        Ok(()) => {
            eprintln!("update: {version} installed; it applies on the next launch");
            app.update
                .lock()
                .expect("update state poisoned")
                .installed();
        }
        Err(e) => {
            eprintln!("update: installing {version} failed: {e}");
            app.update
                .lock()
                .expect("update state poisoned")
                .install_failed(e.to_string());
        }
    }
}

/// Guards the Updates surface's counterpart to [`FIRST_REQUEST`].
static FIRST_UPDATE_REQUEST: std::sync::Once = std::sync::Once::new();

/// The Updates group, polled by the About tab.
///
/// A command the frontend asks, not an event Rust pushes: there is not one
/// `emit` in this file, and an updater is no reason to introduce the first.
#[tauri::command]
fn update_status(state: tauri::State<'_, Arc<App>>) -> Value {
    let payload = {
        let updates = state.update.lock().expect("update state poisoned");
        update::view(&updates, settings::VERSION)
    };
    FIRST_UPDATE_REQUEST.call_once(|| {
        eprintln!(
            "update_status: first frontend request ({})",
            payload["status"]["text"].as_str().unwrap_or_default()
        );
    });
    payload
}

/// Re-run the check now.
///
/// Answers with the state as it stands the instant the check *starts*, which is
/// "Checking for updates…". The result lands in `App::update` when the request
/// completes and the frontend's next poll picks it up — there is nothing to
/// await here, and blocking the IPC call on a release server would freeze the
/// Settings window for as long as it took to time out.
#[tauri::command]
fn update_check(state: tauri::State<'_, Arc<App>>) -> Value {
    let app = Arc::clone(&state);
    // Here, not only inside the spawned task: this answer is what repaints the
    // group, and a scheduled-but-not-yet-started check would otherwise answer
    // with the previous verdict — a press that visibly does nothing.
    app.update.lock().expect("update state poisoned").checking();
    app.runtime.spawn(run_update_check(Arc::clone(&app)));
    let updates = app.update.lock().expect("update state poisoned");
    update::view(&updates, settings::VERSION)
}

/// Install the offered update, on an operator's click.
///
/// The guard is `can_install`, checked here rather than trusted from the
/// frontend: a button the payload said not to draw is still a command the
/// webview can invoke, and two clicks landing together must not start two
/// downloads over the same bundle.
#[tauri::command]
fn update_install(state: tauri::State<'_, Arc<App>>) -> Value {
    let app = Arc::clone(&state);
    {
        let mut updates = app.update.lock().expect("update state poisoned");
        if !updates.can_install() {
            return update::view(&updates, settings::VERSION);
        }
        updates.installing();
    }
    app.runtime.spawn(run_update_install(Arc::clone(&app)));
    let updates = app.update.lock().expect("update state poisoned");
    update::view(&updates, settings::VERSION)
}

/// The GitHub panels' poll loop, on [`PanelInterval::GitHub`].
///
/// The interval is re-read after every pass rather than baked into a
/// `tokio::time::interval`, and the sleep is interruptible — together that is
/// what makes a Settings change apply *now* rather than up to five minutes
/// later, which is the periodic-service equivalent of [`reload_hosts`].
async fn github_loop(app: Arc<App>) {
    loop {
        poll_github(&app).await;
        let secs = panel_interval(&app.store, PanelInterval::GitHub);
        tokio::select! {
            () = tokio::time::sleep(secs) => {}
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

/// The Services pass's loop, on [`PanelInterval::Services`] (#615).
///
/// It runs whether or not a GitHub token exists: that was the whole reason the
/// pass sat above the token gate in [`poll_github`], and a loop of its own
/// keeps it true by construction instead of by the order of two statements.
/// Re-reads its cadence after every pass, like every loop here.
async fn services_loop(app: Arc<App>) {
    loop {
        sync_app_cadences(&app);
        poll_service_status(&app).await;
        let secs = panel_interval(&app.store, PanelInterval::Services);
        tokio::select! {
            () = tokio::time::sleep(secs) => {}
            // `notify_one`, for the reason `github_loop` states.
            () = app.services_wake.notified() => {}
        }
    }
}

/// Cuts the Services loop's sleep short after an edit to the watched set or to
/// the cadence.
fn wake_services(app: &App) {
    app.services_wake.notify_one();
}

/// Guards the Repos panel's counterpart to [`FIRST_REQUEST`].
static FIRST_REPOS_REQUEST: std::sync::Once = std::sync::Once::new();

#[tauri::command]
fn repos(tile: Option<String>, state: tauri::State<'_, Arc<App>>) -> Result<Value, String> {
    scope_to_tile(&state, "ghWorkflows", tile.as_deref(), || {
        repos_unscoped(&state)
    })
}

/// [`repos`] without a tile: the whole source, as the dashboard reads it.
fn repos_unscoped(state: &App) -> Value {
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
fn runners(tile: Option<String>, state: tauri::State<'_, Arc<App>>) -> Result<Value, String> {
    scope_to_tile(&state, "ghRunners", tile.as_deref(), || {
        runners_unscoped(&state)
    })
}

/// [`runners`] without a tile: the whole source, as the dashboard reads it.
fn runners_unscoped(state: &App) -> Value {
    // Copied out and released before the GitHub state's lock: the two are
    // never held together, so this adds no lock ordering. Read on every
    // request, so a Settings edit regroups the next frame without a poll.
    let group_labels = state
        .store
        .lock()
        .expect("store poisoned")
        .runner_group_labels()
        .to_vec();
    let payload = {
        let panel = state.github.lock().expect("github state poisoned");
        github::runners_view(&panel, &group_labels, panel::now_unix())
    };
    FIRST_RUNNERS_REQUEST.call_once(|| {
        let rows = payload["rows"].as_array().map_or(0, Vec::len);
        eprintln!("runners: first frontend request ({rows} runner row(s))");
    });
    payload
}

/// The right-click "Forget" on a remembered-absent runner: drop the name from
/// the persisted roster and from the panel immediately, instead of waiting out
/// the roster's 24h age-out. The escape hatch for a deliberately
/// decommissioned runner — a downsized fleet is not an outage, and a red
/// "missing" row for a machine culled on purpose is a false alarm.
///
/// One accepted race: a poll pass reads the roster at its start and persists
/// its own copy at the end, so a pass in flight across this call can write the
/// name back with its old clock. The window is one fetch; a second Forget wins
/// it, and the 24h age-out mops up regardless.
#[tauri::command]
fn runners_forget(org: String, name: String, state: tauri::State<'_, Arc<App>>) {
    // Org-qualified end to end: a name two orgs share must only be forgotten
    // where the operator clicked, so the row payload carries its org and both
    // halves below filter on it.
    //
    // Store lock released before the panel lock is taken — the two are never
    // held together anywhere (`App::github`'s doc), and this must not become
    // the first place they are.
    {
        let mut store = state.store.lock().expect("store poisoned");
        let kept = github::forget_runner_record(store.runner_roster(), &org, &name);
        if store.set_runner_roster_for_org(&org, kept) {
            if let Err(e) = store.save() {
                eprintln!("could not persist the runner roster after a forget: {e}");
            }
        }
    }
    state
        .github
        .lock()
        .expect("github state poisoned")
        .forget_absent(&org, &name);
}

/// Which credentials currently hold a value — the "stored" badges, and nothing
/// else. A read failure reads as "not stored": the badge is a hint, and an
/// unreadable keychain must not take the Settings window down with it.
fn stored_secrets(
    credentials: &dyn CredentialStore,
    hosts: &[Host],
    accounts: &[VendorAccount],
) -> StoredSecrets {
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
        neon: present(SecretKey::NeonApiKey),
        sentry: present(SecretKey::SentryUsageToken),
        vercel: present(SecretKey::VercelApiToken),
        openclaw: present(SecretKey::OpenClawBearerToken),
        hosts: hosts
            .iter()
            .filter(|host| present(SecretKey::HostToken(host.id)))
            .map(|host| host.id)
            .collect(),
        // Through `account_secret_key`, so the badge reads the item the account
        // actually names -- the migrated account still points at the v1
        // `github_access_token` item, and probing `vendor-<uuid>` for it would
        // report "No token" over a token that is right there. An account whose
        // stored item name this build cannot resolve reads as not-stored; the
        // Repos panel is where that distinct finding is reported.
        accounts: accounts
            .iter()
            .filter(|account| account_secret_key(account).is_some_and(present))
            .map(|account| account.id)
            .collect(),
    }
}

/// The hosts whose published error is "the certificate changed" (#448): the
/// live half of what the host row needs to offer **Re-pair**.
fn certificate_changed_hosts(app: &App) -> Vec<Uuid> {
    let hosts = app.hosts.lock().expect("poll set poisoned");
    hosts
        .iter()
        .filter(|polled| {
            let state = polled.state.lock().expect("host state poisoned");
            state.error.is_some()
                && state.error_kind == Some(viewmodel::card::ERROR_KIND_CERTIFICATE_CHANGED)
        })
        .map(|polled| polled.key.id)
        .collect()
}

/// The hosts whose published error is "plain HTTP refused" (#449 part 3): an
/// unpaired host at an address that is neither loopback nor Tailscale. The
/// live half of what the host row needs to say why it is not being polled.
fn plain_refused_hosts(app: &App) -> Vec<Uuid> {
    let hosts = app.hosts.lock().expect("poll set poisoned");
    hosts
        .iter()
        .filter(|polled| {
            let state = polled.state.lock().expect("host state poisoned");
            state.error.is_some()
                && state.error_kind == Some(viewmodel::card::ERROR_KIND_PLAIN_HTTP_REFUSED)
        })
        .map(|polled| polled.key.id)
        .collect()
}

/// What each polled host last reported as its agent version (#489), for the
/// hosts that have reported one. A host missing from this list has told us
/// nothing — its row says so — and is never given a default.
fn agent_versions(app: &App) -> Vec<(Uuid, String)> {
    let hosts = app.hosts.lock().expect("poll set poisoned");
    hosts
        .iter()
        .filter_map(|polled| {
            let state = polled.state.lock().expect("host state poisoned");
            state.version.clone().map(|v| (polled.key.id, v))
        })
        .collect()
}

/// Fails a test loudly where the code would deadlock (#630): these entry
/// points lock the store, so calling one while the calling thread still holds
/// the store's guard hangs forever. `try_lock` cannot tell "this thread holds
/// it" from "another thread does", so this is test-only, where no poll loop
/// runs against the `App`; in a running app a `WouldBlock` is routine.
///
/// Not covered: `cockpit`, `dashboard_view` and the `#[tauri::command]`
/// wrappers that end in `Ok(dashboard_view(width, state))`, because they take
/// `tauri::State`, which unit tests cannot build. Covering the running app
/// would need a store wrapper that records the holder's `ThreadId`; a
/// `debug_assertions` check would fire spuriously, because the poll loops hold
/// the store briefly all the time.
#[cfg(test)]
fn assert_store_not_held(store: &Mutex<Store>) {
    assert!(
        !matches!(store.try_lock(), Err(std::sync::TryLockError::WouldBlock)),
        "store lock re-entered: the store was already held when a store-locking entry point ran"
    );
}

#[cfg(not(test))]
fn assert_store_not_held(_store: &Mutex<Store>) {}

/// The Settings payload for the app's current state.
fn settings_payload(app: &App) -> Value {
    assert_store_not_held(&app.store);
    // Read before the store's lock is taken, and never while it is held: the
    // OpenClaw session loop takes them in the same order, so there is one
    // ordering between the two and no way to invert it here.
    let facts = openclaw_settings_facts(app);
    // Also before the store's lock, and for the same reason: two locks taken in
    // one order, everywhere.
    let updates = {
        let state = app.update.lock().expect("update state poisoned");
        update::view(&state, settings::VERSION)
    };
    // The poll set's lock, taken and released before the store's: the same
    // one-at-a-time order every other reader of both uses.
    let certificate_changed = certificate_changed_hosts(app);
    let plain_refused = plain_refused_hosts(app);
    let versions = agent_versions(app);
    let agent_release = app
        .agent_release
        .lock()
        .expect("agent release poisoned")
        .clone();
    let store = app.store.lock().expect("store poisoned");
    let stored = stored_secrets(app.credentials.as_ref(), store.hosts(), store.accounts());
    settings::view(
        settings::StoreSections {
            agent_versions: &versions,
            agent_release: &agent_release,
            settings: store.settings(),
            hosts: store.hosts(),
            repos: store.repos(),
            vendors: store.status_vendors(),
            accounts: store.accounts(),
            certificate_changed: &certificate_changed,
            plain_refused: &plain_refused,
        },
        &stored,
        &facts,
        crash_facts(app),
        updates,
    )
}

/// The live half of the crash-reporting section: the store holds the toggle,
/// and these three are what no file can know.
///
/// Kept out of `StoreSections` for the reason the OpenClaw facts are: none of
/// it is in a file. Whether this build carries a DSN is a property of how it
/// was compiled; the other two are properties of this session.
fn crash_facts(app: &App) -> settings::CrashFacts {
    settings::CrashFacts {
        build_has_dsn: crashreport::BUILD_DSN.is_some(),
        active: app.crash_reporting.is_active(),
        needs_restart: app.crash_reporting.needs_restart(),
    }
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

/// Persists the store, turning a failure into the status line the original shows.
fn save_status(store: &Store, ok: impl Into<String>) -> Option<String> {
    match store.save() {
        Ok(()) => Some(ok.into()),
        Err(e) => Some(format!("Failed: {e}")),
    }
}

/// Guards the one-line "the frontend reached us" notice below.
static FIRST_REQUEST: std::sync::Once = std::sync::Once::new();

/// Cached readings only: editing or duplicating tiles never starts a poller.
#[tauri::command]
fn dashboard_view(width: f64, state: tauri::State<'_, Arc<App>>) -> Value {
    dashboard_view_in(width, &state)
}

/// [`dashboard_view`] over the app itself: every source whole, never scoped.
fn dashboard_view_in(width: f64, app: &App) -> Value {
    let layout = app
        .store
        .lock()
        .expect("store poisoned")
        .dashboard()
        .cloned()
        .unwrap_or_else(dashboard::default_layout);
    let mut hosts = cockpit_with(width, app, None);
    hosts["id"] = json!("hosts");
    machine_alerts::attach(
        &mut hosts,
        &app.store
            .lock()
            .expect("store poisoned")
            .settings()
            .machine_alerts,
    );
    let settings = app.store.lock().expect("store poisoned").settings().clone();
    let mut payloads = vec![
        hosts,
        repos_unscoped(app),
        runners_unscoped(app),
        containers_unscoped(app),
        services_unscoped(app),
        crons_unscoped(app),
        usage_unscoped(app),
        azure_cost_unscoped(app),
        openclaw_unscoped(app),
    ];
    cadence::attach(&mut payloads, &settings);
    dashboard::view(&layout, &payloads)
}

fn persist_dashboard(
    store: &mut Store,
    mut layout: store::DashboardLayout,
    expected_revision: u64,
) -> Result<store::DashboardLayout, String> {
    dashboard::validate(&layout)?;
    let revision = store.dashboard().map_or(0, |current| current.revision);
    if revision != expected_revision {
        return Err("The dashboard changed while you were editing. Review the current layout and try again.".into());
    }
    layout.revision = revision
        .checked_add(1)
        .ok_or("The dashboard revision cannot be advanced.")?;
    store
        .save_dashboard(layout.clone())
        .map_err(|error| format!("Could not save the dashboard: {error}"))?;
    Ok(layout)
}

#[tauri::command]
fn dashboard_save(
    layout: Value,
    expected_revision: u64,
    width: f64,
    state: tauri::State<'_, Arc<App>>,
) -> Result<Value, String> {
    // Raw JSON, not `DashboardLayout`: the typed read clamps view settings, and an
    // edit over IPC is refused when out of range instead.
    let layout = dashboard::layout_from_ipc(layout)?;
    persist_dashboard(
        &mut state.store.lock().expect("store poisoned"),
        layout,
        expected_revision,
    )?;
    // Return the persisted view together: a slow pre-save poll cannot leave
    // freshly saved configuration paired with the previous tile bodies.
    Ok(dashboard_view(width, state))
}

#[tauri::command]
fn dashboard_preview(
    tile: store::DashboardTile,
    width: f64,
    state: tauri::State<'_, Arc<App>>,
) -> Result<Value, String> {
    dashboard::preview(&tile, &dashboard_view(width, state))
}

#[tauri::command]
fn cockpit(
    width: f64,
    tile: Option<String>,
    state: tauri::State<'_, Arc<App>>,
) -> Result<Value, String> {
    // The IPC boundary has no automated coverage (#123), and every failure
    // mode the manual smoke test in `app/README.md` looks for — a rejected
    // ACL, an unregistered command, a CSP break that stops `app.js` before it
    // ever calls `invoke` — has the identical shape from in here: this
    // function never runs. So one line on the first call is the whole
    // terminal-side signal, and it makes the procedure runnable on a machine
    // whose screen you cannot see. It says nothing about what the frontend
    // then *painted* — that is what the visual read is still for.
    FIRST_REQUEST.call_once(|| {
        let hosts = state.hosts.lock().expect("poll set poisoned").len();
        eprintln!("cockpit: first frontend request ({hosts} host(s), {width}pt)");
    });
    match tile.as_deref() {
        None => Ok(cockpit_with(width, &state, None)),
        Some(id) => cockpit_for_tile(width, &state, id),
    }
}

/// The Machines payload narrowed to tile `id`'s scope, or why it has none.
///
/// The cards are built once ([`cockpit_frame`]) and laid out twice: whole, to
/// read which card ids the scope keeps (a scope such as `attention` is decided
/// by fields the build and `apply_machine_preferences` set), then again over
/// only those cards. Layout is arithmetic over finished cards; the build is the
/// part that takes the locks (#641).
fn cockpit_for_tile(width: f64, app: &App, id: &str) -> Result<Value, String> {
    let tile = tile_from_store(app, id, "hosts")?;
    let frame = cockpit_frame(app);
    let look = HostLook::from(&tile);
    let payload = frame.payload(width, None, look);
    let empty = dashboard::scoped_empty(&tile, &payload);
    let mut scoped = match kept_hosts(&tile, &payload) {
        Some(keep) => frame.payload(width, Some(&keep), look),
        None => payload,
    };
    tile_scope::stamp_empty(&mut scoped, empty);
    Ok(scoped)
}

/// The card ids a Machines tile keeps, read from the payload `cockpit_with`
/// returns. That payload is already past `apply_machine_preferences`, and a
/// host's attention comes from the card fields that call sets, so a scope such
/// as `attention` keeps the cards the tile shows.
fn kept_hosts(tile: &store::DashboardTile, payload: &Value) -> Option<BTreeSet<String>> {
    dashboard::scoped_row_ids(tile, payload)
}

/// The tile a panel command was asked for, read from the stored dashboard the
/// way `dashboard_view` reads it. The store lock is released before returning.
fn tile_from_store(app: &App, id: &str, source: &str) -> Result<store::DashboardTile, String> {
    let layout = app
        .store
        .lock()
        .expect("store poisoned")
        .dashboard()
        .cloned()
        .unwrap_or_else(dashboard::default_layout);
    tile_scope::find_tile(&layout, id, source)
        .cloned()
        .map_err(str::to_owned)
}

/// The payload `build` makes, narrowed to `tile`'s scope, or refused when the
/// tile is gone, hidden or of another source. No tile means the whole payload.
///
/// The tile is checked first and `build` runs only for a tile that is kept, so
/// a refused request builds nothing and fires none of the panel's `FIRST_*`
/// notices (#641).
fn scope_to_tile(
    app: &App,
    source: &str,
    tile: Option<&str>,
    build: impl FnOnce() -> Value,
) -> Result<Value, String> {
    let Some(id) = tile else {
        return Ok(build());
    };
    let tile = tile_from_store(app, id, source)?;
    Ok(tile_scope::scope_payload(&tile, build()))
}

/// Everything one cockpit frame reads from the app, before any layout: the
/// rendered cards and the settings that arrange them. [`cockpit_frame`] builds
/// it once per poll; [`CockpitFrame::payload`] lays it out as often as asked.
struct CockpitFrame {
    cards: Vec<Value>,
    /// Counted before a scope drops any card: whether hosts are configured is a
    /// question about the store, never about which cards a tile kept.
    configured_remotes: usize,
    alerts: store::machine_alerts::MachineAlerts,
    hidden: volumes::Hidden,
}

impl CockpitFrame {
    /// Today's `cockpit` body. With `keep`, cards outside it are dropped before
    /// layout, so the columns, core ladders, volume slots and tab bar see only
    /// the kept ones. `None` keeps every card.
    ///
    /// **The tile picks the look.** A Machines tile's Full view is sized from
    /// the window and takes the tile's own core rows and overflow mode
    /// (`look`); the overview's rows and the boot read name no tile and take
    /// `HostLook::default()`. Hand-edited stores are laundered on read
    /// (`DashboardTile`'s clamps), not here.
    fn payload(&self, width: f64, keep: Option<&BTreeSet<String>>, look: HostLook) -> Value {
        let cards = self
            .cards
            .iter()
            .filter(|card| {
                keep.is_none_or(|keep| keep.contains(card["id"].as_str().unwrap_or_default()))
            })
            .cloned()
            .collect();
        let mut payload = cockpit_payload(CockpitInput {
            cards,
            remote_count: self.configured_remotes,
            available: width,
            look,
        });
        apply_machine_preferences(&mut payload, &self.hidden, &self.alerts);
        payload
    }
}

/// Builds the cards and reads the settings for one cockpit frame.
fn cockpit_frame(state: &App) -> CockpitFrame {
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
    let local = state.local.lock().expect("local state poisoned").card();
    // Re-read every frame rather than captured at startup, for the same reason
    // the GitHub loop re-reads its token: that is what makes a Settings edit
    // apply without a relaunch. The store lock is taken after the poll set's is
    // released, matching `poll_containers`'s sequence, and is held for one read.
    let (alerts, hidden) = {
        let store = state.store.lock().expect("store poisoned");
        (
            store.settings().machine_alerts.clone(),
            volumes::Hidden::from_store(&store),
        )
    };
    CockpitFrame {
        cards: cockpit_cards(Some(local), &hosts),
        configured_remotes: hosts.len(),
        alerts,
        hidden,
    }
}

/// [`CockpitFrame::payload`] over a fresh [`cockpit_frame`].
fn cockpit_with(width: f64, state: &App, keep: Option<&BTreeSet<String>>) -> Value {
    cockpit_frame(state).payload(width, keep, HostLook::default())
}

/// The operator's per-machine choices, applied to the cockpit payload before
/// the Hosts panel or the dashboard reads a card. One function rather than two
/// calls in `cockpit`, so the test below exercises exactly what ships: drop it,
/// and a full disk the operator hid would colour the strip and raise attention
/// again.
fn apply_machine_preferences(
    payload: &mut Value,
    hidden: &volumes::Hidden,
    alerts: &store::machine_alerts::MachineAlerts,
) {
    hide_volumes(payload, hidden);
    machine_alerts::apply(payload, alerts);
}

// MARK: the Usage + Azure Cost panels
//
// Two loops rather than one because they share nothing: different credentials,
// different sources, and cadences an order of magnitude apart (an hourly-by-default API
// read vs a daily blob export). Both follow `github_loop`'s shape — poll,
// re-read the interval, sleep interruptibly — so a Settings edit applies now
// rather than on the far side of a cadence that can be four hours long.

/// One walk of Claude Code's logs for the Usage panel's token rollups.
///
/// Stamped when the walk returns, not before it starts, like every provider
/// below: a reading is as old as the moment it was taken.
async fn poll_claude_rollups(app: &Arc<App>) {
    // Blocking: this walks a directory tree that can hold ~1600 files.
    let claude = tokio::task::spawn_blocking(read_claude_usage)
        .await
        .map_err(|e| eprintln!("Claude usage walk failed: {e}"))
        .ok();
    if let Some((summary, error)) = claude {
        apply_claude_pass(
            &app.usage,
            &app.services_wake,
            summary,
            panel::now_unix(),
            error,
        );
    }
}

/// One pass over the Usage panel's metered providers: Neon, Sentry and Vercel.
///
/// Each reading is stamped with `panel::now_unix()` as its own read completes.
/// There is deliberately no pass-start time in scope here: a stamp taken
/// before the other providers' requests ages the reading by their duration,
/// which is how Sentry's footer went stale between healthy polls (#635).
async fn poll_usage_providers(app: &Arc<App>) {
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
                        panel::now_unix(),
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
                Ok(summary) => state
                    .neon_invoice_mut()
                    .succeeded(summary, panel::now_unix(), None),
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
                    panel::now_unix(),
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
                        panel::now_unix(),
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
/// The existence check is the *shell's*, exactly as it is in the original: the walk
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

/// The usage loop's two clocks: each half's cadence, and how long ago that
/// half last finished (`None` before its first run).
///
/// Pure, so the decision [`usage_loop`] makes every turn — what is due, and
/// how long to sleep before something is — can be driven through many cycles
/// in a test with passes that take real time.
#[derive(Debug, Clone, Copy)]
struct UsageClocks {
    rollups_every: std::time::Duration,
    since_rollups: Option<std::time::Duration>,
    providers_every: std::time::Duration,
    since_providers: Option<std::time::Duration>,
}

/// What one turn of [`usage_loop`] runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UsageTurn {
    rollups: bool,
    providers: bool,
}

impl UsageClocks {
    /// Each half runs when it never has, when its cadence has elapsed since it
    /// last finished, or when a wake forced it ([`UsageWake`]). Neither half
    /// rides on the other's turn: the Claude walk re-reads every session log,
    /// so running it on the providers' clock would spend that cost more often
    /// than the operator set (#635).
    fn turn(self, force_rollups: bool, force_providers: bool) -> UsageTurn {
        let due = |every: std::time::Duration, since: Option<std::time::Duration>| {
            since.is_none_or(|since| since >= every)
        };
        UsageTurn {
            rollups: force_rollups || due(self.rollups_every, self.since_rollups),
            providers: force_providers || due(self.providers_every, self.since_providers),
        }
    }

    /// How long the loop may sleep before either half falls due: the shorter
    /// of the two remainders, and nothing at all once one is overdue.
    fn sleep(self) -> std::time::Duration {
        let remaining = |every: std::time::Duration, since: Option<std::time::Duration>| {
            since.map_or(std::time::Duration::ZERO, |since| {
                every.saturating_sub(since)
            })
        };
        remaining(self.rollups_every, self.since_rollups)
            .min(remaining(self.providers_every, self.since_providers))
    }
}

/// The Usage panel's poll loop: one loop, two halves, each on **its own**
/// cadence (#635).
///
/// The Claude rollups are a local walk of `~/.claude/projects` on
/// [`PanelInterval::ClaudeRollups`]. The metered providers (Neon, Sentry,
/// Vercel) are three vendor APIs on a rate-limit budget, on
/// [`PanelInterval::UsageProviders`]. Each turn runs only the halves that are
/// due ([`UsageClocks::turn`]) and then sleeps until the next one is
/// ([`UsageClocks::sleep`]), so neither cadence is rounded to the other's.
/// Each half's clock is taken when it *finishes*, so a gap is never shorter
/// than its cadence.
///
/// Both cadences are re-read every turn, so either can be changed without
/// waiting out the old value. See [`panel_interval`].
async fn usage_loop(app: Arc<App>) {
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    let mut rollups_last = None::<Instant>;
    let mut providers_last = None::<Instant>;
    let clocks = |rollups_last: Option<Instant>, providers_last: Option<Instant>| UsageClocks {
        rollups_every: panel_interval(&app.store, PanelInterval::ClaudeRollups),
        since_rollups: rollups_last.map(|at| at.elapsed()),
        providers_every: panel_interval(&app.store, PanelInterval::UsageProviders),
        since_providers: providers_last.map(|at| at.elapsed()),
    };
    loop {
        // Both flags are taken every turn, never inside a short-circuit: a
        // force left set would fire on some later turn nobody asked for.
        let force_rollups = app.usage_rollups_due.swap(false, Ordering::SeqCst);
        let force_providers = app.usage_providers_due.swap(false, Ordering::SeqCst);
        let turn = clocks(rollups_last, providers_last).turn(force_rollups, force_providers);
        // Told to the state before the pass, so the rollups' footer judges
        // staleness against the cadence this pass runs on.
        sync_app_cadences(&app);
        if turn.rollups {
            poll_claude_rollups(&app).await;
            rollups_last = Some(Instant::now());
        }
        if turn.providers {
            poll_usage_providers(&app).await;
            providers_last = Some(Instant::now());
        }

        let secs = clocks(rollups_last, providers_last).sleep();
        tokio::select! {
            () = tokio::time::sleep(secs) => {}
            () = app.usage_wake.notified() => {}
        }
    }
}

// MARK: the Usage panel's plan rows
//
// Their own loop rather than a fourth provider inside `poll_usage_providers`:
// they run on their own cadence, and they are the only reads in the app that
// borrow another program's sign-in. Keeping them apart keeps the opt-in auditable in
// one place (`plan_usage::poll`).

/// The two plan opt-ins and their cadence, as the store holds them now.
fn plan_inputs(store: &Mutex<Store>) -> plan_usage::PlanInputs {
    assert_store_not_held(store);
    let (claude, codex) = {
        let store = store.lock().expect("store poisoned");
        let settings = store.settings();
        (
            settings.claude_plan_usage_enabled,
            settings.codex_plan_usage_enabled,
        )
    };
    plan_usage::PlanInputs {
        claude,
        codex,
        cadence_secs: panel_interval(store, PanelInterval::PlanUsage).as_secs(),
    }
}

/// The plan rows' poll loop.
///
/// Every pass lets [`plan_usage::poll`] decide per provider, and each
/// provider's opt-in is read from the store **at its own turn**, never once for
/// the pass: Claude's reader runs first and may wait on a macOS keychain prompt
/// for up to a minute (`subscription::keychain::DEFAULT_TIMEOUT`), and Codex
/// turned off during that wait must not be read after it. A provider that is
/// off is never read; nothing here reads a sign-in on its own. The credential
/// readers run on the blocking pool, and the token they return goes straight
/// into the one request and is dropped with it.
async fn plan_usage_loop(app: Arc<App>) {
    loop {
        let cadence_secs = panel_interval(&app.store, PanelInterval::PlanUsage).as_secs();
        plan_usage::poll(
            &app.usage,
            subscription::Provider::Claude,
            || plan_inputs(&app.store).claude,
            panel::now_unix(),
            cadence_secs,
            || async {
                tokio::task::spawn_blocking(subscription::claude::read_this_machine)
                    .await
                    .unwrap_or(Err(subscription::PlanErrorKind::CredentialUnreadable))
            },
            |token| async move {
                subscription::claude::Client::default()
                    .usage(&token, github::now_utc())
                    .await
            },
        )
        .await;
        plan_usage::poll(
            &app.usage,
            subscription::Provider::Codex,
            || plan_inputs(&app.store).codex,
            panel::now_unix(),
            cadence_secs,
            || async {
                tokio::task::spawn_blocking(subscription::codex::read_this_machine)
                    .await
                    .unwrap_or(Err(subscription::PlanErrorKind::CredentialUnreadable))
            },
            |login| async move {
                subscription::codex::Client::default()
                    .usage(&login, github::now_utc())
                    .await
            },
        )
        .await;

        let inputs = plan_inputs(&app.store);
        let wait = {
            let usage = app.usage.lock().expect("usage state poisoned");
            plan_usage::next_wake(
                [
                    (inputs.claude, usage.plan(subscription::Provider::Claude)),
                    (inputs.codex, usage.plan(subscription::Provider::Codex)),
                ],
                panel::now_unix(),
                inputs.cadence_secs,
            )
        };
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = app.plan_usage_wake.notified() => {}
        }
    }
}

// MARK: the Sentry Crons panel
//
// Its own loop rather than a fourth provider inside `poll_usage_providers`: it
// is a separate panel with a separate lock, and folding it in would make one
// keychain read and one HTTP failure shared between two cards that answer
// different questions. Its default cadence is the Sentry read's, from the same
// constant; each is the operator's to change.

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
/// network was not up yet at login, and nothing guarantees a `crons_wake`
/// before the first read succeeds — so its failure would otherwise sit on the
/// cockpit, red, for an hour.
const CRONS_FIRST_READ_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

/// The Sentry Crons panel's poll loop, on the operator's cadence for this panel
/// once it has something to show, and a short retry until then.
async fn crons_loop(app: Arc<App>) {
    loop {
        poll_crons(&app).await;
        let settled = app
            .crons
            .lock()
            .expect("crons state poisoned")
            .has_succeeded();
        let wait = if settled {
            panel_interval(&app.store, PanelInterval::Crons)
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
fn crons(tile: Option<String>, state: tauri::State<'_, Arc<App>>) -> Result<Value, String> {
    scope_to_tile(&state, "sentryCrons", tile.as_deref(), || {
        crons_unscoped(&state)
    })
}

/// [`crons`] without a tile: the whole source, as the dashboard reads it.
fn crons_unscoped(state: &App) -> Value {
    // The operator's cadence for this panel, read at render time like the Azure
    // panel's and for the same reason: it is what the reading's age is measured
    // against, so a shortened interval must re-date the card now rather than an
    // hour from now.
    let cadence_secs = panel_interval(&state.store, PanelInterval::Crons).as_secs();
    let payload = {
        let panel = state.crons.lock().expect("crons state poisoned");
        crons::view(&panel, cadence_secs, panel::now_unix())
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

/// Which halves of the usage loop a [`wake_usage`] forces onto its next pass.
///
/// A cadence edit forces neither and goes through [`wake_panel`] instead: the
/// loop re-reads both cadences and works out for itself what is due.
#[derive(Debug, Clone, Copy)]
enum UsageWake {
    /// The metered providers only — a newly-saved Neon key must fill its
    /// section now, and the Claude walk has nothing new to learn from it.
    Providers,
    /// Both halves: resume from sleep refreshes every source.
    Everything,
}

/// Cuts the usage loop's sleep short, forcing the halves `force` names to run
/// on that pass.
fn wake_usage(app: &App, force: UsageWake) {
    use std::sync::atomic::Ordering;
    app.usage_providers_due.store(true, Ordering::SeqCst);
    if matches!(force, UsageWake::Everything) {
        app.usage_rollups_due.store(true, Ordering::SeqCst);
    }
    app.usage_wake.notify_one();
}

/// One Azure cost read.
///
/// The SAS URL *is* the credential, so it never leaves this function: it goes
/// straight into the fetcher, whose `Debug` redacts it and whose errors are
/// stripped of their URL before they become strings.
async fn poll_azure(app: &Arc<App>) {
    // Re-read every pass, like the GitHub token and org: a Save in Settings
    // must apply on the next poll rather than the next launch.
    let (account, container) = {
        let store = app.store.lock().expect("store poisoned");
        let settings = store.settings();
        (
            settings.azure_storage_account.trim().to_owned(),
            settings.azure_cost_container.trim().to_owned(),
        )
    };
    if account.is_empty() || container.is_empty() {
        app.azure
            .lock()
            .expect("azure state poisoned")
            .unconfigure();
        return;
    }

    let previous = {
        let mut state = app.azure.lock().expect("azure state poisoned");
        state.begin();
        state.cached()
    };

    // Minted per poll rather than cached. `az` costs a second or two against a
    // four-hour cadence, and a cache would need an expiry clock, an
    // invalidation path for a settings change, and an answer for a token that
    // expires mid-fetch — none of which buys anything at this rhythm.
    //
    // Blocking (it spawns a process), so it goes off the executor exactly as
    // `docker ps` does.
    let now = github::now_utc();
    let minted = {
        let (account, container) = (account.clone(), container.clone());
        tokio::task::spawn_blocking(move || azure::sas::mint(&account, &container, now)).await
    };
    let sas = match minted {
        Ok(Ok(url)) => url,
        // `failed`, never `unconfigure`: the account and container are set, so
        // painting "add an Azure storage account in Settings" would send the
        // operator to fix something that is already right — and it would throw
        // away the fingerprint cache, making the next good read re-download
        // every partition.
        Ok(Err(e)) => {
            app.azure
                .lock()
                .expect("azure state poisoned")
                .failed(e.user_message());
            return;
        }
        Err(e) => {
            // The spawn_blocking task itself died — a panic or a shutdown.
            // Logged without the account name, and reported as the failure it
            // is rather than as a misconfiguration.
            eprintln!("azure sas mint task failed: {e}");
            app.azure
                .lock()
                .expect("azure state poisoned")
                .failed("could not mint an Azure SAS".to_owned());
            return;
        }
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

/// The Azure Cost panel's poll loop, on the operator's cadence for this panel
/// once it has something to show, and a short retry until then.
async fn azure_loop(app: Arc<App>) {
    loop {
        poll_azure(&app).await;
        let settled = app
            .azure
            .lock()
            .expect("azure state poisoned")
            .has_succeeded();
        let wait = if settled {
            panel_interval(&app.store, PanelInterval::AzureCost)
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
fn usage(tile: Option<String>, state: tauri::State<'_, Arc<App>>) -> Result<Value, String> {
    scope_to_tile(&state, "claudeUsage", tile.as_deref(), || {
        usage_unscoped(&state)
    })
}

/// [`usage`] without a tile: the whole source, as the dashboard reads it.
fn usage_unscoped(state: &App) -> Value {
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
    // The metered providers' cadence, read at render time for the same reason:
    // it is what each section's freshness line is measured against, and a
    // shortened interval must re-date those sections now.
    let cadence_secs = panel_interval(&state.store, PanelInterval::UsageProviders).as_secs();
    // The plan opt-ins, read here rather than by the poller: switching one off
    // must hide its rows on this frame, not after the next pass drops them.
    let plans = plan_inputs(&state.store);
    let payload = {
        let panel = state.usage.lock().expect("usage state poisoned");
        usage::view(&panel, quota, rates, plans, cadence_secs, panel::now_unix())
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
fn services(tile: Option<String>, state: tauri::State<'_, Arc<App>>) -> Result<Value, String> {
    scope_to_tile(&state, "services", tile.as_deref(), || {
        services_unscoped(&state)
    })
}

/// [`services`] without a tile: the whole source, as the dashboard reads it.
fn services_unscoped(state: &App) -> Value {
    let payload = {
        let statuses = state.services.lock().expect("services poisoned");
        services::view(&statuses)
    };
    FIRST_SERVICES_REQUEST.call_once(|| {
        // The row count leads, because `trailing` is deliberately empty when
        // there are none: a smoke line reading `services: first frontend
        // request ()` would say nothing about whether the boundary carried the
        // call, which is the only thing it exists to establish.
        eprintln!(
            "services: first frontend request ({} vendor(s){})",
            payload["rows"].as_array().map_or(0, Vec::len),
            match payload["trailing"].as_str().unwrap_or_default() {
                "" => String::new(),
                trailing => format!(", {trailing}"),
            }
        );
    });
    payload
}

/// Guards the Azure Cost panel's counterpart to [`FIRST_REQUEST`].
static FIRST_AZURE_REQUEST: std::sync::Once = std::sync::Once::new();

#[tauri::command]
fn azure_cost(tile: Option<String>, state: tauri::State<'_, Arc<App>>) -> Result<Value, String> {
    scope_to_tile(&state, "azureCost", tile.as_deref(), || {
        azure_cost_unscoped(&state)
    })
}

/// [`azure_cost`] without a tile: the whole source, as the dashboard reads it.
fn azure_cost_unscoped(state: &App) -> Value {
    let budget = {
        let store = state.store.lock().expect("store poisoned");
        store.settings().azure_monthly_budget_usd
    };
    // The operator's cadence for this panel, read at render time like the budget
    // above and for the same reason: it is what both staleness judgements are
    // measured against, and a shortened interval must re-date the card now
    // rather than on the next four-hour cycle.
    let cadence_secs = panel_interval(&state.store, PanelInterval::AzureCost).as_secs();
    let payload = {
        let panel = state.azure.lock().expect("azure state poisoned");
        azure::view(&panel, budget, cadence_secs, panel::now_unix())
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
                // The gateway's `version` is a WIRE field with a required
                // string, so an underivable version becomes the literal
                // "unknown" rather than an omitted key. Making it optional is a
                // protocol change, and this repo lands wire changes on their
                // own — a UI version fix is not the place. "unknown" is at
                // least an honest claim; `0.1.0`, which this sent until now,
                // was not.
                settings::VERSION.unwrap_or("unknown"),
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
fn openclaw(tile: Option<String>, state: tauri::State<'_, Arc<App>>) -> Result<Value, String> {
    scope_to_tile(&state, "openclawAgents", tile.as_deref(), || {
        openclaw_unscoped(&state)
    })
}

/// [`openclaw`] without a tile: the whole source, as the dashboard reads it.
fn openclaw_unscoped(state: &App) -> Value {
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
fn settings_view(
    route: Option<settings::ConnectionRoute>,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    let mut payload = settings_payload(&state);
    if let Some(route) = route {
        let store = state.store.lock().expect("store poisoned");
        payload["connectionRoute"] = settings::connection_route(&route, store.data());
    }
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

/// Saves the shared CPU/RAM limits (`host_id` null) or one machine's override
/// from the Machines tile's Configure, and answers with the re-rendered
/// dashboard. Saved on the click and outside the dashboard's undo history:
/// the limits judge every Machines tile, Detail and the Full view, not the tile
/// being configured. Rendering reads them every second; no poller or history is
/// reset. Null `thresholds` resets the shared defaults or removes the override.
#[tauri::command]
fn machine_save_alerts(
    host_id: Option<String>,
    thresholds: Option<Value>,
    width: f64,
    state: tauri::State<'_, Arc<App>>,
) -> Result<Value, String> {
    machine_alerts::save(
        &mut state.store.lock().expect("store poisoned"),
        host_id.as_deref(),
        thresholds.as_ref(),
    )?;
    Ok(dashboard_view(width, state))
}

#[tauri::command]
fn machine_acknowledge_warning(
    host_id: String,
    metric: store::machine_alerts::MachineMetric,
    acknowledged: bool,
    width: f64,
    state: tauri::State<'_, Arc<App>>,
) -> Result<Value, String> {
    machine_alerts::acknowledge(
        &mut state.store.lock().expect("store poisoned"),
        &host_id,
        metric,
        acknowledged,
    ).map_err(|_| "Could not save the warning acknowledgement. Check that the machine still exists and your settings can be saved, then try again.".to_owned())?;
    Ok(dashboard_view(width, state))
}

/// Hides or unhides one volume on one machine from the dashboard's Detail, and
/// answers with the re-rendered dashboard so the tile, its Disk meter and
/// attention change in the same frame as the table.
#[tauri::command]
fn machine_set_volume_hidden(
    host_id: String,
    mount: String,
    hidden: bool,
    width: f64,
    state: tauri::State<'_, Arc<App>>,
) -> Result<Value, String> {
    volumes::set_hidden(
        &mut state.store.lock().expect("store poisoned"),
        &host_id,
        &mount,
        hidden,
    )
    .map_err(|error| format!("Could not save the volume change: {error}"))?;
    Ok(dashboard_view(width, state))
}

/// Applies one edit to the runner group labels and saves it, from a Runners
/// tile's Configure. Saved on the click and outside the dashboard's undo
/// history: the list is shared by every GitHub Runners tile, Detail and the Full
/// view, not a property of the tile being configured. No poll is woken: the
/// `runners` command reads the list on every request, so this very response
/// is regrouped.
fn edit_runner_group_labels(
    state: &tauri::State<'_, Arc<App>>,
    next: impl FnOnce(&[String]) -> Result<Vec<String>, String>,
) -> Result<(), String> {
    let mut store = state.store.lock().expect("store poisoned");
    let labels = next(store.runner_group_labels())?;
    runner_groups::save(&mut store, labels)
}

#[tauri::command]
fn runner_group_label_add(
    label: String,
    width: f64,
    state: tauri::State<'_, Arc<App>>,
) -> Result<Value, String> {
    edit_runner_group_labels(&state, |listed| {
        let label = runner_groups::validated_label(&label, listed)?;
        Ok(listed.iter().cloned().chain([label]).collect())
    })?;
    Ok(dashboard_view(width, state))
}

#[tauri::command]
fn runner_group_label_remove(
    label: String,
    width: f64,
    state: tauri::State<'_, Arc<App>>,
) -> Result<Value, String> {
    edit_runner_group_labels(&state, |listed| runner_groups::removed(listed, &label))?;
    Ok(dashboard_view(width, state))
}

#[tauri::command]
fn runner_group_label_move(
    label: String,
    up: bool,
    width: f64,
    state: tauri::State<'_, Arc<App>>,
) -> Result<Value, String> {
    edit_runner_group_labels(&state, |listed| runner_groups::moved(listed, &label, up))?;
    Ok(dashboard_view(width, state))
}

/// Cuts short the sleep of the one loop whose cadence just changed.
///
/// Exhaustive over [`PanelInterval`], so a further settable panel cannot be added
/// with a control that writes a cadence nothing acts on until the *old* one
/// elapses — four hours, on the Azure row, which is indistinguishable from the
/// setting doing nothing. The loops re-read the store every pass ([#301]), so
/// the wake is what makes the change apply *now* rather than next tick.
///
/// The usage wake is deliberately **not** forced onto its provider half: the
/// loop recomputes whether a provider pass is due from the cadence it just
/// re-read, so a wake applies the new rhythm without spending three vendor
/// requests to learn nothing new.
///
/// [#301]: https://github.com/Sassy-Dog/solador/issues/301
fn wake_panel(app: &App, panel: PanelInterval) {
    // Every loop's wake is a plain `notify_one`. The usage wake is deliberately
    // not forced onto its provider half: a cadence change spends no vendor
    // request, which is why this does not go through `wake_usage`.
    wake_notify(app, wake_target(panel)).notify_one();
}

/// The `Notify` each target's loop sleeps on. A function of its own so a test
/// can hold each target to its own field: `wake_target` being right does not
/// help if the arm below it wakes a neighbour.
fn wake_notify(app: &App, target: WakeTarget) -> &tokio::sync::Notify {
    match target {
        WakeTarget::Containers => &app.containers_wake,
        WakeTarget::GitHub => &app.github_wake,
        WakeTarget::Services => &app.services_wake,
        WakeTarget::Usage => &app.usage_wake,
        WakeTarget::Azure => &app.azure_wake,
        WakeTarget::Crons => &app.crons_wake,
        WakeTarget::PlanUsage => &app.plan_usage_wake,
    }
}

/// The loops a cadence save can wake: one value per loop, not per cadence,
/// because the two Usage cadences share a loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WakeTarget {
    Containers,
    GitHub,
    Services,
    Usage,
    Azure,
    Crons,
    PlanUsage,
}

/// Which loop a cadence's save wakes. Pure so a test can pin it: the match in
/// [`wake_panel`] being exhaustive proves every cadence is *handled*, not that
/// `Services` does not wake the GitHub loop.
fn wake_target(panel: PanelInterval) -> WakeTarget {
    match panel {
        PanelInterval::Containers => WakeTarget::Containers,
        PanelInterval::GitHub => WakeTarget::GitHub,
        PanelInterval::Services => WakeTarget::Services,
        PanelInterval::ClaudeRollups | PanelInterval::UsageProviders => WakeTarget::Usage,
        PanelInterval::AzureCost => WakeTarget::Azure,
        PanelInterval::Crons => WakeTarget::Crons,
        PanelInterval::PlanUsage => WakeTarget::PlanUsage,
    }
}

/// Folds one rollup walk into the usage state and wakes the Services loop if
/// that changed whether Claude rollups exist.
///
/// Anthropic's row in Services is evidence-driven by exactly this, and nothing
/// edited by hand changes it, so nothing else would wake the Services loop for
/// it: the row would appear (or go) a whole Services cadence late. Only on a
/// change -- a wake per usage pass would make the cadence setting a lie.
/// Presence is read **before** the walk is applied: reading it after would
/// compare the new state with itself.
fn apply_claude_pass(
    usage: &Mutex<UsageState>,
    services_wake: &tokio::sync::Notify,
    summary: Option<usage::UsageSummary>,
    at: u64,
    error: Option<String>,
) -> bool {
    let (was_present, is_present) = {
        let mut usage = usage.lock().expect("usage state poisoned");
        let was_present = usage.claude_present();
        usage.apply_claude(summary, at, error);
        (was_present, usage.claude_present())
    };
    let flipped = claude_presence_wakes_services(was_present, is_present);
    if flipped {
        services_wake.notify_one();
    }
    flipped
}

/// Whether a usage pass should wake the Services loop: only when Claude
/// rollups appeared or disappeared (`services::active_vendors`' fifth input).
fn claude_presence_wakes_services(was_present: bool, is_present: bool) -> bool {
    was_present != is_present
}

/// Hands the staleness windows cached in panel state (GitHub, Claude rollups,
/// Services) the cadence the store holds *now*.
///
/// Every other cadence-following footer reads its cadence at render time and
/// needs no sync: Containers (`containers_view_in`, #633), Azure Cost
/// (cadence + 1h), the plan limits (twice the cadence), and, since #635, Sentry
/// Crons (`crons_unscoped`) and the metered Usage providers (`usage_unscoped`),
/// whose window is 1.5 cycles of each panel's own cadence.
///
/// The loops call it at the top of each pass, and `panel_interval_save` /
/// `_clear` call it before they re-render, so a shortened cadence does not
/// leave the previous pass's window on screen (a reading fresh under 300s read
/// stale under the new 30s, and Fleet Down `Unknown`, until the woken pass got
/// round to it). The render-time readers above are not set here.
fn sync_cadences(
    store: &Mutex<Store>,
    github: &Mutex<GitHubState>,
    usage: &Mutex<UsageState>,
    services: &Mutex<services::ServiceStatuses>,
) {
    let secs = |panel| panel_interval(store, panel).as_secs();
    github
        .lock()
        .expect("github state poisoned")
        .set_cadence_secs(secs(PanelInterval::GitHub));
    usage
        .lock()
        .expect("usage state poisoned")
        .set_claude_cadence_secs(secs(PanelInterval::ClaudeRollups));
    services
        .lock()
        .expect("services poisoned")
        .set_cadence_secs(secs(PanelInterval::Services));
}

fn sync_app_cadences(app: &App) {
    sync_cadences(&app.store, &app.github, &app.usage, &app.services);
}

/// One panel's poll cadence, from the Configure of a tile of that source.
///
/// **Refused, not clamped.** A value under the panel's floor writes nothing and
/// comes back as an error carrying [`store::settings::IntervalRejection::user_message`]
/// — which names the panel, its floor, why that floor exists and what was asked
/// for. Storing the floor instead would leave an operator looking at a number
/// they did not choose, on a setting whose whole point is that the cadence is
/// theirs.
///
/// The floor is enforced here rather than in the webview, and again on read in
/// [`store::settings::Settings::panel_interval_secs`], because on the Azure
/// panel a bypassed floor is a spend decision. The `min` the control carries is
/// a courtesy to the number input, not the gate.
///
/// Saved on the click and outside the dashboard's undo history (#614): the
/// cadence is the source's, shared by every tile of it and the Full view.
#[tauri::command]
fn panel_interval_save(
    panel: String,
    secs: u32,
    width: f64,
    state: tauri::State<'_, Arc<App>>,
) -> Result<Value, String> {
    panel_interval_save_in(&state, &panel, secs)?;
    Ok(dashboard_view(width, state))
}

/// The command's body, over a plain `&App` so a test can drive it.
fn panel_interval_save_in(app: &App, panel: &str, secs: u32) -> Result<(), String> {
    let kind = PanelInterval::parse(panel).ok_or("Skipped \u{2014} unknown panel.")?;
    let secs = kind.check_secs(secs).map_err(|r| r.user_message())?;
    {
        let mut store = app.store.lock().expect("store poisoned");
        let previous = store.settings().panel_intervals.clone();
        let outcome = store.settings_mut().set_panel_interval_secs(kind, secs);
        debug_assert!(
            !outcome.was_clamped(),
            "a value that passed check_secs must be stored as asked"
        );
        if let Err(error) = store.save() {
            store.settings_mut().panel_intervals = previous;
            return Err(format!("Could not save the poll cadence: {error}"));
        }
    }
    // Before the re-render below, so its footers judge staleness by the saved
    // cadence and not the previous pass's.
    sync_app_cadences(app);
    wake_panel(app, kind);
    Ok(())
}

/// Forget one panel's cadence override.
///
/// Not the same as setting it *to* the default: this restores "never
/// configured", so the panel follows its constant if that ever moves. The
/// control's status line is what makes the two distinguishable on screen.
#[tauri::command]
fn panel_interval_clear(
    panel: String,
    width: f64,
    state: tauri::State<'_, Arc<App>>,
) -> Result<Value, String> {
    panel_interval_clear_in(&state, &panel)?;
    Ok(dashboard_view(width, state))
}

/// The command's body, over a plain `&App` so a test can drive it.
fn panel_interval_clear_in(app: &App, panel: &str) -> Result<(), String> {
    let kind = PanelInterval::parse(panel).ok_or("Skipped \u{2014} unknown panel.")?;
    {
        let mut store = app.store.lock().expect("store poisoned");
        let previous = store.settings().panel_intervals.clone();
        store.settings_mut().clear_panel_interval(kind);
        if let Err(error) = store.save() {
            store.settings_mut().panel_intervals = previous;
            return Err(format!("Could not save the poll cadence: {error}"));
        }
    }
    sync_app_cadences(app);
    wake_panel(app, kind);
    Ok(())
}

/// The crash-reporting opt-in. Its own command, and it saves immediately.
///
/// **Off is applied before the save and regardless of it.** Withdrawing consent
/// is the one instruction here that must not depend on a disk write succeeding;
/// a full disk is not a reason to keep reporting. Opting *in* is the other way
/// round — the consent is recorded only if it persisted, so a failed save
/// cannot leave this session reporting on the strength of a preference the next
/// launch will not find.
///
/// Turning it on still needs a relaunch to do anything, because this only
/// records consent and never starts a client: that is resolved once, at
/// startup. A reporting that started off created none and touched no hub (its
/// `start_with_transport` returns before `sentry::init`), so calling `init`
/// here would make this thread the process hub (a synchronous command runs on
/// the main thread), and every thread that touches a hub afterwards would
/// inherit its client. The Settings payload says so out loud rather than
/// implying the toggle did more than it did — see
/// `crashreport::Reporting::needs_restart`.
#[tauri::command]
fn settings_set_crash_reporting(enabled: bool, state: tauri::State<'_, Arc<App>>) -> Value {
    if !enabled {
        state.crash_reporting.set_consent(false);
    }
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        store.settings_mut().crash_reporting_enabled = enabled;
        save_status(
            &store,
            if enabled {
                "Crash reporting on. It starts with the next launch."
            } else {
                "Crash reporting off."
            },
        )
    };
    let persisted = status.as_deref().is_some_and(|s| !s.starts_with("Failed"));
    if enabled && persisted {
        state.crash_reporting.set_consent(true);
    }
    settings_response(&state, status)
}

/// One plan opt-in (`provider` is `"claude"` or `"codex"`). Saves on the
/// spot, like the crash-reporting toggle, and for the same reason: consent is
/// not a draft.
///
/// **Off is applied before the save and regardless of it.** The loop reads the
/// in-memory store, so a provider switched off is never read again in this
/// session even if the disk write fails. On is kept only if it persisted, so a
/// failed save cannot leave this session reading a sign-in on the strength of a
/// preference the next launch will not find.
///
/// Either direction resets the provider's in-memory state: off drops its
/// reading, and on starts clean, which is also what un-parks a provider the
/// keychain refused.
#[tauri::command]
fn settings_set_plan_usage(
    provider: String,
    enabled: bool,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    let status = apply_plan_usage(&state.store, &state.usage, &provider, enabled);
    state.plan_usage_wake.notify_one();
    settings_response(&state, status)
}

/// The store-mutate-and-reset step of [`settings_set_plan_usage`], apart from
/// the Tauri state so tests can drive it. The two locks are taken one after the
/// other, never nested.
fn apply_plan_usage(
    store: &Mutex<Store>,
    usage: &Mutex<UsageState>,
    provider: &str,
    enabled: bool,
) -> Option<String> {
    let provider = match provider {
        "claude" => subscription::Provider::Claude,
        "codex" => subscription::Provider::Codex,
        _ => return Some("Skipped — unknown provider.".into()),
    };
    let set = |store: &mut Store, on: bool| {
        let settings = store.settings_mut();
        match provider {
            subscription::Provider::Claude => settings.claude_plan_usage_enabled = on,
            subscription::Provider::Codex => settings.codex_plan_usage_enabled = on,
        }
    };
    let name = provider.holder();
    let status = {
        let mut store = store.lock().expect("store poisoned");
        set(&mut store, enabled);
        let status = save_status(
            &store,
            if enabled {
                format!("Reading {name}'s sign-in for plan limits.")
            } else {
                format!("Stopped reading {name}'s sign-in.")
            },
        );
        let persisted = status.as_deref().is_some_and(|s| !s.starts_with("Failed"));
        if enabled && !persisted {
            set(&mut store, false);
        }
        status
    };
    usage
        .lock()
        .expect("usage state poisoned")
        .plan_mut(provider)
        .reset();
    status
}

/// Where the Azure cost export lives.
///
/// Its own command for the same reason as the GitHub org: `ProviderPrefs` is
/// sent whole by two tabs, so a third sender would have to re-send fields it
/// does not show or blank them.
#[tauri::command]
fn settings_save_azure(
    account: String,
    container: String,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        let current = store.settings_mut();
        current.azure_storage_account = account.trim().to_owned();
        current.azure_cost_container = container.trim().to_owned();
        save_status(&store, "Saved.")
    };
    // Woken, unlike the GitHub org: the Azure poll runs on a four-hour rhythm,
    // so without this an operator who has just fixed a typo waits until
    // teatime to find out whether it worked.
    state.azure_wake.notify_one();
    // And Services: an Azure export being configured is what makes Azure a
    // watched vendor (`services::active_vendors`).
    wake_services(&state);
    settings_response(&state, status)
}

/// One org checked or unchecked on one account's runner-org selection.
///
/// Checkbox semantics — saved on the spot, like the crash toggle: a selection
/// is not a draft. A select lands in `Some(vec![org])` even from `None`, and
/// the last deselect leaves `Some(vec![])` — the deliberate "watch nothing",
/// never a fall back to never-configured. An org another account already
/// watches is refused at this gate ([`settings::validated_org`]) rather than
/// tie-broken at poll time.
#[tauri::command]
fn settings_set_account_org(
    id: String,
    org: String,
    selected: bool,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    account_org_in(&state, &id, &org, selected)
}

/// The body of [`settings_set_account_org`], over a plain `&App` so tests can
/// drive it without a Tauri runtime.
///
/// The store's guard lives only inside [`apply_account_org`], which answers
/// with either a refusal or the saved status. Every `settings_response` call
/// is out here, after the guard has dropped: it locks the store again, and a
/// `std::sync::Mutex` locked twice on one thread deadlocks (#630).
fn account_org_in(app: &App, id: &str, org: &str, selected: bool) -> Value {
    let Ok(id) = Uuid::parse_str(id) else {
        return settings_response(app, Some(UNKNOWN_ACCOUNT_MESSAGE.into()));
    };
    match apply_account_org(app, id, org, selected) {
        Err(refusal) => settings_response(app, Some(refusal)),
        Ok(status) => {
            // The selection is what the runners half of the pass polls, so it
            // has to reach the loop now — an org checked and then watched going
            // nowhere for a full GitHub cadence reads as broken. A refusal
            // changed nothing, so it wakes nothing.
            wake_github(app);
            settings_response(app, status)
        }
    }
}

/// Applies the checkbox under the store's lock. `Err` is a refusal that changed
/// nothing; `Ok` carries the save's status.
fn apply_account_org(
    app: &App,
    id: Uuid,
    org: &str,
    selected: bool,
) -> Result<Option<String>, String> {
    let mut store = app.store.lock().expect("store poisoned");
    let org = if selected {
        settings::validated_org(org, id, store.accounts())?
    } else {
        org.trim().to_owned()
    };
    let Some(account) = store.account_mut(id) else {
        return Err(UNKNOWN_ACCOUNT_MESSAGE.into());
    };
    let orgs = account.orgs.get_or_insert_with(Vec::new);
    if selected {
        if !orgs.iter().any(|watched| watched == &org) {
            orgs.push(org);
        }
    } else {
        orgs.retain(|watched| watched != &org);
    }
    Ok(save_status(&store, "Saved."))
}

#[cfg(test)]
mod account_org_tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    use store::{MemoryCredentialStore, VendorAccount, VendorKind};

    /// An App built by `new_app`, the constructor startup uses, over a temp-dir
    /// store and an in-memory credential store: nothing here reaches the OS
    /// keychain or the operator's `store.json`, and no poll loop is spawned.
    struct Fixture {
        app: Arc<App>,
        _rt: tokio::runtime::Runtime,
        _dir: tempfile::TempDir,
    }

    fn fixture(accounts: Vec<VendorAccount>) -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = Store::open_in(dir.path(), false).expect("open");
        for account in accounts {
            store.upsert_account(account);
        }
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let reporting = start_crash_reporting(&store);
        let app = Arc::new(new_app(
            store,
            MemoryCredentialStore::new(),
            rt.handle().clone(),
            reporting,
        ));
        Fixture {
            app,
            _rt: rt,
            _dir: dir,
        }
    }

    fn account(label: &str, orgs: Option<Vec<&str>>) -> VendorAccount {
        let mut a = VendorAccount::new(VendorKind::GitHub, label, format!("vendor-{label}"));
        a.orgs = orgs.map(|o| o.into_iter().map(str::to_owned).collect());
        a
    }

    /// Runs the command body on its own thread and gives it a few seconds, so a
    /// re-entrant lock fails the test with a message instead of hanging the
    /// suite. (A thread stuck on the lock does not stop libtest from exiting.)
    fn run_bounded(app: &Arc<App>, id: &str, org: &str, selected: bool) -> Value {
        let (tx, rx) = mpsc::channel();
        let (app, id, org) = (Arc::clone(app), id.to_owned(), org.to_owned());
        std::thread::spawn(move || {
            let _ = tx.send(account_org_in(&app, &id, &org, selected));
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the command deadlocked or panicked: the store lock was re-entered")
    }

    fn assert_store_free(app: &App) {
        assert!(
            app.store.try_lock().is_ok(),
            "the store lock is still held after the command returned"
        );
    }

    /// Whether `github_wake` holds a pending wake (consuming it).
    fn woke(f: &Fixture) -> bool {
        futures_util::FutureExt::now_or_never(f.app.github_wake.notified()).is_some()
    }

    fn status(v: &Value) -> &str {
        v["status"].as_str().expect("a status")
    }

    #[test]
    fn an_org_another_account_watches_is_refused_and_wakes_nothing() {
        let (a, b) = (account("a", Some(vec!["acme"])), account("b", None));
        let b_id = b.id.to_string();
        let f = fixture(vec![a, b]);
        let v = run_bounded(&f.app, &b_id, "acme", true);
        assert_eq!(status(&v), "Skipped — acme is already watched by a.");
        assert_store_free(&f.app);
        assert!(!woke(&f), "a refusal must not wake the GitHub loop");
    }

    #[test]
    fn an_empty_org_is_refused_and_wakes_nothing() {
        let a = account("a", None);
        let id = a.id.to_string();
        let f = fixture(vec![a]);
        let v = run_bounded(&f.app, &id, "  ", true);
        assert_eq!(status(&v), "Skipped — an organization name is required.");
        assert_store_free(&f.app);
        assert!(!woke(&f));
    }

    #[test]
    fn an_unknown_account_is_refused_and_wakes_nothing() {
        let f = fixture(vec![]);
        let v = run_bounded(&f.app, &Uuid::new_v4().to_string(), "acme", true);
        assert_eq!(status(&v), UNKNOWN_ACCOUNT_MESSAGE);
        assert_store_free(&f.app);
        assert!(!woke(&f));
        // And an id that is not a UUID at all.
        let v = run_bounded(&f.app, "not-a-uuid", "acme", true);
        assert_eq!(status(&v), UNKNOWN_ACCOUNT_MESSAGE);
        assert_store_free(&f.app);
    }

    #[test]
    fn a_select_and_a_deselect_both_save_and_wake() {
        let a = account("a", None);
        let id = a.id.to_string();
        let f = fixture(vec![a.clone()]);
        let v = run_bounded(&f.app, &id, " acme ", true);
        assert_eq!(status(&v), "Saved.");
        assert_store_free(&f.app);
        assert!(woke(&f));
        let orgs = |f: &Fixture| {
            f.app
                .store
                .lock()
                .unwrap()
                .account(a.id)
                .unwrap()
                .orgs
                .clone()
        };
        assert_eq!(orgs(&f), Some(vec!["acme".to_owned()]));

        let v = run_bounded(&f.app, &id, "acme", false);
        assert_eq!(status(&v), "Saved.");
        assert_store_free(&f.app);
        assert!(woke(&f));
        assert_eq!(orgs(&f), Some(vec![]));
    }

    /// Holds the store on its own thread and calls `call` there. Passing means
    /// the call panicked (the tripwire fired: the sender dropped). Returning
    /// normally or hanging fails, the latter within the bound rather than
    /// hanging the suite.
    fn tripwire_fires(f: &Fixture, what: &str, call: impl FnOnce(&App) + Send + 'static) {
        let (tx, rx) = mpsc::channel::<()>();
        let app = Arc::clone(&f.app);
        std::thread::spawn(move || {
            let _held = app.store.lock().unwrap();
            call(&app);
            let _ = tx.send(());
        });
        match rx.recv_timeout(Duration::from_secs(5)) {
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
            Ok(()) => panic!("tripwire missing: {what} returned with the store held"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("tripwire missing: {what} re-entered the held store without panicking")
            }
        }
    }

    #[test]
    fn answering_while_the_store_is_held_trips_the_wire() {
        let f = fixture(vec![]);
        tripwire_fires(&f, "settings_response", |app| {
            let _ = settings_response(app, None);
        });
    }

    #[test]
    fn panel_interval_while_the_store_is_held_trips_the_wire() {
        let f = fixture(vec![]);
        tripwire_fires(&f, "panel_interval", |app| {
            let _ = panel_interval(&app.store, PanelInterval::GitHub);
        });
    }

    #[test]
    fn plan_inputs_while_the_store_is_held_trips_the_wire() {
        let f = fixture(vec![]);
        tripwire_fires(&f, "plan_inputs", |app| {
            let _ = plan_inputs(&app.store);
        });
    }

    #[test]
    fn reload_hosts_while_the_store_is_held_trips_the_wire() {
        let f = fixture(vec![]);
        tripwire_fires(&f, "reload_hosts", reload_hosts);
    }
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
    wake_usage(&state, UsageWake::Providers);
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
    // The fingerprint of a certificate the operator was shown and **trusted**
    // (#448) — `None` for a host that was never paired, which stays plain HTTP.
    // Only the Trust button's state carries one; the form never sends a typed
    // value, and nothing is pinned by a probe alone.
    tls_fingerprint: Option<String>,
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
    let pin = match tls_fingerprint.as_deref().map(settings::validated_pin) {
        Some(Ok(pin)) => Some(pin),
        Some(Err(reason)) => return settings_response(&state, Some(reason.into())),
        None => None,
    };

    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        match add_host_connection(
            &mut store,
            state.credentials.as_ref(),
            (&name, &address, settings::parse_port(&port)),
            &token,
            pin,
        ) {
            Ok(_) => format!("Added {name}."),
            Err(message) => message,
        }
    };

    reload_hosts(&state);
    settings_response(&state, Some(status))
}

/// A new host row, saved, with its token stored under the id the store minted.
///
/// Shared by Settings' Add Host and the command-line hand-off (`handoff.rs`),
/// so the two cannot disagree about what adding a host writes. A failed file
/// write takes the row back out: the in-memory store must not hold a host the
/// file does not, or the next unrelated save would quietly persist it. A
/// credential-store failure keeps the row — the operator can re-enter the
/// token, and losing the row too would be the worse outcome — but says so.
/// The token is keyed by the host's id and never reaches the store file.
fn add_host_connection(
    store: &mut Store,
    credentials: &dyn CredentialStore,
    (name, address, port): (&str, &str, u16),
    token: &str,
    pin: Option<String>,
) -> Result<Host, String> {
    let (name, address) = (name.trim(), address.trim());
    if name.is_empty() || address.is_empty() {
        return Err("Skipped — name and address are required.".into());
    }
    let mut host = Host::new(name, address);
    host.port = port;
    host.tls_fingerprint = pin;
    let id = host.id;
    store.upsert_host(host.clone());
    if let Err(error) = store.save() {
        store.remove_host(id);
        return Err(format!("Failed: {error}"));
    }
    if !token.is_empty() {
        if let Err(error) = credentials.set_secret(SecretKey::HostToken(id), token) {
            eprintln!("could not store the token for new host {name} ({id}): {error}");
            return Err(format!(
                "Added {name}, but its token could not be stored. Enter it again in Settings."
            ));
        }
    }
    Ok(host)
}

/// Step one of pairing a host (#448): fetch the certificate its agent presents,
/// **without trusting it**, and report the fingerprint.
///
/// Nothing is stored and no token is sent — `agentclient::probe_certificate`
/// makes one unauthenticated request. The answer is a finding, not a `Result`
/// (`settings::host_probe_answer`): "no TLS" and "couldn't reach" are results
/// the form paints, and the operator's decision to trust is a separate
/// command's argument, made only by pressing Trust.
#[tauri::command]
async fn settings_probe_host_certificate(address: String, port: String) -> Value {
    let address = address.trim().to_owned();
    let port = settings::parse_port(&port);
    let outcome = agentclient::probe_certificate(&address, port).await;
    // The form gets one sentence; the transport's detail (DNS, refused,
    // timeout, a TLS alert) goes to the log, where it cannot reach a panel.
    if let Err(error) = &outcome {
        eprintln!("pairing: probe of {address}:{port} found nothing: {error}");
    }
    settings::host_probe_answer(&address, port, &outcome)
}

#[tauri::command]
fn settings_save_host(
    id: String,
    name: String,
    address: String,
    port: String,
    token: String,
    // Absent leaves the host's pin exactly as it is. Present **replaces** it
    // with a fingerprint the operator was just shown and trusted (#448) — the
    // Re-pair path — so a plain rename can never touch the pin, and only the
    // Trust button can change it.
    tls_fingerprint: Option<String>,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    let Ok(id) = Uuid::parse_str(&id) else {
        return settings_response(&state, Some("Skipped — unknown host.".into()));
    };
    let result = {
        let mut store = state.store.lock().expect("store poisoned");
        save_host_connection(
            &mut store,
            state.credentials.as_ref(),
            id,
            (&name, &address, &port),
            &token,
            tls_fingerprint.as_deref(),
        )
    };
    if result == Ok(true) {
        restart_host_task(&state, id);
    }
    reload_hosts(&state);
    settings_response(
        &state,
        Some(result.map_or_else(|message| message, |_| "Saved.".into())),
    )
}

/// Drops host `id`'s poll task, so the next [`reload_hosts`] starts a fresh one.
///
/// Clients retain their token, so a replaced token needs a new client.
/// Replacing it restarts only this host; a rename or another connection's edit
/// keeps every poll and its history.
fn restart_host_task(app: &App, id: Uuid) {
    let mut hosts = app.hosts.lock().expect("poll set poisoned");
    hosts.retain(|host| {
        if host.key.id != id {
            return true;
        }
        host.task.abort();
        false
    });
}

/// A failed file write must retain both the previous host and its credential.
/// A successful ordinary edit never touches the credential store at all.
fn save_host_connection(
    store: &mut Store,
    credentials: &dyn CredentialStore,
    id: Uuid,
    (name, address, port): (&str, &str, &str),
    token: &str,
    pin: Option<&str>,
) -> Result<bool, String> {
    let original = store.host(id).cloned().ok_or("Skipped — unknown host.")?;
    let mut edited = original.clone();
    settings::edit_host(&mut edited, name, address, port)?;
    if let Some(raw) = pin {
        edited.tls_fingerprint = Some(settings::validated_pin(raw)?);
    }
    store.upsert_host(edited);
    if let Err(error) = store.save() {
        store.upsert_host(original);
        return Err(format!("Could not save connection: {error}"));
    }
    if token.is_empty() {
        return Ok(false);
    }
    credentials
        .set_secret(SecretKey::HostToken(id), token)
        .map_err(|error| {
            eprintln!("could not replace the host's token: {error}");
            "Connection saved, but its token could not be replaced. Try again.".to_owned()
        })?;
    Ok(true)
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

// MARK: the container group rules editor
//
// Three commands over one persisted list, and every one of them is a
// read-modify-write of the *whole* list under the store's lock. That is the
// port of the original's re-read-on-access bindings: nothing here trusts a client-side
// copy of the rules, so two edits in flight against the same row cannot clobber
// each other's other fields.
//
// **None of them wakes a loop, on purpose.** The rules are read at *render*
// time by the `containers` command (and by `poll_containers`, for presence), so
// an edit is visible on the panel's very next 10s tick with no fetch involved --
// exactly like the Sentry quota and the Azure budget. A wake here would spend a
// full pass of `docker ps` invocations to change nothing.

/// Applies one edit to the container group rules and saves it, from a
/// Containers tile's Configure. Saved on the click and outside the dashboard's
/// undo history: the list is shared by every Containers tile and the Full view,
/// not a property of the tile being configured (#613). A save that fails puts
/// the previous list back, so memory never claims what disk does not hold.
fn edit_container_rules(
    state: &tauri::State<'_, Arc<App>>,
    edit: impl FnOnce(&mut Vec<ContainerGroupRule>) -> Result<(), String>,
) -> Result<(), String> {
    let mut store = state.store.lock().expect("store poisoned");
    let previous = store.container_rules().to_vec();
    let mut rules = previous.clone();
    edit(&mut rules)?;
    store.set_container_rules(rules);
    if let Err(error) = store.save() {
        store.set_container_rules(previous);
        return Err(format!("Could not save the container rules: {error}"));
    }
    Ok(())
}

#[tauri::command]
fn container_rule_add(width: f64, state: tauri::State<'_, Arc<App>>) -> Result<Value, String> {
    edit_container_rules(&state, |rules| {
        rules.push(settings::new_rule());
        Ok(())
    })?;
    Ok(dashboard_view(width, state))
}

/// One field of one rule.
///
/// Deliberately *not* a whole-row write: the frontend sends the field that
/// changed and nothing else, so the value in every other field of that row is
/// whatever is on disk rather than whatever the last render happened to paint.
#[tauri::command]
fn container_rule_set(
    index: usize,
    field: String,
    value: String,
    width: f64,
    state: tauri::State<'_, Arc<App>>,
) -> Result<Value, String> {
    let field = settings::RuleField::parse(&field)
        .ok_or_else(|| "Skipped \u{2014} unknown rule field.".to_owned())?;
    edit_container_rules(&state, |rules| {
        if settings::apply_rule_edit(rules, index, field, &value) {
            Ok(())
        } else {
            Err("Skipped \u{2014} unknown rule.".to_owned())
        }
    })?;
    Ok(dashboard_view(width, state))
}

#[tauri::command]
fn container_rule_remove(
    index: usize,
    width: f64,
    state: tauri::State<'_, Arc<App>>,
) -> Result<Value, String> {
    edit_container_rules(&state, |rules| {
        if index >= rules.len() {
            return Err("Skipped \u{2014} unknown rule.".to_owned());
        }
        rules.remove(index);
        // An emptied list is respected as an emptied list, never re-seeded:
        // `Store::set_container_rules` writes `Some(vec![])`, which the loader
        // reads back as "the user cleared every rule" rather than "never
        // configured". Deleting the last rule and relaunching must not bring
        // the seeded three back.
        Ok(())
    })?;
    Ok(dashboard_view(width, state))
}

/// The Test button: one `/v1/health` probe, rendered as the original result line.
#[tauri::command]
async fn settings_test_host(
    id: String,
    state: tauri::State<'_, Arc<App>>,
) -> Result<Value, String> {
    let uuid = Uuid::parse_str(&id).map_err(|_| "unknown host".to_owned())?;
    // Scoped so no lock is alive across the await below — the guard is not
    // `Send`, so this is enforced by the compiler rather than by care.
    let key = {
        let store = state.store.lock().expect("store poisoned");
        store
            .host(uuid)
            .map(|host| HostKey {
                id: host.id,
                base_url: host.base_url(),
                tls_fingerprint: host.tls_fingerprint.clone(),
            })
            .ok_or_else(|| "unknown host".to_owned())?
    };
    let token = state
        .credentials
        .secret(SecretKey::HostToken(uuid))
        .unwrap_or_default()
        .unwrap_or_default();

    let result = agent_client(&key, token).health().await;
    Ok(json!({
        "id": id,
        "result": settings::health_result(&result),
        // So the row can offer **Re-pair** the moment Test finds the
        // certificate changed (#448), without waiting for the next Settings
        // payload to carry the poll set's view of it.
        "certificateChanged": matches!(result, Err(AgentError::CertificateChanged)),
        // Likewise (#449): the moment Test finds an unpaired host refused, its
        // edit form can say why and point at the pairing step.
        "plainRefused": matches!(result, Err(AgentError::PlainHttpRefused(_))),
    }))
}

#[tauri::command]
fn settings_add_repo(
    slug: String,
    account_id: Option<String>,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    // An explicit account (the per-account "add by name" path) is parsed
    // before the lock, exactly as `settings_set_repo_account` parses its.
    let account = match account_id.as_deref().filter(|id| !id.is_empty()) {
        Some(raw) => match Uuid::parse_str(raw) {
            Ok(id) => Some(id),
            Err(_) => return settings_response(&state, Some(UNKNOWN_ACCOUNT_MESSAGE.into())),
        },
        None => None,
    };
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        if account.is_some_and(|id| store.account(id).is_none()) {
            Some(UNKNOWN_ACCOUNT_MESSAGE.to_owned())
        } else {
            match settings::validated_slug(&slug, store.repos()) {
                Some(slug) => {
                    // With an explicit account the attribution is the
                    // operator's own answer. Without one it is deduced only
                    // when forced: exactly one account and there is nothing to
                    // ask; more than one and `new_repo` leaves it unattributed
                    // for the row's picker rather than guessing. The *pass*
                    // never adopts an unattributed repo — that asymmetry is
                    // the whole point, and `plan_github_pass` has its own test
                    // for it.
                    let repo = match account {
                        Some(id) => TrackedRepo {
                            account_id: Some(id),
                            ..TrackedRepo::new(&*slug)
                        },
                        None => settings::new_repo(&slug, store.accounts()),
                    };
                    store.upsert_repo(repo);
                    save_status(&store, format!("Added {slug}."))
                }
                None => Some("Skipped — invalid or already tracked.".to_owned()),
            }
        }
    };
    // The portfolio IS the Repos panel's row set, so an edit that does not
    // reach the loop leaves the panel describing the previous portfolio.
    wake_github(&state);
    settings_response(&state, status)
}

/// One discovery-picker checkbox: track or untrack `slug` for the account.
///
/// The decision is [`settings::tracked_change`]'s, made in one place and
/// tested there: add, adopt (the explicit operator click that legitimises
/// what the pass refuses to guess), remove, refuse-with-the-owner-named, or
/// a harmless no-op. The untrack consequence (configured watched workflows
/// are lost) is warned about by the discovery payload's `untrackPrompt` —
/// the frontend two-steps on it exactly as the account delete does; by the
/// time this command runs, the decision is made.
#[tauri::command]
fn settings_set_repo_tracked(
    id: String,
    slug: String,
    tracked: bool,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    let Ok(id) = Uuid::parse_str(&id) else {
        return settings_response(&state, Some(UNKNOWN_ACCOUNT_MESSAGE.into()));
    };
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        if store.account(id).is_none() {
            Some(UNKNOWN_ACCOUNT_MESSAGE.to_owned())
        } else {
            match settings::tracked_change(&slug, id, tracked, store.repos(), store.accounts()) {
                Ok(settings::TrackChange::Add(repo)) => {
                    let slug = repo.slug.clone();
                    store.upsert_repo(repo);
                    save_status(&store, format!("Added {slug}."))
                }
                Ok(settings::TrackChange::Adopt(slug)) => {
                    if let Some(repo) = store.repo_mut(&slug) {
                        repo.account_id = Some(id);
                    }
                    save_status(&store, "Saved.")
                }
                Ok(settings::TrackChange::Remove(slug)) => match store.remove_repo(&slug) {
                    Some(repo) => save_status(&store, format!("Removed {}.", repo.slug)),
                    None => Some("Skipped — not tracked.".to_owned()),
                },
                Ok(settings::TrackChange::Noop) => save_status(&store, "Saved."),
                Err(status) => Some(status),
            }
        }
    };
    wake_github(&state);
    settings_response(&state, status)
}

/// The discovery probe: everything this account's token can see, merged
/// against the store into checkbox rows — the Services-probe pattern, one
/// surface over. The answer is ephemeral: nothing here persists, so a failed
/// or stale probe costs nothing but the sentence explaining it.
///
/// Unlike the poll loop this reads exactly one credential and fires exactly
/// one walk, only when the operator opens the picker — discovery never rides
/// a cadence.
#[tauri::command]
async fn settings_discover_repos(
    id: String,
    state: tauri::State<'_, Arc<App>>,
) -> Result<Value, String> {
    // `Result` because Tauri requires one of an async command with reference
    // inputs (`settings_test_host` precedent). Every outcome — including a
    // failed probe — is an `Ok` payload: the finding is the product, and the
    // frontend renders `reason` in place rather than handling a rejection.
    let Ok(account_id) = Uuid::parse_str(&id) else {
        return Ok(json!({ "id": id, "reason": UNKNOWN_ACCOUNT_MESSAGE }));
    };
    // Everything the answer needs, cloned out before the await — no lock is
    // ever held across one (`poll_github`'s rule, applied to a command).
    let (account, repos, accounts) = {
        let store = state.store.lock().expect("store poisoned");
        let Some(account) = store.account(account_id).cloned() else {
            return Ok(json!({ "id": id, "reason": UNKNOWN_ACCOUNT_MESSAGE }));
        };
        (account, store.repos().to_vec(), store.accounts().to_vec())
    };
    let token = match account_secret_key(&account) {
        None => return Ok(json!({ "id": id, "reason": ACCOUNT_CREDENTIAL_UNKNOWN_MESSAGE })),
        Some(key) => match read_credential(&*state.credentials, key) {
            Credential::Present(token) => token,
            Credential::Absent => {
                return Ok(json!({ "id": id, "reason": ACCOUNT_TOKEN_MISSING_MESSAGE }))
            }
            Credential::Unreadable => {
                return Ok(json!({ "id": id, "reason": CREDENTIAL_UNREADABLE_MESSAGE }))
            }
        },
    };
    Ok(
        match github::GitHubClient::new(token)
            .list_accessible_repos()
            .await
        {
            Ok(found) => settings::discovery_answer(&account, &found, &repos, &accounts),
            // A failed probe found something, and the finding is the product —
            // worded by the error's own `user_message`, never a bare transport
            // string.
            Err(e) => json!({ "id": id, "reason": e.user_message() }),
        },
    )
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

/// The Portfolio row's "Fetched by" picker: which account polls this repo.
///
/// `None` — the picker's own `Unattributed` option — is a legitimate answer,
/// not a cleared field: it is the state every repo added while more than one
/// account existed starts in, and the Repos panel reports it rather than
/// fetching the repo with somebody else's token.
///
/// An id no account carries is **refused** rather than stored. A written-down
/// id that resolves to nothing is indistinguishable on screen from a
/// deliberate `Unattributed`, so accepting one would turn a stale window into
/// a silent misconfiguration.
#[tauri::command]
fn settings_set_repo_account(
    slug: String,
    account_id: Option<String>,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    // Parsed before the lock: an unparseable id is not a question for the store.
    let account = match account_id.as_deref().filter(|id| !id.is_empty()) {
        Some(raw) => match Uuid::parse_str(raw) {
            Ok(id) => Some(id),
            Err(_) => return settings_response(&state, Some(UNKNOWN_ACCOUNT_MESSAGE.into())),
        },
        None => None,
    };
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        if account.is_some_and(|id| store.account(id).is_none()) {
            Some(UNKNOWN_ACCOUNT_MESSAGE.to_owned())
        } else {
            match store.repo_mut(&slug) {
                Some(repo) => {
                    repo.account_id = account;
                    save_status(&store, "Saved.")
                }
                None => Some("Skipped — not tracked.".to_owned()),
            }
        }
    };
    // Attribution decides which token fetches this repo, so an edit that does
    // not reach the loop leaves the panel a full GitHub cadence behind on
    // whose credential it is using.
    wake_github(&state);
    settings_response(&state, status)
}

// MARK: - vendor accounts

/// What a command answers when the webview names an account this store has no
/// row for — a stale id from a window left open across a removal.
const UNKNOWN_ACCOUNT_MESSAGE: &str = "Skipped — unknown account.";

/// Creates an account, or saves changes to one that already exists.
///
/// One command for both, as `Store::upsert_account` is one entry point for
/// both: an update that arrived as a create would mint a second row on the same
/// credential, which is the cardinality bug this epic exists to remove.
///
/// The vendor is read on the create path only. Repointing an existing account
/// at another vendor would aim a credential minted for one API at a different
/// one — the same refusal `account_secret_key` makes about item names, applied
/// to the account itself.
#[tauri::command]
fn settings_save_account(
    id: Option<String>,
    vendor: String,
    label: String,
    token: String,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    let status = match id.as_deref() {
        Some(id) => update_account(id, &label, &token, &state),
        None => add_account(&vendor, &label, &token, &state),
    };
    // Accounts ARE the GitHub pass's identities, so a save that does not reach
    // the loop leaves the panel fetching with the previous set for a full
    // GitHub cadence. The Services pass derives its watched set from accounts
    // too, so it is woken beside it.
    wake_github(&state);
    wake_services(&state);
    settings_response(&state, status)
}

fn add_account(vendor: &str, label: &str, token: &str, app: &App) -> Option<String> {
    let mut store = app.store.lock().expect("store poisoned");
    let account = match settings::validated_account(vendor, label, store.accounts()) {
        Ok(account) => account,
        Err(reason) => return Some(reason),
    };
    let name = account.label.clone();
    // Its own item, minted with the account and never shared: `validated_account`
    // is what decides that, and this only has to agree with it.
    let key = SecretKey::VendorToken(account.id);
    store.upsert_account(account);
    let status = save_status(&store, format!("Added {name}."));

    // An account with no token yet is a legitimate state -- the row says "No
    // token" and the operator adds one later -- so an empty field is not a
    // failure.
    if token.is_empty() {
        return status;
    }
    match app.credentials.set_secret(key, token) {
        Ok(()) => status,
        // Said out loud rather than logged alone. The row is saved either way
        // (losing it as well would be the worse outcome, as with a host), but
        // an operator who is not told will read the "No token" badge as this
        // window not having refreshed yet.
        Err(e) => {
            eprintln!("could not store the new account's token: {e}");
            Some(format!(
                "Added {name} — but the credential store rejected the token. Save it again."
            ))
        }
    }
}

/// Relabels an account and, when one was typed, replaces its token.
///
/// An empty token field leaves the stored credential alone: it is the field's
/// resting state on a row whose token is already saved, and clearing a
/// credential by leaving a box untouched is not a decision anyone makes on
/// purpose. Removing the account is what deletes it.
fn update_account(raw_id: &str, label: &str, token: &str, app: &App) -> Option<String> {
    let Ok(id) = Uuid::parse_str(raw_id) else {
        return Some(UNKNOWN_ACCOUNT_MESSAGE.to_owned());
    };
    let mut store = app.store.lock().expect("store poisoned");
    let Some(account) = store.account(id) else {
        return Some(UNKNOWN_ACCOUNT_MESSAGE.to_owned());
    };
    let label = label.trim();
    if label.is_empty() {
        return Some("Skipped — a name is required.".to_owned());
    }
    // Against every OTHER account: an account keeping its own name is not a
    // duplicate of itself.
    if store
        .accounts()
        .iter()
        .any(|other| other.id != id && other.label.eq_ignore_ascii_case(label))
    {
        return Some("Skipped — an account with that name already exists.".to_owned());
    }

    // Where this token belongs, decided from what the account *currently*
    // names. A migrated account still points at the v1 `github_access_token`
    // item and keeps doing so, so a re-save lands where the pass will read it.
    let named = account_secret_key(account);
    // ...and an account whose stored item name nothing recognises is repaired
    // here rather than left unusable: it adopts its own `vendor-<uuid>` item,
    // which is exactly what ACCOUNT_CREDENTIAL_UNKNOWN_MESSAGE tells the
    // operator to come and do. Only when a token was actually typed -- moving
    // the name without writing the value would trade an unreadable credential
    // for a missing one.
    let key = named.unwrap_or(SecretKey::VendorToken(id));
    let adopting = named.is_none() && !token.is_empty();

    let row = store.account_mut(id).expect("read a moment ago");
    row.label = label.to_owned();
    if adopting {
        row.secret_account = key.account();
    }
    let status = save_status(&store, "Saved.");

    if token.is_empty() {
        return status;
    }
    match app.credentials.set_secret(key, token) {
        Ok(()) => status,
        Err(e) => {
            eprintln!("could not store the account's token: {e}");
            Some("Failed to save the token — the credential store rejected the write.".to_owned())
        }
    }
}

#[tauri::command]
fn settings_set_account_enabled(
    id: String,
    enabled: bool,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    let Ok(id) = Uuid::parse_str(&id) else {
        return settings_response(&state, Some(UNKNOWN_ACCOUNT_MESSAGE.into()));
    };
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        match store.account_mut(id) {
            Some(account) => {
                account.enabled = enabled;
                save_status(&store, "Saved.")
            }
            None => Some(UNKNOWN_ACCOUNT_MESSAGE.to_owned()),
        }
    };
    wake_github(&state);
    wake_services(&state);
    settings_response(&state, status)
}

/// Removes an account, its credential, and nothing else.
///
/// The repos it fetched are **not** rewritten — they stay tracked and become
/// unattributed, because re-homing them onto whichever account survives would
/// invent an owner (`TrackedRepo::account_id`). What the removal costs is read
/// *before* it happens and reported back: the Accounts row already carries the
/// same list as a confirmation prompt, so this is a receipt rather than a
/// surprise.
#[tauri::command]
fn settings_remove_account(id: String, state: tauri::State<'_, Arc<App>>) -> Value {
    let Ok(id) = Uuid::parse_str(&id) else {
        return settings_response(&state, Some(UNKNOWN_ACCOUNT_MESSAGE.into()));
    };
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        // Read while the attribution still exists: after `remove_account` the
        // rows are indistinguishable from repos that never had an account.
        let orphaned = settings::account_removal_impact(store.repos(), id);
        // The org selection leaves with the account — read it for the receipt
        // before the row is gone.
        let stopped_orgs = store
            .account(id)
            .and_then(|account| account.orgs.clone())
            .unwrap_or_default();
        match store.remove_account(id) {
            Some(account) => {
                // The store deliberately leaves the credential store alone, so
                // deleting the token is this layer's job -- and only the item
                // the account actually named. An unresolvable name is left in
                // place rather than guessed at: deleting the wrong item is not
                // undoable, and orphaned is better than someone else's
                // credential deleted.
                match account_secret_key(&account) {
                    Some(key) => {
                        if let Err(e) = state.credentials.delete_secret(key) {
                            eprintln!("could not delete the account's token: {e}");
                        }
                    }
                    None => eprintln!(
                        "left an unrecognised credential item in place: nothing here can name it"
                    ),
                }
                save_status(
                    &store,
                    settings::account_removed_status(&account.label, &orphaned, &stopped_orgs),
                )
            }
            None => Some(UNKNOWN_ACCOUNT_MESSAGE.to_owned()),
        }
    };
    wake_github(&state);
    wake_services(&state);
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

/// Step one of adding a status vendor: ask a host what components it publishes.
///
/// Deliberately **not** a `Result`: a probe that fails has *found something*,
/// and the finding is the product (`ProbeError::user_message`). Reporting it as
/// a rejected command would collapse six distinct corrections — "add https://",
/// "that host is unreachable", "that page is not a Statuspage" — into one
/// undifferentiated failure at the boundary, which is the exact flattening
/// `crates/servicestatus::discover` is built to prevent.
///
/// The transport, the https-only policy and the classification are all that
/// crate's; this wrapper adds no rules of its own.
#[tauri::command]
async fn settings_probe_status_vendor(base_url: String) -> Value {
    let outcome = servicestatus::discover::probe(&base_url).await;
    settings::probe_answer(&base_url, &outcome)
}

/// Step two: store the vendor the operator picked a component from.
///
/// The four fields are re-validated here rather than trusted from the probe —
/// every one of them is still editable after an answer lands.
///
/// Wakes the Services loop, which is where [`poll_service_status`] runs: the
/// watched set is re-derived every pass, so a vendor added here is polled on the
/// next one — and cutting that sleep short is the difference between the row
/// appearing now and appearing up to a full Services cadence later.
#[tauri::command]
fn settings_save_status_vendor(
    label: String,
    base_url: String,
    component_id: String,
    component_label: String,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    let status =
        match settings::validated_vendor(&label, &base_url, &component_id, &component_label) {
            Ok(vendor) => {
                let mut store = state.store.lock().expect("store poisoned");
                let name = vendor.label.clone();
                store.upsert_status_vendor(vendor);
                save_status(&store, format!("Added {name}."))
            }
            Err(reason) => Some(reason),
        };
    wake_services(&state);
    settings_response(&state, status)
}

#[tauri::command]
fn settings_set_status_vendor_enabled(
    id: String,
    enabled: bool,
    state: tauri::State<'_, Arc<App>>,
) -> Value {
    let Ok(id) = Uuid::parse_str(&id) else {
        return settings_response(&state, Some("Skipped — unknown status page.".into()));
    };
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        match store.status_vendor_mut(id) {
            Some(vendor) => {
                vendor.enabled = enabled;
                save_status(&store, "Saved.")
            }
            None => Some("Skipped — unknown status page.".to_owned()),
        }
    };
    wake_services(&state);
    settings_response(&state, status)
}

#[tauri::command]
fn settings_remove_status_vendor(id: String, state: tauri::State<'_, Arc<App>>) -> Value {
    let Ok(id) = Uuid::parse_str(&id) else {
        return settings_response(&state, Some("Skipped — unknown status page.".into()));
    };
    let status = {
        let mut store = state.store.lock().expect("store poisoned");
        match store.remove_status_vendor(id) {
            // No credential to clean up beside it, unlike a host: a Statuspage
            // summary is public, which is the whole reason this tier has no
            // onboarding ceiling.
            Some(vendor) => save_status(&store, format!("Removed {}.", vendor.label)),
            None => Some("Skipped — unknown status page.".to_owned()),
        }
    };
    wake_services(&state);
    settings_response(&state, status)
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
        // Also the Services loop: `active_vendors` derives the Neon and Vercel
        // status pages from exactly these credentials.
        SecretField::Neon | SecretField::Vercel => {
            wake_usage(app, UsageWake::Providers);
            wake_services(app);
        }
        // One credential, two panels: the Usage panel's Sentry section and the
        // Sentry Crons panel are separate loops, so a save has to reach both or
        // one of them keeps describing the previous token for up to an hour.
        SecretField::Sentry => {
            wake_usage(app, UsageWake::Providers);
            app.crons_wake.notify_one();
        }
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
    dump_card_of(host_name, &dump_snapshot(), connection)
}

fn dump_snapshot() -> wire::Snapshot {
    const FIXTURE: &str = include_str!("../../../crates/wire/tests/fixtures/snapshot.json");
    serde_json::from_str(FIXTURE).expect("fixture")
}

fn dump_card_of(host_name: &str, snap: &wire::Snapshot, connection: &Connection) -> Value {
    let mut h = HostHistories::new();
    // A full history buffer is what lets the Playwright suite assert "wider
    // charts show proportionally more samples" without needing hundreds of
    // real polls first — see HISTORY_CAPACITY's doc comment.
    for _ in 0..viewmodel::layout::HISTORY_CAPACITY {
        h.record(snap);
    }
    let mut card = host_card(host_name, snap, &h, connection);
    card["id"] = json!(host_name);
    card
}

/// The stale message the agent client produces for an unreachable host, so a
/// dumped fixture carries the real string rather than a copy of it.
fn unreachable_message() -> String {
    AgentError::Unreachable("connection refused".into()).user_message()
}

/// One live host, the offline fallback `app/ui/app.js` fetches when
/// `window.__TAURI__` is absent.
///
/// Deliberately *without* the local card, unlike [`dump_cockpit`]: this fixture
/// exists to exercise one remote host's live/stale transition, and a second card
/// on the page would only make every locator in that test ambiguous.
/// The same envelope as [`dump_single`], for a host with nothing to plot.
fn dump_pending(pending: &Pending) -> Value {
    let mut card = pending_card("ubu-01", pending);
    card["id"] = json!("ubu-01");
    cockpit_payload(CockpitInput {
        cards: vec![card],
        remote_count: 1,
        available: 1000.0,
        look: HostLook {
            overflow: HostOverflowMode::Stack,
            core_row_span: viewmodel::layout::CORE_ROW_SPAN_DEFAULT,
        },
    })
}

fn dump_single(connection: &Connection) -> Value {
    cockpit_payload(CockpitInput {
        cards: vec![dump_card("ubu-01", connection)],
        remote_count: 1,
        available: 1000.0,
        look: HostLook {
            overflow: HostOverflowMode::Stack,
            core_row_span: viewmodel::layout::CORE_ROW_SPAN_DEFAULT,
        },
    })
}

/// This machine's card as a fixture — hand-made, at a fixed shape, so the file
/// is byte-stable across regenerations and reproduces on any machine.
///
/// The unknowns are real ones the shipped card carries wherever a producer
/// cannot answer: this fixture has no `memory.pressureLevel` (the Windows / no-
/// level case, #544) and no GPU, so both render "—" on every run. Baking them in is what lets the
/// Playwright suite assert the em-dash rule against a payload Rust built rather
/// than one hand-written in JS.
fn dump_local_card() -> Value {
    let snapshot = local_fixture_snapshot();
    let mut histories = HostHistories::new();
    let wired = snapshot.to_wire();
    for _ in 0..viewmodel::layout::HISTORY_CAPACITY {
        histories.record(&wired);
    }
    // The same function the live card goes through, so the fixture cannot
    // diverge from what the app actually paints.
    local::card_from("mac-studio", &snapshot, &histories)
}

/// The sample behind [`dump_local_card`], shared with the showcase card.
fn local_fixture_snapshot() -> localhost::LocalSnapshot {
    localhost::LocalSnapshot {
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
            pressure_level: None,
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
            wire::Process::from_cpu_percent(501, "Xcode".to_string(), 62.0, 4096.0),
            wire::Process::from_cpu_percent(733, "rust-analyzer".to_string(), 18.5, 2048.0),
        ],
    }
}

// MARK: - The showcase fixtures (`--showcase`)
//
// The pictures on solador.app and in the README. The fixtures above exercise
// every failure a panel renders, which is right for a test and wrong for a
// picture of the product: the showcase is a working morning with exactly one
// tile out of true, and `the_showcase_dashboard_has_exactly_one_tile_out_of_true`
// holds it to that.

/// A slow drift and a short jitter, in percentage points, for the showcase
/// histories. Fixed tables rather than a `sin`, so the payload is byte-stable
/// everywhere; coprime lengths (the drift advancing every third step), so their
/// sum does not visibly repeat inside one history buffer.
const SHOWCASE_DRIFT: [f64; 17] = [
    0.0, 1.0, 2.5, 3.5, 4.0, 3.0, 1.5, 0.0, -1.0, -2.0, -3.5, -4.0, -3.0, -2.0, -1.5, -0.5, 0.5,
];
const SHOWCASE_JITTER: [f64; 11] = [0.0, 1.5, -1.0, 2.5, -2.0, 0.5, 3.0, -1.5, 1.0, -0.5, 2.0];

fn showcase_wobble(step: usize, offset: usize) -> f64 {
    SHOWCASE_DRIFT[(step / 3 + offset) % SHOWCASE_DRIFT.len()]
        + SHOWCASE_JITTER[(step + offset) % SHOWCASE_JITTER.len()]
}

/// A history buffer that moves: `latest` recorded last, preceded by the same
/// sample nudged along [`showcase_wobble`] — total and per-core CPU and the
/// I/O rates — so every chart on the card has a shape instead of a flat line.
fn showcase_histories(latest: &wire::Snapshot) -> HostHistories {
    let wobble = |value: f64, step: usize, offset: usize| {
        (value + showcase_wobble(step, offset)).clamp(1.0, 99.0)
    };
    let scale = |value: Option<f64>, step: usize, offset: usize| {
        value.map(|v| v * (1.0 + showcase_wobble(step, offset) / 20.0))
    };
    let mut histories = HostHistories::new();
    let samples = viewmodel::layout::HISTORY_CAPACITY;
    for step in 0..samples {
        let mut sample = latest.clone();
        if step + 1 < samples {
            sample.cpu.total_usage = wobble(latest.cpu.total_usage, step, 0);
            for (core, usage) in sample.cpu.core_usages.iter_mut().enumerate() {
                *usage = wobble(*usage, step, core * 3);
            }
            sample.disk.read_mbps = scale(latest.disk.read_mbps, step, 2);
            sample.disk.write_mbps = scale(latest.disk.write_mbps, step, 7);
            sample.network.download_mbps = scale(latest.network.download_mbps, step, 11);
            sample.network.upload_mbps = scale(latest.network.upload_mbps, step, 4);
            sample.gpu.usage = latest.gpu.usage.map(|usage| wobble(usage, step, 5));
        }
        histories.record(&sample);
    }
    histories
}

/// This machine's card, as [`dump_local_card`] paints it but with moving charts.
fn showcase_local_card() -> Value {
    let mut snapshot = local_fixture_snapshot();
    // The picture of the product has the GPU the e2e fixture deliberately
    // lacks: Apple Silicon, so its memory is the GPU's share of RAM.
    snapshot.gpu = wire::Gpu {
        usage: Some(23.0),
        vram_used_gb: Some(9.8),
        vram_total_gb: Some(64.0),
        unified_memory: Some(true),
    };
    local::card_from(
        "mac-studio",
        &snapshot,
        &showcase_histories(&snapshot.to_wire()),
    )
}

/// One healthy remote host. Hand-made rather than read from the wire fixture,
/// whose `/boot` volume sits at 92% on purpose — amber from 85%, so the Machines
/// row would need attention — and whose cores run hot.
fn showcase_remote_card() -> Value {
    let snapshot = wire::Snapshot {
        timestamp: "2026-07-31T12:00:00Z".to_string(),
        cpu: wire::Cpu {
            total_usage: 18.4,
            core_usages: vec![
                22.0, 14.0, 31.0, 9.0, 18.0, 26.0, 12.0, 7.0, 35.0, 16.0, 11.0, 24.0, 8.0, 19.0,
                13.0, 21.0,
            ],
            model: "AMD Ryzen 9 7950X 16-Core Processor".to_string(),
            // Linux exposes no thermal source and no memory pressure; the card
            // says so with an em dash, exactly as a real ubu-01 does.
            thermal_state: None,
        },
        memory: wire::Memory {
            used_gb: 21.6,
            total_gb: 62.7,
            swap_used_gb: 0.0,
            pressure: None,
            pressure_level: None,
        },
        disk: wire::Disk {
            read_mbps: Some(6.8),
            write_mbps: Some(24.5),
        },
        network: wire::Network {
            download_mbps: Some(3.4),
            upload_mbps: Some(1.2),
        },
        // Serving a local model: busy, most of its VRAM taken, and still
        // healthy — the GPU meters never raise attention.
        gpu: wire::Gpu {
            usage: Some(64.0),
            vram_used_gb: Some(18.6),
            vram_total_gb: Some(24.0),
            unified_memory: Some(false),
        },
        battery: None,
        volumes: vec![
            wire::Volume {
                mount: "/".to_string(),
                fstype: Some("ext4".to_string()),
                used_gb: 388.0,
                total_gb: 916.0,
            },
            wire::Volume {
                mount: "/mnt/data".to_string(),
                fstype: Some("xfs".to_string()),
                used_gb: 1_712.0,
                total_gb: 3_686.0,
            },
        ],
        processes: vec![
            wire::Process::from_cpu_percent(991, "podman".to_string(), 31.0, 612.0),
            wire::Process::from_cpu_percent(1377, "ollama".to_string(), 22.0, 4_310.0),
            wire::Process::from_cpu_percent(1204, "postgres".to_string(), 12.5, 1_840.0),
        ],
    };
    let mut card = host_card(
        "ubu-01",
        &snapshot,
        &showcase_histories(&snapshot),
        &Connection::Live,
    );
    card["id"] = json!("ubu-01");
    card
}

/// The showcase Hosts payload: this machine and one healthy remote host — the
/// solo developer's desk and the box under it.
fn dump_showcase_cockpit(available: f64) -> Value {
    cockpit_payload(CockpitInput {
        cards: vec![showcase_local_card(), showcase_remote_card()],
        remote_count: 1,
        available,
        look: HostLook {
            overflow: HostOverflowMode::Stack,
            core_row_span: viewmodel::layout::CORE_ROW_SPAN_DEFAULT,
        },
    })
}

/// The Usage quota the showcase passes: the fixture's 9,400 events land at 38%
/// of it, where the e2e fixture's `10_000` deliberately lands them amber.
const SHOWCASE_USAGE_QUOTA: u64 = 25_000;
/// The Azure budget the showcase passes: comfortably above the fixture's
/// projected month, where the e2e fixture's `2_000.0` lands it at 97%.
const SHOWCASE_AZURE_BUDGET: f64 = 3_000.0;

/// The overview, composed from the showcase views.
fn dump_showcase_dashboard() -> Value {
    let mut hosts = dump_showcase_cockpit(1200.0);
    hosts["id"] = json!("hosts");
    dashboard::view(
        &dashboard::default_layout(),
        &[
            hosts,
            dump_github_showcase(false),
            dump_github_showcase(true),
            dump_containers_showcase(),
            services::view(&services::showcase_statuses()),
            dump_crons(crons::Fixture::Healthy, false),
            dump_usage(usage::Fixture::Measured, false, SHOWCASE_USAGE_QUOTA),
            dump_azure(azure::Fixture::Measured, false, SHOWCASE_AZURE_BUDGET),
            dump_openclaw(openclaw::Fixture::Showcase),
        ],
    )
}

fn dump_github_showcase(runners: bool) -> Value {
    let now = chrono::DateTime::from_timestamp(1_780_056_300, 0).expect("valid timestamp");
    let state = github::showcase_state(now);
    if runners {
        github::runners_view(&state, &[], u64::try_from(now.timestamp()).unwrap_or(0))
    } else {
        github::repos_view(&state, now)
    }
}

/// The cadence the offline container fixtures are judged against: the default.
const DUMP_CONTAINERS_CADENCE_SECS: u64 = PanelInterval::Containers.spec().default_secs as u64;

fn dump_containers_showcase() -> Value {
    const NOW: u64 = 1_700_000_000;
    let (state, rules, presence) = containers::showcase_state(NOW);
    let mut payload =
        containers::view(&state, &rules, &presence, NOW, DUMP_CONTAINERS_CADENCE_SECS);
    payload["ruleEditor"] = settings::rules_section(&rules, &[]);
    payload
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
        dump_card("ubu-01", &Connection::Live),
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
    cockpit_payload(CockpitInput {
        cards: std::iter::once(dump_local_card()).chain(remote).collect(),
        remote_count,
        available,
        look: HostLook {
            overflow,
            core_row_span: viewmodel::layout::CORE_ROW_SPAN_DEFAULT,
        },
    })
}

/// The Usage and Azure Cost panels as fixtures, at a fixed `now` for the same
/// reason `--dump-containers` is: every relative age in a footer would otherwise
/// drift on each regeneration and no test could assert one.
///
/// `stale` dates the **metered providers'** last successful read most of a day
/// back — Claude's walk stays current, because it is on a cadence two orders of
/// magnitude faster and its staleness is a different fixture's job. Like
/// [`dump_azure`], a fixture has no store, so the cadence the age is classified
/// against is the one an unconfigured store polls at.
/// A quota the e2e fixture's own count lands at 94% of, so the amber step is
/// exercised rather than a flat green bar.
const FIXTURE_USAGE_QUOTA: u64 = 10_000;
/// The e2e fixture's Azure budget, which its projected month lands at 97% of.
const FIXTURE_AZURE_BUDGET: f64 = 2_000.0;

fn dump_usage(kind: usage::Fixture, stale: bool, quota: u64) -> Value {
    const NOW: u64 = 1_700_000_000;
    let cadence_secs = u64::from(PanelInterval::UsageProviders.spec().default_secs);
    let providers_at = if stale { NOW - 23 * 3_600 } else { NOW };
    // The plan rows show on the fixture that holds a reading of them, stale or
    // not: the stale dump must keep every row the healthy one has (only its
    // clocks move), or the browser suite's equal-height check would be
    // comparing two different bodies. Every other dump renders the panel as a
    // store starts, both plans off, which puts the setup line in front of the
    // browser suite.
    let plans = if kind == usage::Fixture::Measured {
        usage::FIXTURE_PLANS
    } else {
        plan_usage::PlanInputs::OFF
    };
    usage::view(
        &usage::fixture_state(kind, NOW, providers_at),
        quota,
        usage::NeonRates {
            usd_per_cu_hour: 0.106,
            usd_per_gib_month: 0.35,
        },
        plans,
        cadence_secs,
        NOW,
    )
}

/// `stale` dates the last successful read most of a day back, which is the one
/// rendering the panel's clock produces rather than its state: a real figure,
/// dimmed, with its age beside the footer's separate warning about the poller.
/// A fixture has no store, so the cadence it is classified against is the one an
/// unconfigured store polls at.
fn dump_azure(kind: azure::Fixture, stale: bool, budget: f64) -> Value {
    const NOW: u64 = 1_700_000_000;
    let cadence_secs = u64::from(PanelInterval::AzureCost.spec().default_secs);
    let read_at = if stale { NOW - 23 * 3_600 } else { NOW };
    azure::view(
        &azure::fixture_state(kind, read_at),
        budget,
        cadence_secs,
        NOW,
    )
}

/// The Sentry Crons panel as a fixture.
///
/// Both clocks are fixed so the file is byte-stable across regenerations: the
/// panel clock keeps the footer from drifting, and the *wire* clock is what makes
/// the ages exactly `7d 22h` and `0d 22h` — the two figures the age rule is
/// about — on every machine and at every hour.
///
/// `stale` moves only the **panel** clock, dating the last successful read 23h
/// back. The wire clock stays put, so the rows still read `7d 22h` — which is
/// the point of the rendering: the same ages, now carrying the age of the
/// reading itself.
fn dump_crons(kind: crons::Fixture, stale: bool) -> Value {
    const NOW: u64 = 1_700_000_000;
    let cadence_secs = u64::from(PanelInterval::Crons.spec().default_secs);
    let wire_now = chrono::DateTime::parse_from_rfc3339("2026-08-04T15:00:00Z")
        .expect("a fixed wire clock")
        .with_timezone(&chrono::Utc);
    let read_at = if stale { NOW - 23 * 3_600 } else { NOW };
    crons::view(
        &crons::fixture_state(kind, read_at, wire_now),
        cadence_secs,
        NOW,
    )
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

    let mut live = Host::new("ubu-01", "100.100.100.100");
    live.id = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0001);
    live.hidden_volume_mounts = vec!["/mnt/scratch".into()];
    let mut spare = Host::new("nuc-spare", "100.64.0.7");
    spare.id = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0002);
    spare.enabled = false;
    // Paired, and with the certificate its agent presents no longer the pinned
    // one (#448): the only rendering that carries the pin, the Re-pair button
    // and its explanation. `live` stays unpinned so the ordinary edit form —
    // and its exact `settings_save_host` arguments — keep their coverage.
    spare.tls_fingerprint = Some(certpin::format(&[0xAB; certpin::DIGEST_LEN]));
    let certificate_changed = [spare.id];
    // A third host that has reported no agent version (#489): the one rendering
    // a fixture with only versioned hosts would never reach. Paired with the two
    // above it, the Settings fixture carries a host that is behind the newest
    // verified release (`live`), one at it (`spare`) and one that cannot say.
    let mut silent = Host::new("lab-edge", "100.64.0.9");
    silent.id = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0003);
    let agent_versions = [
        (live.id, "2026.10.13".to_owned()),
        (spare.id, "2026.11.1".to_owned()),
    ];
    let agent_release = viewmodel::agent_release::Latest::Published("2026.11.1".to_owned());

    // Two accounts and, between them, every rendering the row has: one with a
    // token and two repos depending on it (so the removal prompt exists), one
    // disabled with no token and nothing attributed (so the "No token" badge
    // and the *absent* prompt are covered too).
    let accounts = dump_accounts();
    let stored = StoredSecrets {
        neon: false,
        sentry: true,
        vercel: true,
        openclaw: false,
        hosts: [live.id].into_iter().collect(),
        accounts: [accounts[0].id].into_iter().collect(),
    };
    // A gateway configured *and* waiting on approval, so the fixture carries the
    // pairing block — the one part of this tab that only exists in a live
    // state, and therefore the one part a static fixture would otherwise never
    // cover.
    let settings = store::Settings {
        openclaw_gateway_url: "ws://gateway.local:7878".into(),
        // On in the fixture, off in `Settings::default()` — the checked
        // rendering of the toggle is the one a default fixture would never
        // show. The default itself is asserted in `crates/store`, where it
        // belongs, and again in the view tests below.
        crash_reporting_enabled: true,
        // Exactly one panel configured, for the same reason: the cadence rows
        // render two ways — an override (whose **Use default** button is live)
        // and a panel nobody has ever set — and a fixture carrying only one of
        // them leaves the other half of the group untested. Azure, because its
        // floor is the one that guards spend.
        panel_intervals: std::collections::BTreeMap::from([("azure_cost".to_owned(), 6 * 3600)]),
        ..settings
    };
    let facts = openclaw::fixture_state(openclaw::Fixture::Pairing).settings_facts();
    // Two, and one of them disabled, so the Services tab's row renders in both
    // of its states. Fixed uuids for the same reason the hosts have them: a
    // fixture that changes on every dump is one no test can assert an id
    // against.
    let vendors = dump_status_vendors();
    settings::view(
        settings::StoreSections {
            settings: &settings,
            hosts: &[live, spare, silent],
            agent_versions: &agent_versions,
            agent_release: &agent_release,
            // Explicit, not `seeded_repos()`: nothing is seeded any more, and
            // this fixture is what the Playwright suite renders the Portfolio
            // tab from — an empty list would silently stop covering it.
            //
            // The first two are attributed to the first account, which is what
            // gives the Accounts tab a row whose removal actually costs
            // something. The third is unattributed — the state a repo added
            // while two accounts exist arrives in — so the Portfolio tab's
            // "Fetched by" picker is covered in both of its readings.
            repos: &[
                store::TrackedRepo {
                    account_id: Some(accounts[0].id),
                    ..store::TrackedRepo::new("acme/widget")
                },
                store::TrackedRepo {
                    account_id: Some(accounts[0].id),
                    ..store::TrackedRepo::new("acme/gadget")
                },
                store::TrackedRepo::new("acme/pipe-fitting"),
            ],
            vendors: &vendors,
            certificate_changed: &certificate_changed,
            plain_refused: &[],
            accounts: &accounts,
        },
        &stored,
        &facts,
        // The state a contributor's own build is in: opted in (so the toggle is
        // covered in its *checked* rendering, which the default would not be)
        // on a build with no DSN compiled in. That combination is exactly the
        // one whose explanatory line matters, so the Playwright suite sees the
        // line rather than only the checkbox.
        settings::CrashFacts {
            build_has_dsn: false,
            active: false,
            needs_restart: true,
        },
        // An update on offer, which is the one Updates state a static fixture
        // would otherwise never reach: it is the only one carrying both
        // buttons and the release notes, and it is not the state any real
        // `--dump-settings` run would be in. The other three renderings are
        // covered by `update::view`'s own tests, where they cost nothing.
        {
            let mut updates = UpdateState::new();
            updates.found(
                "2026.8.114".to_owned(),
                Some("Signed, notarized releases and the in-app updater.".to_owned()),
            );
            update::view(&updates, Some("2026.8.113"))
        },
    )
}

/// The vendor accounts the Settings fixture carries.
///
/// Fixed uuids for the reason the hosts have them: a fixture that changes on
/// every dump is one no test can assert an id against. `secret_account` is
/// spelled out rather than left to `VendorAccount::new`, because what the store
/// holds is the *item name* and a fixture that skipped it would render a row
/// this build could not resolve a credential for.
fn dump_accounts() -> Vec<VendorAccount> {
    let mut work = VendorAccount::new(VendorKind::GitHub, "work", "");
    work.id = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0021);
    work.secret_account = SecretKey::VendorToken(work.id).account();
    // One org watched, so the fixture carries the org list in its filled state
    // — the "No organizations watched" state rides the second account.
    work.orgs = Some(vec!["acme".to_owned()]);
    let mut personal = VendorAccount::new(VendorKind::GitHub, "personal", "");
    personal.id = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0022);
    personal.secret_account = SecretKey::VendorToken(personal.id).account();
    personal.enabled = false;
    vec![work, personal]
}

/// The status vendors the Settings fixture carries.
///
/// Deliberately not real vendors of this stack: the panel ships no list (#284),
/// and a fixture naming one operator's dependencies is the shipped-example
/// problem in a different file.
fn dump_status_vendors() -> Vec<store::StatusVendor> {
    let mut watched = store::StatusVendor::new(
        "Example Cloud",
        "https://status.example.com",
        "k8w3r06qmzrp",
        "API",
    );
    watched.id = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0011);
    let mut paused = store::StatusVendor::new(
        "Example Mail",
        "https://status.mail.example.com",
        "3f2p8q1x7z0d",
        "Outbound sending",
    );
    paused.id = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0012);
    paused.enabled = false;
    vec![watched, paused]
}

/// The rules the Settings fixture carries — the seeded three, plus the two
/// renderings seeding alone never reaches.
///
/// Every rendering the rule editor has, in one list.
///
/// Written out rather than grown from `seeded_rules()`, which is empty now —
/// a fixture derived from it would cover nothing at all. Between them these
/// five carry: a host-scoped Collapse, an all-hosts Hide, a live **expected
/// count** (the field's non-empty state), an **Expect** row whose
/// Collapse-only fields must therefore be absent, and a rule scoped to a host
/// that no longer exists — the case `rule_host_options` grows an extra option
/// for, and the one where a picker silently renders blank if it doesn't.
fn dump_container_rules() -> Vec<ContainerGroupRule> {
    let mut with_count = ContainerGroupRule::new(
        "api-*",
        "workflow jobs",
        store::ContainerRuleAction::Collapse,
    )
    .on_host("ubu-01");
    with_count.expected_count = Some(4);
    vec![
        ContainerGroupRule::new(
            "runner-*",
            "ci runners",
            store::ContainerRuleAction::Collapse,
        )
        .on_host("ubu-01"),
        with_count,
        ContainerGroupRule::new("ghcr.io/*", "", store::ContainerRuleAction::Hide),
        ContainerGroupRule::new("build-vm", "", store::ContainerRuleAction::Expect)
            .on_host(store::LOCAL_HOST_SCOPE),
        ContainerGroupRule::new(
            "legacy-*",
            "legacy jobs",
            store::ContainerRuleAction::Collapse,
        )
        .on_host("retired-box"),
    ]
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
        let mut payload = containers::view(
            &state,
            &[],
            &std::collections::BTreeMap::new(),
            NOW,
            DUMP_CONTAINERS_CADENCE_SECS,
        );
        payload["ruleEditor"] = settings::rules_section(&[], &[]);
        return payload;
    }
    let (state, rules, presence) = containers::fixture_state(NOW);
    let mut payload =
        containers::view(&state, &rules, &presence, NOW, DUMP_CONTAINERS_CADENCE_SECS);
    // The Settings fixture's rule list, not `fixture_state`'s: it carries every
    // rendering the editor has, including a scope naming a host that is gone.
    payload["ruleEditor"] = settings::rules_section(&dump_container_rules(), &[]);
    payload
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
        github::runners_view(
            &state,
            &[github::FIXTURE_GROUP_LABEL.to_owned()],
            u64::try_from(now.timestamp()).unwrap_or(0),
        )
    } else {
        github::repos_view(&state, now)
    }
}

/// The overview fixture uses the exact same panel views as its live command.
fn dump_dashboard() -> Value {
    dump_dashboard_of(&dashboard::default_layout())
}

/// The same readings under another layout, so a fixture can hold a Rust-authored
/// attention chip target for a layout the default does not have.
fn dump_dashboard_of(layout: &store::DashboardLayout) -> Value {
    let mut hosts = dump_cockpit(1200.0, 3, HostOverflowMode::Stack);
    hosts["id"] = json!("hosts");
    // The Machines tile's GPU meters need a GPU to draw: ubu-01 serves a local
    // model from a 24 GB card, the shape the tile grew those meters for. Only
    // here — the cockpit fixtures keep the GPU-less host their specs assert.
    let mut serving = dump_snapshot();
    serving.gpu = wire::Gpu {
        usage: Some(97.0),
        vram_used_gb: Some(22.1),
        vram_total_gb: Some(24.0),
        unified_memory: Some(false),
    };
    assert_eq!(
        hosts["hosts"][1]["id"], "ubu-01",
        "the remote host the fixture swaps"
    );
    hosts["hosts"][1] = dump_card_of("ubu-01", &serving, &Connection::Live);
    machine_alerts::attach(&mut hosts, &store::machine_alerts::MachineAlerts::default());
    // One cadence set and the rest untouched, so the fixture carries both
    // renderings the control has ("Set to ..." and "Using the default").
    let mut cadences = store::settings::Settings::default();
    cadences.set_panel_interval_secs(store::settings::PanelInterval::AzureCost, 6 * 3600);
    let mut payloads = vec![
        hosts,
        dump_github(false, false),
        dump_github(false, true),
        dump_containers(false),
        services::view(&services::fixture_statuses()),
        dump_crons(crons::Fixture::Alerting, false),
        dump_usage(usage::Fixture::Measured, false, FIXTURE_USAGE_QUOTA),
        dump_azure(azure::Fixture::Measured, false, FIXTURE_AZURE_BUDGET),
        dump_openclaw(openclaw::Fixture::Connected),
    ];
    cadence::attach(&mut payloads, &cadences);
    dashboard::view(layout, &payloads)
}

/// The layout behind `sample-dashboard-chips.json`: one source per way an
/// attention chip can lead. Repos has a tile whose scope misses a failing repo,
/// Runners has only a hidden tile, and Machines has two tiles that keep every
/// row. The rest keep the default's one tile, or none.
fn chips_layout() -> store::DashboardLayout {
    let mut layout = dashboard::default_layout();
    layout.tiles[1].scope = "healthy".into();
    layout.tiles[2].hidden = true;
    let mut second = layout.tiles[0].clone();
    second.id = "hosts-second".into();
    second.title = "Spare Machines".into();
    layout.tiles.push(second);
    layout
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
    // `--showcase` swaps any of the panel dumps below for its marketing
    // counterpart — see "The showcase fixtures" above.
    let showcase = args.iter().any(|arg| arg == "--showcase");
    if let Some(path) = dump_flag_path(args, "--dump-dashboard", "sample-dashboard.json") {
        let mut snapshot = if showcase {
            dump_showcase_dashboard()
        } else if args.iter().any(|arg| arg == "--chips") {
            dump_dashboard_of(&chips_layout())
        } else {
            dump_dashboard()
        };
        if args.iter().any(|arg| arg == "--expanded") {
            let mut layout = dashboard::default_layout();
            let repos = &mut layout.tiles[1];
            repos.row_limit = "all".into();
            repos.sort_by = "issues".into();
            repos.sort_descending = true;
            let runners = &mut layout.tiles[2];
            runners.row_limit = "all".into();
            snapshot["tiles"] = json!(layout
                .tiles
                .iter()
                .map(|t| dashboard::preview(t, &snapshot).unwrap())
                .collect::<Vec<_>>());
            snapshot["layout"] = json!(layout);
        }
        write_json(&path, &snapshot);
        return true;
    }
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
        if showcase {
            write_json(
                &path,
                &dump_showcase_cockpit(value_flag(args, "--width", default_width)),
            );
            return true;
        }
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
        if showcase {
            write_json(&path, &dump_containers_showcase());
        } else {
            write_json(
                &path,
                &dump_containers(args.iter().any(|arg| arg == "--empty")),
            );
        }
        return true;
    }
    let empty = args.iter().any(|arg| arg == "--empty");
    let stale = args.iter().any(|arg| arg == "--stale");
    if let Some(path) = dump_flag_path(args, "--dump-repos", "sample-repos.json") {
        if showcase {
            write_json(&path, &dump_github_showcase(false));
        } else {
            write_json(&path, &dump_github(empty, false));
        }
        return true;
    }
    if let Some(path) = dump_flag_path(args, "--dump-runners", "sample-runners.json") {
        if showcase {
            write_json(&path, &dump_github_showcase(true));
        } else {
            write_json(&path, &dump_github(empty, true));
        }
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
        let quota = if showcase {
            SHOWCASE_USAGE_QUOTA
        } else {
            FIXTURE_USAGE_QUOTA
        };
        write_json(&path, &dump_usage(kind, stale, quota));
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
        let budget = if showcase {
            SHOWCASE_AZURE_BUDGET
        } else {
            FIXTURE_AZURE_BUDGET
        };
        write_json(&path, &dump_azure(kind, stale, budget));
        return true;
    }
    if let Some(path) = dump_flag_path(args, "--dump-services", "sample-services.json") {
        // `--empty` is a pass that *looked* and found nothing to watch — the
        // rendering an unconfigured cockpit gets now that the vendor list is
        // derived (#284/#375), and the one that must never read "all clear".
        let statuses = if showcase {
            services::showcase_statuses()
        } else if empty {
            let mut statuses = services::ServiceStatuses::new();
            statuses.watching(Vec::new());
            statuses
        } else if args.iter().any(|arg| arg == "--loading") {
            // No pass has run: the watched set is not derived yet, so the
            // panel's loading line, built by the real `view`.
            services::ServiceStatuses::new()
        } else {
            let mut statuses = services::fixture_statuses();
            if stale {
                // The last good pass an hour ago, against the default cadence's
                // window: the footer's amber line.
                statuses.pass_finished(panel::now_unix().saturating_sub(3600), 5, 0);
            } else if args.iter().any(|arg| arg == "--fresh") {
                // A real pass that just finished (5 read, 0 failed): the footer
                // is null because the clock is current, not because it never ran.
                statuses.pass_finished(panel::now_unix(), 5, 0);
            }
            statuses
        };
        write_json(&path, &services::view(&statuses));
        return true;
    }
    if let Some(path) = dump_flag_path(args, "--dump-crons", "sample-crons.json") {
        let kind = if empty || showcase {
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
        write_json(&path, &dump_crons(kind, stale));
        return true;
    }
    if let Some(path) = dump_flag_path(args, "--dump-openclaw", "sample-openclaw.json") {
        let kind = if showcase {
            openclaw::Fixture::Showcase
        } else if empty {
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

/// Where the store lives. `SOLADOR_STORE_DIR` overrides the platform default
/// so a smoke run or a throwaway experiment can seed a scratch store instead
/// of editing the real one (see the manual IPC smoke test in `app/README.md`).
///
/// `github_token_present` is the store's v1 → v2 migration input: `crates/store`
/// never reads the credential store itself, so this side of the boundary owns
/// looking. Only [`Credential::Present`] counts — an unreadable keychain is not
/// evidence of an absent token, and the store treats a `false` as "ask again
/// next launch" rather than as a finished migration.
fn open_store(github_token_present: bool) -> Result<Store, StoreError> {
    // The same rule `solador-app host` finds the hand-off socket by.
    match Store::dir_override() {
        Some(dir) => Store::open_in(dir, github_token_present),
        None => Store::open(github_token_present),
    }
}

/// One host as `SOLADOR_SEED_HOST` spells it.
#[derive(Debug, PartialEq, Eq)]
struct SeedHost {
    name: String,
    address: String,
    port: u16,
    token: String,
}

/// Parses `"name|address|port|token"`, byte-for-byte the same rules as
/// `RemoteHostsCoordinator.seedFromEnvironmentIfNeeded()` in the original: empty
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

/// Provisions a host from `SOLADOR_SEED_HOST` if one with that address is
/// not already configured — headless/first-run setup, exactly as in the original.
///
/// The same-address no-op is what makes this safe to leave set: relaunching
/// with the variable still exported must not accumulate duplicate hosts, and
/// address (not name) is the identity because the name is the editable field.
///
/// Returns the id of the host it added, or `None` when it did nothing.
///
/// The original app skips this entirely under `DemoMode.isEnabled`
/// (the app entry point), so a synthetic demo host is never written to real
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
/// Sorted by name, matching the original coordinator's `SortDescriptor(\.name)`.
/// The local machine is not in this list and never sorts against it: the original
/// cockpit puts it first unconditionally (`HostsPanel.hosts` is
/// `[local] + remoteHosts.hosts`), and so does [`cockpit_cards`].
fn display_order(hosts: &[Host]) -> Vec<&Host> {
    let mut enabled: Vec<&Host> = hosts.iter().filter(|h| h.enabled).collect();
    enabled.sort_by(|a, b| a.name.cmp(&b.name));
    enabled
}

/// The app's shared state, fresh. One place, so startup and the tests build the
/// same thing and a test can hold the real wake and cadence wiring.
fn new_app(
    store: Store,
    credentials: impl CredentialStore + Send + Sync + 'static,
    runtime: tokio::runtime::Handle,
    crash_reporting: crashreport::Reporting,
) -> App {
    App {
        store: Mutex::new(store),
        credentials: Box::new(credentials),
        hosts: Mutex::new(Vec::new()),
        local: Arc::new(Mutex::new(LocalHostState::new())),
        containers: Mutex::new(ContainersState::new()),
        containers_wake: tokio::sync::Notify::new(),
        github: Mutex::new(GitHubState::new()),
        github_wake: tokio::sync::Notify::new(),
        services_wake: tokio::sync::Notify::new(),
        usage: Mutex::new(UsageState::new()),
        usage_wake: tokio::sync::Notify::new(),
        usage_providers_due: std::sync::atomic::AtomicBool::new(false),
        usage_rollups_due: std::sync::atomic::AtomicBool::new(false),
        plan_usage_wake: tokio::sync::Notify::new(),
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
        update: Mutex::new(UpdateState::new()),
        agent_release: Mutex::new(viewmodel::agent_release::Latest::NotChecked),
        handle: std::sync::OnceLock::new(),
        runtime,
        crash_reporting,
    }
}

/// Read the operator's answer out of the store and start reporting only if it
/// is yes.
///
/// The store is already open by the time this runs, so the answer is genuinely
/// known here and `OptIn` is `Granted` or `Declined` — never `Unknown`. That
/// third state is the *default* of the enum precisely so that a caller who has
/// not looked cannot accidentally read as consent; refusing is what an unread
/// preference has to mean.
///
/// The DSN is [`crashreport::BUILD_DSN`], compiled in. On a clean clone there
/// is none and this is a silent no-op — the common case, and not a failure.
/// Nothing is printed for the ordinary "off" state either: a line every launch
/// saying a disabled feature is disabled is noise. The three lines below are
/// each about something an operator would otherwise be misled by.
fn start_crash_reporting(store: &Store) -> crashreport::Reporting {
    let opt_in = if store.settings().crash_reporting_enabled {
        crashreport::OptIn::Granted
    } else {
        crashreport::OptIn::Declined
    };
    let reporting = crashreport::start(crashreport::Config {
        opt_in,
        dsn: crashreport::BUILD_DSN,
        release: settings::VERSION,
        // Never a machine name, a user or a host — this is a build profile and
        // nothing else. Sentry stamps it on every event.
        environment: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
    });
    match reporting.decision() {
        // The common case, and the default. Silence.
        crashreport::Decision::Disabled(crashreport::DisabledReason::OptedOut) => {}
        // Turned on and going nowhere. Silent as far as the *user* is concerned
        // — no dialog, no error, nothing fails — but saying so once on the
        // terminal is the difference between "this build cannot report" and
        // "the toggle does nothing", which look identical from the UI.
        crashreport::Decision::Disabled(reason) => {
            eprintln!("crash reporting: {}", reason.user_message());
        }
        crashreport::Decision::Enabled(_) => {
            eprintln!("crash reporting: on (opted in); panics are scrubbed before they are sent");
        }
    }
    reporting
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // `solador-app host …` (and the `solador` symlink): the command mode, which
    // hands hosts to the running instance and touches nothing itself — no
    // store open, no keychain, no window. Decided before anything else here
    // runs, because everything below opens the store.
    if let Some(code) = hostsetup::cli::dispatch(&args) {
        std::process::exit(code);
    }
    if run_dump(&args) {
        return;
    }

    // Tokens live in the OS credential store, never in the store file.
    let credentials = KeyringStore::new();

    // Read before the store opens, because the store's v1 -> v2 migration
    // needs the answer and `crates/store` deliberately cannot get it itself.
    // It has to happen before the credential consolidation below, which needs
    // `store.hosts()` and so cannot run first. Unreadable is not absent.
    let github_token_present = matches!(
        read_credential(&credentials, SecretKey::GitHubAccessToken),
        Credential::Present(_)
    );

    let mut store = match open_store(github_token_present) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("could not open the Solador store: {e}");
            std::process::exit(1);
        }
    };

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
    // Skipped entirely under `SOLADOR_STORE_DIR`: that variable points
    // `store.json` at a scratch directory, but the credential *service*
    // stays the real one (see `open_store`) -- so a scratch/smoke run would
    // migrate against whatever host list the scratch store happens to have
    // (typically none), write the real `secrets_v1` blob from that host
    // list, and permanently freeze migration (it no-ops once the blob
    // exists), leaving every real host's token unreadable on the next real
    // launch. `migrate_legacy`'s own "blob already exists" guard can't catch
    // this: an empty or wrong host list still looks like "nothing to copy",
    // not a scratch run.
    if Store::dir_override().is_none() {
        let mut migrate_keys = SecretKey::static_migration_keys();
        migrate_keys.extend(store.hosts().iter().map(|h| SecretKey::HostToken(h.id)));
        match credentials.migrate_legacy(&migrate_keys) {
            Ok(0) => {}
            Ok(n) => {
                eprintln!(
                    "secrets: migrated {n} credential(s) into the consolidated keychain item"
                );
            }
            Err(e) => {
                eprintln!("secrets: migration failed (legacy items still readable): {e}");
            }
        }
    }

    let seed = std::env::var("SOLADOR_SEED_HOST").ok();
    if let Err(e) = seed_from_env(&mut store, &credentials, seed.as_deref()) {
        eprintln!("could not seed a host from SOLADOR_SEED_HOST: {e}");
    }

    // Crash reporting, as early as the operator's answer can be known — which
    // is here, and not one line sooner: the answer lives in the store, so
    // anything that panics while the store is opening or while the credential
    // migrations run is deliberately not reported. Asking permission first is
    // the whole feature; a report from before permission was read would be the
    // one thing this must never send.
    let reporting = start_crash_reporting(&store);

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let app = Arc::new(new_app(store, credentials, rt.handle().clone(), reporting));
    // One task per host: an unreachable host's 5s client timeout must not hold
    // up any other host's tick. Startup is the same code path a settings edit
    // takes, so there is one definition of "which hosts are polled".
    reload_hosts(&app);
    // The other end of `solador-app host …`: hosts handed over on the local
    // hand-off channel (a Unix socket, a Windows named pipe) are added through the same code Settings uses, so this process
    // stays the only writer of the store and the keychain (crates/hostsetup).
    rt.spawn(handoff::serve(Arc::clone(&app)));
    // The containers panel polls on its own 10s cadence and reads the poll set
    // rather than owning one, so a host added in Settings joins it on the next
    // tick with no reload of its own.
    rt.spawn(containers_loop(Arc::clone(&app)));
    // `/v1/health` on its own slow cadence, over the same poll set: the only
    // source that can tell a *succeeding* poll it is being served frozen
    // numbers. It reads the poll set rather than owning one, so a host added in
    // Settings joins it on the next tick.
    rt.spawn(health_loop(Arc::clone(&app)));
    // The newest verified agent release (#489): once now, then hourly. Settings
    // compares each host's health-reported version against it.
    rt.spawn(agent_release_loop(Arc::clone(&app)));
    rt.spawn(hosts_watch_loop(Arc::clone(&app)));
    rt.spawn(resume_loop(Arc::clone(&app)));
    // The GitHub panels run on their own cadence and read the portfolio and the
    // token on every pass, so an edit in Settings joins them on the next pass —
    // which `wake_github` makes immediate.
    rt.spawn(github_loop(Arc::clone(&app)));
    // Service health, on a cadence and a loop of its own (#615), and needing no
    // credential — it starts whether or not a GitHub token exists.
    rt.spawn(services_loop(Arc::clone(&app)));
    // This machine, on the host cadence — the card that leads the grid.
    rt.spawn(local::poll_loop(Arc::clone(&app.local), POLL_INTERVAL));
    // Usage: Claude on its rollup cadence, Neon, Sentry and Vercel on their own cadence
    // inside the same loop.
    rt.spawn(usage_loop(Arc::clone(&app)));
    rt.spawn(plan_usage_loop(Arc::clone(&app)));
    // Sentry cron monitors, on the same default hourly cadence as the Sentry read
    // inside the usage loop — a persistence watch, not a real-time alarm.
    rt.spawn(crons_loop(Arc::clone(&app)));
    // Azure cost, on the reader's own 4h cadence.
    rt.spawn(azure_loop(Arc::clone(&app)));
    // OpenClaw: a live WebSocket, not a cadence. It idles at once when no
    // gateway is configured and wakes the moment one is saved.
    rt.spawn(openclaw_loop(Arc::clone(&app)));

    window_state::configure(tauri::Builder::default())
        // Opening a repo row's Actions page, or the one waiting run's page.
        // The webview's grant is one command, scoped to two URL shapes:
        // `https://github.com/*/*/actions`, composed by `github::actions_url`,
        // and `https://github.com/*/*/actions/runs/*`, composed by
        // `github::run_url` — see `capabilities/default.json`.
        .plugin(tauri_plugin_opener::init())
        // Needs-approval banners. Registered so `NotificationExt` resolves in
        // `deliver_approval_notices`; granted nothing, because only Rust ever
        // calls it.
        .plugin(tauri_plugin_notification::init())
        // The in-app updater (#308). Granted nothing either, for the stronger
        // version of the same reason: `run_update_check` and
        // `run_update_install` are the only callers, so the webview cannot
        // reach a plugin whose job is to download and unpack executable code.
        // The frontend's whole share is `update_status` / `update_check` /
        // `update_install`, which are app-defined and need no ACL entry.
        //
        // The comparator is not a preference either (#353). Left alone, the
        // plugin evaluates `release.version > package_info().version`, and
        // `package_info().version` is `tauri.conf.json`'s `version` — a key
        // this repo deliberately does not author (#303), so Tauri falls back to
        // `app/src-tauri/Cargo.toml`'s unpublished `0.1.0`. Every CalVer ever
        // released is greater than that, so every build without the release
        // `--config` overlay was told it was behind, on every launch, forever.
        // `UpdaterBuilder` has no `current_version` setter — the field is
        // private and set from `package_info()` — so this is the supported
        // seam. The `current` handed in is that wrong number and is discarded;
        // `settings::VERSION` is the git-derived CalVer `build.rs` publishes
        // for every profile, and is what the app actually is.
        .plugin(
            tauri_plugin_updater::Builder::new()
                .default_version_comparator(|_package_info_version, release| {
                    viewmodel::update::is_newer(settings::VERSION, &release.version.to_string())
                })
                .build(),
        )
        .manage(Arc::clone(&app))
        .setup({
            let app = Arc::clone(&app);
            move |shell| {
                // Before the handle, deliberately: `deliver_banners` drops
                // every notice until the handle exists, so claiming the
                // notification identity here is provably ahead of the first
                // banner — and therefore ahead of the plugin's own
                // `set_application`, which only runs on the first `show()`.
                #[cfg(target_os = "macos")]
                claim_notification_identity(&shell.config().identifier);
                // The poll loops are already running; this is the moment they
                // gain a way to reach the OS.
                let _ = app.handle.set(shell.handle().clone());
                // The launch update check (#308), and the ONLY automatic one.
                // After the handle, necessarily: it is what `updater()` is
                // reached through, and what the banner is delivered over.
                //
                // Spawned rather than awaited — this hook runs before the
                // window is shown, and a release server that never answers
                // must not be able to hold the cockpit closed.
                let checking = Arc::clone(&app);
                app.runtime.spawn(run_update_check(checking));
                Ok(())
            }
        })
        .invoke_handler(tauri::generate_handler![
            dashboard_view,
            dashboard_preview,
            dashboard_save,
            cockpit,
            containers,
            repos,
            runners,
            runners_forget,
            usage,
            azure_cost,
            services,
            crons,
            openclaw,
            update_status,
            update_check,
            update_install,
            settings_view,
            machine_save_alerts,
            machine_acknowledge_warning,
            machine_set_volume_hidden,
            runner_group_label_add,
            runner_group_label_remove,
            runner_group_label_move,
            panel_interval_save,
            panel_interval_clear,
            settings_set_crash_reporting,
            settings_set_plan_usage,
            settings_save_azure,
            settings_save_providers,
            settings_add_host,
            settings_probe_host_certificate,
            settings_set_host_enabled,
            settings_remove_host,
            container_rule_add,
            container_rule_set,
            container_rule_remove,
            settings_test_host,
            settings_save_host,
            settings_add_repo,
            settings_remove_repo,
            settings_set_repo_enabled,
            settings_set_repo_workflows,
            settings_set_repo_account,
            settings_set_repo_tracked,
            settings_discover_repos,
            settings_save_account,
            settings_set_account_enabled,
            settings_set_account_org,
            settings_remove_account,
            settings_save_openclaw,
            settings_openclaw_retry,
            settings_probe_status_vendor,
            settings_save_status_vendor,
            settings_set_status_vendor_enabled,
            settings_remove_status_vendor,
            settings_save_secret,
            settings_clear_secret,
            window_fit::window_fit_height,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start");

    // Keep the runtime alive for the lifetime of the app.
    drop(rt);
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::settings::ClampOutcome;
    use store::{MemoryCredentialStore, SecretError};

    fn plan_fixture() -> (tempfile::TempDir, Mutex<Store>, Mutex<UsageState>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path(), false).unwrap();
        (dir, Mutex::new(store), Mutex::new(UsageState::new()))
    }

    #[test]
    fn a_plan_enable_whose_save_fails_is_put_back_to_off() {
        let (dir, store, usage) = plan_fixture();
        // Make the save fail: its directory becomes a file.
        std::fs::remove_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path(), b"not a directory").unwrap();
        let status = apply_plan_usage(&store, &usage, "claude", true).unwrap();
        assert!(status.starts_with("Failed"), "{status}");
        assert!(
            !store.lock().unwrap().settings().claude_plan_usage_enabled,
            "an enable that did not persist must not stay on"
        );
        std::fs::remove_file(dir.path()).unwrap();
        std::fs::create_dir(dir.path()).unwrap();
    }

    #[test]
    fn a_plan_enable_that_saves_stays_on_and_reads_the_right_provider() {
        let (_dir, store, usage) = plan_fixture();
        apply_plan_usage(&store, &usage, "codex", true).unwrap();
        let guard = store.lock().unwrap();
        assert!(guard.settings().codex_plan_usage_enabled);
        assert!(!guard.settings().claude_plan_usage_enabled);
    }

    #[test]
    fn an_unknown_plan_provider_is_refused_and_changes_nothing() {
        let (_dir, store, usage) = plan_fixture();
        usage
            .lock()
            .unwrap()
            .plan_mut(subscription::Provider::Claude)
            .park_for_test();
        let status = apply_plan_usage(&store, &usage, "gemini", true).unwrap();
        assert!(status.starts_with("Skipped"), "{status}");
        let guard = store.lock().unwrap();
        assert!(!guard.settings().claude_plan_usage_enabled);
        assert!(!guard.settings().codex_plan_usage_enabled);
        drop(guard);
        assert_eq!(
            plan_usage::gate(
                true,
                usage.lock().unwrap().plan(subscription::Provider::Claude),
                0,
                900
            ),
            plan_usage::Gate::Parked,
            "a refused request resets nothing"
        );
    }

    #[test]
    fn turning_a_parked_plan_provider_off_and_on_un_parks_it() {
        let (_dir, store, usage) = plan_fixture();
        let provider = subscription::Provider::Claude;
        let gate = |usage: &Mutex<UsageState>| {
            plan_usage::gate(true, usage.lock().unwrap().plan(provider), 0, 900)
        };
        apply_plan_usage(&store, &usage, "claude", true).unwrap();
        usage.lock().unwrap().plan_mut(provider).park_for_test();
        assert_eq!(gate(&usage), plan_usage::Gate::Parked);
        apply_plan_usage(&store, &usage, "claude", false).unwrap();
        apply_plan_usage(&store, &usage, "claude", true).unwrap();
        assert_eq!(gate(&usage), plan_usage::Gate::Due);
    }

    /// Which loop each cadence's save wakes (#627). The match in `wake_panel`
    /// is exhaustive, so a new cadence cannot be forgotten, but a wrong arm
    /// (`Services` waking GitHub's loop) compiled and passed everything.
    #[test]
    fn each_cadence_wakes_exactly_its_own_loop() {
        let expected = [
            (PanelInterval::Containers, WakeTarget::Containers),
            (PanelInterval::GitHub, WakeTarget::GitHub),
            (PanelInterval::Services, WakeTarget::Services),
            (PanelInterval::ClaudeRollups, WakeTarget::Usage),
            (PanelInterval::UsageProviders, WakeTarget::Usage),
            (PanelInterval::AzureCost, WakeTarget::Azure),
            (PanelInterval::Crons, WakeTarget::Crons),
            (PanelInterval::PlanUsage, WakeTarget::PlanUsage),
        ];
        for (panel, target) in expected {
            assert_eq!(wake_target(panel), target, "{panel:?}");
        }
    }

    /// A usage pass wakes Services only when Claude rollups appear or vanish
    /// (#627), not on every pass.
    #[test]
    fn services_wake_only_when_claude_presence_changes() {
        assert!(claude_presence_wakes_services(false, true), "appeared");
        assert!(claude_presence_wakes_services(true, false), "disappeared");
        assert!(
            !claude_presence_wakes_services(false, false),
            "still absent"
        );
        assert!(!claude_presence_wakes_services(true, true), "still present");
    }

    /// A real `App` over a throwaway store: the same constructor startup uses,
    /// so the wake and cadence wiring under test is the wiring that ships.
    /// The runtime and directory are returned to keep them alive.
    fn test_app() -> (tempfile::TempDir, tokio::runtime::Runtime, App) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path(), false).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let reporting = start_crash_reporting(&store);
        let app = new_app(
            store,
            MemoryCredentialStore::new(),
            rt.handle().clone(),
            reporting,
        );
        (dir, rt, app)
    }

    /// Whether the `Notify` holds a stored permit (consumes it).
    fn woken(notify: &tokio::sync::Notify) -> bool {
        futures_util::FutureExt::now_or_never(notify.notified()).is_some()
    }

    fn windows_of(app: &App) -> (u64, u64, u64) {
        (
            app.github.lock().unwrap().stale_after_secs(),
            app.usage.lock().unwrap().claude_stale_after_secs(),
            app.services.lock().unwrap().stale_after_secs(),
        )
    }

    /// The usage loop's clocks, in whole seconds: `(rollups cadence, since the
    /// rollups last finished, providers cadence, since the providers did)`.
    fn usage_clocks(r: u64, since_r: Option<u64>, p: u64, since_p: Option<u64>) -> UsageClocks {
        use std::time::Duration as D;
        UsageClocks {
            rollups_every: D::from_secs(r),
            since_rollups: since_r.map(D::from_secs),
            providers_every: D::from_secs(p),
            since_providers: since_p.map(D::from_secs),
        }
    }

    /// #635: the loop sleeps until whichever half is due next, so neither
    /// cadence is rounded up to a multiple of the other's. At the 300s provider
    /// floor, a 240s or 600s rollups cadence must not stretch the provider gap
    /// to 480s / 600s, and a 1h rollups cadence is not cut to the providers'.
    #[test]
    fn the_usage_loop_wakes_for_whichever_half_is_due_next() {
        use std::time::Duration as D;
        let sleep = |r, since_r, p, since_p| usage_clocks(r, since_r, p, since_p).sleep();
        assert_eq!(sleep(240, Some(0), 300, Some(240)), D::from_secs(60));
        assert_eq!(sleep(600, Some(0), 300, Some(0)), D::from_secs(300));
        assert_eq!(sleep(60, Some(0), 3_600, Some(0)), D::from_secs(60));
        assert_eq!(sleep(3_600, Some(300), 300, Some(0)), D::from_secs(300));
        assert_eq!(
            sleep(3_600, Some(0), 300, Some(900)),
            D::ZERO,
            "overdue means due now"
        );
        assert_eq!(
            sleep(60, None, 300, None),
            D::ZERO,
            "never run means due now"
        );
    }

    /// #635: each half runs only on its own clock or a forced wake. The Claude
    /// walk re-reads every session log, so it must not ride on the providers'
    /// turn: a 1h rollups cadence beside 300s providers walks hourly, not every
    /// five minutes.
    #[test]
    fn each_usage_half_runs_only_when_its_own_clock_says_so() {
        let turn = |clocks: UsageClocks, force_r, force_p| clocks.turn(force_r, force_p);
        let both = UsageTurn {
            rollups: true,
            providers: true,
        };
        let rollups = UsageTurn {
            rollups: true,
            providers: false,
        };
        let providers = UsageTurn {
            rollups: false,
            providers: true,
        };
        let neither = UsageTurn {
            rollups: false,
            providers: false,
        };

        assert_eq!(
            turn(usage_clocks(60, None, 300, None), false, false),
            both,
            "first turn"
        );
        assert_eq!(
            turn(usage_clocks(60, Some(60), 300, Some(60)), false, false),
            rollups
        );
        assert_eq!(
            turn(usage_clocks(3_600, Some(300), 300, Some(300)), false, false),
            providers,
            "the providers' turn does not walk the logs"
        );
        assert_eq!(
            turn(usage_clocks(3_600, Some(10), 300, Some(10)), false, false),
            neither,
            "a cadence edit's wake recomputes and runs nothing early"
        );
        assert_eq!(
            turn(usage_clocks(3_600, Some(10), 300, Some(10)), false, true),
            providers,
            "a saved credential forces the providers only"
        );
        assert_eq!(
            turn(usage_clocks(3_600, Some(10), 300, Some(10)), true, true),
            both,
            "resume forces both"
        );
    }

    /// #635, end to end over the clocks: drive the loop's own decision through
    /// many cycles at legal cadence pairs, with passes that take real time:
    /// Claude walks of 5s and 25s, and provider passes up to all four requests
    /// at their 30s timeout. Which alignments it reaches depends on the
    /// cadences, so the worst one is pinned separately, below.
    ///
    /// Three properties: neither half starts again sooner than its cadence
    /// after it last finished, and a provider reading, stamped as its read
    /// completes, is never older than its staleness window when the next one
    /// lands. With constant durations every provider's stamps sit at a fixed
    /// offset into the pass, so the gap between two stamps is the gap between
    /// two pass starts.
    #[test]
    fn the_usage_loop_holds_both_cadences_over_many_cycles() {
        use std::time::Duration as D;
        for walk in [5, 25] {
            for rollups in [30, 60, 240, 600, 3_600, 14_400] {
                for providers in [300, 3_600, 7_200, 21_600] {
                    for provider_pass in [3, 120] {
                        let horizon = 20 * rollups.max(providers);
                        let (mut t, mut rollups_last, mut providers_last) = (0u64, None, None);
                        let (mut walks, mut passes) = (Vec::new(), Vec::new());
                        let clocks = |t: u64, rl: Option<u64>, pl: Option<u64>| {
                            usage_clocks(rollups, rl.map(|l| t - l), providers, pl.map(|l| t - l))
                        };
                        while t < horizon {
                            let turn = clocks(t, rollups_last, providers_last).turn(false, false);
                            if turn.rollups {
                                walks.push((t, t + walk));
                                t += walk;
                                rollups_last = Some(t);
                            }
                            if turn.providers {
                                passes.push((t, t + provider_pass));
                                t += provider_pass;
                                providers_last = Some(t);
                            }
                            let sleep = clocks(t, rollups_last, providers_last).sleep();
                            assert!(
                                turn != (UsageTurn {
                                    rollups: false,
                                    providers: false
                                }) || sleep > D::ZERO,
                                "a turn that ran nothing must sleep"
                            );
                            t += sleep.as_secs();
                        }
                        let at = format!(
                            "walk {walk}s, rollups {rollups}s, providers {providers}s, \
                             pass {provider_pass}s"
                        );
                        for pair in walks.windows(2) {
                            assert!(
                                pair[1].0 - pair[0].1 >= rollups,
                                "walked early ({at}): {pair:?}"
                            );
                        }
                        for pair in passes.windows(2) {
                            assert!(
                                pair[1].0 - pair[0].1 >= providers,
                                "polled early ({at}): {pair:?}"
                            );
                            let age = pair[1].0 - pair[0].0;
                            assert!(
                                age <= usage::provider_stale_after_secs(providers),
                                "a healthy loop went stale ({at}): {age}s between readings"
                            );
                        }
                        // Neither half stopped: the last run of each ended no
                        // further from the horizon than one gap of its own.
                        let (last_walk, last_pass) =
                            (walks.last().unwrap(), passes.last().unwrap());
                        assert!(
                            last_walk.1 + rollups + provider_pass >= horizon,
                            "walks stopped ({at})"
                        );
                        assert!(
                            last_pass.1 + providers + walk >= horizon,
                            "polls stopped ({at})"
                        );
                    }
                }
            }
        }
    }

    /// The alignment the many-cycles test reaches only by chance: the walk and
    /// the providers fall due in the same turn, `usage_loop` runs the walk
    /// first (an order this test states but cannot observe: it lives in the
    /// loop, not in `UsageClocks`), and the providers' pass waits out all of
    /// it. At the 300s floor,
    /// with all four requests at their 30s timeout, a walk of up to 30s still
    /// lands the next reading inside the window (the footer warns only past
    /// it). A walk that falls due a second earlier runs alone, and the
    /// providers, due while it ran, go next with no sleep.
    #[test]
    fn a_walk_due_with_the_providers_still_fits_the_floor_window() {
        use std::time::Duration as D;
        const PASS: u64 = 120;
        let window = usage::provider_stale_after_secs(300);
        let only = |rollups, providers| UsageTurn { rollups, providers };
        for walk in [5, 25, 30] {
            // The last provider pass ran 0..120, so the providers fall due at 420.
            let both = usage_clocks(60, Some(60), 300, Some(300));
            assert_eq!(both.turn(false, false), only(true, true), "walk {walk}s");
            let next_pass = PASS + 300 + walk;
            assert!(
                next_pass <= window,
                "walk {walk}s: {next_pass}s between readings against {window}s"
            );

            let first = usage_clocks(60, Some(60), 300, Some(299));
            assert_eq!(first.turn(false, false), only(true, false), "walk {walk}s");
            let after = usage_clocks(60, Some(0), 300, Some(299 + walk));
            assert_eq!(after.sleep(), D::ZERO, "walk {walk}s");
            assert_eq!(after.turn(false, false), only(false, true), "walk {walk}s");
        }
    }

    /// Resume from sleep forces both halves; a saved credential forces only
    /// the providers. `Instant` may not count the sleep, so without the
    /// rollups flag a resume would not reliably find the walk due.
    #[test]
    fn a_resume_forces_the_rollups_and_a_credential_save_does_not() {
        use std::sync::atomic::Ordering;
        let (_dir, _rt, app) = test_app();
        wake_usage(&app, UsageWake::Providers);
        assert!(app.usage_providers_due.swap(false, Ordering::SeqCst));
        assert!(!app.usage_rollups_due.swap(false, Ordering::SeqCst));

        wake_usage(&app, UsageWake::Everything);
        assert!(app.usage_providers_due.swap(false, Ordering::SeqCst));
        assert!(app.usage_rollups_due.swap(false, Ordering::SeqCst));
    }

    /// A resume wakes every slow loop, Sentry Crons included (#635): its
    /// window is 1.5 cycles now, so a loop left to its own timer after a nap
    /// would read stale while healthy. Usage is woken with both halves forced.
    #[test]
    fn a_resume_wakes_every_slow_loop() {
        use std::sync::atomic::Ordering;
        let (_dir, _rt, app) = test_app();
        wake_after_resume(&app);
        let loops: [(&str, &tokio::sync::Notify); 8] = [
            ("github", &app.github_wake),
            ("services", &app.services_wake),
            ("usage", &app.usage_wake),
            ("azure", &app.azure_wake),
            ("containers", &app.containers_wake),
            ("crons", &app.crons_wake),
            ("plan usage", &app.plan_usage_wake),
            ("openclaw", &app.openclaw_wake),
        ];
        for (name, notify) in loops {
            assert!(woken(notify), "a resume did not wake {name}");
        }
        assert!(app.usage_providers_due.load(Ordering::SeqCst));
        assert!(app.usage_rollups_due.load(Ordering::SeqCst));
    }

    /// #635: the Crons and metered-provider footer windows follow their own
    /// cadence at render time, so a save or clear moves them in the same
    /// response, and one panel's cadence never moves the other's.
    #[test]
    fn crons_and_provider_footer_windows_follow_their_own_cadence() {
        // 7000s old: past the 5400s window of the 1h default, inside the 10800s
        // window of a 2h cadence.
        let stale = |v: &Value| !v["footer"].is_null();
        let (_dir, _rt, app) = test_app();
        let now = panel::now_unix();
        *app.crons.lock().unwrap() =
            crons::fixture_state(crons::Fixture::Alerting, now - 7_000, chrono::Utc::now());
        *app.usage.lock().unwrap() =
            usage::fixture_state(usage::Fixture::Measured, now, now - 7_000);
        assert!(stale(&crons_unscoped(&app)) && stale(&usage_unscoped(&app)));

        panel_interval_save_in(&app, "crons", 7_200).unwrap();
        assert!(!stale(&crons_unscoped(&app)), "crons save moves its window");
        assert!(stale(&usage_unscoped(&app)), "providers window untouched");

        panel_interval_clear_in(&app, "crons").unwrap();
        assert!(stale(&crons_unscoped(&app)), "crons clear moves it back");

        panel_interval_save_in(&app, "usage_providers", 7_200).unwrap();
        assert!(
            !stale(&usage_unscoped(&app)),
            "providers save moves its window"
        );
        assert!(stale(&crons_unscoped(&app)), "crons window untouched");

        panel_interval_clear_in(&app, "usage_providers").unwrap();
        assert!(
            stale(&usage_unscoped(&app)),
            "providers clear moves it back"
        );
    }

    /// The usage pass seam (#627): Services wakes exactly when presence flips,
    /// in both directions, and the permit is really stored on the loop's own
    /// `Notify`. Deleting the wake, or reading presence after applying the
    /// walk, fails this.
    #[test]
    fn a_usage_pass_wakes_services_only_when_claude_presence_flips() {
        let usage = Mutex::new(UsageState::new());
        let wake = tokio::sync::Notify::new();
        let walk = |present: bool| {
            apply_claude_pass(
                &usage,
                &wake,
                present.then(usage::UsageSummary::default),
                1,
                None,
            )
        };

        assert!(!walk(false), "absent -> absent");
        assert!(!woken(&wake));
        assert!(walk(true), "absent -> present");
        assert!(woken(&wake), "and the Services loop was actually woken");
        assert!(!walk(true), "present -> present");
        assert!(!woken(&wake), "no wake on a pass that changed nothing");
        assert!(walk(false), "present -> absent");
        assert!(woken(&wake));
    }

    /// A save moves the GitHub, Claude-rollup and Services windows before any
    /// pass has run -- through the command's own body, so dropping its
    /// `sync_app_cadences` fails here.
    #[test]
    fn a_cadence_save_moves_its_window_before_any_pass() {
        let (_dir, _rt, app) = test_app();
        assert_eq!(windows_of(&app), (150, 150, 150), "defaults, no pass yet");

        panel_interval_save_in(&app, "github", 300).unwrap();
        assert_eq!(windows_of(&app).0, 750);
        panel_interval_save_in(&app, "claude_rollups", 300).unwrap();
        assert_eq!(windows_of(&app).1, 750);
        panel_interval_save_in(&app, "services", 600).unwrap();
        assert_eq!(windows_of(&app), (750, 750, 1500));
        // Shortened: the window follows at once, not on the next pass.
        panel_interval_save_in(&app, "services", 30).unwrap();
        assert_eq!(windows_of(&app).2, 150);
        assert!(woken(&app.services_wake), "and the save woke its own loop");
    }

    /// …and a clear returns every window to the default's, again before any pass.
    #[test]
    fn a_cadence_clear_moves_its_window_before_any_pass() {
        let (_dir, _rt, app) = test_app();
        for panel in ["github", "claude_rollups", "services"] {
            panel_interval_save_in(&app, panel, 600).unwrap();
        }
        assert_eq!(windows_of(&app), (1500, 1500, 1500));
        for (panel, index) in [("github", 0), ("claude_rollups", 1), ("services", 2)] {
            panel_interval_clear_in(&app, panel).unwrap();
            let windows = windows_of(&app);
            let at = [windows.0, windows.1, windows.2][index];
            assert_eq!(at, 150, "{panel} follows the default again");
        }
    }

    /// Makes the next `Store::save` fail: the store file's path becomes a
    /// directory, so the atomic rename onto it is refused on every platform.
    fn break_store_saves(app: &App) {
        let path = app.store.lock().unwrap().path().to_path_buf();
        let _ = std::fs::remove_file(&path);
        std::fs::create_dir_all(&path).unwrap();
    }

    fn github_secs(app: &App) -> (u32, bool) {
        let store = app.store.lock().unwrap();
        (
            store.settings().panel_interval_secs(PanelInterval::GitHub),
            store
                .settings()
                .panel_interval_is_configured(PanelInterval::GitHub),
        )
    }

    /// A below-floor value is refused in the floor's words before the store is
    /// touched: nothing stored, no window moved, no loop woken (#623).
    #[test]
    fn a_refused_cadence_leaves_the_store_untouched() {
        let (_dir, _rt, app) = test_app();
        let before = github_secs(&app);
        let error = panel_interval_save_in(&app, "github", 0).unwrap_err();
        assert!(!error.is_empty());
        assert_eq!(github_secs(&app), before, "the refusal stored nothing");
        assert!(!github_secs(&app).1, "and did not mark it configured");
        assert_eq!(windows_of(&app), (150, 150, 150));
        assert!(!woken(&app.github_wake), "a refusal wakes nothing");
    }

    /// A failed disk write puts the previous cadence back, for a save and a
    /// clear alike, and wakes nothing (#623).
    #[test]
    fn a_failed_cadence_save_rolls_back_and_wakes_nothing() {
        let (_dir, _rt, app) = test_app();
        panel_interval_save_in(&app, "github", 300).unwrap();
        assert!(woken(&app.github_wake), "the successful save woke its loop");
        break_store_saves(&app);

        let error = panel_interval_save_in(&app, "github", 600).unwrap_err();
        assert!(
            error.starts_with("Could not save the poll cadence"),
            "{error}"
        );
        assert_eq!(github_secs(&app), (300, true), "the save rolled back");
        assert_eq!(windows_of(&app).0, 750, "the window kept the saved cadence");
        assert!(!woken(&app.github_wake), "a failed save wakes nothing");

        let error = panel_interval_clear_in(&app, "github").unwrap_err();
        assert!(
            error.starts_with("Could not save the poll cadence"),
            "{error}"
        );
        assert_eq!(github_secs(&app), (300, true), "the clear rolled back");
        assert!(!woken(&app.github_wake), "a failed clear wakes nothing");
    }

    /// The wake follows a success and only a success, on its own loop (#623).
    #[test]
    fn a_cadence_change_wakes_its_loop_only_on_success() {
        let (_dir, _rt, app) = test_app();
        panel_interval_save_in(&app, "github", 300).unwrap();
        assert!(woken(&app.github_wake));
        assert!(!woken(&app.services_wake), "and no neighbour");
        panel_interval_clear_in(&app, "github").unwrap();
        assert!(woken(&app.github_wake), "a clear wakes it too");
    }

    /// A panel id the shell does not know is refused by both commands, with
    /// nothing changed and nothing woken (#623).
    #[test]
    fn an_unknown_cadence_panel_is_refused() {
        let (_dir, _rt, app) = test_app();
        let before = github_secs(&app);
        let error = panel_interval_save_in(&app, "nonesuch", 300).unwrap_err();
        assert!(error.contains("unknown panel"), "{error}");
        let error = panel_interval_clear_in(&app, "nonesuch").unwrap_err();
        assert!(error.contains("unknown panel"), "{error}");
        assert_eq!(github_secs(&app), before);
        for target in [
            WakeTarget::Containers,
            WakeTarget::GitHub,
            WakeTarget::Services,
            WakeTarget::Usage,
            WakeTarget::Azure,
            WakeTarget::Crons,
            WakeTarget::PlanUsage,
        ] {
            assert!(!woken(wake_notify(&app, target)), "{target:?}");
        }
    }

    /// The Containers footer judges staleness by the cadence the store holds
    /// *now*: a save or clear moves the window before any pass (#633), through
    /// the command's own render body.
    #[test]
    fn the_containers_window_moves_with_a_save_and_a_clear() {
        let (_dir, _rt, app) = test_app();
        const NOW: u64 = 1_700_000_000;
        app.containers.lock().unwrap().apply_local(
            vec![containers::parse::LocalRuntime::Docker],
            containers::parse::merge(
                vec![(containers::parse::LocalRuntime::Docker, Some(Vec::new()))],
                std::collections::BTreeMap::new(),
            ),
            NOW - 100,
        );
        let stale = |app: &App| !containers_view_in(app, NOW)["footer"].is_null();
        assert!(stale(&app), "100s old is stale at the 10s default (30s)");
        panel_interval_save_in(&app, "containers", 60).unwrap();
        assert!(!stale(&app), "60s cadence -> 150s window, at once");
        panel_interval_save_in(&app, "containers", 5).unwrap();
        assert!(stale(&app), "shortened: the window follows at once");
        panel_interval_save_in(&app, "containers", 600).unwrap();
        assert!(!stale(&app));
        panel_interval_clear_in(&app, "containers").unwrap();
        assert!(stale(&app), "cleared: back to the default's 30s");
    }

    const ALL_WAKE_TARGETS: [WakeTarget; 7] = [
        WakeTarget::Containers,
        WakeTarget::GitHub,
        WakeTarget::Services,
        WakeTarget::Usage,
        WakeTarget::Azure,
        WakeTarget::Crons,
        WakeTarget::PlanUsage,
    ];

    /// A clear wakes exactly its own loop, through `panel_interval_clear_in`
    /// itself (#633): dropping its `wake_panel` or aiming it elsewhere fails here.
    #[test]
    fn a_cadence_clear_wakes_only_its_own_loop() {
        // Every row saves a value that is NOT the panel's default, so the clear
        // really changes the effective cadence (Azure's default is 14_400).
        for (panel, kind, secs, expected) in [
            ("github", PanelInterval::GitHub, 14_400, WakeTarget::GitHub),
            (
                "claude_rollups",
                PanelInterval::ClaudeRollups,
                14_400,
                WakeTarget::Usage,
            ),
            (
                "usage_providers",
                PanelInterval::UsageProviders,
                7_200,
                WakeTarget::Usage,
            ),
            (
                "azure_cost",
                PanelInterval::AzureCost,
                21_600,
                WakeTarget::Azure,
            ),
            (
                "containers",
                PanelInterval::Containers,
                14_400,
                WakeTarget::Containers,
            ),
            (
                "services",
                PanelInterval::Services,
                14_400,
                WakeTarget::Services,
            ),
            ("crons", PanelInterval::Crons, 7_200, WakeTarget::Crons),
            (
                "plan_usage",
                PanelInterval::PlanUsage,
                1_800,
                WakeTarget::PlanUsage,
            ),
        ] {
            let (_dir, _rt, app) = test_app();
            panel_interval_save_in(&app, panel, secs).unwrap();
            assert_eq!(panel_interval(&app.store, kind).as_secs(), u64::from(secs));
            // Drain the save's permit so only the clear can be what we see.
            for target in ALL_WAKE_TARGETS {
                woken(wake_notify(&app, target));
            }
            panel_interval_clear_in(&app, panel).unwrap();
            assert_ne!(
                panel_interval(&app.store, kind).as_secs(),
                u64::from(secs),
                "clearing {panel} must change the effective cadence"
            );
            for target in ALL_WAKE_TARGETS {
                assert_eq!(
                    woken(wake_notify(&app, target)),
                    target == expected,
                    "clearing {panel}: {target:?}"
                );
            }
        }
    }

    /// A save wakes exactly its own loop's `Notify` and no other, through
    /// `wake_panel` itself (#627): hardcoding a target, or ignoring
    /// `wake_target`, leaves every other test green.
    #[test]
    fn a_cadence_save_wakes_only_its_own_loop() {
        for (panel, expected) in [
            ("github", WakeTarget::GitHub),
            ("claude_rollups", WakeTarget::Usage),
            ("azure_cost", WakeTarget::Azure),
            ("services", WakeTarget::Services),
        ] {
            let (_dir, _rt, app) = test_app();
            panel_interval_save_in(&app, panel, 14_400).unwrap();
            for target in [
                WakeTarget::Containers,
                WakeTarget::GitHub,
                WakeTarget::Services,
                WakeTarget::Usage,
                WakeTarget::Azure,
                WakeTarget::Crons,
                WakeTarget::PlanUsage,
            ] {
                assert_eq!(
                    woken(wake_notify(&app, target)),
                    target == expected,
                    "saving {panel}: {target:?}"
                );
            }
        }
    }

    /// Each target's `Notify` is its own loop's field, not a neighbour's
    /// (#627): `wake_target` can be right while the arm under it is wrong.
    #[test]
    fn each_wake_target_fires_its_own_notify() {
        let (_dir, _rt, app) = test_app();
        let expected: [(WakeTarget, &tokio::sync::Notify); 7] = [
            (WakeTarget::Containers, &app.containers_wake),
            (WakeTarget::GitHub, &app.github_wake),
            (WakeTarget::Services, &app.services_wake),
            (WakeTarget::Usage, &app.usage_wake),
            (WakeTarget::Azure, &app.azure_wake),
            (WakeTarget::Crons, &app.crons_wake),
            (WakeTarget::PlanUsage, &app.plan_usage_wake),
        ];
        for (target, notify) in expected {
            assert!(
                std::ptr::eq(wake_notify(&app, target), notify),
                "{target:?}"
            );
        }
    }

    /// A just-saved shorter cadence moves every cached staleness window before
    /// the next pass runs (#627): `sync_cadences` is what `panel_interval_save`
    /// calls ahead of its re-render.
    #[test]
    fn a_saved_cadence_moves_every_staleness_window_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = Mutex::new(Store::open_in(dir.path(), false).unwrap());
        let github = Mutex::new(GitHubState::new());
        let usage = Mutex::new(UsageState::new());
        let services = Mutex::new(services::ServiceStatuses::new());
        let windows = || {
            (
                github.lock().unwrap().stale_after_secs(),
                usage.lock().unwrap().claude_stale_after_secs(),
                services.lock().unwrap().stale_after_secs(),
            )
        };

        {
            let mut store = store.lock().unwrap();
            let settings = store.settings_mut();
            settings.set_panel_interval_secs(PanelInterval::GitHub, 300);
            settings.set_panel_interval_secs(PanelInterval::ClaudeRollups, 300);
            settings.set_panel_interval_secs(PanelInterval::Services, 300);
        }
        sync_cadences(&store, &github, &usage, &services);
        assert_eq!(windows(), (750, 750, 750));

        // Shortened, then synced with no pass in between.
        {
            let mut store = store.lock().unwrap();
            let settings = store.settings_mut();
            settings.set_panel_interval_secs(PanelInterval::GitHub, 30);
            settings.set_panel_interval_secs(PanelInterval::ClaudeRollups, 30);
            settings.set_panel_interval_secs(PanelInterval::Services, 30);
        }
        sync_cadences(&store, &github, &usage, &services);
        assert_eq!(windows(), (150, 150, 150));

        // A cleared override follows the default again.
        {
            let mut store = store.lock().unwrap();
            let settings = store.settings_mut();
            settings.set_panel_interval_secs(PanelInterval::Services, 600);
        }
        sync_cadences(&store, &github, &usage, &services);
        assert_eq!(services.lock().unwrap().stale_after_secs(), 1500);
        store
            .lock()
            .unwrap()
            .settings_mut()
            .clear_panel_interval(PanelInterval::Services);
        sync_cadences(&store, &github, &usage, &services);
        let default = u64::from(store::settings::DEFAULT_REFRESH_INTERVAL_SECS);
        assert_eq!(
            services.lock().unwrap().stale_after_secs(),
            panel::stale_after_for_cadence(default)
        );
    }

    /// The fix itself, asserted against the real `call_once` rather than a
    /// mock of it: our claim must land first, and the plugin's own call must
    /// then be refused.
    ///
    /// What this pins is the *mechanism*, which is the only part a test can
    /// reach. Whether macOS then renders the banner is not observable from
    /// here — that is the manual smoke step in `app/README.md`, and the
    /// measurement that motivated this fix is recorded there.
    ///
    /// **This is the only test in this binary that may call
    /// `set_application`.** It is a process-global `Once` with no reset, and
    /// cargo runs a crate's tests as threads in ONE process — so a second test
    /// touching it would silently make both meaningless, in whichever order
    /// they happened to run.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_plugin_cannot_claim_terminal_after_us() {
        // Exactly what the `setup` hook does.
        claim_notification_identity("app.solador.desktop");

        // And now the plugin's line, verbatim from `tauri-plugin-notification`'s
        // desktop `show()`, on the `is_dev()` branch this repo always takes.
        let refused = mac_notification_sys::set_application("com.apple.Terminal")
            .expect_err("the plugin must not be able to set the identity after our claim");

        assert!(
            refused.to_string().contains("can only be set once"),
            "expected the AlreadySet refusal that makes our claim stick, got: {refused}"
        );
    }

    /// [`cockpit_cards`] / [`cockpit_payload`] / [`dump_cockpit`] with the
    /// **default** look (stacking, the default core rows) — what every test
    /// that is not about the tab bar or the core rows wants. The tab-bar tests
    /// call the real functions with `HostOverflowMode::Tabs`.
    fn stacked_view(
        local: Option<Value>,
        hosts: &[Arc<Mutex<HostState>>],
        available: f64,
    ) -> Value {
        cockpit_payload(CockpitInput {
            cards: cockpit_cards(local, hosts),
            remote_count: hosts.len(),
            available,
            look: HostLook {
                overflow: HostOverflowMode::Stack,
                core_row_span: viewmodel::layout::CORE_ROW_SPAN_DEFAULT,
            },
        })
    }

    fn stacked_payload(cards: Vec<Value>, remote_count: usize, available: f64) -> Value {
        cockpit_payload(CockpitInput {
            cards,
            remote_count,
            available,
            look: HostLook::default(),
        })
    }

    /// The tabbed twin of [`stacked_payload`] — the core row span defaults
    /// here too, so a test that is about tabs says nothing about core grids.
    fn tabbed_payload(cards: Vec<Value>, remote_count: usize, available: f64) -> Value {
        cockpit_payload(CockpitInput {
            cards,
            remote_count,
            available,
            look: HostLook {
                overflow: HostOverflowMode::Tabs,
                core_row_span: viewmodel::layout::CORE_ROW_SPAN_DEFAULT,
            },
        })
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
            error_kind: None,
            sampler_stale: None,
            sample_age_seconds: None,
            version: None,
            plain_refused: false,
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
            version: Some("0.0.0-test".to_string()),
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

    // MARK: tile-scoped panel commands

    fn tile_in(app: &App, id: &str, source: &str, scope: &str, hidden: bool) {
        let mut layout = app
            .store
            .lock()
            .unwrap()
            .dashboard()
            .cloned()
            .unwrap_or_else(dashboard::default_layout);
        let mut tile = layout.tiles[0].clone();
        tile.id = id.into();
        tile.source = source.into();
        tile.scope = scope.into();
        tile.hidden = hidden;
        layout.tiles.push(tile);
        app.store.lock().unwrap().save_dashboard(layout).unwrap();
    }

    /// An App with `n` remote hosts (ids `Uuid::from_u128(1..=n)`), each
    /// holding the fixture snapshot.
    fn app_with_remotes(n: u128) -> (tempfile::TempDir, tokio::runtime::Runtime, App) {
        let (dir, rt, app) = test_app();
        for i in 1..=n {
            let key = HostKey {
                id: Uuid::from_u128(i),
                base_url: "http://127.0.0.1:1".into(),
                tls_fingerprint: None,
            };
            // Built by hand, with a task that does nothing: a real poll task
            // would overwrite the state set here with its own failure.
            let mut state = named_state(&format!("remote-{i}"), None, None, None);
            state.id = key.id.to_string();
            // The fixture's /boot is 92% full, which would raise attention on
            // its own and hide the limits under test.
            let mut snapshot = fixture();
            snapshot.volumes.clear();
            state.latest = Some(snapshot);
            state.last_success = Some(Instant::now());
            let polled = PolledHost {
                client: Arc::new(agent_client(&key, String::new())),
                key,
                state: shared(state),
                token_available: true,
                task: app.runtime.spawn(async {}),
            };
            app.hosts.lock().unwrap().push(polled);
        }
        (dir, rt, app)
    }

    fn keep_of(ids: &[String]) -> BTreeSet<String> {
        ids.iter().cloned().collect()
    }

    fn card_ids(payload: &Value) -> Vec<String> {
        payload["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn scoping_hosts_filters_cards_before_layout() {
        let (_dir, _rt, app) = app_with_remotes(2);
        // Wide enough that three cards would lay out in three columns.
        let keep = keep_of(&["local".to_owned()]);
        let payload = cockpit_with(3600.0, &app, Some(&keep));
        assert_eq!(card_ids(&payload), ["local"]);
        assert_eq!(payload["hostColumns"], 1, "layout saw one card");
        assert!(
            payload["empty"].is_null(),
            "two hosts are configured, so the scope dropping them is not an empty setup: {}",
            payload["empty"]
        );
    }

    #[test]
    fn a_remote_scoped_machines_tile_keeps_both_remotes() {
        let (_dir, _rt, app) = app_with_remotes(2);
        let remotes: Vec<String> = (1..=2).map(|i| Uuid::from_u128(i).to_string()).collect();
        let payload = cockpit_with(1400.0, &app, Some(&keep_of(&remotes)));
        assert_eq!(card_ids(&payload), remotes);
    }

    #[test]
    fn a_scope_that_keeps_no_machine_renders_no_cards() {
        let (_dir, _rt, app) = app_with_remotes(2);
        let payload = cockpit_with(1400.0, &app, Some(&BTreeSet::new()));
        assert!(card_ids(&payload).is_empty());
        assert!(
            payload["empty"].is_null(),
            "two hosts are configured, so the scope dropping them is not an empty setup: {}",
            payload["empty"]
        );
    }

    /// The other half of the rule above: with no remote configured, the setup
    /// line is true, and a scope must not hide it either.
    #[test]
    fn a_store_with_no_remotes_says_so_scoped_or_not() {
        let (_dir, _rt, app) = app_with_remotes(0);
        let message = json!({ "message": "No hosts configured. Add one in Settings." });
        let local = keep_of(&["local".to_owned()]);
        for (case, keep) in [
            ("unscoped", None),
            ("this machine", Some(&local)),
            ("nothing kept", Some(&BTreeSet::new())),
        ] {
            let payload = cockpit_with(1400.0, &app, keep);
            assert_eq!(payload["empty"], message, "{case}");
        }
    }

    /// A Machines tile scoped `all` with no `selectedRepos` keeps every card, so
    /// its Full view is the Machines panel itself, through the command's path.
    #[test]
    fn a_machines_tile_scoped_all_gets_the_cockpit_payload_unchanged() {
        let (_dir, _rt, app) = app_with_remotes(2);
        tile_in(&app, "t-all", "hosts", "all", false);
        let tile = tile_from_store(&app, "t-all", "hosts").unwrap();
        assert!(tile.selected_repos.is_empty(), "no repo selection");
        assert_eq!(
            cockpit_for_tile(1400.0, &app, "t-all").unwrap(),
            cockpit_with(1400.0, &app, None)
        );
    }

    #[test]
    fn a_panel_command_refuses_a_hidden_tile() {
        let (_dir, _rt, app) = test_app();
        tile_in(&app, "t-services", "services", "all", true);
        tile_in(&app, "t-hosts", "hosts", "all", true);
        assert_eq!(
            scope_to_tile(&app, "services", Some("t-services"), || json!({"rows": []})),
            Err(tile_scope::TILE_HIDDEN.to_owned())
        );
        assert_eq!(
            tile_from_store(&app, "t-hosts", "hosts").unwrap_err(),
            tile_scope::TILE_HIDDEN
        );
        assert_eq!(
            scope_to_tile(&app, "services", Some("nope"), || json!({})),
            Err(tile_scope::TILE_GONE.to_owned())
        );
        assert_eq!(
            scope_to_tile(&app, "services", None, || json!({"rows": [1]})),
            Ok(json!({"rows": [1]}))
        );
    }

    /// A refused tile builds nothing (#641). Every panel command passes its
    /// `X_unscoped(&state)` as the builder, and that function is what prints the
    /// panel's `FIRST_*_REQUEST` line, so a builder that never runs is a notice
    /// that never fires for a tile the command refuses.
    #[test]
    fn a_refused_tile_never_builds_its_payload() {
        let (_dir, _rt, app) = test_app();
        tile_in(&app, "t-hidden", "services", "all", true);
        tile_in(&app, "t-hosts", "hosts", "all", false);
        tile_in(&app, "t-services", "services", "all", false);
        let built = std::cell::Cell::new(0);
        let build = || {
            built.set(built.get() + 1);
            json!({"rows": []})
        };
        for (id, refusal) in [
            ("nope", tile_scope::TILE_GONE),
            ("t-hidden", tile_scope::TILE_HIDDEN),
            // Foreign: a Machines tile asked for as Services.
            ("t-hosts", tile_scope::TILE_WRONG_SOURCE),
        ] {
            assert_eq!(
                scope_to_tile(&app, "services", Some(id), build),
                Err(refusal.to_owned()),
                "{id}"
            );
            assert_eq!(built.get(), 0, "{id} was refused, so nothing was built");
        }
        // Controls: a kept tile, and no tile at all, do build.
        assert!(scope_to_tile(&app, "services", Some("t-services"), build).is_ok());
        assert_eq!(built.get(), 1, "a kept tile builds once");
        assert!(scope_to_tile(&app, "services", None, build).is_ok());
        assert_eq!(built.get(), 2, "no tile builds once");
    }

    /// Every panel command asks `find_tile` for the source its tile must show,
    /// and nothing but that call's literal says which (#645). Tauri's `State`
    /// cannot be built outside a running app, so the commands themselves cannot
    /// be called here; this reads each one's body from this file and pins the
    /// literal, that the command hands on its own `tile`, and that it builds
    /// with its own `*_unscoped` builder (`cockpit` with its own `Some(id)` arm).
    /// A command that names a neighbour's source, drops its tile or borrows a
    /// neighbour's builder, which the frontend's double and `scope_to_tile`'s
    /// own tests would all accept, fails here. So does a tenth command that
    /// scopes a tile without a row in the table.
    #[test]
    fn each_panel_command_scopes_its_tile_to_its_own_source() {
        // Command, the function whose body makes the call, and the call's text.
        let table: [(&str, &str, &str); 9] = [
            ("cockpit", "cockpit_for_tile", "tile_from_store(app, id, \""),
            ("repos", "repos", "scope_to_tile(&state, \""),
            ("runners", "runners", "scope_to_tile(&state, \""),
            ("containers", "containers", "scope_to_tile(&state, \""),
            ("services", "services", "scope_to_tile(&state, \""),
            ("crons", "crons", "scope_to_tile(&state, \""),
            ("usage", "usage", "scope_to_tile(&state, \""),
            ("azure_cost", "azure_cost", "scope_to_tile(&state, \""),
            ("openclaw", "openclaw", "scope_to_tile(&state, \""),
        ];
        let expected = [
            ("cockpit", "hosts"),
            ("repos", "ghWorkflows"),
            ("runners", "ghRunners"),
            ("containers", "containers"),
            ("services", "services"),
            ("crons", "sentryCrons"),
            ("usage", "claudeUsage"),
            ("azure_cost", "azureCost"),
            ("openclaw", "openclawAgents"),
        ];
        // Only the code above this module: its own table must not be read back.
        let whole = include_str!("main.rs").replace("\r\n", "\n");
        let source = whole.split("#[cfg(test)]\nmod tests {").next().unwrap();
        fn body_of<'a>(source: &'a str, function: &str) -> &'a str {
            let start = source
                .find(&format!("\nfn {function}("))
                .unwrap_or_else(|| panic!("no `fn {function}`"));
            let body = &source[start..];
            &body[..body.find("\n}\n").expect("function ends")]
        }
        for ((command, function, call), (named, want)) in table.iter().zip(expected) {
            assert_eq!(*command, named, "table rows line up");
            let body = body_of(source, function);
            let at = body
                .find(call)
                .unwrap_or_else(|| panic!("`{function}` makes no `{call}…` call"));
            let literal = &body[at + call.len()..];
            let got = &literal[..literal.find('"').expect("literal ends")];
            assert_eq!(got, want, "the {command} command scopes its tile to {got}");
            if *command == "cockpit" {
                continue;
            }
            assert!(
                literal[got.len()..].starts_with("\", tile.as_deref(),"),
                "the {command} command hands its own tile to `scope_to_tile`"
            );
            assert!(
                body.contains(&format!("{function}_unscoped(&state)")),
                "the {command} command builds with `{function}_unscoped`"
            );
        }
        // `cockpit` reaches `cockpit_for_tile`, the row above, only with its own tile.
        let cockpit = body_of(source, "cockpit");
        assert!(
            cockpit.contains("match tile.as_deref() {")
                && cockpit.contains("Some(id) => cockpit_for_tile("),
            "the cockpit command scopes a Machines tile with its own tile"
        );
        // The nine are all of them. Every function that calls into the scoping
        // is a row above, `cockpit`, or the scoping itself, whatever it calls its
        // tile: a tenth command that scopes one is a new row here.
        let mut callers = std::collections::BTreeSet::new();
        let mut current = "";
        for line in source.lines() {
            let decl = line.trim_start();
            let decl = decl.strip_prefix("pub(crate) ").unwrap_or(decl);
            let decl = decl.strip_prefix("pub ").unwrap_or(decl);
            let decl = decl.strip_prefix("async ").unwrap_or(decl);
            if let Some(rest) = decl.strip_prefix("fn ") {
                current = &rest[..rest.find(['(', '<']).expect("fn name ends")];
            } else if ["scope_to_tile(", "tile_from_store(", "cockpit_for_tile("]
                .iter()
                .any(|call| line.contains(call))
            {
                callers.insert(current);
            }
        }
        let mut want: std::collections::BTreeSet<&str> =
            table.iter().map(|(_, function, _)| *function).collect();
        want.extend(["cockpit", "scope_to_tile"]);
        assert_eq!(callers, want, "only the table's commands scope a tile");
        // And the other way in, which the callers check cannot see: a command
        // that takes `tile: Option<String>`, the argument the frontend's `{tile}`
        // binds to, and never scopes it. This counts that one spelling only.
        assert_eq!(
            source.matches("tile: Option<String>").count(),
            table.len(),
            "every command taking `tile: Option<String>` is in the table"
        );
    }

    /// One poll of a scoped Machines tile builds the cards once, although it
    /// lays them out twice (#641). The build is what [`cockpit_cards`] counts.
    #[test]
    fn a_scoped_machines_tile_builds_the_cards_once_per_poll() {
        let (_dir, _rt, app) = app_with_remotes(2);
        tile_in(&app, "t-local", "hosts", "local", false);
        tile_in(&app, "t-all", "hosts", "all", false);
        for id in ["t-local", "t-all"] {
            let before = CARD_BUILDS.with(std::cell::Cell::get);
            let scoped = cockpit_for_tile(1400.0, &app, id).unwrap();
            assert_eq!(
                CARD_BUILDS.with(std::cell::Cell::get) - before,
                1,
                "{id}: one build"
            );
            if id == "t-local" {
                assert_eq!(card_ids(&scoped), ["local"], "control: the scope applied");
            }
        }
    }

    #[test]
    fn a_machines_tile_scoped_to_attention_keeps_the_cards_it_shows() {
        let (_dir, _rt, app) = app_with_remotes(1);
        let remote = Uuid::from_u128(1).to_string();
        tile_in(&app, "t-attention", "hosts", "attention", false);
        let tile = tile_from_store(&app, "t-attention", "hosts").unwrap();
        let keeps_remote = |app: &App| {
            let payload = cockpit_with(1400.0, app, None);
            kept_hosts(&tile, &payload)
                .expect("attention filters")
                .contains(&remote)
        };
        assert!(!keeps_remote(&app), "within the default limits");
        // The stored RAM limit now sits under the host's reading.
        machine_alerts::save(
            &mut app.store.lock().unwrap(),
            None,
            Some(&json!({"cpuWarning":50,"cpuCritical":60,"ramWarning":1,"ramCritical":2})),
        )
        .unwrap();
        assert!(keeps_remote(&app), "over the stored limit");
        let scoped = cockpit_for_tile(1400.0, &app, "t-attention").unwrap();
        assert!(card_ids(&scoped).contains(&remote));
        let keep = kept_hosts(&tile, &cockpit_with(1400.0, &app, None)).unwrap();
        assert_eq!(
            card_ids(&scoped).into_iter().collect::<BTreeSet<_>>(),
            keep,
            "the scoped payload holds exactly the kept cards"
        );
    }

    #[test]
    fn dashboard_view_never_scopes() {
        let (_dir, _rt, app) = app_with_remotes(1);
        tile_in(&app, "t-local", "hosts", "local", false);
        let view = dashboard_view_in(1400.0, &app);
        let hosts = view["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == "hosts")
            .unwrap();
        let rows = hosts["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "this machine and the remote");
    }

    /// A tile's row gap is the tile's own, carried on `dashboard_view`, so a
    /// tile's Full view has it before any `cockpit` response (#646, #647). Zero
    /// travels as a value: the frontend only paints a finite key. Two tiles of
    /// one source carry two gaps, and the gap between cards stays the constant.
    #[test]
    fn dashboard_view_carries_each_tiles_own_row_gap() {
        let (_dir, _rt, app) = app_with_remotes(0);
        let flush = dashboard_view_in(1400.0, &app);
        assert_eq!(flush["spacing"], SPACING);
        for tile in flush["tiles"].as_array().unwrap() {
            assert_eq!(tile["rowGapPx"], 0, "{}", tile["id"]);
        }
        let mut layout = dashboard::default_layout();
        layout.tiles[0].row_gap_px = 10;
        layout.tiles[1].row_gap_px = 4;
        app.store.lock().unwrap().save_dashboard(layout).unwrap();
        let widened = dashboard_view_in(1400.0, &app);
        let gap = |i: usize| widened["tiles"][i]["rowGapPx"].clone();
        assert_eq!((gap(0), gap(1), gap(2)), (json!(10), json!(4), json!(0)));
    }

    /// Two Machines tiles with different CPU core rows and overflow modes lay
    /// their Full views out differently, and the read that names no tile takes
    /// the defaults: nothing reads a global (#647).
    #[test]
    fn each_machines_tile_lays_its_full_view_out_by_its_own_settings() {
        let (_dir, _rt, app) = app_with_remotes(1);
        tile_in(&app, "t-tall", "hosts", "all", false);
        tile_in(&app, "t-tabs", "hosts", "all", false);
        let mut layout = app.store.lock().unwrap().dashboard().cloned().unwrap();
        for tile in &mut layout.tiles {
            match tile.id.as_str() {
                "t-tall" => tile.core_row_span = 4,
                "t-tabs" => tile.host_overflow = HostOverflowMode::Tabs,
                _ => {}
            }
        }
        app.store.lock().unwrap().save_dashboard(layout).unwrap();
        // 1000pt: two hosts stack, so tabs have something to collapse.
        let tall = cockpit_for_tile(1000.0, &app, "t-tall").unwrap();
        let tabs = cockpit_for_tile(1000.0, &app, "t-tabs").unwrap();
        let plain = cockpit_with(1000.0, &app, None);
        assert!(tall["hostTabs"].is_null() && plain["hostTabs"].is_null());
        assert!(
            !tabs["hostTabs"].is_null(),
            "the tabs tile collapses its cards"
        );
        let block = |v: &Value| v["hosts"][0]["coreBlockHeight"].clone();
        assert_eq!(block(&plain), json!(220.0), "the default is two rows");
        assert_eq!(block(&tall), json!(440.0), "four rows of the core unit");
        assert_eq!(block(&tabs), block(&plain));
    }

    /// A panel's Full view fills the window, so the columns it affords come from
    /// `panel_columns` over the window's width and ride the payload as data:
    /// narrow is one column, wide is the panel's maximum.
    #[test]
    fn the_cockpit_payload_carries_each_panels_columns_for_the_window() {
        let (_dir, _rt, app) = app_with_remotes(0);
        let narrow = cockpit_with(400.0, &app, None);
        let wide = cockpit_with(3000.0, &app, None);
        for kind in viewmodel::cockpit::PanelKind::ALL {
            assert_eq!(narrow["panelColumns"][kind.id()], json!(1), "{kind:?}");
            assert_eq!(
                wide["panelColumns"][kind.id()],
                json!(viewmodel::cockpit::PANEL_MAX_COLUMNS),
                "{kind:?}"
            );
        }
    }

    /// Every sentence the page shows when one of its two reads fails is Rust's:
    /// the overview's own failure rides the `cockpit` payload (a failed
    /// `dashboard_view` has no words of its own), and the `cockpit` failure
    /// rides the dashboard's labels (#646).
    #[test]
    fn each_read_carries_the_sentence_for_the_others_failure() {
        let (_dir, _rt, app) = app_with_remotes(0);
        let cockpit = cockpit_with(1400.0, &app, None);
        assert_eq!(
            cockpit["overviewLoadFailed"],
            dashboard::OVERVIEW_LOAD_FAILED
        );
        let view = dashboard_view_in(1400.0, &app);
        assert_eq!(
            view["labels"]["cockpitFailed"],
            dashboard::COCKPIT_LOAD_FAILED
        );
        assert_eq!(view["labels"]["settings"], settings::OPEN_LABEL);
        for words in [
            dashboard::OVERVIEW_LOAD_FAILED,
            dashboard::COCKPIT_LOAD_FAILED,
        ] {
            assert!(
                !words.to_lowercase().contains("below"),
                "{words:?} points at a view that is going away"
            );
        }
    }

    /// The fixture's CPU reading as a card renders it. Derived from the
    /// parsed fixture rather than written out as `"34%"`, so editing
    /// `snapshot.json` can't leave a literal behind that has to be chased
    /// across this file.
    fn fixture_cpu_value() -> String {
        format!("{}%", fixture().cpu.total_usage.round() as i64)
    }

    // MARK: cadences

    /// Which clock each data source runs on, pinned against the original service
    /// that owns it.
    ///
    /// Every source except the 1s host poll and the event-driven OpenClaw
    /// session polls on a [`PanelInterval`] the operator owns (#615 retired the
    /// shared Refresh Interval that used to govern the two GitHub panels, the
    /// Services pass and the Claude rollups together), each with its own floor
    /// and reason. Wiring a loop to the wrong one would be a real bug that no
    /// other test would catch. A 4h Azure export polled every 30s is 480
    /// pointless blob listings an hour; a 1s host poll slowed to 300s is a dead
    /// chart axis, since one history sample is one fixed time slice.
    ///
    /// The constants below remain what an unconfigured store polls on: this
    /// test pins the constants, and
    /// `each_panels_default_cadence_is_the_constant_this_crate_declares` pins
    /// that the store still hands them back.
    ///
    /// | source | cadence | the original |
    /// |---|---|---|
    /// | Hosts (local + remote) | 1s | `RemoteHostMetricsService` |
    /// | Containers | 10s default | `LocalContainerService` |
    /// | Repos + Runners | 60s default (`PanelInterval::GitHub`) | app entry point |
    /// | Services | 60s default (`PanelInterval::Services`) | app entry point |
    /// | Claude rollups | 60s default (`PanelInterval::ClaudeRollups`) | app entry point |
    /// | Neon + Sentry | 1h default | `NeonUsageService` |
    /// | Sentry crons | 1h default | no counterpart — `crons_loop` |
    /// | Azure Cost | 4h default | `AzureCostService` |
    /// | OpenClaw | none — event-driven | `OpenClawService` |
    ///
    /// The three 60s rows are not constants of this file: they are the retired
    /// shared interval's default, and are pinned in the store beside their
    /// floors. What is pinned here for them is that no *fixed* cadence is
    /// confusable with that default, below.
    ///
    /// The crons row shared `PROVIDER_POLL_INTERVAL_SECS` with the Sentry read
    /// inside `usage_loop` rather than declaring a second hour of its own — same
    /// API, same rhythm, and two constants would have been free to drift. #300
    /// gave it a setting of its own whose default is still that hour, so they
    /// part only on an explicit edit. The consequence to accept either way is
    /// that a newly-red monitor can be invisible for up to an hour — this is a
    /// *persistence* watch, not a real-time alarm, and the daily Slack digest
    /// remains the prompt signal.
    #[test]
    fn every_data_source_polls_on_its_original_services_cadence() {
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
        for (source, cadence, expected) in cadences {
            assert_eq!(
                cadence, expected,
                "{source} drifted from its documented cadence"
            );
        }

        // The three per-source cadences the retired shared interval became
        // (#615), all defaulting to its 60s. Pinned because a changed default
        // silently changes what an unconfigured store polls on, and the values
        // the old control offered must stay legal for each.
        assert_eq!(store::settings::DEFAULT_REFRESH_INTERVAL_SECS, 60);
        for panel in [
            PanelInterval::GitHub,
            PanelInterval::Services,
            PanelInterval::ClaudeRollups,
        ] {
            assert_eq!(panel.spec().default_secs, 60, "{panel:?}");
        }

        // No fixed cadence may coincide with those defaults: that is what would
        // let a loop be wired to the wrong clock and still look correct out of
        // the box.
        for (source, cadence, _) in cadences {
            assert_ne!(
                cadence,
                u64::from(store::settings::DEFAULT_REFRESH_INTERVAL_SECS),
                "{source}'s fixed cadence must not be confusable with the default GitHub, Services or rollup cadence"
            );
        }
    }

    /// An unconfigured store still polls on the constants above.
    ///
    /// `crates/store` states each default as a literal beside its floor and
    /// tests it there, but it cannot import these constants — it is the
    /// dependency, not the dependent. This is the only place the two halves can
    /// be compared, so it is the only thing that would notice
    /// `containers::POLL_INTERVAL_SECS` moving to 15 while the store kept
    /// handing every loop the old 10.
    #[test]
    fn each_panels_default_cadence_is_the_constant_this_crate_declares() {
        let (_dir, store) = scratch_store();
        let store = Mutex::new(store);
        let defaults: [(&str, PanelInterval, u64); 5] = [
            (
                "containers",
                PanelInterval::Containers,
                containers::POLL_INTERVAL_SECS,
            ),
            (
                "neon + sentry",
                PanelInterval::UsageProviders,
                usage::PROVIDER_POLL_INTERVAL_SECS,
            ),
            (
                "azure cost",
                PanelInterval::AzureCost,
                azurecost::POLL_INTERVAL.as_secs(),
            ),
            // Crons has no constant of its own: it shared the Sentry read's
            // hour before it had a setting, and its default still is that hour.
            (
                "sentry crons",
                PanelInterval::Crons,
                usage::PROVIDER_POLL_INTERVAL_SECS,
            ),
            (
                "plan usage",
                PanelInterval::PlanUsage,
                plan_usage::RECOMMENDED_POLL_INTERVAL_SECS,
            ),
        ];
        for (panel, kind, constant) in defaults {
            assert_eq!(
                panel_interval(&store, kind),
                std::time::Duration::from_secs(constant),
                "{panel}: an unconfigured store must poll on this crate's constant"
            );
        }
    }

    /// The whole of #301: the cadence is read from the store on the pass that
    /// is about to sleep, so an edit applies on the next tick.
    ///
    /// Asserting only the first read would pass against the bug this fixes — a
    /// `tokio::time::interval` built once at spawn returns the right number
    /// too, and then goes on returning it for up to four hours after the
    /// operator changed it. So every panel is read, changed, and read again.
    #[test]
    fn a_changed_panel_interval_is_picked_up_on_the_next_pass() {
        let (_dir, store) = scratch_store();
        let store = Mutex::new(store);

        for panel in PanelInterval::ALL {
            let before = panel_interval(&store, panel);
            // Default plus floor: above the floor and above the default, for
            // every panel, by construction — so neither a clamp nor a loop that
            // ignored the store could produce this number by accident. (Twice
            // the floor is not enough: on Containers that is exactly the 10s
            // default, and the assertion below would hold against the bug.)
            let spec = panel.spec();
            let asked = spec.default_secs + spec.floor_secs;
            assert_ne!(
                before,
                std::time::Duration::from_secs(u64::from(asked)),
                "{panel:?}: the new cadence must differ from the old one for this to prove anything"
            );

            let outcome = store
                .lock()
                .expect("store poisoned")
                .settings_mut()
                .set_panel_interval_secs(panel, asked);
            assert_eq!(outcome, ClampOutcome::Applied(asked), "{panel:?}");

            assert_eq!(
                panel_interval(&store, panel),
                std::time::Duration::from_secs(u64::from(asked)),
                "{panel:?}: read from the store each pass, never captured at spawn"
            );
        }
    }

    /// The floor is enforced where the loop reads it, not only where Settings
    /// writes it.
    ///
    /// `panel_intervals` is a plain map in `store.json`, so a hand-edited `1`
    /// reaches the loop without going through the setter at all — and on the
    /// Azure row a bypassed floor is a spend decision, not a tidiness one. This
    /// pins that what the loop sleeps on has been through the clamp.
    #[test]
    fn a_hand_edited_sub_floor_cadence_is_floored_where_the_loop_reads_it() {
        let (_dir, store) = scratch_store();
        let store = Mutex::new(store);

        for panel in PanelInterval::ALL {
            store
                .lock()
                .expect("store poisoned")
                .settings_mut()
                .panel_intervals
                .insert(panel.spec().key.to_owned(), 1);
            assert_eq!(
                panel_interval(&store, panel),
                std::time::Duration::from_secs(u64::from(panel.spec().floor_secs)),
                "{panel:?}: a below-floor store value must not reach the loop"
            );
        }
    }

    // MARK: view_for

    #[test]
    fn live_when_a_snapshot_exists_and_the_last_poll_succeeded() {
        let s = state_with(Some(fixture()), None, Some(Instant::now()));
        let vm = view_for(&s);
        assert_eq!(vm["connection"]["state"], "live");
        assert_eq!(
            vm["connection"]["color"],
            viewmodel::color::hex(viewmodel::color::GREEN)
        );
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
    /// This asserted the opposite until ubu-01 went down during the
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
        assert_eq!(
            vm["connection"]["color"],
            viewmodel::color::hex(viewmodel::color::RED)
        );

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
        assert_eq!(
            vm["connection"]["color"],
            viewmodel::color::hex(viewmodel::color::RED)
        );
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
        assert_eq!(
            vm["connection"]["color"],
            viewmodel::color::hex(viewmodel::color::AMBER)
        );
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
        assert_eq!(
            vm["connection"]["color"],
            viewmodel::color::hex(viewmodel::color::RED)
        );
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

    /// (#489) The version rides the same lifecycle as `sampler_stale`: unknown
    /// until a health read lands, kept through a failed one, and overwritten —
    /// `None` included — by the next that succeeds.
    #[test]
    fn the_hosts_version_follows_the_health_poll_and_survives_a_failed_one() {
        let mut s = live_state();
        assert_eq!(s.version, None, "nobody has told us yet");

        let health = |version: Option<&str>| wire::Health {
            status: "ok".into(),
            hostname: "ubu-01".into(),
            version: version.map(str::to_owned),
            sample_age_seconds: None,
            sampler_stale: None,
        };
        record_health(&mut s, Ok(health(Some("2026.10.13"))));
        assert_eq!(s.version.as_deref(), Some("2026.10.13"));

        record_health(&mut s, Err(AgentError::Unreachable("blip".into())));
        assert_eq!(
            s.version.as_deref(),
            Some("2026.10.13"),
            "a request we could not make is not evidence the host changed"
        );

        // An agent rolled back to a build that cannot name itself no longer has
        // the version we last heard.
        record_health(&mut s, Ok(health(None)));
        assert_eq!(s.version, None);
    }

    /// (#489) Each outcome of the lookup becomes its own view state; only a
    /// verified feed is a verdict, nothing published is neutral, and every other
    /// error carries `user_message()` and nothing the transport said.
    #[test]
    fn an_agent_release_lookup_maps_to_one_state_per_outcome() {
        use viewmodel::agent_release::Latest;
        assert_eq!(
            agent_release_outcome(Ok(agentrelease::Latest {
                version: "2026.11.2".into()
            })),
            Latest::Published("2026.11.2".into())
        );
        assert_eq!(
            agent_release_outcome(Err(agentrelease::Error::NotPublished)),
            Latest::NonePublished
        );
        for err in [
            agentrelease::Error::Unreachable,
            agentrelease::Error::Status(503),
            agentrelease::Error::Unsigned,
            agentrelease::Error::SignatureRejected,
            agentrelease::Error::Malformed,
            agentrelease::Error::TrustSet,
        ] {
            assert_eq!(
                agent_release_outcome(Err(err.clone())),
                Latest::Failed(err.user_message()),
                "{err:?}"
            );
        }
    }

    /// (#489) The cadence is a fixed hour — not an operator setting — and sits
    /// well above the 10s health poll it is not a part of.
    #[test]
    fn the_agent_release_check_is_hourly() {
        assert_eq!(AGENT_RELEASE_CHECK_INTERVAL.as_secs(), 3600);
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
    /// minutes. It is also not any panel's cadence — this is a
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
    /// original side debounces.
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
    /// original service starts in `.connecting` and only flips on the streak.
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

    /// Hiding volumes re-derives the reservation from what is left: a card
    /// padded for mounts nobody can see would be dead space with no reason.
    #[test]
    fn hidden_volumes_leave_the_cards_and_the_reservation() {
        let cards = vec![
            json!({"id": "local", "volumes": [{"mount": "/"}, {"mount": "/a"}, {"mount": "/b"}]}),
            json!({"id": "other", "volumes": [{"mount": "/"}]}),
        ];
        let mut payload = stacked_payload(cards, 1, 2732.0);
        assert_eq!(payload["volumeSlots"], 3);

        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open_in(dir.path(), false).unwrap();
        volumes::set_hidden(&mut store, volumes::LOCAL, "/a", true).unwrap();
        volumes::set_hidden(&mut store, volumes::LOCAL, "/b", true).unwrap();
        hide_volumes(&mut payload, &volumes::Hidden::from_store(&store));

        assert_eq!(payload["hosts"][0]["volumes"], json!([{"mount": "/"}]));
        assert_eq!(payload["volumeSlots"], 1);
    }

    /// End to end through the dashboard: a full volume the operator hid names
    /// nothing, colours nothing and raises no attention.
    #[test]
    fn a_hidden_full_volume_never_reaches_the_dashboard() {
        let mut snapshot: wire::Snapshot =
            serde_json::from_str(include_str!("../../../tests/fixtures/snapshot_mac.json"))
                .unwrap();
        let volume = |mount: &str, used_gb: f64| wire::Volume {
            mount: mount.into(),
            used_gb,
            total_gb: 100.0,
            fstype: None,
        };
        snapshot.volumes = vec![volume("/", 41.0), volume("/srv", 97.0)];
        let mut card = host_card(
            "studio",
            &snapshot,
            &HostHistories::new(),
            &Connection::Live,
        );
        card["id"] = json!(volumes::LOCAL);

        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open_in(dir.path(), false).unwrap();
        let host_row = |store: &Store| {
            let mut payload = stacked_payload(vec![card.clone()], 0, 1200.0);
            apply_machine_preferences(
                &mut payload,
                &volumes::Hidden::from_store(store),
                &store.settings().machine_alerts,
            );
            payload["id"] = json!("hosts");
            let view = dashboard::view(&dashboard::default_layout(), &[payload]);
            let hosts = view["sources"]
                .as_array()
                .unwrap()
                .iter()
                .find(|source| source["id"] == "hosts")
                .unwrap()
                .clone();
            hosts["rows"][0].clone()
        };

        let shown = host_row(&store);
        assert_eq!(shown["value"], "/srv at 97%", "control: visible, it alerts");
        assert_eq!(shown["attention"], true);

        volumes::set_hidden(&mut store, volumes::LOCAL, "/srv", true).unwrap();
        let hidden = host_row(&store);
        assert_eq!(hidden["attention"], false);
        assert_ne!(hidden["value"], "/srv at 97%");
        let disk = hidden["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|metric| metric["header"] == "Disk")
            .unwrap();
        assert_eq!(disk["label"], "/");
        assert_eq!(hidden["hiddenVolumes"], json!(["/srv"]));
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

    /// The case this exists for: a 10-core Mac beside the 36-core ubu-01.
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

    /// The core row span was persisted and editable but never reached the card
    /// — `host_card` hardcoded the default. It arrives through the same rewrite
    /// that carries the shared height, from the tile's `look`.
    #[test]
    fn the_core_row_span_preference_reaches_the_block() {
        let payload = cockpit_payload(CockpitInput {
            cards: core_cards(&[8]),
            remote_count: 0,
            available: 1000.0,
            look: HostLook {
                overflow: HostOverflowMode::Stack,
                core_row_span: 3,
            },
        });
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
        let cards = tab_cards(&["mac-studio", "ubu-01", "nuc-spare"]);

        let tabbed = tabbed_payload(cards.clone(), 2, 1000.0);
        assert_eq!(tabbed["hostColumns"], 1, "1000pt cannot pair 900pt cards");
        let tabs = tabbed["hostTabs"]["tabs"].as_array().expect("tabs");
        // One per card, in payload order, so this machine leads the bar exactly
        // as it leads the grid.
        assert_eq!(
            tabs.iter()
                .map(|tab| tab["label"].as_str().expect("label"))
                .collect::<Vec<_>>(),
            vec!["mac-studio", "ubu-01", "nuc-spare"]
        );
        assert_eq!(tabs[1]["id"], "ubu-01");
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
        let cards = tab_cards(&["mac-studio", "ubu-01"]);
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
    fn the_payload_carries_the_card_sizing_and_none_of_the_arrangement() {
        let vm = stacked_payload(vec![], 0, 1000.0);
        assert_eq!(vm["hostCardMinWidth"], 900.0);
        // Nothing arranges panels any more (#647), and the gaps are the
        // dashboard's. The payload is exactly the host cards' sizing, the
        // columns each panel affords the window, the overview's empty line and
        // its load-failure sentence.
        let mut keys: Vec<&str> = vm
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "empty",
                "hostCardMinWidth",
                "hostColumns",
                "hostTabs",
                "hosts",
                "overviewLoadFailed",
                "panelColumns",
                "volumeSlots",
            ]
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
            Host::new("ubu-01", "10.0.0.2"),
        ];
        let names: Vec<&str> = display_order(&hosts)
            .iter()
            .map(|h| h.name.as_str())
            .collect();
        assert_eq!(names, vec!["mac-mini", "ubu-01", "zed"]);
    }

    // MARK: SOLADOR_SEED_HOST

    #[test]
    fn a_seed_string_parses_the_documented_way() {
        assert_eq!(
            parse_seed_host("ubu-01|100.100.100.100|9000|tok"),
            Some(SeedHost {
                name: "ubu-01".into(),
                address: "100.100.100.100".into(),
                port: 9000,
                token: "tok".into(),
            })
        );
        // Port and token are both optional, and an unparseable port falls
        // back to the agent default rather than rejecting the whole seed.
        assert_eq!(
            parse_seed_host("ubu-01|100.100.100.100"),
            Some(SeedHost {
                name: "ubu-01".into(),
                address: "100.100.100.100".into(),
                port: store::DEFAULT_AGENT_PORT,
                token: String::new(),
            })
        );
        assert_eq!(
            parse_seed_host("ubu-01|100.100.100.100|not-a-port|tok")
                .expect("seed")
                .port,
            store::DEFAULT_AGENT_PORT
        );
        // An empty port field keeps the empty token field addressable -- the
        // The original split does not omit empty subsequences, and neither does this.
        assert_eq!(
            parse_seed_host("ubu-01|100.100.100.100||tok")
                .expect("seed")
                .token,
            "tok"
        );
    }

    #[test]
    fn a_seed_string_missing_a_name_or_address_is_rejected() {
        for raw in ["", "just-a-name", "|100.100.100.100", "ubu-01|", "|"] {
            assert_eq!(parse_seed_host(raw), None, "raw {raw:?}");
        }
    }

    fn scratch_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let store = Store::open_in(dir.path(), false).expect("open");
        (dir, store)
    }

    /// Settings' Add Host and the command-line hand-off both go through this:
    /// the row is saved pinned, the token is keyed by the minted id, and the
    /// token never reaches the store file.
    #[test]
    fn adding_a_host_saves_it_pinned_with_its_token_beside_the_file() {
        let (dir, mut store) = scratch_store();
        let credentials = MemoryCredentialStore::new();
        let pin = certpin::format(&[0x33; certpin::DIGEST_LEN]);
        let host = add_host_connection(
            &mut store,
            &credentials,
            (" agent-1 ", " 192.168.1.2 ", 7878),
            "lan-token",
            Some(pin.clone()),
        )
        .expect("added");
        assert_eq!(
            (host.name.as_str(), host.address.as_str()),
            ("agent-1", "192.168.1.2")
        );
        let reopened = Store::open_in(dir.path(), false).unwrap();
        assert_eq!(reopened.host(host.id).unwrap().tls_fingerprint, Some(pin));
        assert_eq!(
            credentials
                .secret(SecretKey::HostToken(host.id))
                .unwrap()
                .as_deref(),
            Some("lan-token")
        );
        assert!(!std::fs::read_to_string(store.path())
            .unwrap()
            .contains("lan-token"));
    }

    /// A row the file does not hold must not linger in memory, where the next
    /// unrelated save would persist it after all; and its token is not stored.
    #[test]
    fn a_failed_add_takes_the_row_back_out_and_stores_no_token() {
        let (_dir, mut store) = scratch_store();
        let credentials = MemoryCredentialStore::new();
        store.save().unwrap();
        std::fs::remove_file(store.path()).unwrap();
        std::fs::create_dir(store.path()).unwrap();
        let result = add_host_connection(
            &mut store,
            &credentials,
            ("ubu", "192.168.1.2", 7878),
            "lan-token",
            None,
        );
        assert!(result.unwrap_err().starts_with("Failed:"));
        assert!(store.hosts().is_empty());
        assert!(credentials.accounts().is_empty());
    }

    #[test]
    fn adding_a_host_without_a_name_or_address_changes_nothing() {
        let (_dir, mut store) = scratch_store();
        let credentials = MemoryCredentialStore::new();
        for (name, address) in [("", "192.168.1.2"), ("ubu", "  ")] {
            assert!(add_host_connection(
                &mut store,
                &credentials,
                (name, address, 7878),
                "t",
                None
            )
            .is_err());
        }
        assert!(store.hosts().is_empty());
        assert!(credentials.accounts().is_empty());
    }

    #[test]
    fn host_connection_edits_keep_the_existing_token_until_explicitly_replaced() {
        let (dir, mut store) = scratch_store();
        let credentials = MemoryCredentialStore::new();
        let host = Host::new("before", "old.example");
        let id = host.id;
        store.upsert_host(host);
        credentials
            .set_secret(SecretKey::HostToken(id), "old-token")
            .unwrap();
        assert_eq!(
            save_host_connection(
                &mut store,
                &credentials,
                id,
                ("after", "new.example", "9000"),
                "",
                None,
            ),
            Ok(false)
        );
        assert_eq!(
            credentials
                .secret(SecretKey::HostToken(id))
                .unwrap()
                .as_deref(),
            Some("old-token")
        );
        let reopened = Store::open_in(dir.path(), false).unwrap();
        assert_eq!(reopened.host(id).unwrap().address, "new.example");
        assert_eq!(reopened.hosts().len(), 1);
        assert_eq!(
            save_host_connection(
                &mut store,
                &credentials,
                id,
                ("after", "new.example", "9000"),
                "replacement-token",
                None,
            ),
            Ok(true)
        );
        assert_eq!(
            credentials
                .secret(SecretKey::HostToken(id))
                .unwrap()
                .as_deref(),
            Some("replacement-token")
        );
        assert!(!std::fs::read_to_string(store.path())
            .unwrap()
            .contains("replacement-token"));
    }

    /// The pin is only ever changed by the Trust path (#448): an ordinary edit
    /// — a rename, a new address — leaves it alone, and one that carries a
    /// confirmed fingerprint replaces it (Re-pair), in the canonical form.
    #[test]
    fn a_host_edit_leaves_its_pin_alone_and_a_repair_replaces_it() {
        let (dir, mut store) = scratch_store();
        let credentials = MemoryCredentialStore::new();
        let mut host = Host::new("before", "old.example");
        let first = certpin::format(&[0x11; certpin::DIGEST_LEN]);
        let second = certpin::format(&[0x22; certpin::DIGEST_LEN]);
        host.tls_fingerprint = Some(first.clone());
        let id = host.id;
        store.upsert_host(host);

        save_host_connection(
            &mut store,
            &credentials,
            id,
            ("renamed", "new.example", "9000"),
            "",
            None,
        )
        .expect("plain edit");
        assert_eq!(store.host(id).unwrap().tls_fingerprint, Some(first));

        // Re-pair: the operator confirmed a new fingerprint (any notation).
        save_host_connection(
            &mut store,
            &credentials,
            id,
            ("renamed", "new.example", "9000"),
            "",
            Some(&second.to_lowercase()),
        )
        .expect("re-pair");
        let reopened = Store::open_in(dir.path(), false).unwrap();
        assert_eq!(reopened.host(id).unwrap().tls_fingerprint, Some(second));
        assert!(reopened
            .host(id)
            .unwrap()
            .base_url()
            .starts_with("https://"));
    }

    /// Something that is not a fingerprint is refused before anything is
    /// stored — it must not become a pin that matches nothing and looks like
    /// one that matters — and the previous host is untouched.
    #[test]
    fn a_pin_that_is_not_a_fingerprint_is_refused_and_changes_nothing() {
        let (_dir, mut store) = scratch_store();
        let credentials = MemoryCredentialStore::new();
        let host = Host::new("before", "old.example");
        let id = host.id;
        store.upsert_host(host.clone());

        let result = save_host_connection(
            &mut store,
            &credentials,
            id,
            ("after", "new.example", "9000"),
            "",
            Some("not a fingerprint"),
        );

        assert!(result.is_err());
        assert_eq!(store.host(id), Some(&host));
    }

    #[test]
    fn a_failed_host_connection_write_preserves_the_old_host_and_credential() {
        let (_dir, mut store) = scratch_store();
        let credentials = MemoryCredentialStore::new();
        let original = Host::new("before", "old.example");
        let id = original.id;
        store.upsert_host(original.clone());
        store.save().unwrap();
        credentials
            .set_secret(SecretKey::HostToken(id), "old-token")
            .unwrap();
        std::fs::remove_file(store.path()).unwrap();
        std::fs::create_dir(store.path()).unwrap();
        assert!(save_host_connection(
            &mut store,
            &credentials,
            id,
            ("after", "new.example", "9000"),
            "replacement-token",
            None,
        )
        .is_err());
        assert_eq!(store.host(id), Some(&original));
        assert_eq!(
            credentials
                .secret(SecretKey::HostToken(id))
                .unwrap()
                .as_deref(),
            Some("old-token")
        );
    }

    #[test]
    fn seeding_adds_the_host_and_files_its_token_under_the_new_id() {
        let (_dir, mut store) = scratch_store();
        let credentials = MemoryCredentialStore::new();

        let id = seed_from_env(
            &mut store,
            &credentials,
            Some("ubu-01|100.100.100.100|9000|agent-token"),
        )
        .expect("seed")
        .expect("a host was added");

        assert_eq!(store.hosts().len(), 1);
        let host = &store.hosts()[0];
        assert_eq!(host.name, "ubu-01");
        assert_eq!(host.address, "100.100.100.100");
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

    /// The no-op that makes `SOLADOR_SEED_HOST` safe to leave exported:
    /// relaunching must not accumulate duplicate hosts. Address, not name, is
    /// the identity — the name is the field the user edits.
    #[test]
    fn seeding_an_address_that_is_already_configured_is_a_no_op() {
        let (_dir, mut store) = scratch_store();
        let credentials = MemoryCredentialStore::new();
        let seed = "ubu-01|100.100.100.100|7878|agent-token";

        seed_from_env(&mut store, &credentials, Some(seed)).expect("first seed");
        let first = store.hosts()[0].clone();

        // Same address, different name and token: still a no-op.
        assert_eq!(
            seed_from_env(
                &mut store,
                &credentials,
                Some("renamed|100.100.100.100|7878|other-token"),
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
        seed_from_env(&mut store, &credentials, Some("ubu-01|100.100.100.100")).expect("seed");
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
            let mut store = Store::open_in(dir.path(), false).expect("open");
            seed_from_env(&mut store, &credentials, Some("ubu-01|100.100.100.100")).expect("seed");
        }
        let reopened = Store::open_in(dir.path(), false).expect("reopen");
        assert_eq!(reopened.hosts().len(), 1);
        assert_eq!(reopened.hosts()[0].name, "ubu-01");
    }

    // MARK: - attributing a newly tracked repo
    //
    // `settings_add_repo` is a Tauri command and cannot be called from a test,
    // so these exercise the two lines it is made of — `settings::new_repo`
    // followed by `Store::upsert_repo` — against a real store file.

    /// A scratch store carrying `count` GitHub accounts, each pointing at its
    /// own credential item exactly as `settings::validated_account` mints them.
    fn store_with_accounts(count: usize) -> (tempfile::TempDir, Store) {
        let (dir, mut store) = scratch_store();
        for n in 0..count {
            let account = VendorAccount::new(VendorKind::GitHub, format!("account-{n}"), "");
            store.upsert_account(VendorAccount {
                secret_account: SecretKey::VendorToken(account.id).account(),
                ..account
            });
        }
        (dir, store)
    }

    /// `settings_add_repo`'s body, minus the lock and the status line.
    fn add_repo(store: &mut Store, slug: &str) {
        // Read before the mutable borrow, which is also why the command holds
        // one lock rather than two.
        let repo = settings::new_repo(slug, store.accounts());
        store.upsert_repo(repo);
    }

    /// With one account there is nothing to ask: the repo provably belongs to
    /// it, and a picker with a single option is a question with one answer.
    #[test]
    fn a_new_repo_is_attributed_automatically_when_there_is_only_one_account() {
        let (_dir, mut store) = store_with_accounts(1);
        let only = store.accounts()[0].id;
        add_repo(&mut store, "acme/new");
        assert_eq!(store.repos().last().expect("a repo").account_id, Some(only));
    }

    /// The load-bearing one. With two accounts there is no correct default —
    /// not the first, not the most recently added, not the enabled one — so the
    /// repo is stored unattributed and the Portfolio row's picker asks.
    /// Inventing an owner here would silently undo #290, which is what keeps a
    /// repo from being fetched with somebody else's token.
    #[test]
    fn a_new_repo_with_two_accounts_is_not_guessed() {
        let (_dir, mut store) = store_with_accounts(2);
        add_repo(&mut store, "acme/new");
        assert_eq!(store.repos().last().expect("a repo").account_id, None);
    }

    /// A store with no account at all is the v1 shape, and `None` is what
    /// `plan_github_pass` reads as "the v1 credential's". Attributing here
    /// would have nothing to attribute to.
    #[test]
    fn a_new_repo_in_a_store_with_no_accounts_stays_unattributed() {
        let (_dir, mut store) = store_with_accounts(0);
        add_repo(&mut store, "acme/new");
        assert_eq!(store.repos().last().expect("a repo").account_id, None);
    }

    /// …and it survives the round-trip: attribution is a decision recorded in
    /// the file at add time, not one re-derived on every launch from however
    /// many accounts happen to exist then.
    #[test]
    fn an_automatic_attribution_is_persisted_rather_than_re_derived() {
        let (dir, mut store) = store_with_accounts(1);
        let only = store.accounts()[0].id;
        add_repo(&mut store, "acme/new");
        store.save().expect("save");

        let reopened = Store::open_in(dir.path(), false).expect("reopen");
        assert_eq!(
            reopened.repos().last().expect("a repo").account_id,
            Some(only)
        );
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
                account: "solador-secrets".to_owned(),
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
            github::runners_view(state, &[], panel::now_unix()),
        )
    }

    fn populated_github_state() -> GitHubState {
        github::fixture_state(github::now_utc())
    }

    /// A v1 store: a portfolio and no account at all.
    fn v1_repos() -> Vec<TrackedRepo> {
        vec![TrackedRepo::new("acme/widget")]
    }

    /// One GitHub account owning the repos it is handed.
    fn account_owning(label: &str, slugs: &[&str]) -> (VendorAccount, Vec<TrackedRepo>) {
        let account = VendorAccount::new(
            VendorKind::GitHub,
            label,
            // Every account created after the v1 migration reads its own
            // `vendor-<uuid>` item; this is that spelling.
            String::new(),
        );
        let account = VendorAccount {
            secret_account: SecretKey::VendorToken(account.id).account(),
            ..account
        };
        let repos = slugs
            .iter()
            .map(|slug| TrackedRepo {
                account_id: Some(account.id),
                ..TrackedRepo::new(*slug)
            })
            .collect();
        (account, repos)
    }

    #[test]
    fn a_github_token_the_store_hands_over_polls_on_with_the_panels_untouched() {
        let mut state = populated_github_state();
        let (repos_before, runners_before) = github_views(&state);

        let pass = plan_github_pass(
            &store_holding(SecretKey::GitHubAccessToken, "ghp_stored"),
            &[],
            &v1_repos(),
            "",
        );
        apply_pass_credential(pass.credential, &mut state);

        assert_eq!(pass.credential, PassCredential::Present);
        assert_eq!(
            pass.fetches
                .iter()
                .map(|fetch| fetch.token.as_str())
                .collect::<Vec<_>>(),
            vec!["ghp_stored"]
        );
        let (repos_after, runners_after) = github_views(&state);
        assert_eq!(repos_after, repos_before, "a good read changes nothing");
        assert_eq!(runners_after, runners_before);
    }

    #[test]
    fn no_stored_github_token_clears_the_panels_and_asks_for_one() {
        let mut state = populated_github_state();
        let pass = plan_github_pass(&MemoryCredentialStore::new(), &[], &v1_repos(), "");
        apply_pass_credential(pass.credential, &mut state);

        assert!(pass.fetches.is_empty(), "there is nothing to poll with");
        assert!(
            pass.reports.is_empty(),
            "a v1 store has no account to blame — the panel's own setup line is the whole story"
        );
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

        let pass = plan_github_pass(&UnreadableCredentialStore, &[], &v1_repos(), "");
        apply_pass_credential(pass.credential, &mut state);

        assert!(pass.fetches.is_empty(), "there is nothing to poll with");
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

    // MARK: one credential per account (#290)

    /// The migrated account keeps reading the pre-existing item rather than a
    /// `vendor-<uuid>` one — no keychain item is ever renamed, because a
    /// renamed item orphans the credential in it.
    #[test]
    fn the_migrated_account_reads_the_item_the_v1_store_already_had() {
        let account = VendorAccount::new(
            VendorKind::GitHub,
            "GitHub",
            SecretKey::GitHubAccessToken.account(),
        );
        let repos = vec![TrackedRepo {
            account_id: Some(account.id),
            ..TrackedRepo::new("acme/widget")
        }];
        let pass = plan_github_pass(
            &store_holding(SecretKey::GitHubAccessToken, "ghp_migrated"),
            &[account],
            &repos,
            "",
        );
        assert_eq!(pass.credential, PassCredential::Present);
        assert_eq!(pass.fetches.len(), 1);
        assert_eq!(pass.fetches[0].token, "ghp_migrated");
        assert_eq!(pass.fetches[0].repos.len(), 1);
    }

    /// Each account fetches with **its own** token, and a repo is only ever in
    /// the fetch list of the account that owns it.
    #[test]
    fn every_account_fetches_its_own_repos_with_its_own_token() {
        let (work, work_repos) = account_owning("work", &["work/one"]);
        let (personal, personal_repos) = account_owning("personal", &["personal/two"]);
        let credentials = MemoryCredentialStore::new();
        credentials
            .set_secret(SecretKey::VendorToken(work.id), "ghp_work")
            .expect("write");
        credentials
            .set_secret(SecretKey::VendorToken(personal.id), "ghp_personal")
            .expect("write");
        let repos = [work_repos, personal_repos].concat();

        let pass = plan_github_pass(&credentials, &[work, personal], &repos, "");

        let fetched: Vec<(&str, Vec<&str>)> = pass
            .fetches
            .iter()
            .map(|fetch| {
                (
                    fetch.token.as_str(),
                    fetch.repos.iter().map(|(slug, _)| slug.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            fetched,
            vec![
                ("ghp_work", vec!["work/one"]),
                ("ghp_personal", vec!["personal/two"]),
            ],
            "no repo is ever fetched with another account's token"
        );
        assert!(pass.reports.is_empty(), "nothing to report: {pass:?}");
    }

    /// One account's missing credential is that account's failure. The other
    /// still fetches, and the panel still renders its rows.
    #[test]
    fn one_account_without_a_token_does_not_stop_the_others() {
        let (work, work_repos) = account_owning("work", &["work/one"]);
        let (personal, personal_repos) = account_owning("personal", &["personal/two"]);
        let personal_id = personal.id;
        let credentials = store_holding(SecretKey::VendorToken(work.id), "ghp_work");
        let repos = [work_repos, personal_repos].concat();

        let pass = plan_github_pass(&credentials, &[work, personal], &repos, "");

        assert_eq!(
            pass.credential,
            PassCredential::Present,
            "one good token is enough"
        );
        assert_eq!(pass.fetches.len(), 1);
        assert_eq!(pass.fetches[0].label, "work");
        assert!(
            matches!(
                pass.reports.as_slice(),
                [github::AccountResult::Failed { account, label, reason, slugs }]
                    if *account == Some(personal_id)
                        && label == "personal"
                        && reason == ACCOUNT_TOKEN_MISSING_MESSAGE
                        && slugs == &["personal/two"]
            ),
            "{pass:?}"
        );
    }

    /// A disabled account is not polled — and its repos are still named, so
    /// they do not simply vanish off the panel.
    #[test]
    fn a_disabled_accounts_repos_are_reported_rather_than_dropped() {
        let (account, repos) = account_owning("work", &["work/one"]);
        let account = VendorAccount {
            enabled: false,
            ..account
        };
        let credentials = store_holding(SecretKey::VendorToken(account.id), "ghp_work");

        let pass = plan_github_pass(&credentials, &[account], &repos, "");

        assert!(pass.fetches.is_empty(), "a disabled account is not polled");
        assert_eq!(
            pass.credential,
            PassCredential::Unlooked,
            "nothing was read, so the panels claim nothing"
        );
        assert!(matches!(
            pass.reports.as_slice(),
            [github::AccountResult::Failed { label, reason, slugs, .. }]
                if label == "work" && reason == ACCOUNT_DISABLED_MESSAGE && slugs == &["work/one"]
        ));
    }

    /// An account naming a credential item this build cannot resolve is
    /// refused, not resolved to whatever is nearest. Reading the *other*
    /// account's item is the "somebody else's token" this whole change exists
    /// to make impossible.
    #[test]
    fn an_account_pointing_at_an_unknown_credential_item_is_refused() {
        let (other, _) = account_owning("other", &[]);
        let (account, repos) = account_owning("work", &["work/one"]);
        // Hand-edited to point at another account's keychain item.
        let account = VendorAccount {
            secret_account: SecretKey::VendorToken(other.id).account(),
            ..account
        };
        let credentials = store_holding(SecretKey::VendorToken(other.id), "ghp_other");

        assert_eq!(account_secret_key(&account), None);
        let pass = plan_github_pass(&credentials, &[account], &repos, "");
        assert!(pass.fetches.is_empty(), "no token is borrowed: {pass:?}");
        assert!(matches!(
            pass.reports.as_slice(),
            [github::AccountResult::Failed { reason, .. }]
                if reason == ACCOUNT_CREDENTIAL_UNKNOWN_MESSAGE
        ));
    }

    /// A repo naming an account that no longer exists is reported, never
    /// re-homed on whichever account happens to remain.
    #[test]
    fn a_repo_naming_a_removed_account_is_unattributed() {
        let (account, repos) = account_owning("work", &["work/one"]);
        let orphan = TrackedRepo {
            account_id: Some(Uuid::from_u128(999)),
            ..TrackedRepo::new("gone/orphan")
        };
        let credentials = store_holding(SecretKey::VendorToken(account.id), "ghp_work");

        let pass = plan_github_pass(
            &credentials,
            &[account],
            &[repos, vec![orphan]].concat(),
            "",
        );

        assert_eq!(
            pass.fetches[0]
                .repos
                .iter()
                .map(|(slug, _)| slug.as_str())
                .collect::<Vec<_>>(),
            vec!["work/one"],
            "the orphan is not swept into the surviving account's fetch"
        );
        assert!(pass.reports.contains(&github::AccountResult::Unattributed {
            slugs: vec!["gone/orphan".to_owned()],
        }));
    }

    /// A disabled *repo* is not on the board, so it is neither fetched nor
    /// reported as missing an account.
    #[test]
    fn a_disabled_repo_is_not_reported_as_unattributed() {
        let (account, _) = account_owning("work", &[]);
        let credentials = store_holding(SecretKey::VendorToken(account.id), "ghp_work");
        let repos = vec![TrackedRepo {
            enabled: false,
            ..TrackedRepo::new("acme/retired")
        }];
        let pass = plan_github_pass(&credentials, &[account], &repos, "");
        assert!(pass.reports.is_empty(), "{pass:?}");
    }

    /// With no account configured, the v1 credential is the one identity there
    /// is and every repo naming no account belongs to it — the same proof the
    /// store's own v1 → v2 migration relies on.
    #[test]
    fn a_store_with_no_accounts_polls_the_v1_credential_for_every_repo() {
        let pass = plan_github_pass(
            &store_holding(SecretKey::GitHubAccessToken, "ghp_stored"),
            &[],
            &v1_repos(),
            "",
        );
        assert_eq!(pass.fetches.len(), 1);
        assert_eq!(pass.fetches[0].account, None);
        assert_eq!(
            pass.fetches[0]
                .repos
                .iter()
                .map(|(slug, _)| slug.as_str())
                .collect::<Vec<_>>(),
            vec!["acme/widget"]
        );
        assert!(pass.reports.is_empty());
    }

    /// …and once an account exists, a repo that names none is **not** quietly
    /// handed to it. An invented owner is the fabrication rule applied to
    /// configuration; #292 is what asks the operator instead.
    #[test]
    fn an_unattributed_repo_is_not_adopted_by_the_only_account() {
        let (account, repos) = account_owning("work", &["work/one"]);
        let credentials = store_holding(SecretKey::VendorToken(account.id), "ghp_work");
        let repos = [repos, vec![TrackedRepo::new("acme/added-today")]].concat();

        let pass = plan_github_pass(&credentials, &[account], &repos, "");

        assert_eq!(pass.fetches[0].repos.len(), 1, "{pass:?}");
        assert!(pass.reports.contains(&github::AccountResult::Unattributed {
            slugs: vec!["acme/added-today".to_owned()],
        }));
    }

    /// The two halves of #292 meeting the pass. Added under one account the
    /// repo is fetched by it without anyone being asked; added under two it is
    /// reported instead of being handed to either token.
    #[test]
    fn what_the_add_form_attributed_is_what_the_pass_fetches() {
        let (work, _) = account_owning("work", &[]);
        let (personal, _) = account_owning("personal", &[]);
        let credentials = MemoryCredentialStore::new();
        for account in [&work, &personal] {
            credentials
                .set_secret(SecretKey::VendorToken(account.id), "ghp_token")
                .expect("write");
        }

        let one = [work.clone()];
        let pass = plan_github_pass(
            &credentials,
            &one,
            &[settings::new_repo("acme/new", &one)],
            "",
        );
        assert_eq!(
            pass.fetches[0]
                .repos
                .iter()
                .map(|(slug, _)| slug.as_str())
                .collect::<Vec<_>>(),
            vec!["acme/new"],
        );
        assert!(pass.reports.is_empty(), "{pass:?}");

        let two = [work, personal];
        let pass = plan_github_pass(
            &credentials,
            &two,
            &[settings::new_repo("acme/new", &two)],
            "",
        );
        assert!(
            pass.fetches.iter().all(|fetch| fetch.repos.is_empty()),
            "neither token may fetch a repo nobody has attributed: {pass:?}"
        );
        assert!(pass.reports.contains(&github::AccountResult::Unattributed {
            slugs: vec!["acme/new".to_owned()],
        }));
    }

    /// A keychain that would not answer for one account is never "there is no
    /// token" for the panels — unreadable outranks absent, exactly as it does
    /// for the single credential.
    #[test]
    fn an_unreadable_account_credential_outranks_a_missing_one() {
        let (work, work_repos) = account_owning("work", &["work/one"]);
        let (personal, personal_repos) = account_owning("personal", &["personal/two"]);
        let repos = [work_repos, personal_repos].concat();
        // `UnreadableCredentialStore` refuses every read, so `work` reads as
        // unreadable and `personal` never gets the chance to read as absent.
        let pass = plan_github_pass(&UnreadableCredentialStore, &[work, personal], &repos, "");
        assert_eq!(pass.credential, PassCredential::Unreadable);

        let mut state = GitHubState::new();
        apply_pass_credential(pass.credential, &mut state);
        state.apply_repo_accounts(&pass.reports, panel::now_unix());
        let (repos_view, _) = github_views(&state);
        assert_eq!(repos_view["message"]["text"], CREDENTIAL_UNREADABLE_MESSAGE);
    }

    /// A non-GitHub account is never handed to the GitHub client. There is one
    /// vendor today, so this guards the filter rather than an existing bug.
    #[test]
    fn only_github_accounts_are_polled_for_github() {
        let github = VendorAccount::new(VendorKind::GitHub, "work", "vendor-x");
        let identities = github_identities(std::slice::from_ref(&github), "");
        assert_eq!(identities.len(), 1);
        assert!(identities.iter().all(|identity| !identity.is_legacy()));
        assert_eq!(
            github_identities(&[], "").len(),
            1,
            "a store with no account still has the v1 credential"
        );
    }

    // MARK: per-account org selection rides the pass

    #[test]
    fn an_accounts_selected_orgs_ride_its_own_fetch() {
        let (work, work_repos) = account_owning("work", &["work/one"]);
        let work = VendorAccount {
            orgs: Some(vec!["acme".into(), "   ".into(), "beta".into()]),
            ..work
        };
        let credentials = store_holding(SecretKey::VendorToken(work.id), "ghp_work");

        let pass = plan_github_pass(&credentials, std::slice::from_ref(&work), &work_repos, "");

        assert_eq!(pass.fetches.len(), 1);
        assert_eq!(
            pass.fetches[0].orgs,
            vec!["acme", "beta"],
            "trimmed, blank entries dropped — a stray space is not an organization"
        );
        assert!(pass.blocked_orgs.is_empty());
    }

    /// Without this plumb-through, every not-yet-migrated store's Runners
    /// panel would go dark between the v3 upgrade and its migration
    /// completing. Self-eliminating: the migration clears `github_org`.
    #[test]
    fn the_legacy_identity_still_carries_the_unmigrated_global_org() {
        let pass = plan_github_pass(
            &store_holding(SecretKey::GitHubAccessToken, "ghp_stored"),
            &[],
            &v1_repos(),
            "Sassy-Dog",
        );
        assert_eq!(pass.fetches.len(), 1);
        assert_eq!(pass.fetches[0].orgs, vec!["Sassy-Dog"]);
    }

    /// The runners-side sibling of "a disabled account's repos are reported
    /// rather than dropped": a selected org that quietly stops being polled is
    /// the same silence, one panel over.
    #[test]
    fn a_blocked_accounts_orgs_are_reported_rather_than_silently_unpolled() {
        let (work, work_repos) = account_owning("work", &["work/one"]);
        let work = VendorAccount {
            orgs: Some(vec!["acme".into()]),
            enabled: false,
            ..work
        };

        let pass = plan_github_pass(
            &MemoryCredentialStore::new(),
            std::slice::from_ref(&work),
            &work_repos,
            "",
        );

        assert!(pass.fetches.is_empty());
        assert_eq!(
            pass.blocked_orgs,
            vec![BlockedOrg {
                org: "acme".into(),
                label: "work".into(),
                reason: ACCOUNT_DISABLED_MESSAGE.into(),
            }]
        );
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
            tls_fingerprint: None,
        }
    }

    /// A re-pair changes the fingerprint and nothing else: the URL was already
    /// `https://`. If the key ignored the pin the reconcile would keep the old
    /// task — polling with the old client, and the old pin — after the
    /// operator replaced it.
    #[test]
    fn re_pairing_a_host_restarts_its_task_and_only_its_task() {
        let mut old = key(1, "https://10.0.0.1:7878");
        old.tls_fingerprint = Some("AA".into());
        let mut repaired = old.clone();
        repaired.tls_fingerprint = Some("BB".into());
        let other = key(2, "http://10.0.0.2:7878");

        let plan = reconcile(&[old.clone(), other.clone()], &[repaired, other]);

        assert_eq!(plan, vec![None, Some(1)]);
    }

    /// A pairing failure is tagged on the card (#448) so the dashboard can tell
    /// it from the network being down without reading a sentence; an ordinary
    /// failure carries no tag; and recovery clears it with the error.
    #[test]
    fn a_pairing_failure_is_tagged_on_the_card_and_recovery_clears_the_tag() {
        let mut s = live_state();
        for _ in 0..FAILURE_THRESHOLD {
            record_poll(&mut s, Err(AgentError::CertificateChanged), Instant::now());
        }
        let card = view_for(&s);
        assert_eq!(
            card["error"]["kind"],
            viewmodel::card::ERROR_KIND_CERTIFICATE_CHANGED
        );
        assert!(card["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&AgentError::CertificateChanged.user_message()));

        // The same host, now answering plain HTTP.
        for _ in 0..FAILURE_THRESHOLD {
            record_poll(&mut s, Err(AgentError::NoTls), Instant::now());
        }
        assert_eq!(
            view_for(&s)["error"]["kind"],
            viewmodel::card::ERROR_KIND_NO_TLS
        );

        // An unpaired host off loopback and the tailnet (#449): its own kind.
        for _ in 0..FAILURE_THRESHOLD {
            record_poll(&mut s, Err(refused_error()), Instant::now());
        }
        assert_eq!(
            view_for(&s)["error"]["kind"],
            viewmodel::card::ERROR_KIND_PLAIN_HTTP_REFUSED
        );
        assert!(view_for(&s)["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&refused_error().user_message()));

        // An ordinary failure replaces it, and is untagged.
        for _ in 0..FAILURE_THRESHOLD {
            record_poll(
                &mut s,
                Err(AgentError::Unreachable("x".into())),
                Instant::now(),
            );
        }
        assert!(view_for(&s)["error"]["kind"].is_null());

        // Pairing failure again, then the host recovers: no error, no tag.
        for _ in 0..FAILURE_THRESHOLD {
            record_poll(&mut s, Err(AgentError::CertificateChanged), Instant::now());
        }
        record_poll(&mut s, Ok(fixture()), Instant::now());
        let card = view_for(&s);
        assert!(card["error"].is_null(), "{card}");
    }

    /// A refusal to hand to `record_poll`, as the guard would for a LAN host.
    fn refused_error() -> AgentError {
        AgentError::PlainHttpRefused(agentclient::Refused::new(
            Some("agent.lan".into()),
            vec!["192.168.1.20".parse().unwrap()],
        ))
    }

    /// #461: the first refused poll is the only one that logs. Later refused
    /// polls say nothing, and a host that leaves the state and is refused
    /// again is logged again.
    #[test]
    fn a_refusal_logs_once_on_entry_and_again_only_after_it_clears() {
        let mut s = live_state();
        let refused = refused_error();
        let first = refusal_transition(&mut s, Some(&refused));
        let line = first.expect("entering the refused state logs");
        assert!(line.contains(&s.name), "{line}");
        assert!(line.contains("192.168.1.20"), "{line}");
        assert!(line.contains("agent.lan"), "{line}");
        for _ in 0..25 {
            assert_eq!(refusal_transition(&mut s, Some(&refused)), None);
        }
        // A success ends the state; the next refusal is a new entry.
        assert_eq!(refusal_transition(&mut s, None), None);
        assert!(refusal_transition(&mut s, Some(&refused)).is_some());
        // So does a different failure in between.
        let other = AgentError::Unreachable("x".into());
        assert_eq!(refusal_transition(&mut s, Some(&other)), None);
        assert!(refusal_transition(&mut s, Some(&refused)).is_some());
    }

    /// The same, through `record_poll` itself, which is what owns the flag.
    #[test]
    fn record_poll_tracks_the_refused_state_and_leaves_the_card_words_alone() {
        let mut s = live_state();
        record_poll(&mut s, Err(refused_error()), Instant::now());
        assert!(s.plain_refused);
        record_poll(&mut s, Ok(fixture()), Instant::now());
        assert!(!s.plain_refused);
        for _ in 0..FAILURE_THRESHOLD {
            record_poll(&mut s, Err(refused_error()), Instant::now());
        }
        assert!(s.plain_refused);
        assert_eq!(
            s.error_kind,
            Some(viewmodel::card::ERROR_KIND_PLAIN_HTTP_REFUSED)
        );
        assert_eq!(s.error, Some(refused_error().user_message()));
    }

    /// An address-less refusal (a name that resolved to nothing) and a literal
    /// host each read sensibly.
    #[test]
    fn the_log_line_names_a_literal_and_an_empty_answer() {
        let literal = agentclient::Refused::new(None, vec!["10.0.0.5".parse().unwrap()]);
        let line = refusal_log_line("nas", &literal);
        assert!(line.contains("nas") && line.contains("10.0.0.5"), "{line}");
        let empty = agentclient::Refused::new(Some("gone.example".into()), vec![]);
        let line = refusal_log_line("nas", &empty);
        assert!(line.contains("gone.example"), "{line}");
        assert!(line.contains("no addresses"), "{line}");
    }

    /// #461: a real client built with a known token, refused for real, and the
    /// line made from that refusal never holds the token.
    #[tokio::test]
    async fn the_log_line_never_contains_the_token() {
        const TOKEN: &str = "tok-461-do-not-log";
        let client = AgentClient::new("http://192.168.1.20:7878", TOKEN);
        let err = client.health().await.unwrap_err();
        let AgentError::PlainHttpRefused(refused) = &err else {
            panic!("expected a refusal, got {err:?}");
        };
        let line = refusal_log_line("nas", refused);
        assert!(line.contains("192.168.1.20"), "{line}");
        assert!(!line.contains(TOKEN), "{line}");
        assert!(!format!("{err:?}").contains(TOKEN));
    }

    /// The debounce applies to pairing failures like any other: the card keeps
    /// what it had until the streak means something, so the tag cannot appear
    /// ahead of the sentence it describes.
    #[test]
    fn the_tag_is_published_with_the_error_and_not_before() {
        let mut s = live_state();
        for _ in 0..FAILURE_THRESHOLD - 1 {
            record_poll(&mut s, Err(AgentError::CertificateChanged), Instant::now());
        }
        assert_eq!(s.error_kind, None);
        assert_eq!(s.error, None);
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
        assert_eq!(ids, vec![local::CARD_ID, "ubu-01", "mac-mini", "nuc-spare"]);

        // The local card carries the em dashes the shipped one does where a
        // producer cannot answer — this fixture has no memory-pressure level
        // and no GPU — so the Playwright suite exercises that rule against
        // Rust's own output.
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

    /// The showcase is what solador.app and the README show, and its claim is
    /// the product's: a grid where exactly one thing is not right. Held here
    /// rather than eyeballed in a PNG, so a fixture edit that adds a second
    /// anomaly — or loses the first — fails a test instead of shipping a
    /// screenshot that contradicts the copy beside it.
    ///
    /// Attention and colour are separate rules (a running build is amber and
    /// needs no attention), so both are checked: one attention source, and no
    /// amber or red anywhere but the failing repo's row.
    #[test]
    fn the_showcase_dashboard_has_exactly_one_tile_out_of_true() {
        let vm = dump_showcase_dashboard();
        assert_eq!(
            vm,
            dump_showcase_dashboard(),
            "the fixture must not vary per run"
        );

        let attention = vm["attention"].as_array().expect("attention list");
        assert_eq!(attention.len(), 1, "{attention:?}");
        assert_eq!(attention[0]["source"], "ghWorkflows");

        let warm = [
            Value::from(viewmodel::color::hex(viewmodel::color::RED)),
            Value::from(viewmodel::color::hex(viewmodel::color::AMBER)),
        ];
        let flagged: Vec<String> = vm["sources"]
            .as_array()
            .expect("sources")
            .iter()
            .flat_map(|source| {
                source["rows"]
                    .as_array()
                    .expect("rows")
                    .iter()
                    .filter(|row| {
                        row["attention"] == true
                            || warm.contains(&row["color"])
                            || warm.contains(&row["valueColor"])
                    })
                    .map(move |row| format!("{}/{}", source["id"], row["id"]))
            })
            .collect();
        assert_eq!(flagged, vec![r#""ghWorkflows"/"acme/gadget""#]);
    }

    /// The settings fixture must be stable across dumps (ids the Playwright
    /// suite can address) and mixed (both sides of every badge), or the specs
    /// it drives are only ever exercising one branch.
    #[test]
    fn the_settings_dump_is_stable_and_covers_both_sides_of_every_badge() {
        let vm = dump_settings();
        assert_eq!(vm, dump_settings(), "the fixture must not vary per run");

        let rows = vm["hosts"]["rows"].as_array().expect("host rows");
        assert_eq!(rows.len(), 3);
        // (#489) The Playwright suite paints these three: behind (amber), at
        // the newest verified release, and a host that has reported nothing.
        let states: Vec<&str> = rows
            .iter()
            .map(|row| row["agentRelease"]["state"].as_str().expect("state"))
            .collect();
        assert_eq!(states, ["behind", "up-to-date", "host-unknown"]);
        assert_eq!(rows[0]["tokenStored"], true);
        assert_eq!(rows[0]["enabled"], true);
        assert_eq!(rows[1]["tokenStored"], false);
        assert_eq!(rows[1]["enabled"], false);

        assert!(vm["github"].is_null(), "the GitHub tab stays retired");
        assert!(vm["azure"]["secret"].is_null(), "no Azure credential");

        // Both states of the org list, same rule as the account badges below:
        // one account watching an org, one watching none.
        let account_rows = vm["accounts"]["rows"].as_array().expect("account rows");
        assert!(account_rows
            .iter()
            .any(|row| row["orgs"] == serde_json::json!(["acme"])));
        assert!(account_rows
            .iter()
            .any(|row| row["orgs"] == serde_json::json!([])));

        // Both homes a repo can have, for the reason the rules fixture below
        // carries every action: the Playwright suite must not be able to pass
        // against a payload that quietly lost one of them. The Portfolio tab
        // retired into the Accounts tab, so the two readings are "under a
        // card" and "in the unattributed section".
        assert!(account_rows.iter().any(|row| row["repos"]
            .as_array()
            .is_some_and(|repos| !repos.is_empty())));
        assert!(
            !vm["accounts"]["unattributed"]
                .as_array()
                .expect("unattributed rows")
                .is_empty(),
            "the fixture must carry an orphaned repo"
        );
        assert!(vm["portfolio"].is_null(), "the Portfolio tab stays retired");
        assert_eq!(
            vm["accounts"]["accountOptions"]
                .as_array()
                .expect("picker options")
                .len(),
            dump_accounts().len() + 1,
            "every account, plus the unattributed state"
        );
    }

    /// The rules half of the same fixture, and for the same reason: the
    /// Playwright suite must not be able to pass against a payload that
    /// quietly lost the rendering it claims to exercise. Between them these
    /// rows carry every action, both sides of the Collapse-only fields, an
    /// expectation set and unset, an unscoped rule, and a scope naming a host
    /// that is not configured.
    #[test]
    fn the_containers_fixture_covers_every_rule_rendering_the_editor_has() {
        let rows = dump_containers(false)["ruleEditor"]["rows"]
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

    /// The Playwright suite renders the Services tab from this fixture, so a
    /// fixture that quietly lost one of the row's two states would leave that
    /// half of the tab covered by nothing at all.
    #[test]
    fn the_settings_fixture_covers_both_states_of_a_status_vendor_row() {
        let rows = dump_settings()["services"]["rows"]
            .as_array()
            .expect("vendor rows")
            .clone();
        assert!(
            rows.iter().any(|row| row["enabled"] == true),
            "no watched vendor in the fixture"
        );
        assert!(
            rows.iter().any(|row| row["enabled"] == false),
            "no paused vendor in the fixture"
        );
        for row in &rows {
            // Both halves of the component reach the row: the id it is polled
            // by is the identity the delete button addresses, and the label is
            // what the operator recognises.
            assert!(row["id"].as_str().is_some_and(|id| !id.is_empty()), "{row}");
            assert!(
                row["component"].as_str().is_some_and(|c| !c.is_empty()),
                "{row}"
            );
        }
    }

    /// The Playwright suite renders the Accounts tab from this fixture, so it
    /// has to carry both sides of the credential badge *and* both sides of the
    /// removal prompt — a fixture where every account looks alike leaves half
    /// the tab covered by nothing.
    #[test]
    fn the_settings_fixture_covers_both_states_of_an_account_row() {
        let rows = dump_settings()["accounts"]["rows"]
            .as_array()
            .expect("account rows")
            .clone();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter().any(|row| row["stored"] == true),
            "no account with a token in the fixture"
        );
        assert!(
            rows.iter().any(|row| row["stored"] == false),
            "no account without a token in the fixture"
        );
        assert!(
            rows.iter().any(|row| row["removePrompt"].is_string()),
            "no account whose removal would orphan a repo"
        );
        assert!(
            rows.iter().any(|row| row["removePrompt"].is_null()),
            "no account whose removal costs nothing"
        );
    }

    /// The badge reads the item the account *names*, not the one its id would
    /// derive. The account `migrate_v1_to_v2` mints still points at the v1
    /// `github_access_token` item, and probing `vendor-<uuid>` for it would
    /// report "No token" over a token sitting right there.
    #[test]
    fn the_account_badge_reads_the_credential_item_the_account_names() {
        let migrated = VendorAccount::new(
            VendorKind::GitHub,
            "migrated",
            SecretKey::GitHubAccessToken.account(),
        );
        let own = VendorAccount {
            secret_account: SecretKey::VendorToken(Uuid::from_u128(7)).account(),
            id: Uuid::from_u128(7),
            ..VendorAccount::new(VendorKind::GitHub, "own", "")
        };
        let stranger = VendorAccount::new(VendorKind::GitHub, "stranger", "vendor-somebody-else");

        let credentials = MemoryCredentialStore::default();
        credentials
            .set_secret(SecretKey::GitHubAccessToken, "ghp_legacy")
            .expect("store the legacy token");
        credentials
            .set_secret(SecretKey::VendorToken(own.id), "ghp_own")
            .expect("store the account's own token");

        let stored = stored_secrets(
            &credentials,
            &[],
            &[migrated.clone(), own.clone(), stranger.clone()],
        );
        assert!(stored.accounts.contains(&migrated.id));
        assert!(stored.accounts.contains(&own.id));
        assert!(
            !stored.accounts.contains(&stranger.id),
            "an item name this build cannot resolve must not read as a stored token"
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
