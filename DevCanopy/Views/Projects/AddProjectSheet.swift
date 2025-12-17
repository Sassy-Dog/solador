import SwiftUI
import SwiftData

struct AddProjectSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.modelContext) private var modelContext
    
    let parentWorkspace: Workspace?
    let parentProject: Project?
    
    @State private var projectName = ""
    @State private var projectDescription = ""
    @State private var projectType: ProjectType = .application
    @State private var selectedColor = "blue"
    @State private var selectedIcon: String?
    @State private var errorMessage: String?
    
    private let availableColors = ["blue", "green", "orange", "red", "purple", "indigo", "teal", "pink", "gray"]
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text("New Project")
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
                Section {
                    TextField("Project Name", text: $projectName)
                        .textFieldStyle(.roundedBorder)
                    
                    TextField("Description (optional)", text: $projectDescription, axis: .vertical)
                        .textFieldStyle(.roundedBorder)
                        .lineLimit(3...5)
                }
                
                Section("Project Type") {
                    Picker("Type", selection: $projectType) {
                        ForEach(ProjectType.allCases, id: \.self) { type in
                            HStack {
                                Image(systemName: iconForProjectType(type))
                                Text(type.displayName)
                            }
                            .tag(type)
                        }
                    }
                    .pickerStyle(.radioGroup)
                }
                
                Section("Appearance") {
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
                
                if let workspace = parentWorkspace {
                    Section("Organization") {
                        HStack {
                            Image(systemName: workspace.icon ?? "building.2")
                            Text("Part of \(workspace.name)")
                                .foregroundColor(.secondary)
                        }
                    }
                } else if let project = parentProject {
                    Section("Organization") {
                        HStack {
                            Image(systemName: project.icon ?? "folder")
                            Text("Subproject of \(project.name)")
                                .foregroundColor(.secondary)
                        }
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
                Spacer()
                
                Button("Create") {
                    createProject()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(projectName.isEmpty)
            }
            .padding()
        }
        .frame(width: 500, height: 500)
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
    
    private func createProject() {
        let project = Project(
            name: projectName,
            type: projectType,
            description: projectDescription.isEmpty ? nil : projectDescription
        )
        
        project.color = selectedColor
        project.workspace = parentWorkspace
        project.parentProject = parentProject
        
        modelContext.insert(project)
        
        do {
            try modelContext.save()
            dismiss()
        } catch {
            errorMessage = "Failed to create project: \(error.localizedDescription)"
        }
    }
}

struct AddWorkspaceSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.modelContext) private var modelContext
    
    @State private var workspaceName = ""
    @State private var workspaceDescription = ""
    @State private var selectedColor = "blue"
    @State private var selectedIcon = "building.2"
    @State private var errorMessage: String?
    
    private let availableColors = ["blue", "green", "orange", "red", "purple", "indigo", "teal", "pink", "gray"]
    private let availableIcons = [
        "building.2", "building", "house", "globe", "network", "server.rack",
        "externaldrive.connected.to.line.below", "cloud", "star"
    ]
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text("New Workspace")
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
                Section {
                    TextField("Workspace Name", text: $workspaceName)
                        .textFieldStyle(.roundedBorder)
                    
                    TextField("Description (optional)", text: $workspaceDescription, axis: .vertical)
                        .textFieldStyle(.roundedBorder)
                        .lineLimit(3...5)
                }
                
                Section("Appearance") {
                    VStack(alignment: .leading, spacing: 12) {
                        HStack {
                            Text("Icon")
                            Spacer()
                            HStack(spacing: 12) {
                                ForEach(availableIcons, id: \.self) { icon in
                                    Image(systemName: icon)
                                        .font(.title3)
                                        .foregroundColor(.accentColor)
                                        .frame(width: 30, height: 30)
                                        .background(Color.accentColor.opacity(0.1))
                                        .cornerRadius(6)
                                        .overlay(
                                            RoundedRectangle(cornerRadius: 6)
                                                .stroke(Color.accentColor, lineWidth: selectedIcon == icon ? 2 : 0)
                                        )
                                        .onTapGesture {
                                            selectedIcon = icon
                                        }
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
                Text("Workspaces are top-level containers for organizing large platforms or organizations")
                    .font(.caption)
                    .foregroundColor(.secondary)
                
                Spacer()
                
                Button("Create") {
                    createWorkspace()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(workspaceName.isEmpty)
            }
            .padding()
        }
        .frame(width: 500, height: 400)
    }
    
    private func createWorkspace() {
        let workspace = Workspace(
            name: workspaceName,
            description: workspaceDescription.isEmpty ? nil : workspaceDescription,
            color: selectedColor,
            icon: selectedIcon
        )
        
        modelContext.insert(workspace)
        
        do {
            try modelContext.save()
            dismiss()
        } catch {
            errorMessage = "Failed to create workspace: \(error.localizedDescription)"
        }
    }
}