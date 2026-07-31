//! DevCanopy shell. The frontend receives a finished view-model and paints;
//! all logic lives in `viewmodel`.

use agentclient::{AgentClient, AgentError};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use viewmodel::card::{host_card, pending_card, Connection, HostHistories, Pending};

/// The mutable half of one watched host. The `AgentClient` is immutable and is
/// held separately so the poll loop never locks across an await.
struct HostState {
    name: String,
    histories: HostHistories,
    latest: Option<wire::Snapshot>,
    error: Option<String>,
    /// When the last *successful* poll landed. Together with `error`, this is
    /// what lets `snapshot()` tell "still live" apart from "dead since Xs
    /// ago" instead of letting a stale `latest` masquerade as current.
    last_success: Option<Instant>,
}

type Shared = Arc<Mutex<HostState>>;

/// Tokens live in the OS credential store, never in app storage. The
/// *service* string matches the Swift `KeychainHelper`
/// (`com.sassydog.devcanopy`), but the *account* does not: Swift stores each
/// host's token under `host_token_<UUID>`
/// (`DevCanopy/Services/KeychainHelper.swift`), this stores it under
/// `host-{id}`. Nothing is actually reused today -- a token saved by one app
/// is invisible to the other; unifying the account scheme is separate work.
fn load_token(host_id: &str) -> Option<String> {
    keyring::Entry::new("com.sassydog.devcanopy", &format!("host-{host_id}"))
        .ok()?
        .get_password()
        .ok()
}

