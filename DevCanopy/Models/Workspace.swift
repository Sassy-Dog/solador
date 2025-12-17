import Foundation
import SwiftData

@Model
final class Workspace {
    var id: UUID = UUID()
    var name: String
    var workspaceDescription: String?
    var color: String?
    var icon: String?
    var createdAt: Date
    var updatedAt: Date
    
    // Relationships
    @Relationship(deleteRule: .cascade)
    var projects: [Project]? = []
    
    @Relationship
    var repositories: [TrackedRepository]? = []
    
    // Display preferences
    var isExpanded: Bool = true
    var sortOrder: Int = 0
    
    init(name: String, description: String? = nil, color: String? = nil, icon: String? = nil) {
        self.name = name
        self.workspaceDescription = description
        self.color = color
        self.icon = icon ?? "building.2"
        self.createdAt = Date()
        self.updatedAt = Date()
    }
    
    // Computed properties for status aggregation
    var totalRepositoryCount: Int {
        let directRepos = repositories?.count ?? 0
        let projectRepos = projects?.reduce(0) { count, project in
            count + project.totalRepositoryCount
        } ?? 0
        return directRepos + projectRepos
    }
    
    var repositoriesNeedingAttention: Int {
        let directNeedingAttention = repositories?.filter { repo in
            repo.displayStatus != .synced
        }.count ?? 0
        
        let projectNeedingAttention = projects?.reduce(0) { count, project in
            count + project.repositoriesNeedingAttention
        } ?? 0
        
        return directNeedingAttention + projectNeedingAttention
    }
    
    var aggregatedStatus: WorkspaceStatus {
        let needingAttention = repositoriesNeedingAttention
        let total = totalRepositoryCount
        
        if total == 0 {
            return .empty
        } else if needingAttention == 0 {
            return .allSynced
        } else if needingAttention == total {
            return .allNeedAttention
        } else {
            return .someNeedAttention(needingAttention)
        }
    }
    
    // Get all repositories including from projects
    var allRepositories: [TrackedRepository] {
        var result = repositories ?? []
        for project in (projects ?? []) {
            result.append(contentsOf: project.allRepositories)
        }
        return result
    }
}

enum WorkspaceStatus {
    case empty
    case allSynced
    case allNeedAttention
    case someNeedAttention(Int)
    
    var color: String {
        switch self {
        case .empty:
            return "gray"
        case .allSynced:
            return "green"
        case .allNeedAttention:
            return "red"
        case .someNeedAttention:
            return "orange"
        }
    }
    
    var icon: String {
        switch self {
        case .empty:
            return "folder"
        case .allSynced:
            return "checkmark.circle.fill"
        case .allNeedAttention:
            return "exclamationmark.circle.fill"
        case .someNeedAttention:
            return "exclamationmark.triangle.fill"
        }
    }
    
    var description: String {
        switch self {
        case .empty:
            return "No repositories"
        case .allSynced:
            return "All repositories synced"
        case .allNeedAttention:
            return "All repositories need attention"
        case .someNeedAttention(let count):
            return "\(count) repositories need attention"
        }
    }
}