import SwiftUI
import SwiftData

struct RepositoriesView: View {
    @Query private var repositories: [TrackedRepository]
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
    
    var body: some View {
        VStack {
            Text("Add Repository")
                .font(.title2)
                .padding()
            
            Text("Drag and drop a folder or click to browse")
                .foregroundStyle(.secondary)
            
            // Implementation will include drag/drop and file browser
            
            HStack {
                Button("Cancel") {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)
                
                Button("Add") {
                    // Add repository
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(true) // Until path is selected
            }
            .padding()
        }
        .frame(width: 400, height: 300)
    }
}