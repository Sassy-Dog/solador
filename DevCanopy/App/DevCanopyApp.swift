import SwiftUI
import SwiftData

@main
struct DevCanopyApp: App {
    let modelContainer: ModelContainer
    @StateObject private var remoteHosts: RemoteHostsCoordinator
    @StateObject private var localHostMetrics = LocalHostMetricsService()
    @StateObject private var containerService = LocalContainerService()
    @StateObject private var gitWorktreeService = GitWorktreeService()
    @StateObject private var ciRunnersService = CIRunnersService()
    @StateObject private var claudeUsageService = ClaudeUsageService()
    @StateObject private var portfolioCIService = PortfolioCIService()
    
    init() {
        appLogger.info("DevCanopy starting...")
        appLogger.debug("Console logging: \(ProcessInfo.processInfo.environment["DEVCANOPY_LOG_CONSOLE"] ?? "disabled")")
        appLogger.debug("Log level: \(ProcessInfo.processInfo.environment["DEVCANOPY_LOG_LEVEL"] ?? "info")")
        
        do {
            let schema = Schema([
                AppSettings.self,
                MonitoredHost.self
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

            // Coordinator for remote-host agents (reads MonitoredHost from SwiftData)
            self._remoteHosts = StateObject(
                wrappedValue: RemoteHostsCoordinator(modelContext: container.mainContext)
            )

            appLogger.info("ModelContainer and services initialized successfully")
        } catch {
            appLogger.error("Failed to create ModelContainer: \(error)")
            fatalError("Failed to create ModelContainer: \(error)")
        }
    }
    
    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(GitHubService.shared)
                .environmentObject(localHostMetrics)
                .environmentObject(containerService)
                .environmentObject(gitWorktreeService)
                .environmentObject(claudeUsageService)
                .environmentObject(portfolioCIService)
                .environmentObject(ciRunnersService)
                .environmentObject(remoteHosts)
                .task {
                    localHostMetrics.start()
                    containerService.start()
                    gitWorktreeService.start()
                    claudeUsageService.start()
                    portfolioCIService.start()
                    ciRunnersService.start()
                    remoteHosts.seedFromEnvironmentIfNeeded()
                    remoteHosts.reload()
                }
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
                .environmentObject(remoteHosts)
                .environmentObject(localHostMetrics)
        }
        .modelContainer(modelContainer)
    }
}