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

## `snapshot_unknowns.json`

A post-#183 snapshot from a producer that cannot measure everything: the keys it
has no reading for are **omitted**, never sent as `null` and never faked as `0`.
Here that is `cpu.thermalState`, `memory.pressure`, both `disk` rates and every
`gpu` field (both objects present but empty), plus a zero-capacity volume. Both
decoders must read an absent key as *unknown* — the distinction that keeps a
green `Pressure: 0%` off a card for a figure Linux never reports.

`snapshot.json`'s counterpart (the all-keys-present payload, including the
literal zeros pre-#183 agents send) stays the backward-compatibility case.

- Rust lock: `crates/wire/tests/wire.rs`, via its own byte-identical copy at
  `crates/wire/tests/fixtures/snapshot-unknowns.json` — the wire crate's tests
  read fixtures from inside their own crate. The Swift lock below asserts the
  two files decode to the same snapshot, so the copy cannot drift; folding them
  onto this one file is a follow-up (#192 was a Swift-only change).
- Swift lock: `DevCanopyTests/HostSnapshotWireContractTests.swift`
  (`testSharedUnknownsFixture*`)
