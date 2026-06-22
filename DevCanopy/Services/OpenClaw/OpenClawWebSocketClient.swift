import Foundation

/// Errors that end a single OpenClaw session. The service's reconnect loop maps
/// these to backoff behavior — `pairingRequired` gets a long, quiet backoff
/// (a human must approve out-of-band); the rest reconnect on exponential backoff.
enum OpenClawSessionError: Error {
    case invalidURL
    case handshakeTimeout
    case handshakeRejected(String)
    case pairingRequired(PairingState)
    case socketClosed
}

/// Native `URLSessionWebSocketTask` client for the OpenClaw gateway. Ports
/// periclaw's `net/openclaw.rs` session: connect → await `connect.challenge` →
/// sign the nonce → send `connect` → await `hello-ok` → bootstrap snapshot RPCs
/// → receive loop, with a 30s channel/session heartbeat and a ping keepalive.
///
/// An `actor` so the socket and the request-id counter are isolated; decoded
/// frames are handed back through a `@Sendable` callback the service hops to the
/// main actor. The client never touches `@Published` state directly.
actor OpenClawWebSocketClient {
    private let gatewayURL: String
    private let token: String?
    private let identity: OpenClawDeviceIdentity

    private let urlSession: URLSession
    private var socket: URLSessionWebSocketTask?

    /// Connect uses the literal id "connect-1" (matches the gateway reference
    /// client). Bootstrap/heartbeat RPCs use id == method so responses route by id.
    private static let connectID = "connect-1"

    init(gatewayURL: String, token: String?, identity: OpenClawDeviceIdentity) {
        self.gatewayURL = gatewayURL
        self.token = token
        self.identity = identity
        let config = URLSessionConfiguration.default
        config.waitsForConnectivity = false
        urlSession = URLSession(configuration: config)
    }

    /// Run one full session. Returns only by throwing (socket close, handshake
    /// failure, pairing required, or task cancellation). `onConnected` fires once
    /// after `hello-ok`; `onFrame` fires for every steady-state frame.
    func runSession(
        onConnected: @escaping @Sendable () -> Void,
        onFrame: @escaping @Sendable (OCEnvelope) -> Void
    ) async throws {
        let request = try buildRequest()
        let socket = urlSession.webSocketTask(with: request)
        self.socket = socket
        socket.resume()

        let nonce = try await awaitChallenge()
        try await sendConnect(nonce: nonce)
        try await awaitHelloOk()
        onConnected()
        try await sendBootstrap()

        // Receive loop + heartbeat + ping run concurrently; the first to throw
        // (almost always the receive loop on socket close) cancels the rest.
        try await withThrowingTaskGroup(of: Void.self) { group in
            group.addTask { try await self.receiveLoop(onFrame: onFrame) }
            group.addTask { try await self.heartbeatLoop() }
            group.addTask { try await self.pingLoop() }
            defer { group.cancelAll() }
            try await group.next()
        }
    }

    func close() {
        socket?.cancel(with: .goingAway, reason: nil)
        socket = nil
    }

    // MARK: - Request

    private func buildRequest() throws -> URLRequest {
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
        if let origin = Self.deriveOrigin(scheme: scheme, host: url.host, port: url.port) {
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

    // MARK: - Handshake

    private func awaitChallenge() async throws -> String {
        let deadline = Date().addingTimeInterval(10)
        while Date() < deadline {
            guard let env = try await receiveEnvelope(until: deadline) else { continue }
            if env.type == "event", env.event == "connect.challenge",
               let nonce = env.payload?["nonce"]?.stringValue, !nonce.isEmpty
            {
                return nonce
            }
            // Ignore any other pre-connect frames.
        }
        throw OpenClawSessionError.handshakeTimeout
    }

    private func sendConnect(nonce: String) async throws {
        let scopes = ["operator.read", "operator.approvals", "operator.admin"]
        let signedAtMs = Int64(Date().timeIntervalSince1970 * 1000)
        let signature = identity.signConnect(SignConnectParams(
            clientID: "openclaw-tui",
            clientMode: "ui",
            role: "operator",
            scopes: scopes,
            token: token,
            nonce: nonce,
            signedAtMs: signedAtMs
        ))

        var params: [String: Any] = [
            "minProtocol": 3,
            "maxProtocol": 3,
            "client": [
                "id": "openclaw-tui",
                "displayName": "DevCanopy",
                "version": Self.appVersion,
                "platform": "macos",
                "mode": "ui",
                "instanceId": identity.deviceID
            ],
            "role": "operator",
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
        try await send([
            "type": "req",
            "id": Self.connectID,
            "method": "connect",
            "params": params
        ])
    }

    private func awaitHelloOk() async throws {
        let deadline = Date().addingTimeInterval(10)
        while Date() < deadline {
            guard let env = try await receiveEnvelope(until: deadline) else { continue }
            guard env.type == "res", env.id == Self.connectID else { continue }
            if env.ok == true, env.payload?["type"]?.stringValue == "hello-ok" {
                return
            }
            // Distinguish a pairing request from a generic rejection so the
            // service can back off long and surface the approve instruction.
            if let pairing = Self.classifyPairing(env, fallbackDeviceID: identity.deviceID) {
                throw OpenClawSessionError.pairingRequired(pairing)
            }
            let message = env.error?.message ?? env.error?.code ?? "handshake rejected"
            throw OpenClawSessionError.handshakeRejected(message)
        }
        throw OpenClawSessionError.handshakeTimeout
    }

    /// `error.details.code == "PAIRING_REQUIRED"` with a `requestId` → a pairing
    /// request; `reason == "scope-upgrade"` distinguishes upgrade from first pair.
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

    // MARK: - Bootstrap + heartbeat

    /// One-shot snapshot of cron/channels/sessions/agents so the panel has state
    /// before any push event. `id == method` so responses route by id. We rely on
    /// the gateway's `cron` broadcast for live cron deltas (no cron poll), like
    /// periclaw.
    private func sendBootstrap() async throws {
        for method in ["cron.list", "channels.status", "sessions.list", "agents.list"] {
            try await sendRPC(method)
        }
    }

    /// Channels aren't broadcast, and session usage drifts, so re-snapshot both
    /// every 30s over the live socket.
    private func heartbeatLoop() async throws {
        while !Task.isCancelled {
            try await Task.sleep(nanoseconds: 30 * 1_000_000_000)
            if Task.isCancelled { return }
            try await sendRPC("channels.status")
            try await sendRPC("sessions.list")
        }
    }

    /// Application-level ping keepalive to survive NAT idle timeouts.
    private func pingLoop() async throws {
        while !Task.isCancelled {
            try await Task.sleep(nanoseconds: 20 * 1_000_000_000)
            if Task.isCancelled { return }
            try await sendPing()
        }
    }

    private func sendRPC(_ method: String) async throws {
        try await send(["type": "req", "id": method, "method": method, "params": [:]])
    }

    // MARK: - Socket primitives

    private func send(_ object: [String: Any]) async throws {
        guard let socket else { throw OpenClawSessionError.socketClosed }
        let data = try JSONSerialization.data(withJSONObject: object)
        guard let text = String(bytes: data, encoding: .utf8) else {
            throw OpenClawSessionError.socketClosed
        }
        try await socket.send(.string(text))
    }

    private func sendPing() async throws {
        guard let socket else { throw OpenClawSessionError.socketClosed }
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            socket.sendPing { error in
                if let error { cont.resume(throwing: error) } else { cont.resume() }
            }
        }
    }

    /// Steady-state receive loop: decode each frame and hand it to the service.
    private func receiveLoop(onFrame: @escaping @Sendable (OCEnvelope) -> Void) async throws {
        while !Task.isCancelled {
            guard let env = try await receiveEnvelope(until: nil) else { continue }
            onFrame(env)
        }
    }

    /// Receive the next frame and decode it as an envelope. With a `deadline`,
    /// races the receive against a timeout (used during handshake). Returns `nil`
    /// for a frame that isn't a decodable text envelope (binary/undecodable —
    /// skip, don't fail the session).
    @discardableResult
    private func receiveEnvelope(until deadline: Date?) async throws -> OCEnvelope? {
        guard let socket else { throw OpenClawSessionError.socketClosed }

        let message: URLSessionWebSocketTask.Message
        if let deadline {
            let remaining = deadline.timeIntervalSinceNow
            guard remaining > 0 else { throw OpenClawSessionError.handshakeTimeout }
            message = try await withThrowingTaskGroup(of: URLSessionWebSocketTask.Message.self) { group in
                group.addTask { try await socket.receive() }
                group.addTask {
                    try await Task.sleep(nanoseconds: UInt64(remaining * 1_000_000_000))
                    throw OpenClawSessionError.handshakeTimeout
                }
                defer { group.cancelAll() }
                guard let first = try await group.next() else {
                    throw OpenClawSessionError.socketClosed
                }
                return first
            }
        } else {
            message = try await socket.receive()
        }

        let data: Data
        switch message {
        case let .string(s): data = Data(s.utf8)
        case let .data(d): data = d
        @unknown default: return nil
        }
        return try? JSONDecoder().decode(OCEnvelope.self, from: data)
    }

    // MARK: -

    private static var appVersion: String {
        Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "0.0.0"
    }
}
