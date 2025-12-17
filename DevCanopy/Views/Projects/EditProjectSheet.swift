import SwiftUI
import SwiftData

struct EditProjectSheet: View {
    let project: Project
    
    @Environment(\.dismiss) private var dismiss
    @Environment(\.modelContext) private var modelContext
    @Query private var workspaces: [Workspace]
    @Query private var allProjects: [Project]
    
    @State private var projectName: String = ""
    @State private var projectDescription: String = ""
    @State private var projectType: ProjectType = .application
    @State private var selectedColor: String = "blue"
    @State private var selectedIcon: String?
    @State private var selectedWorkspace: Workspace?
    @State private var selectedParentProject: Project?
    @State private var githubOrgId: String = ""
    @State private var vercelTeamId: String = ""
    @State private var errorMessage: String?
    
    private let availableColors = ["blue", "green", "orange", "red", "purple", "indigo", "teal", "pink", "gray"]
    private let availableIcons = [
        "folder", "folder.fill", "folder.circle", "square.stack.3d.up", 
        "app", "app.fill", "books.vertical", "server.rack",
        "uiwindow.split.2x1", "circle.hexagongrid", "cpu", "network"
    ]
    
    init(project: Project) {
        self.project = project
    }
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text("Edit Project")
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
                Section("Project Details") {
                    TextField("Project Name", text: $projectName)
                        .textFieldStyle(.roundedBorder)
                    
                    TextField("Description (optional)", text: $projectDescription, axis: .vertical)
                        .textFieldStyle(.roundedBorder)
                        .lineLimit(3...5)
                    
                    Picker("Type", selection: $projectType) {
                        ForEach(ProjectType.allCases, id: \.self) { type in
                            HStack {
                                Image(systemName: iconForProjectType(type))
                                Text(type.displayName)
                            }
                            .tag(type)
                        }
                    }
                    .pickerStyle(.menu)
                }
                
                Section("Appearance") {
                    VStack(alignment: .leading, spacing: 12) {
                        HStack {
                            Text("Icon")
                            Spacer()
                            HStack(spacing: 8) {
                                ForEach(availableIcons, id: \.self) { icon in
                                    Button(action: { selectedIcon = icon }) {
                                        Image(systemName: icon)
                                            .font(.title3)
                                            .frame(width: 30, height: 30)
                                            .background(Color.accentColor.opacity(selectedIcon == icon ? 0.2 : 0.1))
                                            .overlay(
                                                RoundedRectangle(cornerRadius: 6)
                                                    .stroke(Color.accentColor, lineWidth: selectedIcon == icon ? 2 : 0)
                                            )
                                            .cornerRadius(6)
                                    }
                                    .buttonStyle(.plain)
                                }
                            }
                        }
                        
                        HStack {
                            Text("Color")
                            Spacer()
                            HStack(spacing: 12) {
                                ForEach(availableColors, id: \.self) { color in
                                    Circle()
                                        .fill(Color(color))
                                        .frame(width: 24, height: 24)
                                        .overlay(
                                            Circle()
                                                .stroke(Color.primary, lineWidth: selectedColor == color ? 2 : 0)
                                        )
                                        .onTapGesture {
                                            selectedColor = color
                                        }
                                }
                            }
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
                    
                    Picker("Parent Project", selection: $selectedParentProject) {
                        Text("None (Root Level)").tag(nil as Project?)
                        ForEach(availableParentProjects) { parentProject in
                            HStack {
                                if parentProject.depth > 0 {
                                    Text(String(repeating: "  ", count: parentProject.depth))
                                }
                                Text(parentProject.name)
                            }
                            .tag(parentProject as Project?)
                        }
                    }
                }
                
                Section("Service Connections") {
                    TextField("GitHub Organization ID", text: $githubOrgId)
                        .textFieldStyle(.roundedBorder)
                        .help("e.g., 'myorg/myrepo' or just 'myorg'")
                    
                    TextField("Vercel Team ID", text: $vercelTeamId)
                        .textFieldStyle(.roundedBorder)
                        .help("Team ID from Vercel dashboard")
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
                Text("\(project.totalRepositoryCount) repositories in this project")
                    .font(.caption)
                    .foregroundColor(.secondary)
                
                Spacer()
                
                Button("Save") {
                    saveChanges()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(projectName.isEmpty)
            }
            .padding()
        }
        .frame(width: 600, height: 700)
        .onAppear {
            loadProjectData()
        }
    }
    
    private var availableParentProjects: [Project] {
        // Filter out current project and its descendants to prevent circular references
        let excludedIds = Set([project.id] + project.allSubProjects.map { $0.id })
        
        return allProjects
            .filter { !excludedIds.contains($0.id) }
            .filter { selectedWorkspace == nil || $0.workspace == selectedWorkspace }
            .sorted(by: { $0.name < $1.name })
    }
    
    private func iconForProjectType(_ type: ProjectType) -> String {
        switch type {
        case .platform:
            return "square.stack.3d.up"
        case .application:
            return "app"
        case .library:
            return "books.vertical"
        case .service:
            return "server.rack"
        case .microfrontend:
            return "uiwindow.split.2x1"
        case .microservice:
            return "circle.hexagongrid"
        }
    }
    
    private func loadProjectData() {
        projectName = project.name
        projectDescription = project.projectDescription ?? ""
        projectType = project.projectType
        selectedColor = project.color ?? "blue"
        selectedIcon = project.icon
        selectedWorkspace = project.workspace
        selectedParentProject = project.parentProject
        githubOrgId = project.githubOrgId ?? ""
        vercelTeamId = project.vercelTeamId ?? ""
    }
    
    private func saveChanges() {
        project.name = projectName
        project.projectDescription = projectDescription.isEmpty ? nil : projectDescription
        project.projectType = projectType
        project.color = selectedColor
        project.icon = selectedIcon ?? iconForProjectType(projectType)
        project.workspace = selectedWorkspace
        project.parentProject = selectedParentProject
        project.githubOrgId = githubOrgId.isEmpty ? nil : githubOrgId
        project.vercelTeamId = vercelTeamId.isEmpty ? nil : vercelTeamId
        project.updatedAt = Date()
        
        do {
            try modelContext.save()
            dismiss()
        } catch {
            errorMessage = "Failed to save changes: \(error.localizedDescription)"
        }
    }
}

// MARK: - Merge Projects Sheet

struct MergeProjectsSheet: View {
    let sourceProject: Project
    
