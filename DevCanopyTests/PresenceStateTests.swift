@testable import DevCanopy
import XCTest

final class PresenceStateTests: XCTestCase {
    private let base = Date(timeIntervalSince1970: 1_000_000)

    private func state(absence: TimeInterval, grace: TimeInterval = Presence.defaultGrace) -> PresenceState {
        Presence.state(isPresent: false, lastSeen: base, now: base.addingTimeInterval(absence), grace: grace)
    }

    // MARK: - State machine

    func testPresentWinsRegardlessOfLastSeen() {
        let ancient = base.addingTimeInterval(-86400)
        XCTAssertEqual(Presence.state(isPresent: true, lastSeen: ancient, now: base), .present)
    }

    func testAbsenceJustUnderGraceIsRecycling() {
        XCTAssertEqual(state(absence: 299), .recycling(absence: 299))
    }

    func testAbsenceExactlyAtGraceIsMissing() {
        XCTAssertEqual(state(absence: 300), .missing(absence: 300))
    }

    func testAbsenceBeyondGraceIsMissing() {
        XCTAssertEqual(state(absence: 301), .missing(absence: 301))
    }

    func testBackwardClockSkewClampsToZeroAbsence() {
        XCTAssertEqual(state(absence: -50), .recycling(absence: 0))
    }

    func testCustomGraceIsRespected() {
        XCTAssertEqual(state(absence: 45, grace: 30), .missing(absence: 45))
        XCTAssertEqual(state(absence: 20, grace: 30), .recycling(absence: 20))
    }

    // MARK: - Labels

    func testPresentHasNoLabel() {
        XCTAssertNil(Presence.label(.present))
    }

    func testRecyclingLabelUsesSecondsUnderAMinute() {
        XCTAssertEqual(Presence.label(.recycling(absence: 40)), "recycling 40s")
    }

    func testRecyclingLabelUsesMinutesFromAMinute() {
        XCTAssertEqual(Presence.label(.recycling(absence: 180)), "recycling 3m")
    }

    func testMissingLabelUsesMinutesUnderAnHour() {
        XCTAssertEqual(Presence.label(.missing(absence: 720)), "missing 12m")
    }

    func testMissingLabelUsesHoursUnderADay() {
        XCTAssertEqual(Presence.label(.missing(absence: 7200)), "missing 2h")
    }

    func testMissingLabelUsesDaysBeyondADay() {
        XCTAssertEqual(Presence.label(.missing(absence: 200_000)), "missing 2d")
    }

    func testMissingLabelUsesSecondsUnderAMinuteWithTinyGrace() {
        XCTAssertEqual(Presence.label(.missing(absence: 45)), "missing 45s")
    }
}
