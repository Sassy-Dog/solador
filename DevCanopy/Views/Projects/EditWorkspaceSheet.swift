import SwiftUI
import SwiftData

struct EditWorkspaceSheet: View {
    let workspace: Workspace
    
    @Environment(\.dismiss) private var dismiss
    @Environment(\.modelContext) private var modelContext
    @Query private var allProjects: [Project]
    
    @State private var workspaceName: String = ""
    @State private var workspaceDescription: String = ""
    @State private var selectedColor: String = "blue"
    @State private var selectedIcon: String?
    @State private var errorMessage: String?
    
    private let availableColors = ["blue", "green", "orange", "red", "purple", "indigo", "teal", "pink", "gray"]
    private let availableIcons = [
        "building.2", "building", "building.columns", "building.2.crop.circle",
        "house", "house.fill", "storefront", "storefront.fill",
        "globe", "globe.americas.fill", "network", "server.rack"
    ]
    
    init(workspace: Workspace) {
        self.workspace = workspace
    }
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text("Edit Workspace")
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
                Section("Workspace Details") {
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
                
                Section("Statistics") {
                    HStack {
                        Text("Projects")
                            .foregroundColor(.secondary)
                        Spacer()
                        Text("\(workspace.projects?.count ?? 0)")
                    }
                    
                    HStack {
                        Text("Total Repositories")
                            .foregroundColor(.secondary)
                        Spacer()
                        Text("\(workspace.totalRepositoryCount)")
                    }
                    
                    HStack {
                        Text("Repositories Needing Attention")
                            .foregroundColor(.secondary)
                        Spacer()
                        Text("\(workspace.repositoriesNeedingAttention)")
                            .foregroundColor(workspace.repositoriesNeedingAttention > 0 ? .orange : .secondary)
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
                Text("Created \(workspace.createdAt, format: .relative(presentation: .named))")
                    .font(.caption)
                    .foregroundColor(.secondary)
                
                Spacer()
                
                Button("Save") {
                    saveChanges()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(workspaceName.isEmpty)
            }
            .padding()
        }
        .frame(width: 600, height: 650)
        .onAppear {
            loadWorkspaceData()
        }
    }
    
    private func loadWorkspaceData() {
        workspaceName = workspace.name
        workspaceDescription = workspace.workspaceDescription ?? ""
        selectedColor = workspace.color ?? "blue"
        selectedIcon = workspace.icon ?? "building.2"
    }
    
    private func saveChanges() {
        workspace.name = workspaceName
        workspace.workspaceDescription = workspaceDescription.isEmpty ? nil : workspaceDescription
        workspace.color = selectedColor
        workspace.icon = selectedIcon ?? "building.2"
        workspace.updatedAt = Date()
        
        do {
            try modelContext.save()
            dismiss()
        } catch {
            errorMessage = "Failed to save changes: \(error.localizedDescription)"
        }
    }
}

