import SwiftUI

/// Settings for the OpenClaw gateway: the WebSocket URL, an optional bearer
/// token, and the device-pairing status block (this install's device id plus the
/// `openclaw devices approve` instruction while approval is pending). Modeled on
/// `GitHubSettingsView`.
struct OpenClawSettingsView: View {
    @EnvironmentObject private var service: OpenClawService

    @AppStorage("openclawGatewayURL") private var gatewayURL: String = ""
    @State private var token: String = ""
    @State private var hasStoredToken: Bool = KeychainHelper.shared.loadOpenClawToken()?.isEmpty == false
    @State private var statusMessage: String?

    var body: some View {
        Form {
            Section {
                TextField("ws://host:7878  or  wss://host", text: $gatewayURL)
                    .textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled()

                SecureField("Bearer token (optional)", text: $token)
                    .textFieldStyle(.roundedBorder)

                HStack {
                    Button("Save") { save() }
                    Button("Clear token") { clearToken() }
                        .disabled(!hasStoredToken)
                    Spacer()
                    if hasStoredToken {
                        Label("Token stored", systemImage: "checkmark.seal.fill")
                            .foregroundStyle(.green)
                            .font(.caption)
                    }
                }

                if let statusMessage {
                    Text(statusMessage).font(.caption).foregroundStyle(.secondary)
                }

                Text("DevCanopy monitors an OpenClaw agent farm over a WebSocket. Most gateways authenticate via device pairing (below); a bearer token is only needed if the gateway requires one. The gateway's controlUi.allowedOrigins must permit this host. Stored in your macOS Keychain.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } header: {
                Text("OpenClaw Gateway").font(.headline)
            }

            Section {
                connectionRow
                pairingBlock
            } header: {
                Text("Device Pairing").font(.headline)
            }
        }
        .formStyle(.grouped)
        .padding()
    }

    private var connectionRow: some View {
        HStack {
            Text("Status").foregroundStyle(.secondary)
            Spacer()
            switch service.snapshot.connection {
            case .connected:
                Label("Connected", systemImage: "dot.radiowaves.left.and.right").foregroundStyle(.green)
            case .connecting:
                Label("Connecting…", systemImage: "ellipsis").foregroundStyle(.orange)
            case .idle:
                Label("Idle", systemImage: "pause.circle").foregroundStyle(.secondary)
            case let .disconnected(reason):
                Label(reason, systemImage: "exclamationmark.triangle").foregroundStyle(.orange)
            }
        }
        .font(.caption)
    }

    @ViewBuilder
    private var pairingBlock: some View {
        let deviceID = service.snapshot.pairing?.deviceID ?? OpenClawDeviceIdentity.currentDeviceID()
        VStack(alignment: .leading, spacing: 8) {
            if let deviceID {
                HStack {
                    Text("Device ID").foregroundStyle(.secondary)
                    Spacer()
                    Text(deviceID).font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                }
                .font(.caption)
            } else {
                Text("Device key is generated on first connect.")
                    .font(.caption).foregroundStyle(.secondary)
            }

            if let pairing = service.snapshot.pairing {
                Divider()
                Text(
                    pairing.kind == .scopeUpgrade
                        ? "The gateway needs to approve broader scopes for this device."
                        : "This device isn't paired yet. Approve it on the gateway host:"
                )
                .font(.caption)
                if let request = pairing.requestID {
                    Text("openclaw devices approve \(request)")
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .padding(6)
                        .background(Color.secondary.opacity(0.1))
                        .clipShape(RoundedRectangle(cornerRadius: 6))
                }
                if let hint = pairing.remediationHint {
                    Text(hint).font(.caption).foregroundStyle(.secondary)
                }
                Button("Retry now") { service.restart() }
            }
        }
    }

    private func save() {
        do {
            if token.isEmpty {
                // Empty field means "no change"; only persist a non-empty entry.
            } else {
                try KeychainHelper.shared.saveOpenClawToken(token)
                hasStoredToken = true
                token = ""
            }
            service.restart()
            statusMessage = "Saved."
        } catch {
            statusMessage = "Failed to save: \(error.localizedDescription)"
        }
    }

    private func clearToken() {
        KeychainHelper.shared.deleteOpenClawToken()
        hasStoredToken = false
        token = ""
        service.restart()
        statusMessage = "Token cleared."
    }
}
