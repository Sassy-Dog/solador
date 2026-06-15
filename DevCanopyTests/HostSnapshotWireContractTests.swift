@testable import DevCanopy
import HostMetricsKit
import XCTest

/// Locks the wire contract: the Rust agent emits this exact JSON, and the client
/// must decode it into `HostSnapshot` using the shared decoder. If the agent's keys
/// or the decoder config drift, this test fails.
final class HostSnapshotWireContractTests: XCTestCase {
    /// The canonical agent payload (kept identical to the Rust agent's contract test).
    private let canonicalJSON = """
    {
      "timestamp": "2026-06-04T22:00:00Z",
      "cpu": { "totalUsage": 37.5, "coreUsages": [40.0, 35.0, 50.0, 25.0], "model": "Apple M1 Max", "thermalState": 0 },
      "memory": { "usedGB": 12.3, "totalGB": 32.0, "swapUsedGB": 0.5, "pressure": 0.0 },
      "disk": { "readMBps": 1.2, "writeMBps": 0.3 },
      "network": { "downloadMBps": 0.2, "uploadMBps": 0.1 },
      "gpu": { "usage": 0.0, "vramUsedGB": 0.0, "vramTotalGB": 0.0 },
      "battery": null,
      "volumes": [{ "mount": "/", "usedGB": 10.0, "totalGB": 100.0, "fstype": "ext4" }],
      "processes": [{ "pid": 123, "name": "node", "cpuPercent": 12.5, "memoryMB": 256.0 }]
    }
    """

    func testDecodesCanonicalAgentPayload() throws {
        let data = Data(canonicalJSON.utf8)
        let snap = try RemoteHostMetricsService.snapshotDecoder.decode(HostSnapshot.self, from: data)

        XCTAssertEqual(snap.cpu.totalUsage, 37.5, accuracy: 0.001)
        XCTAssertEqual(snap.cpu.coreUsages.count, 4)
        XCTAssertEqual(snap.cpu.model, "Apple M1 Max")
        XCTAssertEqual(snap.cpu.thermalState, .nominal) // 0
        XCTAssertEqual(snap.memory.usedGB, 12.3, accuracy: 0.001)
        XCTAssertEqual(snap.memory.totalGB, 32.0, accuracy: 0.001)
        XCTAssertEqual(snap.memory.usagePercentage, 12.3 / 32.0 * 100, accuracy: 0.001) // computed, not in JSON
        XCTAssertEqual(snap.disk.readMBps, 1.2, accuracy: 0.001)
        XCTAssertEqual(snap.network.uploadMBps, 0.1, accuracy: 0.001)
        XCTAssertEqual(snap.gpu.usage, 0.0, accuracy: 0.001)
        XCTAssertNil(snap.battery)
        XCTAssertEqual(snap.volumes.count, 1)
        XCTAssertEqual(snap.volumes.first?.mount, "/")
        XCTAssertEqual(snap.volumes.first?.fstype, "ext4")
        XCTAssertEqual(snap.volumes.first?.percentUsed ?? 0, 10.0, accuracy: 0.001) // 10/100, computed
        XCTAssertEqual(snap.processes.count, 1)
        XCTAssertEqual(snap.processes.first?.name, "node")
        XCTAssertEqual(snap.processes.first?.cpuPercent ?? 0, 12.5, accuracy: 0.001)
        XCTAssertEqual(snap.processes.first?.memoryMB ?? 0, 256.0, accuracy: 0.001)

        let comps = try Calendar(identifier: .gregorian).dateComponents(
            in: XCTUnwrap(TimeZone(identifier: "UTC")), from: snap.timestamp
        )
        XCTAssertEqual(comps.year, 2026)
        XCTAssertEqual(comps.hour, 22)
    }

