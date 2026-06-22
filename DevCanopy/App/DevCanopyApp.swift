import SwiftData
import SwiftUI

@main
struct DevCanopyApp: App {
    let modelContainer: ModelContainer
    @StateObject private var remoteHosts: RemoteHostsCoordinator
    @StateObject private var portfolioStore: PortfolioStore
    /// "This machine" metrics. Base type so demo mode can swap a
    /// `DemoHostMetricsService` in for the live `LocalHostMetricsService` without
    /// touching the panels (which read it as `HostMetricsService`).
    @StateObject private var localHostMetrics: HostMetricsService =
        DemoMode.isEnabled
            ? DemoHostMetricsService(hostName: "mac-demo", profile: .localMac, connectionState: .local)
            : LocalHostMetricsService()
    @StateObject private var containerService = LocalContainerService()
    @StateObject private var gitWorktreeService = GitWorktreeService()
    @StateObject private var ghRunnersService = GHRunnersService()
    @StateObject private var claudeUsageService = ClaudeUsageService()
    @StateObject private var portfolioCIService = GHWorkflowsService()
    /// OpenClaw agent-farm runtime + the coordinator the panel reads. The
    /// coordinator wraps one runtime today; adding hermes is a second runtime in
    /// the array. The WS reconnects itself, so it does not ride `refreshInterval`.
    @StateObject private var openclawService: OpenClawService
    @StateObject private var agentRuntimes: AgentRuntimeCoordinator

    /// User-selected cadence (seconds) for the periodic data-fetch services.
    /// Single settings mechanism: `AppStorage`/`UserDefaults` under
    /// `refreshInterval`. Changes restart those services live.
    @AppStorage("refreshInterval") private var refreshIntervalRaw: Int = RefreshInterval.default.rawValue

    private var refreshInterval: TimeInterval {
        TimeInterval(RefreshInterval(rawValue: refreshIntervalRaw)?.rawValue ?? RefreshInterval.default.rawValue)
    }

    init() {
        appLogger.info("DevCanopy starting...")

        // Initialize optional crash/error reporting early (issue #18). Complete
        // no-op unless a SENTRY_DSN is configured (env var / Info.plist), so a
        // normal local launch is entirely unaffected — no telemetry by default.
        SentrySetup.start()
        appLogger.debug("Console logging: \(ProcessInfo.processInfo.environment["DEVCANOPY_LOG_CONSOLE"] ?? "disabled")")
        appLogger.debug("Log level: \(ProcessInfo.processInfo.environment["DEVCANOPY_LOG_LEVEL"] ?? "info")")

        do {
            let schema = Schema([
                MonitoredHost.self,
                TrackedRepo.self
            ])

            let modelConfiguration = ModelConfiguration(
                schema: schema,
                isStoredInMemoryOnly: false
            )

            let container = try ModelContainer(
                for: schema,
                configurations: [modelConfiguration]
            )

            modelContainer = container

            // Coordinator for remote-host agents (reads MonitoredHost from SwiftData)
            _remoteHosts = StateObject(
                wrappedValue: RemoteHostsCoordinator(modelContext: container.mainContext)
            )

            // Editable portfolio repo set (reads TrackedRepo from SwiftData, seeds
            // once). Drives the GitHub Workflows, GitHub Runners, and Git/Worktrees panels.
            _portfolioStore = StateObject(
                wrappedValue: PortfolioStore(modelContext: container.mainContext)
            )

            // OpenClaw runtime: URL from AppStorage-backed UserDefaults, token
            // from the Keychain — both read at connect time so a Settings change
            // (applied via restart) picks up the new value.
            let openclaw = OpenClawService(
                urlProvider: { UserDefaults.standard.string(forKey: "openclawGatewayURL") },
                tokenProvider: { KeychainHelper.shared.loadOpenClawToken() }
            )
            _openclawService = StateObject(wrappedValue: openclaw)
            _agentRuntimes = StateObject(wrappedValue: AgentRuntimeCoordinator(runtimes: [openclaw]))

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
                .environmentObject(ghRunnersService)
                .environmentObject(remoteHosts)
                .environmentObject(portfolioStore)
                .environmentObject(agentRuntimes)
                .task {
                    let interval = refreshInterval

                    // Feed the editable portfolio set into the panels that key off
                    // it, and refresh them immediately when the set changes.
                    portfolioCIService.slugsProvider = { [weak portfolioStore] in
                        portfolioStore?.slugs ?? PortfolioRepos.seedSlugs
                    }
                    portfolioCIService.watchedProvider = { [weak portfolioStore] in
                        portfolioStore?.watchedWorkflows ?? [:]
                    }
                    gitWorktreeService.slugsProvider = { [weak portfolioStore] in
                        portfolioStore?.slugs ?? PortfolioRepos.seedSlugs
                    }
                    portfolioStore.onChange = { [weak portfolioCIService, weak gitWorktreeService] in
                        Task { await portfolioCIService?.refresh() }
                        Task { await gitWorktreeService?.refresh() }
                    }

                    localHostMetrics.start()
                    containerService.start()
                    gitWorktreeService.start(interval: interval)
                    claudeUsageService.start(interval: interval)
                    WorkflowNotificationService.shared.requestAuthorizationIfNeeded()
                    portfolioCIService.start(interval: interval)
                    ghRunnersService.start(interval: interval)
                    // Self-reconnecting WebSocket; not on the poll cadence.
                    agentRuntimes.start()
                    if DemoMode.isEnabled {
                        // Inject a synthetic remote host + demo containers; skip env
                        // seeding and real-agent reload entirely.
                        remoteHosts.loadDemo()
                    } else {
                        remoteHosts.seedFromEnvironmentIfNeeded()
                        remoteHosts.reload()
                    }
                }
                .onChange(of: refreshIntervalRaw) {
                    let interval = refreshInterval
                    gitWorktreeService.restart(interval: interval)
                    claudeUsageService.restart(interval: interval)
                    portfolioCIService.restart(interval: interval)
                    ghRunnersService.restart(interval: interval)
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
                .environmentObject(portfolioStore)
                .environmentObject(openclawService)
        }
        .modelContainer(modelContainer)
    }
}
