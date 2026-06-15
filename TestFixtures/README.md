# Shared wire-contract fixtures

JSON fixtures that lock cross-language wire contracts so the Rust agent
(`agent/`) and the Swift app (`Packages/HostMetricsKit`) cannot silently drift.

Each fixture is decoded by **both** a Rust test and a Swift test. Changing the
shape requires updating both sides, and both test suites fail until they agree.

## `battery_contract.json`

The canonical battery wire shape emitted under `snapshot.battery` by the agent.
It is the minimal cross-platform contract — `level` (0–100) and `isCharging` —
the only two fields a generic host agent (Linux/`sysinfo`) can produce. The
macOS-local IOKit collector additionally populates richer optional fields
(`health`, `cycleCount`, `wattage`, …); those are decode-optional and never part
of this floor.

- Rust lock: `agent/src/metrics.rs` (`battery_*` tests)
- Swift lock: `DevCanopyTests/HostSnapshotWireContractTests.swift` (`testSharedBatteryContractFixture*`)
