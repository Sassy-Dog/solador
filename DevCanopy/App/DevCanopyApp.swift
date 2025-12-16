import SwiftUI
import SwiftData

@main
struct DevCanopyApp: App {
    let modelContainer: ModelContainer
    
    init() {
        appLogger.info("DevCanopy starting...")
        appLogger.debug("Console logging: \(ProcessInfo.processInfo.environment["DEVCANOPY_LOG_CONSOLE"] ?? "disabled")")
        appLogger.debug("Log level: \(ProcessInfo.processInfo.environment["DEVCANOPY_LOG_LEVEL"] ?? "info")")
        
        do {
            let schema = Schema([
                TrackedRepository.self,
                ServiceConnection.self,
                AppSettings.self
            ])
            
            let modelConfiguration = ModelConfiguration(
                schema: schema,
                isStoredInMemoryOnly: false
            )
            
            modelContainer = try ModelContainer(
                for: schema,
                configurations: [modelConfiguration]
            )
            appLogger.info("ModelContainer initialized successfully")
        } catch {
            appLogger.error("Failed to create ModelContainer: \(error)")
            fatalError("Failed to create ModelContainer: \(error)")
        }
    }
    
    var body: some Scene {
        WindowGroup {
            ContentView()
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
        }
        .modelContainer(modelContainer)
    }
}