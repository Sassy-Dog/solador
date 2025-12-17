import SwiftUI

struct RepositoryCard: View {
    let repository: TrackedRepository
    @EnvironmentObject private var gitMonitorService: GitMonitorService
    @State private var isHovered = false
    @State private var isRefreshing = false
    @State private var showWorkflowConfiguration = false
    
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            // Header
            HStack {
                Image(systemName: "folder.fill")
                    .foregroundColor(.accentColor)
                
                VStack(alignment: .leading, spacing: 2) {
                    Text(repository.name)
                        .font(.headline)
                        .lineLimit(1)
                    
                    Text(repository.path)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .help(repository.path)
                }
                
                Spacer()
                
                // Status Icon
                if isRefreshing {
                    ProgressView()
                        .scaleEffect(0.7)
                        .frame(width: 24, height: 24)
                } else {
                    Image(systemName: repository.displayStatus.icon)
                        .foregroundColor(Color(repository.displayStatus.color))
                        .font(.title2)
                }
            }
            
            Divider()
            
            // Git Status
            VStack(alignment: .leading, spacing: 8) {
                if let branch = repository.currentBranch {
                    HStack {
                        Image(systemName: "arrow.branch")
                            .foregroundStyle(.secondary)
                            .frame(width: 16)
                        Text(branch)
                            .font(.system(.body, design: .monospaced))
                    }
                }
                
                // Status Description
                HStack {
                    Image(systemName: "info.circle")
                        .foregroundStyle(.secondary)
                        .frame(width: 16)
                    
                    Text(statusDescription)
                        .foregroundStyle(statusTextColor)
                }
                
                // Service Links
                if repository.githubRepoIdentifier != nil || repository.vercelProjectId != nil {
                    HStack(spacing: 12) {
                        if repository.githubRepoIdentifier != nil {
                            Label("GitHub", systemImage: "link")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        
                        if repository.vercelProjectId != nil {
                            Label("Vercel", systemImage: "triangle")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                
                // GitHub Workflows
                if !repository.activeWorkflows.isEmpty {
                    WorkflowStatusView(repository: repository)
                }
            }
        }
        .padding()
        .background(Color(NSColor.controlBackgroundColor))
        .cornerRadius(8)
        .shadow(color: Color.black.opacity(isHovered ? 0.15 : 0.05), radius: isHovered ? 8 : 4)
        .scaleEffect(isHovered ? 1.02 : 1.0)
        .onHover { hovering in
            withAnimation(.easeInOut(duration: 0.2)) {
                isHovered = hovering
            }
        }
        .onTapGesture {
            openInTerminal()
        }
        .contextMenu {
            Button("Open in Terminal") {
                openInTerminal()
            }
            
            Button("Open in Finder") {
                openInFinder()
            }
            
            Divider()
            
            Button("Refresh") {
                Task {
                    isRefreshing = true
                    await gitMonitorService.refreshRepository(repository)
                    isRefreshing = false
                }
            }
            .disabled(isRefreshing)
            
            if repository.githubRepoIdentifier != nil {
                Button("Configure Workflows...") {
                    showWorkflowConfiguration = true
                }
                
                Button("Refresh Workflows") {
                    Task {
                        await gitMonitorService.refreshWorkflows(for: repository)
                    }
                }
            }
            
            Button("Remove") {
                // Remove repository
            }
        }
        .sheet(isPresented: $showWorkflowConfiguration) {
            WorkflowConfigurationSheet(repository: repository)
        }
    }
    
    private var statusDescription: String {
        switch repository.displayStatus {
        case .synced:
            return "In sync with \(repository.trackingBranch ?? "remote")"
        case .ahead:
            return "\(repository.aheadCount) commit\(repository.aheadCount == 1 ? "" : "s") ahead"
        case .behind:
            return "\(repository.behindCount) commit\(repository.behindCount == 1 ? "" : "s") behind"
        case .diverged:
            return "\(repository.aheadCount) ahead, \(repository.behindCount) behind"
        case .uncommitted:
            return "Uncommitted changes"
        case .error:
            return "Unable to check status"
        }
    }
    
    private var statusTextColor: Color {
        switch repository.displayStatus {
        case .synced:
            return .primary
        case .ahead, .behind:
            return .orange
        case .diverged, .uncommitted:
            return .orange
        case .error:
            return .red
        }
    }
    
    private func openInTerminal() {
        // Implement terminal opening logic
        NSWorkspace.shared.openTerminal(at: repository.path)
    }
    
    private func openInFinder() {
        NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: repository.path)
    }
}

extension NSWorkspace {
    func openTerminal(at path: String) {
        // This will be implemented with proper terminal detection
        let url = URL(fileURLWithPath: path)
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.activates = true
        
        // For now, use Terminal.app
        if let terminalURL = NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.Terminal") {
            NSWorkspace.shared.open([url], withApplicationAt: terminalURL, configuration: configuration)
        }
    }
}