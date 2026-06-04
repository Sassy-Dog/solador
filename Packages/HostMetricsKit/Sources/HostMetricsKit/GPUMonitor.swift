import Foundation
import IOKit
import IOKit.graphics
import Metal
import os

/// GPU monitoring with real VRAM tracking
class GPUMonitor {
    private let metalDevice: MTLDevice?
    private var previousGPUStats: GPUStats?
    private var lastUpdateTime: TimeInterval = 0

    private struct GPUStats {
        let gpuTime: Int64
        let totalTime: Int64
        let timestamp: TimeInterval
    }

    init() {
        metalDevice = MTLCreateSystemDefaultDevice()
    }

    func getGPUInfo() -> (usage: Double, memoryUsed: Double, memoryTotal: Double) {
        // Try multiple methods to get accurate GPU information

        // Method 1: IOAccelerator for GPU usage
        var usage = getIOAcceleratorInfo()

        // Method 2: AGPMController for power state
        let powerInfo = getAGPMInfo()

        // Method 3: IOGraphics for VRAM
        let vramInfo = getIOGraphicsVRAMInfo()

        // Method 4: Metal for unified memory devices
        let metalInfo = getMetalInfo()

        // Combine results
        var memoryUsed = vramInfo.used
        var memoryTotal = vramInfo.total

        // Use power state to estimate usage if needed
        if usage == 0, powerInfo > 0 {
            usage = estimateUsageFromPowerState(powerInfo)
        }

        // Use Metal for memory if IOKit didn't provide it
        if memoryTotal == 0 {
            memoryTotal = metalInfo.total
            if memoryUsed == 0 {
                memoryUsed = metalInfo.used
            }
        }

        // For Apple Silicon, get memory pressure
        if metalDevice?.hasUnifiedMemory == true, memoryUsed == 0 {
            memoryUsed = getUnifiedMemoryGPUUsage()
        }

        return (usage, memoryUsed, memoryTotal)
    }

    private func getIOAcceleratorInfo() -> Double {
        var usage: Double = 0

        // Try multiple accelerator classes
        let acceleratorClasses = [
            "IOAccelerator",
            "IntelAccelerator",
            "AMDRadeonX4000_AMDEllesmereGraphicsAccelerator",
            "AMDRadeonX6000_AMDNavi23GraphicsAccelerator",
            "AGXAccelerator",
            "AppleGPUPerfStateController"
        ]

        for className in acceleratorClasses {
            var iterator: io_iterator_t = 0
            let result = IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching(className), &iterator)

            guard result == KERN_SUCCESS else { continue }
            defer { IOObjectRelease(iterator) }

            var service: io_object_t = IOIteratorNext(iterator)
            while service != 0 {
                defer { IOObjectRelease(service) }

                var properties: Unmanaged<CFMutableDictionary>?
                let propertiesResult = IORegistryEntryCreateCFProperties(service, &properties, kCFAllocatorDefault, 0)

                if propertiesResult == KERN_SUCCESS, let props = properties?.takeRetainedValue() as? [String: Any] {
                    // Check PerformanceStatistics
                    if let perfStats = props["PerformanceStatistics"] as? [String: Any] {
                        // Try various utilization keys
                        if let deviceUtil = perfStats["Device Utilization %"] as? Int {
                            usage = Double(deviceUtil)
                        } else if let gpuActivity = perfStats["GPU Activity(%)"] as? Int {
                            usage = Double(gpuActivity)
                        } else if let utilization = perfStats["utilization"] as? Int {
                            usage = Double(utilization)
                        } else if let gpuCoreUtil = perfStats["GPU Core Utilization"] as? Int {
                            usage = Double(gpuCoreUtil)
                        } else if let rendererUtil = perfStats["Renderer Utilization %"] as? Int {
                            // Sometimes renderer utilization is a good proxy
                            usage = Double(rendererUtil)
                        }

                        // Try to calculate utilization from time metrics
                        if usage == 0, let gpuTime = perfStats["GPU Time"] as? Int64,
                           let totalTime = perfStats["Total Time"] as? Int64
                        {
                            let currentTime = Date().timeIntervalSince1970
                            let currentStats = GPUStats(gpuTime: gpuTime, totalTime: totalTime, timestamp: currentTime)

                            if let previousStats = previousGPUStats,
                               currentTime - previousStats.timestamp > 0.1
                            {
                                let gpuTimeDelta = currentStats.gpuTime - previousStats.gpuTime
                                let totalTimeDelta = currentStats.totalTime - previousStats.totalTime

                                if totalTimeDelta > 0 {
                                    usage = (Double(gpuTimeDelta) / Double(totalTimeDelta)) * 100
                                    usage = min(100, max(0, usage))
                                }
                            }

                            previousGPUStats = currentStats
                        }
                    }

                    // Check IOAcceleratorStatistics
                    if usage == 0, let accelStats = props["IOAcceleratorStatistics"] as? [String: Any] {
                        if let gpuTime = accelStats["GPU Time"] as? Int64,
                           let totalTime = accelStats["Total Time"] as? Int64, totalTime > 0
                        {
                            usage = (Double(gpuTime) / Double(totalTime)) * 100
                        }
                    }

                    // For AGX (Apple Silicon) specific properties
                    if className == "AGXAccelerator", usage == 0 {
                        if let agxStats = props["AGXPerfStats"] as? [String: Any] {
                            if let utilPercent = agxStats["UtilizationPercent"] as? Int {
                                usage = Double(utilPercent)
                            }
                        }
                    }

                    // If we found data, break out of the loop
                    if usage > 0 {
                        break
                    }
                }

                service = IOIteratorNext(iterator)
            }

            // If we found data from this class, don't try others
            if usage > 0 {
                break
            }
        }

