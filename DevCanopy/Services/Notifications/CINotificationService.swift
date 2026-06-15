import AppKit
import Foundation
import UserNotifications

/// Delivers macOS notifications for CI events. Today it surfaces one signal:
/// a tracked run entering the `.waiting` deployment-protection gate ("needs
/// approval"). Tapping the notification opens the run's `htmlURL`.
///
/// This is the app's only notification delivery path. Authorization is requested
/// lazily on first use; if the user has denied it, delivery is a silent no-op.
@MainActor
final class CINotificationService: NSObject, UNUserNotificationCenterDelegate {
    static let shared = CINotificationService()

    /// userInfo key carrying the deep-link URL to open on tap.
    private static let urlKey = "htmlURL"

    private let center: UNUserNotificationCenter
    private var didRequestAuthorization = false

    /// `UNUserNotificationCenter.current()` traps in a process without a proper
    /// app bundle (e.g. the XCTest host), so allow injecting nil to disable.
    init(center: UNUserNotificationCenter? = CINotificationService.defaultCenter()) {
        self.center = center ?? UNUserNotificationCenter.current()
        enabled = center != nil
        super.init()
        if enabled {
            self.center.delegate = self
        }
    }

    private let enabled: Bool

    private nonisolated static func defaultCenter() -> UNUserNotificationCenter? {
        // Only safe to touch when running inside a real, bundled app.
        guard Bundle.main.bundleIdentifier != nil,
              Bundle.main.bundleURL.pathExtension == "app"
        else {
            return nil
        }
        return UNUserNotificationCenter.current()
    }

    /// Ask once for notification permission. Safe to call repeatedly.
    func requestAuthorizationIfNeeded() {
        guard enabled, !didRequestAuthorization else { return }
        didRequestAuthorization = true
        center.requestAuthorization(options: [.alert, .sound]) { granted, error in
            if let error {
                appLogger.debug("Notifications: authorization error: \(error.localizedDescription)")
            } else {
                appLogger.debug("Notifications: authorization granted=\(granted)")
            }
        }
    }

    /// Deliver a "needs approval" notification for a run parked at a gate.
    /// `repo` is the short repo name; `htmlURL` is the deep link opened on tap.
    func notifyNeedsApproval(repo: String, title: String, context: String, htmlURL: String) {
        guard enabled else { return }
        requestAuthorizationIfNeeded()

        let content = UNMutableNotificationContent()
        content.title = "\(repo) · needs approval"
        content.body = "\(title) · \(context) is parked at an approval gate."
        content.sound = .default
        content.userInfo = [Self.urlKey: htmlURL]

        let request = UNNotificationRequest(
            identifier: UUID().uuidString,
            content: content,
            trigger: nil // deliver immediately
        )
        center.add(request) { error in
            if let error {
                appLogger.debug("Notifications: delivery failed: \(error.localizedDescription)")
            }
        }
    }

    // MARK: - UNUserNotificationCenterDelegate

    /// Open the deep-linked run when the user taps the notification.
    nonisolated func userNotificationCenter(
        _: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let info = response.notification.request.content.userInfo
        if let urlString = info[Self.urlKey] as? String, let url = URL(string: urlString) {
            DispatchQueue.main.async { NSWorkspace.shared.open(url) }
        }
        completionHandler()
    }

    /// Show the banner even when DevCanopy is frontmost.
    nonisolated func userNotificationCenter(
        _: UNUserNotificationCenter,
        willPresent _: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .sound])
    }
}
