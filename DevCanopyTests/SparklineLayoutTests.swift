import CoreGraphics
@testable import DevCanopy
import XCTest

final class SparklineLayoutTests: XCTestCase {
    private let size = CGSize(width: 119, height: 100)

    /// A few points must sit at the RIGHT edge at constant spacing (sliding in),
    /// NOT be stretched across the full width.
    func testPartialBufferIsRightAnchoredAtFixedSpacing() {
        let pts = Sparkline.layout(values: [10, 20, 30], capacity: 120, size: size, range: 0 ... 100)
        XCTAssertEqual(pts.count, 3)
        // stepX = 119 / (120-1) = 1; newest pinned at width.
        XCTAssertEqual(pts[2].x, 119, accuracy: 0.01) // newest at right edge
        XCTAssertEqual(pts[1].x, 118, accuracy: 0.01)
        XCTAssertEqual(pts[0].x, 117, accuracy: 0.01)
        // Explicitly NOT the old stretch behavior (which would be 0, 59.5, 119).
        XCTAssertGreaterThan(pts[0].x, 100)
    }

    /// y is normalized within the given range (0..100): v=30 -> 70% down from top.
    func testYNormalizationWithinRange() {
        let pts = Sparkline.layout(values: [10, 20, 30], capacity: 120, size: size, range: 0 ... 100)
        XCTAssertEqual(pts[2].y, 70, accuracy: 0.01) // 100 * (1 - 0.30)
        XCTAssertEqual(pts[0].y, 90, accuracy: 0.01) // 100 * (1 - 0.10)
    }

    /// A full buffer spans the whole width: oldest at 0, newest at width.
    func testFullBufferSpansFullWidth() throws {
        let values = (0 ..< 120).map { Double($0) }
        let pts = Sparkline.layout(values: values, capacity: 120, size: size, range: 0 ... 119)
        XCTAssertEqual(try XCTUnwrap(pts.first?.x), 0, accuracy: 0.01)
        XCTAssertEqual(try XCTUnwrap(pts.last?.x), 119, accuracy: 0.01)
    }

    /// More values than capacity keeps only the most recent `capacity`.
    func testOverflowKeepsMostRecent() throws {
        let values = (0 ..< 200).map { Double($0) }
        let pts = Sparkline.layout(values: values, capacity: 120, size: size, range: 0 ... 199)
        XCTAssertEqual(pts.count, 120)
        XCTAssertEqual(try XCTUnwrap(pts.last?.x), 119, accuracy: 0.01) // newest still pinned right
    }

    func testFewerThanTwoPointsYieldsEmpty() {
        XCTAssertTrue(Sparkline.layout(values: [42], capacity: 120, size: size, range: 0 ... 100).isEmpty)
    }
}
