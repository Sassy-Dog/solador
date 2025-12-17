import SwiftUI
import SwiftData

struct WorkflowConfigurationSheet: View {
    let repository: TrackedRepository
    @Environment(\.dismiss) private var dismiss
    @Environment(\.modelContext) private var modelContext
    @EnvironmentObject private var gitMonitorService: GitMonitorService
    
    @State private var showOnlyEnabled = false
    @State private var refreshing = false
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                VStack(alignment: .leading) {
                    Text("Configure GitHub Workflows")
                        .font(.title2)
                        .bold()
                    Text(repository.githubRepoIdentifier ?? repository.name)
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
                
                Spacer()
                
                Button("Done") {
                    dismiss()
                }
            }
            .padding()
            
            Divider()
            
            // Toolbar
            HStack {
                Toggle("Show only enabled", isOn: $showOnlyEnabled)
                
                Spacer()
                
                Button(action: { toggleAll(enabled: true) }) {
                    Text("Enable All")
                }
                .buttonStyle(.link)
                
                Button(action: { toggleAll(enabled: false) }) {
                    Text("Disable All")
                }
                .buttonStyle(.link)
                
                Button(action: { refreshWorkflows() }) {
                    if refreshing {
                        ProgressView()
                            .scaleEffect(0.7)
                    } else {
                        Label("Refresh", systemImage: "arrow.clockwise")
                    }
                }
                .disabled(refreshing)
            }
            .padding()
            
            Divider()
            
