import Foundation

/// Central, off-by-default switch for demo mode.
///
/// Demo mode replaces the live metric collectors (local IOKit sampling and remote
/// agent polling over Tailscale) with a self-contained synthetic data source, so
/// the cockpit shows plausible CPU/memory/disk/network/GPU/battery activity without
/// touching real system state or any remote host. It's meant for screenshots, demos,
/// and UI testing.
///
/// ## Enabling
/// Launch with the `--demo` argument:
/// ```bash
/// ./dev run -- --demo
/// ```
/// or set the `DEVCANOPY_DEMO` environment variable to a truthy value
/// (`1`/`true`/`yes`). In Xcode: Product → Scheme → Edit Scheme → Arguments → add
/// `--demo`.
///
/// Normal launches are completely unaffected — nothing reads demo data unless this
/// flag is on.
enum DemoMode {
    /// Whether demo mode is active for this process. Resolved once at first access
    /// from the launch arguments / environment, so it's stable for the run.
    static let isEnabled: Bool = resolve()

    private static func resolve() -> Bool {
        let args = ProcessInfo.processInfo.arguments
        if args.contains("--demo") { return true }

        if let raw = ProcessInfo.processInfo.environment["DEVCANOPY_DEMO"]?.lowercased() {
            return ["1", "true", "yes", "on"].contains(raw)
        }
        return false
    }
}
