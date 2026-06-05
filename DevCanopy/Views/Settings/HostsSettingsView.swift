import SwiftUI
import SwiftData

/// Settings tab to manage remote hosts running the DevCanopy agent. Connection
/// details persist as `MonitoredHost`; the bearer token goes to the Keychain.
struct HostsSettingsView: View {
    @Environment(\.modelContext) private var modelContext
    @EnvironmentObject private var coordinator: RemoteHostsCoordinator
    @Query(sort: \MonitoredHost.name) private var hosts: [MonitoredHost]

    @State private var newName = ""
    @State private var newAddress = ""
    @State private var newPort = "7878"
    @State private var newToken = ""
    @State private var statusMessage: String?
    @State private var testResults: [UUID: String] = [:]

    var body: some View {
        Form {
            Section {
                if hosts.isEmpty {
                    Text("No remote hosts yet. Add one below, then it appears in the cockpit.")
                        .font(.caption).foregroundStyle(.secondary)
                } else {
                    ForEach(hosts) { host in hostRow(host) }
                }
            } header: {
                Text("Remote Hosts").font(.headline)
            }

            Section {
                TextField("Name (e.g. ubu-3xdv)", text: $newName)
                TextField("Address (Tailscale IP or MagicDNS name)", text: $newAddress)
                TextField("Port", text: $newPort)
                SecureField("Agent token", text: $newToken)
                HStack {
                    Button("Add Host") { addHost() }
                        .disabled(newName.isEmpty || newAddress.isEmpty)
                    Spacer()
                    if let statusMessage {
                        Text(statusMessage).font(.caption).foregroundStyle(.secondary)
                    }
                }
                Text("The agent serves metrics on the host's tailnet address. Token is stored in your Keychain.")
                    .font(.caption).foregroundStyle(.secondary)
            } header: {
                Text("Add Host").font(.headline)
            }
        }
        .formStyle(.grouped)
        .padding()
    }

    private func hostRow(_ host: MonitoredHost) -> some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(host.name).bold()
                Text("\(host.address):\(host.port)")
                    .font(.caption).foregroundStyle(.secondary)
                if let result = testResults[host.id] {
                    Text(result).font(.caption2).foregroundStyle(.secondary)
                }
            }
            Spacer()
            Button("Test") { test(host) }
            Toggle("", isOn: enabledBinding(host)).labelsHidden()
            Button(role: .destructive) { remove(host) } label: {
                Image(systemName: "trash")
            }
            .buttonStyle(.borderless)
        }
    }

    private func enabledBinding(_ host: MonitoredHost) -> Binding<Bool> {
        Binding(
            get: { host.enabled },
            set: { host.enabled = $0; try? modelContext.save(); coordinator.reload() }
        )
    }

    private func addHost() {
        let port = Int(newPort) ?? 7878
        let host = MonitoredHost(name: newName, address: newAddress, port: port)
        modelContext.insert(host)
        if !newToken.isEmpty {
            try? KeychainHelper.shared.saveHostToken(newToken, hostID: host.id)
        }
        do {
            try modelContext.save()
            coordinator.reload()
            statusMessage = "Added \(newName)."
            newName = ""; newAddress = ""; newPort = "7878"; newToken = ""
        } catch {
            statusMessage = "Failed: \(error.localizedDescription)"
        }
    }

    private func remove(_ host: MonitoredHost) {
        KeychainHelper.shared.deleteHostToken(hostID: host.id)
        modelContext.delete(host)
        try? modelContext.save()
        coordinator.reload()
    }

    private func test(_ host: MonitoredHost) {
        let token = KeychainHelper.shared.loadHostToken(hostID: host.id)
        let probe = RemoteHostMetricsService(
            hostName: host.name, address: host.address, port: host.port, token: token
        )
        testResults[host.id] = "Testing…"
        Task {
            let ok = await probe.checkHealth()
            testResults[host.id] = ok ? "✓ reachable" : "✗ unreachable / bad token"
        }
    }
}
