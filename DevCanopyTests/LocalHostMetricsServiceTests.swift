import XCTest
import HostMetricsKit
@testable import DevCanopy

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
        for i in 0..<n {
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
        XCTAssertEqual(service.diskReadHistory.last, 1)   // snapshot() uses readMBps: 1
        XCTAssertEqual(service.diskWriteHistory.last, 2)  // writeMBps: 2
        XCTAssertEqual(service.netDownHistory.last, 3)    // downloadMBps: 3
        XCTAssertEqual(service.netUpHistory.last, 4)      // uploadMBps: 4
    }
}
