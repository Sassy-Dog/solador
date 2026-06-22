import Combine
import Foundation

/// The OpenClaw runtime: an `AgentRuntime` conformer that owns the reconnect
/// loop and reduces incoming frames into an `AgentRuntimeSnapshot`. This is
/// periclaw's session outer-loop + state reducer, on the main actor.
///
/// Lifecycle: `start()` launches a single reconnect task that opens a session,
/// streams its frames in order through an `AsyncStream`, and reconnects with
/// exponential backoff (a long, quiet backoff while pairing approval is pending).
@MainActor
final class OpenClawService: ObservableObject, AgentRuntime {
    @Published private(set) var snapshot: AgentRuntimeSnapshot

    var snapshotPublisher: AnyPublisher<AgentRuntimeSnapshot, Never> {
        $snapshot.eraseToAnyPublisher()
    }

    /// Provides the gateway URL (from `@AppStorage`) at connect time, so a
    /// Settings change picked up on `restart()` uses the new value.
    private let urlProvider: @MainActor () -> String?
    private let tokenProvider: @MainActor () -> String?

    private var task: Task<Void, Never>?
    private var identity: OpenClawDeviceIdentity?

    /// Pure frame→sections state machine; the service only adds connection state.
    private var reducer = OpenClawSnapshotReducer()

    init(
        id: String = "openclaw",
        displayName: String = "OpenClaw",
        urlProvider: @escaping @MainActor () -> String?,
        tokenProvider: @escaping @MainActor () -> String?
    ) {
        snapshot = .idle(id: id, displayName: displayName)
        self.urlProvider = urlProvider
        self.tokenProvider = tokenProvider
    }

    // MARK: - Lifecycle

    func start() {
        guard task == nil else { return }
        guard let urlString = urlProvider(), !urlString.isEmpty else {
            // Not configured yet — stay idle (we haven't attempted anything), so
            // the panel shows a Settings hint rather than a failure.
            snapshot.connection = .idle
            return
        }
        if identity == nil { identity = OpenClawDeviceIdentity.loadOrCreate() }
        task = Task { [weak self] in await self?.reconnectLoop() }
    }

    func stop() {
        task?.cancel()
        task = nil
        snapshot.connection = .idle
    }

    /// Stop and start — call when the gateway URL or token changes in Settings.
    func restart() {
        stop()
        start()
    }

    deinit { task?.cancel() }

    // MARK: - Reconnect loop

    private func reconnectLoop() async {
        var backoff: TimeInterval = 0.5
        let maxBackoff: TimeInterval = 30

        while !Task.isCancelled {
            guard let urlString = urlProvider(), !urlString.isEmpty, let identity else {
                snapshot.connection = .idle
                return
            }
            let token = tokenProvider()
            snapshot.connection = .connecting

            let client = OpenClawWebSocketClient(gatewayURL: urlString, token: token, identity: identity)
            var connectedAt: Date?

            let (stream, continuation) = AsyncStream.makeStream(of: OpenClawClientEvent.self)
            let consumer = Task { @MainActor [weak self] in
                for await event in stream {
                    self?.apply(event)
                }
            }

            do {
                try await client.runSession(
                    onConnected: { continuation.yield(.connected) },
                    onFrame: { continuation.yield(.frame($0)) }
                )
                continuation.finish()
                _ = await consumer.value
                // Clean end (server closed) — treat as a normal drop.
                snapshot.connection = .disconnected(reason: "connection closed")
            } catch let OpenClawSessionError.pairingRequired(pairing) {
                continuation.finish()
                _ = await consumer.value
                await client.close()
                snapshot.pairing = pairing
                snapshot.connection = .disconnected(
                    reason: pairing.kind == .scopeUpgrade
                        ? "awaiting scope approval"
                        : "awaiting device pairing"
                )
                // A human must approve out-of-band; reconnecting fast is pointless
                // and floods both logs. Long, fixed backoff.
                await sleepInterruptible(15)
                continue
            } catch {
                continuation.finish()
                _ = await consumer.value
                await client.close()
                snapshot.connection = .disconnected(reason: Self.humanize(error))
            }
            connectedAt = lastConnectedAt

            if Task.isCancelled { return }

            // Reset backoff only if the session actually lived past hello-ok for a
            // bit — a gateway that accepts then immediately drops keeps the
            // backoff climbing instead of hammering at the 0.5s floor.
            if let connectedAt, Date().timeIntervalSince(connectedAt) > 10 {
                backoff = 0.5
            } else {
                backoff = min(backoff * 2, maxBackoff)
            }
            await sleepInterruptible(backoff)
        }
    }

    private var lastConnectedAt: Date?

    private func sleepInterruptible(_ seconds: TimeInterval) async {
        try? await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
    }

    private static func humanize(_ error: Error) -> String {
        switch error {
        case OpenClawSessionError.invalidURL: "invalid gateway URL"
        case OpenClawSessionError.handshakeTimeout: "handshake timed out"
        case let OpenClawSessionError.handshakeRejected(msg): "gateway rejected: \(msg)"
        case OpenClawSessionError.socketClosed: "connection closed"
        default: error.localizedDescription
        }
    }

    // MARK: - Reducer bridge

    private func apply(_ event: OpenClawClientEvent) {
        switch event {
        case .connected:
            lastConnectedAt = Date()
            snapshot.pairing = nil
            snapshot.connection = .connected
            snapshot.lastUpdated = Date()
        case let .frame(env):
            // Liveness-only frames (health/heartbeat/tick) still bump the
            // freshness stamp even though they don't change data sections.
            if env.type == "event", let e = env.event, ["health", "heartbeat", "tick"].contains(e) {
                snapshot.lastUpdated = Date()
                return
            }
            if reducer.ingest(env) { rebuildSnapshot() }
        }
    }

    /// Copy the reducer's data sections into the published snapshot.
    private func rebuildSnapshot() {
        snapshot.agents = reducer.agentRows
        snapshot.cron = reducer.cronSummary
        snapshot.channels = reducer.channelStatuses
        snapshot.usage = reducer.usageRollup
        snapshot.lastUpdated = Date()
    }
}

/// What the WS client streams to the service: connection establishment plus
/// every steady-state frame. Carried through an `AsyncStream` so the reducer
/// sees them in order on the main actor.
enum OpenClawClientEvent {
    case connected
    case frame(OCEnvelope)
}
