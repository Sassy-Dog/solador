import Foundation

/// User-selectable cadence for the periodic data-fetch services (workflow health,
/// runners, worktrees, Claude usage). Stored as its raw `Int` (seconds) under
/// the `refreshInterval` `AppStorage`/`UserDefaults` key — the single settings
/// mechanism for the app.
///
/// Note: real-time host/container metrics poll on their own sub-second/10s
/// cadences and are intentionally not governed by this setting.
enum RefreshInterval: Int, Codable, CaseIterable {
    case thirtySeconds = 30
    case oneMinute = 60
    case fiveMinutes = 300

    /// The default cadence when nothing has been chosen yet.
    static let `default`: RefreshInterval = .oneMinute

    /// The current selection from `UserDefaults`, falling back to `.default`
    /// when unset or out of range.
    static var current: RefreshInterval {
        let raw = UserDefaults.standard.integer(forKey: "refreshInterval")
        return RefreshInterval(rawValue: raw) ?? .default
    }

    var seconds: TimeInterval {
        TimeInterval(rawValue)
    }

    var displayName: String {
        switch self {
        case .thirtySeconds:
            "30 seconds"
        case .oneMinute:
            "1 minute"
        case .fiveMinutes:
            "5 minutes"
        }
    }
}
