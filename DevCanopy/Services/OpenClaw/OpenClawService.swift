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

    // Reducer state, keyed for incremental updates.
    private var cronByName: [String: OCCronJob] = [:]
    private var cronIDToName: [String: String] = [:]
    private var channels: [ChannelStatus] = []
    private var agents: [OCAgentInfo] = []
    /// Transient per-agent activity overriding the resting `.ok` (cleared on a
    /// `lifecycle` end). Keyed by agent id.
    private var agentActivity: [String: AgentStatus] = [:]
    private var usage: SessionUsageRollup?

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

    // MARK: - Reducer

    private func apply(_ event: OpenClawClientEvent) {
        switch event {
        case .connected:
            lastConnectedAt = Date()
            snapshot.pairing = nil
            snapshot.connection = .connected
            snapshot.lastUpdated = Date()
        case let .frame(env):
            reduce(env)
        }
    }

    private func reduce(_ env: OCEnvelope) {
        switch env.type {
        case "res":
            reduceResponse(id: env.id, payload: env.payload)
        case "event":
            reduceEvent(name: env.event, payload: env.payload)
        default:
            break
        }
    }

    private func reduceResponse(id: String?, payload: OCJSON?) {
        guard let id, let payload else { return }
        switch id {
        case "cron.list":
            for job in Self.decodeCronJobs(payload) {
                cronByName[job.name] = job
                if let jid = job.id { cronIDToName[jid] = job.name }
            }
            rebuild()
        case "channels.status":
            channels = Self.decodeChannels(payload).map {
                ChannelStatus(
                    id: $0.name,
                    name: $0.name,
                    status: OpenClawStatusMapping.channelStatus($0),
                    lastError: $0.lastError
                )
            }
            rebuild()
        case "sessions.list":
            usage = Self.decodeUsage(payload)
            rebuild()
        case "agents.list":
            if let resp = payload.decode(OCAgentsListResponse.self), let list = resp.agents {
                agents = list
                rebuild()
            }
        default:
            break
        }
    }

    private func reduceEvent(name: String?, payload: OCJSON?) {
        guard let name else { return }
        switch name {
        case "cron":
            if let payload, let job = Self.cronJobFromEvent(payload, idToName: cronIDToName) {
                cronByName[job.name] = job
                rebuild()
            }
        case "agent":
            if let payload, let evt = payload.decode(OCAgentEvent.self) {
                applyAgentEvent(evt)
                rebuild()
            }
        case "health", "heartbeat", "tick":
            snapshot.lastUpdated = Date()
        default:
            break
        }
    }

    private func applyAgentEvent(_ evt: OCAgentEvent) {
        guard let agentID = OpenClawStatusMapping.agentID(fromSessionKey: evt.sessionKey) else { return }
        if evt.stream == "lifecycle" {
            switch evt.data?.phase {
            case "end": agentActivity[agentID] = nil
            case "error": agentActivity[agentID] = .error
            case "start": agentActivity[agentID] = .running
            default: break
            }
            return
        }
        if let status = OpenClawStatusMapping.agentActivity(stream: evt.stream) {
            agentActivity[agentID] = status
        }
    }

    /// Reassemble the published snapshot from reducer state.
    private func rebuild() {
        var cron = CronSummary()
        for job in cronByName.values {
            let status = OpenClawStatusMapping.cronStatus(job.state)
            switch status {
            case .ok: cron.ok += 1
            case .running: cron.running += 1
            case .error: cron.error += 1
            case .unknown: cron.unknown += 1
            case .disabled: cron.disabled += 1
            }
            if let err = job.state?.lastError, !err.isEmpty { cron.lastError = err }
            cron.jobs.append(AgentRollupItem(
                id: job.name,
                name: job.name,
                status: status,
                detail: job.state?.lastError
            ))
        }
        cron.jobs.sort { $0.name < $1.name }

        let agentRows = agents.map { info in
            AgentRollupItem(
                id: info.id,
                name: info.displayName,
                emoji: info.displayEmoji,
                status: agentActivity[info.id] ?? .ok,
                detail: info.primaryModel
            )
        }

        snapshot.agents = agentRows
        snapshot.cron = cron
        snapshot.channels = channels.sorted { $0.name < $1.name }
        snapshot.usage = usage
        snapshot.lastUpdated = Date()
    }

    // MARK: - Payload decoding (shape-flexible, mirrors periclaw)

    /// `cron.list` payload is `{jobs:[...]}`, `{crons:[...]}`, or a bare array.
    static func decodeCronJobs(_ payload: OCJSON) -> [OCCronJob] {
        if let arr = payload.decode([OCCronJob].self) { return arr }
        if let obj = payload["jobs"], let arr = obj.decode([OCCronJob].self) { return arr }
        if let obj = payload["crons"], let arr = obj.decode([OCCronJob].self) { return arr }
        return []
    }

    /// `channels.status` is `{channels:[...]}` or a bare array.
    static func decodeChannels(_ payload: OCJSON) -> [OCChannel] {
        if let arr = payload.decode([OCChannel].self) { return arr }
        if let obj = payload["channels"], let arr = obj.decode([OCChannel].self) { return arr }
        return []
    }

    /// `sessions.list` is `{sessions:[...]}` or a bare array. The glance shows the
    /// most-recently-updated session's totals.
    static func decodeUsage(_ payload: OCJSON) -> SessionUsageRollup? {
        var sessions: [OCSessionInfo] = []
        if let arr = payload.decode([OCSessionInfo].self) {
            sessions = arr
        } else if let obj = payload["sessions"], let arr = obj.decode([OCSessionInfo].self) {
            sessions = arr
        }
        guard let latest = sessions.max(by: { ($0.updatedAt ?? 0) < ($1.updatedAt ?? 0) }) else { return nil }
        let updatedAt = latest.updatedAt.map { Date(timeIntervalSince1970: Double($0) / 1000) }
        return SessionUsageRollup(
            totalTokens: Int(latest.totalTokens ?? 0),
            contextTokens: Int(latest.contextTokens ?? 0),
            inputTokens: Int(latest.inputTokens ?? 0),
            outputTokens: Int(latest.outputTokens ?? 0),
            updatedAt: updatedAt
        )
    }

    /// Synthesize a `CronJob` from a push `cron` event (started → running;
    /// finished → status/error). Other actions imply no live status change.
    static func cronJobFromEvent(_ payload: OCJSON, idToName: [String: String]) -> OCCronJob? {
        guard let evt = payload.decode(OCCronEvent.self), let action = evt.action else { return nil }
        let name = evt.jobName ?? evt.jobId.flatMap { idToName[$0] } ?? evt.jobId ?? "unknown"
        switch action {
        case "started":
            return OCCronJob(
                name: name,
                id: evt.jobId,
                state: OCCronState(
                    nextRunAtMs: nil,
                    lastRunAtMs: nil,
                    lastStatus: nil,
                    lastDurationMs: nil,
                    lastError: nil,
                    running: true
                )
            )
        case "finished":
            return OCCronJob(
                name: name,
                id: evt.jobId,
                state: OCCronState(
                    nextRunAtMs: evt.nextRunAtMs,
                    lastRunAtMs: evt.runAtMs,
                    lastStatus: evt.status,
                    lastDurationMs: evt.durationMs,
                    lastError: evt.error,
                    running: false
                )
            )
        default:
            return nil
        }
    }
}

/// What the WS client streams to the service: connection establishment plus
/// every steady-state frame. Carried through an `AsyncStream` so the reducer
/// sees them in order on the main actor.
enum OpenClawClientEvent {
    case connected
    case frame(OCEnvelope)
}
