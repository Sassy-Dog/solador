import Foundation
import SwiftData

/// A remote host running the DevCanopy agent. Connection details are persisted;
/// the bearer token lives in the Keychain (keyed by `id`), never in SwiftData.
@Model
final class MonitoredHost {
    var id: UUID = UUID()
    var name: String = ""
    var address: String = "" // Tailscale IP or MagicDNS name
    var port: Int = 7878
    var enabled: Bool = true
    var createdAt: Date = Date()
    var lastSeen: Date?
    /// Mount paths the user hid from this host's Volumes section in the cockpit.
    var hiddenVolumeMounts: [String] = []

    init(name: String, address: String, port: Int = 7878, enabled: Bool = true) {
        id = UUID()
        self.name = name
        self.address = address
        self.port = port
        self.enabled = enabled
        createdAt = Date()
    }
}
