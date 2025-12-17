import SwiftUI
import SwiftData

struct ProjectsView: View {
    @Query private var workspaces: [Workspace]
    @Query private var rootProjects: [Project]
    @Query private var unorganizedRepositories: [TrackedRepository]
    
    @State private var showAddProject = false
    @State private var showAddWorkspace = false
    @State private var selectedView: ProjectViewMode = .hierarchy
    @State private var expandedItems: Set<String> = []
    
    init() {
        // Query for root projects (no parent project and no workspace)
        let projectPredicate = #Predicate<Project> { project in
            project.parentProject == nil && project.workspace == nil
        }
        _rootProjects = Query(filter: projectPredicate, sort: \Project.name)
        
        // Query for unorganized repositories
        let repoPredicate = #Predicate<TrackedRepository> { repo in
            repo.project == nil && repo.workspace == nil
        }
        _unorganizedRepositories = Query(filter: repoPredicate, sort: \TrackedRepository.name)
    }
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            ProjectsHeaderView(
                selectedView: $selectedView,
                showAddWorkspace: $showAddWorkspace,
                showAddProject: $showAddProject
            )
            
            Divider()
            
            // Content based on selected view
            switch selectedView {
            case .hierarchy:
                HierarchyView(
                    workspaces: workspaces,
                    rootProjects: rootProjects,
                    unorganizedRepositories: unorganizedRepositories,
                    expandedItems: $expandedItems
                )
            case .grid:
                ProjectGridView(
                    workspaces: workspaces,
                    rootProjects: rootProjects,
                    unorganizedRepositories: unorganizedRepositories
                )
            case .list:
                ProjectListView(
                    workspaces: workspaces,
                    rootProjects: rootProjects,
                    unorganizedRepositories: unorganizedRepositories
                )
            }
        }
        .sheet(isPresented: $showAddWorkspace) {
            AddWorkspaceSheet()
        }
        .sheet(isPresented: $showAddProject) {
            AddProjectSheet(parentWorkspace: nil, parentProject: nil)
        }
    }
}

enum ProjectViewMode: String, CaseIterable {
    case hierarchy = "Hierarchy"
    case grid = "Grid"
    case list = "List"
    
    var icon: String {
        switch self {
        case .hierarchy:
            return "list.bullet.indent"
        case .grid:
            return "square.grid.2x2"
        case .list:
            return "list.bullet"
        }
    }
}

struct ProjectsHeaderView: View {
    @Binding var selectedView: ProjectViewMode
    @Binding var showAddWorkspace: Bool
    @Binding var showAddProject: Bool
    
    var body: some View {
        HStack {
            Text("Projects")
                .font(.largeTitle)
                .bold()
            
            Spacer()
            
            // View mode picker
            Picker("View", selection: $selectedView) {
                ForEach(ProjectViewMode.allCases, id: \.self) { mode in
                    Label(mode.rawValue, systemImage: mode.icon)
                        .tag(mode)
                }
            }
            .pickerStyle(.segmented)
            .frame(width: 200)
            
            // Add buttons
            Menu {
                Button(action: { showAddWorkspace = true }) {
                    Label("New Workspace", systemImage: "building.2")
                }
                
                Button(action: { showAddProject = true }) {
                    Label("New Project", systemImage: "folder.badge.plus")
                }
            } label: {
                Label("Add", systemImage: "plus")
            }
            .menuStyle(.borderlessButton)
        }
        .padding()
    }
}

// MARK: - Hierarchy View

struct HierarchyView: View {
    let workspaces: [Workspace]
    let rootProjects: [Project]
    let unorganizedRepositories: [TrackedRepository]
    @Binding var expandedItems: Set<String>
    
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                // Workspaces
                ForEach(workspaces) { workspace in
                    WorkspaceRowView(
                        workspace: workspace,
                        isExpanded: expandedItems.contains(workspace.id.uuidString)
                    ) {
                        toggleExpanded(workspace.id.uuidString)
                    }
                }
                
                // Root Projects (not in workspaces)
                ForEach(rootProjects) { project in
                    ProjectRowView(
                        project: project,
                        depth: 0,
                        isExpanded: expandedItems.contains(project.id.uuidString)
                    ) {
                        toggleExpanded(project.id.uuidString)
                    }
                }
                
