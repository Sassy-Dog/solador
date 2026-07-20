@testable import DevCanopy
import XCTest

final class GHRunnerRosterTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 4_000_000)

    private func runner(_ name: String, os: RunnerOS = .macOS, state: RunnerState = .idle) -> GHRunner {
        GHRunner(id: name.hashValue, name: name, os: os, state: state)
    }

    private func entry(_ name: String, os: RunnerOS = .macOS, absentFor: TimeInterval) -> RunnerRosterEntry {
        RunnerRosterEntry(name: name, os: os, lastSeen: now.addingTimeInterval(-absentFor))
    }

    // MARK: - updated(roster:registered:now:)

    func testUpdatedLearnsAndRefreshesRegisteredRunners() {
        let roster = GHRunnerRosterLogic.updated(
            roster: [entry("mac-s1", absentFor: 900)],
            registered: [runner("mac-s1"), runner("ubu-1", os: .linux)],
            now: now
        )
        XCTAssertEqual(roster.map(\.name), ["mac-s1", "ubu-1"])
        XCTAssertEqual(roster.map(\.lastSeen), [now, now], "every registered name is a sighting")
    }

    func testUpdatedKeepsAbsentEntriesUnchanged() {
        let absent = entry("mac-s2", absentFor: 120)
        let roster = GHRunnerRosterLogic.updated(roster: [absent], registered: [], now: now)
        XCTAssertEqual(roster, [absent], "a de-registered ephemeral runner stays remembered")
    }

    func testUpdatedAgesOutEntriesAbsentBeyondADay() {
        let roster = GHRunnerRosterLogic.updated(
            roster: [entry("stale", absentFor: 86400), entry("fresh", absentFor: 86399)],
            registered: [],
            now: now
        )
        XCTAssertEqual(roster.map(\.name), ["fresh"], "24h absent → silently forgotten (rotated hash names)")
    }

    // MARK: - absences(roster:registered:now:)

    func testAbsencesEmptyWhenEverythingRegistered() {
        let absences = GHRunnerRosterLogic.absences(
            roster: [entry("mac-s1", absentFor: 0)],
            registered: [runner("mac-s1")],
            now: now
        )
        XCTAssertTrue(absences.isEmpty)
    }

    func testAbsencesEscalateRecyclingToMissingAcrossGrace() {
        let recycling = GHRunnerRosterLogic.absences(
            roster: [entry("mac-s2", absentFor: 90)], registered: [], now: now
        )
        XCTAssertEqual(recycling.first?.state, .recycling(absence: 90))

        let missing = GHRunnerRosterLogic.absences(
            roster: [entry("mac-s2", absentFor: 420)], registered: [], now: now
        )
        XCTAssertEqual(missing.first?.state, .missing(absence: 420))
    }

    func testAbsencesCarryTheRosterOS() {
        let absences = GHRunnerRosterLogic.absences(
            roster: [entry("ubu-x", os: .linux, absentFor: 60)], registered: [], now: now
        )
        XCTAssertEqual(absences.first?.os, .linux)
    }

    // MARK: - Display rows

    func testDisplayRowsInterleaveByOSRankThenName() {
        let rows = GHRunnerRosterLogic.displayRows(
            registered: [runner("mac-s1"), runner("ubu-2", os: .linux)],
            absent: [
                GHRunnerAbsence(name: "mac-s2", os: .macOS, state: .recycling(absence: 60)),
                GHRunnerAbsence(name: "ubu-1", os: .linux, state: .missing(absence: 400))
            ]
        )
        XCTAssertEqual(
            rows.map(\.id),
            ["mac-s1", "mac-s2", "ubu-1", "ubu-2"],
            "macOS before Linux, name within — absent rows hold their slot"
        )
    }

    // MARK: - Persistence shape

    func testRosterEntryRoundTripsThroughJSON() throws {
        let original = [entry("mac-s1", os: .macOS, absentFor: 0), entry("ubu-1", os: .linux, absentFor: 60)]
        let data = try JSONEncoder().encode(original)
        let decoded = try JSONDecoder().decode([RunnerRosterEntry].self, from: data)
        XCTAssertEqual(decoded, original)
    }
}
