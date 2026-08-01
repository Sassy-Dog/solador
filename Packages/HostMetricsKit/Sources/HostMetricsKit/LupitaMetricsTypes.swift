import Foundation

// Internal data structs returned by the vendored SystemMonitorV2 engine.
//
// Provenance: originally copied from Lupita's PerformanceMonitor.swift (struct/enum
// definitions only — no PerformanceMonitor class, demo mode, or notifications).
// Sync policy: this copy is canonical; Lupita is dead / reference-only — do NOT
// upstream changes or sync back to Lupita. See ../../../README.md. These remain
// `internal`: they are an implementation detail behind the public `HostSnapshot`.
//
// One required edit: `CPUData.initializeCoreHistories` referenced
// `PerformanceMonitor.maxDataPoints` (a constant on the class that was NOT copied)
// and is replaced with the literal `300` to match the buffer capacities used
// throughout these types.
//
// `BatteryData` is NOT redefined here — the battery info type lives in
// BatteryMonitor.swift (as `BatteryMonitor.BatteryInfo`).

/// CPU performance data including usage and historical metrics
///
/// Tracks both overall CPU usage and per-core usage with historical data for charting.
struct CPUData {
    /// Overall CPU usage percentage (0-100)
    var totalUsage = 0.0

    /// Per-core CPU usage percentages
    var coreUsages: [Double] = []

    /// Historical CPU usage data for charting
    var history = CircularBuffer<Double>(capacity: 300)

    /// Historical per-core usage data for detailed monitoring
    var coreHistories: [CircularBuffer<Double>] = []

    /// CPU model name (e.g., "Apple M1")
    var model = ""

    /// System thermal state
    var thermalState: ProcessInfo.ThermalState = .nominal

    mutating func addHistoryPoint(_ value: Double) {
        history.append(value)
    }

    mutating func initializeCoreHistories(count: Int) {
        if coreHistories.isEmpty {
            coreHistories = Array(
                repeating: CircularBuffer<Double>(capacity: 300),
                count: count
            )
        }
    }

    mutating func addCoreHistoryPoints(_ values: [Double]) {
        for (index, value) in values.enumerated() {
            if index < coreHistories.count {
                coreHistories[index].append(value)
            }
        }
    }
}

/// Memory performance data including usage, pressure, and swap information
///
/// Tracks physical memory usage, swap usage, and memory pressure metrics.
///
/// Everything derived from the kernel's VM statistics is Optional, because that
/// one `host_statistics64` call is the only source for all of it and it can fail.
/// `nil` is *unknown*; a defaulted number would be a whole invented memory panel
/// — which is exactly what the failure path used to paint (`total * 0.5`).
/// `totalMemory` is the exception: `ProcessInfo.physicalMemory` is an
/// independent, infallible read, so it stays a measurement even when the VM call
/// fails. It carries **no default** for the same reason `ProcessItem`'s disk
/// rates don't: a default is a fake-zero trap at the type level.
struct MemoryData {
    /// Used memory in GB, or `nil` when the VM statistics read failed.
    var usedMemory: Double?

    /// Total physical memory in GB. Always measured — see the type's docs.
    var totalMemory: Double

    /// Swap memory in use in GB, or `nil` when the VM statistics read failed.
    var swapUsed: Double?

    /// Memory pressure (0-100, higher means more pressure), or `nil` when the VM
    /// statistics read failed.
    var pressure: Double?

    /// Historical memory usage percentages for charting
    var history = CircularBuffer<Double>(capacity: 300)

    mutating func addHistoryPoint(_ value: Double) {
        history.append(value)
    }

    /// Calculated memory usage percentage, or `nil` when there is no reading to
    /// compute it from — a percentage of an unmeasured figure is a fabrication.
    var usagePercentage: Double? {
        guard let usedMemory, totalMemory > 0 else { return nil }
        return (usedMemory / totalMemory) * 100
    }
}

/// Disk I/O performance data
///
/// Tracks disk read and write speeds with historical data.
struct DiskData {
    /// Current disk read speed in MB/s (aggregate of all physical disks)
    var readSpeed = 0.0

    /// Current disk write speed in MB/s (aggregate of all physical disks)
    var writeSpeed = 0.0

