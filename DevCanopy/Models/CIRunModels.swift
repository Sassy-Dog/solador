import Foundation

// Live CI model types consumed by the cockpit's Portfolio CI path
// (`Services/GitHub/PortfolioCIMapping.swift`). The dead v1 `@Model`
// workflow classes that previously lived alongside these were deleted in
// issue #30; these enums/structs are the surviving, still-used types.

/// Status of a GitHub Actions workflow run (the `status` field).
enum RunStatus: String, Codable {
    case requested = "requested"
    case queued = "queued"
    case pending = "pending"
    case waiting = "waiting"
    case inProgress = "in_progress"
    case completed = "completed"
}

/// Conclusion of a completed GitHub Actions workflow run (the `conclusion` field).
enum RunConclusion: String, Codable {
    case success = "success"
    case failure = "failure"
    case neutral = "neutral"
    case cancelled = "cancelled"
    case skipped = "skipped"
    case timedOut = "timed_out"
    case actionRequired = "action_required"
    case stale = "stale"
    case startupFailure = "startup_failure"
}

/// Per-workflow display preferences. Retained as a live type for UI configuration.
struct WorkflowDisplayOptions: Codable, Equatable {
    var showInDashboard: Bool = true
    var showInRepositoryCard: Bool = true
    var badgeStyle: WorkflowBadgeStyle = .iconAndText
    var prominentForRelease: Bool = true  // Show release version prominently
    var notifyOnFailure: Bool = true
    var notifyOnSuccess: Bool = false
}

enum WorkflowBadgeStyle: String, Codable {
    case iconOnly = "icon"
    case textOnly = "text"
    case iconAndText = "iconAndText"
    case detailed = "detailed"  // Shows run number, duration, etc.
}
