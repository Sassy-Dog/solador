# Shared wire-contract fixtures

JSON fixtures that lock the wire contract between the agent (`agent/`) and the
app's decoder (`crates/wire`, `crates/localhost`) so the two cannot silently
drift. They live here, above both, because `agent/` is a separate Cargo
workspace — neither side can own a file the other must agree with.

Each fixture is decoded by tests on **both** sides. Changing the shape requires
updating both, and both suites fail until they agree.

## `battery_contract.json`

The canonical battery wire shape emitted under `snapshot.battery` by the agent.
It is the minimal cross-platform contract — `level` (0–100) and `isCharging` —
the only two fields a generic host agent (Linux/`sysinfo`) can produce. A
macOS-local collector additionally populates richer optional fields (`health`,
`cycleCount`, `wattage`, …); those are decode-optional and never part of this
floor.

- Agent lock: `agent/src/metrics.rs` (`battery_*` tests)
- App lock: `crates/wire/tests/wire.rs`
  (`battery_deserialises_from_shared_contract_fixture`)

## `snapshot_unknowns.json`

A post-#183 snapshot from a producer that cannot measure everything: the keys it
has no reading for are **omitted**, never sent as `null` and never faked as `0`.
Here that is `cpu.thermalState`, `memory.pressure`, both `disk` rates and every
`gpu` field (both objects present but empty), plus a zero-capacity volume. Both
decoders must read an absent key as *unknown* — the distinction that keeps a
green `Pressure: 0%` off a card for a figure Linux never reports.

`snapshot.json`'s counterpart (the all-keys-present payload, including the
literal zeros pre-#183 agents send) stays the backward-compatibility case.

- App lock: `crates/wire/tests/wire.rs`, via a byte-identical copy at
  `crates/wire/tests/fixtures/snapshot-unknowns.json` — that crate's tests
  `include_str!` their fixtures, so the copy has to exist.

  **The copy is guarded by
  `the_local_unknowns_fixture_is_byte_identical_to_the_shared_one`.** That guard
  is new, and it exists because the previous one was a Swift test that left CI
  when the Swift app was frozen (2026-08-04) and vanished when it was deleted.
  In the gap the two files did drift — the rename updated this file's
  `solador-agent` process name and left the copy on `devcanopy-agent`, and
  nothing failed, because nothing was still checking. Folding the two onto one
  file remains the real fix (#192).