    /// Backward compatibility: an older agent that predates `volumes` must still
    /// decode (the field is optional on the wire, defaulting to empty).
    func testDecodesLegacyPayloadWithoutVolumes() throws {
        let legacy = """
        {
          "timestamp": "2026-06-04T22:00:00Z",
          "cpu": { "totalUsage": 1.0, "coreUsages": [1.0], "model": "x", "thermalState": 0 },
          "memory": { "usedGB": 1.0, "totalGB": 8.0, "swapUsedGB": 0.0, "pressure": 0.0 },
          "disk": { "readMBps": 0.0, "writeMBps": 0.0 },
          "network": { "downloadMBps": 0.0, "uploadMBps": 0.0 },
          "gpu": { "usage": 0.0, "vramUsedGB": 0.0, "vramTotalGB": 0.0 },
          "battery": null
        }
        """
        let snap = try RemoteHostMetricsService.snapshotDecoder.decode(HostSnapshot.self, from: Data(legacy.utf8))
        XCTAssertEqual(snap.volumes, [], "missing volumes key decodes to empty, not a failure")
        XCTAssertEqual(snap.processes, [], "missing processes key decodes to empty too")
        XCTAssertEqual(snap.cpu.totalUsage, 1.0, accuracy: 0.001)
    }

    /// Backward compatibility: an agent that predates `fstype` emits volumes
    /// without the key — they must decode with `fstype == nil`.
    func testDecodesVolumeWithoutFstype() throws {
        let json = """
        [{ "mount": "/", "usedGB": 10.0, "totalGB": 100.0 }]
        """
        let volumes = try RemoteHostMetricsService.snapshotDecoder.decode([VolumeUsage].self, from: Data(json.utf8))
        XCTAssertEqual(volumes.first?.mount, "/")
        XCTAssertNil(volumes.first?.fstype, "missing fstype key decodes to nil, not a failure")
    }

    // MARK: - Shared battery contract fixture

