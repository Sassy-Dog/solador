//! DevCanopy shell. The frontend receives a finished view-model and paints;
//! all logic lives in `viewmodel`.

use agentclient::{AgentClient, AgentError};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use store::{CredentialStore, Host, KeyringStore, SecretKey, Store, StoreError};
use viewmodel::card::{host_card, pending_card, Connection, HostHistories, Pending};
use viewmodel::cockpit::{host_columns, panel_table, HOST_CARD_MIN_WIDTH, SPACING};

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
}

/// Every watched host, each behind its own lock.
///
/// Per-host locks, not one lock over the list: a poll that is mid-flight to an
/// unreachable host must not be able to hold up the `cockpit` command or any
/// other host's tick. That isolation is the whole point of one task per host.
struct Cockpit {
    hosts: Vec<Arc<Mutex<HostState>>>,
}

/// The whole `(latest, error)` -> view-model decision, pulled out of the
/// `#[tauri::command]` so it's a plain function over `&HostState`: no
/// locking, no `tauri::State`, trivial to unit-test all four combinations
/// against directly. This is deliberately where CRITICAL 1 (a stale
/// snapshot rendering as live forever) would reappear if it ever did — see
/// the tests below, which fail loudly if this match ever grows a wildcard
/// arm again.
fn view_for(s: &HostState) -> Value {
    let mut card = match (&s.latest, &s.error) {
        // A snapshot exists and the most recent poll succeeded: live.
        (Some(snap), None) => host_card(&s.name, snap, &s.histories, &Connection::Live),
        // A snapshot exists but the most recent poll failed: show it anyway
        // (this is real data, just not current), with an unmissable stale
        // badge instead of silently going on looking live forever.
        (Some(snap), Some(msg)) => {
            // `None` when a snapshot exists but no successful poll is on
            // record -- unreachable via the real poll loop below (latest and
            // last_success are always set together), but host_card must
            // still render it as "unknown", never a fabricated `0s ago`.
            let stale_secs = s.last_success.map(|t| t.elapsed().as_secs());
            host_card(
                &s.name,
                snap,
                &s.histories,
                &Connection::Stale {
                    message: msg.clone(),
                    stale_secs,
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
fn cockpit_view(hosts: &[Arc<Mutex<HostState>>], available: f64) -> Value {
    let cards: Vec<Value> = hosts
        .iter()
        .map(|host| view_for(&host.lock().expect("host state poisoned")))
        .collect();
    cockpit_payload(cards, available)
}

/// The payload shape, over already-rendered cards. Split from
/// [`cockpit_view`] so the tests can drive it without locks.
fn cockpit_payload(cards: Vec<Value>, available: f64) -> Value {
    let columns = host_columns(available, cards.len(), HOST_CARD_MIN_WIDTH, SPACING);
    // A cockpit with nothing to show says so in words made here, like every
    // other string the frontend paints -- an empty grid would read as a
    // broken app rather than an unconfigured one.
    let empty = if cards.is_empty() {
        json!({ "message": "No hosts configured. Add one in Settings." })
    } else {
        Value::Null
    };
    json!({
        "hosts": cards,
        "hostColumns": columns,
        "hostCardMinWidth": HOST_CARD_MIN_WIDTH,
        "spacing": SPACING,
        "panels": panel_table(),
        "empty": empty,
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

/// A host with no token configured. Not debounced like a failed poll: an
/// empty token never leaves this process to flap, so there is no momentary
/// drop to absorb, and the operator should see the cause on the first tick.
fn record_missing_token(s: &mut HostState) {
    s.consecutive_failures = FAILURE_THRESHOLD;
    // Distinct from a *wrong* token, which the agent itself rejects with a
    // 401 (`AgentError::AuthFailed`). Reusing that message would send the
    // operator to check the wrong layer.
    s.error = Some("No agent token configured for this host. Add one in Settings.".to_string());
}

/// Guards the one-line "the frontend reached us" notice below.
static FIRST_REQUEST: std::sync::Once = std::sync::Once::new();

#[tauri::command]
fn cockpit(width: f64, state: tauri::State<'_, Arc<Cockpit>>) -> Value {
    // The IPC boundary has no automated coverage (#123), and every failure
    // mode the manual smoke test in `app/README.md` looks for — a rejected
    // ACL, an unregistered command, a CSP break that stops `app.js` before it
    // ever calls `invoke` — has the identical shape from in here: this
    // function never runs. So one line on the first call is the whole
    // terminal-side signal, and it makes the procedure runnable on a machine
    // whose screen you cannot see. It says nothing about what the frontend
    // then *painted* — that is what the visual read is still for.
    FIRST_REQUEST.call_once(|| {
        eprintln!(
            "cockpit: first frontend request ({} host(s), {width}pt)",
            state.hosts.len()
        );
    });
    cockpit_view(&state.hosts, width)
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
fn dump_single(connection: &Connection) -> Value {
    cockpit_payload(vec![dump_card("ubu-3xdv", connection)], 1000.0)
}

/// Three hosts in three different connection states, so the Playwright suite
/// can assert per-host failure isolation against a payload the *shell* built
/// rather than one hand-assembled in JS. A hand-built envelope could not
/// notice `host_columns` or the payload's own key names drifting.
///
/// `hosts` takes the first N of them — 0 is the unconfigured cockpit, which
/// is a real state with its own rendering and no host card to build it from.
fn dump_cockpit(available: f64, hosts: usize) -> Value {
    let cards = vec![
        dump_card("ubu-3xdv", &Connection::Live),
        dump_card(
            "mac-mini",
            &Connection::Stale {
                message: unreachable_message(),
                stale_secs: Some(42),
            },
        ),
        {
            let mut card = pending_card("nuc-spare", &Pending::Failed(unreachable_message()));
            card["id"] = json!("nuc-spare");
            card
        },
    ];
    cockpit_payload(cards.into_iter().take(hosts).collect(), available)
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
    // A stale counterpart to --dump, built from the identical underlying
    // snapshot/history (see dump_card) so tests/frontend/layout.spec.js can
    // assert "the numbers don't change, only the badge does" against two
    // Rust-derived fixtures instead of hand-building the stale one in JS --
    // a hand-built fixture can't notice viewmodel's own `"stale"` string (or
    // its message format) drifting out from under it.
    if let Some(path) = dump_flag_path(args, "--dump-stale", "sample-stale.json") {
        write_json(
            &path,
            &dump_single(&Connection::Stale {
                message: unreachable_message(),
                stale_secs: Some(2),
            }),
        );
        return true;
    }
    if let Some(path) = dump_flag_path(args, "--dump-cockpit", "sample-cockpit.json") {
        // 3 * 900 + 2 * 16 — exactly the width three cards need side by side.
        let default_width = 3.0 * HOST_CARD_MIN_WIDTH + 2.0 * SPACING;
        write_json(
            &path,
            &dump_cockpit(
                value_flag(args, "--width", default_width),
                value_flag(args, "--hosts", 3),
            ),
        );
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

/// Enabled hosts in the cockpit's display order.
///
/// Remotes sorted by name, matching the Swift coordinator's
/// `SortDescriptor(\.name)`. The Swift cockpit puts the *local* machine first
/// (`HostsPanel.hosts` is `[local] + remoteHosts.hosts`); this shell has no
/// local collector — `HostMetricsKit` is Swift-only — so there is nothing to
/// put in front yet, and the remote ordering is the whole ordering.
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
    let seed = std::env::var("DEVCANOPY_SEED_HOST").ok();
    if let Err(e) = seed_from_env(&mut store, &credentials, seed.as_deref()) {
        eprintln!("could not seed a host from DEVCANOPY_SEED_HOST: {e}");
    }

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let mut states = Vec::new();
    for host in display_order(store.hosts()) {
        let state = Arc::new(Mutex::new(HostState {
            id: host.id.to_string(),
            name: host.name.clone(),
            histories: HostHistories::new(),
            latest: None,
            error: None,
            last_success: None,
            consecutive_failures: 0,
        }));
        states.push(Arc::clone(&state));

        let token = credentials
            .secret(SecretKey::HostToken(host.id))
            .unwrap_or_else(|e| {
                eprintln!("could not read the token for host {}: {e}", host.name);
                None
            })
            .unwrap_or_default();
        let token_configured = !token.is_empty();
        // Immutable, so it is owned by the poll task rather than the mutex.
        let client = AgentClient::new(host.base_url(), token);

        // One task per host: an unreachable host's 5s client timeout must not
        // hold up any other host's tick, and no task ever holds a lock across
        // an await, so a slow poll cannot block the `cockpit` command either.
        rt.spawn(async move {
            let mut tick = tokio::time::interval(POLL_INTERVAL);
            // `interval`'s default (`Burst`) fires every missed tick
            // back-to-back the moment a slow poll releases the executor -- and
            // the client timeout (5s, agentclient) exceeds this period, so one
            // slow poll can trigger several polls in a row. The charts equate
            // one history sample with one fixed time slice (PX_PER_SAMPLE), so
            // a burst silently compresses the time axis instead of just
            // running late. `Delay` waits a full period from completion
            // instead, so a slow poll shifts later polls rather than bunching
            // them.
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if !token_configured {
                    record_missing_token(&mut state.lock().expect("host state poisoned"));
                    continue;
                }
                // No lock is held across this await.
                let result = client.snapshot().await;
                let at = Instant::now();
                record_poll(&mut state.lock().expect("host state poisoned"), result, at);
            }
        });
    }

    tauri::Builder::default()
        .manage(Arc::new(Cockpit { hosts: states }))
        .invoke_handler(tauri::generate_handler![cockpit])
        .run(tauri::generate_context!())
        .expect("failed to start");

    // Keep the runtime alive for the lifetime of the app.
    drop(rt);
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::MemoryCredentialStore;

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
        }
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
    #[test]
    fn stale_when_a_snapshot_exists_but_the_last_poll_failed_and_the_data_is_retained() {
        let s = state_with(
            Some(fixture()),
            Some("Couldn't reach the agent. Check the host is up and the agent is running."),
            Some(Instant::now()),
        );
        let vm = view_for(&s);
        assert_eq!(vm["connection"]["state"], "stale");
        assert_eq!(vm["connection"]["color"], "#e05a4f");
        let msg = vm["connection"]["message"].as_str().unwrap();
        assert!(msg.contains("Couldn't reach the agent"));
        assert!(msg.contains("ago"), "expected a relative age, got {msg:?}");

        // The data itself must be exactly what a `Connection::Live` render of
        // the same snapshot would have produced -- staleness changes the
        // badge, never the numbers. Compared against a live render of the
        // same fixture rather than a hardcoded `"34%"`, so this covers every
        // data field at once and survives edits to `snapshot.json`.
        let live = view_for(&state_with(Some(fixture()), None, Some(Instant::now())));
        assert_eq!(vm["cpuValue"], live["cpuValue"]);
        assert_eq!(vm["cores"], live["cores"]);
        assert_eq!(vm["volumes"], live["volumes"]);
        assert_eq!(vm["memValue"], live["memValue"]);
        assert_ne!(
            vm["connection"], live["connection"],
            "the badge is the only thing staleness may change"
        );
        assert_eq!(vm["hostName"], "test-host");
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
        assert_eq!(view_for(&s)["connection"]["state"], "stale");
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

        // …so the card still dates its staleness instead of falling back to
        // the "unknown" branch `view_for` reserves for a missing
        // `last_success`.
        let vm = view_for(&s);
        assert_eq!(vm["connection"]["state"], "stale");
        let msg = vm["connection"]["message"].as_str().unwrap();
        assert!(msg.contains("ago"), "expected a relative age, got {msg:?}");
        assert!(
            !msg.contains("unknown"),
            "the age is known -- a run of failures must not erase it: {msg:?}"
        );
        assert_eq!(vm["cpuValue"], fixture_cpu_value());
    }

    #[test]
    fn a_success_after_failures_clears_the_error_and_re_dates_the_host() {
        let mut s = state_with(None, None, None);
        let first_success = Instant::now();
        record_poll(&mut s, Ok(fixture()), first_success);
        record_poll(&mut s, Err(AgentError::AuthFailed), Instant::now());
        record_poll(&mut s, Err(AgentError::AuthFailed), Instant::now());
        assert_eq!(view_for(&s)["connection"]["state"], "stale");

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
        record_missing_token(&mut s);
        let vm = view_for(&s);
        assert_eq!(vm["connection"]["state"], "failed");
        assert_eq!(
            vm["error"]["message"],
            "No agent token configured for this host. Add one in Settings."
        );
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
        let vm = cockpit_view(&hosts, 2000.0);
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
        let vm = cockpit_view(&hosts, 3000.0);
        let cards = vm["hosts"].as_array().expect("hosts array");

        assert_eq!(cards[0]["connection"]["state"], "live");
        assert_eq!(cards[0]["cpuValue"], fixture_cpu_value());

        assert_eq!(cards[1]["connection"]["state"], "stale");
        assert_eq!(
            cards[1]["cpuValue"], cards[0]["cpuValue"],
            "a stale host keeps the numbers it last heard"
        );

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
        assert_eq!(cockpit_view(&hosts, 2732.0)["hostColumns"], 3);
        assert_eq!(cockpit_view(&hosts, 2731.0)["hostColumns"], 2);
        assert_eq!(cockpit_view(&hosts, 1000.0)["hostColumns"], 1);
        // Unknown width stacks rather than assuming wide -- assuming wide is
        // what let a dead measurement masquerade as a deliberate layout.
        assert_eq!(cockpit_view(&hosts, 0.0)["hostColumns"], 1);
    }

    #[test]
    fn the_payload_carries_the_grid_constants_and_the_panel_table() {
        let vm = cockpit_payload(vec![], 1000.0);
        assert_eq!(vm["hostCardMinWidth"], 900.0);
        assert_eq!(vm["spacing"], 16.0);
        let panels = vm["panels"].as_array().expect("panel table");
        assert_eq!(panels[0]["id"], "hosts");
        assert_eq!(panels[0]["title"], "Hosts");
    }

    /// No hosts is a configuration state, not a broken app — so it arrives as
    /// a sentence made here, like every other string the frontend paints.
    #[test]
    fn an_empty_cockpit_says_so_instead_of_rendering_nothing() {
        let vm = cockpit_payload(vec![], 1000.0);
        assert!(vm["hosts"].as_array().expect("hosts array").is_empty());
        assert_eq!(
            vm["empty"]["message"],
            "No hosts configured. Add one in Settings."
        );

        let populated = cockpit_payload(vec![view_for(&state_with(None, None, None))], 1000.0);
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
    /// let the frontend's per-card state handling regress unnoticed.
    #[test]
    fn the_cockpit_dump_carries_three_hosts_in_three_different_states() {
        let vm = dump_cockpit(3.0 * HOST_CARD_MIN_WIDTH + 2.0 * SPACING, 3);
        let cards = vm["hosts"].as_array().expect("hosts array");
        assert_eq!(cards.len(), 3);
        assert_eq!(vm["hostColumns"], 3);

        let states: Vec<&str> = cards
            .iter()
            .map(|c| c["connection"]["state"].as_str().expect("state"))
            .collect();
        assert_eq!(states, vec!["live", "stale", "failed"]);

        let ids: Vec<&str> = cards
            .iter()
            .map(|c| c["id"].as_str().expect("id"))
            .collect();
        assert_eq!(ids, vec!["ubu-3xdv", "mac-mini", "nuc-spare"]);

        // The same payload at a narrow width is the stacked fixture -- same
        // cards, one column, so the frontend's grid can be tested both ways
        // against numbers Rust produced.
        assert_eq!(dump_cockpit(1000.0, 3)["hostColumns"], 1);

        // …and no hosts at all is the unconfigured cockpit.
        let none = dump_cockpit(1000.0, 0);
        assert!(none["hosts"].as_array().expect("hosts array").is_empty());
        assert_eq!(
            none["empty"]["message"],
            "No hosts configured. Add one in Settings."
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
