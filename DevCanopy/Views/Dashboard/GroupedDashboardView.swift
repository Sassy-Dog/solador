import SwiftUI
import SwiftData

struct GroupedDashboardView: View {
    @Query private var workspaces: [Workspace]
    @Query private var rootProjects: [Project]
    @Query private var unorganizedRepositories: [TrackedRepository]
    @Query private var services: [ServiceConnection]
    
    init() {
        // Query for root projects (no parent project and no workspace)
        let projectPredicate = #Predicate<Project> { project in
            project.parentProject == nil && project.workspace == nil
        }
        _rootProjects = Query(filter: projectPredicate, sort: \Project.sortOrder)
        
        // Query for unorganized repositories
        let repoPredicate = #Predicate<TrackedRepository> { repo in
            repo.project == nil && repo.workspace == nil
        }
        _unorganizedRepositories = Query(filter: repoPredicate, sort: \TrackedRepository.sortOrder)
    }
    
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                // Header
                DashboardHeaderView()
                
                // Quick Stats
                QuickStatsView(
                    workspaces: workspaces,
                    projects: rootProjects,
                    unorganized: unorganizedRepositories,
                    services: services
                )
                
                // Pinned Repositories
                if let pinnedRepos = getPinnedRepositories(), !pinnedRepos.isEmpty {
                    VStack(alignment: .leading, spacing: 12) {
                        Label("Pinned", systemImage: "pin.fill")
                            .font(.headline)
                            .padding(.horizontal)
                        
                        LazyVGrid(columns: [
                            GridItem(.adaptive(minimum: 300, maximum: 400), spacing: 16)
                        ], spacing: 16) {
                            ForEach(pinnedRepos) { repo in
                                RepositoryCard(repository: repo)
                            }
                        }
                        .padding(.horizontal)
                    }
                }
                
                // Workspaces
                ForEach(workspaces.sorted(by: { $0.sortOrder < $1.sortOrder })) { workspace in
                    WorkspaceDashboardSection(workspace: workspace)
                }
                
                // Root Projects
                ForEach(rootProjects.sorted(by: { $0.sortOrder < $1.sortOrder })) { project in
                    ProjectDashboardSection(project: project)
                }
                
                // Unorganized Repositories
                if !unorganizedRepositories.isEmpty {
                    UnorganizedDashboardSection(repositories: unorganizedRepositories)
                }
                
                // Services
                if !services.isEmpty {
                    ServicesDashboardSection(services: services)
                }
            }
            .padding(.vertical)
        }
        .background(Color(NSColor.windowBackgroundColor))
    }
    
    private func getPinnedRepositories() -> [TrackedRepository]? {
        let allRepos = workspaces.flatMap { $0.allRepositories } +
                      rootProjects.flatMap { $0.allRepositories } +
                      unorganizedRepositories
        
        let pinned = allRepos.filter { $0.isPinned }
        return pinned.isEmpty ? nil : pinned
    }
}

struct DashboardHeaderView: View {
    @EnvironmentObject private var gitMonitorService: GitMonitorService
    
    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 4) {
                Text("Dashboard")
                    .font(.largeTitle)
                    .bold()
                
                Text(Date().formatted(date: .complete, time: .omitted))
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            
            Spacer()
            
            Button(action: {
                Task {
                    await gitMonitorService.refreshAllRepositories()
                }
            }) {
                Label("Refresh All", systemImage: "arrow.clockwise")
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal)
    }
}

struct QuickStatsView: View {
    let workspaces: [Workspace]
    let projects: [Project]
    let unorganized: [TrackedRepository]
    let services: [ServiceConnection]
    
    private var totalRepositories: Int {
        workspaces.reduce(0) { $0 + $1.totalRepositoryCount } +
        projects.reduce(0) { $0 + $1.totalRepositoryCount } +
        unorganized.count
    }
    
    private var needingAttention: Int {
        workspaces.reduce(0) { $0 + $1.repositoriesNeedingAttention } +
        projects.reduce(0) { $0 + $1.repositoriesNeedingAttention } +
        unorganized.filter { $0.displayStatus != .synced }.count
    }
    
    var body: some View {
        HStack(spacing: 20) {
            StatCard(
                title: "Total Repositories",
                value: "\(totalRepositories)",
                icon: "folder.fill",
                color: .accentColor
            )
            
            StatCard(
                title: "Need Attention",
                value: "\(needingAttention)",
                icon: "exclamationmark.triangle.fill",
                color: needingAttention > 0 ? .orange : .green
            )
            
            StatCard(
                title: "Workspaces",
                value: "\(workspaces.count)",
                icon: "building.2.fill",
                color: .blue
            )
            
            StatCard(
                title: "Services",
                value: "\(services.count)",
                icon: "cloud.fill",
                color: .purple
            )
        }
        .padding(.horizontal)
    }
}

struct StatCard: View {
    let title: String
    let value: String
    let icon: String
    let color: Color
    