            // Workflow List
            if let workflows = repository.githubWorkflows, !workflows.isEmpty {
                List {
                    ForEach(filteredWorkflows) { workflow in
                        WorkflowConfigRow(workflow: workflow)
                    }
                }
                .listStyle(.inset)
            } else {
                VStack(spacing: 16) {
                    Image(systemName: "arrow.triangle.2.circlepath")
                        .font(.largeTitle)
                        .foregroundColor(.secondary)
                    
                    Text("No workflows found")
                        .font(.headline)
                        .foregroundColor(.secondary)
                    
                    Button("Fetch Workflows") {
                        refreshWorkflows()
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            
            Divider()
            
            // Footer with summary
            HStack {
                if let workflows = repository.githubWorkflows {
                    let enabledCount = workflows.filter { $0.isEnabled }.count
                    Text("\(enabledCount) of \(workflows.count) workflows enabled")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
                
                Spacer()
            }
            .padding()
        }
        .frame(width: 700, height: 600)
    }
    
    private var filteredWorkflows: [GitHubWorkflow] {
        guard let workflows = repository.githubWorkflows else { return [] }
        
        if showOnlyEnabled {
            return workflows.filter { $0.isEnabled }.sorted { $0.name < $1.name }
        } else {
            return workflows.sorted { $0.name < $1.name }
        }
    }
    
    private func toggleAll(enabled: Bool) {
        guard let workflows = repository.githubWorkflows else { return }
        
        for workflow in workflows {
            workflow.isEnabled = enabled
        }
        
        try? modelContext.save()
    }
    
    private func refreshWorkflows() {
        refreshing = true
        Task {
            await gitMonitorService.refreshWorkflows(for: repository)
            refreshing = false
        }
    }
}

struct WorkflowConfigRow: View {
    @Bindable var workflow: GitHubWorkflow
    @State private var showingOptions = false
    
    var body: some View {
        VStack(spacing: 8) {
            HStack {
                // Enable toggle
                Toggle(isOn: $workflow.isEnabled) {
                    VStack(alignment: .leading, spacing: 4) {
                        HStack {
                            Text(workflow.name)
                                .font(.headline)
                            
                            if workflow.isReleaseWorkflow {
                                Badge(text: "Release", color: .blue)
                            }
                            
                            if workflow.isCIWorkflow {
                                Badge(text: "CI", color: .green)
                            }
                        }
                        
                        Text(workflow.path)
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                }
                
                Spacer()
                
                // Latest run status
                if let latestRun = workflow.latestRun {
                    VStack(alignment: .trailing, spacing: 2) {
                        HStack(spacing: 4) {
                            Image(systemName: latestRun.statusIcon)
                                .foregroundColor(Color(latestRun.statusColor))
                            Text("#\(latestRun.runNumber)")
                                .font(.caption)
                                .foregroundColor(.secondary)
                        }
                        
                        if let duration = latestRun.durationString {
                            Text(duration)
                                .font(.caption2)
                                .foregroundColor(.secondary)
                        }
                    }
                }
                
                // Options button
                Button(action: { showingOptions = true }) {
                    Image(systemName: "gear")
                        .foregroundColor(.secondary)
                }
                .buttonStyle(.plain)
                .popover(isPresented: $showingOptions) {
                    WorkflowOptionsPopover(workflow: workflow)
                }
            }
            .padding(.vertical, 4)
            
            // Display options preview
            if workflow.isEnabled {
                HStack(spacing: 12) {
                    Label("Dashboard", systemImage: workflow.displayOptions.showInDashboard ? "checkmark.circle.fill" : "circle")
                        .font(.caption)
                        .foregroundColor(workflow.displayOptions.showInDashboard ? .green : .secondary)
                    
                    Label("Card", systemImage: workflow.displayOptions.showInRepositoryCard ? "checkmark.circle.fill" : "circle")
                        .font(.caption)
                        .foregroundColor(workflow.displayOptions.showInRepositoryCard ? .green : .secondary)
                    
                    Text("Style: \(workflow.displayOptions.badgeStyle.rawValue)")
                        .font(.caption)
                        .foregroundColor(.secondary)
                    
                    if workflow.isReleaseWorkflow && workflow.displayOptions.prominentForRelease {
                        Label("Prominent", systemImage: "star.fill")
                            .font(.caption)
                            .foregroundColor(.orange)
                    }
                    
                    Spacer()
                }
                .padding(.leading, 28)
            }
        }
    }
}

struct WorkflowOptionsPopover: View {
    @Bindable var workflow: GitHubWorkflow
    @Environment(\.modelContext) private var modelContext
    
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Display Options for \(workflow.name)")
                .font(.headline)
                .padding(.bottom, 8)
            
            VStack(alignment: .leading, spacing: 12) {
                Toggle("Show in Dashboard", isOn: $workflow.displayOptions.showInDashboard)
                    .help("Display this workflow's status in the main dashboard view")
                
                Toggle("Show in Repository Card", isOn: $workflow.displayOptions.showInRepositoryCard)
                    .help("Display this workflow's status in the repository card")
                
                Divider()
                
                Picker("Badge Style", selection: $workflow.displayOptions.badgeStyle) {
                    Text("Icon Only").tag(WorkflowBadgeStyle.iconOnly)
                    Text("Text Only").tag(WorkflowBadgeStyle.textOnly)
                    Text("Icon and Text").tag(WorkflowBadgeStyle.iconAndText)
                    Text("Detailed").tag(WorkflowBadgeStyle.detailed)
                }
                .pickerStyle(.radioGroup)
                
                Divider()
                
                if workflow.isReleaseWorkflow {
                    Toggle("Show prominently for releases", isOn: $workflow.displayOptions.prominentForRelease)
                        .help("Display release version/tag prominently")
                }
                
                Divider()
                
                Text("Notifications")
                    .font(.subheadline)
                    .foregroundColor(.secondary)
                
                Toggle("Notify on failure", isOn: $workflow.displayOptions.notifyOnFailure)
                Toggle("Notify on success", isOn: $workflow.displayOptions.notifyOnSuccess)
            }
            .padding()
            .onChange(of: workflow.displayOptions) { _, _ in
                try? modelContext.save()
            }
        }
        .frame(width: 350)
        .padding()
    }
}

struct Badge: View {
    let text: String
    let color: Color
    
    var body: some View {
        Text(text)
            .font(.caption2)
            .fontWeight(.medium)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(color.opacity(0.2))
            .foregroundColor(color)
            .cornerRadius(4)
    }
}