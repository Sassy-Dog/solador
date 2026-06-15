import Foundation
import os

/// Dependency-free drop-in replacement for Lupita's `Logger`.
///
/// The vendored metrics engine references `Logger.monitor`, `Logger.system`,
/// `Logger.ui`, `Logger.network`, plus the `isDebugEnabled` / `isVerboseEnabled`
/// flags. This shim backs those with `os.Logger` so the engine compiles unchanged
/// without pulling in swift-log.
enum Logger {
    /// Lazy-message logging surface mirroring the call sites in the engine.
    struct Channel {
        private let logger: os.Logger

        init(category: String) {
            logger = os.Logger(subsystem: "com.sassydog.HostMetricsKit", category: category)
        }

        func error(_ message: @autoclosure () -> String) {
            let text = message()
            logger.error("\(text, privacy: .public)")
        }

        func warning(_ message: @autoclosure () -> String) {
            let text = message()
            logger.warning("\(text, privacy: .public)")
        }

        func info(_ message: @autoclosure () -> String) {
            let text = message()
            logger.info("\(text, privacy: .public)")
        }

        func debug(_ message: @autoclosure () -> String) {
            let text = message()
            logger.debug("\(text, privacy: .public)")
        }

        func trace(_ message: @autoclosure () -> String) {
            let text = message()
            logger.trace("\(text, privacy: .public)")
        }
    }

    static let monitor = Channel(category: "monitor")
    static let system = Channel(category: "system")
    static let ui = Channel(category: "ui")
    static let network = Channel(category: "network")

    static var isDebugEnabled: Bool {
        false
    }

    static var isVerboseEnabled: Bool {
        false
    }
}
