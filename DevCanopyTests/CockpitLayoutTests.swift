@testable import DevCanopy
import XCTest

final class CockpitLayoutTests: XCTestCase {
    /// The shipped layout must render every panel exactly once — guards against
    /// dropping or duplicating a panel when arranging rows.
    func testHostsForwardLayoutContainsEveryPanelExactlyOnce() {
        let kinds = CockpitLayout.hostsForward.panelKinds.sorted { $0.rawValue < $1.rawValue }
        let expected = CockpitPanelKind.allCases.sorted { $0.rawValue < $1.rawValue }

        XCTAssertEqual(kinds, expected, "hostsForward layout must place each panel kind exactly once")
    }

    func testLayoutRowsAreNonEmpty() {
        for row in CockpitLayout.hostsForward.rows {
            XCTAssertFalse(row.isEmpty, "a cockpit row must contain at least one panel")
        }
    }

    /// Every panel declares its own breakpoint. A new panel kind that forgets one
    /// would reflow against a zero minimum and never break out of a cramped row.
    func testEveryPanelKindDeclaresAPositiveMinWidth() {
        for kind in CockpitPanelKind.allCases {
            XCTAssertGreaterThan(kind.minWidth, 0, "\(kind.rawValue) must declare a minWidth")
        }
    }
}