    /// Historical read speeds for charting (aggregate)
    var readHistory = CircularBuffer<Double>(capacity: 300)

    /// Historical write speeds for charting (aggregate)
    var writeHistory = CircularBuffer<Double>(capacity: 300)

    /// Individual physical disks on the system
    var physicalDisks: [PhysicalDisk] = []
}

/// Network performance data
///
/// Tracks network download and upload speeds with historical data.
struct NetworkData {
    /// Current download speed in MB/s
    var downloadSpeed = 0.0

    /// Current upload speed in MB/s
    var uploadSpeed = 0.0

    /// Historical download speeds for charting
    var downloadHistory = CircularBuffer<Double>(capacity: 300)

    /// Historical upload speeds for charting
    var uploadHistory = CircularBuffer<Double>(capacity: 300)

    init() {}
}

/// GPU performance data including usage and memory
///
/// Tracks GPU utilization and VRAM usage metrics.
struct GPUData {
    /// GPU usage percentage (0-100)
    var usage = 0.0

    /// GPU memory (VRAM) used in GB
    var memoryUsed = 0.0

    /// Total GPU memory (VRAM) in GB
    var memoryTotal = 8.0

    /// Historical GPU usage percentages for charting
    var history = CircularBuffer<Double>(capacity: 300)

    /// Historical VRAM usage for charting
    var memoryHistory = CircularBuffer<Double>(capacity: 300)

    init() {}
}

/// Individual physical disk information
///
/// Represents a single physical disk with its performance metrics and properties
struct PhysicalDisk: Identifiable {
    let id: String // BSD name (e.g., "disk0", "disk1")
    var name: String // Friendly name (e.g., "Macintosh HD")
    var model: String // Model identifier
    var totalSize: Int64 // Total size in bytes
    var availableSpace: Int64 // Available space in bytes
    var readSpeed: Double // Current read speed in MB/s
    var writeSpeed: Double // Current write speed in MB/s
    var readHistory = CircularBuffer<Double>(capacity: 300)
    var writeHistory = CircularBuffer<Double>(capacity: 300)

    init(id: String, name: String = "", model: String = "", totalSize: Int64 = 0, availableSpace: Int64 = 0) {
        self.id = id
        self.name = name.isEmpty ? id : name
        self.model = model
        self.totalSize = totalSize
        self.availableSpace = availableSpace
        readSpeed = 0
        writeSpeed = 0
    }

    var usedSpace: Int64 {
        totalSize - availableSpace
    }

    var usagePercentage: Double {
        guard totalSize > 0 else { return 0 }
        return Double(usedSpace) / Double(totalSize) * 100
    }
}

/// Individual process information
///
/// Represents a single process with its resource usage metrics
struct ProcessItem: Identifiable {
    let id: Int32 // Process ID (PID)
    var name: String
    var cpuUsage: Double // CPU usage percentage
    var memoryUsage: Double // Memory usage in MB

    /// Disk read bytes per second, or `nil` when the rate is *unknown*.
    ///
    /// Unknown is not zero. `proc_pid_rusage` returns EPERM for processes this
    /// app does not own (most of the system), and the first sample for a pid has
    /// no previous counter to diff against. Both cases must stay distinguishable
    /// from a process that genuinely did no I/O, so a renderer can paint "—".
    var diskReadBytes: UInt64?

    /// Disk write bytes per second, or `nil` when unknown — see `diskReadBytes`.
    var diskWriteBytes: UInt64?

    var isSystemProcess: Bool

    /// - Note: `diskReadBytes`/`diskWriteBytes` deliberately have **no** default.
    ///   A `= 0` default is a fake-zero trap at the type level: it lets a caller
    ///   that never measured anything silently publish a hard zero. Callers must
    ///   pass a measured value or an explicit `nil`.
    init(
        pid: Int32,
        name: String,
        cpuUsage: Double = 0,
        memoryUsage: Double = 0,
        diskReadBytes: UInt64?,
        diskWriteBytes: UInt64?,
        isSystemProcess: Bool = false
    ) {
        id = pid
        self.name = name
        self.cpuUsage = cpuUsage
        self.memoryUsage = memoryUsage
        self.diskReadBytes = diskReadBytes
        self.diskWriteBytes = diskWriteBytes
        self.isSystemProcess = isSystemProcess
    }
}

