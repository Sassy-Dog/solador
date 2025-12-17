import SwiftUI
import SwiftData

struct WorkflowStatusView: View {
    let repository: TrackedRepository
    @State private var showAllWorkflows = false
    
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            // Summary view
            if repository.activeWorkflows.count > 2 && !showAllWorkflows {
                HStack(spacing: 12) {
                    WorkflowSummaryBadge(repository: repository)
                    
                    Button(action: { showAllWorkflows.toggle() }) {
                        Text("Show all \(repository.activeWorkflows.count) workflows")
                            .font(.caption)
                            .foregroundColor(.accentColor)
                    }
                    .buttonStyle(.plain)
                }
            } else {
                // Individual workflow badges
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(displayedWorkflows) { workflow in
                        WorkflowBadge(workflow: workflow, repository: repository)
                    }
                    
                    if repository.activeWorkflows.count > 2 && showAllWorkflows {
                        Button(action: { showAllWorkflows.toggle() }) {
                            Text("Show less")
                                .font(.caption)
                                .foregroundColor(.accentColor)
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
        }
    }
    
    private var displayedWorkflows: [GitHubWorkflow] {
        let workflows = repository.activeWorkflows.sorted { $0.name < $1.name }
        
        if showAllWorkflows {
            return workflows
        } else {
            // Show priority workflows: failing ones first, then release workflows
            let failing = workflows.filter { $0.latestRun?.isFailure ?? false }
            let release = workflows.filter { $0.isReleaseWorkflow && !failing.contains($0) }
            let others = workflows.filter { !failing.contains($0) && !release.contains($0) }
            
            return Array((failing + release + others).prefix(2))
        }
    }
}

struct WorkflowSummaryBadge: View {
    let repository: TrackedRepository
    
    var body: some View {
        HStack(spacing: 4) {
            let summary = repository.workflowSummary
            
            if summary.failure > 0 {
                Image(systemName: "xmark.circle.fill")
                    .foregroundColor(.red)
                Text("\(summary.failure)")
                    .foregroundColor(.red)
                    .font(.caption)
            }
            
            if summary.running > 0 {
                Image(systemName: "arrow.triangle.2.circlepath")
                    .foregroundColor(.orange)
                Text("\(summary.running)")
                    .foregroundColor(.orange)
                    .font(.caption)
            }
            
            if summary.success > 0 {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundColor(.green)
                Text("\(summary.success)")
                    .foregroundColor(.green)
                    .font(.caption)
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 2)
        .background(Color(NSColor.controlBackgroundColor))
        .cornerRadius(4)
    }
}

struct WorkflowBadge: View {
    let workflow: GitHubWorkflow
    let repository: TrackedRepository
    @State private var showDetails = false
    
    var body: some View {
        HStack(spacing: 6) {
            // Workflow status icon
            if let latestRun = workflow.latestRun {
                Image(systemName: latestRun.statusIcon)
                    .foregroundColor(Color(latestRun.statusColor))
                    .font(.caption)
            } else {
                Image(systemName: "questionmark.circle")
                    .foregroundColor(.gray)
                    .font(.caption)
            }
            
            // Workflow name
            Text(workflow.name)
                .font(.caption)
                .lineLimit(1)
                .truncationMode(.tail)
            
            // Release tag if available and it's a release workflow
            if workflow.isReleaseWorkflow,
               let latestRun = workflow.latestRun,
               let releaseTag = latestRun.releaseTag {
                Text(releaseTag)
                    .font(.caption2)
                    .fontWeight(.medium)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 1)
                    .background(Color.accentColor.opacity(0.2))
                    .cornerRadius(3)
            }
            
            // Duration if completed
            if let latestRun = workflow.latestRun,
               let duration = latestRun.durationString,
               workflow.displayOptions.badgeStyle == .detailed {
                Text(duration)
                    .font(.caption2)
                    .foregroundColor(.secondary)
            }
            
            Spacer()
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(backgroundColorFor(workflow))
        .cornerRadius(4)
        .onTapGesture {
            if let latestRun = workflow.latestRun,
               let url = URL(string: latestRun.htmlUrl) {
                NSWorkspace.shared.open(url)
            }
        }
        .contextMenu {
            if let latestRun = workflow.latestRun {
                Button("View Run on GitHub") {
                    if let url = URL(string: latestRun.htmlUrl) {
                        NSWorkspace.shared.open(url)
                    }
                }
                
                Button("Copy Run URL") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(latestRun.htmlUrl, forType: .string)
                }
                
                Divider()
                
                Text("Run #\(latestRun.runNumber)")
                    .font(.caption)
                
                if let duration = latestRun.durationString {
                    Text("Duration: \(duration)")
                        .font(.caption)
                }
                
                if let actor = latestRun.actor {
                    Text("By: \(actor)")
                        .font(.caption)
                }
            }
            
            Divider()
            
            Button("Configure Workflow") {
                showDetails = true
            }
        }
        .sheet(isPresented: $showDetails) {
            WorkflowConfigurationSheet(repository: repository)
        }
    }
    
    private func backgroundColorFor(_ workflow: GitHubWorkflow) -> Color {
        guard let latestRun = workflow.latestRun else {
            return Color(NSColor.controlBackgroundColor)
        }
        
        if latestRun.isInProgress {
            return Color.orange.opacity(0.1)
        } else if latestRun.isSuccess {
            return Color.green.opacity(0.1)
        } else if latestRun.isFailure {
            return Color.red.opacity(0.1)
        } else {
            return Color(NSColor.controlBackgroundColor)
        }
    }
}

