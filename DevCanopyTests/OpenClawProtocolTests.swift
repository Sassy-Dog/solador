import CryptoKit
@testable import DevCanopy
import XCTest

/// Pins the load-bearing wire format: the `Origin` derivation, the upgrade
/// request headers, the signed `connect` frame structure, and the
/// pairing-required classifier. These are the parts that silently make or break
/// the handshake against a real gateway (plan risks #1 and #2).
final class OpenClawProtocolTests: XCTestCase {
    private func env(_ json: String) throws -> OCEnvelope {
        try JSONDecoder().decode(OCEnvelope.self, from: Data(json.utf8))
    }

    private func decodeBase64URL(_ s: String) -> Data? {
        var b64 = s.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        while b64.count % 4 != 0 {
            b64 += "="
        }
        return Data(base64Encoded: b64)
    }

    // MARK: - Origin

    func testDeriveOrigin() {
        XCTAssertEqual(OpenClawProtocol.deriveOrigin(scheme: "ws", host: "host", port: 7878), "http://host:7878")
        XCTAssertEqual(OpenClawProtocol.deriveOrigin(scheme: "wss", host: "host", port: nil), "https://host")
        XCTAssertEqual(OpenClawProtocol.deriveOrigin(scheme: "wss", host: "h", port: 443), "https://h:443")
        XCTAssertNil(OpenClawProtocol.deriveOrigin(scheme: "ws", host: nil, port: 1))
        XCTAssertNil(OpenClawProtocol.deriveOrigin(scheme: "ws", host: "", port: 1))
    }

    // MARK: - Request

    func testMakeRequestSetsHeaders() throws {
        let req = try OpenClawProtocol.makeRequest(gatewayURL: "ws://gw.local:7878", token: "tok")
        XCTAssertEqual(req.url?.absoluteString, "ws://gw.local:7878")
        XCTAssertEqual(req.value(forHTTPHeaderField: "Authorization"), "Bearer tok")
        XCTAssertEqual(req.value(forHTTPHeaderField: "Origin"), "http://gw.local:7878")
    }

    func testMakeRequestOmitsAuthWhenNoToken() throws {
        let req = try OpenClawProtocol.makeRequest(gatewayURL: "wss://gw.local", token: nil)
        XCTAssertNil(req.value(forHTTPHeaderField: "Authorization"))
        XCTAssertEqual(req.value(forHTTPHeaderField: "Origin"), "https://gw.local")
    }

    func testMakeRequestRejectsNonWebSocketURL() {
        XCTAssertThrowsError(try OpenClawProtocol.makeRequest(gatewayURL: "https://gw.local", token: nil))
        XCTAssertThrowsError(try OpenClawProtocol.makeRequest(gatewayURL: "not a url", token: nil))
    }

    // MARK: - Connect frame

    func testConnectFrameStructureAndSignature() throws {
        let key = try Curve25519.Signing.PrivateKey(rawRepresentation: Data(repeating: 9, count: 32))
        let identity = OpenClawDeviceIdentity(privateKey: key)
        let frame = OpenClawProtocol.connectFrame(
            nonce: "n", identity: identity, token: "tok", signedAtMs: 5, appVersion: "1.2.3"
        )

        XCTAssertEqual(frame["type"] as? String, "req")
        XCTAssertEqual(frame["id"] as? String, "connect-1")
        XCTAssertEqual(frame["method"] as? String, "connect")

        let params = try XCTUnwrap(frame["params"] as? [String: Any])
        XCTAssertEqual(params["minProtocol"] as? Int, 4)
        XCTAssertEqual(params["maxProtocol"] as? Int, 4)
        XCTAssertEqual(params["role"] as? String, "operator")
        XCTAssertEqual(params["scopes"] as? [String], ["operator.read", "operator.approvals", "operator.admin"])
        XCTAssertNotNil(params["auth"]) // present because token supplied

        let device = try XCTUnwrap(params["device"] as? [String: Any])
        XCTAssertEqual(device["id"] as? String, identity.deviceID)
        XCTAssertEqual(device["publicKey"] as? String, identity.publicKeyBase64URL)
        XCTAssertEqual(device["nonce"] as? String, "n")
        XCTAssertEqual(device["signedAt"] as? Int64, 5)

        // End-to-end: the signature in the frame must verify against the public
        // key over the EXACT v2 payload — pins the whole connect signing path.
        let sig = try XCTUnwrap(device["signature"] as? String)
        let sigData = try XCTUnwrap(decodeBase64URL(sig))
        let expectedPayload =
            "v2|\(identity.deviceID)|openclaw-tui|ui|operator|operator.read,operator.approvals,operator.admin|5|tok|n"
        XCTAssertTrue(key.publicKey.isValidSignature(sigData, for: Data(expectedPayload.utf8)))
    }

    func testConnectFrameOmitsAuthWithoutToken() throws {
        let key = Curve25519.Signing.PrivateKey()
        let frame = OpenClawProtocol.connectFrame(
            nonce: "n", identity: OpenClawDeviceIdentity(privateKey: key),
            token: nil, signedAtMs: 1, appVersion: "0"
        )
        let params = try XCTUnwrap(frame["params"] as? [String: Any])
        XCTAssertNil(params["auth"])
    }

    // MARK: - Pairing classifier

    func testClassifyFirstPair() throws {
        let e = try env("""
        {"type":"res","id":"connect-1","ok":false,"error":{"code":"X","details":{"code":"PAIRING_REQUIRED","requestId":"req-7","deviceId":"dev-9"}}}
        """)
        let p = try XCTUnwrap(OpenClawProtocol.classifyPairing(e, fallbackDeviceID: "fb"))
        XCTAssertEqual(p.kind, .firstPair)
        XCTAssertEqual(p.requestID, "req-7")
        XCTAssertEqual(p.deviceID, "dev-9")
    }

    func testClassifyScopeUpgradeAndFallbackDeviceID() throws {
        let e = try env("""
        {"type":"res","id":"connect-1","ok":false,"error":{"details":{"code":"PAIRING_REQUIRED","requestId":"r","reason":"scope-upgrade"}}}
        """)
        let p = try XCTUnwrap(OpenClawProtocol.classifyPairing(e, fallbackDeviceID: "fallback-id"))
        XCTAssertEqual(p.kind, .scopeUpgrade)
        XCTAssertEqual(p.deviceID, "fallback-id") // no deviceId in payload → fallback
    }

    func testClassifyReturnsNilForNonPairingError() throws {
        let e = try env("""
        {"type":"res","id":"connect-1","ok":false,"error":{"code":"NOPE","details":{"code":"AUTH_RATE_LIMITED"}}}
        """)
        XCTAssertNil(OpenClawProtocol.classifyPairing(e, fallbackDeviceID: "fb"))
    }
}
