import Foundation

/// Pure state machine that folds OpenClaw gateway frames into the data sections
/// of an `AgentRuntimeSnapshot` (agents / cron / channels / usage). Extracted
/// from `OpenClawService` so it's unit-testable without any networking — the
/// service owns one of these and only adds connection/pairing/timestamp state.
///
/// Mirrors periclaw's frame dispatch + status derivation. Shape-flexible decoding
/// (bare array vs `{jobs:[…]}` etc.) is preserved because OpenClaw ships frequent
/// gateway updates.
struct OpenClawSnapshotReducer {
    private var cronByName: [String: OCCronJob] = [:]
    private var cronIDToName: [String: String] = [:]
    private var channels: [ChannelStatus] = []
    private var agents: [OCAgentInfo] = []
    /// Transient per-agent activity overriding resting `.ok`, cleared on a
    /// `lifecycle` end. Keyed by agent id.
    private var agentActivity: [String: AgentStatus] = [:]
    private var usage: SessionUsageRollup?

    /// Fold one frame into the reducer state. Returns `true` if the frame changed
    /// data sections (so the caller knows whether to republish), `false` for
    /// frames it ignores. Liveness-only events (`health`/`heartbeat`/`tick`) are
    /// not data changes and return `false`.
    @discardableResult
    mutating func ingest(_ env: OCEnvelope) -> Bool {
        switch env.type {
        case "res": reduceResponse(id: env.id, payload: env.payload)
        case "event": reduceEvent(name: env.event, payload: env.payload)
        default: false
        }
    }

    private mutating func reduceResponse(id: String?, payload: OCJSON?) -> Bool {
        guard let id, let payload else { return false }
        switch id {
        case "cron.list":
            for job in Self.decodeCronJobs(payload) {
                cronByName[job.name] = job
                if let jid = job.id { cronIDToName[jid] = job.name }
            }
            return true
        case "channels.status":
            channels = Self.decodeChannels(payload).map {
                ChannelStatus(
                    id: $0.name,
                    name: $0.name,
                    status: OpenClawStatusMapping.channelStatus($0),
                    lastError: $0.lastError
                )
            }
            return true
        case "sessions.list":
            usage = Self.decodeUsage(payload)
            return true
        case "agents.list":
            if let resp = payload.decode(OCAgentsListResponse.self), let list = resp.agents {
                agents = list
                return true
            }
            return false
        default:
            return false
        }
    }

    private mutating func reduceEvent(name: String?, payload: OCJSON?) -> Bool {
        guard let name else { return false }
        switch name {
        case "cron":
            if let payload, let job = Self.cronJobFromEvent(payload, idToName: cronIDToName) {
                cronByName[job.name] = job
                return true
            }
            return false
        case "agent":
            if let payload, let evt = payload.decode(OCAgentEvent.self) {
                applyAgentEvent(evt)
                return true
            }
            return false
        default:
            return false
        }
    }

    private mutating func applyAgentEvent(_ evt: OCAgentEvent) {
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

    // MARK: - Section assembly

    var agentRows: [AgentRollupItem] {
        agents.map { info in
            AgentRollupItem(
                id: info.id,
                name: info.displayName,
                emoji: info.displayEmoji,
                status: agentActivity[info.id] ?? .ok,
                detail: info.primaryModel
            )
        }
    }

    var cronSummary: CronSummary {
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
        return cron
    }

    var channelStatuses: [ChannelStatus] {
        channels.sorted { $0.name < $1.name }
    }

    var usageRollup: SessionUsageRollup? {
        usage
    }

    // MARK: - Payload decoding (shape-flexible, mirrors periclaw)

    /// `cron.list` payload is `{jobs:[…]}`, `{crons:[…]}`, or a bare array.
    static func decodeCronJobs(_ payload: OCJSON) -> [OCCronJob] {
        if let arr = payload.decode([OCCronJob].self) { return arr }
        if let obj = payload["jobs"], let arr = obj.decode([OCCronJob].self) { return arr }
        if let obj = payload["crons"], let arr = obj.decode([OCCronJob].self) { return arr }
        return []
    }

    /// `channels.status` is `{channels:[…]}` or a bare array.
    static func decodeChannels(_ payload: OCJSON) -> [OCChannel] {
        if let arr = payload.decode([OCChannel].self) { return arr }
        if let obj = payload["channels"], let arr = obj.decode([OCChannel].self) { return arr }
        return []
    }

    /// `sessions.list` is `{sessions:[…]}` or a bare array. The glance shows the
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
            return OCCronJob(name: name, id: evt.jobId, state: OCCronState(
                nextRunAtMs: nil, lastRunAtMs: nil, lastStatus: nil,
                lastDurationMs: nil, lastError: nil, running: true
            ))
        case "finished":
            return OCCronJob(name: name, id: evt.jobId, state: OCCronState(
                nextRunAtMs: evt.nextRunAtMs, lastRunAtMs: evt.runAtMs, lastStatus: evt.status,
                lastDurationMs: evt.durationMs, lastError: evt.error, running: false
            ))
        default:
            return nil
        }
    }
}
