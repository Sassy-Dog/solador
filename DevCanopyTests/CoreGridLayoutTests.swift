@testable import DevCanopy
import XCTest

final class CoreGridLayoutTests: XCTestCase {
    /// Largest proper divisor ≤ 10 with ≥ 2 rows — divides evenly, never a single tall row.
    func testEvenDivisors() {
        XCTAssertEqual(coreColumns(coreCount: 36), 9) // ubu-3xdv: 9 × 4
        XCTAssertEqual(coreColumns(coreCount: 16), 8) // 8 × 2
        XCTAssertEqual(coreColumns(coreCount: 12), 6) // 6 × 2
        XCTAssertEqual(coreColumns(coreCount: 10), 5) // 5 × 2  (was 10 × 1)
        XCTAssertEqual(coreColumns(coreCount: 9), 3) // 3 × 3
        XCTAssertEqual(coreColumns(coreCount: 8), 4) // 4 × 2  (was 8 × 1)
        XCTAssertEqual(coreColumns(coreCount: 4), 2) // 2 × 2  (was 4 × 1)
        XCTAssertEqual(coreColumns(coreCount: 64), 8) // 8 × 8
    }

    /// Prime core counts have no even split: least-waste columns keeping ≥ 2 rows.
    func testPrimesAndFallback() {
        XCTAssertEqual(coreColumns(coreCount: 7), 4) // 4 × 2, waste 1
        XCTAssertEqual(coreColumns(coreCount: 13), 7) // 7 × 2, waste 1
        XCTAssertEqual(coreColumns(coreCount: 11), 6) // 6 × 2, waste 1
    }

    /// No host should ever render as a single row (rows ≥ 2 whenever there are ≥ 3 cores).
    func testAlwaysAtLeastTwoRows() {
        for n in 3 ... 128 {
            let cols = coreColumns(coreCount: n)
            let rows = (n + cols - 1) / cols
            XCTAssertGreaterThanOrEqual(rows, 2, "core count \(n) produced a single row")
        }
    }

    func testEdgeCases() {
        XCTAssertEqual(coreColumns(coreCount: 1), 1)
        XCTAssertEqual(coreColumns(coreCount: 0), 1)
        XCTAssertEqual(coreColumns(coreCount: 2), 1) // 1 × 2
    }
}