                // Unorganized Repositories
                if !unorganizedRepositories.isEmpty {
                    UnorganizedSection(repositories: unorganizedRepositories)
                }
            }
            .padding()
        }
    }
    
    private func toggleExpanded(_ id: String) {
        if expandedItems.contains(id) {
            expandedItems.remove(id)
        } else {
            expandedItems.insert(id)
        }
    }
}

struct WorkspaceRowView: View {
    let workspace: Workspace
    let isExpanded: Bool
    let toggleExpanded: () -> Void
    
    @Environment(\.modelContext) private var modelContext
    @EnvironmentObject private var gitMonitorService: GitMonitorService
    @State private var showEditSheet = false
    @State private var showAddProjectSheet = false
    @State private var showDeleteConfirmation = false
    @State private var expandedProjects: Set<String> = []
    
    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Button(action: toggleExpanded) {
                    Image(systemName: isExpanded ? "chevron.down" : "chevron.right")
                        .foregroundColor(.secondary)
                        .frame(width: 20)
                }
                .buttonStyle(.plain)
                
                Image(systemName: workspace.icon ?? "building.2")
                    .foregroundColor(.accentColor)
                
                Text(workspace.name)
                    .font(.headline)
                
                // Status badge
                StatusBadge(
                    color: workspace.aggregatedStatus.color,
                    text: workspace.aggregatedStatus.description
                )
                
                Spacer()
                
                // Repository count
                Text("\(workspace.totalRepositoryCount) repos")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            .padding(.vertical, 8)
            .padding(.horizontal, 12)
            .background(Color(NSColor.controlBackgroundColor))
            .cornerRadius(8)
            .contentShape(Rectangle())
            .contextMenu {
                // Workspace Actions
                Button(action: { showEditSheet = true }) {
                    Label("Edit Workspace", systemImage: "pencil")
                }
                
                Button(action: { showAddProjectSheet = true }) {
                    Label("Add Project", systemImage: "folder.badge.plus")
                }
                
                Divider()
                
                // Refresh All
                if workspace.totalRepositoryCount > 0 {
                    Button(action: { refreshAllRepositories() }) {
                        Label("Refresh All Repositories", systemImage: "arrow.clockwise")
                    }
                }
                
                // Expansion Actions
                Menu("Expand/Collapse") {
                    Button(action: { expandAll() }) {
                        Label("Expand All", systemImage: "arrow.down.right.and.arrow.up.left")
                    }
                    Button(action: { collapseAll() }) {
                        Label("Collapse All", systemImage: "arrow.up.left.and.arrow.down.right")
                    }
                }
                
                Divider()
                
                // Set as Default
                Button(action: { setAsDefault() }) {
                    Label("Set as Default for New Repos", systemImage: "star")
                }
                
                Divider()
                
                // Copy Actions
                Menu("Copy") {
                    Button(action: { copyToClipboard(workspace.name) }) {
                        Label("Copy Name", systemImage: "doc.on.clipboard")
                    }
                    
                    Button(action: { copyWorkspaceStructure() }) {
                        Label("Copy Structure", systemImage: "doc.on.clipboard")
                    }
                }
                
                Divider()
                
                // Destructive Actions
                Button(role: .destructive, action: { showDeleteConfirmation = true }) {
                    Label("Delete Workspace", systemImage: "trash")
                }
            }
            .sheet(isPresented: $showEditSheet) {
                EditWorkspaceSheet(workspace: workspace)
            }
            .sheet(isPresented: $showAddProjectSheet) {
                AddProjectSheet(parentWorkspace: workspace, parentProject: nil)
            }
            .confirmationDialog(
                "Delete Workspace",
                isPresented: $showDeleteConfirmation,
                titleVisibility: .visible
            ) {
                Button("Delete and Move All to Unorganized", role: .destructive) {
                    deleteWorkspace()
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Delete \"\(workspace.name)\"? All projects and repositories will be moved to unorganized.")
            }
            
