import SwiftUI

struct SettingsView: View {
    var body: some View {
        TabView {
            GeneralSettingsView()
                .tabItem {
                    Label("General", systemImage: "gear")
                }

            GitHubSettingsView()
                .tabItem {
                    Label("GitHub", systemImage: "checkmark.seal")
                }

            PortfolioSettingsView()
                .tabItem {
                    Label("Portfolio", systemImage: "square.grid.2x2")
                }

            HostsSettingsView()
                .tabItem {
                    Label("Hosts", systemImage: "server.rack")
                }

            OpenClawSettingsView()
                .tabItem {
                    Label("OpenClaw", systemImage: "brain.head.profile")
                }

            AzureCostSettingsView()
                .tabItem {
                    Label("Azure Cost", systemImage: "dollarsign.circle")
                }

            AboutView()
                .tabItem {
                    Label("About", systemImage: "info.circle")
                }
        }
        .frame(width: 680, height: 400)
    }
}

struct GeneralSettingsView: View {
    @AppStorage("refreshInterval") private var refreshInterval: Int = RefreshInterval.default.rawValue
    @AppStorage("coreRowSpan") private var coreRowSpan: Int = 2
    @AppStorage("hostOverflowMode") private var hostOverflowRaw: String = HostOverflowMode.stack.rawValue

    /// Mirrors the system login-item registration so the toggle reflects real
    /// state rather than a stored preference that may have failed to apply.
    @State private var launchAtLogin: Bool = LaunchAtLogin.isEnabled

    var body: some View {
        Form {
            Section {
                Picker("Refresh Interval", selection: $refreshInterval) {
                    ForEach(RefreshInterval.allCases, id: \.rawValue) { interval in
                        Text(interval.displayName).tag(interval.rawValue)
                    }
                }

                Toggle("Launch at Login", isOn: $launchAtLogin)
                    .onChange(of: launchAtLogin) { _, newValue in
                        LaunchAtLogin.setEnabled(newValue)
                        // Re-sync to the authoritative system state in case
                        // registration was denied or failed.
                        launchAtLogin = LaunchAtLogin.isEnabled
                    }

                VStack(alignment: .leading, spacing: 2) {
                    Stepper("CPU core rows: \(coreRowSpan)", value: $coreRowSpan, in: 1 ... 4)
                    Text("How many rows tall the per-core CPU grid is on every host card.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                VStack(alignment: .leading, spacing: 2) {
                    Picker("When host cards don't fit", selection: $hostOverflowRaw) {
                        ForEach(HostOverflowMode.allCases) { mode in
                            Text(mode.displayName).tag(mode.rawValue)
                        }
                    }
                    Text("Stacking keeps every host visible; tabs keep the cockpit short.")
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
        .onAppear {
            // Reflect any out-of-band change to the login-item registration.
            launchAtLogin = LaunchAtLogin.isEnabled
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

                Text("Used by the Repos panel. Grant the fine-grained PAT read access to **Actions** (workflow runs), **Contents** (remote branch counts), **Issues** (open-issue counts), and **Pull requests** (open-PR counts). Stored in your macOS Keychain.")
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

struct AzureCostSettingsView: View {
    @State private var sasURL: String = ""
    @State private var hasStoredSAS: Bool = KeychainHelper.shared.loadAzureCostSAS()?.isEmpty == false
    @State private var statusMessage: String?

    /// Monthly budget (USD) for the panel's projected-vs-budget bar. Not a secret,
    /// so `@AppStorage`/UserDefaults, not Keychain; the panel reads the same key.
    @AppStorage("azureMonthlyBudgetUSD") private var budgetUSD: Double = 0

    var body: some View {
        Form {
            Section {
                TextField("Monthly budget (USD)", value: $budgetUSD, format: .number)
                    .textFieldStyle(.roundedBorder)

                Text("Powers the projected-vs-budget bar on the Azure Cost panel. Leave at 0 to hide the bar.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } header: {
                Text("Budget")
                    .font(.headline)
            }

            Section {
                SecureField("SAS URL", text: $sasURL)
                    .textFieldStyle(.roundedBorder)

                HStack {
                    Button("Save") { save() }
                        .disabled(sasURL.isEmpty)
                    Button("Clear") { clear() }
                        .disabled(!hasStoredSAS)
                    Spacer()
                    if hasStoredSAS {
                        Label("SAS stored", systemImage: "checkmark.seal.fill")
                            .foregroundStyle(.green)
                            .font(.caption)
                    }
                }

                if let statusMessage {
                    Text(statusMessage)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                Text("Used by the Azure Cost panel. Paste a container-scoped, read+list SAS URL for the `cost-exports` container on `stsassydog` (e.g. `https://stsassydog.blob.core.windows.net/cost-exports?sv=...&sig=...`). The SAS is the only credential — no Azure sign-in. Stored in your macOS Keychain.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } header: {
                Text("Azure Cost SAS")
                    .font(.headline)
            }
        }
        .formStyle(.grouped)
        .padding()
    }

    private func save() {
        do {
            try KeychainHelper.shared.saveAzureCostSAS(sasURL)
            AzureCostService.shared.configureFromKeychain()
            hasStoredSAS = true
            sasURL = ""
            statusMessage = "Saved."
        } catch {
            statusMessage = "Failed to save: \(error.localizedDescription)"
        }
    }

    private func clear() {
        KeychainHelper.shared.deleteAzureCostSAS()
        AzureCostService.shared.configureFromKeychain()
        hasStoredSAS = false
        sasURL = ""
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
