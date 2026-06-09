import XCTest
import HostMetricsKit
@testable import DevCanopy

final class VolumeStabilizerTests: XCTestCase {

    private func vol(_ mount: String, usedGB: Double = 10) -> VolumeUsage {
        VolumeUsage(mount: mount, usedGB: usedGB, totalGB: 100)
    }

    private func mounts(_ volumes: [VolumeUsage]) -> [String] {
        volumes.map(\.mount)
    }

    // MARK: Seeding

    func testFirstObservationSeedsVisibleSetImmediately() {
        var stabilizer = VolumeStabilizer()
        let result = stabilizer.observe([vol("/"), vol("/data")])
        XCTAssertEqual(mounts(result), ["/", "/data"], "launch must not show an empty Volumes section for threshold polls")
    }

    // MARK: Appearance hysteresis

    func testNewVolumeAppearsOnlyAfterThresholdConsecutiveObservations() {
        var stabilizer = VolumeStabilizer(threshold: 3)
        _ = stabilizer.observe([vol("/")])
        XCTAssertEqual(mounts(stabilizer.observe([vol("/"), vol("/new")])), ["/"])
        XCTAssertEqual(mounts(stabilizer.observe([vol("/"), vol("/new")])), ["/"])
        XCTAssertEqual(mounts(stabilizer.observe([vol("/"), vol("/new")])), ["/", "/new"])
    }

    func testInterruptedPresenceResetsAppearanceStreak() {
        var stabilizer = VolumeStabilizer(threshold: 3)
        _ = stabilizer.observe([vol("/")])
        _ = stabilizer.observe([vol("/"), vol("/new")])
        _ = stabilizer.observe([vol("/"), vol("/new")])
        _ = stabilizer.observe([vol("/")])                                    // gap — streak resets
        XCTAssertEqual(mounts(stabilizer.observe([vol("/"), vol("/new")])), ["/"])
        XCTAssertEqual(mounts(stabilizer.observe([vol("/"), vol("/new")])), ["/"])
        XCTAssertEqual(mounts(stabilizer.observe([vol("/"), vol("/new")])), ["/", "/new"])
    }

    // MARK: Disappearance hysteresis

    func testVisibleVolumeSurvivesBriefAbsence() {
        var stabilizer = VolumeStabilizer(threshold: 3)
        _ = stabilizer.observe([vol("/"), vol("/data")])
        XCTAssertEqual(mounts(stabilizer.observe([vol("/")])), ["/", "/data"])
        XCTAssertEqual(mounts(stabilizer.observe([vol("/")])), ["/", "/data"])
        // Reappears before the threshold — absence streak resets.
        XCTAssertEqual(mounts(stabilizer.observe([vol("/"), vol("/data")])), ["/", "/data"])
        XCTAssertEqual(mounts(stabilizer.observe([vol("/")])), ["/", "/data"])
    }

    func testVisibleVolumeDisappearsAfterThresholdConsecutiveMisses() {
        var stabilizer = VolumeStabilizer(threshold: 3)
        _ = stabilizer.observe([vol("/"), vol("/data")])
        _ = stabilizer.observe([vol("/")])
        _ = stabilizer.observe([vol("/")])
        XCTAssertEqual(mounts(stabilizer.observe([vol("/")])), ["/"])
    }

    // MARK: Flapping (the actual bug)

    func testFlappingMountNeverAppearsAndNeverDuplicates() {
        var stabilizer = VolumeStabilizer(threshold: 3)
        _ = stabilizer.observe([vol("/")])
        // /shared alternates present/absent every poll — must never surface.
        for _ in 0..<10 {
            XCTAssertEqual(mounts(stabilizer.observe([vol("/"), vol("/shared")])), ["/"])
            XCTAssertEqual(mounts(stabilizer.observe([vol("/")])), ["/"])
        }
    }

    // MARK: Value freshness

    func testVisibleVolumeValuesUpdateEveryPoll() {
        var stabilizer = VolumeStabilizer(threshold: 3)
        _ = stabilizer.observe([vol("/", usedGB: 10)])
        let result = stabilizer.observe([vol("/", usedGB: 42)])
        XCTAssertEqual(result.first?.usedGB, 42, "stale data must not be served while a volume is visible")
    }

    func testReappearedVolumeWithinAbsenceWindowServesFreshValues() {
        var stabilizer = VolumeStabilizer(threshold: 3)
        _ = stabilizer.observe([vol("/"), vol("/data", usedGB: 10)])
        _ = stabilizer.observe([vol("/")])
        let result = stabilizer.observe([vol("/"), vol("/data", usedGB: 55)])
        XCTAssertEqual(result.first { $0.mount == "/data" }?.usedGB, 55)
    }
}
