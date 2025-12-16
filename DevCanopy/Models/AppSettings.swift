import Foundation
import SwiftData

@Model
final class AppSettings {
    var id: UUID = UUID()
    var refreshInterval: RefreshInterval
    var preferredTerminal: String
    var launchAtLogin: Bool
    var showPerformanceOverlay: Bool
    var lastUpdated: Date
    
    init() {
        self.refreshInterval = .oneMinute
        self.preferredTerminal = "Terminal"
        self.launchAtLogin = false
        self.showPerformanceOverlay = false
        self.lastUpdated = Date()
    }
    
    static var shared: AppSettings {
        // This will be loaded from SwiftData
        return AppSettings()
    }
}

enum RefreshInterval: Int, Codable, CaseIterable {
    case thirtySeconds = 30
    case oneMinute = 60
    case fiveMinutes = 300
    
    var displayName: String {
        switch self {
        case .thirtySeconds:
            return "30 seconds"
        case .oneMinute:
            return "1 minute"
        case .fiveMinutes:
            return "5 minutes"
        }
    }
}