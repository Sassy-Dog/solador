import SwiftUI
import SwiftData
import UniformTypeIdentifiers

struct RepositoriesView: View {
    @Query private var repositories: [TrackedRepository]
    @EnvironmentObject private var gitMonitorService: GitMonitorService
    @State private var showAddRepository = false
    
    var body: some View {
        VStack {
            // Header
            HStack {
                Text("Repositories")
                    .font(.largeTitle)
                    .bold()
                
                Spacer()
                
                Button(action: { showAddRepository = true }) {
                    Label("Add Repository", systemImage: "plus")
                }
            }
            .padding()
            
            // Repository List
            if repositories.isEmpty {
                ContentUnavailableView {
                    Label("No Repositories", systemImage: "folder")
                } description: {
                    Text("Add repositories to track their git status")
                } actions: {
                    Button("Add Repository") {
                        showAddRepository = true
                    }
                    .buttonStyle(.borderedProminent)
                }
            } else {
                List {
                    ForEach(repositories) { repository in
                        RepositoryListRow(repository: repository)
                    }
                }
                .listStyle(.inset)
            }
        }
        .sheet(isPresented: $showAddRepository) {
            AddRepositorySheet()
        }
    }
}

struct RepositoryListRow: View {
    let repository: TrackedRepository
    
    var body: some View {
        HStack {
            Image(systemName: repository.displayStatus.icon)
                .foregroundColor(Color(repository.displayStatus.color))
            
            VStack(alignment: .leading) {
                Text(repository.name)
                    .font(.headline)
                Text(repository.path)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            
            Spacer()
            
            if let branch = repository.currentBranch {
                Text(branch)
                    .font(.system(.body, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 4)
    }
}

struct AddRepositorySheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.modelContext) private var modelContext
    @EnvironmentObject private var gitMonitorService: GitMonitorService
    @Query private var workspaces: [Workspace]
    @Query private var projects: [Project]
    @State private var selectedPath: String = ""
    @State private var showFileImporter = false
    @State private var errorMessage: String?
    @State private var selectedWorkspace: Workspace?
    @State private var selectedProject: Project?
    
    var body: some View {
        VStack(spacing: 20) {
            Text("Add Repository")
                .font(.title2)
                .padding(.top)
            
            VStack(spacing: 12) {
                Text("Select a git repository folder")
                    .foregroundStyle(.secondary)
                
                if !selectedPath.isEmpty {
                    HStack {
                        Image(systemName: "folder.fill")
                            .foregroundColor(.accentColor)
                        Text(selectedPath)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .help(selectedPath)
                    }
                    .padding()
                    .background(Color(NSColor.controlBackgroundColor))
                    .cornerRadius(8)
                }
                
                Button("Browse...") {
                    showFileImporter = true
                }
                .buttonStyle(.borderedProminent)
                
                // Organization selection
                if !selectedPath.isEmpty {
                    Divider()
                    
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Organization (optional)")
                            .font(.headline)
                        
                        if !workspaces.isEmpty {
                            Picker("Workspace", selection: $selectedWorkspace) {
                                Text("None").tag(nil as Workspace?)
                                ForEach(workspaces) { workspace in
                                    Text(workspace.name).tag(workspace as Workspace?)
                                }
                            }
                        }
                        
                        if !projects.isEmpty {
                            Picker("Project", selection: $selectedProject) {
                                Text("None").tag(nil as Project?)
                                ForEach(projects.filter { project in
                                    // Show only root projects or projects in selected workspace
                                    if let workspace = selectedWorkspace {
                                        return project.workspace == workspace
                                    } else {
                                        return project.parentProject == nil && project.workspace == nil
                                    }
                                }) { project in
                                    Text(project.name).tag(project as Project?)
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
            
            Spacer()
            
            HStack {
                Button("Cancel") {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)
                
                Button("Add") {
                    addRepository()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(selectedPath.isEmpty)
            }
            .padding()
        }
        .frame(width: 400, height: 300)
        .fileImporter(
            isPresented: $showFileImporter,
            allowedContentTypes: [.folder],
            allowsMultipleSelection: false
        ) { result in
            switch result {
            case .success(let urls):
                if let url = urls.first {
                    selectedPath = url.path
                    errorMessage = nil
                    validateRepository(at: url)
                }
            case .failure(let error):
                errorMessage = error.localizedDescription
            }
        }
    }
    
    private func validateRepository(at url: URL) {
        let gitURL = url.appendingPathComponent(".git")
        if !FileManager.default.fileExists(atPath: gitURL.path) {
            errorMessage = "Selected folder is not a git repository"
            selectedPath = ""
        }
    }
    
    private func addRepository() {
        guard !selectedPath.isEmpty else { return }
        
        let name = URL(fileURLWithPath: selectedPath).lastPathComponent
        let repository = TrackedRepository(name: name, path: selectedPath)
        
        // Set organization
        repository.workspace = selectedWorkspace
        repository.project = selectedProject
        
        modelContext.insert(repository)
        
        do {
            try modelContext.save()
            
            // Start monitoring the repository
            gitMonitorService.startMonitoring(repository)
            
            dismiss()
        } catch {
            errorMessage = "Failed to add repository: \(error.localizedDescription)"
        }
    }
}