import Foundation

/// A point-in-time snapshot of host machine metrics.
public struct HostSnapshot: Sendable, Codable, Equatable {
    public let timestamp: Date
    public let cpu: CPUMetrics
    public let memory: MemoryMetrics
    public let disk: DiskMetrics
    public let network: NetworkMetrics
    public let gpu: GPUMetrics
    /// `nil` when no battery is present (e.g. desktop Macs).
    public let battery: BatteryMetrics?

    public init(
        timestamp: Date,
        cpu: CPUMetrics,
        memory: MemoryMetrics,
        disk: DiskMetrics,
        network: NetworkMetrics,
        gpu: GPUMetrics,
        battery: BatteryMetrics?
    ) {
        self.timestamp = timestamp
        self.cpu = cpu
        self.memory = memory
        self.disk = disk
        self.network = network
        self.gpu = gpu
        self.battery = battery
    }
}

public struct CPUMetrics: Sendable, Codable, Equatable {
    /// Overall CPU usage, 0–100.
    public let totalUsage: Double
    /// Per-core CPU usage, each 0–100.
    public let coreUsages: [Double]
    public let model: String
    public let thermalState: ThermalState

    public init(
        totalUsage: Double,
        coreUsages: [Double],
        model: String,
        thermalState: ThermalState
    ) {
        self.totalUsage = totalUsage
        self.coreUsages = coreUsages
        self.model = model
        self.thermalState = thermalState
    }
}

public enum ThermalState: Int, Sendable, Codable {
    case nominal
    case fair
    case serious
    case critical
}

public struct MemoryMetrics: Sendable, Codable, Equatable {
    public let usedGB: Double
    public let totalGB: Double
    public let swapUsedGB: Double
    /// Memory pressure, 0–100.
    public let pressure: Double

    public var usagePercentage: Double { totalGB > 0 ? usedGB / totalGB * 100 : 0 }

    public init(
        usedGB: Double,
        totalGB: Double,
        swapUsedGB: Double,
        pressure: Double
    ) {
        self.usedGB = usedGB
        self.totalGB = totalGB
        self.swapUsedGB = swapUsedGB
        self.pressure = pressure
    }
}

public struct DiskMetrics: Sendable, Codable, Equatable {
    public let readMBps: Double
    public let writeMBps: Double

    public init(readMBps: Double, writeMBps: Double) {
        self.readMBps = readMBps
        self.writeMBps = writeMBps
    }
}

public struct NetworkMetrics: Sendable, Codable, Equatable {
    public let downloadMBps: Double
    public let uploadMBps: Double

    public init(downloadMBps: Double, uploadMBps: Double) {
        self.downloadMBps = downloadMBps
        self.uploadMBps = uploadMBps
    }
}

public struct GPUMetrics: Sendable, Codable, Equatable {
    /// GPU usage, 0–100.
    public let usage: Double
    public let vramUsedGB: Double
    public let vramTotalGB: Double

    public init(usage: Double, vramUsedGB: Double, vramTotalGB: Double) {
        self.usage = usage
        self.vramUsedGB = vramUsedGB
        self.vramTotalGB = vramTotalGB
    }
}

public struct BatteryMetrics: Sendable, Codable, Equatable {
    public let hasBattery: Bool
    public let currentCapacity: Int
    public let isCharging: Bool
    public let cycleCount: Int
    public let health: Double
    public let powerSource: String
    public let timeRemaining: String
    public let wattage: Double

    public init(
        hasBattery: Bool,
        currentCapacity: Int,
        isCharging: Bool,
        cycleCount: Int,
        health: Double,
        powerSource: String,
        timeRemaining: String,
        wattage: Double
    ) {
        self.hasBattery = hasBattery
        self.currentCapacity = currentCapacity
        self.isCharging = isCharging
        self.cycleCount = cycleCount
        self.health = health
        self.powerSource = powerSource
        self.timeRemaining = timeRemaining
        self.wattage = wattage
    }
}
