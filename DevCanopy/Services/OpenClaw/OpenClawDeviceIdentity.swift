import CryptoKit
import Foundation

/// Ed25519 device identity for OpenClaw gateway pairing, via CryptoKit.
///
/// Ports periclaw's `device_identity.rs` (which itself matches OpenClaw's
/// `buildDeviceAuthPayload` v2). The keypair is generated once and the 32-byte
/// seed persisted in the Keychain; the gateway operator approves the resulting
/// `deviceID` out-of-band (`openclaw devices approve <requestId>`), after which
/// connects authenticate silently by signing the challenge nonce.
///
/// Cross-implementation parity is guaranteed by RFC 8032: Ed25519 signatures are
/// deterministic, so the same 32-byte seed + the same payload bytes produce the
/// same signature here as in ed25519-dalek. The only thing we must get exactly
/// right is the payload string — see `signConnect` and the unit test.
struct OpenClawDeviceIdentity {
    /// Lowercase hex of SHA-256(raw 32-byte public key). Stable across runs.
    let deviceID: String
    /// base64url-no-pad of the raw 32-byte public key (43 chars).
    let publicKeyBase64URL: String

    private let privateKey: Curve25519.Signing.PrivateKey

    init(privateKey: Curve25519.Signing.PrivateKey) {
        self.privateKey = privateKey
        let pubRaw = privateKey.publicKey.rawRepresentation
        deviceID = Data(SHA256.hash(data: pubRaw)).hexLowercased
        publicKeyBase64URL = pubRaw.base64URLNoPad
    }

    /// Load the stored device key from the Keychain, or generate + persist a new
    /// one on first run. A failed Keychain write is non-fatal (we still return a
    /// usable identity for this session); a read miss generates fresh.
    static func loadOrCreate(keychain: KeychainHelper = .shared) -> OpenClawDeviceIdentity {
        if let seed = keychain.loadOpenClawDeviceKey(),
           let key = try? Curve25519.Signing.PrivateKey(rawRepresentation: seed)
        {
            return OpenClawDeviceIdentity(privateKey: key)
        }
        let key = Curve25519.Signing.PrivateKey()
        do {
            try keychain.saveOpenClawDeviceKey(key.rawRepresentation)
        } catch {
            appLogger.warning("OpenClaw: could not persist device key: \(error.localizedDescription)")
        }
        let identity = OpenClawDeviceIdentity(privateKey: key)
        appLogger.info("OpenClaw: generated device identity \(identity.deviceID) — approve it on the gateway to pair")
        return identity
    }

    /// The device id for the already-stored key, or `nil` if no key exists yet.
    /// Lets Settings show the fingerprint (so the operator can pre-approve)
    /// without generating a key as a side effect.
    static func currentDeviceID(keychain: KeychainHelper = .shared) -> String? {
        guard let seed = keychain.loadOpenClawDeviceKey(),
              let key = try? Curve25519.Signing.PrivateKey(rawRepresentation: seed) else { return nil }
        return OpenClawDeviceIdentity(privateKey: key).deviceID
    }

    /// Build and sign the OpenClaw v2 connect payload, returning the
    /// base64url-no-pad signature. The caller must echo the exact `signedAtMs`
    /// and `nonce` in the connect frame's `device` block, since the gateway
    /// reconstructs and verifies against the same payload.
    ///
    /// Payload (exact):
    /// `v2|{deviceID}|{clientID}|{clientMode}|{role}|{scopes,joined}|{signedAtMs}|{token}|{nonce}`
    func signConnect(_ params: SignConnectParams) -> String {
        let joinedScopes = params.scopes.joined(separator: ",")
        let tokenPart = params.token ?? ""
        let payload = "v2|\(deviceID)|\(params.clientID)|\(params.clientMode)|\(params.role)|\(joinedScopes)|\(params.signedAtMs)|\(tokenPart)|\(params.nonce)"
        // `signature(for:)` cannot throw for a valid key; fall back to empty on
        // the impossible error so callers stay non-throwing (an empty signature
        // is simply rejected by the gateway, same as any bad signature).
        guard let sig = try? privateKey.signature(for: Data(payload.utf8)) else { return "" }
        return sig.base64URLNoPad
    }
}

/// Inputs to the OpenClaw v2 connect signature. Bundled so the signing API stays
/// a single parameter (and mirrors periclaw's `SignConnectParams`).
struct SignConnectParams {
    let clientID: String
    let clientMode: String
    let role: String
    let scopes: [String]
    let token: String?
    let nonce: String
    let signedAtMs: Int64
}

extension Data {
    /// Lowercase hex encoding.
    var hexLowercased: String {
        map { String(format: "%02x", $0) }.joined()
    }

    /// base64url encoding with `+`→`-`, `/`→`_`, and padding stripped.
    var base64URLNoPad: String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
