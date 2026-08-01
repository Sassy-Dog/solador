import Foundation

/// Pure, IO-free OpenClaw protocol helpers: connection constants, the WS upgrade
/// request, the signed `connect` frame, the `Origin` derivation, and the
/// pairing-required classifier. Extracted from the socket actor so the
/// load-bearing wire format (the parts that silently make or break the handshake)
/// is unit-testable without a live gateway. Mirrors periclaw's `net/openclaw.rs`.
enum OpenClawProtocol {
    static let clientID = "openclaw-tui"
    static let clientMode = "ui"
    static let role = "operator"
    static let scopes = ["operator.read", "operator.approvals", "operator.admin"]
    /// Both bounds of the supported WS protocol range. The gateway requires a
    /// UI-mode operator client to cover v4 (`maxProtocol >= 4 && minProtocol <= 4`
    /// in its connect gate — v3 tolerance exists only for probe/node modes), and
    /// rejects v3-only clients with `PROTOCOL_MISMATCH`. Observed live against
    /// OpenClaw 2026.7.1-2 (issue #186); this client's v3 predates that gate and
    /// had never reached a live gateway. The signed device-auth payload is a
    /// separate version and stays at v2 — the gateway verifies v3-then-v2.
    static let protocolVersion = 4
    /// The fixed id used for the connect request, matching the gateway reference
    /// client; the gateway echoes it on the `res`.
    static let connectID = "connect-1"

    /// Build the WS upgrade request with the optional bearer token and the
    /// `Origin` derived from the gateway URL. Throws `.invalidURL` for a URL that
    /// isn't `ws(s)://`.
    static func makeRequest(gatewayURL: String, token: String?) throws -> URLRequest {
        guard let url = URL(string: gatewayURL),
              let scheme = url.scheme?.lowercased(),
              scheme == "ws" || scheme == "wss"
        else {
            throw OpenClawSessionError.invalidURL
        }
        var request = URLRequest(url: url)
        if let token, !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        // The gateway enforces `controlUi.allowedOrigins` on the WS upgrade.
        // Derive the Origin from the gateway URL itself (ws→http, wss→https) so
        // the operator's allowedOrigins only needs the gateway's own hostname.
        if let origin = deriveOrigin(scheme: scheme, host: url.host, port: url.port) {
            request.setValue(origin, forHTTPHeaderField: "Origin")
        }
        return request
    }

    /// `ws://host:port` → `http://host:port`; `wss://` → `https://`.
    static func deriveOrigin(scheme: String, host: String?, port: Int?) -> String? {
        guard let host, !host.isEmpty else { return nil }
        let httpScheme = scheme == "wss" ? "https" : "http"
        if let port {
            return "\(httpScheme)://\(host):\(port)"
        }
        return "\(httpScheme)://\(host)"
    }

    /// The full signed `connect` request frame. `signedAtMs` is injected (not
    /// read from the clock) so the value echoed in the `device` block is exactly
    /// the one folded into the signed payload — and so this is deterministically
    /// testable.
    static func connectFrame(
        nonce: String,
        identity: OpenClawDeviceIdentity,
        token: String?,
        signedAtMs: Int64,
        appVersion: String
    ) -> [String: Any] {
        let signature = identity.signConnect(SignConnectParams(
            clientID: clientID,
            clientMode: clientMode,
            role: role,
            scopes: scopes,
            token: token,
            nonce: nonce,
            signedAtMs: signedAtMs
        ))

        var params: [String: Any] = [
            "minProtocol": protocolVersion,
            "maxProtocol": protocolVersion,
            "client": [
                "id": clientID,
                "displayName": "DevCanopy",
                "version": appVersion,
                "platform": "macos",
                "mode": clientMode,
                "instanceId": identity.deviceID
            ],
            "role": role,
            "scopes": scopes,
            "caps": [],
            "device": [
                "id": identity.deviceID,
                "publicKey": identity.publicKeyBase64URL,
                "signature": signature,
                "signedAt": signedAtMs,
                "nonce": nonce
            ]
        ]
        if let token, !token.isEmpty {
            params["auth"] = ["token": token]
        }
        return [
            "type": "req",
            "id": connectID,
            "method": "connect",
            "params": params
        ]
    }

    /// `error.details.code == "PAIRING_REQUIRED"` with a `requestId` → a pairing
    /// request; `reason == "scope-upgrade"` distinguishes upgrade from first pair.
    /// Returns `nil` for any other rejection.
    static func classifyPairing(_ env: OCEnvelope, fallbackDeviceID: String) -> PairingState? {
        guard let details = env.error?.details, details.code == "PAIRING_REQUIRED" else { return nil }
        let kind: PairingState.Kind = details.reason == "scope-upgrade" ? .scopeUpgrade : .firstPair
        return PairingState(
            deviceID: details.deviceId ?? fallbackDeviceID,
            requestID: details.requestId,
            kind: kind,
            remediationHint: details.remediationHint
        )
    }
}