/// The whole `(latest, error)` -> view-model decision, pulled out of the
/// `#[tauri::command]` so it's a plain function over `&HostState`: no
/// locking, no `tauri::State`, trivial to unit-test all four combinations
/// against directly. This is deliberately where CRITICAL 1 (a stale
/// snapshot rendering as live forever) would reappear if it ever did — see
/// the tests below, which fail loudly if this match ever grows a wildcard
/// arm again.
fn view_for(s: &HostState) -> Value {
    match (&s.latest, &s.error) {
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
    }
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
fn record_poll(s: &mut HostState, result: Result<wire::Snapshot, AgentError>, at: Instant) {
    match result {
        Ok(snap) => {
            s.histories.record(&snap);
            s.latest = Some(snap);
            s.error = None;
            s.last_success = Some(at);
        }
        Err(e) => s.error = Some(e.user_message().into_owned()),
    }
}

#[tauri::command]
fn snapshot(state: tauri::State<'_, Shared>) -> Value {
    let s = state.lock().unwrap();
    view_for(&s)
}

/// Test-fixture generation only, decoupled from the live-agent path below
/// (which starts with empty history and only ever records real samples).
/// Built from the committed agent-contract fixture so this is reproducible on
/// a clean checkout with no live agent involved. Shared by both `--dump` and
/// `--dump-stale` so a live and a stale view-model always carry the exact
/// same underlying data -- staleness is meant to change only the connection
/// badge, never the numbers (see `card::tests::a_stale_host_card_keeps_its_
/// data_but_turns_red_and_says_how_old_it_is`), and the Playwright suite
/// depends on that holding for its dumped fixtures too.
fn dump_view_model(connection: &Connection) -> Value {
    const FIXTURE: &str = include_str!("../../../crates/wire/tests/fixtures/snapshot.json");
    let snap: wire::Snapshot = serde_json::from_str(FIXTURE).expect("fixture");
    let mut h = HostHistories::new();
    // A full history buffer is what lets the Playwright suite assert "wider
    // charts show proportionally more samples" without needing hundreds of
    // real polls first — see HISTORY_CAPACITY's doc comment.
    for _ in 0..viewmodel::layout::HISTORY_CAPACITY {
        h.record(&snap);
    }
    host_card("ubu-3xdv", &snap, &h, connection)
}

/// Returns the path argument following `flag` if `flag` is present, falling
/// back to `default` when the flag is given with no path.
fn dump_flag_path(args: &[String], flag: &str, default: &str) -> Option<String> {
    let i = args.iter().position(|a| a == flag)?;
    Some(args.get(i + 1).cloned().unwrap_or_else(|| default.into()))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(path) = dump_flag_path(&args, "--dump", "sample.json") {
        let vm = dump_view_model(&Connection::Live);
        std::fs::write(&path, serde_json::to_string_pretty(&vm).unwrap())
            .expect("write view-model");
        println!("wrote {path}");
        return;
    }
    // A stale counterpart to --dump, built from the identical underlying
    // snapshot/history (see dump_view_model) so tests/frontend/layout.spec.js
    // can assert "the numbers don't change, only the badge does" against two
    // Rust-derived fixtures instead of hand-building the stale one in JS --
    // a hand-built fixture can't notice viewmodel's own `"stale"` string (or
    // its message format) drifting out from under it.
    if let Some(path) = dump_flag_path(&args, "--dump-stale", "sample-stale.json") {
        let vm = dump_view_model(&Connection::Stale {
            message: "Couldn't reach the agent. Check the host is up and the agent is running."
                .to_string(),
            stale_secs: Some(2),
        });
        std::fs::write(&path, serde_json::to_string_pretty(&vm).unwrap())
            .expect("write view-model");
        println!("wrote {path}");
        return;
    }

    // Configuration is env-driven for the skeleton; Settings arrives with the
    // store crate in a later plan.
    let host_id = std::env::var("DEVCANOPY_HOST_ID").unwrap_or_else(|_| "default".into());
    let name = std::env::var("DEVCANOPY_HOST_NAME").unwrap_or_else(|_| "ubu-3xdv".into());
    let url =
        std::env::var("DEVCANOPY_HOST_URL").unwrap_or_else(|_| "http://100.87.202.125:7878".into());
    let token = std::env::var("DEVCANOPY_AGENT_TOKEN")
        .ok()
        .or_else(|| load_token(&host_id))
        .unwrap_or_default();
    // Distinct from a *wrong* token, which the agent itself rejects with a
    // 401 (`AgentError::AuthFailed`). An empty token never leaves this
    // process to be rejected by anything, so it gets its own message rather
    // than reusing that one and misleading the operator into checking the
    // wrong layer.
    let token_configured = !token.is_empty();

    let shared: Shared = Arc::new(Mutex::new(HostState {
        name,
        histories: HostHistories::new(),
        latest: None,
        error: None,
        last_success: None,
    }));

    // Immutable, so it is owned by the poll task rather than the mutex.
    let client = AgentClient::new(url, token);

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let poll_target = shared.clone();
    rt.spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
        // `interval`'s default (`Burst`) fires every missed tick back-to-back
        // the moment a slow poll releases the executor -- and the client
        // timeout (5s, agentclient) exceeds this 2s period, so one slow poll
        // can trigger 2-3 polls in a row. The charts equate one history
        // sample with one fixed time slice (PX_PER_SAMPLE), so a burst
        // silently compresses the time axis instead of just running late.
        // `Delay` waits a full period from completion instead, so a slow
        // poll shifts later polls rather than bunching them.
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if !token_configured {
                let mut s = poll_target.lock().unwrap();
                s.error = Some(
                    "No agent token configured for this host. Add one in Settings.".to_string(),
                );
                continue;
            }
            // No lock is held across this await.
            let result = client.snapshot().await;
            let mut s = poll_target.lock().unwrap();
            record_poll(&mut s, result, Instant::now());
        }
    });

    tauri::Builder::default()
        .manage(shared)
        .invoke_handler(tauri::generate_handler![snapshot])
        .run(tauri::generate_context!())
        .expect("failed to start");

    // Keep the runtime alive for the lifetime of the app.
    drop(rt);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> wire::Snapshot {
        const FIXTURE: &str = include_str!("../../../crates/wire/tests/fixtures/snapshot.json");
        serde_json::from_str(FIXTURE).unwrap()
    }

    fn state_with(
        latest: Option<wire::Snapshot>,
        error: Option<&str>,
        last_success: Option<Instant>,
    ) -> HostState {
        HostState {
            name: "test-host".to_string(),
            histories: HostHistories::new(),
            latest,
            error: error.map(str::to_string),
            last_success,
        }
    }

    /// The fixture's CPU reading as a card renders it. Derived from the
    /// parsed fixture rather than written out as `"34%"`, so editing
    /// `snapshot.json` can't leave a literal behind that has to be chased
    /// across this file.
    fn fixture_cpu_value() -> String {
        format!("{}%", fixture().cpu.total_usage.round() as i64)
    }

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
        assert_eq!(view_for(&s)["connection"]["state"], "stale");

        let recovered = Instant::now();
        record_poll(&mut s, Ok(fixture()), recovered);
        assert_eq!(s.last_success, Some(recovered));
        assert!(s.error.is_none());
        assert_eq!(view_for(&s)["connection"]["state"], "live");
    }
}
