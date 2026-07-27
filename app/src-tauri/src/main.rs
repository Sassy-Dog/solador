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

#[tauri::command]
fn snapshot(state: tauri::State<'_, Shared>) -> Value {
    let s = state.lock().unwrap();
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
