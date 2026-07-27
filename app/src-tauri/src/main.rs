//! DevCanopy shell. The frontend receives a finished view-model and paints;
//! all logic lives in `viewmodel`.

use agentclient::AgentClient;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use viewmodel::card::{host_card, HostHistories};

/// The mutable half of one watched host. The `AgentClient` is immutable and is
/// held separately so the poll loop never locks across an await.
struct HostState {
    name: String,
    histories: HostHistories,
    latest: Option<metrics::Snapshot>,
    error: Option<String>,
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
        (Some(snap), _) => host_card(&s.name, snap, &s.histories),
        (None, Some(msg)) => json!({ "error": { "message": msg, "hostName": s.name } }),
        (None, None) => json!({
            "error": { "message": "waiting for first sample…", "hostName": s.name }
        }),
    }
}

fn main() {
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

    let shared: Shared = Arc::new(Mutex::new(HostState {
        name,
        histories: HostHistories::new(),
        latest: None,
        error: None,
    }));

    // Immutable, so it is owned by the poll task rather than the mutex.
    let client = AgentClient::new(url, token);

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let poll_target = shared.clone();
    rt.spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            tick.tick().await;
            // No lock is held across this await.
            let result = client.snapshot().await;
            let mut s = poll_target.lock().unwrap();
            match result {
                Ok(snap) => {
                    s.histories.record(&snap);
                    s.latest = Some(snap);
                    s.error = None;
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
