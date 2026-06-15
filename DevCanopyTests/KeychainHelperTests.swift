@testable import DevCanopy
import XCTest

/// Round-trip tests for `KeychainHelper`.
///
/// These exercise the real macOS Keychain. In unsigned CI (where the test host
/// app has no keychain-access entitlement) `SecItemAdd` fails with
/// `errSecMissingEntitlement` / `errSecNotAvailable`. Rather than fail the
/// suite there, each test first probes availability and `XCTSkip`s when the
/// keychain is unreachable, so the suite passes both locally and in unsigned CI.
final class KeychainHelperTests: XCTestCase {
    private let helper = KeychainHelper.shared
    private var keys: [String] = []

    /// A unique key per test run so concurrent runs / leftover state can't collide.
    private func uniqueKey(_ label: String) -> String {
        let key = "test_\(label)_\(UUID().uuidString)"
        keys.append(key)
        return key
    }

    override func tearDown() {
        for key in keys {
            try? helper.delete(for: key)
        }
        keys.removeAll()
        super.tearDown()
    }

    /// Skip the current test unless the keychain accepts a write/read cycle.
    private func requireKeychain() throws {
        let probeKey = uniqueKey("probe")
        do {
            try helper.save(Data([0x1]), for: probeKey)
            _ = try helper.load(for: probeKey)
            try helper.delete(for: probeKey)
        } catch {
            throw XCTSkip("Keychain unavailable in this environment: \(error)")
        }
    }

    // MARK: - Generic round-trip

    func testSaveLoadDeleteRoundTrip() throws {
        try requireKeychain()
        let key = uniqueKey("roundtrip")
        let data = Data("payload-\(UUID().uuidString)".utf8)

        try helper.save(data, for: key)
        XCTAssertEqual(try helper.load(for: key), data)

        try helper.delete(for: key)
        XCTAssertThrowsError(try helper.load(for: key)) { error in
            XCTAssertEqual(error as? KeychainError, .itemNotFound)
        }
    }

    func testOverwriteSemantics() throws {
        try requireKeychain()
        let key = uniqueKey("overwrite")

        try helper.saveString("first", for: key)
        XCTAssertEqual(try helper.loadString(for: key), "first")

        // Saving again must replace, not duplicate or throw duplicateItem.
        try helper.saveString("second", for: key)
        XCTAssertEqual(try helper.loadString(for: key), "second")
    }

    func testDeleteIsIdempotent() throws {
        try requireKeychain()
        let key = uniqueKey("idempotent-delete")

        try helper.saveString("value", for: key)
        XCTAssertNoThrow(try helper.delete(for: key))
        // Deleting a non-existent item must not throw.
        XCTAssertNoThrow(try helper.delete(for: key))
    }

    func testLoadMissingThrowsItemNotFound() throws {
        try requireKeychain()
        let key = uniqueKey("never-saved")
        XCTAssertThrowsError(try helper.load(for: key)) { error in
            XCTAssertEqual(error as? KeychainError, .itemNotFound)
        }
    }

    func testStringRoundTrip() throws {
        try requireKeychain()
        let key = uniqueKey("string")
        let value = "unicode ✓ token \(UUID().uuidString)"
        try helper.saveString(value, for: key)
        XCTAssertEqual(try helper.loadString(for: key), value)
    }

    // MARK: - Per-host token key derivation

    func testHostTokenRoundTrip() throws {
        try requireKeychain()
        let hostID = UUID()
        defer { helper.deleteHostToken(hostID: hostID) }

        let token = "bearer-\(UUID().uuidString)"
        try helper.saveHostToken(token, hostID: hostID)

        // loadHostToken must read exactly what saveHostToken wrote — guards the
        // "host_token_<uuid>" key derivation against a silent regression.
        XCTAssertEqual(helper.loadHostToken(hostID: hostID), token)
    }

    func testHostTokensAreKeyedPerHost() throws {
        try requireKeychain()
        let hostA = UUID()
        let hostB = UUID()
        defer {
            helper.deleteHostToken(hostID: hostA)
            helper.deleteHostToken(hostID: hostB)
        }

        try helper.saveHostToken("token-a", hostID: hostA)
        try helper.saveHostToken("token-b", hostID: hostB)

        XCTAssertEqual(helper.loadHostToken(hostID: hostA), "token-a")
        XCTAssertEqual(helper.loadHostToken(hostID: hostB), "token-b")
    }

    func testDeleteHostTokenRemovesIt() throws {
        try requireKeychain()
        let hostID = UUID()
        try helper.saveHostToken("token", hostID: hostID)
        helper.deleteHostToken(hostID: hostID)
        XCTAssertNil(helper.loadHostToken(hostID: hostID))
    }

    func testLoadHostTokenMissingReturnsNil() throws {
        try requireKeychain()
        // A host that never had a token saved must read back nil, not crash.
        XCTAssertNil(helper.loadHostToken(hostID: UUID()))
    }
}
