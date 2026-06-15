import Foundation
import os.log

/// DevCanopy logging system
public struct Logger {
    private let subsystem = "com.sassydog.devcanopy"
    private let category: String
    private let osLog: OSLog

    // Check environment variables
    private static let consoleLoggingEnabled = ProcessInfo.processInfo.environment["DEVCANOPY_LOG_CONSOLE"] == "1"
    private static let logLevelString = ProcessInfo.processInfo.environment["DEVCANOPY_LOG_LEVEL"] ?? "info"

    public enum LogLevel: Int, Comparable {
        case debug = 0
        case info = 1
        case warning = 2
        case error = 3

        public static func < (lhs: LogLevel, rhs: LogLevel) -> Bool {
            lhs.rawValue < rhs.rawValue
        }

        static func from(string: String) -> LogLevel {
            switch string.lowercased() {
            case "debug": .debug
            case "info": .info
            case "warn", "warning": .warning
            case "error": .error
            default: .info
            }
        }

        var osLogType: OSLogType {
            switch self {
            case .debug: .debug
            case .info: .info
            case .warning: .default
            case .error: .error
            }
        }

        var emoji: String {
            switch self {
            case .debug: "🔍"
            case .info: "ℹ️"
            case .warning: "⚠️"
            case .error: "❌"
            }
        }

        var color: String {
            switch self {
            case .debug: "\u{001B}[90m" // Gray
            case .info: "\u{001B}[34m" // Blue
            case .warning: "\u{001B}[33m" // Yellow
            case .error: "\u{001B}[31m" // Red
            }
        }
    }

    private static let currentLogLevel = LogLevel.from(string: logLevelString)
    private static let resetColor = "\u{001B}[0m"

    public init(category: String) {
        self.category = category
        osLog = OSLog(subsystem: subsystem, category: category)
    }

    public func debug(_ message: String, file: String = #file, function: String = #function, line: Int = #line) {
        log(message, level: .debug, file: file, function: function, line: line)
    }

    public func info(_ message: String, file: String = #file, function: String = #function, line: Int = #line) {
        log(message, level: .info, file: file, function: function, line: line)
    }

    public func warning(_ message: String, file: String = #file, function: String = #function, line: Int = #line) {
        log(message, level: .warning, file: file, function: function, line: line)
    }

    public func error(_ message: String, file: String = #file, function: String = #function, line: Int = #line) {
        log(message, level: .error, file: file, function: function, line: line)
    }

    private func log(_ message: String, level: LogLevel, file: String, function: String, line: Int) {
        // Check if we should log at this level
        guard level >= Self.currentLogLevel else { return }

        // Always log to unified logging system
        os_log("%{public}@", log: osLog, type: level.osLogType, message)

        // Also log to console if enabled
        if Self.consoleLoggingEnabled {
            let fileName = URL(fileURLWithPath: file).lastPathComponent
            let timestamp = DateFormatter.logTimestamp.string(from: Date())

            let consoleMessage = "\(level.color)\(timestamp) \(level.emoji) [\(category)] \(fileName):\(line) - \(function) - \(message)\(Self.resetColor)"
            print(consoleMessage)
            fflush(stdout)
        }
    }
}

extension DateFormatter {
    static let logTimestamp: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateFormat = "HH:mm:ss.SSS"
        return formatter
    }()
}

// MARK: - Global Loggers

public let appLogger = Logger(category: "App")
public let gitLogger = Logger(category: "Git")
public let serviceLogger = Logger(category: "Service")
public let uiLogger = Logger(category: "UI")
