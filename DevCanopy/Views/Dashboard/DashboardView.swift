import SwiftUI
import SwiftData

struct DashboardView: View {
    @State private var useGroupedView = true
    
    var body: some View {
        if useGroupedView {
            GroupedDashboardView()
                .toolbar {
                    ToolbarItem(placement: .automatic) {
                        Button(action: { useGroupedView.toggle() }) {
                            Label("Toggle View", systemImage: useGroupedView ? "square.grid.2x2" : "list.bullet.indent")
                        }
                        .help(useGroupedView ? "Switch to flat view" : "Switch to grouped view")
                    }
                }
        } else {
            FlatDashboardView()
                .toolbar {
                    ToolbarItem(placement: .automatic) {
                        Button(action: { useGroupedView.toggle() }) {
                            Label("Toggle View", systemImage: useGroupedView ? "square.grid.2x2" : "list.bullet.indent")
                        }
                        .help(useGroupedView ? "Switch to flat view" : "Switch to grouped view")
                    }
                }
        }
    }
}

struct FlatDashboardView: View {
    @Query private var repositories: [TrackedRepository]
    @Query private var services: [ServiceConnection]
    @State private var refreshTimer: Timer?
    
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                // Header
                HStack {
                    Text("Dashboard")
                        .font(.largeTitle)
                        .bold()
                    
                    Spacer()
                    
                    Button(action: refreshAll) {
                        Label("Refresh", systemImage: "arrow.clockwise")
                    }
                    .buttonStyle(.plain)
                }
                .padding(.horizontal)
                
                // Status Summary
                StatusSummaryView(repositories: repositories, services: services)
                    .padding(.horizontal)
                
                // Repository Status Grid
                if !repositories.isEmpty {
                    VStack(alignment: .leading, spacing: 12) {
                        Text("Repositories")
                            .font(.headline)
                            .padding(.horizontal)
                        
                        LazyVGrid(columns: [
                            GridItem(.adaptive(minimum: 300, maximum: 400), spacing: 16)
                        ], spacing: 16) {
                            ForEach(repositories) { repo in
                                RepositoryCard(repository: repo)
                            }
                        }
                        .padding(.horizontal)
                    }
                }
                
                // Service Status
                if !services.isEmpty {
                    VStack(alignment: .leading, spacing: 12) {
                        Text("Services")
                            .font(.headline)
                            .padding(.horizontal)
                        
                        LazyVGrid(columns: [
                            GridItem(.adaptive(minimum: 200, maximum: 300), spacing: 16)
                        ], spacing: 16) {
                            ForEach(services) { service in
                                ServiceCard(service: service)
                            }
                        }
                        .padding(.horizontal)
                    }
                }
                
                // Empty State
                if repositories.isEmpty && services.isEmpty {
                    EmptyDashboardView()
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .padding(.vertical)
        }
        .background(Color(NSColor.windowBackgroundColor))
        .onAppear {
            startRefreshTimer()
        }
        .onDisappear {
            stopRefreshTimer()
        }
    }
    
    private func refreshAll() {
        uiLogger.info("Manual refresh requested")
        if repositories.isEmpty {
            uiLogger.warning("No repositories to refresh")
        }
        // Implement refresh logic
    }
    
    private func startRefreshTimer() {
        // Implement timer based on settings
    }
    
    private func stopRefreshTimer() {
        refreshTimer?.invalidate()
        refreshTimer = nil
    }
}

struct StatusSummaryView: View {
    let repositories: [TrackedRepository]
    let services: [ServiceConnection]
    
    private var repositoryStatusCounts: [RepositoryStatus: Int] {
        var counts: [RepositoryStatus: Int] = [:]
        for repo in repositories {
            counts[repo.displayStatus, default: 0] += 1
        }
        return counts
    }
    
    var body: some View {
        HStack(spacing: 20) {
            // Repository Summary
            VStack(alignment: .leading) {
                Text("Repositories")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text("\(repositories.count)")
                    .font(.title2)
                    .bold()
            }
            
            Divider()
                .frame(height: 40)
            
            // Status Breakdown
            ForEach(Array(repositoryStatusCounts.keys.sorted(by: { $0.description < $1.description })), id: \.self) { status in
                if let count = repositoryStatusCounts[status], count > 0 {
                    HStack(spacing: 4) {
                        Image(systemName: status.icon)
                            .foregroundColor(Color(status.color))
                        Text("\(count)")
                            .font(.title3)
                    }
                }
            }
            
            Divider()
                .frame(height: 40)
            
            // Services Summary
            VStack(alignment: .leading) {
                Text("Services")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text("\(services.count)")
                    .font(.title2)
                    .bold()
            }
            
            Spacer()
        }
        .padding()
        .background(Color(NSColor.controlBackgroundColor))
        .cornerRadius(8)
    }
}

struct EmptyDashboardView: View {
    var body: some View {
        VStack(spacing: 20) {
            Image(systemName: "square.dashed")
                .font(.system(size: 60))
                .foregroundStyle(.secondary)
            
            Text("No repositories or services configured")
                .font(.title2)
            
            Text("Add repositories to track or connect services to get started")
                .foregroundStyle(.secondary)
            
            HStack(spacing: 16) {
                Button("Add Repository") {
                    // Show add repository sheet
                }
                .buttonStyle(.borderedProminent)
                
                Button("Connect Service") {
                    // Show connect service sheet
                }
                .buttonStyle(.bordered)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}