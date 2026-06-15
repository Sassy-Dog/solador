import XCTest
@testable import DevCanopy

final class DevCanopyTests: XCTestCase {

    func testRefreshIntervals() {
        XCTAssertEqual(RefreshInterval.thirtySeconds.rawValue, 30)
        XCTAssertEqual(RefreshInterval.oneMinute.rawValue, 60)
        XCTAssertEqual(RefreshInterval.fiveMinutes.rawValue, 300)
    }
}
