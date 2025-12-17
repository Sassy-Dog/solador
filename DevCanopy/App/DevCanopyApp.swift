import SwiftUI
import SwiftData

@main
struct DevCanopyApp: App {
    let modelContainer: ModelContainer
    @StateObject private var gitMonitorService: GitMonitorService
    
    init() {
        appLogger.info("DevCanopy starting...")
        appLogger.debug("Console logging: \(ProcessInfo.processInfo.environment["DEVCANOPY_LOG_CONSOLE"] ?? "disabled")")
        appLogger.debug("Log level: \(ProcessInfo.processInfo.environment["DEVCANOPY_LOG_LEVEL"] ?? "info")")
        
        do {
            let schema = Schema([
                TrackedRepository.self,
                ServiceConnection.self,
                AppSettings.self,
                Workspace.self,
                Project.self,
                Tag.self,
                GitHubWorkflow.self,
                GitHubWorkflowRun.self
            ])
            
            let modelConfiguration = ModelConfiguration(
                schema: schema,
                isStoredInMemoryOnly: false
            )
            
            let container = try ModelContainer(
                for: schema,
                configurations: [modelConfiguration]
            )
            
            self.modelContainer = container
            
            // Initialize GitMonitorService
            let gitMonitor = GitMonitorService(modelContext: container.mainContext)
            self._gitMonitorService = StateObject(wrappedValue: gitMonitor)
            
            // Start monitoring existing repositories
            Task { @MainActor in
                let descriptor = FetchDescriptor<TrackedRepository>()
                if let repositories = try? container.mainContext.fetch(descriptor) {
                    for repository in repositories {
                        gitMonitor.startMonitoring(repository)
                    }
                    appLogger.info("Started monitoring \(repositories.count) existing repositories")
                }
            }
            
            appLogger.info("ModelContainer and services initialized successfully")
        } catch {
            appLogger.error("Failed to create ModelContainer: \(error)")
            fatalError("Failed to create ModelContainer: \(error)")
        }
    }
    
    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(gitMonitorService)
                .environmentObject(GitHubService.shared)
        }
        .modelContainer(modelContainer)
        .windowStyle(.automatic)
        .windowResizability(.contentSize)
        .defaultSize(width: 1200, height: 800)
        .commands {
            CommandGroup(replacing: .appInfo) {
                Button("About DevCanopy") {
                    NSApp.orderFrontStandardAboutPanel(
                        options: [
                            NSApplication.AboutPanelOptionKey.credits: NSAttributedString(
                                string: "Monitor your development infrastructure",
                                attributes: [
                                    NSAttributedString.Key.font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
                                ]
                            ),
                            NSApplication.AboutPanelOptionKey(rawValue: "Copyright"): "© 2024 Sassy Dog"
                        ]
                    )
                }
            }
        }
        
        Settings {
            SettingsView()
                .environmentObject(gitMonitorService)
        }
        .modelContainer(modelContainer)
    }
}