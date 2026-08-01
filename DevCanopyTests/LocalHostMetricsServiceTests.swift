@testable import DevCanopy
import HostMetricsKit
import XCTest

@MainActor
final class LocalHostMetricsServiceTests: XCTestCase {
    private func snapshot(cpu: Double, cores: [Double]) -> HostSnapshot {
        HostSnapshot(
            timestamp: Date(),
            cpu: CPUMetrics(totalUsage: cpu, coreUsages: cores, model: "Test", thermalState: .nominal),
            memory: MemoryMetrics(usedGB: 8, totalGB: 32, swapUsedGB: 0, pressure: 10),
            disk: DiskMetrics(readMBps: 1, writeMBps: 2),
            network: NetworkMetrics(downloadMBps: 3, uploadMBps: 4),
            gpu: GPUMetrics(usage: 5, vramUsedGB: 1, vramTotalGB: 16),
            battery: nil
        )
    }

    func testLatestSnapshotReflectsLastIngest() {
        let service = LocalHostMetricsService()
        service.ingest(snapshot(cpu: 42, cores: [10, 20]))
        XCTAssertEqual(service.snapshot?.cpu.totalUsage, 42)
    }

    func testCPUHistoryIsCappedAtCapacity() {
        let service = LocalHostMetricsService()
        let n = LocalHostMetricsService.historyCapacity + 50
        for i in 0 ..< n {
            service.ingest(snapshot(cpu: Double(i), cores: [Double(i)]))
        }
        XCTAssertEqual(service.cpuHistory.count, LocalHostMetricsService.historyCapacity)
        // Oldest points dropped; newest retained.
        XCTAssertEqual(service.cpuHistory.last, Double(n - 1))
    }

    func testCoreHistoriesTrackEachCore() {
        let service = LocalHostMetricsService()
        service.ingest(snapshot(cpu: 50, cores: [11, 22, 33]))
        service.ingest(snapshot(cpu: 60, cores: [44, 55, 66]))
        XCTAssertEqual(service.coreHistories.count, 3)
        XCTAssertEqual(service.coreHistories[1].last, 55)
    }

    func testIOHistoriesTrackDiskAndNetwork() {
        let service = LocalHostMetricsService()
        service.ingest(snapshot(cpu: 10, cores: [1]))
        XCTAssertEqual(service.diskReadHistory.last, 1) // snapshot() uses readMBps: 1
        XCTAssertEqual(service.diskWriteHistory.last, 2) // writeMBps: 2
        XCTAssertEqual(service.netDownHistory.last, 3) // downloadMBps: 3
        XCTAssertEqual(service.netUpHistory.last, 4) // uploadMBps: 4
    }

    // MARK: - Unmeasured samples never enter a chart

    /// A chart is a claim about measurements. A figure the host never reported
    /// must not be plotted as a zero — the buffer simply doesn't grow.
    func testUnmeasuredValuesAreNeverPlotted() {
        let service = LocalHostMetricsService()
        service.ingest(snapshot(cpu: 10, cores: [1]))
        service.ingest(HostSnapshot(
            timestamp: Date(),
            cpu: CPUMetrics(totalUsage: 20, coreUsages: [2], model: "Test", thermalState: nil),
            memory: MemoryMetrics(usedGB: 8, totalGB: 32, swapUsedGB: 0, pressure: nil),
            disk: .unknown,
            network: .unknown,
            gpu: .unknown,
            battery: nil
        ))
        XCTAssertEqual(service.diskReadHistory, [1], "the unmeasured second sample added nothing")
        XCTAssertEqual(service.netUpHistory, [4])
        XCTAssertEqual(service.cpuHistory, [10, 20], "measured series keep growing alongside")
    }

