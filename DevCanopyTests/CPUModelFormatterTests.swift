@testable import DevCanopy
import XCTest

final class CPUModelFormatterTests: XCTestCase {
    func testIntelCoreBrandString() {
        XCTAssertEqual(
            CPUModelFormatter.clean("Intel(R) Core(TM) i9-10980XE CPU @ 3.00GHz"),
            "Intel Core i9-10980XE @ 3.00GHz"
        )
    }

    func testIntelXeonCollapsesNativeDoubledSpaces() {
        XCTAssertEqual(
            CPUModelFormatter.clean("Intel(R) Xeon(R) CPU           E5-2680 v4 @ 2.40GHz"),
            "Intel Xeon E5-2680 v4 @ 2.40GHz"
        )
    }

    func testAMDStripsTrailingCoreProcessorSuffix() {
        XCTAssertEqual(
            CPUModelFormatter.clean("AMD Ryzen 9 7950X 16-Core Processor"),
            "AMD Ryzen 9 7950X"
        )
    }

    func testAppleSiliconPassesThrough() {
        XCTAssertEqual(CPUModelFormatter.clean("Apple M2 Max"), "Apple M2 Max")
        XCTAssertEqual(CPUModelFormatter.clean("Apple M4 Max"), "Apple M4 Max")
    }

    func testEmptyAndWhitespaceOnlyYieldEmpty() {
        XCTAssertEqual(CPUModelFormatter.clean(""), "")
        XCTAssertEqual(CPUModelFormatter.clean("   "), "")
    }
}
