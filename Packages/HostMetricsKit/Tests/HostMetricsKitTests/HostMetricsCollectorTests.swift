import Foundation
@testable import HostMetricsKit
import XCTest

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

    func testMemoryUsedDoesNotExceedTotal() async throws {
        let collector = HostMetricsCollector()
        let snapshot = await collector.collectSnapshot()

        XCTAssertGreaterThan(snapshot.memory.totalGB, 0, "Total memory must be positive")
        let used = try XCTUnwrap(snapshot.memory.usedGB, "a machine whose mach read works reports a number")
        XCTAssertGreaterThanOrEqual(used, 0)
        XCTAssertLessThanOrEqual(used, snapshot.memory.totalGB, "Used memory must not exceed total")
    }

    // MARK: - Memory: a failed read is unknown, never invented (#204)

    private var sampleReading: MemoryReading {
        MemoryReading(usedGB: 12.0, totalGB: 32.0, swapUsedGB: 0.5, pressure: 7.0)
    }

    private var readFailure: MonitoringResult<MemoryReading> {
        .failure(.ioKitError(KERN_FAILURE, "host_statistics64"))
    }

    /// The bug: a failed `host_statistics64` used to publish
    /// `usedMemory = totalMemory * 0.5` with swap and pressure defaulted to 0 —
    /// a whole memory panel invented from a constant. Every figure that read
    /// would have produced is now unknown.
    func testFailedMemoryReadIsUnknownNotHalfOfTotal() {
        let failure = readFailure
        let monitor = SystemMonitorV2(memoryReader: { failure })

        let data = monitor.getMemoryData()

        XCTAssertNil(data.usedMemory, "never total * 0.5")
        XCTAssertNil(data.swapUsed, "swap came from the same failed read")
        XCTAssertNil(data.pressure, "so did pressure")
        XCTAssertNil(data.usagePercentage, "and nothing downstream can derive a percentage from nothing")
        // Capacity comes from ProcessInfo, a different and infallible source, so
        // the failure does not take it away.
        XCTAssertEqual(
            data.totalMemory,
            Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824,
            accuracy: 0.001,
            "physical memory is still measured"
        )
    }

    /// The other half: a real reading is passed through untouched, and a real
    /// zero stays zero rather than being mistaken for the unknown case.
    func testSuccessfulMemoryReadPassesThroughIncludingRealZeros() throws {
        let reading = sampleReading
        let monitor = SystemMonitorV2(memoryReader: { .success(reading) })

        let data = monitor.getMemoryData()

        XCTAssertEqual(try XCTUnwrap(data.usedMemory), 12.0, accuracy: 0.001)
        XCTAssertEqual(data.totalMemory, 32.0, accuracy: 0.001)
        XCTAssertEqual(try XCTUnwrap(data.swapUsed), 0.5, accuracy: 0.001)
        XCTAssertEqual(try XCTUnwrap(data.pressure), 7.0, accuracy: 0.001)
        XCTAssertEqual(try XCTUnwrap(data.usagePercentage), 12.0 / 32.0 * 100, accuracy: 0.001)

        let idleReading = MemoryReading(usedGB: 0, totalGB: 32, swapUsedGB: 0, pressure: 0)
        let idle = SystemMonitorV2(memoryReader: { .success(idleReading) })
        let idleData = idle.getMemoryData()
        XCTAssertEqual(try XCTUnwrap(idleData.swapUsed), 0.0, "an unused swap file is a measurement")
        XCTAssertEqual(try XCTUnwrap(idleData.pressure), 0.0, "so is an idle machine's pressure")
        XCTAssertEqual(try XCTUnwrap(idleData.usagePercentage), 0.0)
    }

    /// Collection runs at 1 Hz and a broken mach call fails every tick, so the
    /// failure is logged on the transition — the same rule the unmeasured-fields
    /// log follows. This asserts the state that gates it.
    func testMemoryFailureIsRecordedOnTransitionOnly() {
        final class Switch {
            var failing = false
        }
        let toggle = Switch()
        let reading = sampleReading
        let failure = readFailure
        let monitor = SystemMonitorV2(memoryReader: {
            toggle.failing ? failure : .success(reading)
        })

        _ = monitor.getMemoryData()
        XCTAssertFalse(monitor.memoryReadIsFailing, "a working read has nothing to report")

        toggle.failing = true
        _ = monitor.getMemoryData()
        XCTAssertTrue(monitor.memoryReadIsFailing, "the first failure is the one that logs")
        _ = monitor.getMemoryData()
        XCTAssertTrue(monitor.memoryReadIsFailing, "still failing — no second line")

        toggle.failing = false
        _ = monitor.getMemoryData()
        XCTAssertFalse(monitor.memoryReadIsFailing, "recovery clears it, and logs once")
        _ = monitor.getMemoryData()
        XCTAssertFalse(monitor.memoryReadIsFailing)
    }

    func testThermalStateIsPopulated() async {
        let collector = HostMetricsCollector()
        let snapshot = await collector.collectSnapshot()

        XCTAssertEqual(snapshot.cpu.thermalState, expectedThermalState())
    }

    func testSnapshotTimestampIsRecent() async {
        let collector = HostMetricsCollector()
        let snapshot = await collector.collectSnapshot()

        // Generous window: this guards against a wildly-wrong timestamp (e.g. an
        // epoch/uninitialized value), not clock precision. A tight bound flakes on a
        // cold CI runner where the first snapshot collection can be slow; 30s still
        // catches the failure mode while tolerating a sluggish runner.
        XCTAssertLessThan(
            abs(snapshot.timestamp.timeIntervalSinceNow),
            30,
            "Snapshot timestamp should be recent (within 30 seconds of now)"
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
            currentTime: 1000,
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
