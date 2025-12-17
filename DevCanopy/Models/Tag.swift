import Foundation
import SwiftData

@Model
final class Tag {
    var id: UUID = UUID()
    var name: String
    var color: String
    var category: TagCategory
    var createdAt: Date
    
    // Relationships
    @Relationship
    var repositories: [TrackedRepository]? = []
    
    init(name: String, color: String, category: TagCategory = .custom) {
        self.id = UUID()
        self.name = name
        self.color = color
        self.category = category
        self.createdAt = Date()
    }
    
    // Computed properties
    var repositoryCount: Int {
        repositories?.count ?? 0
    }
    
    var repositoriesNeedingAttention: Int {
        repositories?.filter { repo in
            repo.displayStatus != .synced
        }.count ?? 0
    }
    
    // Predefined tags for common categories
    static func defaultTags() -> [Tag] {
        [
            // Technology tags
            Tag(name: "React", color: "blue", category: .technology),
            Tag(name: "Swift", color: "orange", category: .technology),
            Tag(name: "Node.js", color: "green", category: .technology),
            Tag(name: "Python", color: "yellow", category: .technology),
            Tag(name: "Go", color: "cyan", category: .technology),
            Tag(name: "TypeScript", color: "blue", category: .technology),
            
            // Type tags
            Tag(name: "Frontend", color: "purple", category: .type),
            Tag(name: "Backend", color: "indigo", category: .type),
            Tag(name: "Mobile", color: "teal", category: .type),
            Tag(name: "Infrastructure", color: "gray", category: .type),
            Tag(name: "Documentation", color: "brown", category: .type),
            
            // Status tags
            Tag(name: "Active Development", color: "green", category: .status),
            Tag(name: "Maintenance", color: "orange", category: .status),
            Tag(name: "Deprecated", color: "red", category: .status),
            Tag(name: "Archived", color: "gray", category: .status),
        ]
    }
}

enum TagCategory: String, CaseIterable, Codable {
    case technology = "technology"
    case type = "type"
    case team = "team"
    case status = "status"
    case custom = "custom"
    
    var displayName: String {
        switch self {
        case .technology:
            return "Technology"
        case .type:
            return "Type"
        case .team:
            return "Team"
        case .status:
            return "Status"
        case .custom:
            return "Custom"
        }
    }
    
    var icon: String {
        switch self {
        case .technology:
            return "cpu"
        case .type:
            return "square.stack"
        case .team:
            return "person.3"
        case .status:
            return "flag"
        case .custom:
            return "tag"
        }
    }
}