import Foundation
import ServiceManagement

/// Wraps `SMAppService.mainApp` so the "Launch at Login" setting actually
/// registers/unregisters the app as a login item. Reads of `isEnabled` reflect
/// the system's real registration state, so the toggle can never silently lie.
enum LaunchAtLogin {
    /// Whether the app is currently registered to launch at login.
    static var isEnabled: Bool {
        SMAppService.mainApp.status == .enabled
    }

    /// Registers or unregisters the app as a login item, mirroring `enabled`.
    /// Returns `true` on success; on failure the caller should re-read
    /// `isEnabled` so the UI reflects the unchanged system state.
    @discardableResult
    static func setEnabled(_ enabled: Bool) -> Bool {
        do {
            if enabled {
                if SMAppService.mainApp.status != .enabled {
                    try SMAppService.mainApp.register()
                }
            } else {
                if SMAppService.mainApp.status == .enabled {
                    try SMAppService.mainApp.unregister()
                }
            }
            return true
        } catch {
            appLogger.error("LaunchAtLogin: failed to set enabled=\(enabled): \(error.localizedDescription)")
            return false
        }
    }
}
