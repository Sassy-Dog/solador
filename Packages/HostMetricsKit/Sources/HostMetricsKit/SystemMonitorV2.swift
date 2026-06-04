import Darwin
import Foundation
import IOKit
import IOKit.graphics
import IOKit.ps
import IOKit.storage
import Metal
import os

// MARK: - Safe System Monitor with Comprehensive Error Handling

class SystemMonitorV2 {
    private var previousCPUInfo: host_cpu_load_info_data_t?
    private var previousCoreStates: [processor_cpu_load_info]?
    private var previousNetworkStats: NetworkStats?
    private var previousDiskStats: DiskStats?
    private var metalDevice: MTLDevice?
    private let gpuMonitor = GPUMonitor()

    // Process CPU tracking for percentage calculation
    private var previousProcessCPUTimes: [pid_t: UInt64] = [:]
    private var lastProcessUpdateTime = Date()

    init() {
        metalDevice = MTLCreateSystemDefaultDevice()
    }

    // MARK: - Public API with Error Handling

    func getCPUData() -> CPUData {
        var cpuData = CPUData()

        // Get CPU usage with error handling
        switch getCPUUsage() {
        case let .success(usage):
            cpuData.totalUsage = usage.total
            cpuData.coreUsages = usage.perCore
        case let .failure(error):
            Logger.monitor.error("CPU usage monitoring failed: \(error.localizedDescription)")
            cpuData.totalUsage = 0.0
            cpuData.coreUsages = []
        }

        // Get CPU model
        switch getCPUModel() {
        case let .success(model):
            cpuData.model = model
        case let .failure(error):
            Logger.monitor.error("CPU model detection failed: \(error.localizedDescription)")
            cpuData.model = "Unknown CPU"
        }

        return cpuData
    }

    func getMemoryData() -> MemoryData {
        var memoryData = MemoryData()

        switch getMemoryInfo() {
        case let .success(info):
            memoryData.usedMemory = info.used
            memoryData.totalMemory = info.total
            memoryData.swapUsed = info.swapUsed
            memoryData.pressure = info.pressure
        case let .failure(error):
            Logger.monitor.error("Memory monitoring failed: \(error.localizedDescription)")
            // Use sensible defaults
            let totalMemory = Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824
            memoryData.totalMemory = totalMemory
            memoryData.usedMemory = totalMemory * 0.5 // Assume 50% usage
            memoryData.swapUsed = 0.0
            memoryData.pressure = 0.0
        }

        return memoryData
    }

    func getDiskData() -> DiskData {
        var diskData = DiskData()

        // Get aggregate disk stats (existing functionality)
        switch getDiskStats() {
        case let .success(stats):
            diskData.readSpeed = stats.readSpeed
            diskData.writeSpeed = stats.writeSpeed
        case let .failure(error):
            Logger.monitor.error("Disk monitoring failed: \(error.localizedDescription)")
            diskData.readSpeed = 0.0
            diskData.writeSpeed = 0.0
        }

        // Get individual physical disk information
        switch getPhysicalDisks() {
        case let .success(disks):
            diskData.physicalDisks = disks
        case let .failure(error):
            Logger.monitor.error("Physical disk enumeration failed: \(error.localizedDescription)")
            diskData.physicalDisks = []
        }

        return diskData
    }

    func getNetworkData() -> NetworkData {
        var networkData = NetworkData()

        switch getNetworkStats() {
        case let .success(stats):
            networkData.downloadSpeed = stats.downloadSpeed
            networkData.uploadSpeed = stats.uploadSpeed
        case let .failure(error):
            Logger.monitor.error("Network monitoring failed: \(error.localizedDescription)")
            networkData.downloadSpeed = 0.0
            networkData.uploadSpeed = 0.0
        }

        return networkData
    }

    func getGPUData() -> GPUData {
        var gpuData = GPUData()

        // Use the new GPUMonitor
        let info = gpuMonitor.getGPUInfo()
        gpuData.usage = info.usage
        gpuData.memoryUsed = info.memoryUsed
        gpuData.memoryTotal = info.memoryTotal

        // Validate values
        if gpuData.memoryTotal <= 0 {
            // Fallback to Metal device info
            if let device = metalDevice {
                gpuData.memoryTotal = Double(device.recommendedMaxWorkingSetSize) / 1_073_741_824
                if gpuData.memoryTotal <= 0 {
                    gpuData.memoryTotal = 8.0 // Default fallback
                }
            }
        }

        return gpuData
    }

    func getProcessData() -> ProcessData {
        Logger.monitor.debug("=== getProcessData called ===")
        var processData = ProcessData()

        // Get basic process list
        var mib: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_ALL]
        var size: size_t = 0

