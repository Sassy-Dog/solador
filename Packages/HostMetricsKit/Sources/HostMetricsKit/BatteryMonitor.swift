import Foundation
import IOKit.ps
import os

/// Battery and power monitoring for Pro edition
class BatteryMonitor {
    // Use the centralized logger from Logger.swift

    struct BatteryInfo {
        let hasBattery: Bool
        let isCharging: Bool
        let currentCapacity: Int // 0-100%
        let cycleCount: Int
        let health: Double // 0-100%
        let powerSource: String // "AC Power" or "Battery Power"
        let timeRemaining: String // "2:30" or "Calculating..." or "Charged"
        let voltage: Double // in volts
        let amperage: Double // in milliamps (negative = discharging)
        let wattage: Double // calculated from voltage * amperage
        let temperature: Double // battery temperature in Celsius
    }

    func getBatteryInfo() -> BatteryInfo? {
        // Get power source info
        guard let info = IOPSCopyPowerSourcesInfo()?.takeRetainedValue(),
              let sources = IOPSCopyPowerSourcesList(info)?.takeRetainedValue() as? [Any]
        else {
            Logger.system.debug("No power sources found")
            return nil
        }

        // Find internal battery
        for source in sources {
            guard let sourceInfo = IOPSGetPowerSourceDescription(info, source as CFTypeRef)?.takeUnretainedValue() as? [String: Any] else {
                continue
            }

            // Check if this is the internal battery
            let type = sourceInfo[kIOPSTypeKey as String] as? String
            let transportType = sourceInfo[kIOPSTransportTypeKey as String] as? String

            if type == kIOPSInternalBatteryType as String || transportType == kIOPSInternalType as String {
                return parseBatteryInfo(from: sourceInfo)
            }
        }

        // No battery found - likely a desktop Mac
        return BatteryInfo(
            hasBattery: false,
            isCharging: false,
            currentCapacity: 100,
            cycleCount: 0,
            health: 100,
            powerSource: "AC Power",
            timeRemaining: "∞",
            voltage: 0,
            amperage: 0,
            wattage: 0,
            temperature: 0
        )
    }

    private func parseBatteryInfo(from info: [String: Any]) -> BatteryInfo {
        // Basic battery properties
        let currentCapacity = info[kIOPSCurrentCapacityKey as String] as? Int ?? 0
        let maxCapacity = info[kIOPSMaxCapacityKey as String] as? Int ?? 100
        let designCapacity = info["DesignCapacity"] as? Int ?? maxCapacity

        // Calculate health as percentage of design capacity
        let health = designCapacity > 0 ? (Double(maxCapacity) / Double(designCapacity)) * 100 : 100

        // Charging state
        let isCharging = info[kIOPSIsChargingKey as String] as? Bool ?? false
        let powerSource = info[kIOPSPowerSourceStateKey as String] as? String ?? "Unknown"
        let acPowered = powerSource == kIOPSACPowerValue as String

        // Cycle count
        let cycleCount = info["CycleCount"] as? Int ?? 0

        // Time remaining
        let timeRemainingMinutes = info[kIOPSTimeToEmptyKey as String] as? Int ?? -1
        let timeToFullMinutes = info[kIOPSTimeToFullChargeKey as String] as? Int ?? -1

        let timeRemaining: String = if isCharging, timeToFullMinutes > 0 {
            formatTime(minutes: timeToFullMinutes) + " until charged"
        } else if !acPowered, timeRemainingMinutes > 0 {
            formatTime(minutes: timeRemainingMinutes) + " remaining"
        } else if isCharging {
            "Charging..."
        } else if acPowered, currentCapacity >= 100 {
            "Charged"
        } else if acPowered {
            "Not charging"
        } else {
            "Calculating..."
        }

        // Voltage and current
        let voltage = Double(info["Voltage"] as? Int ?? 0) / 1000.0 // Convert mV to V
        let amperage = Double(info["Amperage"] as? Int ?? 0) // Already in mA
        let wattage = abs(voltage * amperage / 1000.0) // Convert to watts

        // Temperature (if available)
        let temperature = Double(info["Temperature"] as? Int ?? 0) / 100.0 // Convert from decidegrees

        return BatteryInfo(
            hasBattery: true,
            isCharging: isCharging,
            currentCapacity: currentCapacity,
            cycleCount: cycleCount,
            health: min(100, max(0, health)),
            powerSource: acPowered ? "AC Power" : "Battery Power",
            timeRemaining: timeRemaining,
            voltage: voltage,
            amperage: amperage,
            wattage: wattage,
            temperature: temperature
        )
    }

    private func formatTime(minutes: Int) -> String {
        if minutes < 60 {
            return "\(minutes)m"
        } else {
            let hours = minutes / 60
            let mins = minutes % 60
            if mins == 0 {
                return "\(hours)h"
            } else {
                return "\(hours):\(String(format: "%02d", mins))"
            }
        }
    }

    /// Get current power adapter information
    func getPowerAdapterInfo() -> (watts: Int, connected: Bool) {
        guard let info = IOPSCopyPowerSourcesInfo()?.takeRetainedValue(),
              let sources = IOPSCopyPowerSourcesList(info)?.takeRetainedValue() as? [Any]
        else {
            return (0, false)
        }

        // Check all power sources for AC adapter
        for source in sources {
            guard let sourceInfo = IOPSGetPowerSourceDescription(info, source as CFTypeRef)?.takeUnretainedValue() as? [String: Any] else {
                continue
            }

            let type = sourceInfo[kIOPSTypeKey as String] as? String
            if type == kIOPSUPSType as String {
                // This might be an AC adapter
                let watts = sourceInfo["AdapterWatts"] as? Int ?? 0
                return (watts, watts > 0)
            }
        }

        // Alternative: Check if we're on AC power
        if let mainSource = sources.first,
           let sourceInfo = IOPSGetPowerSourceDescription(info, mainSource as CFTypeRef)?.takeUnretainedValue() as? [String: Any]
        {
            let powerSource = sourceInfo[kIOPSPowerSourceStateKey as String] as? String
            let isAC = powerSource == kIOPSACPowerValue as String

            // Try to get adapter details from IOKit
            if isAC {
                return getAdapterWattageFromIOKit()
            }
        }

        return (0, false)
    }

    private func getAdapterWattageFromIOKit() -> (watts: Int, connected: Bool) {
        var iterator: io_iterator_t = 0
        let result = IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching("AppleSmartBattery"), &iterator)

        guard result == KERN_SUCCESS else {
            return (0, false)
        }
        defer { IOObjectRelease(iterator) }

        var service: io_object_t = IOIteratorNext(iterator)
        while service != 0 {
            defer { IOObjectRelease(service) }

            var properties: Unmanaged<CFMutableDictionary>?
            let propertiesResult = IORegistryEntryCreateCFProperties(service, &properties, kCFAllocatorDefault, 0)

            if propertiesResult == KERN_SUCCESS, let props = properties?.takeRetainedValue() as? [String: Any] {
                // Look for adapter information
                if let adapterInfo = props["AdapterInfo"] as? [String: Any] {
                    let watts = adapterInfo["Watts"] as? Int ?? 0
                    return (watts, true)
                }

                // Alternative keys
                if let watts = props["AdapterWatts"] as? Int {
                    return (watts, true)
                }
            }

            service = IOIteratorNext(iterator)
        }

        // Default wattages based on Mac model (could be enhanced)
        return (87, true) // Common MacBook Pro adapter
    }
}
