use wire::{Container, Health, Snapshot};

const FIXTURE: &str = include_str!("fixtures/snapshot.json");

/// `GET /v1/containers`, in the agent's exact wire format. Keys are copied from
/// the `#[serde(rename)]` attributes on `agent/src/containers.rs::Container`
/// (`statusText`, `isRunning`, and the un-renamed `name`/`runtime`/`image`);
/// do not invent names. The three rows cover every runtime the agent can emit,
/// and the tart row carries the `"image": null` that only tart produces.
const CONTAINERS_FIXTURE: &str = include_str!("fixtures/containers.json");

/// `GET /v1/health`, in the agent's exact wire format. Keys are copied from the
/// `json!` literal in `agent/src/server.rs::health_handler` — `status`,
/// `hostname`, `version`, `sampleAgeSeconds`, `samplerStale`; do not invent
/// names.
const HEALTH_FIXTURE: &str = include_str!("fixtures/health.json");

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

    let empty = wire::Volume {
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
///
/// Every wire struct derives `PartialEq`, so this compares the whole
/// `Snapshot` rather than a hand-picked pair of fields -- a field that
/// serialised lossily (or not at all) used to survive here as long as
/// `core_usages` and the volume count came back intact.
#[test]
fn round_tripping_through_json_preserves_the_snapshot() {
    let s: Snapshot = serde_json::from_str(FIXTURE).unwrap();
    let out = serde_json::to_string(&s).unwrap();
    let again: Snapshot = serde_json::from_str(&out).expect("re-read own output");
    assert_eq!(s, again, "every field must survive a round trip");
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
    let battery: wire::Battery =
        serde_json::from_str(&raw).expect("shared fixture must deserialize into Battery");
    assert_eq!(battery.level, 82.5, "battery level must be 82.5");
    assert!(battery.is_charging, "battery must be charging");
}

/// Guards against the committed fixture drifting from what the agent sends.
/// Skipped when the live capture is absent so CI stays hermetic.
///
/// # Capturing the live fixture
///
/// The capture is deliberately gitignored (`.gitignore`: `crates/wire/tests/
/// fixtures/snapshot-live.json`) — do **not** commit it. Capture it locally
/// before merging a change to the wire types, then re-run this test:
///
/// ```bash
/// curl -fsS -H "Authorization: Bearer $DEVCANOPY_AGENT_TOKEN" \
///   http://<agent-host>:7878/v1/snapshot \
///   > crates/wire/tests/fixtures/snapshot-live.json
/// cargo test -p devcanopy-wire --locked
/// ```
///
/// Write straight to the path below: a capture that lands anywhere else makes
/// this test silently return instead of failing (#144). `<agent-host>` is the
/// Tailscale address of any host running `agent/`.
///
/// If this then fails, the committed `snapshot.json` is wrong — fix the types
/// or the fixture, not the test.
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

/// The agent omits `fstype` entirely when a volume has none, rather than
/// emitting `"fstype": null`. That is wire behaviour, not a formatting detail:
/// this crate is the single definition of the contract, so dropping
/// `skip_serializing_if` here would silently change what every agent sends.
#[test]
fn a_volume_without_fstype_omits_the_key_rather_than_emitting_null() {
    let absent = wire::Volume {
        mount: "/x".into(),
        used_gb: 1.0,
        total_gb: 2.0,
        fstype: None,
    };
    let json = serde_json::to_string(&absent).unwrap();
    assert!(
        !json.contains("fstype"),
        "fstype must be omitted, got {json}"
    );

    let present = wire::Volume {
        mount: "/y".into(),
        used_gb: 1.0,
        total_gb: 2.0,
        fstype: Some("ext4".into()),
    };
    assert!(serde_json::to_string(&present)
        .unwrap()
        .contains(r#""fstype":"ext4""#));
}

/// "Does this host have a GPU" now has ONE definition, shared by the agent that
/// produces the data and the app that decides between a number and an em dash.
#[test]
fn gpu_absence_is_one_definition_shared_by_producer_and_consumer() {
    assert!(
        !wire::Gpu::zeros().is_present(),
        "an all-zero GPU is absent"
    );

    // A real GPU sitting idle reports usage 0.0 and must still count as present
    // — it renders "0%", not an em dash. VRAM capacity is the discriminator.
    let idle = wire::Gpu {
        usage: 0.0,
        vram_used_gb: 0.5,
        vram_total_gb: 24.0,
    };
    assert!(idle.is_present());
}

#[test]
fn deserialises_the_agents_container_list() {
    let cs: Vec<Container> =
        serde_json::from_str(CONTAINERS_FIXTURE).expect("agent JSON must deserialise");
    assert_eq!(cs.len(), 3);

    assert_eq!(cs[0].name, "llm");
    assert_eq!(cs[0].status_text, "Up 2 days");
    assert!(cs[0].is_running);
    assert_eq!(cs[0].runtime, "podman");
    assert_eq!(cs[0].image.as_deref(), Some("llama-swap:latest"));

    assert!(
        !cs[1].is_running,
        "an exited container must not read running"
    );
    assert_eq!(cs[1].runtime, "docker");

    // tart VMs have no image, and the agent sends the key as JSON null.
    assert_eq!(cs[2].runtime, "tart");
    assert!(cs[2].image.is_none());
}

/// The mirror image of `a_volume_without_fstype_omits_the_key_rather_than_
/// emitting_null`: `agent/src/containers.rs` has no `skip_serializing_if` on
/// `image`, and its `container_serializes_to_exact_wire_keys` test asserts the
/// key survives as null. Adding `skip_serializing_if` here would silently
/// change the contract in the opposite direction.
#[test]
fn an_imageless_container_emits_a_null_image_rather_than_omitting_the_key() {
    let vm = Container {
        name: "build-runner".into(),
        status_text: "running".into(),
        is_running: true,
        runtime: "tart".into(),
        image: None,
    };
    let json = serde_json::to_value(&vm).unwrap();
    assert!(
        json.as_object().unwrap().contains_key("image"),
        "image key must be present, got {json}"
    );
    assert!(json["image"].is_null(), "image must be null, got {json}");
}

/// Why `runtime` is a `String` and not an enum. A future agent that learns a
/// fourth runtime must not break the *whole* list's decode for an app that
/// hasn't shipped yet — the unknown label arrives intact and renders as itself.
#[test]
fn an_unknown_runtime_label_decodes_instead_of_failing_the_whole_list() {
    let json = r#"[
        { "name": "ci", "statusText": "Up 1 hour", "isRunning": true, "runtime": "containerd", "image": null }
    ]"#;
    let cs: Vec<Container> = serde_json::from_str(json).expect("unknown runtime must decode");
    assert_eq!(cs[0].runtime, "containerd");
}

/// A host with no containers at all serves `[]`. That is a legitimate answer,
/// not skew — it must never surface as `DecodeFailed`.
#[test]
fn an_empty_container_list_is_not_a_decode_failure() {
    let cs: Vec<Container> = serde_json::from_str("[]").expect("empty list must decode");
    assert!(cs.is_empty());
}

#[test]
fn deserialises_the_agents_health_payload() {
    let h: Health = serde_json::from_str(HEALTH_FIXTURE).expect("agent JSON must deserialise");
    assert_eq!(h.status, "ok");
    assert_eq!(h.hostname, "ubu-3xdv");
    assert_eq!(h.version, "0.4.0");
    assert_eq!(h.sample_age_seconds, Some(2));
    assert_eq!(h.sampler_stale, Some(false));
}

/// `sampleAgeSeconds`/`samplerStale` arrived in #35; an agent that predates the
/// redeploy omits them. Decoding must yield `None` — the same tolerance the
/// Swift `HealthInfo` gives them (`Int?`/`Bool?`) — never a decode failure and
/// never a fabricated `0`/`false`, which would read as a healthy fresh sampler.
#[test]
fn health_from_a_pre_35_agent_decodes_with_none_sampler_fields() {
    let json = r#"{ "status": "ok", "hostname": "old-host", "version": "0.1.0" }"#;
    let h: Health = serde_json::from_str(json).expect("older agent payload must decode");
    assert_eq!(h.hostname, "old-host");
    assert!(h.sample_age_seconds.is_none());
    assert!(h.sampler_stale.is_none());
}

/// Whole-value round trips, for the same reason
/// `round_tripping_through_json_preserves_the_snapshot` compares whole
/// `Snapshot`s: a field that serialised lossily (or under the wrong key) would
/// otherwise survive behind a hand-picked assertion.
#[test]
fn round_tripping_through_json_preserves_containers_and_health() {
    let cs: Vec<Container> = serde_json::from_str(CONTAINERS_FIXTURE).unwrap();
    let again: Vec<Container> = serde_json::from_str(&serde_json::to_string(&cs).unwrap())
        .expect("re-read own container output");
    assert_eq!(cs, again);

    let h: Health = serde_json::from_str(HEALTH_FIXTURE).unwrap();
    let again: Health = serde_json::from_str(&serde_json::to_string(&h).unwrap())
        .expect("re-read own health output");
    assert_eq!(h, again);
}
