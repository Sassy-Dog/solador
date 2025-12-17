import SwiftUI
import SwiftData

struct EditRepositorySheet: View {
    let repository: TrackedRepository
    
    @Environment(\.dismiss) private var dismiss
    @Environment(\.modelContext) private var modelContext
    @EnvironmentObject private var gitMonitorService: GitMonitorService
    @Query private var workspaces: [Workspace]
    @Query private var projects: [Project]
    @Query private var tags: [Tag]
    
    @State private var displayName: String = ""
    @State private var selectedWorkspace: Workspace?
    @State private var selectedProject: Project?
    @State private var selectedTags: Set<Tag> = []
    @State private var isPinned: Bool = false
    @State private var githubRepoIdentifier: String = ""
    @State private var customMetadata: [MetadataItem] = []
    @State private var errorMessage: String?
    
    init(repository: TrackedRepository) {
        self.repository = repository
    }
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text("Edit Repository")
                    .font(.title2)
                    .bold()
                
                Spacer()
                
                Button("Cancel") {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)
            }
            .padding()
            
            Divider()
            
            // Form
            Form {
                Section("Repository Info") {
                    HStack {
                        Text("Path")
                            .foregroundColor(.secondary)
                        Spacer()
                        Text(repository.path)
                            .font(.system(.body, design: .monospaced))
                            .textSelection(.enabled)
                    }
                    
                    TextField("Display Name", text: $displayName)
                        .textFieldStyle(.roundedBorder)
                        .help("Override the folder name for display")
                    
                    Toggle("Pin to Dashboard", isOn: $isPinned)
                }
                
                Section("Service Connections") {
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            TextField("GitHub Repository", text: $githubRepoIdentifier, prompt: Text("owner/repo"))
                                .textFieldStyle(.roundedBorder)
                                .help("Enter the GitHub repository in format: owner/repo")
                            
                            if !githubRepoIdentifier.isEmpty && githubRepoIdentifier == repository.githubRepoIdentifier {
                                Text("(auto-detected)")
                                    .font(.caption)
                                    .foregroundColor(.secondary)
                            }
                        }
                        
                        if githubRepoIdentifier.isEmpty {
                            Button(action: { detectGitHubRepo() }) {
                                Label("Detect from Git Remote", systemImage: "magnifyingglass")
                            }
                            .buttonStyle(.link)
                        }
                    }
                }
                
                Section("Organization") {
                    Picker("Workspace", selection: $selectedWorkspace) {
                        Text("None").tag(nil as Workspace?)
                        ForEach(workspaces) { workspace in
                            Text(workspace.name).tag(workspace as Workspace?)
                        }
                    }
                    
                    Picker("Project", selection: $selectedProject) {
                        Text("None").tag(nil as Project?)
                        ForEach(availableProjects) { project in
                            HStack {
                                if project.depth > 0 {
                                    Text(String(repeating: "  ", count: project.depth))
                                }
                                Text(project.name)
                            }
                            .tag(project as Project?)
                        }
                    }
                    .disabled(availableProjects.isEmpty)
                }
                
                Section("Tags") {
                    ScrollView {
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 100))], alignment: .leading, spacing: 8) {
                            ForEach(groupedTags.keys.sorted(), id: \.self) { category in
                                if let categoryTags = groupedTags[category] {
                                    VStack(alignment: .leading, spacing: 4) {
                                        Text(category)
                                            .font(.caption)
                                            .foregroundColor(.secondary)
                                        
                                        ForEach(categoryTags) { tag in
                                            TagToggle(tag: tag, isSelected: selectedTags.contains(tag)) {
                                                if selectedTags.contains(tag) {
                                                    selectedTags.remove(tag)
                                                } else {
                                                    selectedTags.insert(tag)
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    .frame(maxHeight: 150)
                }
                
                // GitHub Workflows section
                if repository.githubRepoIdentifier != nil {
                    Section("GitHub Workflows") {
                        if repository.githubWorkflows?.isEmpty ?? true {
                            HStack {
                                Image(systemName: "arrow.triangle.2.circlepath")
                                    .foregroundColor(.secondary)
                                Text("No workflows configured")
                                    .foregroundColor(.secondary)
                                Spacer()
                                Button("Fetch Workflows") {
                                    Task {
                                        await gitMonitorService.refreshWorkflows(for: repository)
                                    }
                                }
                                .buttonStyle(.link)
                            }
                        } else {
                            VStack(alignment: .leading, spacing: 8) {
                                ForEach(repository.githubWorkflows ?? []) { workflow in
                                    HStack {
                                        Toggle(isOn: Binding(
                                            get: { workflow.isEnabled },
                                            set: { workflow.isEnabled = $0 }
                                        )) {
                                            VStack(alignment: .leading) {
                                                Text(workflow.name)
                                                    .font(.body)
                                                Text(workflow.path)
                                                    .font(.caption)
                                                    .foregroundColor(.secondary)
                                            }
                                        }
                                        
                                        if let latestRun = workflow.latestRun {
                                            Image(systemName: latestRun.statusIcon)
                                                .foregroundColor(Color(latestRun.statusColor))
                                        }
                                    }
                                    .padding(.vertical, 2)
                                }
                                
                                Button("Refresh Workflows") {
                                    Task {
                                        await gitMonitorService.refreshWorkflows(for: repository)
                                    }
                                }
                                .buttonStyle(.link)
                                .padding(.top, 4)
                            }
                        }
                    }
                }
                
                Section("Custom Metadata") {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach($customMetadata) { $item in
                            HStack {
                                TextField("Key", text: $item.key)
                                    .textFieldStyle(.roundedBorder)
                                    .frame(width: 120)
                                
                                TextField("Value", text: $item.value)
                                    .textFieldStyle(.roundedBorder)
                                
                                Button(action: { removeMetadata(item) }) {
                                    Image(systemName: "minus.circle.fill")
                                        .foregroundColor(.red)
                                }
                                .buttonStyle(.plain)
                            }
                        }
                        
                        Button(action: { addMetadata() }) {
                            Label("Add Metadata", systemImage: "plus.circle.fill")
                        }
                        .buttonStyle(.link)
                    }
                }
                
                if let error = errorMessage {
                    Text(error)
                        .foregroundColor(.red)
                        .font(.caption)
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
            
            Divider()
            
            // Footer
            HStack {
                Text("Changes are saved automatically")
                    .font(.caption)
                    .foregroundColor(.secondary)
                
                Spacer()
                
                Button("Done") {
                    saveChanges()
                }
                .keyboardShortcut(.defaultAction)
            }
            .padding()
        }
        .frame(width: 600, height: 700)
        .onAppear {
            loadRepositoryData()
        }
    }
    
    private var availableProjects: [Project] {
        if let workspace = selectedWorkspace {
            return workspace.projects?.sorted(by: { $0.name < $1.name }) ?? []
        } else {
            return projects.filter { $0.workspace == nil && $0.parentProject == nil }
                          .sorted(by: { $0.name < $1.name })
        }
    }
    
    private var groupedTags: [String: [Tag]] {
        Dictionary(grouping: tags.sorted(by: { $0.name < $1.name })) { tag in
            tag.category.displayName
        }
    }
    
    private func loadRepositoryData() {
        displayName = repository.name
        isPinned = repository.isPinned
        githubRepoIdentifier = repository.githubRepoIdentifier ?? ""
        selectedWorkspace = repository.workspace
        selectedProject = repository.project
        
        if let tags = repository.tags {
            selectedTags = Set(tags)
        }
        
        if let metadata = repository.customMetadata {
            customMetadata = metadata.map { MetadataItem(key: $0.key, value: $0.value) }
        }
    }
    
    private func saveChanges() {
        // Update display name only if changed
        if !displayName.isEmpty && displayName != repository.name {
            repository.name = displayName
        }
        
        repository.isPinned = isPinned
        repository.githubRepoIdentifier = githubRepoIdentifier.isEmpty ? nil : githubRepoIdentifier
        repository.workspace = selectedWorkspace
        repository.project = selectedProject
        repository.tags = Array(selectedTags)
        
        // Save custom metadata
        var metadata: [String: String] = [:]
        for item in customMetadata where !item.key.isEmpty {
            metadata[item.key] = item.value
        }
        repository.customMetadata = metadata.isEmpty ? nil : metadata
        
        do {
            try modelContext.save()
            
            // If GitHub repo identifier was added/changed, refresh workflows
            if repository.githubRepoIdentifier != nil && 
               (githubRepoIdentifier != (repository.githubRepoIdentifier ?? "")) {
                Task {
                    await gitMonitorService.refreshWorkflows(for: repository)
                }
            }
            
            dismiss()
        } catch {
            errorMessage = "Failed to save changes: \(error.localizedDescription)"
        }
    }
    
    private func addMetadata() {
        customMetadata.append(MetadataItem(key: "", value: ""))
    }
    
    private func removeMetadata(_ item: MetadataItem) {
        customMetadata.removeAll { $0.id == item.id }
    }
    
    private func detectGitHubRepo() {
        Task {
            let gitCommand = GitCommand(workingDirectory: repository.path)
            if let detectedRepo = await gitCommand.getGitHubRepoIdentifier() {
                githubRepoIdentifier = detectedRepo
            }
        }
    }
}

// Make GitCommand available here temporarily
private struct GitCommand {
    let workingDirectory: String
    
    func getGitHubRepoIdentifier() async -> String? {
        await withCheckedContinuation { continuation in
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/bin/git")
            process.currentDirectoryURL = URL(fileURLWithPath: workingDirectory)
            process.arguments = ["remote", "-v"]
            
            let pipe = Pipe()
            process.standardOutput = pipe
            process.standardError = pipe
            
            do {
                try process.run()
                process.waitUntilExit()
                
                let data = pipe.fileHandleForReading.readDataToEndOfFile()
                let output = String(data: data, encoding: .utf8) ?? ""
                let lines = output.components(separatedBy: .newlines)
                
                // Look for origin first, then any GitHub remote
                var githubUrls: [(name: String, url: String)] = []
                
                for line in lines {
                    let components = line.split(separator: "\t", maxSplits: 1).map(String.init)
                    guard components.count >= 2 else { continue }
                    
                    let remoteName = components[0]
                    let urlPart = components[1].split(separator: " ").first.map(String.init) ?? ""
                    
                    if urlPart.contains("github.com") {
                        githubUrls.append((name: remoteName, url: urlPart))
                    }
                }
                
                // Prefer origin
                let targetUrl = githubUrls.first(where: { $0.name == "origin" })?.url ?? githubUrls.first?.url
                
                guard let url = targetUrl else {
                    continuation.resume(returning: nil)
                    return
                }
                
                // Parse GitHub URL formats
                if url.contains("git@github.com:") {
                    // SSH format
                    let repoPath = url
                        .replacingOccurrences(of: "git@github.com:", with: "")
                        .replacingOccurrences(of: ".git", with: "")
                    continuation.resume(returning: repoPath.isEmpty ? nil : repoPath)
                } else if url.contains("github.com/") {
                    // HTTPS format
                    let pattern = #"github\.com/([^/]+/[^/\.]+)"#
                    if let regex = try? NSRegularExpression(pattern: pattern, options: []),
                       let match = regex.firstMatch(in: url, options: [], range: NSRange(url.startIndex..., in: url)) {
                        if let range = Range(match.range(at: 1), in: url) {
                            continuation.resume(returning: String(url[range]))
                            return
                        }
                    }
                    continuation.resume(returning: nil)
                } else {
                    continuation.resume(returning: nil)
                }
            } catch {
                continuation.resume(returning: nil)
            }
        }
    }
}

struct TagToggle: View {
    let tag: Tag
    let isSelected: Bool
    let action: () -> Void
    
    var body: some View {
        Button(action: action) {
            HStack(spacing: 4) {
                Image(systemName: isSelected ? "checkmark.circle.fill" : "circle")
                    .font(.caption)
                Text(tag.name)
                    .font(.caption)
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(Color(tag.color).opacity(isSelected ? 0.3 : 0.1))
            .overlay(
                RoundedRectangle(cornerRadius: 6)
                    .stroke(Color(tag.color).opacity(isSelected ? 0.8 : 0.3), lineWidth: 1)
            )
            .cornerRadius(6)
        }
        .buttonStyle(.plain)
    }
}

struct MetadataItem: Identifiable {
    let id = UUID()
    var key: String
    var value: String
}

// MARK: - Move Repository Sheet

struct MoveRepositorySheet: View {
    let repository: TrackedRepository
    
    @Environment(\.dismiss) private var dismiss
    @Environment(\.modelContext) private var modelContext
    @Query private var workspaces: [Workspace]
    @Query private var projects: [Project]
    
    @State private var selectedWorkspace: Workspace?
    @State private var selectedProject: Project?
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text("Move Repository")
                    .font(.title2)
                    .bold()
                
                Spacer()
                
                Button("Cancel") {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)
            }
            .padding()
            
            Divider()
            
            // Current Location
            VStack(alignment: .leading, spacing: 12) {
                Text("Move \"\(repository.name)\" to:")
                    .font(.headline)
                
                if !repository.organizationPath.isEmpty {
                    HStack {
                        Text("Currently in:")
                            .foregroundColor(.secondary)
                        Text(repository.organizationPath)
                    }
                    .font(.caption)
                }
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
            
            // Destination Selection
            List {
                Section("Move to Unorganized") {
                    Button(action: { moveToUnorganized() }) {
                        HStack {
                            Image(systemName: "tray")
                            Text("Unorganized")
                            Spacer()
                        }
                    }
                    .buttonStyle(.plain)
                }
                
                Section("Workspaces") {
                    ForEach(workspaces) { workspace in
                        Button(action: { moveToWorkspace(workspace) }) {
                            HStack {
                                Image(systemName: workspace.icon ?? "building.2")
                                    .foregroundColor(Color(workspace.color ?? "blue"))
                                Text(workspace.name)
                                Spacer()
                                if repository.workspace == workspace {
                                    Image(systemName: "checkmark")
                                        .foregroundColor(.accentColor)
                                }
                            }
                        }
                        .buttonStyle(.plain)
                    }
                }
                
                Section("Projects") {
                    ForEach(projects.filter { $0.parentProject == nil }) { project in
                        ProjectMoveRow(
                            project: project,
                            currentProject: repository.project,
                            indent: 0
                        ) { selectedProject in
                            moveToProject(selectedProject)
                        }
                    }
                }
            }
            .listStyle(.sidebar)
            
            Divider()
            
            // Footer
            HStack {
                Spacer()
                Button("Cancel") {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)
            }
            .padding()
        }
        .frame(width: 500, height: 600)
    }
    
    private func moveToUnorganized() {
        repository.workspace = nil
        repository.project = nil
        try? modelContext.save()
        dismiss()
    }
    
    private func moveToWorkspace(_ workspace: Workspace) {
        repository.workspace = workspace
        repository.project = nil
        try? modelContext.save()
        dismiss()
    }
    
    private func moveToProject(_ project: Project) {
        repository.workspace = project.workspace
        repository.project = project
        try? modelContext.save()
        dismiss()
    }
}

struct ProjectMoveRow: View {
    let project: Project
    let currentProject: Project?
    let indent: Int
    let onSelect: (Project) -> Void
    
    var body: some View {
        VStack(spacing: 0) {
            Button(action: { onSelect(project) }) {
                HStack {
                    if indent > 0 {
                        Text(String(repeating: "  ", count: indent))
                    }
                    
                    Image(systemName: project.icon ?? "folder")
                        .foregroundColor(Color(project.color ?? "blue"))
                    
                    Text(project.name)
                    
                    Text(project.projectType.displayName)
                        .font(.caption)
                        .foregroundColor(.secondary)
                    
                    Spacer()
                    
                    if currentProject == project {
                        Image(systemName: "checkmark")
                            .foregroundColor(.accentColor)
                    }
                }
            }
            .buttonStyle(.plain)
            
            if let subProjects = project.subProjects {
                ForEach(subProjects.sorted(by: { $0.name < $1.name })) { subProject in
                    ProjectMoveRow(
                        project: subProject,
                        currentProject: currentProject,
                        indent: indent + 1,
                        onSelect: onSelect
                    )
                }
            }
        }
    }
}