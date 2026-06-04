import Foundation
import IOKit

/// Shared utilities for working with IOKit services
/// These functions encapsulate common patterns and ensure proper resource cleanup
enum IOKitUtilities {
    /// Iterates over all IOKit services matching a given class name
    /// Automatically handles iterator lifecycle and service cleanup
    ///
    /// - Parameters:
    ///   - className: The IOKit service class name to match
    ///   - body: Closure called for each matching service
    /// - Returns: True if iteration was successful, false if the service lookup failed
    @discardableResult
    static func iterateServices(
        matching className: String,
        body: (io_service_t) throws -> Void
    ) rethrows -> Bool {
        var iterator: io_iterator_t = 0
        let result = IOServiceGetMatchingServices(
            kIOMainPortDefault,
            IOServiceMatching(className),
            &iterator
        )

        guard result == KERN_SUCCESS else {
            return false
        }

        defer { IOObjectRelease(iterator) }

        var service: io_object_t = IOIteratorNext(iterator)
        while service != 0 {
            defer { IOObjectRelease(service) }
            try body(service)
            service = IOIteratorNext(iterator)
        }

        return true
    }

    /// Reads a property from an IOKit service
    /// Automatically handles type conversion and cleanup
    ///
    /// - Parameters:
    ///   - service: The IOKit service object
    ///   - key: The property key to read
    /// - Returns: The property value as Any, or nil if not found
    static func readProperty(from service: io_service_t, key: String) -> Any? {
        guard let property = IORegistryEntryCreateCFProperty(
            service,
            key as CFString,
            kCFAllocatorDefault,
            0
        ) else {
            return nil
        }

        return property.takeRetainedValue()
    }

    /// Reads a property from an IOKit service and casts to specified type
    /// Automatically handles type conversion and cleanup
    ///
    /// - Parameters:
    ///   - service: The IOKit service object
    ///   - key: The property key to read
    /// - Returns: The property value cast to T, or nil if not found or wrong type
    static func readProperty<T>(from service: io_service_t, key: String) -> T? {
        readProperty(from: service, key: key) as? T
    }

    /// Reads a numeric property from an IOKit service
    /// Handles both CFNumber and direct numeric types
    ///
    /// - Parameters:
    ///   - service: The IOKit service object
    ///   - key: The property key to read
    /// - Returns: The numeric value as Double, or nil if not found
    static func readNumericProperty(from service: io_service_t, key: String) -> Double? {
        guard let value = readProperty(from: service, key: key) else {
            return nil
        }

        // Handle CFNumber
        if let number = value as? NSNumber {
            return number.doubleValue
        }

        // Handle direct numeric types
        if let doubleValue = value as? Double {
            return doubleValue
        }

        if let intValue = value as? Int {
            return Double(intValue)
        }

        if let int64Value = value as? Int64 {
            return Double(int64Value)
        }

        return nil
    }

    /// Reads a string property from an IOKit service
    ///
    /// - Parameters:
    ///   - service: The IOKit service object
    ///   - key: The property key to read
    /// - Returns: The string value, or nil if not found
    static func readStringProperty(from service: io_service_t, key: String) -> String? {
        readProperty(from: service, key: key)
    }

    /// Reads a Data property from an IOKit service
    ///
    /// - Parameters:
    ///   - service: The IOKit service object
    ///   - key: The property key to read
    /// - Returns: The data value, or nil if not found
    static func readDataProperty(from service: io_service_t, key: String) -> Data? {
        readProperty(from: service, key: key)
    }

    /// Gets the parent service of a given IOKit service
    /// Automatically handles cleanup
    ///
    /// - Parameters:
    ///   - service: The child service
    ///   - plane: The IOKit plane to search (e.g., kIOServicePlane)
    ///   - body: Closure called with the parent service
    /// - Returns: True if a parent was found, false otherwise
    @discardableResult
    static func withParentService(
        of service: io_service_t,
        in plane: String = kIOServicePlane,
        body: (io_service_t) throws -> Void
    ) rethrows -> Bool {
        var parent: io_service_t = 0
        let result = IORegistryEntryGetParentEntry(service, plane, &parent)

        guard result == KERN_SUCCESS else {
            return false
        }

        defer { IOObjectRelease(parent) }
        try body(parent)
        return true
    }

    /// Finds the first service matching a class name and returns a value from it
    /// This is a convenience method for when you only need one service
    ///
    /// - Parameters:
    ///   - className: The IOKit service class name to match
    ///   - transform: Closure to extract a value from the service
    /// - Returns: The transformed value, or nil if no service found
    static func findService<T>(
        matching className: String,
        transform: (io_service_t) throws -> T?
    ) rethrows -> T? {
        var result: T?

        try iterateServices(matching: className) { service in
            if let value = try transform(service) {
                result = value
                return // Stop after first match
            }
        }

        return result
    }
}
