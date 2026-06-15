import Foundation
import Sentry

/// Configures optional Sentry crash/error reporting for DevCanopy (issue #18).
///
/// Privacy contract: DevCanopy ships **no telemetry by default**. Sentry is
/// strictly opt-in — it is started *only* when a non-empty DSN is configured.
/// When no DSN is found this is a complete no-op: the SDK is never started,
/// there is zero network activity, and nothing is initialized. We never attach
/// personally identifying information (`sendDefaultPii = false`) and must never
/// log or attach tokens/credentials to Sentry events.
enum SentrySetup {
    /// Info.plist key that carries the DSN. Left empty in dev/local builds; a
    /// real value is injected at build/release time from the GitHub Actions
    /// secret (never hardcoded in the repo).
    static let plistKey = "SentryDSN"

    /// Environment variable that overrides the Info.plist DSN (handy for
    /// local/CI experiments without rebuilding).
    static let environmentKey = "SENTRY_DSN"

    /// Resolves the Sentry DSN from a configurable source.
    ///
    /// Resolution order:
    /// 1. `SENTRY_DSN` environment variable (local/CI overrides)
    /// 2. `SentryDSN` key in the app bundle's `Info.plist`
    ///
    /// Returns `nil` when no non-empty DSN is configured, so the SDK is never
    /// started in dev/local builds that ship with an empty default value.
    ///
    /// Parameterized on its inputs so the no-op contract is unit-testable
    /// without touching the process environment or app bundle.
    static func resolveDSN(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        plistValue: String? = Bundle.main.object(forInfoDictionaryKey: plistKey) as? String
    ) -> String? {
        if let envDSN = environment[environmentKey],
           !envDSN.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        {
            return envDSN
        }

        if let plistDSN = plistValue,
           !plistDSN.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        {
            return plistDSN
        }

        return nil
    }

    /// Starts Sentry if a non-empty DSN is configured; otherwise a complete
    /// no-op (SDK not started, zero network, no init).
    static func start() {
        guard let dsn = resolveDSN() else {
            appLogger.info("Sentry disabled: no \(environmentKey) configured (no telemetry by default)")
            return
        }

        SentrySDK.start { options in
            options.dsn = dsn
            options.releaseName = releaseName()

            // Privacy: never send default PII (IP address, etc.). Telemetry
            // stays opt-in / disclosed.
            options.sendDefaultPii = false
        }

        appLogger.info("Sentry initialized")
    }

    /// `bundleId@shortVersion+build`, matching Sentry's release-name convention.
    private static func releaseName() -> String {
        let info = Bundle.main.infoDictionary
        let bundleID = Bundle.main.bundleIdentifier ?? "com.sassydog.devcanopy"
        let version = info?["CFBundleShortVersionString"] as? String ?? "0.0.0"
        let build = info?["CFBundleVersion"] as? String ?? "0"
        return "\(bundleID)@\(version)+\(build)"
    }
}
