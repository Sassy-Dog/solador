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

/// Round-trips only this crate's own `Snapshot` (deserialise -> serialise ->
/// deserialise again) -- it exercises this type's serde impls, nothing more.
/// It CANNOT catch the app and the live agent drifting apart: that needs a
/// real agent payload, which is what `live_capture_deserialises_when_present`
/// below checks (and only when `snapshot-live.json` is actually present).
#[test]
fn round_tripping_through_json_preserves_the_snapshot() {
    let s: Snapshot = serde_json::from_str(FIXTURE).unwrap();
    let out = serde_json::to_string(&s).unwrap();
    let again: Snapshot = serde_json::from_str(&out).unwrap();
    assert_eq!(s.cpu.core_usages, again.cpu.core_usages);
    assert_eq!(s.volumes.len(), again.volumes.len());
}

/// Volume without fstype key exercises #[serde(default)] on the omitted field.
/// The agent uses #[serde(skip_serializing_if)] so fstype is absent when unknown;
/// deserialization must default to None, not fail.
#[test]
fn volume_without_fstype_deserialises_to_none() {
    let json = r#"{
        "timestamp": "2026-07-27T14:03:12Z",
        "cpu": { "totalUsage": 0.0, "coreUsages": [], "model": "test", "thermalState": 0 },
        "memory": { "usedGB": 0.0, "totalGB": 1.0, "swapUsedGB": 0.0, "pressure": 0.0 },
        "disk": { "readMBps": 0.0, "writeMBps": 0.0 },
        "network": { "downloadMBps": 0.0, "uploadMBps": 0.0 },
        "gpu": { "usage": 0.0, "vramUsedGB": 0.0, "vramTotalGB": 0.0 },
        "battery": null,
        "volumes": [
            { "mount": "/", "usedGB": 10.0, "totalGB": 100.0 }
        ],
        "processes": []
    }"#;
    let s: Snapshot = serde_json::from_str(json).expect("volume without fstype must deserialise");
    let vol = &s.volumes[0];
    assert_eq!(vol.mount, "/");
    assert!(
        vol.fstype.is_none(),
        "omitted fstype key must deserialize to None via #[serde(default)]"
    );
}

/// Non-null battery deserialization using the shared cross-language contract fixture.
/// The agent and Swift decoder both use TestFixtures/battery_contract.json;
/// this crate is the third implementation of the contract and must join that pattern.
#[test]
fn battery_deserialises_from_shared_contract_fixture() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../TestFixtures/battery_contract.json"
    );
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read shared battery fixture {path}: {e}"));
    let battery: metrics::Battery =
        serde_json::from_str(&raw).expect("shared fixture must deserialize into Battery");
    assert_eq!(battery.level, 82.5, "battery level must be 82.5");
    assert!(battery.is_charging, "battery must be charging");
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
