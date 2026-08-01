@testable import DevCanopy
import HostMetricsKit
import XCTest

/// Locks the demo data source's invariants: snapshots stay in plausible ranges,
/// the warmup fills the full history window, and profile shape (cores, battery,
/// volumes) is honored — so demo screenshots don't silently degrade.
final class DemoSnapshotGeneratorTests: XCTestCase {
    /// The demo source stands in for a fully-instrumented host: it measures
    /// everything, so every Optional the wire contract allows to be unknown must
    /// arrive populated here. A nil would mean a demo card full of em dashes.
    func testSnapshotValuesStayInPlausibleRanges() throws {
        let gen = DemoSnapshotGenerator(profile: .localMac)
        // Sweep a range of phases to exercise the periodic components.
        for i in 0 ..< 200 {
            let snap = gen.snapshot(at: Double(i) * DemoSnapshotGenerator.step)
            let gpuUsage = try XCTUnwrap(snap.gpu.usage)
            let pressure = try XCTUnwrap(snap.memory.pressure)
            XCTAssert((0 ... 100).contains(snap.cpu.totalUsage), "cpu out of range: \(snap.cpu.totalUsage)")
            XCTAssert((0 ... 100).contains(gpuUsage), "gpu out of range: \(gpuUsage)")
            XCTAssert((0 ... 100).contains(snap.memory.usagePercentage), "mem out of range")
            XCTAssert((0 ... 100).contains(pressure), "pressure out of range")
            XCTAssertGreaterThanOrEqual(try XCTUnwrap(snap.disk.readMBps), 0)
            XCTAssertGreaterThanOrEqual(try XCTUnwrap(snap.disk.writeMBps), 0)
            XCTAssertGreaterThanOrEqual(try XCTUnwrap(snap.network.downloadMBps), 0)
            XCTAssertGreaterThanOrEqual(try XCTUnwrap(snap.network.uploadMBps), 0)
            XCTAssertLessThanOrEqual(try XCTUnwrap(snap.gpu.vramUsedGB), try XCTUnwrap(snap.gpu.vramTotalGB))
            XCTAssertNotNil(snap.cpu.thermalState, "a demo host measures its thermal state")
        }
    }

    func testProfileShapeIsHonored() {
        let snap = DemoSnapshotGenerator(profile: .localMac).snapshot(at: 0)
        XCTAssertEqual(snap.cpu.coreUsages.count, DemoHostProfile.localMac.cpuCores)
        XCTAssertEqual(snap.cpu.model, "Apple M4 Max")
        XCTAssertNotNil(snap.battery, "laptop profile should report a battery")
        XCTAssertFalse(snap.volumes.isEmpty)

        let server = DemoSnapshotGenerator(profile: .remoteLinux).snapshot(at: 0)
        XCTAssertEqual(server.cpu.coreUsages.count, DemoHostProfile.remoteLinux.cpuCores)
        XCTAssertNil(server.battery, "server profile should not report a battery")
    }

    func testWarmupFillsFullHistoryWindow() {
        let gen = DemoSnapshotGenerator(profile: .remoteLinux)
        let result = gen.warmup(samples: HostMetricsService.historyCapacity)
        XCTAssertEqual(result.snapshots.count, HostMetricsService.historyCapacity)
        // Warmup is oldest-first, ending just before the live stream's continuation phase.
        XCTAssertEqual(result.nextPhase, 0, accuracy: 1e-9)
    }

    @MainActor
    func testDemoServicePreSeedsHistoryAndStreams() async {
        let service = DemoHostMetricsService(
            hostName: "demo", profile: .localMac, connectionState: .local
        )
        // History should be populated immediately from the warmup.
        XCTAssertEqual(service.cpuHistory.count, HostMetricsService.historyCapacity)
        XCTAssertNotNil(service.snapshot)

        let before = service.cpuHistory.count
        service.start(interval: 0.01)
        try? await Task.sleep(nanoseconds: 50_000_000)
        service.stop()
        // Capped at historyCapacity, but the snapshot should keep refreshing.
        XCTAssertEqual(service.cpuHistory.count, before)
        XCTAssertNotNil(service.snapshot)
    }
}