        // First, get the size needed
        if sysctl(&mib, u_int(mib.count), nil, &size, nil, 0) == -1 {
            Logger.monitor.error("Failed to get process list size")
            return processData
        }

        Logger.monitor.debug("Getting process list, expected size: \(size) bytes")

        // Allocate memory and get the process list
        let count = size / MemoryLayout<kinfo_proc>.size
        let buffer = UnsafeMutablePointer<kinfo_proc>.allocate(capacity: count)
        defer { buffer.deallocate() }

        if sysctl(&mib, u_int(mib.count), buffer, &size, nil, 0) == -1 {
            Logger.monitor.error("Failed to get process list")
            return processData
        }

        let actualCount = size / MemoryLayout<kinfo_proc>.size
        var processes: [ProcessItem] = []

        Logger.monitor.debug("Found \(actualCount) processes in system")

        // Iterate through processes
        var skippedKernel = 0
        var skippedEmpty = 0
        let failedMemory = 0

        for i in 0 ..< actualCount {
            let proc = buffer[i]
            let pid = proc.kp_proc.p_pid

            // Skip kernel_task (pid 0)
            if pid == 0 {
                skippedKernel += 1
                continue
            }

            // Get process name - try multiple methods for best results
            var name = ""

            // Method 1: Try to get the process name using proc_name
            var nameBuffer = [CChar](repeating: 0, count: Int(MAXPATHLEN))
            if proc_name(pid, &nameBuffer, UInt32(MAXPATHLEN)) > 0 {
                name = String(cString: &nameBuffer)
            }

            // Method 2: Fall back to p_comm if proc_name failed
            if name.isEmpty {
                let namePtr = withUnsafePointer(to: proc.kp_proc.p_comm) { ptr in
                    ptr.withMemoryRebound(to: CChar.self, capacity: Int(MAXCOMLEN)) { $0 }
                }
                name = String(cString: namePtr)
            }

            // Skip if name is still empty
            if name.isEmpty {
                skippedEmpty += 1
                continue
            }

            // Basic process info (we'll enhance this with more metrics later)
            let isSystem = proc.kp_eproc.e_ppid == 1 || name.hasPrefix("com.apple.") || name.contains("daemon")

            // Get process memory info
            let memoryUsage = getProcessMemoryInfo(pid: pid, proc: proc)

            // Get process CPU usage
            let cpuUsage = getProcessCPUUsage(pid: pid, proc: proc)

            let process = ProcessItem(
                pid: pid,
                name: name,
                cpuUsage: cpuUsage,
                memoryUsage: memoryUsage,
                diskReadBytes: 0, // TODO: Get disk I/O stats
                diskWriteBytes: 0,
                isSystemProcess: isSystem
            )

            processes.append(process)
        }

        Logger.monitor.debug("Process collection stats: Total: \(actualCount), Skipped kernel: \(skippedKernel), Skipped empty: \(skippedEmpty), Failed memory: \(failedMemory), Collected: \(processes.count)")

        // Sort by memory usage by default
        processData.processes = processes
            .sorted { $0.memoryUsage > $1.memoryUsage }

        Logger.monitor.debug("Returning \(processData.processes.count) processes")

        if processData.processes.isEmpty {
            Logger.monitor.warning("No processes collected!")
        }

        // Clean up stale CPU time entries for processes that no longer exist
        let currentPIDs = Set(processes.map(\.id))
        previousProcessCPUTimes = previousProcessCPUTimes.filter { currentPIDs.contains($0.key) }

        // Update the timestamp for next CPU percentage calculation
        lastProcessUpdateTime = Date()

