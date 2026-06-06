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

    // MARK: - Per-process CPU percentage

    /// Regression: PID reuse over a long uptime can leave a cached cumulative CPU
    /// counter that is *higher* than the new process's. The old code did
    /// `currentTime - previousTime` on UInt64, which underflowed and trapped
    /// (the overnight crash). A backwards counter must yield 0%, not a crash.
    func testCPUPercentBackwardsCounterDoesNotCrash() {
        let percent = SystemMonitorV2.cpuPercent(
            currentTime: 1_000,
            previousTime: 5_000_000_000,
            elapsedSeconds: 1.0,
            coreCount: 10
        )
        XCTAssertEqual(percent, 0.0, "Reused PID (counter went backwards) must report 0%, not crash")
    }

    func testCPUPercentNoPreviousSampleIsZero() {
        let percent = SystemMonitorV2.cpuPercent(
            currentTime: 1_000_000_000,
            previousTime: nil,
            elapsedSeconds: 1.0,
            coreCount: 10
        )
        XCTAssertEqual(percent, 0.0, "First sample has no baseline to diff against")
    }

    func testCPUPercentNormalUsage() {
        // 1.0s of CPU time over 2.0s of wall clock = 50%.
        let percent = SystemMonitorV2.cpuPercent(
            currentTime: 3_000_000_000,
            previousTime: 2_000_000_000,
            elapsedSeconds: 2.0,
            coreCount: 10
        )
        XCTAssertEqual(percent, 50.0, accuracy: 0.001)
    }

    func testCPUPercentCapsAtAvailableCores() {
        // 10s of CPU in 1s of wall clock would be 1000%, capped to coreCount*100.
        let percent = SystemMonitorV2.cpuPercent(
            currentTime: 10_000_000_000,
            previousTime: 0,
            elapsedSeconds: 1.0,
            coreCount: 4
        )
        XCTAssertEqual(percent, 400.0, accuracy: 0.001, "Should cap at coreCount * 100%")
    }

    func testCPUPercentZeroElapsedIsZero() {
        let percent = SystemMonitorV2.cpuPercent(
            currentTime: 3_000_000_000,
            previousTime: 2_000_000_000,
            elapsedSeconds: 0.0,
            coreCount: 10
        )
        XCTAssertEqual(percent, 0.0, "No elapsed wall-clock time means no measurable usage")
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
