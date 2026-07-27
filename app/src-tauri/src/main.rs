//! DevCanopy shell. The frontend receives a finished view-model and paints;
//! all logic lives in `viewmodel`.

use agentclient::AgentClient;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use viewmodel::card::{host_card, pending_card, Connection, HostHistories, Pending};

/// The mutable half of one watched host. The `AgentClient` is immutable and is
/// held separately so the poll loop never locks across an await.
struct HostState {
    name: String,
    histories: HostHistories,
    latest: Option<metrics::Snapshot>,
    error: Option<String>,
    /// When the last *successful* poll landed. Together with `error`, this is
    /// what lets `snapshot()` tell "still live" apart from "dead since Xs
    /// ago" instead of letting a stale `latest` masquerade as current.
    last_success: Option<Instant>,
}

type Shared = Arc<Mutex<HostState>>;

/// Tokens live in the OS credential store, never in app storage. The service
/// name matches the Swift `KeychainHelper` so an existing entry is reused.
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
            let stale_secs = s.last_success.map(|t| t.elapsed().as_secs()).unwrap_or(0);
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

#[tauri::command]
fn snapshot(state: tauri::State<'_, Shared>) -> Value {
    let s = state.lock().unwrap();
    view_for(&s)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--dump") {
        let path = args
            .get(i + 1)
            .cloned()
            .unwrap_or_else(|| "sample.json".into());
        // Test-fixture generation only, decoupled from the live-agent path
        // below (which starts with empty history and only ever records real
        // samples). Built from the committed agent-contract fixture so this
        // is reproducible on a clean checkout with no live agent involved.
        const FIXTURE: &str = include_str!("../../../crates/metrics/tests/fixtures/snapshot.json");
        let snap: metrics::Snapshot = serde_json::from_str(FIXTURE).expect("fixture");
        let mut h = HostHistories::new();
        // A full history buffer is what lets the Playwright suite assert
        // "wider charts show proportionally more samples" without needing
        // hundreds of real polls first — see HISTORY_CAPACITY's doc comment.
        for _ in 0..viewmodel::layout::HISTORY_CAPACITY {
            h.record(&snap);
        }
        let vm = host_card("ubu-3xdv", &snap, &h, &Connection::Live);
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
            match result {
                Ok(snap) => {
                    s.histories.record(&snap);
                    s.latest = Some(snap);
                    s.error = None;
                    s.last_success = Some(Instant::now());
                }
                Err(e) => s.error = Some(e.user_message().to_string()),
            }
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

    fn fixture() -> metrics::Snapshot {
        const FIXTURE: &str = include_str!("../../../crates/metrics/tests/fixtures/snapshot.json");
        serde_json::from_str(FIXTURE).unwrap()
    }

    fn state_with(
        latest: Option<metrics::Snapshot>,
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

    #[test]
    fn live_when_a_snapshot_exists_and_the_last_poll_succeeded() {
        let s = state_with(Some(fixture()), None, Some(Instant::now()));
        let vm = view_for(&s);
        assert_eq!(vm["connection"]["state"], "live");
        assert_eq!(vm["connection"]["color"], "#33d17a");
        assert!(vm["connection"]["message"].is_null());
        assert_eq!(vm["cpuValue"], "34%");
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
        // badge, never the numbers.
        assert_eq!(vm["cpuValue"], "34%");
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
}