            if isExpanded {
                VStack(spacing: 8) {
                    // Workspace's direct repositories
                    if let repos = workspace.repositories, !repos.isEmpty {
                        ForEach(repos.sorted(by: { $0.name < $1.name })) { repo in
                            RepositoryRowView(repository: repo, depth: 1)
                        }
                    }
                    
                    // Workspace's projects
                    if let projects = workspace.projects {
                        ForEach(projects.sorted(by: { $0.name < $1.name })) { project in
                            ProjectRowView(
                                project: project,
                                depth: 1,
                                isExpanded: expandedProjects.contains(project.id.uuidString)
                            ) {
                                if expandedProjects.contains(project.id.uuidString) {
                                    expandedProjects.remove(project.id.uuidString)
                                } else {
                                    expandedProjects.insert(project.id.uuidString)
                                }
                            }
                        }
                    }
                }
                .padding(.leading, 24)
            }
        }
    }
    
    private func refreshAllRepositories() {
        Task {
            for repo in workspace.allRepositories {
                await gitMonitorService.refreshRepository(repo)
            }
        }
    }
    
    private func expandAll() {
        // Add all project IDs to expanded set
        if let projects = workspace.projects {
            for project in projects {
                expandedProjects.insert(project.id.uuidString)
            }
        }
    }
    
    private func collapseAll() {
        expandedProjects.removeAll()
    }
    
    private func setAsDefault() {
        // Store in user defaults or app settings
        UserDefaults.standard.set(workspace.id.uuidString, forKey: "defaultWorkspaceId")
    }
    
    private func copyToClipboard(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }
    
    private func copyWorkspaceStructure() {
        var structure = "\(workspace.name)\n"
        
        // Direct repositories
        for repo in (workspace.repositories ?? []).sorted(by: { $0.name < $1.name }) {
            structure += "├─ \(repo.name)\n"
        }
        
        // Projects
        for project in (workspace.projects ?? []).sorted(by: { $0.name < $1.name }) {
            structure += "├─ \(project.name)\n"
            generateProjectStructure(for: project, indent: 1, into: &structure)
        }
        
        copyToClipboard(structure)
    }
    
    private func generateProjectStructure(for project: Project, indent: Int, into result: inout String) {
        let prefix = String(repeating: "  ", count: indent)
        
        for repo in (project.repositories ?? []).sorted(by: { $0.name < $1.name }) {
            result += "\(prefix)└─ \(repo.name)\n"
        }
        
        for subProject in (project.subProjects ?? []).sorted(by: { $0.name < $1.name }) {
            result += "\(prefix)├─ \(subProject.name)\n"
            generateProjectStructure(for: subProject, indent: indent + 1, into: &result)
        }
    }
    
    private func deleteWorkspace() {
        // Move all projects to root level
        if let projects = workspace.projects {
            for project in projects {
                project.workspace = nil
            }
        }
        
        // Move all repositories to unorganized
        if let repos = workspace.repositories {
            for repo in repos {
                repo.workspace = nil
            }
        }
        
        modelContext.delete(workspace)
        try? modelContext.save()
    }
}

struct ProjectRowView: View {
    let project: Project
    let depth: Int
    let isExpanded: Bool
    let toggleExpanded: () -> Void
    
    @Environment(\.modelContext) private var modelContext
    @EnvironmentObject private var gitMonitorService: GitMonitorService
    @State private var showEditSheet = false
    @State private var showAddSubprojectSheet = false
    @State private var showDeleteConfirmation = false
    @State private var expandedItems: Set<String> = []
    
