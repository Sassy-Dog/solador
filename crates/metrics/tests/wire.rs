use metrics::Snapshot;

const FIXTURE: &str = include_str!("fixtures/snapshot.json");

#[test]
fn deserialises_the_agents_wire_format() {
    let s: Snapshot = serde_json::from_str(FIXTURE).expect("agent JSON must deserialise");
    assert_eq!(s.cpu.core_usages.len(), 16);
    assert_eq!(s.cpu.total_usage, 34.2);
    assert_eq!(s.memory.total_gb, 62.7);
    assert_eq!(s.disk.write_mbps, 88.1);
    assert_eq!(s.network.download_mbps, 2.1);
    assert_eq!(s.volumes.len(), 3);
    assert_eq!(s.processes[0].name, "cargo");
}

#[test]
fn absent_battery_is_none_not_a_default() {
    let s: Snapshot = serde_json::from_str(FIXTURE).unwrap();
    assert!(
        s.battery.is_none(),
        "a host with no battery must be None, never a zeroed Battery"
    );
}

#[test]
fn percent_used_guards_against_a_zero_total() {
    let s: Snapshot = serde_json::from_str(FIXTURE).unwrap();
    let root = s.volumes.iter().find(|v| v.mount == "/").unwrap();
    assert!((root.percent_used() - 44.978).abs() < 0.01);

    let empty = metrics::Volume {
        mount: "/x".into(),
        used_gb: 1.0,
        total_gb: 0.0,
        fstype: None,
    };
    assert_eq!(empty.percent_used(), 0.0, "must not divide by zero");
}

#[test]
fn round_trips_so_the_app_and_agent_cannot_drift() {
    let s: Snapshot = serde_json::from_str(FIXTURE).unwrap();
    let out = serde_json::to_string(&s).unwrap();
    let again: Snapshot = serde_json::from_str(&out).unwrap();
    assert_eq!(s.cpu.core_usages, again.cpu.core_usages);
    assert_eq!(s.volumes.len(), again.volumes.len());
}

/// Guards against the committed fixture drifting from what the agent sends.
/// Skipped when the live capture is absent so CI stays hermetic.
#[test]
fn live_capture_deserialises_when_present() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/snapshot-live.json"
    );
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let s: Snapshot = serde_json::from_str(&raw).expect("live agent JSON must deserialise");
    assert!(!s.cpu.core_usages.is_empty());
}
