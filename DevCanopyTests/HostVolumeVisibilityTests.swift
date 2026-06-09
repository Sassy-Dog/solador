import XCTest
import HostMetricsKit
@testable import DevCanopy

/// `visibleVolumes` is the single source of truth the cockpit renders: the
/// debounced volume list minus the user's per-host hide list. These tests drive
/// it through `ingest(_:)` the way real collectors do.
@MainActor
final class HostVolumeVisibilityTests: XCTestCase {

    private func snapshot(volumes: [VolumeUsage]) -> HostSnapshot {
        HostSnapshot(
            timestamp: Date(),
            cpu: CPUMetrics(totalUsage: 1, coreUsages: [1], model: "Test", thermalState: .nominal),
            memory: MemoryMetrics(usedGB: 8, totalGB: 32, swapUsedGB: 0, pressure: 10),
            disk: DiskMetrics(readMBps: 1, writeMBps: 2),
            network: NetworkMetrics(downloadMBps: 3, uploadMBps: 4),
            gpu: GPUMetrics(usage: 5, vramUsedGB: 1, vramTotalGB: 16),
            battery: nil,
            volumes: volumes
        )
    }

    private func vol(_ mount: String) -> VolumeUsage {
        VolumeUsage(mount: mount, usedGB: 10, totalGB: 100)
    }

    private func service() -> HostMetricsService {
        HostMetricsService(hostName: "test", connectionState: .local)
    }

    func testFlappingVolumeNeverReachesVisibleVolumes() {
        let s = service()
        s.ingest(snapshot(volumes: [vol("/")]))
        for _ in 0..<10 {
            s.ingest(snapshot(volumes: [vol("/"), vol("/shared")]))
            s.ingest(snapshot(volumes: [vol("/")]))
        }
        XCTAssertEqual(s.visibleVolumes.map(\.mount), ["/"], "a mount flapping every poll must never surface")
    }

    func testStablyPresentVolumeBecomesVisible() {
        let s = service()
        s.ingest(snapshot(volumes: [vol("/")]))
        for _ in 0..<VolumeStabilizer.defaultThreshold {
            s.ingest(snapshot(volumes: [vol("/"), vol("/data")]))
        }
        XCTAssertEqual(s.visibleVolumes.map(\.mount), ["/", "/data"])
    }

    func testHiddenMountIsExcludedFromVisibleVolumes() {
        let s = service()
        s.ingest(snapshot(volumes: [vol("/"), vol("/boring")]))
        s.hideVolume("/boring")
        XCTAssertEqual(s.visibleVolumes.map(\.mount), ["/"])
        s.unhideVolume("/boring")
        XCTAssertEqual(s.visibleVolumes.map(\.mount), ["/", "/boring"])
    }

    // MARK: Local host persistence (UserDefaults)

    func testLocalServiceLoadsAndPersistsHiddenMountsViaUserDefaults() {
        let defaults = UserDefaults(suiteName: "HostVolumeVisibilityTests")!
        defaults.removePersistentDomain(forName: "HostVolumeVisibilityTests")
        defaults.set(["/seeded"], forKey: LocalHostMetricsService.hiddenMountsDefaultsKey)

        let s = LocalHostMetricsService(hostName: "test", defaults: defaults)
        XCTAssertEqual(s.hiddenMounts, ["/seeded"], "stored hide list loads on init")

        s.hideVolume("/extra")
        let stored = defaults.stringArray(forKey: LocalHostMetricsService.hiddenMountsDefaultsKey) ?? []
        XCTAssertEqual(Set(stored), ["/seeded", "/extra"], "hiding writes back to defaults")

        s.unhideVolume("/seeded")
        let after = defaults.stringArray(forKey: LocalHostMetricsService.hiddenMountsDefaultsKey) ?? []
        XCTAssertEqual(after, ["/extra"])
    }

    func testHideAndUnhidePersistTheUpdatedSet() {
        let s = service()
        var persisted: [Set<String>] = []
        s.persistHiddenMounts = { persisted.append($0) }

        s.hideVolume("/boring")
        s.hideVolume("/tmp2")
        s.unhideVolume("/boring")

        XCTAssertEqual(persisted, [["/boring"], ["/boring", "/tmp2"], ["/tmp2"]])
    }
}
