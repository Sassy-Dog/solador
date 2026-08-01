//! Sampling the real machine the tests are running on.
//!
//! Everything the crate decides is unit-tested on values next to the code that
//! decides it; what those tests cannot cover is whether the platform read
//! underneath actually works — that the sysinfo calls compile *and* return
//! something on macOS and on Windows. So this asserts **shape, never values**:
//! a build agent's CPU load, disk layout and battery are nobody's business, and
//! an assertion about them is a flake waiting to happen.
//!
//! ## Skip-safety
//!
//! Set `DEVCANOPY_SKIP_HOST_SAMPLING_TESTS` to skip. A sandbox that reports no
//! memory at all is also treated as "nothing to sample here" rather than as a
//! failure — the crate cannot be blamed for a kernel that will not answer, and
//! a red test in that environment would be noise, not signal.

use std::thread::sleep;
use std::time::Duration;

use localhost::LocalSampler;

/// How long to wait between the two samples. Comfortably past sysinfo's
/// `MINIMUM_CPU_UPDATE_INTERVAL` (200 ms on both target platforms) so the second
/// sample's deltas are the measured ones this test is here to see.
const SAMPLE_GAP: Duration = Duration::from_millis(300);

fn skip_requested() -> bool {
    std::env::var_os("DEVCANOPY_SKIP_HOST_SAMPLING_TESTS").is_some()
}

#[test]
fn sampling_the_real_machine_produces_a_well_formed_snapshot() {
    if skip_requested() {
        eprintln!("skipping: DEVCANOPY_SKIP_HOST_SAMPLING_TESTS is set");
        return;
    }

    let mut sampler = LocalSampler::new();
    let first = sampler.sample();

    if first.memory.total_gb <= 0.0 {
        eprintln!("skipping: this environment reports no memory to sample");
        return;
    }

    // Capacity is the one thing every platform answers, so it is the one thing
    // asserted about a value rather than a shape.
    assert!(
        first.memory.used_gb <= first.memory.total_gb,
        "used {} GB of {} GB",
        first.memory.used_gb,
        first.memory.total_gb
    );
    assert!(
        !first.cpu.model.is_empty(),
        "the model falls back, never empty"
    );
    assert!(first.timestamp.ends_with('Z'), "got {:?}", first.timestamp);

    // The first sample has no previous cumulative reading to diff against, so
    // its rates must be unknown rather than a plausible-looking zero.
    assert_eq!(
        first.network,
        wire::Network::default(),
        "the first sample cannot know a rate"
    );
    assert_eq!(
        first.disk,
        wire::Disk::default(),
        "the first sample cannot know a rate"
    );

    for volume in &first.volumes {
        assert!(volume.total_gb > 0.0, "{} has no capacity", volume.mount);
        assert!(
            volume.used_gb <= volume.total_gb,
            "{} uses more than it has",
            volume.mount
        );
    }

    sleep(SAMPLE_GAP);
    let second = sampler.sample();

    // With a baseline in hand the rates become measurable. What they measured is
    // this machine's business; that they are finite and non-negative is not.
    for (label, rate) in [
        ("download", second.network.download_mbps),
        ("upload", second.network.upload_mbps),
        ("read", second.disk.read_mbps),
        ("write", second.disk.write_mbps),
    ] {
        let rate = rate.unwrap_or_else(|| panic!("a second sample can rate {label}"));
        assert!(rate.is_finite() && rate >= 0.0, "{label} rate was {rate}");
    }

    let usage = second
        .cpu
        .usage
        .expect("300 ms is past sysinfo's minimum CPU interval");
    assert!(
        !usage.per_core.is_empty(),
        "a machine running this test has cores"
    );
    // Finite and non-negative, deliberately without an upper bound: Windows'
    // performance counters can momentarily overshoot 100%, and a red CI job over
    // that would be noise. A negative or NaN percentage is the real bug, and
    // that is what this catches.
    for (core, percent) in usage.per_core.iter().enumerate() {
        assert!(
            percent.is_finite() && *percent >= 0.0,
            "core {core} reported {percent}%"
        );
    }
    assert!(
        usage.total.is_finite() && usage.total >= 0.0,
        "total CPU was {}",
        usage.total
    );

    // The union of two top-5s: between one and ten, never the same pid twice.
    // Non-empty matters — a process enumerating the machine it is running on
    // always finds at least itself, so an empty list means the enumeration
    // broke rather than that the machine is idle.
    assert!(
        !second.processes.is_empty(),
        "the enumeration must at least find this test"
    );
    assert!(second.processes.len() <= 10, "{:?}", second.processes.len());
    let mut pids: Vec<i64> = second.processes.iter().map(|p| p.pid).collect();
    pids.sort_unstable();
    let unique = pids.len();
    pids.dedup();
    assert_eq!(pids.len(), unique, "the union must be deduped by pid");
}

/// A real sample must survive the lowering with the wire contract's shape
/// intact — including the GPU absence this crate reports on every platform
/// today, which `is_present()` has to read as "no GPU", not as an idle one.
#[test]
fn a_real_sample_lowers_onto_the_wire_contract() {
    if skip_requested() {
        eprintln!("skipping: DEVCANOPY_SKIP_HOST_SAMPLING_TESTS is set");
        return;
    }

    let mut sampler = LocalSampler::new();
    let snapshot = sampler.sample();

    if snapshot.memory.total_gb <= 0.0 {
        eprintln!("skipping: this environment reports no memory to sample");
        return;
    }

    let wired = snapshot.to_wire();

    assert_eq!(wired.timestamp, snapshot.timestamp);
    assert_eq!(wired.memory.total_gb, snapshot.memory.total_gb);
    assert!(
        !wired.gpu.is_present(),
        "an unmeasured GPU must lower to an absent one"
    );

    // The contract is a JSON one, so a real sample has to survive a round trip
    // through it. Compared field-shape rather than field-for-field: serde_json
    // parses floats to within an ULP unless its `float_roundtrip` feature is on,
    // and a byte-exact `assert_eq!` here would be testing that feature flag
    // rather than this crate.
    let json = serde_json::to_string(&wired).expect("a snapshot serialises");
    let decoded: wire::Snapshot = serde_json::from_str(&json).expect("and decodes again");

    assert_eq!(decoded.timestamp, wired.timestamp);
    assert_eq!(decoded.cpu.model, wired.cpu.model);
    assert_eq!(decoded.cpu.thermal_state, wired.cpu.thermal_state);
    assert_eq!(decoded.cpu.core_usages.len(), wired.cpu.core_usages.len());
    assert_eq!(decoded.volumes.len(), wired.volumes.len());
    assert_eq!(decoded.processes.len(), wired.processes.len());
    assert_eq!(decoded.battery.is_some(), wired.battery.is_some());
    assert_eq!(decoded.gpu.is_present(), wired.gpu.is_present());
}
