import Foundation
import SwiftData

@Model
final class Project {
    var id: UUID = UUID()
    var name: String
    var projectDescription: String?
    var projectType: ProjectType
    var color: String?
    var icon: String?
    var createdAt: Date
    var updatedAt: Date
    
    // Hierarchical relationships
    @Relationship(inverse: \Workspace.projects)
    var workspace: Workspace?
    
    @Relationship
    var parentProject: Project?
    
    @Relationship(deleteRule: .cascade, inverse: \Project.parentProject)
    var subProjects: [Project]? = []
    
    @Relationship
    var repositories: [TrackedRepository]? = []
    
    // Service connections at project level
    var githubOrgId: String?
    var vercelTeamId: String?
    
    // Display preferences
    var isExpanded: Bool = true
    var sortOrder: Int = 0
    
    init(name: String, type: ProjectType = .application, description: String? = nil) {
        self.name = name
        self.projectType = type
        self.projectDescription = description
        self.createdAt = Date()
        self.updatedAt = Date()
        
        // Set default icon based on type
        switch type {
        case .platform:
            self.icon = "square.stack.3d.up"
        case .application:
            self.icon = "app"
        case .library:
            self.icon = "books.vertical"
        case .service:
            self.icon = "server.rack"
        case .microfrontend:
            self.icon = "uiwindow.split.2x1"
        case .microservice:
            self.icon = "circle.hexagongrid"
        }
    }
    
    // Computed properties for hierarchy navigation
    var depth: Int {
        var count = 0
        var current = parentProject
        while current != nil {
            count += 1
            current = current?.parentProject
        }
        return count
    }
    
    var rootProject: Project {
        var current = self
        while let parent = current.parentProject {
            current = parent
        }
        return current
    }
    
    var allSubProjects: [Project] {
        var result: [Project] = []
        if let subs = subProjects {
            result.append(contentsOf: subs)
            for sub in subs {
                result.append(contentsOf: sub.allSubProjects)
            }
        }
        return result
    }
    
    // Repository counting and status aggregation
    var totalRepositoryCount: Int {
        let direct = repositories?.count ?? 0
        let subProjectRepos = subProjects?.reduce(0) { count, project in
            count + project.totalRepositoryCount
        } ?? 0
        return direct + subProjectRepos
    }
    
    var repositoriesNeedingAttention: Int {
        let directNeedingAttention = repositories?.filter { repo in
            repo.displayStatus != .synced
        }.count ?? 0
        
        let subProjectNeedingAttention = subProjects?.reduce(0) { count, project in
            count + project.repositoriesNeedingAttention
        } ?? 0
        
        return directNeedingAttention + subProjectNeedingAttention
    }
    
    var aggregatedStatus: ProjectStatus {
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
    
    // Get all repositories including from subprojects
    var allRepositories: [TrackedRepository] {
        var result = repositories ?? []
        for subProject in (subProjects ?? []) {
            result.append(contentsOf: subProject.allRepositories)
        }
        return result
    }
}

enum ProjectType: String, CaseIterable, Codable {
    case platform = "platform"
    case application = "application"
    case library = "library"
    case service = "service"
    case microfrontend = "microfrontend"
    case microservice = "microservice"
    
    var displayName: String {
        switch self {
        case .platform:
            return "Platform"
        case .application:
            return "Application"
        case .library:
            return "Library"
        case .service:
            return "Service"
        case .microfrontend:
            return "Microfrontend"
        case .microservice:
            return "Microservice"
        }
    }
}

enum ProjectStatus {
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
}