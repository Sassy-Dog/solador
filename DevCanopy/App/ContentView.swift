import SwiftUI
import SwiftData

struct ContentView: View {
    @Environment(\.modelContext) private var modelContext
    @Query private var repositories: [TrackedRepository]
    @Query private var services: [ServiceConnection]
    @State private var selectedSection: SidebarSection = .overview
    
    var body: some View {
        NavigationSplitView {
            SidebarView(selection: $selectedSection)
        } detail: {
            switch selectedSection {
            case .overview:
                DashboardView()
            case .projects:
                ProjectsView()
            case .repositories:
                RepositoriesView()
            case .services:
                ServicesView()
            }
        }
        .navigationTitle("")
        .toolbar {
            ToolbarItem(placement: .navigation) {
                Button(action: toggleSidebar) {
                    Image(systemName: "sidebar.leading")
                }
                .help("Toggle Sidebar")
            }
        }
        .onAppear {
            setupInitialData()
        }
    }
    
    private func toggleSidebar() {
        NSApp.keyWindow?.firstResponder?.tryToPerform(
            #selector(NSSplitViewController.toggleSidebar(_:)),
            with: nil
        )
    }
    
    private func setupInitialData() {
        // Check if this is first launch
        if repositories.isEmpty && services.isEmpty {
            uiLogger.info("First launch detected - no repositories or services configured")
            // Show onboarding in future
        } else {
            uiLogger.debug("Found \(repositories.count) repositories and \(services.count) services")
        }
    }
}

enum SidebarSection: String, CaseIterable {
    case overview = "Overview"
    case projects = "Projects"
    case repositories = "Repositories"
    case services = "Services"
    
    var icon: String {
        switch self {
        case .overview:
            return "square.grid.2x2"
        case .projects:
            return "square.stack.3d.up"
        case .repositories:
            return "folder"
        case .services:
            return "cloud"
        }
    }
}

struct SidebarView: View {
    @Binding var selection: SidebarSection
    
    var body: some View {
        List(SidebarSection.allCases, id: \.self, selection: $selection) { section in
            Label(section.rawValue, systemImage: section.icon)
        }
        .listStyle(SidebarListStyle())
        .navigationTitle("DevCanopy")
        .frame(minWidth: 200)
    }
}

#Preview {
    ContentView()
        .modelContainer(for: TrackedRepository.self, inMemory: true)
}