        return processData
    }

    // MARK: - Private Process Methods

    private func getProcessMemoryInfo(pid: pid_t, proc _: kinfo_proc) -> Double {
        // Use proc_pidinfo to get real memory statistics
        var taskInfo = proc_taskinfo()
        let size = Int32(MemoryLayout<proc_taskinfo>.size)

        let result = proc_pidinfo(pid, PROC_PIDTASKINFO, 0, &taskInfo, size)

        if result <= 0 {
            // Fallback: Return a minimal value so the process shows up in the list
            // This happens for some system processes we don't have access to
            // We'll show 1 MB as a placeholder since we can't get real data
            return 1.0
        }

        // Return resident memory in MB (this matches what Activity Monitor shows)
        return Double(taskInfo.pti_resident_size) / (1024.0 * 1024.0)
    }

    private func getProcessCPUUsage(pid: pid_t, proc _: kinfo_proc) -> Double {
        // Use proc_pidinfo to get real CPU usage statistics
        var taskInfo = proc_taskinfo()
        let size = Int32(MemoryLayout<proc_taskinfo>.size)

        let result = proc_pidinfo(pid, PROC_PIDTASKINFO, 0, &taskInfo, size)

        if result <= 0 {
            // Expected for processes this app doesn't own (proc_pidinfo requires
            // matching ownership/entitlements); treat as 0% rather than logging.
            return 0.0
        }

        let currentTime = taskInfo.pti_total_user + taskInfo.pti_total_system

        // Calculate percentage based on previous sample
        if let previousTime = previousProcessCPUTimes[pid] {
            let timeDiff = currentTime - previousTime
            let elapsedSeconds = Date().timeIntervalSince(lastProcessUpdateTime)

            if elapsedSeconds > 0, timeDiff > 0 {
                // Convert nanoseconds to seconds and calculate percentage
                let cpuSeconds = Double(timeDiff) / 1_000_000_000.0
                let cpuPercent = (cpuSeconds / elapsedSeconds) * 100.0

                previousProcessCPUTimes[pid] = currentTime

                // Get CPU core count to normalize the percentage
                let coreCount = Double(ProcessInfo.processInfo.processorCount)

                // Return normalized percentage (can be over 100% on multi-core systems)
                return min(cpuPercent, coreCount * 100.0)
            }
        }

        // First sample - store for next calculation
        previousProcessCPUTimes[pid] = currentTime
        return 0.0
    }

    // MARK: - Private CPU Methods

    private func getCPUUsage() -> MonitoringResult<(total: Double, perCore: [Double])> {
        var info = mach_msg_type_number_t()
        var cpuInfo = host_cpu_load_info_data_t()
        info = UInt32(MemoryLayout<host_cpu_load_info_data_t>.size / MemoryLayout<integer_t>.size)

        let hostPort = mach_host_self()
        defer { mach_port_deallocate(mach_task_self_, hostPort) }

        let result = withUnsafeMutablePointer(to: &cpuInfo) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(info)) {
                host_statistics(hostPort, HOST_CPU_LOAD_INFO, $0, &info)
            }
        }

        guard result == KERN_SUCCESS else {
            return .failure(.ioKitError(result, "host_statistics"))
        }

        let userTicks = Double(cpuInfo.cpu_ticks.0)
        let systemTicks = Double(cpuInfo.cpu_ticks.1)
        let idleTicks = Double(cpuInfo.cpu_ticks.2)
        let niceTicks = Double(cpuInfo.cpu_ticks.3)

        if let previous = previousCPUInfo {
            let userDelta = userTicks - Double(previous.cpu_ticks.0)
            let systemDelta = systemTicks - Double(previous.cpu_ticks.1)
            let idleDelta = idleTicks - Double(previous.cpu_ticks.2)
            let niceDelta = niceTicks - Double(previous.cpu_ticks.3)

            let totalDelta = userDelta + systemDelta + idleDelta + niceDelta
            let usedDelta = userDelta + systemDelta + niceDelta

            let usage = totalDelta > 0 ? (usedDelta / totalDelta) * 100 : 0
            previousCPUInfo = cpuInfo

            // Get per-core usage
            let perCoreResult = getPerCoreUsage()
            let perCore = perCoreResult.value ?? []

            return .success((usage.clamped(to: 0 ... 100), perCore))
        } else {
            previousCPUInfo = cpuInfo
            // Still get per-core data to return zeros for the correct number of cores
            let perCoreResult = getPerCoreUsage()
            let perCore = perCoreResult.value ?? []
            return .success((0, perCore))
        }
    }

    private func getPerCoreUsage() -> MonitoringResult<[Double]> {
        var cpuInfo: processor_info_array_t?
        var numCpuInfo: mach_msg_type_number_t = 0
        var numCpus: natural_t = 0

        let result = host_processor_info(mach_host_self(), PROCESSOR_CPU_LOAD_INFO, &numCpus, &cpuInfo, &numCpuInfo)

        guard result == KERN_SUCCESS else {
            return .failure(.ioKitError(result, "host_processor_info"))
        }

        guard let validCpuInfo = cpuInfo else {
            return .failure(.invalidData("CPU info is nil"))
        }

        var coreUsages: [Double] = []
        var currentCoreStates: [processor_cpu_load_info] = []

        for i in 0 ..< Int(numCpus) {
            let offset = Int(CPU_STATE_MAX) * i

            // Bounds check
            guard offset + Int(CPU_STATE_NICE) < Int(numCpuInfo) else {
                continue
            }

            let user = validCpuInfo[offset + Int(CPU_STATE_USER)]
            let system = validCpuInfo[offset + Int(CPU_STATE_SYSTEM)]
            let idle = validCpuInfo[offset + Int(CPU_STATE_IDLE)]
            let nice = validCpuInfo[offset + Int(CPU_STATE_NICE)]

            let currentState = processor_cpu_load_info(
                cpu_ticks: (UInt32(user), UInt32(system), UInt32(idle), UInt32(nice))
            )
            currentCoreStates.append(currentState)

            // Calculate delta if we have previous data
            if let previous = previousCoreStates, i < previous.count {
                let userDelta = Double(user) - Double(previous[i].cpu_ticks.0)
                let systemDelta = Double(system) - Double(previous[i].cpu_ticks.1)
                let idleDelta = Double(idle) - Double(previous[i].cpu_ticks.2)
                let niceDelta = Double(nice) - Double(previous[i].cpu_ticks.3)

                let totalDelta = userDelta + systemDelta + idleDelta + niceDelta
                let usedDelta = userDelta + systemDelta + niceDelta

                let usage = totalDelta > 0 ? (usedDelta / totalDelta) * 100 : 0
                coreUsages.append(usage.clamped(to: 0 ... 100))
            } else {
                // First run, no delta available
                coreUsages.append(0.0)
            }
        }

        // Store current states for next calculation
        previousCoreStates = currentCoreStates

        // Deallocate memory
        let deallocateResult = vm_deallocate(mach_task_self_, vm_address_t(bitPattern: validCpuInfo), vm_size_t(numCpuInfo))
        if deallocateResult != KERN_SUCCESS {
            Logger.system.warning("Failed to deallocate CPU info memory: \(deallocateResult)")
        }

        return .success(coreUsages)
    }

    private func getCPUModel() -> MonitoringResult<String> {
        var size: size_t = 0
        let result = sysctlbyname("machdep.cpu.brand_string", nil, &size, nil, 0)

        guard result == 0 else {
            return .failure(.sysctlError(errno, "machdep.cpu.brand_string size"))
        }

        guard size > 0 else {
            return .failure(.invalidData("CPU brand string size is 0"))
        }

        var model = [CChar](repeating: 0, count: size)
        let result2 = sysctlbyname("machdep.cpu.brand_string", &model, &size, nil, 0)

        guard result2 == 0 else {
            return .failure(.sysctlError(errno, "machdep.cpu.brand_string value"))
        }

        // Remove null bytes and create string
        let modelData = model.withUnsafeBufferPointer { buffer in
            Data(bytes: buffer.baseAddress!, count: buffer.count)
        }
        let cpuModel = String(data: modelData, encoding: .utf8)?
            .trimmingCharacters(in: .controlCharacters)
            .trimmingCharacters(in: CharacterSet(charactersIn: "\0"))
            ?? "Unknown"
        return .success(cpuModel)
    }

    // MARK: - Private Memory Methods

    private func getMemoryInfo() -> MonitoringResult<(used: Double, total: Double, swapUsed: Double, pressure: Double)> {
        // Get physical memory
        let physicalMemory = Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824

        // Get VM statistics
        var vmInfo = vm_statistics64()
        var vmInfoSize = mach_msg_type_number_t(MemoryLayout<vm_statistics64>.size / MemoryLayout<natural_t>.size)
        let hostPort = mach_host_self()
        defer { mach_port_deallocate(mach_task_self_, hostPort) }

        let vmResult = withUnsafeMutablePointer(to: &vmInfo) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(vmInfoSize)) {
                host_statistics64(hostPort, HOST_VM_INFO64, $0, &vmInfoSize)
            }
        }

        guard vmResult == KERN_SUCCESS else {
            return .failure(.ioKitError(vmResult, "host_statistics64"))
        }

        let pageSize: Double = {
            var size = vm_size_t()
            let hostPort = mach_host_self()
            host_page_size(hostPort, &size)
            return Double(size)
        }()
        let usedPages = Double(vmInfo.active_count + vmInfo.wire_count)
        let usedMemory = (usedPages * pageSize) / 1_073_741_824
        let swapUsed = Double(vmInfo.swapouts) * pageSize / 1_073_741_824

        // Calculate pressure safely
        let compressions = Double(vmInfo.compressions)
        let decompressions = Double(vmInfo.decompressions)
        let pressure: Double = if compressions > 0 {
            ((compressions - decompressions) / compressions) * 100
        } else {
            0
        }

        return .success((usedMemory, physicalMemory, swapUsed, pressure.clamped(to: 0 ... 100)))
    }

    // MARK: - Private Disk Methods

    /// Determines if an IOBlockStorageDriver service represents a physical disk
    ///
    /// This filters out virtual disks, disk images, iOS Simulator disks, and other non-physical storage.
    /// Only internal physical disks (SSDs, HDDs, NVMe) are considered valid.
    ///
    /// - Parameter service: The IOBlockStorageDriver service to check
    /// - Returns: true if the device is a physical, internal disk; false otherwise
    private func isPhysicalDisk(service: io_service_t) -> Bool {
        // Get the parent IOBlockStorageDevice
        var parent: io_service_t = 0
        let result = IORegistryEntryGetParentEntry(service, kIOServicePlane, &parent)

        guard result == KERN_SUCCESS else {
            if Logger.isDebugEnabled {
                Logger.system.debug("Failed to get parent device for IOBlockStorageDriver")
            }
            return false
        }

        defer { IOObjectRelease(parent) }

        // Get properties from the parent device
        var properties: Unmanaged<CFMutableDictionary>?
        let propResult = IORegistryEntryCreateCFProperties(parent, &properties, kCFAllocatorDefault, 0)

        guard propResult == KERN_SUCCESS,
              let props = properties?.takeRetainedValue() as? [String: Any]
        else {
            if Logger.isDebugEnabled {
                Logger.system.debug("Failed to get properties from parent device")
            }
            return false
        }

        // Check for Protocol Characteristics indicating a physical disk
        guard let protocolCharacteristics = props["Protocol Characteristics"] as? [String: Any] else {
            // No protocol characteristics = not a physical disk (likely disk image or virtual)
            if Logger.isVerboseEnabled {
                Logger.system.trace("Device has no Protocol Characteristics - excluding (likely virtual/image)")
            }
            return false
        }

        // Check if it's an internal disk
        guard let location = protocolCharacteristics["Physical Interconnect Location"] as? String,
              location == "Internal"
        else {
            if Logger.isVerboseEnabled {
                let location = protocolCharacteristics["Physical Interconnect Location"] as? String ?? "Unknown"
                Logger.system.trace("Device location '\(location)' is not Internal - excluding")
            }
            return false
        }

        // Check for a valid physical interconnect type (optional but helpful)
        if let interconnect = protocolCharacteristics["Physical Interconnect"] as? String {
            // Valid physical interconnect types
            let validTypes = ["Apple Fabric", "PCI-Express", "SATA", "NVMe", "SAS", "USB", "Thunderbolt"]
            let isValid = validTypes.contains { interconnect.contains($0) }

            if Logger.isVerboseEnabled {
                if isValid {
                    Logger.system.trace("Physical disk detected: \(interconnect) at Internal location")
                } else {
                    Logger.system.trace("Unknown interconnect type '\(interconnect)' - excluding")
                }
            }

            return isValid
        }

        // If we have Protocol Characteristics and Internal location but no interconnect,
        // cautiously include it
        if Logger.isDebugEnabled {
            Logger.system.debug("Device has Protocol Characteristics and Internal location but no interconnect type - including")
        }
        return true
    }

    private func getDiskStats() -> MonitoringResult<(readSpeed: Double, writeSpeed: Double)> {
        var iterator: io_iterator_t = 0
        let result = IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching("IOBlockStorageDriver"), &iterator)

        guard result == KERN_SUCCESS else {
            return .failure(.ioKitError(result, "IOServiceGetMatchingServices"))
        }

        guard iterator != 0 else {
            return .failure(.serviceNotFound("IOBlockStorageDriver"))
        }

        defer { IOObjectRelease(iterator) }

        // Sum statistics from physical disk devices only
        var totalBytesRead: Int64 = 0
        var totalBytesWritten: Int64 = 0
        var physicalDeviceCount = 0
        var skippedDeviceCount = 0

        if Logger.isVerboseEnabled {
            Logger.system.trace("=== Disk Stats Collection ===")
        }

        while case let service = IOIteratorNext(iterator), service != 0 {
            defer { IOObjectRelease(service) }

            // Filter: Only include physical disks
            guard isPhysicalDisk(service: service) else {
                skippedDeviceCount += 1
                // Don't log every single skipped device - too verbose and hurts performance
                continue
            }

            var properties: Unmanaged<CFMutableDictionary>?
            let propResult = IORegistryEntryCreateCFProperties(service, &properties, kCFAllocatorDefault, 0)

            guard propResult == KERN_SUCCESS,
                  let props = properties?.takeRetainedValue() as? [String: Any],
                  let statistics = props["Statistics"] as? [String: Any]
            else {
                // Skip this device if we can't get its properties - don't log each instance
                continue
            }

            let bytesRead = statistics["Bytes (Read)"] as? Int64 ?? 0
            let bytesWritten = statistics["Bytes (Write)"] as? Int64 ?? 0

            // Don't log every physical device - just collect the data

            totalBytesRead += bytesRead
            totalBytesWritten += bytesWritten
            physicalDeviceCount += 1
        }

        if Logger.isDebugEnabled {
            Logger.system.trace("Found \(physicalDeviceCount) physical disk(s) (skipped \(skippedDeviceCount) non-physical), total: read=\(totalBytesRead) bytes, write=\(totalBytesWritten) bytes")
        }

        let bytesRead = totalBytesRead
        let bytesWritten = totalBytesWritten

        if let previous = previousDiskStats {
            let currentBytesRead = UInt64(max(0, bytesRead))
            let currentBytesWritten = UInt64(max(0, bytesWritten))

            // Check for counter wrap-around or reset
            let readDiff: Double
            let writeDiff: Double

            if currentBytesRead >= previous.bytesRead {
                readDiff = Double(currentBytesRead - previous.bytesRead) / 1_048_576 // Convert to MB
            } else {
                // Counter wrapped around or reset
                Logger.system.debug("Disk read counter wrap-around detected")
                readDiff = 0
            }

            if currentBytesWritten >= previous.bytesWritten {
                writeDiff = Double(currentBytesWritten - previous.bytesWritten) / 1_048_576
            } else {
                // Counter wrapped around or reset
                Logger.system.debug("Disk write counter wrap-around detected")
                writeDiff = 0
            }

            previousDiskStats = DiskStats(bytesRead: currentBytesRead, bytesWritten: currentBytesWritten)

            if Logger.isDebugEnabled {
                Logger.system.trace("Speed: read=\(readDiff) MB/s, write=\(writeDiff) MB/s")
            }

            return .success((max(0, readDiff), max(0, writeDiff)))
        } else {
            previousDiskStats = DiskStats(bytesRead: UInt64(max(0, bytesRead)), bytesWritten: UInt64(max(0, bytesWritten)))

            if Logger.isDebugEnabled {
                Logger.system.debug("First run - establishing baseline")
            }

            return .success((0, 0))
        }
    }

    private func getPhysicalDisks() -> MonitoringResult<[PhysicalDisk]> {
        var disks: [PhysicalDisk] = []
        var diskStats: [String: (bytesRead: UInt64, bytesWritten: UInt64)] = [:]

        // First pass: Get disk I/O statistics
        var iterator: io_iterator_t = 0
        let result = IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching("IOBlockStorageDriver"), &iterator)

        guard result == KERN_SUCCESS else {
            return .failure(.ioKitError(result, "IOServiceGetMatchingServices"))
        }

        guard iterator != 0 else {
            return .failure(.serviceNotFound("IOBlockStorageDriver"))
        }

        defer { IOObjectRelease(iterator) }

        while case let service = IOIteratorNext(iterator), service != 0 {
            defer { IOObjectRelease(service) }

            // Filter: Only include physical disks
            guard isPhysicalDisk(service: service) else { continue }

            // Get BSD name from the service properties
            var serviceProps: Unmanaged<CFMutableDictionary>?
            let servicePropResult = IORegistryEntryCreateCFProperties(service, &serviceProps, kCFAllocatorDefault, 0)

            guard servicePropResult == KERN_SUCCESS,
                  let svcProps = serviceProps?.takeRetainedValue() as? [String: Any] else { continue }

            // Try to get BSD name from properties
            let bsdNameFromProps = svcProps["BSD Name"] as? String

            // Get statistics
            var properties: Unmanaged<CFMutableDictionary>?
            let propResult = IORegistryEntryCreateCFProperties(service, &properties, kCFAllocatorDefault, 0)

            guard propResult == KERN_SUCCESS,
                  let props = properties?.takeRetainedValue() as? [String: Any]
            else { continue }

            // Get disk properties
            var parent: io_service_t = 0
            let parentResult = IORegistryEntryGetParentEntry(service, kIOServicePlane, &parent)

            if parentResult == KERN_SUCCESS {
                defer { IOObjectRelease(parent) }

                var parentProps: Unmanaged<CFMutableDictionary>?
                let parentPropResult = IORegistryEntryCreateCFProperties(parent, &parentProps, kCFAllocatorDefault, 0)

                if parentPropResult == KERN_SUCCESS,
                   let parentProperties = parentProps?.takeRetainedValue() as? [String: Any]
                {
                    // Extract model information - try multiple properties
                    var model = parentProperties["Model"] as? String
                    if model == nil {
                        if let deviceChar = parentProperties["Device Characteristics"] as? [String: Any],
                           let productName = deviceChar["Product Name"] as? String
                        {
                            model = productName
                        }
                    }
                    if model == nil {
                        model = parentProperties["device-type"] as? String
                    }
                    if model == nil || model == "Unknown" {
                        model = "Apple SSD" // Default for modern Macs
                    }

                    // Get the BSD name from parent properties
                    let bsdUnit = parentProperties["BSD Unit"] as? Int
                    let bsdName = bsdUnit != nil ? "disk\(bsdUnit!)" : (bsdNameFromProps ?? "unknown")

                    // Create PhysicalDisk
                    let disk = PhysicalDisk(id: bsdName, name: bsdName, model: model!)

                    // Get statistics for speed calculation
                    if let statistics = props["Statistics"] as? [String: Any] {
                        let bytesRead = UInt64(statistics["Bytes (Read)"] as? Int64 ?? 0)
                        let bytesWritten = UInt64(statistics["Bytes (Write)"] as? Int64 ?? 0)
                        diskStats[bsdName] = (bytesRead, bytesWritten)
                    }

                    disks.append(disk)
                }
            }
        }

        // Get volume information for disk sizes
        // Only include the root filesystem and other mounted physical volumes

        // First, get the root filesystem info
        if let rootURL = URL(string: "file:///") {
            if let resourceValues = try? rootURL.resourceValues(forKeys: [.volumeTotalCapacityKey, .volumeAvailableCapacityKey, .volumeNameKey]) {
                let totalSize = resourceValues.volumeTotalCapacity ?? 0
                let availableSpace = resourceValues.volumeAvailableCapacity ?? 0
                let volumeName = resourceValues.volumeName ?? "Macintosh HD"

                // Find the main physical disk and update its volume info
                // Modern Macs may have disk3, disk4, etc. as the main disk
                for i in 0 ..< disks.count {
                    // Match any diskN pattern or assign to first disk if only one exists
                    let diskID = disks[i].id

                    // If there's only one physical disk, it must be the system disk
                    // Or if it matches the disk pattern
                    if disks.count == 1 || (diskID.hasPrefix("disk") && diskID.dropFirst(4).first?.isNumber ?? false) {
                        disks[i].name = volumeName
                        disks[i].totalSize = Int64(totalSize)
                        disks[i].availableSpace = Int64(availableSpace)
                        // Don't break - in case there are multiple physical disks, we want the first one to get the root volume
                        break
                    }
                }
            }
        }

        // For any remaining physical disks without size info, try to get from IORegistry
        for i in 0 ..< disks.count {
            if disks[i].totalSize == 0 {
                // Leave it with zero size rather than incorrectly assigning mounted image volumes
                disks[i].name = disks[i].model // Use model name if no volume name found
            }
        }

        return .success(disks)
    }

    // MARK: - Private Network Methods

    private func getNetworkStats() -> MonitoringResult<(downloadSpeed: Double, uploadSpeed: Double)> {
        var ifaddr: UnsafeMutablePointer<ifaddrs>?

        let result = getifaddrs(&ifaddr)
        guard result == 0 else {
            return .failure(.sysctlError(errno, "getifaddrs"))
        }

        guard let ifaddr else {
            return .failure(.invalidData("Network interfaces list is nil"))
        }

        defer { freeifaddrs(ifaddr) }

        var totalBytesIn: UInt64 = 0
        var totalBytesOut: UInt64 = 0

        var ptr: UnsafeMutablePointer<ifaddrs>? = ifaddr
        while let current = ptr {
            if let data = current.pointee.ifa_data,
               let name = current.pointee.ifa_name
            {
                let interfaceName = String(cString: name)

                // Include more network interfaces (en for ethernet/wifi, utun for VPN, etc.)
                if interfaceName.hasPrefix("en") ||
                    interfaceName.hasPrefix("utun") ||
                    interfaceName.hasPrefix("ipsec")
                {
                    let stats = data.assumingMemoryBound(to: if_data.self)
                    totalBytesIn += UInt64(stats.pointee.ifi_ibytes)
                    totalBytesOut += UInt64(stats.pointee.ifi_obytes)
                }
            }
            ptr = current.pointee.ifa_next
        }

        let currentStats = NetworkStats(bytesReceived: totalBytesIn, bytesSent: totalBytesOut)

        if let previous = previousNetworkStats {
            // Check for counter wrap-around or reset
            let downloadDiff: Double
            let uploadDiff: Double

            if currentStats.bytesReceived >= previous.bytesReceived {
                downloadDiff = Double(currentStats.bytesReceived - previous.bytesReceived) / 1_048_576 // MB/s
            } else {
                // Counter wrapped around or reset
                Logger.network.debug("Network download counter wrap-around detected")
                downloadDiff = 0
            }

            if currentStats.bytesSent >= previous.bytesSent {
                uploadDiff = Double(currentStats.bytesSent - previous.bytesSent) / 1_048_576
            } else {
                // Counter wrapped around or reset
                Logger.network.debug("Network upload counter wrap-around detected")
                uploadDiff = 0
            }

            previousNetworkStats = currentStats

            return .success((max(0, downloadDiff), max(0, uploadDiff)))
        } else {
            previousNetworkStats = currentStats
            return .success((0, 0))
        }
    }

    // MARK: - Private GPU Methods

    private func getGPUInfo() -> MonitoringResult<(usage: Double, memoryUsed: Double, memoryTotal: Double)> {
        // Try IOAccelerator first
        var iterator: io_iterator_t = 0
        let result = IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching("IOAccelerator"), &iterator)

        guard result == KERN_SUCCESS else {
            // Fallback to Metal
            return getMetalGPUInfo()
        }

        defer { IOObjectRelease(iterator) }

        var gpuUsage: Double = 0
        var memoryUsed: Double = 0
        var memoryTotal: Double = 0
        var foundGPU = false

        var service: io_object_t = IOIteratorNext(iterator)
        while service != 0 {
            defer { IOObjectRelease(service) }

            var properties: Unmanaged<CFMutableDictionary>?
            let propertiesResult = IORegistryEntryCreateCFProperties(service, &properties, kCFAllocatorDefault, 0)

            if propertiesResult == KERN_SUCCESS, let props = properties?.takeRetainedValue() as? [String: Any] {
                foundGPU = true

                // Look for performance statistics
                if let performanceStatistics = props["PerformanceStatistics"] as? [String: Any] {
                    // Device Utilization %
                    if let deviceUtilization = performanceStatistics["Device Utilization %"] as? Int {
                        gpuUsage = Double(deviceUtilization)
                    } else if let gpuActivityPercent = performanceStatistics["GPU Activity(%)"] as? Int {
                        gpuUsage = Double(gpuActivityPercent)
                    }
                }
            }

            service = IOIteratorNext(iterator)
        }

        if !foundGPU {
            return getMetalGPUInfo()
        }

        // Get VRAM info
        let vramResult = getVRAMInfo()
        if case let .success(vram) = vramResult {
            memoryUsed = vram.used
            memoryTotal = vram.total
        }

        // If we still don't have memory info, use Metal
        if memoryTotal == 0 {
            if case let .success(metalInfo) = getMetalGPUInfo() {
                memoryTotal = metalInfo.memoryTotal
                if memoryUsed == 0 {
                    memoryUsed = metalInfo.memoryUsed
                }
            }
        }

        return .success((gpuUsage, memoryUsed, memoryTotal))
    }

    private func getVRAMInfo() -> MonitoringResult<(used: Double, total: Double)> {
        // Implementation would go here - simplified for now
        .failure(.serviceNotFound("VRAM monitoring not fully implemented"))
    }

    private func getMetalGPUInfo() -> MonitoringResult<(
        usage: Double,
        memoryUsed: Double,
        memoryTotal: Double
    )> {
        guard let device = metalDevice else {
            return .failure(.serviceNotFound("Metal device not available"))
        }

        // Get total memory
        var memoryTotal = 8.0
        if device.hasUnifiedMemory {
            // For Apple Silicon, use system memory info
            let totalSystemMemory = Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824
            // Apple Silicon typically allocates up to half of system memory for GPU
            memoryTotal = totalSystemMemory / 2.0
        } else {
            // For discrete GPUs
            let totalMemory = device.recommendedMaxWorkingSetSize
            memoryTotal = totalMemory > 0 ? Double(totalMemory) / 1_073_741_824 : 8.0
        }

        // Get current allocated size
        let memoryUsed = Double(device.currentAllocatedSize) / 1_073_741_824

        // Estimate usage
        let usage = (memoryUsed > 0 && memoryTotal > 0) ? (memoryUsed / memoryTotal) * 100 : 0

        return .success((usage, memoryUsed, memoryTotal))
    }
}

/// Extension to make Result easier to work with
extension Result {
    var value: Success? {
        switch self {
        case let .success(value):
            value
        case .failure:
            nil
        }
    }
}

// MARK: - Supporting Data Structures

public struct NetworkStats {
    public let bytesReceived: UInt64
    public let bytesSent: UInt64

    public init(bytesReceived: UInt64, bytesSent: UInt64) {
        self.bytesReceived = bytesReceived
        self.bytesSent = bytesSent
    }
}

public struct DiskStats {
    public let bytesRead: UInt64
    public let bytesWritten: UInt64

    public init(bytesRead: UInt64, bytesWritten: UInt64) {
        self.bytesRead = bytesRead
        self.bytesWritten = bytesWritten
    }
}

// MARK: - Double Extension

public extension Double {
    func clamped(to range: ClosedRange<Double>) -> Double {
        min(max(self, range.lowerBound), range.upperBound)
    }
}