    /// A pre-#183 agent reports an absent GPU as a literal `usage: 0.0` on every
    /// sample. Gating on the reading alone would fill the buffer with zeros and
    /// draw a flat line under the em dash; capacity is what sees through it.
    func testAnAbsentGPUPlotsNothingEvenWhenItReportsZeroUsage() {
        let service = LocalHostMetricsService()
        for _ in 0 ..< 5 {
            service.ingest(HostSnapshot(
                timestamp: Date(),
                cpu: CPUMetrics(totalUsage: 1, coreUsages: [1], model: "Test", thermalState: .nominal),
                memory: MemoryMetrics(usedGB: 8, totalGB: 32, swapUsedGB: 0, pressure: 10),
                disk: DiskMetrics(readMBps: 1, writeMBps: 2),
                network: NetworkMetrics(downloadMBps: 3, uploadMBps: 4),
                // What agents predating #183 send for a host with no adapter.
                gpu: GPUMetrics(usage: 0, vramUsedGB: 0, vramTotalGB: 0),
                battery: nil
            ))
        }
        XCTAssertEqual(service.gpuHistory, [], "no adapter, no series")
        XCTAssertEqual(service.diskReadHistory.count, 5, "the rest of the sample still lands")
    }

    /// A present GPU sitting idle is a real 0% and must plot.
    func testAnIdleGPUStillPlotsItsZero() {
        let service = LocalHostMetricsService()
        service.ingest(snapshot(cpu: 10, cores: [1])) // vramTotalGB: 16, usage: 5
        service.ingest(HostSnapshot(
            timestamp: Date(),
            cpu: CPUMetrics(totalUsage: 1, coreUsages: [1], model: "Test", thermalState: .nominal),
            memory: MemoryMetrics(usedGB: 8, totalGB: 32, swapUsedGB: 0, pressure: 10),
            disk: DiskMetrics(readMBps: 1, writeMBps: 2),
            network: NetworkMetrics(downloadMBps: 3, uploadMBps: 4),
            gpu: GPUMetrics(usage: 0, vramUsedGB: 1, vramTotalGB: 16),
            battery: nil
        ))
        XCTAssertEqual(service.gpuHistory, [5, 0])
    }

    /// The log line behind the em dashes fires on the transition, not per poll —
    /// a Linux host omits the same fields every second.
    func testUnmeasuredSetIsTrackedForTransitionOnlyLogging() {
        let service = LocalHostMetricsService()
        service.ingest(snapshot(cpu: 10, cores: [1]))
        XCTAssertEqual(service.lastUnmeasuredFields, [], "a fully-measured host has nothing to explain")

        let blind = HostSnapshot(
            timestamp: Date(),
            cpu: CPUMetrics(totalUsage: 1, coreUsages: [1], model: "Test", thermalState: nil),
            memory: MemoryMetrics(usedGB: 8, totalGB: 32, swapUsedGB: 0, pressure: nil),
            disk: DiskMetrics(readMBps: 1, writeMBps: 2),
            network: NetworkMetrics(downloadMBps: 3, uploadMBps: 4),
            gpu: GPUMetrics(usage: 5, vramUsedGB: 1, vramTotalGB: 16),
            battery: nil
        )
        service.ingest(blind)
        XCTAssertEqual(service.lastUnmeasuredFields, ["cpu.thermalState", "memory.pressure"])
        service.ingest(blind)
        XCTAssertEqual(service.lastUnmeasuredFields, ["cpu.thermalState", "memory.pressure"], "unchanged, so nothing re-logged")
    }

    // MARK: - Lifecycle pause/resume (the burst fix's state machine)

    /// A test double that starts/stops a no-op task — exercises the base-class
    /// pause/resume state machine without real system collection or notifications.
    private final class FakeHostMetricsService: HostMetricsService {
        init() {
            super.init(hostName: "fake", connectionState: .local)
        }

        override func start(interval: TimeInterval = 1.0) {
            startInterval = interval
            guard task == nil else { return }
            task = Task {}
        }
    }

    func testLifecyclePauseStopsCollectionAndResumeRestartsFresh() {
        let s = FakeHostMetricsService()
        XCTAssertFalse(s.isCollecting)

        s.start(interval: 2)
        XCTAssertTrue(s.isCollecting)
        XCTAssertEqual(s.startInterval, 2)

        s.pauseForLifecycle()
        XCTAssertFalse(s.isCollecting, "paused = not collecting, so no backlog accrues while away")

        s.resumeForLifecycle()
        XCTAssertTrue(s.isCollecting, "resumes a fresh task at the remembered interval")

        s.stop()
    }

    func testResumeWithoutPriorPauseIsNoOp() {
        // A wake (didWake) before the app has ever started collection must not start it.
        let s = FakeHostMetricsService()
        s.resumeForLifecycle()
        XCTAssertFalse(s.isCollecting, "resume only restarts if we previously paused a running collector")
    }
}
