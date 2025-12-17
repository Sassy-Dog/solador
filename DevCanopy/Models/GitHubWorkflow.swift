import Foundation
import SwiftData

@Model
final class GitHubWorkflow {
    var id: UUID = UUID()
    var workflowId: Int64  // GitHub's workflow ID
    var nodeId: String     // GitHub's node ID
    var name: String
    var path: String       // .github/workflows/ci.yml
    var state: WorkflowState
    
    // Repository relationship
    @Relationship(inverse: \TrackedRepository.githubWorkflows)
    var repository: TrackedRepository?
    
    // Workflow runs
    @Relationship(deleteRule: .cascade, inverse: \GitHubWorkflowRun.workflow)
    var runs: [GitHubWorkflowRun]? = []
    
    // User preferences
    var isEnabled: Bool = true
    var displayOptions: WorkflowDisplayOptions
    
    // Metadata
    var createdAt: Date
    var updatedAt: Date
    var lastFetchedAt: Date?
    
    init(
        workflowId: Int64,
        nodeId: String,
        name: String,
        path: String,
        state: WorkflowState = .active
    ) {
        self.workflowId = workflowId
        self.nodeId = nodeId
        self.name = name
        self.path = path
        self.state = state
        self.displayOptions = WorkflowDisplayOptions()
        self.createdAt = Date()
        self.updatedAt = Date()
    }
    
    // Get the latest run
    var latestRun: GitHubWorkflowRun? {
        runs?.sorted(by: { $0.createdAt > $1.createdAt }).first
    }
    
    // Get runs for a specific branch
    func runs(for branch: String) -> [GitHubWorkflowRun] {
        runs?.filter { $0.headBranch == branch } ?? []
    }
    
    // Check if this is likely a release workflow
    var isReleaseWorkflow: Bool {
        name.lowercased().contains("release") ||
        name.lowercased().contains("publish") ||
        path.lowercased().contains("release")
    }
    
    // Check if this is likely a CI workflow
    var isCIWorkflow: Bool {
        name.lowercased().contains("ci") ||
        name.lowercased().contains("test") ||
        name.lowercased().contains("build") ||
        path.lowercased().contains("ci")
    }
}

enum WorkflowState: String, Codable {
    case active = "active"
    case deleted = "deleted"
    case disabled = "disabled_fork"
    case disabledManually = "disabled_manually"
    case disabledInactivity = "disabled_inactivity"
}

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

@Model
final class GitHubWorkflowRun {
    var id: UUID = UUID()
    var runId: Int64        // GitHub's run ID
    var runNumber: Int      // Sequential number for this workflow
    var nodeId: String      // GitHub's node ID
    
    // Workflow relationship
    @Relationship
    var workflow: GitHubWorkflow?
    
    // Run details
    var name: String        // Run name (can differ from workflow name)
    var displayTitle: String
    var status: RunStatus
    var conclusion: RunConclusion?
    var workflowUrl: String
    var htmlUrl: String
    
    // Git information
    var headBranch: String?
    var headSha: String
    var event: String       // push, pull_request, release, etc.
    
    // Timing
    var createdAt: Date
    var updatedAt: Date
    var runStartedAt: Date?
    var completedAt: Date?
    
    // Additional metadata
    var actor: String?      // User who triggered the run
    var triggerActor: String?  // User who triggered the rerun
    
    // Release information (if applicable)
    var releaseTag: String?
    var releaseTitle: String?
    
    init(
        runId: Int64,
        runNumber: Int,
        nodeId: String,
        name: String,
        displayTitle: String,
        status: RunStatus,
        headSha: String,
        event: String,
        htmlUrl: String,
        workflowUrl: String
    ) {
        self.runId = runId
        self.runNumber = runNumber
        self.nodeId = nodeId
        self.name = name
        self.displayTitle = displayTitle
        self.status = status
        self.headSha = headSha
        self.event = event
        self.htmlUrl = htmlUrl
        self.workflowUrl = workflowUrl
        self.createdAt = Date()
        self.updatedAt = Date()
    }
    
    // Computed properties
    var duration: TimeInterval? {
        guard let start = runStartedAt, let end = completedAt else { return nil }
        return end.timeIntervalSince(start)
    }
    
    var isSuccess: Bool {
        conclusion == .success
    }
    
    var isFailure: Bool {
        conclusion == .failure || conclusion == .timedOut || conclusion == .cancelled
    }
    
    var isInProgress: Bool {
        status == .inProgress || status == .queued || status == .requested || status == .waiting || status == .pending
    }
}

enum RunStatus: String, Codable {
    case requested = "requested"
    case queued = "queued"
    case pending = "pending"
    case waiting = "waiting"
    case inProgress = "in_progress"
    case completed = "completed"
}

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

// Extension to add computed properties for UI
extension GitHubWorkflowRun {
    var statusColor: String {
        if isInProgress {
            return "orange"
        }
        
        switch conclusion {
        case .success:
            return "green"
        case .failure, .timedOut, .startupFailure:
            return "red"
        case .cancelled, .skipped:
            return "gray"
        case .neutral, .stale:
            return "yellow"
        case .actionRequired:
            return "orange"
        case .none:
            return "gray"
        }
    }
    
    var statusIcon: String {
        if isInProgress {
            return "arrow.triangle.2.circlepath"
        }
        
        switch conclusion {
        case .success:
            return "checkmark.circle.fill"
        case .failure, .timedOut, .startupFailure:
            return "xmark.circle.fill"
        case .cancelled:
            return "stop.circle.fill"
        case .skipped:
            return "forward.circle.fill"
        case .neutral, .stale:
            return "minus.circle.fill"
        case .actionRequired:
            return "exclamationmark.circle.fill"
        case .none:
            return "questionmark.circle.fill"
        }
    }
    
    var durationString: String? {
        guard let duration = duration else { return nil }
        
        let minutes = Int(duration) / 60
        let seconds = Int(duration) % 60
        
        if minutes > 0 {
            return "\(minutes)m \(seconds)s"
        } else {
            return "\(seconds)s"
        }
    }
}