/// Application information (grouped processes)
///
/// Represents an application with aggregated resource usage from all its processes
struct ApplicationItem: Identifiable {
    let id: String // Application name (used as unique identifier)
    var name: String
    var processCount: Int
    var cpuUsage: Double // Total CPU usage percentage
    var memoryUsage: Double // Total memory usage in MB

    /// Total disk read bytes per second across this app's processes, counting
    /// only the ones whose rate is known; `nil` when none of them are.
    var diskReadBytes: UInt64?

    /// Total disk write bytes per second — same unknown handling as `diskReadBytes`.
    var diskWriteBytes: UInt64?

    var isSystemApp: Bool
    var processes: [ProcessItem] // Individual processes in this app

    init(
        name: String,
        processes: [ProcessItem] = []
    ) {
        id = name
        self.name = name
        self.processes = processes
        processCount = processes.count
        cpuUsage = processes.reduce(0) { $0 + $1.cpuUsage }
        memoryUsage = processes.reduce(0) { $0 + $1.memoryUsage }
        diskReadBytes = Self.sumKnownRates(processes.map(\.diskReadBytes))
        diskWriteBytes = Self.sumKnownRates(processes.map(\.diskWriteBytes))
        isSystemApp = processes.first?.isSystemProcess ?? false
    }

    /// Sums the known per-process rates and skips the unknown ones.
    ///
    /// Returns `nil` when *every* input is unknown: an application whose
    /// processes all refused a rusage read has an unknown total, not a zero one.
    /// Folding an unknown in as 0 is exactly the fabricated number the cockpit
    /// must never render. Saturates at `UInt64.max` instead of trapping.
    static func sumKnownRates(_ rates: [UInt64?]) -> UInt64? {
        let known = rates.compactMap(\.self)
        guard !known.isEmpty else { return nil }
        return known.reduce(UInt64(0)) { total, rate in
            let (sum, overflowed) = total.addingReportingOverflow(rate)
            return overflowed ? .max : sum
        }
    }
}

/// Process monitoring data
///
/// Tracks running processes and their resource usage
struct ProcessData {
    /// All monitored processes
    var processes: [ProcessItem] = []

    /// Sort order for process list
    var sortOrder: ProcessSortOrder = .cpuUsage

    /// Whether to show system processes
    var showSystemProcesses: Bool = false

    /// Returns sorted processes based on current sort order
    var sortedProcesses: [ProcessItem] {
        switch sortOrder {
        case .name:
            processes.sorted { $0.name.lowercased() < $1.name.lowercased() }
        case .cpuUsage:
            processes.sorted { $0.cpuUsage > $1.cpuUsage }
        case .memoryUsage:
            processes.sorted { $0.memoryUsage > $1.memoryUsage }
        }
    }

    /// Returns applications grouped from processes
    var groupedApplications: [ApplicationItem] {
        // Group processes by application name
        let grouped = Dictionary(grouping: processes) { process -> String in
            // Extract base application name (remove version numbers, suffixes like "Helper", etc.)
            let name = process.name

            // Common patterns to identify the base app name
            if name.contains(" Helper") {
                return name.components(separatedBy: " Helper").first ?? name
            } else if name.contains(" (") {
                return name.components(separatedBy: " (").first ?? name
            } else if name.hasSuffix(" Agent") {
                return name.replacingOccurrences(of: " Agent", with: "")
            } else if name.contains("."), !name.hasPrefix("com.") {
                // For names like "1.0.58" keep as is
                return name
            }

            return name
        }

        // Convert to ApplicationItem array
        let apps = grouped.map { appName, processes in
            ApplicationItem(name: appName, processes: processes)
        }

        // Sort applications based on current sort order
        switch sortOrder {
        case .name:
            return apps.sorted { $0.name.lowercased() < $1.name.lowercased() }
        case .cpuUsage:
            return apps.sorted { $0.cpuUsage > $1.cpuUsage }
        case .memoryUsage:
            return apps.sorted { $0.memoryUsage > $1.memoryUsage }
        }
    }
}

/// Sort order options for application list
enum ProcessSortOrder: String, CaseIterable {
    case name = "Name"
    case cpuUsage = "CPU"
    case memoryUsage = "Memory"
}