    var body: some View {
        VStack(spacing: 0) {
            HStack {
                if project.totalRepositoryCount > 0 || !(project.subProjects?.isEmpty ?? true) {
                    Button(action: toggleExpanded) {
                        Image(systemName: isExpanded ? "chevron.down" : "chevron.right")
                            .foregroundColor(.secondary)
                            .frame(width: 20)
                    }
                    .buttonStyle(.plain)
                } else {
                    Color.clear.frame(width: 20)
                }
                
                Image(systemName: project.icon ?? "folder")
                    .foregroundColor(.accentColor)
                
                Text(project.name)
                    .font(depth == 0 ? .headline : .subheadline)
                
                // Project type badge
                Text(project.projectType.displayName)
                    .font(.caption)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(Color.accentColor.opacity(0.2))
                    .cornerRadius(4)
                
                // Status
                if project.totalRepositoryCount > 0 {
                    StatusBadge(
                        color: project.aggregatedStatus.color,
                        text: "\(project.repositoriesNeedingAttention)/\(project.totalRepositoryCount)"
                    )
                }
                
                Spacer()
            }
            .padding(.vertical, 6)
            .padding(.horizontal, 12)
            .padding(.leading, CGFloat(depth * 20))
            .background(depth == 0 ? Color(NSColor.controlBackgroundColor) : Color.clear)
            .cornerRadius(depth == 0 ? 8 : 0)
            .contentShape(Rectangle())
            .contextMenu {
                // Project Actions
                Button(action: { showEditSheet = true }) {
                    Label("Edit Project", systemImage: "pencil")
                }
                
                Button(action: { showAddSubprojectSheet = true }) {
                    Label("Add Subproject", systemImage: "folder.badge.plus")
                }
                
                Divider()
                
                // Refresh All Repositories
                if project.totalRepositoryCount > 0 {
                    Button(action: { refreshAllRepositories() }) {
                        Label("Refresh All Repositories", systemImage: "arrow.clockwise")
                    }
                }
                
                // Expansion Actions
                Menu("Expand/Collapse") {
                    Button(action: { expandAll() }) {
                        Label("Expand All", systemImage: "arrow.down.right.and.arrow.up.left")
                    }
                    Button(action: { collapseAll() }) {
                        Label("Collapse All", systemImage: "arrow.up.left.and.arrow.down.right")
                    }
                }
                
                Divider()
                
                // Organization Actions
                if depth == 0 && project.workspace == nil {
                    Button(action: { convertToWorkspace() }) {
                        Label("Convert to Workspace", systemImage: "building.2")
                    }
                }
                
                if project.parentProject != nil {
                    Button(action: { moveToRoot() }) {
                        Label("Move to Root", systemImage: "arrow.up.to.line")
                    }
                }
                
                Divider()
                
                // Copy Actions
                Menu("Copy") {
                    Button(action: { copyToClipboard(project.name) }) {
                        Label("Copy Name", systemImage: "doc.on.clipboard")
                    }
                    
                    Button(action: { copyProjectStructure() }) {
                        Label("Copy Structure", systemImage: "doc.on.clipboard")
                    }
                }
                
                Divider()
                
                // Destructive Actions
                Button(role: .destructive, action: { showDeleteConfirmation = true }) {
                    Label("Delete Project", systemImage: "trash")
                }
            }
            .sheet(isPresented: $showEditSheet) {
                EditProjectSheet(project: project)
            }
            .sheet(isPresented: $showAddSubprojectSheet) {
                AddProjectSheet(parentWorkspace: project.workspace, parentProject: project)
            }
            .confirmationDialog(
                "Delete Project",
                isPresented: $showDeleteConfirmation,
                titleVisibility: .visible
            ) {
                Button("Delete and Move Contents to Unorganized", role: .destructive) {
                    deleteProject(moveContents: true)
                }
                Button("Delete Including All Contents", role: .destructive) {
                    deleteProject(moveContents: false)
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Delete \"\(project.name)\" and all its subprojects? This action cannot be undone.")
            }
            
            if isExpanded {
                VStack(spacing: 4) {
                    // Project's repositories
                    if let repos = project.repositories {
                        ForEach(repos.sorted(by: { $0.name < $1.name })) { repo in
                            RepositoryRowView(repository: repo, depth: depth + 1)
                        }
                    }
                    
                    // Subprojects
                    if let subProjects = project.subProjects {
                        ForEach(subProjects.sorted(by: { $0.name < $1.name })) { subProject in
                            ProjectRowView(
                                project: subProject,
                                depth: depth + 1,
                                isExpanded: expandedItems.contains(subProject.id.uuidString),
                                toggleExpanded: {
                                    if expandedItems.contains(subProject.id.uuidString) {
                                        expandedItems.remove(subProject.id.uuidString)
                                    } else {
                                        expandedItems.insert(subProject.id.uuidString)
                                    }
                                }
                            )
                        }
                    }
                }
            }
        }
    }
    
    private func refreshAllRepositories() {
        Task {
            for repo in project.allRepositories {
                await gitMonitorService.refreshRepository(repo)
            }
        }
    }
    
    private func expandAll() {
        // Add all subproject IDs to expanded set
        for subProject in project.allSubProjects {
            expandedItems.insert(subProject.id.uuidString)
        }
    }
    
    private func collapseAll() {
        // Clear all expanded items
        expandedItems.removeAll()
    }
    
    private func convertToWorkspace() {
        // Create new workspace from project
        let workspace = Workspace(
            name: project.name,
            description: project.projectDescription,
            color: project.color,
            icon: project.icon
        )
        
        // Move all repositories and subprojects
        workspace.repositories = project.repositories
        
        // Update repository relationships
        for repo in project.repositories ?? [] {
            repo.project = nil
            repo.workspace = workspace
        }
        
        // Move subprojects to workspace
        if let subProjects = project.subProjects {
            for subProject in subProjects {
                subProject.parentProject = nil
                subProject.workspace = workspace
            }
        }
        
        modelContext.insert(workspace)
        modelContext.delete(project)
        try? modelContext.save()
    }
    
