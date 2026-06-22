import CryptoKit
@testable import DevCanopy
import XCTest

/// Ports periclaw's `device_identity.rs` tests. The load-bearing guarantee:
/// `signConnect` must serialize the **exact** OpenClaw v2 payload — any drift in
/// the delimiter, scope joining, or empty-token handling makes the gateway
/// silently reject every connect.
///
/// Note: unlike ed25519-dalek, CryptoKit's Ed25519 produces *randomized* (hedged)
/// signatures, so we can't assert byte-equality against a second signing. Instead
/// we verify the produced signature against the public key over the **exact**
/// expected payload bytes — if `signConnect` built any other string, verification
/// fails. That's a direct check on the payload format, and it doesn't depend on
/// determinism. The gateway likewise only checks validity, so randomization is
/// fine on the wire.
final class OpenClawDeviceIdentityTests: XCTestCase {
    private func identity(seedByte: UInt8) throws -> (OpenClawDeviceIdentity, Curve25519.Signing.PrivateKey) {
        let seed = Data(repeating: seedByte, count: 32)
        let key = try Curve25519.Signing.PrivateKey(rawRepresentation: seed)
        return (OpenClawDeviceIdentity(privateKey: key), key)
    }

    /// base64url-no-pad → Data (mirror of `Data.base64URLNoPad`).
    private func decodeBase64URL(_ s: String) -> Data? {
        var b64 = s.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        while b64.count % 4 != 0 {
            b64 += "="
        }
        return Data(base64Encoded: b64)
    }

    /// Fixed client identity used by every signing test.
    private func params(scopes: [String], token: String?, nonce: String, signedAtMs: Int64) -> SignConnectParams {
        SignConnectParams(
            clientID: "openclaw-tui", clientMode: "ui", role: "operator",
            scopes: scopes, token: token, nonce: nonce, signedAtMs: signedAtMs
        )
    }

    private func assertSigns(
        _ ident: OpenClawDeviceIdentity,
        key: Curve25519.Signing.PrivateKey,
        _ params: SignConnectParams,
        expectedPayload: String,
        line: UInt = #line
    ) {
        let signature = ident.signConnect(params)
        XCTAssertFalse(signature.isEmpty, line: line)
        guard let sigData = decodeBase64URL(signature) else {
            return XCTFail("signature is not valid base64url", line: line)
        }
        XCTAssertTrue(
            key.publicKey.isValidSignature(sigData, for: Data(expectedPayload.utf8)),
            "signConnect must sign the exact v2 payload format",
            line: line
        )
    }

    func testSignConnectMatchesOpenClawV2Shape() throws {
        let (ident, key) = try identity(seedByte: 0)
        // v2|<deviceId>|openclaw-tui|ui|operator|operator.read|1776000000000|tok|nonce-xyz
        assertSigns(
            ident, key: key,
            params(scopes: ["operator.read"], token: "tok", nonce: "nonce-xyz", signedAtMs: 1_776_000_000_000),
            expectedPayload: "v2|\(ident.deviceID)|openclaw-tui|ui|operator|operator.read|1776000000000|tok|nonce-xyz"
        )
    }

    func testEmptyTokenSerializesAsEmptyField() throws {
        // token == nil must produce `...|signedAt||nonce` (empty field), not a
        // dropped delimiter — the gateway signs with an empty string for absent
        // tokens.
        let (ident, key) = try identity(seedByte: 3)
        assertSigns(
            ident, key: key,
            params(scopes: ["operator.read", "operator.admin"], token: nil, nonce: "n2", signedAtMs: 42),
            expectedPayload: "v2|\(ident.deviceID)|openclaw-tui|ui|operator|operator.read,operator.admin|42||n2"
        )
    }

    func testWrongPayloadFailsVerification() throws {
        // Guard the guard: a signature over the real payload must NOT verify
        // against a tampered payload (proves the verification check has teeth).
        let (ident, key) = try identity(seedByte: 5)
        let signature = ident.signConnect(
            params(scopes: ["operator.read"], token: "t", nonce: "n", signedAtMs: 7)
        )
        let sigData = try XCTUnwrap(decodeBase64URL(signature))
        let tampered = "v2|\(ident.deviceID)|openclaw-tui|ui|operator|operator.read|7|t|DIFFERENT"
        XCTAssertFalse(key.publicKey.isValidSignature(sigData, for: Data(tampered.utf8)))
    }

    func testDeviceIDIsSHA256HexOfPublicKey() throws {
        let (ident, _) = try identity(seedByte: 1)
        XCTAssertEqual(ident.deviceID.count, 64, "SHA-256 is 32 bytes → 64 hex chars")
        XCTAssertTrue(ident.deviceID.allSatisfy(\.isHexDigit))
        XCTAssertEqual(ident.deviceID, ident.deviceID.lowercased(), "device id is lowercase hex")
    }

    func testPublicKeyBase64URLIs43Chars() throws {
        let (ident, _) = try identity(seedByte: 2)
        XCTAssertEqual(ident.publicKeyBase64URL.count, 43, "32 bytes base64url-no-pad is 43 chars")
        XCTAssertFalse(ident.publicKeyBase64URL.contains("="), "url-safe encoding drops padding")
        XCTAssertFalse(ident.publicKeyBase64URL.contains("+"))
        XCTAssertFalse(ident.publicKeyBase64URL.contains("/"))
    }
}
