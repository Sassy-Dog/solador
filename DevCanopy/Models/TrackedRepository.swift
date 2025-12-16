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