    private func moveToRoot() {
        project.parentProject = nil
        try? modelContext.save()
    }
    
    private func copyToClipboard(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }
    
    private func copyProjectStructure() {
        // Generate text representation of project structure
        var structure = "\(project.name)\n"
        generateStructureText(for: project, indent: 0, into: &structure)
        copyToClipboard(structure)
    }
    
    private func generateStructureText(for project: Project, indent: Int, into result: inout String) {
        let prefix = String(repeating: "  ", count: indent)
        
        // Add repositories
        for repo in (project.repositories ?? []).sorted(by: { $0.name < $1.name }) {
            result += "\(prefix)└─ \(repo.name)\n"
        }
        
        // Add subprojects
        for subProject in (project.subProjects ?? []).sorted(by: { $0.name < $1.name }) {
            result += "\(prefix)├─ \(subProject.name)\n"
            generateStructureText(for: subProject, indent: indent + 1, into: &result)
        }
    }
    
    private func deleteProject(moveContents: Bool) {
        if moveContents {
            // Move all repositories to unorganized
            for repo in project.allRepositories {
                repo.project = nil
                repo.workspace = nil
            }
        } else {
            // Delete all repositories
            for repo in project.allRepositories {
                gitMonitorService.stopMonitoring(repo)
                modelContext.delete(repo)
            }
        }
        
        // Delete the project (cascade will handle subprojects)
        modelContext.delete(project)
        try? modelContext.save()
    }
}

struct RepositoryRowView: View {
    let repository: TrackedRepository
    let depth: Int
    
    @Environment(\.modelContext) private var modelContext
    @EnvironmentObject private var gitMonitorService: GitMonitorService
    @State private var showEditSheet = false
    @State private var showMoveToSheet = false
    @State private var showDeleteConfirmation = false
    
    var body: some View {
        HStack {
            Color.clear.frame(width: 20)
            
            Image(systemName: repository.displayStatus.icon)
                .foregroundColor(Color(repository.displayStatus.color))
            
            Text(repository.name)
                .font(.body)
            
            if let branch = repository.currentBranch {
                Text(branch)
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(Color.secondary.opacity(0.1))
                    .cornerRadius(4)
            }
            
            // Tags
            if let tags = repository.tags, !tags.isEmpty {
                HStack(spacing: 4) {
                    ForEach(tags.prefix(3)) { tag in
                        TagBadge(tag: tag)
                    }
                    if tags.count > 3 {
                        Text("+\(tags.count - 3)")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                }
            }
            
            Spacer()
        }
        .padding(.vertical, 4)
        .padding(.horizontal, 12)
        .padding(.leading, CGFloat(depth * 20))
        .contentShape(Rectangle())
        .onTapGesture {
            NSWorkspace.shared.openTerminal(at: repository.path)
        }
        .contextMenu {
            // Git Actions
            Button(action: { Task { await gitMonitorService.refreshRepository(repository) } }) {
                Label("Refresh Status", systemImage: "arrow.clockwise")
            }
            
            Divider()
            
            // Open Actions
            Button(action: { NSWorkspace.shared.openTerminal(at: repository.path) }) {
                Label("Open in Terminal", systemImage: "terminal")
            }
            
            Button(action: { NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: repository.path) }) {
                Label("Open in Finder", systemImage: "folder")
            }
            
            if let githubId = repository.githubRepoIdentifier {
                Button(action: { 
                    if let url = URL(string: "https://github.com/\(githubId)") {
                        NSWorkspace.shared.open(url)
                    }
                }) {
                    Label("Open on GitHub", systemImage: "link")
                }
            }
            
            Divider()
            
            // Organization Actions
            Button(action: { showEditSheet = true }) {
                Label("Edit Repository", systemImage: "pencil")
            }
            
            Button(action: { showMoveToSheet = true }) {
                Label("Move to...", systemImage: "folder.badge.arrow.right")
            }
            
            Button(action: { togglePin() }) {
                Label(repository.isPinned ? "Unpin" : "Pin", 
                      systemImage: repository.isPinned ? "pin.slash" : "pin")
            }
            
            Divider()
            
            // Copy Actions
            Menu("Copy") {
                Button(action: { copyToClipboard(repository.path) }) {
                    Label("Copy Path", systemImage: "doc.on.clipboard")
                }
                
                Button(action: { copyToClipboard(repository.name) }) {
                    Label("Copy Name", systemImage: "doc.on.clipboard")
                }
                
                if let branch = repository.currentBranch {
                    Button(action: { copyToClipboard(branch) }) {
                        Label("Copy Branch", systemImage: "doc.on.clipboard")
                    }
                }
            }
            
            Divider()
            
            // Destructive Actions
            Button(role: .destructive, action: { showDeleteConfirmation = true }) {
                Label("Remove from Tracking", systemImage: "trash")
            }
        }
        .sheet(isPresented: $showEditSheet) {
            EditRepositorySheet(repository: repository)
        }
        .sheet(isPresented: $showMoveToSheet) {
            MoveRepositorySheet(repository: repository)
        }
        .confirmationDialog(
            "Remove Repository",
            isPresented: $showDeleteConfirmation,
            titleVisibility: .visible
        ) {
            Button("Remove", role: .destructive) {
                removeRepository()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Are you sure you want to stop tracking \"\(repository.name)\"? This will not delete the repository from disk.")
        }
    }
    
