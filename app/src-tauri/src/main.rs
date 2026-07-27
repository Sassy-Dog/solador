//! DevCanopy shell. The frontend receives a finished view-model and paints;
//! all logic lives in `viewmodel`.

use serde_json::Value;
use viewmodel::card::{host_card, HostHistories};

const FIXTURE: &str = include_str!("../../../crates/metrics/tests/fixtures/snapshot.json");

/// Task 7 replaces this with live polling.
fn current_view_model() -> Value {
    let snap: metrics::Snapshot = serde_json::from_str(FIXTURE).expect("fixture");
    let mut h = HostHistories::new();
    // Seed a full history buffer: fewer samples than HISTORY_CAPACITY would
    // plateau the visible-sample count at every viewport wide enough to
    // request more than we have, defeating the "widen instead of stretch"
    // chart behaviour the Playwright suite checks.
    for _ in 0..viewmodel::layout::HISTORY_CAPACITY {
        h.record(&snap);
    }
    host_card("ubu-3xdv", &snap, &h)
}

#[tauri::command]
fn snapshot() -> Value {
    current_view_model()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--dump") {
        let path = args
            .get(i + 1)
            .cloned()
            .unwrap_or_else(|| "sample.json".into());
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&current_view_model()).unwrap(),
        )
        .expect("write view-model");
        println!("wrote {path}");
        return;
    }

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![snapshot])
        .run(tauri::generate_context!())
        .expect("failed to start");
}
