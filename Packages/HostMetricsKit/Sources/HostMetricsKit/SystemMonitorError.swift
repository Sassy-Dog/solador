import Foundation

enum SystemMonitorError: LocalizedError {
    case ioKitError(kern_return_t, String)
    case sysctlError(Int32, String)
    case invalidData(String)
    case serviceNotFound(String)
    case memoryAllocationError
    case counterWrapAround(String)

    var errorDescription: String? {
        switch self {
        case let .ioKitError(code, context):
            "IOKit error \(code) in \(context): \(String(cString: mach_error_string(code)))"
        case let .sysctlError(code, context):
            "sysctl error \(code) in \(context): \(String(cString: strerror(code)))"
        case let .invalidData(context):
            "Invalid data received: \(context)"
        case let .serviceNotFound(service):
            "IOKit service not found: \(service)"
        case .memoryAllocationError:
            "Failed to allocate memory for system monitoring"
        case let .counterWrapAround(context):
            "Counter wrap-around detected in \(context)"
        }
    }
}

/// Result type for monitoring operations
typealias MonitoringResult<T> = Result<T, SystemMonitorError>

/// Helper to convert kern_return_t to Result
extension kern_return_t {
    func toResult<T>(value: T, context: String) -> MonitoringResult<T> {
        if self == KERN_SUCCESS {
            .success(value)
        } else {
            .failure(.ioKitError(self, context))
        }
    }
}