    private func togglePin() {
        repository.isPinned.toggle()
        try? modelContext.save()
    }
    
    private func copyToClipboard(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }
    
    private func removeRepository() {
        gitMonitorService.stopMonitoring(repository)
        modelContext.delete(repository)
        try? modelContext.save()
    }
}

struct UnorganizedSection: View {
    let repositories: [TrackedRepository]
    @State private var isExpanded = true
    
    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Button(action: { isExpanded.toggle() }) {
                    Image(systemName: isExpanded ? "chevron.down" : "chevron.right")
                        .foregroundColor(.secondary)
                        .frame(width: 20)
                }
                .buttonStyle(.plain)
                
                Image(systemName: "tray")
                    .foregroundColor(.secondary)
                
                Text("Unorganized")
                    .font(.headline)
                    .foregroundColor(.secondary)
                
                Text("\(repositories.count)")
                    .font(.caption)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 2)
                    .background(Color.secondary.opacity(0.2))
                    .cornerRadius(10)
                
                Spacer()
                
                Button("Organize") {
                    // Show organize sheet
                }
                .buttonStyle(.link)
            }
            .padding(.vertical, 8)
            .padding(.horizontal, 12)
            .background(Color(NSColor.controlBackgroundColor).opacity(0.5))
            .cornerRadius(8)
            
            if isExpanded {
                VStack(spacing: 4) {
                    ForEach(repositories) { repo in
                        RepositoryRowView(repository: repo, depth: 1)
                    }
                }
                .padding(.leading, 24)
            }
        }
    }
}

// MARK: - Grid View

struct ProjectGridView: View {
    let workspaces: [Workspace]
    let rootProjects: [Project]
    let unorganizedRepositories: [TrackedRepository]
    
    let columns = [
        GridItem(.adaptive(minimum: 300, maximum: 400), spacing: 16)
    ]
    
    var body: some View {
        ScrollView {
            LazyVGrid(columns: columns, spacing: 16) {
                ForEach(workspaces) { workspace in
                    WorkspaceCard(workspace: workspace)
                }
                
                ForEach(rootProjects) { project in
                    ProjectCard(project: project)
                }
                
                if !unorganizedRepositories.isEmpty {
                    UnorganizedCard(repositories: unorganizedRepositories)
                }
            }
            .padding()
        }
    }
}

// MARK: - List View

struct ProjectListView: View {
    let workspaces: [Workspace]
    let rootProjects: [Project]
    let unorganizedRepositories: [TrackedRepository]
    