    @Environment(\.dismiss) private var dismiss
    @Environment(\.modelContext) private var modelContext
    @Query private var projects: [Project]
    
    @State private var targetProject: Project?
    @State private var mergeSubprojects = true
    @State private var mergeRepositories = true
    @State private var deleteSource = true
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text("Merge Projects")
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
            
            // Merge Info
            VStack(alignment: .leading, spacing: 12) {
                Text("Merge \"\(sourceProject.name)\" into another project")
                    .font(.headline)
                
                HStack {
                    VStack(alignment: .leading) {
                        Text("Source")
                            .font(.caption)
                            .foregroundColor(.secondary)
                        Text(sourceProject.name)
                            .font(.headline)
                        Text("\(sourceProject.totalRepositoryCount) repositories")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    
                    Image(systemName: "arrow.right")
                        .font(.title2)
                        .foregroundColor(.secondary)
                    
                    VStack(alignment: .leading) {
                        Text("Target")
                            .font(.caption)
                            .foregroundColor(.secondary)
                        if let target = targetProject {
                            Text(target.name)
                                .font(.headline)
                            Text("\(target.totalRepositoryCount) repositories")
                                .font(.caption)
                                .foregroundColor(.secondary)
                        } else {
                            Text("Select target...")
                                .foregroundColor(.secondary)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .padding()
                .background(Color(NSColor.controlBackgroundColor))
                .cornerRadius(8)
            }
            .padding()
            
            // Target Selection
            List(selection: $targetProject) {
                ForEach(availableTargets) { project in
                    HStack {
                        Image(systemName: project.icon ?? "folder")
                            .foregroundColor(Color(project.color ?? "blue"))
                        VStack(alignment: .leading) {
                            Text(project.name)
                            Text("\(project.projectType.displayName) • \(project.totalRepositoryCount) repos")
                                .font(.caption)
                                .foregroundColor(.secondary)
                        }
                    }
                    .tag(project)
                }
            }
            .listStyle(.inset)
            
            // Options
            VStack(alignment: .leading, spacing: 8) {
                Toggle("Merge subprojects", isOn: $mergeSubprojects)
                Toggle("Merge repositories", isOn: $mergeRepositories)
                Toggle("Delete source project after merge", isOn: $deleteSource)
            }
            .padding()
            
            Divider()
            
            // Footer
            HStack {
                Spacer()
                
                Button("Cancel") {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)
                
                Button("Merge") {
                    performMerge()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(targetProject == nil)
            }
            .padding()
        }
        .frame(width: 600, height: 700)
    }
    
    private var availableTargets: [Project] {
        let excludedIds = Set([sourceProject.id] + sourceProject.allSubProjects.map { $0.id })
        return projects
            .filter { !excludedIds.contains($0.id) }
            .filter { $0 != sourceProject.parentProject } // Don't merge into parent
            .sorted(by: { $0.name < $1.name })
    }
    
    private func performMerge() {
        guard let target = targetProject else { return }
        
        // Move repositories
        if mergeRepositories {
            for repo in sourceProject.repositories ?? [] {
                repo.project = target
            }
        }
        
        // Move subprojects
        if mergeSubprojects {
            for subProject in sourceProject.subProjects ?? [] {
                subProject.parentProject = target
            }
        }
        
        // Delete source if requested
        if deleteSource {
            modelContext.delete(sourceProject)
        }
        
        try? modelContext.save()
        dismiss()
    }
}