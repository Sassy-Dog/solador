import Foundation

// Decodable shapes for the OpenClaw gateway wire protocol, ported from
// periclaw's `net/rpc.rs`. Intentionally loose — every field optional, unknown
// keys ignored — because OpenClaw ships frequent gateway updates and we'd
// rather tolerate drift than hard-fail a frame parse. These types stay private
// to the OpenClaw service; nothing outside `Services/OpenClaw/` sees them.

// MARK: - Envelope

/// A top-level WS frame: `{type, id?, event?, ok?, error?, payload?}`. We decode
/// `payload`/`error` lazily per-message rather than into one giant union.
struct OCEnvelope: Decodable {
    let type: String?
    let id: String?
    let event: String?
    let ok: Bool?
    let payload: OCJSON?
    let error: OCError?
}

/// Type-erased JSON value. Lets us capture `payload` generically, then re-decode
/// it into the concrete response type once we've routed the frame by `id`/`event`
/// (round-trips through `JSONEncoder`/`JSONDecoder`). `Decodable` shapes alone
/// can't do this because they need to know the target type up front.
enum OCJSON: Codable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([OCJSON])
    case object([String: OCJSON])

    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() {
            self = .null
        } else if let v = try? c.decode(Bool.self) {
            self = .bool(v)
        } else if let v = try? c.decode(Double.self) {
            self = .number(v)
        } else if let v = try? c.decode(String.self) {
            self = .string(v)
        } else if let v = try? c.decode([OCJSON].self) {
            self = .array(v)
        } else if let v = try? c.decode([String: OCJSON].self) {
            self = .object(v)
        } else {
            self = .null
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case let .bool(v): try c.encode(v)
        case let .number(v): try c.encode(v)
        case let .string(v): try c.encode(v)
        case let .array(v): try c.encode(v)
        case let .object(v): try c.encode(v)
        }
    }

    /// Re-decode this JSON value into a concrete `Decodable` type.
    func decode<T: Decodable>(_: T.Type) -> T? {
        guard let data = try? JSONEncoder().encode(self) else { return nil }
        return try? JSONDecoder().decode(T.self, from: data)
    }

    /// Convenience for `.object` member access (e.g. routing on `payload.type`).
    subscript(key: String) -> OCJSON? {
        if case let .object(dict) = self { return dict[key] }
        return nil
    }

    var stringValue: String? {
        if case let .string(s) = self { return s }
        return nil
    }
}

struct OCError: Decodable {
    let code: String?
    let message: String?
    let details: OCErrorDetails?
}

struct OCErrorDetails: Decodable {
    let code: String?
    let requestId: String?
    let reason: String?
    let deviceId: String?
    let remediationHint: String?
}

// MARK: - RPC payload shapes

struct OCCronJob: Decodable {
    let name: String
    let id: String?
    let state: OCCronState?
}

struct OCCronState: Decodable {
    let nextRunAtMs: Int64?
    let lastRunAtMs: Int64?
    let lastStatus: String?
    let lastDurationMs: Int64?
    let lastError: String?
    let running: Bool?
}

struct OCCronListResponse: Decodable {
    let jobs: [OCCronJob]?
    // Some gateway builds return a bare array; handled in the reducer via
    // `OCJSON` before decoding, so this covers the `{jobs:[...]}` object shape.
}

struct OCChannel: Decodable {
    let name: String
    let enabled: Bool?
    let connected: Bool?
    let lastError: String?
}

struct OCChannelsResponse: Decodable {
    let channels: [OCChannel]?
}

struct OCAgentIdentity: Decodable {
    let name: String?
    let emoji: String?
}

struct OCAgentModelRef: Decodable {
    let primary: String?
}

struct OCAgentInfo: Decodable {
    let id: String
    let name: String?
    let identity: OCAgentIdentity?
    let model: OCAgentModelRef?
    let workspace: String?

    /// `identity.name` → `name` → `id`.
    var displayName: String {
        if let n = identity?.name, !n.isEmpty { return n }
        if let n = name, !n.isEmpty { return n }
        return id
    }