    /// Absolute path to a repo-root fixture, derived from this test file's own
    /// location (`DevCanopyTests/…` → repo root). Keeps the fixture out of the
    /// test bundle's resources while still committing one file both languages read.
    private func repoFixtureURL(_ relativePath: String) -> URL {
        URL(fileURLWithPath: #filePath) // …/DevCanopyTests/HostSnapshotWireContractTests.swift
            .deletingLastPathComponent() // …/DevCanopyTests
            .deletingLastPathComponent() // …/  (repo root)
            .appendingPathComponent(relativePath)
    }

    /// CONTRACT LOCK (shared fixture): the non-null battery payload both sides
    /// agree on must decode into `BatteryMetrics` from the SAME committed JSON the
    /// Rust `battery_round_trips_shared_contract_fixture` test decodes. If the
    /// agent's keys or this decoder drift, exactly one suite fails on this file.
    func testSharedBatteryContractFixtureDecodes() throws {
        let url = repoFixtureURL("TestFixtures/battery_contract.json")
        let data = try Data(contentsOf: url)
        let battery = try RemoteHostMetricsService.snapshotDecoder.decode(BatteryMetrics.self, from: data)
        XCTAssertEqual(battery.level, 82.5, accuracy: 0.001)
        XCTAssertTrue(battery.isCharging)
        // Agent payload carries only the two contract keys — enrichment is nil.
        XCTAssertNil(battery.hasBattery)
        XCTAssertNil(battery.health)
        XCTAssertNil(battery.wattage)
    }

    /// A full `HostSnapshot` carrying the shared non-null battery payload (the
    /// path the always-`null` agent never exercised) round-trips end to end.
    func testSnapshotWithSharedBatteryPayloadDecodes() throws {
        let batteryJSON = try String(
            contentsOf: repoFixtureURL("TestFixtures/battery_contract.json"), encoding: .utf8
        )
        let json = """
        {
          "timestamp": "2026-06-04T22:00:00Z",
          "cpu": { "totalUsage": 1.0, "coreUsages": [1.0], "model": "x", "thermalState": 0 },
          "memory": { "usedGB": 1.0, "totalGB": 8.0, "swapUsedGB": 0.0, "pressure": 0.0 },
          "disk": { "readMBps": 0.0, "writeMBps": 0.0 },
          "network": { "downloadMBps": 0.0, "uploadMBps": 0.0 },
          "gpu": { "usage": 0.0, "vramUsedGB": 0.0, "vramTotalGB": 0.0 },
          "battery": \(batteryJSON)
        }
        """
        let snap = try RemoteHostMetricsService.snapshotDecoder.decode(
            HostSnapshot.self, from: Data(json.utf8)
        )
        XCTAssertEqual(snap.battery?.level, 82.5)
        XCTAssertEqual(snap.battery?.isCharging, true)
    }

    /// A battery object with unexpected keys (and missing the required contract
    /// keys) degrades to `battery == nil` rather than failing the whole snapshot —
    /// the core regression guard from issue #37.
    func testUnexpectedBatteryShapeDegradesToNil() throws {
        let json = """
        {
          "timestamp": "2026-06-04T22:00:00Z",
          "cpu": { "totalUsage": 1.0, "coreUsages": [1.0], "model": "x", "thermalState": 0 },
          "memory": { "usedGB": 1.0, "totalGB": 8.0, "swapUsedGB": 0.0, "pressure": 0.0 },
          "disk": { "readMBps": 0.0, "writeMBps": 0.0 },
          "network": { "downloadMBps": 0.0, "uploadMBps": 0.0 },
          "gpu": { "usage": 0.0, "vramUsedGB": 0.0, "vramTotalGB": 0.0 },
          "battery": { "percentage": 50, "plugged": "yes" }
        }
        """
        let snap = try RemoteHostMetricsService.snapshotDecoder.decode(
            HostSnapshot.self, from: Data(json.utf8)
        )
        XCTAssertNil(snap.battery, "unexpected battery shape degrades to nil, not a snapshot failure")
        XCTAssertEqual(snap.cpu.totalUsage, 1.0, accuracy: 0.001, "rest of snapshot still decodes")
    }

    func testProcessRankingUnionsTopCPUAndTopMemoryDeduped() {
        func p(_ pid: Int, cpu: Double, mem: Double) -> ProcessSample {
            ProcessSample(pid: pid, name: "p\(pid)", cpuPercent: cpu, memoryMB: mem)
        }
        // pid 1 tops CPU; pid 2 tops memory; pid 3 is middling (should drop at limit 2).
        let all = [p(1, cpu: 90, mem: 10), p(2, cpu: 5, mem: 900), p(3, cpu: 50, mem: 50), p(4, cpu: 1, mem: 1)]
        let top = ProcessRanking.top(all, limit: 2)
        let pids = Set(top.map(\.pid))
        XCTAssertTrue(pids.contains(1), "top CPU included")
        XCTAssertTrue(pids.contains(2), "top memory included even though low CPU")
        XCTAssertTrue(pids.contains(3), "second-by-cpu/second-by-mem included")
        XCTAssertFalse(pids.contains(4), "neither top-CPU nor top-mem dropped")
        XCTAssertEqual(top.first?.pid, 1, "result sorted by CPU desc")
        XCTAssertEqual(top.count, Set(top.map(\.pid)).count, "no duplicate pids")
    }

    func testDecodesRemoteContainersPayload() throws {
        let json = """
        [ { "name": "llm", "statusText": "Up 2 days", "isRunning": true, "runtime": "podman", "image": "llama-swap:latest" },
          { "name": "ci-runner_1", "statusText": "Up 3 days", "isRunning": true, "runtime": "podman", "image": null } ]
        """
        let containers = try RemoteHostMetricsService.snapshotDecoder.decode([ContainerInfo].self, from: Data(json.utf8))
        XCTAssertEqual(containers.count, 2)
        XCTAssertEqual(containers[0].name, "llm")
        XCTAssertTrue(containers[0].isRunning)
        XCTAssertEqual(containers[0].runtime, .podman)
        XCTAssertNil(containers[1].image)
    }
}
