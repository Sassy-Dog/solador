import SwiftData
import SwiftUI

/// Settings tab to manage remote hosts running the DevCanopy agent. Connection
/// details persist as `MonitoredHost`; the bearer token goes to the Keychain.
struct HostsSettingsView: View {
    @Environment(\.modelContext) private var modelContext
    @EnvironmentObject private var coordinator: RemoteHostsCoordinator
    @EnvironmentObject private var local: HostMetricsService
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

            if !local.hiddenMounts.isEmpty {
                Section {
                    ForEach(local.hiddenMounts.sorted(), id: \.self) { mount in
                        hiddenVolumeRow(mount) { local.unhideVolume(mount) }
                    }
                } header: {
                    Text("Hidden Volumes — \(local.hostName) (this Mac)").font(.headline)
                }
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
        VStack(alignment: .leading, spacing: 6) {
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
            if !host.hiddenVolumeMounts.isEmpty {
                ForEach(host.hiddenVolumeMounts, id: \.self) { mount in
                    hiddenVolumeRow(mount) { unhide(mount, on: host) }
                }
                .padding(.leading, 12)
            }
        }
    }

    private func hiddenVolumeRow(_ mount: String, unhide: @escaping () -> Void) -> some View {
        HStack {
            Image(systemName: "eye.slash").font(.caption).foregroundStyle(.secondary)
            Text(mount).font(.caption.monospaced())
            Spacer()
            Button("Unhide", action: unhide).buttonStyle(.borderless).font(.caption)
        }
    }

    private func unhide(_ mount: String, on host: MonitoredHost) {
        host.hiddenVolumeMounts.removeAll { $0 == mount }
        try? modelContext.save()
        // Push into the live service without reload() — a reload would reset the
        // host card's sparkline history and volume debounce.
        coordinator.applyHiddenMounts()
    }

    private func enabledBinding(_ host: MonitoredHost) -> Binding<Bool> {
        Binding(
            get: { host.enabled },
            set: { host.enabled = $0
                try? modelContext.save()
                coordinator.reload()
            }
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
            newName = ""
            newAddress = ""
            newPort = "7878"
            newToken = ""
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
            switch await probe.checkHealth() {
            case let .ok(info):
                // Show the live agent version + hostname so a redeploy can be
                // confirmed without SSHing in. Flag a stale sampler if present.
                var line = "✓ \(info.hostname) · agent v\(info.version)"
                if info.samplerStale == true { line += " · sampler stale" }
                testResults[host.id] = line
            case let .failure(error):
                switch error {
                case .authFailed:
                    testResults[host.id] = "✗ auth failed (401) — check token"
                case .decodeFailed:
                    testResults[host.id] = "✗ decode failed — agent/app version skew?"
                case let .httpStatus(code):
                    testResults[host.id] = "✗ HTTP \(code)"
                case .unreachable:
                    testResults[host.id] = "✗ unreachable — host down or agent stopped"
                }
            }
        }
    }
}
