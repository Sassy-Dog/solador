@testable import DevCanopy
import XCTest

final class CockpitBreakpointsTests: XCTestCase {
    private let layout = CockpitLayout.hostsForward

    /// Rows as panel kinds — `CockpitPlacement` isn't Equatable, and the kinds are
    /// what the assertions are actually about.
    private func kinds(_ rows: [[CockpitPlacement]]) -> [[CockpitPanelKind]] {
        rows.map { $0.map(\.kind) }
    }

    // MARK: reflow

    /// The reported case: a ~1568pt portrait window (1528pt of content). Every
    /// authored pair still fits, so the cockpit rows must not move — only the host
    /// cards inside the Hosts panel stack. See `hostColumns` below.
    func testPortraitWidthLeavesEveryPanelRowPaired() {
        let reflowed = CockpitBreakpoints.reflow(layout.rows, available: 1528)

        XCTAssertEqual(kinds(reflowed), kinds(layout.rows), "no row should break at 1528pt")
    }

    /// At the 880pt window floor (840pt of content) the two hungrier pairs break
    /// apart, while Claude Usage + Azure Cost (360 + 16 + 400 = 776) stay paired.
    func testWindowFloorBreaksOnlyTheRowsThatDontFit() {
        let reflowed = CockpitBreakpoints.reflow(layout.rows, available: 840)

        XCTAssertEqual(kinds(reflowed), [
            [.hosts],
            [.ghWorkflows], // 560 + 16 + 400 = 976 > 840
            [.ghRunners],
            [.containers], // 400 + 16 + 440 = 856 > 840
            [.openclawAgents],
            [.claudeUsage, .azureCost] // 776 <= 840
        ])
    }

    /// Boundary: Repos + Runners need exactly 976pt.
    func testRowSplitsOnceBelowItsExactRequirement() {
        let atRequirement = CockpitBreakpoints.reflow(layout.rows, available: 976)
        XCTAssertEqual(kinds(atRequirement)[1], [.ghWorkflows, .ghRunners], "976pt is enough")

        let justUnder = CockpitBreakpoints.reflow(layout.rows, available: 975)
        XCTAssertEqual(kinds(justUnder)[1], [.ghWorkflows], "975pt is not")
    }

    /// Width unknown on first render — pass the authored rows through untouched
    /// rather than flashing a stacked layout.
    func testUnmeasuredWidthPassesRowsThrough() {
        XCTAssertEqual(kinds(CockpitBreakpoints.reflow(layout.rows, available: 0)), kinds(layout.rows))
    }

    /// A panel wider than the window still gets its own row rather than vanishing.
    func testPanelWiderThanAvailableStillRenders() {
        let reflowed = CockpitBreakpoints.reflow(layout.rows, available: 100)

        XCTAssertEqual(reflowed.count, CockpitPanelKind.allCases.count, "one panel per row")
        XCTAssertTrue(reflowed.allSatisfy { $0.count == 1 })
    }

    /// Reflow only ever splits rows: no panel is dropped, duplicated, or reordered,
    /// and no row comes back empty — at any width.
    func testNoPanelLostDuplicatedOrReordered() {
        let authored = layout.panelKinds

        for width in stride(from: 100 as CGFloat, through: 3000, by: 50) {
            let reflowed = CockpitBreakpoints.reflow(layout.rows, available: width)
            let flat = reflowed.flatMap { $0.map(\.kind) }

            XCTAssertEqual(flat, authored, "order and membership must survive at \(width)pt")
            XCTAssertFalse(reflowed.contains(where: \.isEmpty), "empty row at \(width)pt")
        }
    }

    /// Reflow never merges across authored rows, even when two would fit together —
    /// the layout's pairings are intent, not a packing suggestion.
    func testWideWidthsNeverMergeAuthoredRows() {
        XCTAssertEqual(kinds(CockpitBreakpoints.reflow(layout.rows, available: 5000)), kinds(layout.rows))
    }

    // MARK: hostColumns

    /// The bug this change fixes: two cards at 1528pt would be ~752pt each.
    func testTwoHostsStackOnAPortraitWindow() {
        XCTAssertEqual(CockpitBreakpoints.hostColumns(available: 1528, hostCount: 2), 1)
    }

    func testTwoHostsPairUpOnceBothCardsClearTheMinimum() {
        // 2 * 900 + 16 = 1816
        XCTAssertEqual(CockpitBreakpoints.hostColumns(available: 1815, hostCount: 2), 1)
        XCTAssertEqual(CockpitBreakpoints.hostColumns(available: 1816, hostCount: 2), 2)
        XCTAssertEqual(CockpitBreakpoints.hostColumns(available: 1880, hostCount: 2), 2)
    }

    func testThreeHostsNeedAnUltrawide() {
        // 3 * 900 + 2 * 16 = 2732
        XCTAssertEqual(CockpitBreakpoints.hostColumns(available: 2731, hostCount: 3), 2)
        XCTAssertEqual(CockpitBreakpoints.hostColumns(available: 2732, hostCount: 3), 3)
    }

    func testColumnsNeverExceedHostCountOrDropBelowOne() {
        XCTAssertEqual(CockpitBreakpoints.hostColumns(available: 5000, hostCount: 1), 1)
        XCTAssertEqual(CockpitBreakpoints.hostColumns(available: 5000, hostCount: 2), 2)
        XCTAssertEqual(CockpitBreakpoints.hostColumns(available: 100, hostCount: 4), 1)
        XCTAssertEqual(CockpitBreakpoints.hostColumns(available: 1000, hostCount: 0), 1)
    }

    /// Regression guard: an unknown width must NOT be treated as "wide enough".
    /// Assuming wide is what let a dead width reading masquerade as a deliberate
    /// (unreadable) side-by-side layout. Stacking always reads.
    func testUnknownWidthStacksRatherThanAssumingWide() {
        XCTAssertEqual(CockpitBreakpoints.hostColumns(available: 0, hostCount: 3), 1)
    }

    // MARK: rows

    func testRowsChunkByColumnCount() {
        XCTAssertEqual(CockpitBreakpoints.rows([1, 2, 3, 4], columns: 2), [[1, 2], [3, 4]])
        XCTAssertEqual(CockpitBreakpoints.rows([1, 2, 3], columns: 2), [[1, 2], [3]], "short last row")
        XCTAssertEqual(CockpitBreakpoints.rows([1, 2, 3], columns: 3), [[1, 2, 3]])
    }

    func testSingleColumnGivesOneItemPerRow() {
        XCTAssertEqual(CockpitBreakpoints.rows([1, 2, 3], columns: 1), [[1], [2], [3]])
        XCTAssertEqual(CockpitBreakpoints.rows([1, 2], columns: 0), [[1], [2]], "0 columns degrades to stacked")
    }

    func testRowsOfNothingIsNoRows() {
        XCTAssertTrue(CockpitBreakpoints.rows([Int](), columns: 2).isEmpty)
    }
}
