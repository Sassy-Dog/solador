import XCTest
import HostMetricsKit
@testable import DevCanopy

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
      "volumes": [{ "mount": "/", "usedGB": 10.0, "totalGB": 100.0 }],
      "processes": [{ "pid": 123, "name": "node", "cpuPercent": 12.5, "memoryMB": 256.0 }]
    }
    """

    func testDecodesCanonicalAgentPayload() throws {
        let data = Data(canonicalJSON.utf8)
        let snap = try RemoteHostMetricsService.snapshotDecoder.decode(HostSnapshot.self, from: data)

        XCTAssertEqual(snap.cpu.totalUsage, 37.5, accuracy: 0.001)
        XCTAssertEqual(snap.cpu.coreUsages.count, 4)
        XCTAssertEqual(snap.cpu.model, "Apple M1 Max")
        XCTAssertEqual(snap.cpu.thermalState, .nominal)        // 0
        XCTAssertEqual(snap.memory.usedGB, 12.3, accuracy: 0.001)
        XCTAssertEqual(snap.memory.totalGB, 32.0, accuracy: 0.001)
        XCTAssertEqual(snap.memory.usagePercentage, 12.3 / 32.0 * 100, accuracy: 0.001)  // computed, not in JSON
        XCTAssertEqual(snap.disk.readMBps, 1.2, accuracy: 0.001)
        XCTAssertEqual(snap.network.uploadMBps, 0.1, accuracy: 0.001)
        XCTAssertEqual(snap.gpu.usage, 0.0, accuracy: 0.001)
        XCTAssertNil(snap.battery)
        XCTAssertEqual(snap.volumes.count, 1)
        XCTAssertEqual(snap.volumes.first?.mount, "/")
        XCTAssertEqual(snap.volumes.first?.percentUsed ?? 0, 10.0, accuracy: 0.001)  // 10/100, computed
        XCTAssertEqual(snap.processes.count, 1)
        XCTAssertEqual(snap.processes.first?.name, "node")
        XCTAssertEqual(snap.processes.first?.cpuPercent ?? 0, 12.5, accuracy: 0.001)
        XCTAssertEqual(snap.processes.first?.memoryMB ?? 0, 256.0, accuracy: 0.001)

        let comps = Calendar(identifier: .gregorian).dateComponents(
            in: TimeZone(identifier: "UTC")!, from: snap.timestamp)
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
