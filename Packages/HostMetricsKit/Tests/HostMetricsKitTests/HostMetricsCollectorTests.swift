import Foundation
import XCTest
@testable import HostMetricsKit

final class HostMetricsCollectorTests: XCTestCase {
    /// Maps the live `ProcessInfo` thermal state into the package's enum,
    /// mirroring the collector's own mapping so the test stays in lockstep.
    private func expectedThermalState() -> ThermalState {
        switch ProcessInfo.processInfo.thermalState {
        case .nominal: return .nominal
        case .fair: return .fair
        case .serious: return .serious
        case .critical: return .critical
        @unknown default: return .nominal
        }
    }

    func testSnapshotHasValidCPU() async {
        let collector = HostMetricsCollector()
        let snapshot = await collector.collectSnapshot()

        XCTAssertGreaterThan(snapshot.cpu.coreUsages.count, 0, "Expected at least one CPU core")
        XCTAssertFalse(snapshot.cpu.model.isEmpty, "Expected a non-empty CPU model")
        XCTAssertGreaterThanOrEqual(snapshot.cpu.totalUsage, 0)
        for usage in snapshot.cpu.coreUsages {
            XCTAssertGreaterThanOrEqual(usage, 0, "Per-core usage must be non-negative")
        }
    }

    func testMemoryUsedDoesNotExceedTotal() async {
        let collector = HostMetricsCollector()
        let snapshot = await collector.collectSnapshot()

        XCTAssertGreaterThan(snapshot.memory.totalGB, 0, "Total memory must be positive")
        XCTAssertGreaterThanOrEqual(snapshot.memory.usedGB, 0)
        XCTAssertLessThanOrEqual(
            snapshot.memory.usedGB,
            snapshot.memory.totalGB,
            "Used memory must not exceed total"
        )
    }

    func testThermalStateIsPopulated() async {
        let collector = HostMetricsCollector()
        let snapshot = await collector.collectSnapshot()

        XCTAssertEqual(snapshot.cpu.thermalState, expectedThermalState())
    }

    func testSnapshotTimestampIsRecent() async {
        let collector = HostMetricsCollector()
        let snapshot = await collector.collectSnapshot()

        XCTAssertLessThan(
            abs(snapshot.timestamp.timeIntervalSinceNow),
            5,
            "Snapshot timestamp should be within 5 seconds of now"
        )
    }

    func testCodableRoundTrip() async throws {
        let collector = HostMetricsCollector()
        let snapshot = await collector.collectSnapshot()

        let encoder = JSONEncoder()
        let decoder = JSONDecoder()
        let data = try encoder.encode(snapshot)
        let decoded = try decoder.decode(HostSnapshot.self, from: data)

        XCTAssertEqual(snapshot, decoded, "Snapshot should survive a JSON round trip unchanged")
    }

    func testStreamYieldsSnapshots() async {
        let collector = HostMetricsCollector()
        var received = 0

        for await _ in collector.snapshots(interval: 0.1) {
            received += 1
            if received == 2 { break }
        }

        XCTAssertEqual(received, 2, "Expected to receive exactly 2 snapshots from the stream")
    }
}
