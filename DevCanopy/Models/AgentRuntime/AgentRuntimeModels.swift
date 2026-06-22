import Foundation

// Runtime-agnostic value types describing a monitored AI-agent runtime
// (OpenClaw today, hermes later). The cockpit panel reads **only**
// `AgentRuntimeSnapshot`; no runtime's wire types ever cross this boundary, so
// adding a second runtime is "produce the same snapshot from a different
// protocol" rather than a panel/domain change.

/// Operational state of an agent / cron / channel — the colored status dot.
/// Mirrors periclaw's `domain::AgentStatus` so the status-derivation rules port
/// across verbatim.
enum AgentStatus: String {
    case running
    case ok
    case error
    case unknown
    case disabled
}

/// One renderable row in a runtime section (an agent, a cron job, etc.).
struct AgentRollupItem: Identifiable {
    let id: String
    let name: String
    let emoji: String?
    let status: AgentStatus
    /// Secondary line — model ref, last error, "next run in 12m", etc.
    let detail: String?

    init(id: String, name: String, emoji: String? = nil, status: AgentStatus, detail: String? = nil) {
        self.id = id
        self.name = name
        self.emoji = emoji
        self.status = status
        self.detail = detail
    }
}

/// Glanceable cron rollup: counts by status plus the worst recent error.
struct CronSummary {
    var ok = 0
    var running = 0
    var error = 0
    var unknown = 0
    var disabled = 0
    /// Most recent non-empty cron `lastError`, surfaced as the one error line.
    var lastError: String?
    /// Per-job rows (kept for an optional drill / tooltip; the glance uses counts).
    var jobs: [AgentRollupItem] = []

    var total: Int {
        ok + running + error + unknown + disabled
    }
}

/// A messaging channel provider (Slack / Telegram / WhatsApp) + its dot.
struct ChannelStatus: Identifiable {
    let id: String
    let name: String
    let status: AgentStatus
    let lastError: String?
}

/// Token-usage rollup for the most relevant session.
struct SessionUsageRollup {
    let totalTokens: Int
    let contextTokens: Int
    let inputTokens: Int
    let outputTokens: Int
    let updatedAt: Date?
}

/// Liveness of a runtime's connection, for the panel's connection line.
enum RuntimeConnectionState: Equatable {
    case idle
    case connecting
    case connected
    case disconnected(reason: String)
}

/// Surfaced (in both the panel banner and Settings) while the gateway is
/// waiting for the operator to approve this device's pairing request.
struct PairingState: Equatable {
    enum Kind { case firstPair, scopeUpgrade }

    /// SHA-256(pubkey) hex fingerprint of this install's device key.
    let deviceID: String
    /// `requestId` the operator passes to `openclaw devices approve <id>`.
    let requestID: String?
    let kind: Kind
    /// Server-authored hint, shown verbatim when present.
    let remediationHint: String?
}

/// The single type the cockpit panel knows about. Every runtime reports this.
struct AgentRuntimeSnapshot: Identifiable {
    /// Stable runtime id ("openclaw", "hermes").
    let id: String
    /// Human-facing label ("OpenClaw").
    let displayName: String
    var connection: RuntimeConnectionState
    var agents: [AgentRollupItem]
    var cron: CronSummary
    var channels: [ChannelStatus]
    var usage: SessionUsageRollup?
    /// Non-nil while pairing approval is pending.
    var pairing: PairingState?
    var lastUpdated: Date?

    /// An empty, idle snapshot for a runtime that hasn't connected yet.
    static func idle(id: String, displayName: String) -> AgentRuntimeSnapshot {
        AgentRuntimeSnapshot(
            id: id,
            displayName: displayName,
            connection: .idle,
            agents: [],
            cron: CronSummary(),
            channels: [],
            usage: nil,
            pairing: nil,
            lastUpdated: nil
        )
    }
}