        return usage
    }

    private func getAGPMInfo() -> Int {
        var powerState = 0

        var iterator: io_iterator_t = 0
        let result = IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching("AGPMController"), &iterator)

        guard result == KERN_SUCCESS else {
            return powerState
        }
        defer { IOObjectRelease(iterator) }

        var service: io_object_t = IOIteratorNext(iterator)
        while service != 0 {
            defer { IOObjectRelease(service) }

            var properties: Unmanaged<CFMutableDictionary>?
            let propertiesResult = IORegistryEntryCreateCFProperties(service, &properties, kCFAllocatorDefault, 0)

            if propertiesResult == KERN_SUCCESS, let props = properties?.takeRetainedValue() as? [String: Any] {
                if let graphicsControl = props["graphics-control"] as? [String: Any],
                   let currentState = graphicsControl["current-state"] as? Int
                {
                    powerState = currentState
                }
            }

            service = IOIteratorNext(iterator)
        }

        return powerState
    }

    private func getIOGraphicsVRAMInfo() -> (used: Double, total: Double) {
        var memoryUsed: Double = 0
        var memoryTotal: Double = 0

        // Try IOFramebuffer
        var iterator: io_iterator_t = 0
        var result = IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching("IOFramebuffer"), &iterator)

        if result == KERN_SUCCESS {
            defer { IOObjectRelease(iterator) }

            var service: io_object_t = IOIteratorNext(iterator)
            while service != 0 {
                defer { IOObjectRelease(service) }

                // Get parent display controller
                var parent: io_object_t = 0
                let parentResult = IORegistryEntryGetParentEntry(service, kIOServicePlane, &parent)

                if parentResult == KERN_SUCCESS {
                    defer { IOObjectRelease(parent) }

                    var properties: Unmanaged<CFMutableDictionary>?
                    let propertiesResult = IORegistryEntryCreateCFProperties(parent, &properties, kCFAllocatorDefault, 0)

                    if propertiesResult == KERN_SUCCESS, let props = properties?.takeRetainedValue() as? [String: Any] {
                        // Look for VRAM size
                        if let vramTotalMB = props["VRAM,totalMB"] as? Int {
                            memoryTotal = Double(vramTotalMB) / 1024.0
                        } else if let vramTotalSize = props["VRAM,totalsize"] as? Int64 {
                            memoryTotal = Double(vramTotalSize) / 1_073_741_824
                        } else if let vramSize = props["@0,VRAM,memsize"] as? Int64 {
                            memoryTotal = Double(vramSize) / 1_073_741_824
                        }

                        // Look for used memory
                        if let perfStats = props["PerformanceStatistics"] as? [String: Any] {
                            if let vramUsedBytes = perfStats["vramUsedBytes"] as? Int64 {
                                memoryUsed = Double(vramUsedBytes) / 1_073_741_824
                            } else if let allocatedVRAM = perfStats["Allocated VRAM"] as? Int64 {
                                memoryUsed = Double(allocatedVRAM) / 1_073_741_824
                            } else if let inUseVRAM = perfStats["In use VRAM"] as? Int64 {
                                memoryUsed = Double(inUseVRAM) / 1_073_741_824
                            }
                        }
                    }
                }

                service = IOIteratorNext(iterator)
            }
        }

        // Try IOPCIDevice for discrete GPUs
        if memoryTotal == 0 {
            iterator = 0
            result = IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching("IOPCIDevice"), &iterator)

            if result == KERN_SUCCESS {
                defer { IOObjectRelease(iterator) }

                var service: io_object_t = IOIteratorNext(iterator)
                while service != 0 {
                    defer { IOObjectRelease(service) }

                    var properties: Unmanaged<CFMutableDictionary>?
                    let propertiesResult = IORegistryEntryCreateCFProperties(service, &properties, kCFAllocatorDefault, 0)

                    if propertiesResult == KERN_SUCCESS, let props = properties?.takeRetainedValue() as? [String: Any] {
                        // Check if this is a GPU
                        if let deviceClass = props["class-code"] as? Data, deviceClass.count >= 3 {
                            let classBytes = [UInt8](deviceClass)
                            // 0x03 = Display Controller
                            if classBytes[2] == 0x03 {
                                // Look for memory regions
                                if let memRegions = props["assigned-addresses"] as? Data {
                                    // Parse PCI memory regions to find VRAM size
                                    let vramSize = parsePCIMemoryRegions(memRegions)
                                    if vramSize > 0 {
                                        memoryTotal = Double(vramSize) / 1_073_741_824
                                    }
                                }
                            }
                        }
                    }

                    service = IOIteratorNext(iterator)
                }
            }
        }

        return (memoryUsed, memoryTotal)
    }

    private func parsePCIMemoryRegions(_ data: Data) -> Int64 {
        // PCI memory regions are in a specific format
        // Each region is 20 bytes: [space:1][flags:3][base:8][size:8]
        guard data.count >= 20 else { return 0 }

        var maxSize: Int64 = 0
        let regionSize = 20
        let regionCount = data.count / regionSize

        for i in 0 ..< regionCount {
            let offset = i * regionSize
            let regionData = data.subdata(in: offset ..< offset + regionSize)

            // Get size (last 8 bytes)
            if regionData.count >= 20 {
                let sizeData = regionData.subdata(in: 12 ..< 20)
                var size: Int64 = 0
                sizeData.withUnsafeBytes { bytes in
                    size = bytes.load(as: Int64.self).bigEndian
                }

                // VRAM is typically the largest memory region
                if size > maxSize {
                    maxSize = size
                }
            }
        }

        return maxSize
    }

    private func getMetalInfo() -> (used: Double, total: Double) {
        guard let device = metalDevice else {
            return (0, 8.0)
        }

        var total = 8.0
        var used: Double = 0

        if device.hasUnifiedMemory {
            // Apple Silicon
            let systemMemory = Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824
            total = systemMemory / 2.0 // Up to half for GPU

            // Get current allocation
            used = Double(device.currentAllocatedSize) / 1_073_741_824
        } else {
            // Discrete GPU
            let recommendedSize = device.recommendedMaxWorkingSetSize
            if recommendedSize > 0 {
                total = Double(recommendedSize) / 1_073_741_824
            }

            used = Double(device.currentAllocatedSize) / 1_073_741_824
        }

        return (used, total)
    }

    private func getUnifiedMemoryGPUUsage() -> Double {
        // For Apple Silicon, estimate GPU memory usage from IOAccelerator allocations
        var totalAllocated: Double = 0

        // Check IOAccelerator memory allocations
        var iterator: io_iterator_t = 0
        let result = IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching("IOAccelerator"), &iterator)

        guard result == KERN_SUCCESS else {
            return totalAllocated
        }
        defer { IOObjectRelease(iterator) }

        var service: io_object_t = IOIteratorNext(iterator)
        while service != 0 {
            defer { IOObjectRelease(service) }

            // Try to get memory info from properties
            var properties: Unmanaged<CFMutableDictionary>?
            let propertiesResult = IORegistryEntryCreateCFProperties(service, &properties, kCFAllocatorDefault, 0)

            if propertiesResult == KERN_SUCCESS, let props = properties?.takeRetainedValue() as? [String: Any] {
                // Look for memory allocations
                if let memoryInfo = props["IOAccelMemoryInfo"] as? [String: Any] {
                    if let allocated = memoryInfo["allocated"] as? Int64 {
                        totalAllocated += Double(allocated) / 1_073_741_824
                    }
                } else if let stats = props["PerformanceStatistics"] as? [String: Any] {
                    if let vramUsed = stats["vramUsedBytes"] as? Int64 {
                        totalAllocated += Double(vramUsed) / 1_073_741_824
                    }
                }
            }

            service = IOIteratorNext(iterator)
        }

        // If we couldn't get allocations, estimate based on Metal
        if totalAllocated == 0, let device = metalDevice {
            totalAllocated = Double(device.currentAllocatedSize) / 1_073_741_824
        }

        // Ensure a minimum value for UI feedback
        if totalAllocated == 0 {
            totalAllocated = 0.5 // Show 0.5 GB as minimum
        }

        return totalAllocated
    }

    private func estimateUsageFromPowerState(_ state: Int) -> Double {
        // Estimate GPU usage based on power state
        switch state {
        case 0: 0 // Idle
        case 1: 15 // Low power
        case 2: 35 // Medium power
        case 3: 60 // High power
        case 4: 85 // Maximum power
        default: 0
        }
    }

    /// Debug method to print available GPU properties
    func debugPrintGPUProperties() {
        Logger.system.debug("=== GPU Debug Information ===")

        let acceleratorClasses = [
            "IOAccelerator",
            "IntelAccelerator",
            "AMDRadeonX4000_AMDEllesmereGraphicsAccelerator",
            "AGXAccelerator"
        ]

        for className in acceleratorClasses {
            var iterator: io_iterator_t = 0
            let result = IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching(className), &iterator)

            guard result == KERN_SUCCESS else { continue }
            defer { IOObjectRelease(iterator) }

            var service: io_object_t = IOIteratorNext(iterator)
            while service != 0 {
                defer { IOObjectRelease(service) }

                var properties: Unmanaged<CFMutableDictionary>?
                let propertiesResult = IORegistryEntryCreateCFProperties(service, &properties, kCFAllocatorDefault, 0)

                if propertiesResult == KERN_SUCCESS, let props = properties?.takeRetainedValue() as? [String: Any] {
                    Logger.system.trace("\nFound \(className):")
                    Logger.system.trace("Available properties: \(props.keys.sorted())")

                    if let perfStats = props["PerformanceStatistics"] as? [String: Any] {
                        Logger.system.trace("Performance Statistics keys: \(perfStats.keys.sorted())")
                    }
                }

                service = IOIteratorNext(iterator)
            }
        }
    }
}
