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
        let request = try OpenClawProtocol.makeRequest(gatewayURL: gatewayURL, token: token)
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
        let frame = OpenClawProtocol.connectFrame(
            nonce: nonce,
            identity: identity,
            token: token,
            signedAtMs: Int64(Date().timeIntervalSince1970 * 1000),
            appVersion: Self.appVersion
        )
        try await send(frame)
    }

    private func awaitHelloOk() async throws {
        let deadline = Date().addingTimeInterval(10)
        while Date() < deadline {
            guard let env = try await receiveEnvelope(until: deadline) else { continue }
            guard env.type == "res", env.id == OpenClawProtocol.connectID else { continue }
            if env.ok == true, env.payload?["type"]?.stringValue == "hello-ok" {
                return
            }
            // Distinguish a pairing request from a generic rejection so the
            // service can back off long and surface the approve instruction.
            if let pairing = OpenClawProtocol.classifyPairing(env, fallbackDeviceID: identity.deviceID) {
                throw OpenClawSessionError.pairingRequired(pairing)
            }
            let message = env.error?.message ?? env.error?.code ?? "handshake rejected"
            throw OpenClawSessionError.handshakeRejected(message)
        }
        throw OpenClawSessionError.handshakeTimeout
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
