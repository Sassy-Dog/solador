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
      "battery": null
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

        let comps = Calendar(identifier: .gregorian).dateComponents(
            in: TimeZone(identifier: "UTC")!, from: snap.timestamp)
        XCTAssertEqual(comps.year, 2026)
        XCTAssertEqual(comps.hour, 22)
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
