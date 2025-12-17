import Foundation
import SwiftData

@Model
final class TrackedRepository {
    var id: UUID = UUID()
    var name: String
    var path: String
    var addedAt: Date
    var lastChecked: Date?
    
    // Git state
    var currentBranch: String?
    var trackingBranch: String?
    var aheadCount: Int
    var behindCount: Int
    var hasUncommittedChanges: Bool
    
    // Service links
    var githubRepoIdentifier: String?
    var vercelProjectId: String?
    
    // GitHub workflows
    @Relationship(deleteRule: .cascade)
    var githubWorkflows: [GitHubWorkflow]? = []
    
    // Organization relationships
    @Relationship(inverse: \Workspace.repositories)
    var workspace: Workspace?
    
    @Relationship(inverse: \Project.repositories)
    var project: Project?
    
    @Relationship(inverse: \Tag.repositories)
    var tags: [Tag]? = []
    
    // Custom metadata
    var customMetadata: [String: String]? = [:]
    
    // Display preferences
    var isPinned: Bool = false
    var sortOrder: Int = 0
    
    init(name: String, path: String) {
        self.name = name
        self.path = path
        self.addedAt = Date()
        self.aheadCount = 0
        self.behindCount = 0
        self.hasUncommittedChanges = false
    }
    
    var displayStatus: RepositoryStatus {
        if hasUncommittedChanges {
            return .uncommitted
        } else if aheadCount > 0 && behindCount > 0 {
            return .diverged
        } else if aheadCount > 0 {
            return .ahead
        } else if behindCount > 0 {
            return .behind
        } else {
            return .synced
        }
    }
    
    // Organization helpers
    var organizationPath: String {
        var components: [String] = []
        
        if let workspace = workspace {
            components.append(workspace.name)
        }
        
        if let project = project {
            // Build full project path
            var projectPath: [String] = [project.name]
            var current = project.parentProject
            while let parent = current {
                projectPath.insert(parent.name, at: 0)
                current = parent.parentProject
            }
            components.append(contentsOf: projectPath)
        }
        
        return components.joined(separator: " / ")
    }
    
    var isOrganized: Bool {
        workspace != nil || project != nil
    }
    
    // Tag helpers
    func hasTag(_ tagName: String) -> Bool {
        tags?.contains(where: { $0.name == tagName }) ?? false
    }
    
    func hasTagInCategory(_ category: TagCategory) -> Bool {
        tags?.contains(where: { $0.category == category }) ?? false
    }
    
    // GitHub workflow helpers
    var activeWorkflows: [GitHubWorkflow] {
        githubWorkflows?.filter { $0.isEnabled && $0.state == .active } ?? []
    }
    
    var hasFailingWorkflows: Bool {
        activeWorkflows.contains { workflow in
            workflow.latestRun?.isFailure ?? false
        }
    }
    
    var workflowSummary: (success: Int, failure: Int, running: Int) {
        var success = 0
        var failure = 0
        var running = 0
        
        for workflow in activeWorkflows {
            if let latestRun = workflow.latestRun {
                if latestRun.isInProgress {
                    running += 1
                } else if latestRun.isSuccess {
                    success += 1
                } else if latestRun.isFailure {
                    failure += 1
                }
            }
        }
        
        return (success, failure, running)
    }
}

enum RepositoryStatus {
    case synced
    case ahead
    case behind
    case diverged
    case uncommitted
    case error
    
    var color: String {
        switch self {
        case .synced:
            return "green"
        case .ahead:
            return "blue"
        case .behind:
            return "orange"
        case .diverged:
            return "red"
        case .uncommitted:
            return "yellow"
        case .error:
            return "red"
        }
    }
    
    var icon: String {
        switch self {
        case .synced:
            return "checkmark.circle.fill"
        case .ahead:
            return "arrow.up.circle.fill"
        case .behind:
            return "arrow.down.circle.fill"
        case .diverged:
            return "arrow.up.arrow.down.circle.fill"
        case .uncommitted:
            return "pencil.circle.fill"
        case .error:
            return "exclamationmark.circle.fill"
        }
    }
    
    var description: String {
        switch self {
        case .synced:
            return "In sync"
        case .ahead:
            return "Ahead"
        case .behind:
            return "Behind"
        case .diverged:
            return "Diverged"
        case .uncommitted:
            return "Uncommitted changes"
        case .error:
            return "Error"
        }
    }
}