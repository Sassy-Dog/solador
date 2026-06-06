import XCTest
import HostMetricsKit
@testable import DevCanopy

final class VolumeGridLayoutTests: XCTestCase {

    private func vols(_ n: Int) -> [VolumeUsage] {
        (0..<n).map { VolumeUsage(mount: "/v\($0)", usedGB: Double($0), totalGB: 100) }
    }

    // MARK: rowCount

    func testRowCountSingleColumnIsOneRowPerVolume() {
        XCTAssertEqual(VolumeGridLayout.rowCount(1, columns: 1), 1)
        XCTAssertEqual(VolumeGridLayout.rowCount(5, columns: 1), 5)
        XCTAssertEqual(VolumeGridLayout.rowCount(0, columns: 1), 0)
    }

    func testRowCountTwoColumns() {
        XCTAssertEqual(VolumeGridLayout.rowCount(1, columns: 2), 1)  // single → one full-width row
        XCTAssertEqual(VolumeGridLayout.rowCount(2, columns: 2), 1)  // a pair
        XCTAssertEqual(VolumeGridLayout.rowCount(3, columns: 2), 2)  // full-width top + 1 pair
        XCTAssertEqual(VolumeGridLayout.rowCount(8, columns: 2), 4)  // four pairs
        XCTAssertEqual(VolumeGridLayout.rowCount(7, columns: 2), 4)  // full-width top + 3 pairs
    }

    // MARK: rows

    func testSingleVolumeIsOneFullWidthRow() {
        let rows = VolumeGridLayout.rows(vols(1), columns: 2)
        XCTAssertEqual(rows.count, 1)
        XCTAssertEqual(rows[0].count, 1, "a lone volume spans the row")
    }

    func testOddCountGivesMostImportantAFullWidthTopRow() {
        let rows = VolumeGridLayout.rows(vols(5), columns: 2)
        XCTAssertEqual(rows.count, 3)
        XCTAssertEqual(rows[0].count, 1, "top row is the single most-important volume")
        XCTAssertEqual(rows.dropFirst().map(\.count), [2, 2], "rest are paired")
        XCTAssertEqual(rows[0][0].mount, "/v0", "first (importance-sorted) leads")
    }

    func testEvenCountPairsAll() {
        let rows = VolumeGridLayout.rows(vols(6), columns: 2)
        XCTAssertEqual(rows.map(\.count), [2, 2, 2])
    }

    func testSingleColumnAllFullWidth() {
        let rows = VolumeGridLayout.rows(vols(3), columns: 1)
        XCTAssertEqual(rows.map(\.count), [1, 1, 1])
    }

    /// Every volume appears exactly once regardless of layout.
    func testNoVolumeLostOrDuplicated() {
        for n in 0...9 {
            for cols in 1...2 {
                let flat = VolumeGridLayout.rows(vols(n), columns: cols).flatMap { $0 }
                XCTAssertEqual(flat.count, n, "n=\(n) cols=\(cols)")
                XCTAssertEqual(Set(flat.map(\.mount)).count, n)
            }
        }
    }
}