    var body: some View {
        List {
            Section("Workspaces") {
                ForEach(workspaces) { workspace in
                    NavigationLink(destination: EmptyView()) {
                        HStack {
                            Image(systemName: workspace.icon ?? "building.2")
                            VStack(alignment: .leading) {
                                Text(workspace.name)
                                Text("\(workspace.totalRepositoryCount) repositories")
                                    .font(.caption)
                                    .foregroundColor(.secondary)
                            }
                        }
                    }
                }
            }
            
            Section("Projects") {
                ForEach(rootProjects) { project in
                    NavigationLink(destination: EmptyView()) {
                        HStack {
                            Image(systemName: project.icon ?? "folder")
                            VStack(alignment: .leading) {
                                Text(project.name)
                                Text(project.projectType.displayName)
                                    .font(.caption)
                                    .foregroundColor(.secondary)
                            }
                        }
                    }
                }
            }
            
            if !unorganizedRepositories.isEmpty {
                Section("Unorganized") {
                    ForEach(unorganizedRepositories) { repo in
                        HStack {
                            Image(systemName: repo.displayStatus.icon)
                                .foregroundColor(Color(repo.displayStatus.color))
                            Text(repo.name)
                        }
                    }
                }
            }
        }
        .listStyle(.sidebar)
    }
}

// MARK: - Helper Views

struct StatusBadge: View {
    let color: String
    let text: String
    
    var body: some View {
        Text(text)
            .font(.caption)
            .padding(.horizontal, 8)
            .padding(.vertical, 2)
            .background(Color(color).opacity(0.2))
            .overlay(
                RoundedRectangle(cornerRadius: 4)
                    .stroke(Color(color).opacity(0.4), lineWidth: 1)
            )
            .cornerRadius(4)
    }
}

struct TagBadge: View {
    let tag: Tag
    
    var body: some View {
        Text(tag.name)
            .font(.caption2)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(Color(tag.color).opacity(0.2))
            .cornerRadius(4)
    }
}

struct WorkspaceCard: View {
    let workspace: Workspace
    
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Image(systemName: workspace.icon ?? "building.2")
                    .font(.title2)
                    .foregroundColor(.accentColor)
                
                VStack(alignment: .leading) {
                    Text(workspace.name)
                        .font(.headline)
                    
                    if let description = workspace.workspaceDescription {
                        Text(description)
                            .font(.caption)
                            .foregroundColor(.secondary)
                            .lineLimit(2)
                    }
                }
                
                Spacer()
            }
            
            Divider()
            
            HStack {
                VStack(alignment: .leading) {
                    Text("\(workspace.totalRepositoryCount)")
                        .font(.title2)
                        .bold()
                    Text("Repositories")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
                
                Spacer()
                
                StatusBadge(
                    color: workspace.aggregatedStatus.color,
                    text: workspace.aggregatedStatus.description
                )
            }
        }
        .padding()
        .background(Color(NSColor.controlBackgroundColor))
        .cornerRadius(12)
        .shadow(color: Color.black.opacity(0.05), radius: 4, x: 0, y: 2)
    }
}

struct ProjectCard: View {
    let project: Project
    
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Image(systemName: project.icon ?? "folder")
                    .font(.title2)
                    .foregroundColor(.accentColor)
                
                VStack(alignment: .leading) {
                    Text(project.name)
                        .font(.headline)
                    
                    Text(project.projectType.displayName)
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
                
                Spacer()
            }
            
            if project.totalRepositoryCount > 0 {
                Divider()
                
                HStack {
                    VStack(alignment: .leading) {
                        Text("\(project.totalRepositoryCount)")
                            .font(.title2)
                            .bold()
                        Text("Repositories")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                    
                    Spacer()
                    
                    if project.repositoriesNeedingAttention > 0 {
                        StatusBadge(
                            color: project.aggregatedStatus.color,
                            text: "\(project.repositoriesNeedingAttention) need attention"
                        )
                    }
                }
            }
        }
        .padding()
        .background(Color(NSColor.controlBackgroundColor))
        .cornerRadius(12)
        .shadow(color: Color.black.opacity(0.05), radius: 4, x: 0, y: 2)
    }
}

struct UnorganizedCard: View {
    let repositories: [TrackedRepository]
    
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Image(systemName: "tray")
                    .font(.title2)
                    .foregroundColor(.secondary)
                
                Text("Unorganized")
                    .font(.headline)
                    .foregroundColor(.secondary)
                
                Spacer()
                
                Button("Organize") {
                    // Show organize sheet
                }
                .buttonStyle(.link)
            }
            
            Divider()
            
            Text("\(repositories.count) repositories")
                .font(.caption)
                .foregroundColor(.secondary)
        }
        .padding()
        .background(Color(NSColor.controlBackgroundColor).opacity(0.5))
        .cornerRadius(12)
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .stroke(style: StrokeStyle(lineWidth: 2, dash: [5]))
                .foregroundColor(.secondary.opacity(0.3))
        )
    }
}