    var displayEmoji: String? {
        guard let e = identity?.emoji, !e.isEmpty else { return nil }
        return e
    }

    var primaryModel: String? {
        model?.primary
    }
}

struct OCAgentsListResponse: Decodable {
    let defaultId: String?
    let agents: [OCAgentInfo]?
}

struct OCSessionInfo: Decodable {
    let key: String
    let totalTokens: Int64?
    let contextTokens: Int64?
    let inputTokens: Int64?
    let outputTokens: Int64?
    let updatedAt: Int64?
    let ageMs: Int64?
    let agentId: String?

    enum CodingKeys: String, CodingKey {
        // `sessions.list` uses `key`; `session.message` uses `sessionKey`.
        case key
        case sessionKey
        case totalTokens, contextTokens, inputTokens, outputTokens, updatedAt, ageMs, agentId
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        if let k = try c.decodeIfPresent(String.self, forKey: .key) {
            key = k
        } else {
            key = try c.decodeIfPresent(String.self, forKey: .sessionKey) ?? ""
        }
        totalTokens = try c.decodeIfPresent(Int64.self, forKey: .totalTokens)
        contextTokens = try c.decodeIfPresent(Int64.self, forKey: .contextTokens)
        inputTokens = try c.decodeIfPresent(Int64.self, forKey: .inputTokens)
        outputTokens = try c.decodeIfPresent(Int64.self, forKey: .outputTokens)
        updatedAt = try c.decodeIfPresent(Int64.self, forKey: .updatedAt)
        ageMs = try c.decodeIfPresent(Int64.self, forKey: .ageMs)
        agentId = try c.decodeIfPresent(String.self, forKey: .agentId)
    }
}

struct OCSessionsListResponse: Decodable {
    let sessions: [OCSessionInfo]?
}

/// Gateway broadcast `cron` event payload.
struct OCCronEvent: Decodable {
    let jobId: String?
    let jobName: String?
    let action: String?
    let runAtMs: Int64?
    let durationMs: Int64?
    let status: String?
    let error: String?
    let nextRunAtMs: Int64?
}

/// Gateway broadcast `agent` event payload — `stream` classifies activity,
/// `sessionKey` (`agent:<id>:<sid>`) routes it to an agent.
struct OCAgentEvent: Decodable {
    let stream: String?
    let sessionKey: String?
    let data: OCAgentEventData?
}

struct OCAgentEventData: Decodable {
    let phase: String?
}

// MARK: - Status derivation (ported verbatim from periclaw events.rs)

enum OpenClawStatusMapping {
    /// `running` → `.running`; `ok` → `.ok`; `error|failed|timeout` → `.error`;
    /// otherwise `.unknown`.
    static func cronStatus(_ state: OCCronState?) -> AgentStatus {
        guard let state else { return .unknown }
        if state.running == true { return .running }
        switch state.lastStatus {
        case "ok": return .ok
        case "error", "failed", "timeout": return .error
        default: return .unknown
        }
    }

    /// `!enabled` → `.disabled`; `lastError != nil` → `.error`;
    /// `connected` → `.ok`; otherwise `.unknown`.
    static func channelStatus(_ channel: OCChannel) -> AgentStatus {
        if channel.enabled != true { return .disabled }
        if let err = channel.lastError, !err.isEmpty { return .error }
        return channel.connected == true ? .ok : .unknown
    }

    /// Coarse agent activity from the `agent` event `stream`.
    /// `tool` → running; `item|assistant` → running; `error` → error;
    /// `lifecycle` is handled by the reducer via `data.phase`.
    static func agentActivity(stream: String?) -> AgentStatus? {
        switch stream {
        case "tool", "item", "assistant": .running
        case "error": .error
        default: nil
        }
    }

    /// Parse the agent id out of an `agent:<id>:<sessionId>` session key.
    static func agentID(fromSessionKey key: String?) -> String? {
        guard let key, key.hasPrefix("agent:") else { return nil }
        let parts = key.split(separator: ":")
        guard parts.count >= 2 else { return nil }
        return String(parts[1])
    }
}