    var body: some View {
        HStack {
            Image(systemName: icon)
                .font(.title2)
                .foregroundColor(color)
                .frame(width: 40)
            
            VStack(alignment: .leading, spacing: 2) {
                Text(value)
                    .font(.title2)
                    .bold()
                Text(title)
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            
            Spacer()
        }
        .padding()
        .frame(maxWidth: .infinity)
        .background(Color(NSColor.controlBackgroundColor))
        .cornerRadius(8)
    }
}

struct WorkspaceDashboardSection: View {
    let workspace: Workspace
    @State private var isExpanded = true
    
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            // Section Header
            HStack {
                Button(action: { withAnimation { isExpanded.toggle() } }) {
                    HStack(spacing: 8) {
                        Image(systemName: isExpanded ? "chevron.down" : "chevron.right")
                            .foregroundColor(.secondary)
                            .frame(width: 16)
                        
                        Image(systemName: workspace.icon ?? "building.2")
                            .font(.title3)
                            .foregroundColor(Color(workspace.color ?? "blue"))
                        
                        Text(workspace.name)
                            .font(.title3)
                            .bold()
                        
                        if workspace.repositoriesNeedingAttention > 0 {
                            Label("\(workspace.repositoriesNeedingAttention)", systemImage: "exclamationmark.circle.fill")
                                .font(.caption)
                                .foregroundColor(.orange)
                        }
                    }
                }
                .buttonStyle(.plain)
                
                Spacer()
                
                Text("\(workspace.totalRepositoryCount) repositories")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            .padding(.horizontal)
            
            if isExpanded {
                // Direct repositories
                if let repos = workspace.repositories, !repos.isEmpty {
                    RepositoryGrid(repositories: repos)
                }
                
                // Projects in workspace
                if let projects = workspace.projects {
                    ForEach(projects.sorted(by: { $0.sortOrder < $1.sortOrder })) { project in
                        ProjectDashboardSection(project: project, inWorkspace: true)
                    }
                }
            }
        }
    }
}

struct ProjectDashboardSection: View {
    let project: Project
    var inWorkspace: Bool = false
    @State private var isExpanded = true
    
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            // Section Header
            HStack {
                Button(action: { withAnimation { isExpanded.toggle() } }) {
                    HStack(spacing: 8) {
                        Image(systemName: isExpanded ? "chevron.down" : "chevron.right")
                            .foregroundColor(.secondary)
                            .frame(width: 16)
                        
                        if inWorkspace {
                            // Indent for projects in workspaces
                            Color.clear.frame(width: 20)
                        }
                        
                        Image(systemName: project.icon ?? "folder")
                            .font(inWorkspace ? .body : .title3)
                            .foregroundColor(Color(project.color ?? "blue"))
                        
                        Text(project.name)
                            .font(inWorkspace ? .headline : .title3)
                            .bold(!inWorkspace)
                        
                        Text(project.projectType.displayName)
                            .font(.caption)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(Color.accentColor.opacity(0.1))
                            .cornerRadius(4)
                        
                        if project.repositoriesNeedingAttention > 0 {
                            Label("\(project.repositoriesNeedingAttention)", systemImage: "exclamationmark.circle.fill")
                                .font(.caption)
                                .foregroundColor(.orange)
                        }
                    }
                }
                .buttonStyle(.plain)
                
                Spacer()
                
                if project.totalRepositoryCount > 0 {
                    Text("\(project.totalRepositoryCount) repositories")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }
            .padding(.horizontal)
            .padding(.leading, inWorkspace ? 20 : 0)
            
            if isExpanded && project.totalRepositoryCount > 0 {
                // All repositories including from subprojects
                RepositoryGrid(
                    repositories: project.allRepositories,
                    indentLevel: inWorkspace ? 1 : 0
                )
            }
        }
    }
}

struct UnorganizedDashboardSection: View {
    let repositories: [TrackedRepository]
    @State private var isExpanded = true
    
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Button(action: { withAnimation { isExpanded.toggle() } }) {
                    HStack(spacing: 8) {
                        Image(systemName: isExpanded ? "chevron.down" : "chevron.right")
                            .foregroundColor(.secondary)
                            .frame(width: 16)
                        
                        Image(systemName: "tray")
                            .font(.title3)
                            .foregroundColor(.secondary)
                        
                        Text("Unorganized")
                            .font(.title3)
                            .foregroundColor(.secondary)
                        
                        Text("\(repositories.count)")
                            .font(.caption)
                            .padding(.horizontal, 8)
                            .padding(.vertical, 2)
                            .background(Color.secondary.opacity(0.2))
                            .cornerRadius(10)
                    }
                }
                .buttonStyle(.plain)
                
                Spacer()
                
                Button("Organize") {
                    // Show organize sheet
                }
                .buttonStyle(.link)
            }
            .padding(.horizontal)
            
            if isExpanded {
                RepositoryGrid(repositories: repositories)
            }
        }
    }
}

struct ServicesDashboardSection: View {
    let services: [ServiceConnection]
    
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("Connected Services", systemImage: "cloud")
                .font(.headline)
                .padding(.horizontal)
            
            LazyVGrid(columns: [
                GridItem(.adaptive(minimum: 200, maximum: 300), spacing: 16)
            ], spacing: 16) {
                ForEach(services) { service in
                    ServiceCard(service: service)
                }
            }
            .padding(.horizontal)
        }
    }
}

struct RepositoryGrid: View {
    let repositories: [TrackedRepository]
    var indentLevel: Int = 0
    
    var body: some View {
        LazyVGrid(columns: [
            GridItem(.adaptive(minimum: 300, maximum: 400), spacing: 16)
        ], spacing: 16) {
            ForEach(repositories.sorted(by: { 
                if $0.isPinned != $1.isPinned {
                    return $0.isPinned
                }
                return $0.sortOrder < $1.sortOrder
            })) { repo in
                RepositoryCard(repository: repo)
                    .overlay(alignment: .topTrailing) {
                        if repo.isPinned {
                            Image(systemName: "pin.fill")
                                .font(.caption)
                                .foregroundColor(.accentColor)
                                .padding(8)
                        }
                    }
            }
        }
        .padding(.horizontal)
        .padding(.leading, CGFloat(indentLevel * 20))
    }
}