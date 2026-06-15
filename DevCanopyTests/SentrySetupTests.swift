@testable import DevCanopy
import XCTest

/// Verifies the opt-in / no-telemetry-by-default contract for `SentrySetup`
/// (issue #18): the DSN resolves to `nil` — meaning Sentry is never started —
/// whenever no non-empty DSN is configured via the environment or Info.plist.
final class SentrySetupTests: XCTestCase {
    private let envKey = SentrySetup.environmentKey

    // MARK: No-op cases (DSN absent → Sentry must not start)

    func testNoDSNWhenEnvironmentAndPlistAreUnset() {
        XCTAssertNil(SentrySetup.resolveDSN(environment: [:], plistValue: nil))
    }

    func testNoDSNWhenPlistValueIsEmptyString() {
        // The Info.plist ships an empty default ($(SENTRY_DSN) unset) — this
        // is the normal local-build path and MUST resolve to no DSN.
        XCTAssertNil(SentrySetup.resolveDSN(environment: [:], plistValue: ""))
    }

    func testNoDSNWhenPlistValueIsWhitespaceOnly() {
        XCTAssertNil(SentrySetup.resolveDSN(environment: [:], plistValue: "   \n  "))
    }

    func testNoDSNWhenEnvironmentValueIsEmpty() {
        XCTAssertNil(SentrySetup.resolveDSN(environment: [envKey: ""], plistValue: nil))
    }

    func testNoDSNWhenEnvironmentValueIsWhitespaceOnly() {
        XCTAssertNil(SentrySetup.resolveDSN(environment: [envKey: "  "], plistValue: ""))
    }

    // MARK: DSN-present cases (opt-in → resolved)

    func testResolvesDSNFromEnvironment() {
        let dsn = "https://public@example.ingest.sentry.io/1"
        XCTAssertEqual(SentrySetup.resolveDSN(environment: [envKey: dsn], plistValue: nil), dsn)
    }

    func testResolvesDSNFromPlistWhenEnvironmentUnset() {
        let dsn = "https://public@example.ingest.sentry.io/2"
        XCTAssertEqual(SentrySetup.resolveDSN(environment: [:], plistValue: dsn), dsn)
    }

    func testEnvironmentTakesPrecedenceOverPlist() {
        let envDSN = "https://env@example.ingest.sentry.io/3"
        let plistDSN = "https://plist@example.ingest.sentry.io/4"
        XCTAssertEqual(
            SentrySetup.resolveDSN(environment: [envKey: envDSN], plistValue: plistDSN),
            envDSN
        )
    }
}
