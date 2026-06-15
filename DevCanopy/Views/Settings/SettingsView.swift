import SwiftUI
import SwiftData

struct SettingsView: View {
    @Environment(\.modelContext) private var modelContext
    @Query private var settings: [AppSettings]
    
    private var appSettings: AppSettings? {
        settings.first
    }
    
    var body: some View {
        TabView {
            GeneralSettingsView()
                .tabItem {
                    Label("General", systemImage: "gear")
                }
            
            TerminalSettingsView()
                .tabItem {
                    Label("Terminal", systemImage: "terminal")
                }

            GitHubSettingsView()
                .tabItem {
                    Label("GitHub", systemImage: "checkmark.seal")
                }

            HostsSettingsView()
                .tabItem {
                    Label("Hosts", systemImage: "server.rack")
                }

            AboutView()
                .tabItem {
                    Label("About", systemImage: "info.circle")
                }
        }
        .frame(width: 500, height: 400)
    }
}

struct GeneralSettingsView: View {
    @AppStorage("refreshInterval") private var refreshInterval: Int = 60
    @AppStorage("launchAtLogin") private var launchAtLogin: Bool = false
    @AppStorage("showPerformanceOverlay") private var showPerformanceOverlay: Bool = false
    @AppStorage("coreRowSpan") private var coreRowSpan: Int = 2

    var body: some View {
        Form {
            Section {
                Picker("Refresh Interval", selection: $refreshInterval) {
                    ForEach(RefreshInterval.allCases, id: \.rawValue) { interval in
                        Text(interval.displayName).tag(interval.rawValue)
                    }
                }

                Toggle("Launch at Login", isOn: $launchAtLogin)

                Toggle("Show Performance Overlay", isOn: $showPerformanceOverlay)

                VStack(alignment: .leading, spacing: 2) {
                    Stepper("CPU core rows: \(coreRowSpan)", value: $coreRowSpan, in: 1...4)
                    Text("How many rows tall the per-core CPU grid is on every host card.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            } header: {
                Text("General Settings")
                    .font(.headline)
            }
        }
        .formStyle(.grouped)
        .padding()
    }
}

struct TerminalSettingsView: View {
    @AppStorage("preferredTerminal") private var preferredTerminal: String = "Terminal"
    @State private var availableTerminals: [(name: String, bundleId: String)] = []
    
    var body: some View {
        Form {
            Section {
                Picker("Preferred Terminal", selection: $preferredTerminal) {
                    ForEach(availableTerminals, id: \.bundleId) { terminal in
                        Text(terminal.name).tag(terminal.bundleId)
                    }
                }
                
                Text("Select your preferred terminal application for opening repositories")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } header: {
                Text("Terminal Settings")
                    .font(.headline)
            }
        }
        .formStyle(.grouped)
        .padding()
        .onAppear {
            detectAvailableTerminals()
        }
    }
    
    private func detectAvailableTerminals() {
        let terminals = [
            ("Terminal", "com.apple.Terminal"),
            ("iTerm", "com.googlecode.iterm2"),
            ("Warp", "dev.warp.Warp-Stable"),
            ("Ghostty", "com.mitchellh.ghostty")
        ]
        
        availableTerminals = terminals.filter { terminal in
            NSWorkspace.shared.urlForApplication(withBundleIdentifier: terminal.1) != nil
        }
    }
}

struct GitHubSettingsView: View {
    @State private var token: String = ""
    @State private var hasStoredToken: Bool = KeychainHelper.shared.loadGitHubToken()?.isEmpty == false
    @State private var statusMessage: String?

    var body: some View {
        Form {
            Section {
                SecureField("Fine-grained PAT", text: $token)
                    .textFieldStyle(.roundedBorder)

                HStack {
                    Button("Save") { save() }
                        .disabled(token.isEmpty)
                    Button("Clear") { clear() }
                        .disabled(!hasStoredToken)
                    Spacer()
                    if hasStoredToken {
                        Label("Token stored", systemImage: "checkmark.seal.fill")
                            .foregroundStyle(.green)
                            .font(.caption)
                    }
                }

                if let statusMessage {
                    Text(statusMessage)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                Text("Used by Portfolio CI to read GitHub Actions runs. A fine-grained PAT with read access to Actions is sufficient. Stored in your macOS Keychain.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } header: {
                Text("GitHub Token")
                    .font(.headline)
            }
        }
        .formStyle(.grouped)
        .padding()
    }

    private func save() {
        do {
            try KeychainHelper.shared.saveGitHubToken(token)
            GitHubService.shared.configureFromKeychain()
            hasStoredToken = true
            token = ""
            statusMessage = "Saved."
        } catch {
            statusMessage = "Failed to save: \(error.localizedDescription)"
        }
    }

    private func clear() {
        KeychainHelper.shared.deleteGitHubToken()
        GitHubService.shared.configureFromKeychain()
        hasStoredToken = false
        token = ""
        statusMessage = "Cleared."
    }
}

struct AboutView: View {
    var body: some View {
        VStack(spacing: 20) {
            Image(systemName: "square.grid.2x2")
                .font(.system(size: 60))
                .foregroundColor(.accentColor)
            
            Text("DevCanopy")
                .font(.largeTitle)
                .bold()
            
            Text("Version \(Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "0.1.0")")
                .foregroundStyle(.secondary)
            
            Text("Monitor your development infrastructure")
                .font(.headline)
            
            Divider()
                .frame(width: 200)
            
            VStack(alignment: .leading, spacing: 8) {
                Link("GitHub Repository", destination: URL(string: "https://github.com/Sassy-Dog/devcanopy")!)
                Link("Report an Issue", destination: URL(string: "https://github.com/Sassy-Dog/devcanopy/issues")!)
                Link("Documentation", destination: URL(string: "https://devcanopy.app/docs")!)
            }
            
            Spacer()
            
            Text("© 2024 Sassy Dog")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding()
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}